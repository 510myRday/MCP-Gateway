use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::config::{DefaultsConfig, LifecycleMode, ServerConfig};
use crate::error::AppError;

use super::connection::ProcessConnection;
use super::pool::PooledEntry;
use super::protocol_negotiation::{
    alternate_protocol, protocol_label, should_attempt_protocol_fallback, NegotiatedStdioProtocol,
};

#[derive(Clone)]
pub struct ProcessManager {
    pooled: Arc<RwLock<HashMap<String, Arc<PooledEntry>>>>,
    protocol_hints: Arc<RwLock<HashMap<String, NegotiatedStdioProtocol>>>,
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
        let candidates = {
            let guard = self.pooled.read().await;
            guard
                .iter()
                .map(|(server_name, entry)| (server_name.clone(), entry.clone()))
                .collect::<Vec<_>>()
        };
        let mut stale = Vec::new();

        for (server_name, entry) in candidates {
            let last_used = *entry.last_used.lock().await;
            if now.duration_since(last_used) >= idle_ttl {
                stale.push(server_name);
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
        let lifecycle = server
            .lifecycle
            .clone()
            .unwrap_or_else(|| defaults.lifecycle.clone());
        let timeout_duration = Duration::from_millis(defaults.request_timeout_ms);

        if self.should_auto_detect_protocol(server).await && is_initialize_request(&request) {
            return self
                .call_initialize_with_auto_detection(
                    server,
                    &lifecycle,
                    &request,
                    timeout_duration,
                    defaults.max_response_wait_iterations,
                )
                .await;
        }

        let effective_protocol = self.effective_protocol_for(server).await;
        let allow_any_request_fallback = self.allow_any_request_protocol_fallback(server).await;

        let primary_error = match self
            .call_server_with_protocol(
                server,
                effective_protocol,
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

        let fallback_protocol = alternate_protocol(effective_protocol);

        if matches!(lifecycle, LifecycleMode::Pooled) {
            self.evict_server(&server.name).await;
        }
        self.remember_protocol_hint(&server.name, fallback_protocol.clone())
            .await;

        match self
            .call_server_with_protocol(
                server,
                fallback_protocol,
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
                    effective_protocol, fallback_protocol, primary_error, fallback_error
                )))
            }
        }
    }

    async fn call_server_with_protocol(
        &self,
        server: &ServerConfig,
        protocol: NegotiatedStdioProtocol,
        lifecycle: &LifecycleMode,
        request: &Value,
        timeout_duration: Duration,
        max_response_wait_iterations: u32,
    ) -> Result<Value, AppError> {
        match lifecycle {
            LifecycleMode::PerRequest => {
                let mut conn = ProcessConnection::spawn(server, protocol).await?;
                let response = conn
                    .request(request, timeout_duration, max_response_wait_iterations)
                    .await;
                let _ = conn.shutdown().await;
                response
            }
            LifecycleMode::Pooled => {
                self.call_pooled_with_recover(
                    server,
                    protocol,
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
        protocol: NegotiatedStdioProtocol,
        request: &Value,
        timeout_duration: Duration,
        max_response_wait_iterations: u32,
    ) -> Result<Value, AppError> {
        match self
            .call_pooled_once(
                server,
                protocol,
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
                    protocol,
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
        protocol: NegotiatedStdioProtocol,
        request: &Value,
        timeout_duration: Duration,
        max_response_wait_iterations: u32,
    ) -> Result<Value, AppError> {
        let entry = self.get_or_create_pooled_entry(server, protocol).await?;
        entry.touch().await;
        let mut conn = entry.connection.lock().await;
        conn.request(request, timeout_duration, max_response_wait_iterations)
            .await
    }

    async fn call_initialize_with_auto_detection(
        &self,
        server: &ServerConfig,
        lifecycle: &LifecycleMode,
        request: &Value,
        timeout_duration: Duration,
        max_response_wait_iterations: u32,
    ) -> Result<Value, AppError> {
        let (mut conn, response, protocol) = self
            .race_protocol_request(
                server,
                request,
                timeout_duration,
                max_response_wait_iterations,
            )
            .await?;
        self.remember_protocol_hint(&server.name, protocol).await;

        match lifecycle {
            LifecycleMode::PerRequest => {
                let _ = conn.shutdown().await;
            }
            LifecycleMode::Pooled => {
                self.replace_pooled_entry(server, conn).await;
            }
        }

        Ok(response)
    }

    async fn race_protocol_request(
        &self,
        server: &ServerConfig,
        request: &Value,
        timeout_duration: Duration,
        max_response_wait_iterations: u32,
    ) -> Result<(ProcessConnection, Value, NegotiatedStdioProtocol), AppError> {
        let (primary_protocol, secondary_protocol) = auto_detection_protocol_candidates();

        let mut primary_conn = ProcessConnection::spawn(server, primary_protocol).await?;
        let mut secondary_conn = ProcessConnection::spawn(server, secondary_protocol).await?;

        let mut primary_request =
            Box::pin(primary_conn.request(request, timeout_duration, max_response_wait_iterations));
        let mut secondary_request = Box::pin(secondary_conn.request(
            request,
            timeout_duration,
            max_response_wait_iterations,
        ));

        let mut primary_error: Option<AppError> = None;
        let mut secondary_error: Option<AppError> = None;

        loop {
            tokio::select! {
                result = &mut primary_request, if primary_error.is_none() => {
                    match result {
                        Ok(response) => {
                            drop(primary_request);
                            drop(secondary_request);
                            let _ = secondary_conn.shutdown().await;
                            return Ok((primary_conn, response, primary_protocol));
                        }
                        Err(error) => {
                            primary_error = Some(error);
                        }
                    }
                }
                result = &mut secondary_request, if secondary_error.is_none() => {
                    match result {
                        Ok(response) => {
                            drop(primary_request);
                            drop(secondary_request);
                            let _ = primary_conn.shutdown().await;
                            return Ok((secondary_conn, response, secondary_protocol));
                        }
                        Err(error) => {
                            secondary_error = Some(error);
                        }
                    }
                }
            }

            if primary_error.is_some() && secondary_error.is_some() {
                drop(primary_request);
                drop(secondary_request);
                let primary_stderr = primary_conn.stderr_snapshot().await;
                let secondary_stderr = secondary_conn.stderr_snapshot().await;
                let _ = primary_conn.shutdown().await;
                let _ = secondary_conn.shutdown().await;

                let primary_error = primary_error.expect("primary error should exist");
                let secondary_error = secondary_error.expect("secondary error should exist");
                return Err(build_auto_detection_error(
                    primary_protocol,
                    &primary_error,
                    &primary_stderr,
                    secondary_protocol,
                    &secondary_error,
                    &secondary_stderr,
                ));
            }
        }
    }

    async fn replace_pooled_entry(&self, server: &ServerConfig, connection: ProcessConnection) {
        let signature = server_signature(server);
        let new_entry = Arc::new(PooledEntry::new(signature, connection));
        let old_entry = {
            let mut guard = self.pooled.write().await;
            guard.insert(server.name.clone(), new_entry)
        };

        if let Some(old_entry) = old_entry {
            tokio::spawn(async move {
                old_entry.shutdown().await;
            });
        }
    }

    async fn should_auto_detect_protocol(&self, server: &ServerConfig) -> bool {
        !self.protocol_hints.read().await.contains_key(&server.name)
    }

    async fn effective_protocol_for(&self, server: &ServerConfig) -> NegotiatedStdioProtocol {
        self.protocol_hints
            .read()
            .await
            .get(&server.name)
            .copied()
            .unwrap_or(NegotiatedStdioProtocol::ContentLength)
    }

    async fn allow_any_request_protocol_fallback(&self, server: &ServerConfig) -> bool {
        !self.protocol_hints.read().await.contains_key(&server.name)
    }

    async fn remember_protocol_hint(&self, server_name: &str, protocol: NegotiatedStdioProtocol) {
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
        if self.should_auto_detect_protocol(server).await {
            let (conn, response, protocol) = self
                .race_protocol_request(
                    server,
                    init_request,
                    timeout_duration,
                    defaults.max_response_wait_iterations,
                )
                .await?;
            self.remember_protocol_hint(&server.name, protocol).await;
            return Ok((conn, response));
        }

        let effective_protocol = self.effective_protocol_for(server).await;

        let mut conn = ProcessConnection::spawn(server, effective_protocol).await?;
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

                let fallback_protocol = alternate_protocol(effective_protocol);
                self.remember_protocol_hint(&server.name, fallback_protocol)
                    .await;

                let mut fallback_conn = ProcessConnection::spawn(server, fallback_protocol).await?;
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
        protocol: NegotiatedStdioProtocol,
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

        let guard = self.pooled.write().await;
        if let Some(entry) = guard.get(&server.name) {
            if entry.signature == signature {
                return Ok(entry.clone());
            }
        }

        // Do not await process spawn while holding pooled write lock.
        drop(guard);
        let mut conn = ProcessConnection::spawn(server, protocol).await?;

        let mut guard = self.pooled.write().await;
        if let Some(entry) = guard.get(&server.name) {
            if entry.signature == signature {
                let _ = conn.shutdown().await;
                return Ok(entry.clone());
            }
        }

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

fn is_initialize_request(request: &Value) -> bool {
    request
        .get("method")
        .and_then(Value::as_str)
        .is_some_and(|method| method == "initialize")
}

fn auto_detection_protocol_candidates() -> (NegotiatedStdioProtocol, NegotiatedStdioProtocol) {
    (
        NegotiatedStdioProtocol::ContentLength,
        NegotiatedStdioProtocol::JsonLines,
    )
}

struct AuthPromptInfo {
    url: String,
    browser_opened: bool,
    waiting_for_authorization: bool,
}

fn build_auto_detection_error(
    primary_protocol: NegotiatedStdioProtocol,
    primary_error: &AppError,
    primary_stderr: &str,
    secondary_protocol: NegotiatedStdioProtocol,
    secondary_error: &AppError,
    secondary_stderr: &str,
) -> AppError {
    if let Some(auth_prompt) = extract_auth_prompt(primary_stderr, secondary_stderr) {
        return AppError::Upstream(format!(
            "authentication required; authorize_url={}; browser_opened={}; waiting_for_authorization={}",
            auth_prompt.url, auth_prompt.browser_opened, auth_prompt.waiting_for_authorization
        ));
    }

    let stderr_excerpt = combined_stderr_excerpt(
        (protocol_label(primary_protocol), primary_stderr),
        (protocol_label(secondary_protocol), secondary_stderr),
    );
    if stderr_excerpt.is_empty() {
        AppError::Upstream(format!(
            "auto protocol detection failed ({}: {}; {}: {})",
            protocol_label(primary_protocol),
            primary_error,
            protocol_label(secondary_protocol),
            secondary_error
        ))
    } else {
        AppError::Upstream(format!(
            "auto protocol detection failed ({}: {}; {}: {}); stderr: {}",
            protocol_label(primary_protocol),
            primary_error,
            protocol_label(secondary_protocol),
            secondary_error,
            stderr_excerpt
        ))
    }
}

fn extract_auth_prompt(primary_stderr: &str, secondary_stderr: &str) -> Option<AuthPromptInfo> {
    let browser_opened =
        primary_stderr.contains("Browser opened automatically")
            || secondary_stderr.contains("Browser opened automatically");
    let waiting_for_authorization = primary_stderr.contains("Waiting for authorization")
        || primary_stderr.contains("Waiting for auth code")
        || secondary_stderr.contains("Waiting for authorization")
        || secondary_stderr.contains("Waiting for auth code");

    [primary_stderr, secondary_stderr]
        .into_iter()
        .find_map(extract_authorize_url)
        .map(|url| AuthPromptInfo {
            url,
            browser_opened,
            waiting_for_authorization,
        })
}

fn extract_authorize_url(stderr: &str) -> Option<String> {
    stderr
        .lines()
        .filter_map(extract_http_url)
        .find(|url| url.contains("/authorize?"))
        .or_else(|| stderr.lines().filter_map(extract_http_url).next())
}

fn extract_http_url(line: &str) -> Option<String> {
    line.split_whitespace().find_map(|token| {
        let trimmed = token.trim_matches(|ch: char| matches!(ch, '"' | '\'' | '(' | ')' | ',' ));
        if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
            Some(trimmed.to_string())
        } else {
            None
        }
    })
}

fn combined_stderr_excerpt(
    primary: (&str, &str),
    secondary: (&str, &str),
) -> String {
    let mut parts = Vec::new();
    let primary_excerpt = stderr_excerpt(primary.1);
    if !primary_excerpt.is_empty() {
        parts.push(format!("{} => {}", primary.0, primary_excerpt));
    }
    let secondary_excerpt = stderr_excerpt(secondary.1);
    if !secondary_excerpt.is_empty() {
        parts.push(format!("{} => {}", secondary.0, secondary_excerpt));
    }
    parts.join(" || ")
}

fn stderr_excerpt(stderr: &str) -> String {
    let lines = stderr
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    let start = lines.len().saturating_sub(6);
    lines[start..].join(" | ")
}
