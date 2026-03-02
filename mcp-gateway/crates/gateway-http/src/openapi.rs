use axum::Router;
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

use crate::response::ApiErrorBody;
use crate::routes;

#[derive(OpenApi)]
#[openapi(
    paths(
        routes::admin::get_health,
        routes::admin::get_config,
        routes::admin::put_config,
        routes::admin::get_servers,
        routes::admin::post_server,
        routes::admin::put_server,
        routes::admin::delete_server,
        routes::admin::test_server,
        routes::admin::get_server_tools,
        routes::admin::export_mcp_servers_payload,
        routes::gateway::handle_mcp_http,
        routes::gateway::handle_sse_post,
        routes::gateway::handle_sse_subscribe,
    ),
    components(
        schemas(ApiErrorBody, gateway_core::GatewayConfig, gateway_core::ServerConfig)
    ),
    tags((name = "mcp-gateway", description = "MCP Gateway V2 API"))
)]
pub struct ApiDoc;

pub fn openapi_router(api_prefix: &str) -> Router {
    // SwaggerUi 会自动注册 /api/v2/openapi.json 端点，不需要手动注册
    let prefix = api_prefix.trim_end_matches('/');
    let docs_path: &'static str = Box::leak(format!("{}/docs", prefix).into_boxed_str());
    let openapi_path: &'static str = Box::leak(format!("{}/openapi.json", prefix).into_boxed_str());
    Router::new().merge(SwaggerUi::new(docs_path).url(openapi_path, ApiDoc::openapi()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openapi_document_serializes() {
        let doc = ApiDoc::openapi();
        let json = serde_json::to_string(&doc).expect("serialize openapi");
        assert!(json.contains("\"openapi\""));
    }
}
