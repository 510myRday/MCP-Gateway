pub mod admin;
pub mod gateway;

use axum::middleware;
use axum::Router;
use gateway_core::GatewayConfig;

use crate::auth::{require_admin_auth, require_mcp_auth};
use crate::state::AppState;

pub fn admin_router(state: AppState, api_prefix: &str) -> Router {
    admin::router(state.clone(), api_prefix)
        .route_layer(middleware::from_fn_with_state(state, require_admin_auth))
}

pub fn gateway_router(state: AppState, config: &GatewayConfig) -> Router {
    gateway::router(state.clone(), config)
        .route_layer(middleware::from_fn_with_state(state, require_mcp_auth))
}
