use std::sync::Arc;

use axum::extract::State;
use axum::http::header;
use axum::response::IntoResponse;
use axum::Json;
use serde_json::{json, Value};

use crate::AppState;

pub async fn favicon() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "image/x-icon"), (header::CACHE_CONTROL, "public, max-age=604800")],
        include_bytes!("../../favicon.ico").as_slice(),
    )
}

pub async fn health(State(state): State<Arc<AppState>>) -> Json<Value> {
    let start = std::time::Instant::now();
    let db_ok = sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(&state.pool)
        .await
        .is_ok();
    let db_latency = start.elapsed().as_millis() as u64;

    let status = if db_ok { "healthy" } else { "degraded" };

    Json(json!({
        "status": status,
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "checks": {
            "database": {
                "status": if db_ok { "up" } else { "down" },
                "latency_ms": db_latency
            }
        },
    }))
}
