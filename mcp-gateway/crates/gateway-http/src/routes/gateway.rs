use std::convert::Infallible;

use axum::extract::{Path, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::stream::{self, Stream, StreamExt};
use gateway_core::{AppError, GatewayConfig};
use serde_json::{json, Value};
use tokio_stream::wrappers::BroadcastStream;

use crate::response;
use crate::state::AppState;

/// MCP 代理端点的错误响应 - 返回 JSON-RPC 2.0 格式的错误
fn mcp_error(code: i32, message: &str) -> (axum::http::StatusCode, Json<Value>) {
    (
        axum::http::StatusCode::OK, // JSON-RPC 错误也返回 200
        Json(json!({
            "jsonrpc": "2.0",
            "error": {
                "code": code,
                "message": message
            },
            "id": null
        })),
    )
}

pub fn router(state: AppState, config: &GatewayConfig) -> Router {
    let http_path = format!(
        "{}/:server_name",
        config.transport.streamable_http.base_path
    );
    let sse_path = format!("{}/:server_name", config.transport.sse.base_path);

    Router::new()
        .route(&http_path, post(handle_mcp_http))
        .route(&sse_path, get(handle_sse_subscribe).post(handle_sse_post))
        .with_state(state)
}

#[utoipa::path(
    post,
    path = "/{api_prefix}/mcp/{server_name}",
    request_body = Object,
    params(("server_name" = String, Path, description = "Server name")),
    responses((status = 200, description = "MCP response - raw JSON-RPC 2.0"))
)]
pub async fn handle_mcp_http(
    State(state): State<AppState>,
    Path(server_name): Path<String>,
    Json(request): Json<Value>,
) -> Result<Json<Value>, (axum::http::StatusCode, Json<Value>)> {
    let cfg = state.config_service.get_config().await;
    let server = cfg
        .servers
        .iter()
        .find(|item| item.name == server_name)
        .cloned()
        .ok_or_else(|| mcp_error(-32001, "server not found"))?;

    if !server.enabled {
        return Err(mcp_error(-32002, "server is disabled"));
    }

    let result = state
        .process_manager
        .call_server(&server, &cfg.defaults, request)
        .await
        .map_err(|e| mcp_error(-32603, &e.to_string()))?;

    // 直接返回原始 JSON-RPC 响应，不做包装
    Ok(Json(result))
}

#[utoipa::path(
    post,
    path = "/{api_prefix}/sse/{server_name}",
    request_body = Object,
    params(("server_name" = String, Path, description = "Server name")),
    responses((status = 200, description = "SSE bridge response - raw JSON-RPC 2.0"))
)]
pub async fn handle_sse_post(
    State(state): State<AppState>,
    Path(server_name): Path<String>,
    Json(request): Json<Value>,
) -> Result<Json<Value>, (axum::http::StatusCode, Json<Value>)> {
    let cfg = state.config_service.get_config().await;
    let server = cfg
        .servers
        .iter()
        .find(|item| item.name == server_name)
        .cloned()
        .ok_or_else(|| mcp_error(-32001, "server not found"))?;

    if !server.enabled {
        return Err(mcp_error(-32002, "server is disabled"));
    }

    let result = state
        .process_manager
        .call_server(&server, &cfg.defaults, request)
        .await
        .map_err(|e| mcp_error(-32603, &e.to_string()))?;

    if let Ok(payload) = serde_json::to_string(&result) {
        state.sse_hub.publish(&server.name, payload).await;
    }

    // 直接返回原始 JSON-RPC 响应，不做包装
    Ok(Json(result))
}

#[utoipa::path(
    get,
    path = "/{api_prefix}/sse/{server_name}",
    params(("server_name" = String, Path, description = "Server name")),
    responses((status = 200, description = "SSE stream"))
)]
pub async fn handle_sse_subscribe(
    State(state): State<AppState>,
    Path(server_name): Path<String>,
) -> Result<
    Sse<impl Stream<Item = Result<Event, Infallible>>>,
    (
        axum::http::StatusCode,
        Json<crate::response::ApiEnvelope<Value>>,
    ),
> {
    let cfg = state.config_service.get_config().await;

    let server_exists = cfg
        .servers
        .iter()
        .any(|item| item.name == server_name && item.enabled);
    if !server_exists {
        return Err(response::err_response(AppError::NotFound(
            "server not found or disabled".to_string(),
        )));
    }

    let receiver = state.sse_hub.subscribe(&server_name).await;
    let initial_server_name = server_name.clone();
    let initial = stream::once(async move {
        Ok(Event::default().event("ready").data(
            json!({"ok": true, "serverId": initial_server_name, "message": "connected"})
                .to_string(),
        ))
    });

    let updates = BroadcastStream::new(receiver).filter_map(|item| async move {
        match item {
            Ok(payload) => Some(Ok(Event::default().event("message").data(payload))),
            Err(_) => None,
        }
    });

    let stream = initial.chain(updates);
    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(std::time::Duration::from_secs(10))))
}
