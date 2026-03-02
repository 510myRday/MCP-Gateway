use axum::extract::State;
use axum::http::{header, HeaderMap, Request};
use axum::middleware::Next;
use axum::response::Response;
use gateway_core::AppError;

use crate::response;
use crate::state::AppState;

pub async fn require_admin_auth(
    State(state): State<AppState>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let cfg = state.config_service.get_config().await;
    if let Err(error) = enforce_bearer(
        request.headers(),
        cfg.security.admin.enabled,
        &cfg.security.admin.token,
    ) {
        return response::into_response(error);
    }
    next.run(request).await
}

pub async fn require_mcp_auth(
    State(state): State<AppState>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let cfg = state.config_service.get_config().await;
    if let Err(error) = enforce_bearer(
        request.headers(),
        cfg.security.mcp.enabled,
        &cfg.security.mcp.token,
    ) {
        return response::into_response(error);
    }
    next.run(request).await
}

fn enforce_bearer(
    headers: &HeaderMap,
    enabled: bool,
    expected_token: &str,
) -> Result<(), AppError> {
    if !enabled {
        return Ok(());
    }

    let Some(raw) = headers.get(header::AUTHORIZATION) else {
        return Err(AppError::Unauthorized("missing bearer token".to_string()));
    };
    let Ok(value) = raw.to_str() else {
        return Err(AppError::Unauthorized(
            "invalid authorization header".to_string(),
        ));
    };
    let Some(token) = value.strip_prefix("Bearer ") else {
        return Err(AppError::Unauthorized("invalid bearer format".to_string()));
    };

    if token == expected_token {
        Ok(())
    } else {
        Err(AppError::Unauthorized("token mismatch".to_string()))
    }
}
