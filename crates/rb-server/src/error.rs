use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

/// The standard error envelope: `{"error": {"code": ..., "message": ...}}`.
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
}

/// Wire form of [`ApiError`].
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct ErrorEnvelope {
    pub error: ErrorDetail,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct ErrorDetail {
    pub code: String,
    pub message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        ApiError { status, code, message: message.into() }
    }
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "bad_request", message)
    }
    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "unauthorized", message)
    }
    pub fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", "no route")
    }
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = ErrorEnvelope { error: ErrorDetail { code: self.code.to_string(), message: self.message } };
        (self.status, Json(body)).into_response()
    }
}
