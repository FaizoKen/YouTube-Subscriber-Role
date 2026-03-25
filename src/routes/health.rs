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
    let row = sqlx::query_as::<_, (i64, i64)>(
        "SELECT \
           (SELECT COUNT(*) FROM linked_accounts), \
           (SELECT COUNT(*) FROM role_links)",
    )
    .fetch_one(&state.pool)
    .await;
    let db_latency = start.elapsed().as_millis() as u64;

    let (db_ok, total_verified, total_plugins) = match row {
        Ok((verified, plugins)) => (true, verified, plugins),
        Err(_) => (false, 0, 0),
    };

    let status = if db_ok { "healthy" } else { "degraded" };

    Json(json!({
        "status": status,
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "checks": {
            "database": {
                "status": status,
                "latency_ms": db_latency
            }
        },
        "total_verified": total_verified,
        "total_plugins": total_plugins,
    }))
}
