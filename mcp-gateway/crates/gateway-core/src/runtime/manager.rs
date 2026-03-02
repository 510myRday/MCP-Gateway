use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::config::{DefaultsConfig, LifecycleMode, ServerConfig, StdioProtocol};
use crate::error::AppError;

use super::connection::ProcessConnection;
use super::pool::PooledEntry;
use super::protocol_negotiation::{
    alternate_protocol, infer_protocol_from_server, should_attempt_protocol_fallback,
};

#[derive(Clone)]
pub struct ProcessManager {
    pooled: Arc<RwLock<HashMap<String, Arc<PooledEntry>>>>,
    protocol_hints: Arc<RwLock<HashMap<String, StdioProtocol>>>,
    tools_cache: Arc<RwLock<HashMap<String, Value>>>,
}

impl ProcessManager {
    pub fn new() -> Self {
        Self {
            pooled: Arc::new(RwLock::new(HashMap::new())),
            protocol_hints: Arc::new(RwLock::new(HashMap::new())),
            tools_cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn call_server(
        &self,
        server: &ServerConfig,
        defaults: &DefaultsConfig,
        request: Value,
    ) -> Result<Value, AppError> {
        let max_attempts = defaults.max_retries.saturating_add(1);
        let mut last_error: Option<AppError> = None;

        for attempt in 1..=max_attempts {
            match self
                .call_server_once(server, defaults, request.clone())
                .await
            {
                Ok(response) => return Ok(response),
                Err(error) => {
                    last_error = Some(error);
                    if attempt < max_attempts {
                        tokio::time::sleep(Duration::from_millis(100 * u64::from(attempt))).await;
                    }
                }
            }
        }

        Err(last_error.unwrap_or_else(|| {
            AppError::Upstream("request failed without explicit error".to_string())
        }))
    }

    pub async fn test_server(
        &self,
        server: &ServerConfig,
        defaults: &DefaultsConfig,
    ) -> Result<Value, AppError> {
        let timeout_duration = Duration::from_millis(defaults.request_timeout_ms);

        let init_request = initialize_request();
        let (mut conn, initialize_response) = self
            .spawn_initialized_connection(server, defaults, timeout_duration, &init_request)
            .await?;

        conn.notify(&initialized_notification()).await?;
        let _ = conn.shutdown().await;

        Ok(json!({
            "ok": true,
            "initialize": initialize_response,
            "testedAt": chrono::Utc::now()
        }))
    }

    pub async fn list_tools(
        &self,
        server: &ServerConfig,
        defaults: &DefaultsConfig,
        refresh: bool,
    ) -> Result<Value, AppError> {
        if !refresh {
            if let Some(cached) = self.tools_cache.read().await.get(&server.name).cloned() {
                return Ok(cached);
            }
        }

        let timeout_duration = Duration::from_millis(defaults.request_timeout_ms);
        let init_request = initialize_request();
        let (mut conn, _) = self
            .spawn_initialized_connection(server, defaults, timeout_duration, &init_request)
            .await?;

        conn.notify(&initialized_notification()).await?;
        let list_req = json!({
            "jsonrpc": "2.0",
            "id": format!("tools-{}", Uuid::new_v4()),
            "method": "tools/list",
            "params": {}
        });
        let tools_response = conn
            .request(
                &list_req,
                timeout_duration,
                defaults.max_response_wait_iterations,
            )
            .await?;

        let _ = conn.shutdown().await;
        self.tools_cache
            .write()
            .await
            .insert(server.name.clone(), tools_response.clone());

        Ok(tools_response)
    }

    pub async fn reset_pool(&self) {
        let old_entries = {
            let mut guard = self.pooled.write().await;
            guard.drain().map(|(_, value)| value).collect::<Vec<_>>()
        };
        self.protocol_hints.write().await.clear();
        self.tools_cache.write().await.clear();

        for entry in old_entries {
            entry.shutdown().await;
        }
    }

    pub async fn evict_server(&self, server_name: &str) {
        let removed = {
            let mut guard = self.pooled.write().await;
            guard.remove(server_name)
        };
        self.protocol_hints.write().await.remove(server_name);
        self.tools_cache.write().await.remove(server_name);

        if let Some(entry) = removed {
            entry.shutdown().await;
        }
    }

    pub async fn reap_idle(&self, idle_ttl: Duration) {
        let now = Instant::now();
        let mut stale = Vec::new();

        {
            let guard = self.pooled.read().await;
            for (server_name, entry) in guard.iter() {
                let last_used = *entry.last_used.lock().await;
                if now.duration_since(last_used) >= idle_ttl {
                    stale.push(server_name.clone());
                }
            }
        }

        for server_name in stale {
            self.evict_server(&server_name).await;
        }
    }

    async fn call_server_once(
        &self,
        server: &ServerConfig,
        defaults: &DefaultsConfig,
        request: Value,
    ) -> Result<Value, AppError> {
        let mut effective_server = server.clone();
        effective_server.stdio_protocol = self.effective_protocol_for(server).await;
        let allow_any_request_fallback = self.allow_any_request_protocol_fallback(server).await;

        let lifecycle = server
            .lifecycle
            .clone()
            .unwrap_or_else(|| defaults.lifecycle.clone());
        let timeout_duration = Duration::from_millis(defaults.request_timeout_ms);

        let primary_error = match self
            .call_server_with_protocol(
                &effective_server,
                &lifecycle,
                &request,
                timeout_duration,
                defaults.max_response_wait_iterations,
            )
            .await
        {
            Ok(response) => return Ok(response),
            Err(error) => error,
        };

        if !should_attempt_protocol_fallback(&request, &primary_error, allow_any_request_fallback) {
            return Err(primary_error);
        }

        let fallback_protocol = alternate_protocol(&effective_server.stdio_protocol);

        if matches!(lifecycle, LifecycleMode::Pooled) {
            self.evict_server(&server.name).await;
        }
        self.remember_protocol_hint(&server.name, fallback_protocol.clone())
            .await;

        let mut fallback_server = server.clone();
        fallback_server.stdio_protocol = fallback_protocol.clone();
        match self
            .call_server_with_protocol(
                &fallback_server,
                &lifecycle,
                &request,
                timeout_duration,
                defaults.max_response_wait_iterations,
            )
            .await
        {
            Ok(response) => Ok(response),
            Err(fallback_error) => {
                self.protocol_hints.write().await.remove(&server.name);
                Err(AppError::Upstream(format!(
                    "protocol fallback failed (configured: {:?}, fallback: {:?}); original error: {}; fallback error: {}",
                    effective_server.stdio_protocol, fallback_protocol, primary_error, fallback_error
                )))
            }
        }
    }

    async fn call_server_with_protocol(
        &self,
        server: &ServerConfig,
        lifecycle: &LifecycleMode,
        request: &Value,
        timeout_duration: Duration,
        max_response_wait_iterations: u32,
    ) -> Result<Value, AppError> {
        match lifecycle {
            LifecycleMode::PerRequest => {
                let mut conn = ProcessConnection::spawn(server).await?;
                let response = conn
                    .request(request, timeout_duration, max_response_wait_iterations)
                    .await;
                let _ = conn.shutdown().await;
                response
            }
            LifecycleMode::Pooled => {
                self.call_pooled_with_recover(
                    server,
                    request,
                    timeout_duration,
                    max_response_wait_iterations,
                )
                .await
            }
        }
    }

    async fn call_pooled_with_recover(
        &self,
        server: &ServerConfig,
        request: &Value,
        timeout_duration: Duration,
        max_response_wait_iterations: u32,
    ) -> Result<Value, AppError> {
        match self
            .call_pooled_once(
                server,
                request,
                timeout_duration,
                max_response_wait_iterations,
            )
            .await
        {
            Ok(value) => Ok(value),
            Err(_) => {
                self.evict_server(&server.name).await;
                self.call_pooled_once(
                    server,
                    request,
                    timeout_duration,
                    max_response_wait_iterations,
                )
                .await
            }
        }
    }

    async fn call_pooled_once(
        &self,
        server: &ServerConfig,
        request: &Value,
        timeout_duration: Duration,
        max_response_wait_iterations: u32,
    ) -> Result<Value, AppError> {
        let entry = self.get_or_create_pooled_entry(server).await?;
        entry.touch().await;
        let mut conn = entry.connection.lock().await;
        conn.request(request, timeout_duration, max_response_wait_iterations)
            .await
    }

    async fn effective_protocol_for(&self, server: &ServerConfig) -> StdioProtocol {
        let guard = self.protocol_hints.read().await;
        let configured = guard
            .get(&server.name)
            .cloned()
            .unwrap_or_else(|| server.stdio_protocol.clone());
        match configured {
            StdioProtocol::Auto => {
                infer_protocol_from_server(server).unwrap_or(StdioProtocol::ContentLength)
            }
            other => other,
        }
    }

    async fn allow_any_request_protocol_fallback(&self, server: &ServerConfig) -> bool {
        if !matches!(server.stdio_protocol, StdioProtocol::Auto) {
            return false;
        }
        !self.protocol_hints.read().await.contains_key(&server.name)
    }

    async fn remember_protocol_hint(&self, server_name: &str, protocol: StdioProtocol) {
        self.protocol_hints
            .write()
            .await
            .insert(server_name.to_string(), protocol);
    }

    async fn spawn_initialized_connection(
        &self,
        server: &ServerConfig,
        defaults: &DefaultsConfig,
        timeout_duration: Duration,
        init_request: &Value,
    ) -> Result<(ProcessConnection, Value), AppError> {
        let mut effective_server = server.clone();
        effective_server.stdio_protocol = self.effective_protocol_for(server).await;

        let mut conn = ProcessConnection::spawn(&effective_server).await?;
        match conn
            .request(
                init_request,
                timeout_duration,
                defaults.max_response_wait_iterations,
            )
            .await
        {
            Ok(response) => Ok((conn, response)),
            Err(primary_error) => {
                let _ = conn.shutdown().await;
                if !should_attempt_protocol_fallback(init_request, &primary_error, false) {
                    return Err(primary_error);
                }

                let fallback_protocol = alternate_protocol(&effective_server.stdio_protocol);
                self.remember_protocol_hint(&server.name, fallback_protocol.clone())
                    .await;

                let mut fallback_server = server.clone();
                fallback_server.stdio_protocol = fallback_protocol;
                let mut fallback_conn = ProcessConnection::spawn(&fallback_server).await?;
                match fallback_conn
                    .request(
                        init_request,
                        timeout_duration,
                        defaults.max_response_wait_iterations,
                    )
                    .await
                {
                    Ok(response) => Ok((fallback_conn, response)),
                    Err(fallback_error) => {
                        let _ = fallback_conn.shutdown().await;
                        self.protocol_hints.write().await.remove(&server.name);
                        Err(AppError::Upstream(format!(
                            "protocol fallback failed; original error: {primary_error}; fallback error: {fallback_error}"
                        )))
                    }
                }
            }
        }
    }

    async fn get_or_create_pooled_entry(
        &self,
        server: &ServerConfig,
    ) -> Result<Arc<PooledEntry>, AppError> {
        let signature = server_signature(server);

        {
            let guard = self.pooled.read().await;
            if let Some(entry) = guard.get(&server.name) {
                if entry.signature == signature {
                    return Ok(entry.clone());
                }
            }
        }

        let mut guard = self.pooled.write().await;
        if let Some(entry) = guard.get(&server.name) {
            if entry.signature == signature {
                return Ok(entry.clone());
            }
        }

        let conn = ProcessConnection::spawn(server).await?;
        let new_entry = Arc::new(PooledEntry::new(signature, conn));

        if let Some(old_entry) = guard.insert(server.name.clone(), new_entry.clone()) {
            tokio::spawn(async move {
                old_entry.shutdown().await;
            });
        }

        Ok(new_entry)
    }
}

impl Default for ProcessManager {
    fn default() -> Self {
        Self::new()
    }
}

fn initialize_request() -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": format!("init-{}", Uuid::new_v4()),
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {
                "name": "mcp-gateway",
                "version": env!("CARGO_PKG_VERSION")
            }
        }
    })
}

fn initialized_notification() -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized",
        "params": {}
    })
}

fn server_signature(server: &ServerConfig) -> String {
    let mut env_items = server
        .env
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>();
    env_items.sort();

    format!(
        "{}|{}|{}|{}|{}|{:?}",
        server.command,
        server.args.join("\u{001f}"),
        server.cwd,
        env_items.join("\u{001e}"),
        server.enabled,
        server.stdio_protocol
    )
}
