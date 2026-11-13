use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

/// Single error type for handlers: everything converts into a JSON response.
#[derive(Debug)]
pub enum AppError {
    NotFound,
    BadRequest(String),
    Forbidden(String),
    Internal(String),
    Database(sqlx::Error),
    Io(std::io::Error),
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppError::NotFound => write!(f, "not found"),
            AppError::BadRequest(message) => write!(f, "bad request: {message}"),
            AppError::Forbidden(message) => write!(f, "forbidden: {message}"),
            AppError::Internal(message) => write!(f, "internal error: {message}"),
            AppError::Database(err) => write!(f, "database error: {err}"),
            AppError::Io(err) => write!(f, "io error: {err}"),
        }
    }
}

impl std::error::Error for AppError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            AppError::NotFound
            | AppError::BadRequest(_)
            | AppError::Forbidden(_)
            | AppError::Internal(_) => None,
            AppError::Database(err) => Some(err),
            AppError::Io(err) => Some(err),
        }
    }
}

impl From<sqlx::Error> for AppError {
    fn from(err: sqlx::Error) -> Self {
        match err {
            sqlx::Error::RowNotFound => AppError::NotFound,
            other => AppError::Database(other),
        }
    }
}

impl From<std::io::Error> for AppError {
    fn from(err: std::io::Error) -> Self {
        if err.kind() == std::io::ErrorKind::NotFound {
            AppError::NotFound
        } else {
            AppError::Io(err)
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            AppError::NotFound => (StatusCode::NOT_FOUND, "not found".to_string()),
            AppError::BadRequest(message) => (StatusCode::BAD_REQUEST, message),
            AppError::Forbidden(message) => (StatusCode::FORBIDDEN, message),
            AppError::Internal(message) => {
                eprintln!("internal error: {message}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal server error".to_string(),
                )
            }
            // Internal details stay in logs, not in the response.
            AppError::Database(err) => {
                eprintln!("database error: {err}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal server error".to_string(),
                )
            }
            AppError::Io(err) => {
                eprintln!("io error: {err}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal server error".to_string(),
                )
            }
        };

        (status, Json(json!({ "error": message }))).into_response()
    }
}
