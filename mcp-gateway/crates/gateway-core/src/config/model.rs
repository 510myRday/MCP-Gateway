use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::fs;
use std::path::{Path, PathBuf};

use rand::distributions::{Alphanumeric, DistString};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::error::AppError;

use super::validate::validate_config;

const CONFIG_DIR_NAME: &str = "mcp-gateway";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunMode {
    Extension,
    General,
    #[default]
    Both,
}

impl Display for RunMode {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Extension => write!(f, "extension"),
            Self::General => write!(f, "general"),
            Self::Both => write!(f, "both"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleMode {
    #[default]
    Pooled,
    PerRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum StdioProtocol {
    #[default]
    Auto,
    #[serde(alias = "contentLength", alias = "content-length")]
    ContentLength,
    #[serde(alias = "jsonl", alias = "jsonLines", alias = "json-lines")]
    JsonLines,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GatewayConfig {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default = "default_listen")]
    pub listen: String,
    #[serde(default)]
    pub allow_non_loopback: bool,
    #[serde(default)]
    pub mode: RunMode,
    #[serde(default = "default_api_prefix")]
    pub api_prefix: String,
    #[serde(default)]
    pub security: SecurityConfig,
    #[serde(default)]
    pub transport: TransportConfig,
    #[serde(default)]
    pub defaults: DefaultsConfig,
    #[serde(default)]
    pub servers: Vec<ServerConfig>,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            version: default_version(),
            listen: default_listen(),
            allow_non_loopback: false,
            mode: RunMode::Both,
            api_prefix: default_api_prefix(),
            security: SecurityConfig::default(),
            transport: TransportConfig::default(),
            defaults: DefaultsConfig::default(),
            servers: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SecurityConfig {
    #[serde(default)]
    pub mcp: TokenConfig,
    #[serde(default = "default_admin_token_config")]
    pub admin: TokenConfig,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            mcp: TokenConfig {
                enabled: false,
                token: String::new(),
            },
            admin: default_admin_token_config(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TokenConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TransportConfig {
    #[serde(default = "default_streamable_http_path", rename = "streamableHttp")]
    pub streamable_http: TransportPath,
    #[serde(default = "default_sse_path")]
    pub sse: TransportPath,
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            streamable_http: default_streamable_http_path(),
            sse: default_sse_path(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TransportPath {
    #[serde(default)]
    pub base_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DefaultsConfig {
    #[serde(default)]
    pub lifecycle: LifecycleMode,
    #[serde(default = "default_idle_ttl_ms")]
    pub idle_ttl_ms: u64,
    #[serde(default = "default_request_timeout_ms")]
    pub request_timeout_ms: u64,
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    #[serde(default = "default_max_response_wait_iterations")]
    pub max_response_wait_iterations: u32,
}

impl Default for DefaultsConfig {
    fn default() -> Self {
        Self {
            lifecycle: LifecycleMode::Pooled,
            idle_ttl_ms: default_idle_ttl_ms(),
            request_timeout_ms: default_request_timeout_ms(),
            max_retries: default_max_retries(),
            max_response_wait_iterations: default_max_response_wait_iterations(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ServerConfig {
    #[serde(default)]
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub cwd: String,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub lifecycle: Option<LifecycleMode>,
    #[serde(default)]
    pub stdio_protocol: StdioProtocol,
    #[serde(default = "default_server_enabled")]
    pub enabled: bool,
}

impl ServerConfig {
    pub fn display_name(&self) -> String {
        if self.description.trim().is_empty() {
            self.name.clone()
        } else {
            self.description.clone()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenScope {
    Admin,
    Mcp,
}

fn default_version() -> u32 {
    2
}

fn default_listen() -> String {
    "127.0.0.1:8765".to_string()
}

fn default_streamable_http_path() -> TransportPath {
    TransportPath {
        base_path: "/api/v2/mcp".to_string(),
    }
}

fn default_sse_path() -> TransportPath {
    TransportPath {
        base_path: "/api/v2/sse".to_string(),
    }
}

fn default_idle_ttl_ms() -> u64 {
    300_000
}

fn default_request_timeout_ms() -> u64 {
    60_000
}

fn default_max_retries() -> u32 {
    2
}

fn default_max_response_wait_iterations() -> u32 {
    100
}

fn default_admin_token_config() -> TokenConfig {
    TokenConfig {
        enabled: true,
        token: String::new(),
    }
}

fn default_api_prefix() -> String {
    "/api/v2".to_string()
}

fn default_server_enabled() -> bool {
    true
}

pub fn generate_token() -> String {
    Alphanumeric.sample_string(&mut rand::rngs::OsRng, 40)
}

pub fn default_config_path() -> Result<PathBuf, AppError> {
    let mut base =
        dirs::config_dir().ok_or_else(|| AppError::Internal("Invalid config path".to_string()))?;
    base.push(CONFIG_DIR_NAME);
    base.push("config.v2.json");
    Ok(base)
}

pub fn load_config_from_path(path: &Path) -> Result<GatewayConfig, AppError> {
    let text = fs::read_to_string(path)?;
    let mut cfg: GatewayConfig = serde_json::from_str(&text)?;
    normalize_config_in_place(&mut cfg);
    validate_config(&cfg)?;
    Ok(cfg)
}

pub fn init_default_config(path: &Path, mode: RunMode) -> Result<GatewayConfig, AppError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut cfg = GatewayConfig {
        mode,
        ..GatewayConfig::default()
    };
    cfg.security.admin.token = generate_token();
    cfg.security.mcp.token = generate_token();
    normalize_config_in_place(&mut cfg);
    validate_config(&cfg)?;
    save_config_atomic(path, &cfg)?;
    Ok(cfg)
}

pub fn save_config_atomic(path: &Path, cfg: &GatewayConfig) -> Result<(), AppError> {
    let parent = path
        .parent()
        .ok_or_else(|| AppError::Internal("Invalid config path".to_string()))?;
    fs::create_dir_all(parent)?;

    let tmp_name = format!(".config-{}.tmp", Uuid::new_v4());
    let tmp_path = parent.join(tmp_name);
    let data = serde_json::to_vec_pretty(cfg)?;

    fs::write(&tmp_path, data)?;

    #[cfg(target_os = "windows")]
    {
        if path.exists() {
            fs::remove_file(path)?;
        }
    }

    fs::rename(tmp_path, path)?;
    Ok(())
}

pub fn rotate_token(path: &Path, scope: TokenScope) -> Result<String, AppError> {
    let mut cfg = load_config_from_path(path)?;
    let token = generate_token();
    match scope {
        TokenScope::Admin => cfg.security.admin.token = token.clone(),
        TokenScope::Mcp => cfg.security.mcp.token = token.clone(),
    }
    save_config_atomic(path, &cfg)?;
    Ok(token)
}

pub fn apply_runtime_overrides(
    cfg: &mut GatewayConfig,
    mode: Option<RunMode>,
    listen: Option<String>,
) {
    if let Some(m) = mode {
        cfg.mode = m;
    }
    if let Some(l) = listen {
        cfg.listen = l;
    }
    normalize_config_in_place(cfg);
}

pub fn normalize_config_in_place(cfg: &mut GatewayConfig) {
    cfg.version = 2;
    cfg.listen = cfg.listen.trim().to_string();
    cfg.transport.streamable_http.base_path =
        normalize_path(&cfg.transport.streamable_http.base_path, "/api/v2/mcp");
    cfg.transport.sse.base_path = normalize_path(&cfg.transport.sse.base_path, "/api/v2/sse");

    cfg.security.admin.token = cfg.security.admin.token.trim().to_string();
    cfg.security.mcp.token = cfg.security.mcp.token.trim().to_string();

    for server in &mut cfg.servers {
        server.name = server.name.trim().to_string();
        server.description = server.description.trim().to_string();
        server.command = server.command.trim().to_string();
        server.cwd = server.cwd.trim().to_string();
        server.args = server
            .args
            .iter()
            .map(|x| x.trim().to_string())
            .filter(|x| !x.is_empty())
            .collect();

        server.env = server
            .env
            .iter()
            .filter_map(|(k, v)| {
                let key = k.trim().to_string();
                let value = v.trim().to_string();
                if key.is_empty() || value.is_empty() {
                    None
                } else {
                    Some((key, value))
                }
            })
            .collect();
    }
}

fn normalize_path(input: &str, fallback: &str) -> String {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return fallback.to_string();
    }
    let mut path = if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{trimmed}")
    };
    while path.ends_with('/') && path.len() > 1 {
        path.pop();
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        let mut cfg = GatewayConfig::default();
        cfg.security.admin.token = "abc".to_string();
        assert!(validate_config(&cfg).is_ok());
    }

    #[test]
    fn normalize_path_defaults() {
        let mut cfg = GatewayConfig::default();
        cfg.transport.streamable_http.base_path = "mcp/".to_string();
        normalize_config_in_place(&mut cfg);
        assert_eq!(cfg.transport.streamable_http.base_path, "/mcp");
        assert_eq!(cfg.version, 2);
    }
}
