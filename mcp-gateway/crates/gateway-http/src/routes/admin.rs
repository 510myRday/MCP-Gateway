use std::net::{IpAddr, SocketAddr};

use axum::extract::{Path, Query, State};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use gateway_core::{AppError, GatewayConfig, ServerConfig};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::response::{self, ApiResult};
use crate::state::AppState;

pub fn router(state: AppState, api_prefix: &str) -> Router {
    let prefix = api_prefix.trim_end_matches('/');
    Router::new()
        .route(&format!("{}/admin/health", prefix), get(get_health))
        .route(
            &format!("{}/admin/config", prefix),
            get(get_config).put(put_config),
        )
        .route(
            &format!("{}/admin/servers", prefix),
            get(get_servers).post(post_server),
        )
        .route(
            &format!("{}/admin/servers/:server_name", prefix),
            put(put_server).delete(delete_server),
        )
        .route(
            &format!("{}/admin/servers/:server_name/test", prefix),
            post(test_server),
        )
        .route(
            &format!("{}/admin/servers/:server_name/tools", prefix),
            get(get_server_tools),
        )
        .route(
            &format!("{}/admin/export/mcp-servers", prefix),
            get(export_mcp_servers_payload),
        )
        .with_state(state)
}

#[derive(serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HealthData {
    started_at: chrono::DateTime<chrono::Utc>,
    uptime_seconds: i64,
    mode: String,
    listen: String,
    server_count: usize,
    version: &'static str,
}

#[utoipa::path(
    get,
    path = "/{api_prefix}/admin/health",
    responses((status = 200, description = "Gateway health"))
)]
pub async fn get_health(State(state): State<AppState>) -> ApiResult<HealthData> {
    let cfg = state.config_service.get_config().await;
    Ok(response::ok(HealthData {
        started_at: state.started_at,
        uptime_seconds: (chrono::Utc::now() - state.started_at).num_seconds(),
        mode: cfg.mode.to_string(),
        listen: cfg.listen,
        server_count: cfg.servers.len(),
        version: env!("CARGO_PKG_VERSION"),
    }))
}

#[utoipa::path(
    get,
    path = "/{api_prefix}/admin/config",
    responses((status = 200, description = "Current config"))
)]
pub async fn get_config(State(state): State<AppState>) -> ApiResult<GatewayConfig> {
    let cfg = state.config_service.get_config().await;
    Ok(response::ok(cfg))
}

#[utoipa::path(
    put,
    path = "/{api_prefix}/admin/config",
    request_body = GatewayConfig,
    responses((status = 200, description = "Updated config"))
)]
pub async fn put_config(
    State(state): State<AppState>,
    Json(next_config): Json<GatewayConfig>,
) -> ApiResult<GatewayConfig> {
    let updated = state
        .config_service
        .replace(next_config)
        .await
        .map_err(response::err_response)?;
    state.process_manager.reset_pool().await;
    Ok(response::ok(updated))
}

#[utoipa::path(
    get,
    path = "/{api_prefix}/admin/servers",
    responses((status = 200, description = "Server list"))
)]
pub async fn get_servers(State(state): State<AppState>) -> ApiResult<Vec<ServerConfig>> {
    let cfg = state.config_service.get_config().await;
    Ok(response::ok(cfg.servers))
}

#[utoipa::path(
    post,
    path = "/{api_prefix}/admin/servers",
    request_body = ServerConfig,
    responses((status = 200, description = "Server created"))
)]
pub async fn post_server(
    State(state): State<AppState>,
    Json(server): Json<ServerConfig>,
) -> ApiResult<ServerConfig> {
    let server_name = server.name.clone();
    state
        .config_service
        .update(|current| {
            if current.servers.iter().any(|item| item.name == server_name) {
                return Err(AppError::Conflict(format!(
                    "server already exists: {server_name}"
                )));
            }
            let mut cfg = current.clone();
            cfg.servers.push(server.clone());
            Ok(cfg)
        })
        .await
        .map_err(response::err_response)?;

    state.process_manager.evict_server(&server_name).await;
    Ok(response::ok(server))
}

#[utoipa::path(
    put,
    path = "/{api_prefix}/admin/servers/{server_name}",
    request_body = ServerConfig,
    params(("server_name" = String, Path, description = "Server name")),
    responses((status = 200, description = "Server updated"))
)]
pub async fn put_server(
    State(state): State<AppState>,
    Path(server_name): Path<String>,
    Json(server): Json<ServerConfig>,
) -> ApiResult<ServerConfig> {
    let next = server.clone();
    state
        .config_service
        .update(|current| {
            let Some(index) = current
                .servers
                .iter()
                .position(|item| item.name == server_name)
            else {
                return Err(AppError::NotFound("server not found".to_string()));
            };

            let mut cfg = current.clone();
            let mut updated_server = next.clone();
            updated_server.name = server_name.clone();
            cfg.servers[index] = updated_server;
            Ok(cfg)
        })
        .await
        .map_err(response::err_response)?;

    state.process_manager.evict_server(&server_name).await;
    Ok(response::ok(server))
}

#[utoipa::path(
    delete,
    path = "/{api_prefix}/admin/servers/{server_name}",
    params(("server_name" = String, Path, description = "Server name")),
    responses((status = 200, description = "Server deleted"))
)]
pub async fn delete_server(
    State(state): State<AppState>,
    Path(server_name): Path<String>,
) -> ApiResult<Value> {
    state
        .config_service
        .update(|current| {
            let before = current.servers.len();
            let mut cfg = current.clone();
            cfg.servers.retain(|item| item.name != server_name);
            if cfg.servers.len() == before {
                return Err(AppError::NotFound("server not found".to_string()));
            }
            Ok(cfg)
        })
        .await
        .map_err(response::err_response)?;

    state.process_manager.evict_server(&server_name).await;
    Ok(response::ok(json!({"deleted": server_name})))
}

#[utoipa::path(
    post,
    path = "/{api_prefix}/admin/servers/{server_name}/test",
    params(("server_name" = String, Path, description = "Server name")),
    responses((status = 200, description = "Server test result"))
)]
pub async fn test_server(
    State(state): State<AppState>,
    Path(server_name): Path<String>,
) -> ApiResult<Value> {
    let cfg = state.config_service.get_config().await;
    let server = cfg
        .servers
        .iter()
        .find(|item| item.name == server_name)
        .cloned()
        .ok_or_else(|| {
            response::err_response(AppError::NotFound("server not found".to_string()))
        })?;

    let result = state
        .process_manager
        .test_server(&server, &cfg.defaults)
        .await
        .map_err(response::err_response)?;

    Ok(response::ok(result))
}

#[derive(Debug, Deserialize)]
pub struct ToolsQuery {
    refresh: Option<bool>,
}

#[utoipa::path(
    get,
    path = "/api/v2/admin/servers/{server_name}/tools",
    params(
        ("server_name" = String, Path, description = "Server name"),
        ("refresh" = Option<bool>, Query, description = "Force refresh from upstream")
    ),
    responses((status = 200, description = "Server tools"))
)]
pub async fn get_server_tools(
    State(state): State<AppState>,
    Path(server_name): Path<String>,
    Query(query): Query<ToolsQuery>,
) -> ApiResult<Value> {
    let cfg = state.config_service.get_config().await;
    let server = cfg
        .servers
        .iter()
        .find(|item| item.name == server_name)
        .cloned()
        .ok_or_else(|| {
            response::err_response(AppError::NotFound("server not found".to_string()))
        })?;

    let refresh = query.refresh.unwrap_or(true);
    let result = state
        .process_manager
        .list_tools(&server, &cfg.defaults, refresh)
        .await
        .map_err(response::err_response)?;

    Ok(response::ok(json!({"refresh": refresh, "result": result})))
}

#[utoipa::path(
    get,
    path = "/api/v2/admin/export/mcp-servers",
    responses((status = 200, description = "Export MCP server payload"))
)]
pub async fn export_mcp_servers_payload(State(state): State<AppState>) -> ApiResult<Value> {
    let cfg = state.config_service.get_config().await;
    let base_url = gateway_base_url(&cfg.listen).map_err(response::err_response)?;

    let mut mcp_servers = serde_json::Map::new();
    for server in cfg.servers.iter().filter(|item| item.enabled) {
        let url = format!(
            "{}{}{}",
            base_url,
            cfg.transport
                .streamable_http
                .base_path
                .trim_end_matches('/'),
            format_args!("/{}", server.name)
        );

        let mut entry = serde_json::Map::new();
        entry.insert("name".to_string(), Value::String(server.display_name()));
        entry.insert(
            "type".to_string(),
            Value::String("streamable-http".to_string()),
        );
        entry.insert("url".to_string(), Value::String(url));

        if cfg.security.mcp.enabled {
            entry.insert(
                "headers".to_string(),
                json!({"Authorization": format!("Bearer {}", cfg.security.mcp.token)}),
            );
        }

        mcp_servers.insert(server.name.clone(), Value::Object(entry));
    }

    Ok(response::ok(
        json!({"mcpServers": Value::Object(mcp_servers)}),
    ))
}

fn gateway_base_url(listen: &str) -> Result<String, AppError> {
    let addr: SocketAddr = listen
        .parse()
        .map_err(|_| AppError::Validation(format!("invalid listen address: {listen}")))?;

    let host = match addr.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => "127.0.0.1".to_string(),
        IpAddr::V6(ip) if ip.is_unspecified() => "[::1]".to_string(),
        IpAddr::V4(ip) => ip.to_string(),
        IpAddr::V6(ip) => format!("[{ip}]"),
    };

    Ok(format!("http://{host}:{}", addr.port()))
}
