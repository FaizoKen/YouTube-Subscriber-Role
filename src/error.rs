use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("YouTube API error: {0}")]
    YouTube(#[from] YouTubeError),

    #[error("RoleLogic API error: {0}")]
    RoleLogic(String),

    #[error("Role link user limit reached ({limit})")]
    UserLimitReached { limit: usize },

    #[error("Invalid request: {0}")]
    BadRequest(String),

    #[error("Unauthorized")]
    Unauthorized,

    #[error("Unauthorized: {0}")]
    UnauthorizedWith(String),

    #[error("Forbidden: {0}")]
    Forbidden(String),

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Internal error: {0}")]
    Internal(String),
}

#[derive(Debug, thiserror::Error)]
pub enum YouTubeError {
    #[error("Google token expired")]
    TokenExpired,
    #[error("Google token revoked")]
    TokenRevoked,
    #[error("YouTube API quota exceeded")]
    QuotaExceeded,
    #[error("YouTube API not found")]
    NotFound,
    #[error("Network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("YouTube API error: {0}")]
    ApiError(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            AppError::Database(e) => {
                tracing::error!("Database error: {e}");
                (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error")
            }
            AppError::YouTube(YouTubeError::TokenExpired | YouTubeError::TokenRevoked) => {
                (StatusCode::UNAUTHORIZED, "YouTube authorization expired. Please re-link your account.")
            }
            AppError::YouTube(YouTubeError::QuotaExceeded) => {
                (StatusCode::TOO_MANY_REQUESTS, "YouTube API quota exceeded. Please try again later.")
            }
            AppError::YouTube(e) => {
                tracing::error!("YouTube API error: {e}");
                (StatusCode::BAD_GATEWAY, "Failed to check YouTube subscription. Please try again later.")
            }
            AppError::RoleLogic(e) => {
                tracing::error!("RoleLogic API error: {e}");
                (StatusCode::BAD_GATEWAY, "Failed to sync roles")
            }
            AppError::UserLimitReached { limit } => {
                tracing::warn!("Role link user limit reached: {limit}");
                (StatusCode::FORBIDDEN, "Role link user limit reached")
            }
            AppError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg.as_str()),
            AppError::Unauthorized => (StatusCode::UNAUTHORIZED, "Invalid or missing authorization"),
            AppError::UnauthorizedWith(msg) => (StatusCode::UNAUTHORIZED, msg.as_str()),
            AppError::Forbidden(msg) => (StatusCode::FORBIDDEN, msg.as_str()),
            AppError::NotFound(msg) => (StatusCode::NOT_FOUND, msg.as_str()),
            AppError::Internal(e) => {
                tracing::error!("Internal error: {e}");
                (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error")
            }
        };

        let body = json!({ "error": message });
        (status, axum::Json(body)).into_response()
    }
}
