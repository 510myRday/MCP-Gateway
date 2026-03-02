use std::collections::HashSet;
use std::net::SocketAddr;

use crate::error::AppError;

use super::model::GatewayConfig;

pub fn validate_config(cfg: &GatewayConfig) -> Result<(), AppError> {
    let listen_addr: SocketAddr = cfg
        .listen
        .parse()
        .map_err(|_| AppError::Validation(format!("invalid listen address: {}", cfg.listen)))?;

    if !cfg.allow_non_loopback && !listen_addr.ip().is_loopback() {
        return Err(AppError::Validation(
            "listen address must be loopback unless allowNonLoopback=true".to_string(),
        ));
    }

    validate_path(
        "transport.streamableHttp.basePath",
        &cfg.transport.streamable_http.base_path,
    )?;
    validate_path("transport.sse.basePath", &cfg.transport.sse.base_path)?;

    if cfg.security.admin.enabled && cfg.security.admin.token.trim().is_empty() {
        return Err(AppError::Validation(
            "security.admin.enabled=true requires non-empty security.admin.token".to_string(),
        ));
    }
    if cfg.security.mcp.enabled && cfg.security.mcp.token.trim().is_empty() {
        return Err(AppError::Validation(
            "security.mcp.enabled=true requires non-empty security.mcp.token".to_string(),
        ));
    }

    if cfg.defaults.request_timeout_ms < 1000 {
        return Err(AppError::Validation(
            "defaults.requestTimeoutMs must be >= 1000".to_string(),
        ));
    }
    if cfg.defaults.idle_ttl_ms < 1000 {
        return Err(AppError::Validation(
            "defaults.idleTtlMs must be >= 1000".to_string(),
        ));
    }
    if cfg.defaults.max_response_wait_iterations < 1 {
        return Err(AppError::Validation(
            "defaults.maxResponseWaitIterations must be >= 1".to_string(),
        ));
    }

    let mut names = HashSet::new();
    for server in &cfg.servers {
        if server.name.trim().is_empty() {
            return Err(AppError::Validation(
                "server.name cannot be empty".to_string(),
            ));
        }
        if !names.insert(server.name.clone()) {
            return Err(AppError::Validation(format!(
                "duplicate server.name: {}",
                server.name
            )));
        }
        if server.command.trim().is_empty() {
            return Err(AppError::Validation(format!(
                "server.command cannot be empty for {}",
                server.name
            )));
        }
    }

    Ok(())
}

fn validate_path(name: &str, value: &str) -> Result<(), AppError> {
    if !value.starts_with('/') {
        return Err(AppError::Validation(format!(
            "{name} must start with '/': {value}"
        )));
    }
    if value.contains(' ') {
        return Err(AppError::Validation(format!(
            "{name} cannot contain spaces: {value}"
        )));
    }
    Ok(())
}
