use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use gateway_core::{AppError, ErrorCode};
use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiEnvelope<T>
where
    T: Serialize,
{
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ApiErrorBody>,
    pub request_id: String,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ApiErrorBody {
    pub code: ErrorCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<Object>)]
    pub details: Option<serde_json::Value>,
}

pub type ApiResult<T> =
    Result<Json<ApiEnvelope<T>>, (StatusCode, Json<ApiEnvelope<serde_json::Value>>)>;

pub fn ok<T>(data: T) -> Json<ApiEnvelope<T>>
where
    T: Serialize,
{
    Json(ApiEnvelope {
        ok: true,
        data: Some(data),
        error: None,
        request_id: Uuid::new_v4().to_string(),
    })
}

pub fn err_response(error: AppError) -> (StatusCode, Json<ApiEnvelope<serde_json::Value>>) {
    let status = map_status(error.code());
    let payload = ApiEnvelope {
        ok: false,
        data: None,
        error: Some(ApiErrorBody {
            code: error.code(),
            message: error.message(),
            details: None,
        }),
        request_id: Uuid::new_v4().to_string(),
    };
    (status, Json(payload))
}

pub fn map_status(code: ErrorCode) -> StatusCode {
    match code {
        ErrorCode::Unauthorized => StatusCode::UNAUTHORIZED,
        ErrorCode::NotFound => StatusCode::NOT_FOUND,
        ErrorCode::Conflict => StatusCode::CONFLICT,
        ErrorCode::ValidationFailed | ErrorCode::BadRequest => StatusCode::BAD_REQUEST,
        ErrorCode::UpstreamFailed => StatusCode::BAD_GATEWAY,
        ErrorCode::Internal => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

pub fn into_response(error: AppError) -> axum::response::Response {
    err_response(error).into_response()
}
