use std::convert::Infallible;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::stream::{self, Stream, StreamExt};
use gateway_core::{AppError, GatewayConfig};
use serde_json::{json, Value};
use tokio_stream::wrappers::BroadcastStream;
use uuid::Uuid;

use crate::response;
use crate::state::AppState;

const MCP_SESSION_ID_HEADER: &str = "mcp-session-id";

/// MCP 代理端点的错误响应 - 返回 JSON-RPC 2.0 格式的错误
fn mcp_error(id: Option<Value>, code: i32, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "error": {
            "code": code,
            "message": message
        },
        "id": sanitize_jsonrpc_id(id)
    })
}

fn sanitize_jsonrpc_id(id: Option<Value>) -> Value {
    match id {
        Some(Value::String(value)) => Value::String(value),
        Some(Value::Number(value)) => Value::Number(value),
        _ => json!(0),
    }
}

fn extract_request_id(request: &Value) -> Option<Value> {
    request.get("id").cloned()
}

fn is_initialize_request(request: &Value) -> bool {
    request
        .get("method")
        .and_then(Value::as_str)
        .is_some_and(|method| method == "initialize")
}

fn is_notification_message(request: &Value) -> bool {
    request.get("method").is_some() && request.get("id").is_none()
}

fn session_header_value(headers: &HeaderMap) -> Option<String> {
    headers
        .get(MCP_SESSION_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn session_scoped_server_name(server_name: &str, session_id: &str) -> String {
    format!("{server_name}::session::{session_id}")
}

fn session_scoped_server(
    server: &gateway_core::ServerConfig,
    session_id: Option<&str>,
) -> gateway_core::ServerConfig {
    let Some(session_id) = session_id else {
        return server.clone();
    };

    let mut scoped = server.clone();
    scoped.name = session_scoped_server_name(&server.name, session_id);
    scoped
}

fn json_response(
    status: StatusCode,
    payload: Value,
    session_id: Option<&str>,
) -> axum::response::Response {
    let mut response = (status, Json(payload)).into_response();
    if let Some(session_id) = session_id {
        if let Ok(value) = HeaderValue::from_str(session_id) {
            response.headers_mut().insert(MCP_SESSION_ID_HEADER, value);
        }
    }
    response
}

fn empty_response(status: StatusCode, session_id: Option<&str>) -> axum::response::Response {
    let mut response = status.into_response();
    if let Some(session_id) = session_id {
        if let Ok(value) = HeaderValue::from_str(session_id) {
            response.headers_mut().insert(MCP_SESSION_ID_HEADER, value);
        }
    }
    response
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
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> axum::response::Response {
    let cfg = state.config_service.get_config().await;
    let request_id = extract_request_id(&request);
    let is_notification = is_notification_message(&request);
    let incoming_session_id = session_header_value(&headers);
    let generated_session_id = if incoming_session_id.is_none() && is_initialize_request(&request) {
        Some(Uuid::new_v4().to_string())
    } else {
        None
    };
    let effective_session_id = incoming_session_id
        .as_deref()
        .or(generated_session_id.as_deref());

    if state.skills.is_skills_server(&cfg, &server_name) {
        let result = state.skills.handle_mcp_request(&cfg, request).await;
        if is_notification {
            return empty_response(StatusCode::ACCEPTED, effective_session_id);
        }
        return json_response(StatusCode::OK, result, effective_session_id);
    }

    let server = cfg
        .servers
        .iter()
        .find(|item| item.name == server_name)
        .cloned()
        .ok_or_else(|| mcp_error(request_id.clone(), -32001, "server not found"));
    let server = match server {
        Ok(server) => server,
        Err(error) => return json_response(StatusCode::OK, error, effective_session_id),
    };

    if !server.enabled {
        return json_response(
            StatusCode::OK,
            mcp_error(request_id, -32002, "server is disabled"),
            effective_session_id,
        );
    }

    let scoped_server = session_scoped_server(&server, effective_session_id);
    let result = state
        .process_manager
        .call_server(&scoped_server, &cfg.defaults, request)
        .await
        .map_err(|e| mcp_error(request_id, -32603, &e.to_string()));
    let result = match result {
        Ok(result) => result,
        Err(error) => return json_response(StatusCode::OK, error, effective_session_id),
    };

    if is_notification {
        return empty_response(StatusCode::ACCEPTED, effective_session_id);
    }

    if let Ok(payload) = serde_json::to_string(&result) {
        let channel = effective_session_id
            .map(|session_id| session_scoped_server_name(&server.name, session_id))
            .unwrap_or_else(|| server.name.clone());
        state.sse_hub.publish(&channel, payload).await;
    }

    // 直接返回原始 JSON-RPC 响应，不做包装
    json_response(StatusCode::OK, result, effective_session_id)
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
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> axum::response::Response {
    let cfg = state.config_service.get_config().await;
    let request_id = extract_request_id(&request);
    let is_notification = is_notification_message(&request);
    let session_id = session_header_value(&headers);

    if state.skills.is_skills_server(&cfg, &server_name) {
        let result = state.skills.handle_mcp_request(&cfg, request).await;
        if is_notification {
            return empty_response(StatusCode::ACCEPTED, session_id.as_deref());
        }
        if let Ok(payload) = serde_json::to_string(&result) {
            let channel = session_id
                .as_deref()
                .map(|value| session_scoped_server_name(&server_name, value))
                .unwrap_or_else(|| server_name.clone());
            state.sse_hub.publish(&channel, payload).await;
        }
        return json_response(StatusCode::OK, result, session_id.as_deref());
    }

    let server = cfg
        .servers
        .iter()
        .find(|item| item.name == server_name)
        .cloned()
        .ok_or_else(|| mcp_error(request_id.clone(), -32001, "server not found"));
    let server = match server {
        Ok(server) => server,
        Err(error) => return json_response(StatusCode::OK, error, session_id.as_deref()),
    };

    if !server.enabled {
        return json_response(
            StatusCode::OK,
            mcp_error(request_id, -32002, "server is disabled"),
            session_id.as_deref(),
        );
    }

    let scoped_server = session_scoped_server(&server, session_id.as_deref());
    let result = state
        .process_manager
        .call_server(&scoped_server, &cfg.defaults, request)
        .await
        .map_err(|e| mcp_error(request_id, -32603, &e.to_string()));
    let result = match result {
        Ok(result) => result,
        Err(error) => return json_response(StatusCode::OK, error, session_id.as_deref()),
    };

    if is_notification {
        return empty_response(StatusCode::ACCEPTED, session_id.as_deref());
    }

    if let Ok(payload) = serde_json::to_string(&result) {
        let channel = session_id
            .as_deref()
            .map(|value| session_scoped_server_name(&server.name, value))
            .unwrap_or_else(|| server.name.clone());
        state.sse_hub.publish(&channel, payload).await;
    }

    // 直接返回原始 JSON-RPC 响应，不做包装
    json_response(StatusCode::OK, result, session_id.as_deref())
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
    headers: HeaderMap,
) -> Result<
    Sse<impl Stream<Item = Result<Event, Infallible>>>,
    (
        axum::http::StatusCode,
        Json<crate::response::ApiEnvelope<Value>>,
    ),
> {
    let cfg = state.config_service.get_config().await;
    let session_id = session_header_value(&headers);

    let server_exists = cfg
        .servers
        .iter()
        .any(|item| item.name == server_name && item.enabled)
        || state.skills.is_skills_server(&cfg, &server_name);
    if !server_exists {
        return Err(response::err_response(AppError::NotFound(
            "server not found or disabled".to_string(),
        )));
    }

    let channel = session_id
        .as_deref()
        .map(|value| session_scoped_server_name(&server_name, value))
        .unwrap_or_else(|| server_name.clone());
    let receiver = state.sse_hub.subscribe(&channel).await;
    let initial_server_name = channel.clone();
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
