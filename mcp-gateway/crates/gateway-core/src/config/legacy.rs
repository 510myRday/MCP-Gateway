use std::collections::HashMap;
use std::fs;
use std::path::Path;

use serde::Deserialize;

use crate::error::AppError;

use super::model::{
    normalize_config_in_place, save_config_atomic, DefaultsConfig, GatewayConfig, RunMode,
    ServerConfig, StdioProtocol,
};
use super::validate::validate_config;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyGatewayConfig {
    #[serde(default)]
    listen: String,
    #[serde(default)]
    allow_non_loopback: bool,
    #[serde(default)]
    mode: RunMode,
    #[serde(default)]
    security: serde_json::Value,
    #[serde(default)]
    transport: serde_json::Value,
    #[serde(default)]
    defaults: LegacyDefaults,
    #[serde(default)]
    servers: Vec<LegacyServerConfig>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct LegacyDefaults {
    #[serde(default)]
    lifecycle: Option<super::model::LifecycleMode>,
    #[serde(default)]
    idle_ttl_ms: Option<u64>,
    #[serde(default)]
    request_timeout_ms: Option<u64>,
    #[serde(default)]
    max_retries: Option<u32>,
    #[serde(default)]
    max_response_wait_iterations: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyServerConfig {
    #[serde(default)]
    name: String,
    #[serde(default)]
    id: String,
    #[serde(default, alias = "describe", alias = "description")]
    description_raw: String,
    #[serde(default)]
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    cwd: String,
    #[serde(default)]
    env: HashMap<String, String>,
    #[serde(default)]
    lifecycle: Option<super::model::LifecycleMode>,
    #[serde(default)]
    stdio_protocol: StdioProtocol,
    #[serde(default = "default_enabled")]
    enabled: bool,
}

fn default_enabled() -> bool {
    true
}

pub fn migrate_v1_to_v2_file(input: &Path, output: &Path) -> Result<GatewayConfig, AppError> {
    let content = fs::read_to_string(input)?;
    let legacy: LegacyGatewayConfig = serde_json::from_str(&content)?;

    let mut cfg = GatewayConfig {
        version: 2,
        listen: if legacy.listen.trim().is_empty() {
            "127.0.0.1:8765".to_string()
        } else {
            legacy.listen
        },
        allow_non_loopback: legacy.allow_non_loopback,
        mode: legacy.mode,
        api_prefix: "/api/v2".to_string(),
        security: serde_json::from_value(legacy.security).unwrap_or_default(),
        transport: serde_json::from_value(legacy.transport).unwrap_or_default(),
        defaults: DefaultsConfig {
            lifecycle: legacy.defaults.lifecycle.unwrap_or_default(),
            idle_ttl_ms: legacy.defaults.idle_ttl_ms.unwrap_or(300_000),
            request_timeout_ms: legacy.defaults.request_timeout_ms.unwrap_or(60_000),
            max_retries: legacy.defaults.max_retries.unwrap_or(2),
            max_response_wait_iterations: legacy
                .defaults
                .max_response_wait_iterations
                .unwrap_or(100),
        },
        servers: legacy
            .servers
            .into_iter()
            .map(|item| ServerConfig {
                name: if item.name.trim().is_empty() {
                    item.id
                } else {
                    item.name
                },
                description: item.description_raw,
                command: item.command,
                args: item.args,
                cwd: item.cwd,
                env: item.env,
                lifecycle: item.lifecycle,
                stdio_protocol: item.stdio_protocol,
                enabled: item.enabled,
            })
            .collect(),
    };

    normalize_config_in_place(&mut cfg);
    validate_config(&cfg)?;
    save_config_atomic(output, &cfg)?;
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use tempfile::NamedTempFile;

    use super::*;

    #[test]
    fn migrate_describe_to_description() {
        let input = NamedTempFile::new().expect("temp input");
        let output = NamedTempFile::new().expect("temp output");

        fs::write(
            input.path(),
            r#"{
  "version": 1,
  "listen": "127.0.0.1:8765",
  "security": {"mcp": {"enabled": false, "token": ""}, "admin": {"enabled": true, "token": "abc"}},
  "transport": {"streamableHttp": {"basePath": "/mcp"}, "sse": {"basePath": "/sse"}},
  "defaults": {"lifecycle": "pooled", "idleTtlMs": 300000, "requestTimeoutMs": 60000, "maxRetries": 2},
  "servers": [{"name": "fs", "describe": "Filesystem", "command": "npx", "args": []}]
}"#,
        )
        .expect("write input");

        let cfg = migrate_v1_to_v2_file(input.path(), output.path()).expect("migrate");
        assert_eq!(cfg.version, 2);
        assert_eq!(cfg.servers[0].description, "Filesystem");
        assert_eq!(cfg.defaults.max_response_wait_iterations, 100);
    }
}
