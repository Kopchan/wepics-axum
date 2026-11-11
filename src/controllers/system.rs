use axum::Json;
use axum::extract::State;
use serde_json::{Value, json};

use crate::error::AppError;
use crate::state::AppState;

/// Proves the database is reachable, not just that the process is up.
pub async fn health(State(state): State<AppState>) -> Result<Json<Value>, AppError> {
    sqlx::query_scalar::<_, i64>("SELECT 1")
        .fetch_one(&state.db)
        .await?;

    Ok(Json(json!({ "status": "ok" })))
}
