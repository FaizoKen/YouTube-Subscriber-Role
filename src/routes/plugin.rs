use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::Json;
use serde::Deserialize;
use serde_json::Value;

use crate::error::AppError;
use crate::models::condition::Condition;
use crate::schema;
use crate::services::sync::ConfigSyncEvent;
use crate::AppState;

fn extract_token(headers: &HeaderMap) -> Result<String, AppError> {
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .ok_or(AppError::Unauthorized)?;

    let token = auth.strip_prefix("Token ").ok_or(AppError::Unauthorized)?;
    Ok(token.to_string())
}

#[derive(Deserialize)]
pub struct RegisterBody {
    pub guild_id: String,
    pub role_id: String,
}

pub async fn register(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<RegisterBody>,
) -> Result<Json<Value>, AppError> {
    let token = extract_token(&headers)?;

    sqlx::query(
        "INSERT INTO role_links (guild_id, role_id, api_token) VALUES ($1, $2, $3) \
         ON CONFLICT (guild_id, role_id) DO UPDATE SET api_token = $3, updated_at = now()",
    )
    .bind(&body.guild_id)
    .bind(&body.role_id)
    .bind(&token)
    .execute(&state.pool)
    .await?;

    tracing::info!(
        guild_id = body.guild_id,
        role_id = body.role_id,
        "Role link registered"
    );

    Ok(Json(serde_json::json!({"success": true})))
}

pub async fn get_config(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    let token = extract_token(&headers)?;

    let link = sqlx::query_as::<_, (Option<String>, Value)>(
        "SELECT channel_id, conditions FROM role_links WHERE api_token = $1",
    )
    .bind(&token)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::Unauthorized)?;

    let (channel_id, raw_conditions) = link;

    // Deserialize conditions with tolerance for old format
    let conditions: Vec<Condition> = raw_conditions
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|v| serde_json::from_value::<Condition>(v.clone()).ok())
        .collect();

    let verify_url = format!("{}/verify", state.config.base_url);
    let schema = schema::build_config_schema(channel_id.as_deref(), &conditions, &verify_url);

    Ok(Json(schema))
}

#[derive(Deserialize)]
pub struct ConfigBody {
    pub guild_id: String,
    pub role_id: String,
    pub config: HashMap<String, Value>,
}

pub async fn post_config(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<ConfigBody>,
) -> Result<Json<Value>, AppError> {
    let token = extract_token(&headers)?;

    // Verify token matches this role link
    let exists = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM role_links WHERE guild_id = $1 AND role_id = $2 AND api_token = $3)",
    )
    .bind(&body.guild_id)
    .bind(&body.role_id)
    .bind(&token)
    .fetch_one(&state.pool)
    .await
    .unwrap_or(false);

    if !exists {
        return Err(AppError::Unauthorized);
    }

    let (channel_id, conditions) = schema::parse_config(&body.config)?;

    sqlx::query(
        "UPDATE role_links SET channel_id = $1, conditions = $2, updated_at = now() \
         WHERE guild_id = $3 AND role_id = $4",
    )
    .bind(&channel_id)
    .bind(serde_json::json!(&conditions))
    .bind(&body.guild_id)
    .bind(&body.role_id)
    .execute(&state.pool)
    .await?;

    // Seed subscription_cache rows for all linked accounts for this channel
    // so the refresh worker picks them up. No guild filtering needed here —
    // the sync engine handles guild scoping via Auth Gateway.
    sqlx::query(
        "INSERT INTO subscription_cache (discord_id, channel_id, next_check_at) \
         SELECT la.discord_id, $1, now() \
         FROM linked_accounts la \
         ON CONFLICT (discord_id, channel_id) DO NOTHING",
    )
    .bind(&channel_id)
    .execute(&state.pool)
    .await?;

    // If conditions need channel stats, seed channel_cache rows too
    let needs_channel_cache = conditions
        .iter()
        .any(|c| c.field.needs_channel_cache());

    if needs_channel_cache {
        sqlx::query(
            "INSERT INTO channel_cache (discord_id, next_check_at) \
             SELECT la.discord_id, now() \
             FROM linked_accounts la \
             ON CONFLICT (discord_id) DO NOTHING",
        )
        .execute(&state.pool)
        .await?;
    }

    tracing::info!(
        guild_id = body.guild_id,
        role_id = body.role_id,
        channel_id,
        condition_count = conditions.len(),
        "Config updated"
    );

    // Trigger re-evaluation for this role link
    let _ = state.config_sync_tx.send(ConfigSyncEvent {
        guild_id: body.guild_id,
        role_id: body.role_id,
    }).await;

    Ok(Json(serde_json::json!({"success": true})))
}

#[derive(Deserialize)]
pub struct DeleteConfigBody {
    pub guild_id: String,
    pub role_id: String,
}

pub async fn delete_config(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<DeleteConfigBody>,
) -> Result<Json<Value>, AppError> {
    let token = extract_token(&headers)?;

    let result = sqlx::query(
        "DELETE FROM role_links WHERE guild_id = $1 AND role_id = $2 AND api_token = $3",
    )
    .bind(&body.guild_id)
    .bind(&body.role_id)
    .bind(&token)
    .execute(&state.pool)
    .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::Unauthorized);
    }

    tracing::info!(
        guild_id = body.guild_id,
        role_id = body.role_id,
        "Role link deleted"
    );

    Ok(Json(serde_json::json!({"success": true})))
}
