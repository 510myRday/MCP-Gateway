mod auth;
mod openapi;
mod response;
mod routes;
mod state;

use std::time::Duration;

use axum::http::{header, HeaderValue, Method};
use axum::Router;
use gateway_core::GatewayConfig;
use tower_http::cors::{AllowOrigin, CorsLayer};

pub use openapi::ApiDoc;
pub use state::{AppState, SseHub};

pub fn build_router(state: AppState, config: &GatewayConfig) -> Router {
    Router::new()
        .merge(routes::gateway_router(state.clone(), config))
        .merge(routes::admin_router(state.clone(), &config.api_prefix))
        .merge(openapi::openapi_router(&config.api_prefix))
        .layer(build_cors_layer())
}

pub fn spawn_idle_reaper(state: AppState) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(15)).await;
            let cfg = state.config_service.get_config().await;
            state
                .process_manager
                .reap_idle(Duration::from_millis(cfg.defaults.idle_ttl_ms))
                .await;
        }
    });
}

fn build_cors_layer() -> CorsLayer {
    let allow_origin = AllowOrigin::predicate(|origin: &HeaderValue, _| {
        let Ok(origin) = origin.to_str() else {
            return false;
        };

        origin == "tauri://localhost"
            || origin == "http://tauri.localhost"
            || origin == "https://tauri.localhost"
            || origin.starts_with("http://localhost:")
            || origin.starts_with("http://127.0.0.1:")
            || origin.starts_with("https://localhost:")
            || origin.starts_with("https://127.0.0.1:")
    });

    CorsLayer::new()
        .allow_origin(allow_origin)
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE, header::ACCEPT])
}
