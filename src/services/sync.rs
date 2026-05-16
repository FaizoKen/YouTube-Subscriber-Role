use std::collections::HashSet;

use futures_util::stream::{self, StreamExt};

use crate::error::AppError;
use crate::models::condition::{Condition, ConditionField, ConditionOperator};
use crate::services::auth_gateway;
use crate::services::condition_eval::{self, PlayerYouTubeData};
use crate::AppState;

/// Events sent to the player sync worker (lightweight, per-user).
#[derive(Debug, Clone)]
pub enum PlayerSyncEvent {
    PlayerUpdated { discord_id: String },
    AccountLinked { discord_id: String },
    AccountUnlinked { discord_id: String },
}

/// Events sent to the config sync worker (heavy, per-role-link).
#[derive(Debug, Clone)]
pub struct ConfigSyncEvent {
    pub guild_id: String,
    pub role_id: String,
}

// ---------------------------------------------------------------------------
// SQL condition builder
// ---------------------------------------------------------------------------

enum ConditionBind {
    Int(i64),
    Text(String),
}

/// Build a SQL WHERE clause from conditions.
/// Returns (where_clause, bind_values, needs_channel_cache_join).
/// Bind parameter indices start at `bind_offset + 1`.
fn build_condition_where(
    conditions: &[Condition],
    bind_offset: usize,
) -> (String, Vec<ConditionBind>, bool) {
    if conditions.is_empty() {
        return ("TRUE".to_string(), vec![], false);
    }

    let mut clauses: Vec<String> = Vec::new();
    let mut binds: Vec<ConditionBind> = Vec::new();
    let mut needs_cc = false;

    for condition in conditions {
        if condition.field.needs_channel_cache() {
            needs_cc = true;
        }

        match &condition.field {
            // --- Boolean field: HasCustomUrl ---
            ConditionField::HasCustomUrl => {
                clauses.push(
                    "cc.custom_url IS NOT NULL AND cc.custom_url != ''".to_string(),
                );
            }

            // --- String field: Country (Eq only) ---
            ConditionField::Country => {
                let val = condition.value.as_str().unwrap_or("").to_uppercase();
                let idx = bind_offset + binds.len() + 1;
                clauses.push(format!("UPPER(cc.country) = ${idx}"));
                binds.push(ConditionBind::Text(val));
            }

            // --- Numeric fields ---
            field => {
                // For SubscriberCount: require hidden_subscribers = FALSE
                if matches!(field, ConditionField::SubscriberCount) {
                    clauses.push("cc.hidden_subscribers = FALSE".to_string());
                }

                let expr = field.sql_expr().expect("numeric field must have sql_expr");
                let val = condition.value.as_i64().unwrap_or(0);

                if matches!(condition.operator, ConditionOperator::Between) {
                    let end = condition
                        .value_end
                        .as_ref()
                        .and_then(|v| v.as_i64())
                        .unwrap_or(val);
                    let idx_start = bind_offset + binds.len() + 1;
                    let idx_end = bind_offset + binds.len() + 2;
                    clauses.push(format!(
                        "({expr}) >= ${idx_start} AND ({expr}) <= ${idx_end}"
                    ));
                    binds.push(ConditionBind::Int(val));
                    binds.push(ConditionBind::Int(end));
                } else {
                    let op = condition.operator.sql_operator();
                    let idx = bind_offset + binds.len() + 1;
                    clauses.push(format!("({expr}) {op} ${idx}"));
                    binds.push(ConditionBind::Int(val));
                }

                // For age fields, require the timestamp to be NOT NULL
                match field {
                    ConditionField::SubscriptionAgeDays => {
                        clauses.push("sc.subscribed_at IS NOT NULL".to_string());
                    }
                    ConditionField::ChannelAgeDays => {
                        clauses.push("cc.channel_created_at IS NOT NULL".to_string());
                    }
                    _ => {}
                }
            }
        }
    }

    (clauses.join(" AND "), binds, needs_cc)
}

/// Check if any condition in the list needs channel_cache data.
pub fn conditions_need_channel_cache(conditions: &[Condition]) -> bool {
    conditions.iter().any(|c| c.field.needs_channel_cache())
}

// ---------------------------------------------------------------------------
// Per-player sync
// ---------------------------------------------------------------------------

/// Sync roles for a single player across all guilds.
/// Checks subscription_cache + conditions for each role link, then adds/removes roles.
pub async fn sync_for_player(
    discord_id: &str,
    state: &AppState,
) -> Result<(), AppError> {
    let pool = &state.pool;
    let rl_client = &state.rl_client;

    // Get guild IDs from Auth Gateway
    let guild_ids = auth_gateway::fetch_user_guild_ids(
        &state.http,
        &state.config.auth_gateway_url,
        &state.config.internal_api_key,
        discord_id,
    )
    .await?;

    if guild_ids.is_empty() {
        return Ok(());
    }

    // Get role links for guilds this user is a member of (only those with a configured channel)
    let role_links = sqlx::query_as::<_, (String, String, String, String, serde_json::Value)>(
        "SELECT rl.guild_id, rl.role_id, rl.api_token, rl.channel_id, rl.conditions \
         FROM role_links rl \
         WHERE rl.guild_id = ANY($1) AND rl.channel_id IS NOT NULL",
    )
    .bind(&guild_ids[..])
    .fetch_all(pool)
    .await?;

    if role_links.is_empty() {
        return Ok(());
    }

    // Batch: fetch all existing assignments for this user in ONE query
    let existing: HashSet<(String, String)> = sqlx::query_as::<_, (String, String)>(
        "SELECT guild_id, role_id FROM role_assignments WHERE discord_id = $1",
    )
    .bind(discord_id)
    .fetch_all(pool)
    .await?
    .into_iter()
    .collect();

    // Pre-load channel_cache data for this user (needed for channel stat conditions)
    let channel_data = sqlx::query_as::<_, (Option<i64>, Option<i64>, Option<i64>, Option<chrono::DateTime<chrono::Utc>>, bool, Option<String>, Option<String>)>(
        "SELECT subscriber_count, view_count, video_count, channel_created_at, hidden_subscribers, \
         country, custom_url \
         FROM channel_cache WHERE discord_id = $1",
    )
    .bind(discord_id)
    .fetch_optional(pool)
    .await?;

    enum Action {
        Add { guild_id: String, role_id: String, api_token: String },
        Remove { guild_id: String, role_id: String, api_token: String },
    }

    let mut actions: Vec<Action> = Vec::new();
    for (guild_id, role_id, api_token, channel_id, raw_conditions) in &role_links {
        // Deserialize conditions with tolerance for old format
        let conditions: Vec<Condition> = raw_conditions
            .as_array()
            .unwrap_or(&vec![])
            .iter()
            .filter_map(|v| serde_json::from_value::<Condition>(v.clone()).ok())
            .collect();

        // Check subscription status + subscribed_at from cache
        let sub_row = sqlx::query_as::<_, (bool, Option<chrono::DateTime<chrono::Utc>>)>(
            "SELECT is_subscribed, subscribed_at FROM subscription_cache \
             WHERE discord_id = $1 AND channel_id = $2",
        )
        .bind(discord_id)
        .bind(channel_id)
        .fetch_optional(pool)
        .await?;

        let (is_subscribed, subscribed_at) = sub_row.unwrap_or((false, None));

        // Determine if user qualifies
        let qualifies = if !is_subscribed {
            false
        } else if conditions.is_empty() {
            true
        } else {
            let yt_data = PlayerYouTubeData {
                subscribed_at,
                subscriber_count: channel_data.as_ref().and_then(|d| d.0),
                view_count: channel_data.as_ref().and_then(|d| d.1),
                video_count: channel_data.as_ref().and_then(|d| d.2),
                channel_created_at: channel_data.as_ref().and_then(|d| d.3),
                hidden_subscribers: channel_data.as_ref().map_or(false, |d| d.4),
                country: channel_data.as_ref().and_then(|d| d.5.clone()),
                custom_url: channel_data.as_ref().and_then(|d| d.6.clone()),
            };
            condition_eval::evaluate_conditions(&conditions, &yt_data)
        };

        let currently_assigned = existing.contains(&(guild_id.clone(), role_id.clone()));
        match (qualifies, currently_assigned) {
            (true, false) => actions.push(Action::Add {
                guild_id: guild_id.clone(),
                role_id: role_id.clone(),
                api_token: api_token.clone(),
            }),
            (false, true) => actions.push(Action::Remove {
                guild_id: guild_id.clone(),
                role_id: role_id.clone(),
                api_token: api_token.clone(),
            }),
            _ => {}
        }
    }

    if actions.is_empty() {
        return Ok(());
    }

    // Execute API calls concurrently (max 10 parallel)
    let discord_id_owned = discord_id.to_string();
    stream::iter(actions)
        .for_each_concurrent(10, |action| {
            let pool = pool.clone();
            let rl_client = rl_client.clone();
            let discord_id = discord_id_owned.clone();
            async move {
                match action {
                    Action::Add { guild_id, role_id, api_token } => {
                        match rl_client.add_user(&guild_id, &role_id, &discord_id, &api_token).await {
                            Err(AppError::RoleLinkNotFound) => {
                                delete_orphan_role_link(&guild_id, &role_id, &pool).await;
                                return;
                            }
                            Err(AppError::UserLimitReached { limit }) => {
                                tracing::warn!(
                                    guild_id, role_id, discord_id, limit,
                                    "Cannot add user: role link user limit reached"
                                );
                                return;
                            }
                            Err(e) => {
                                tracing::error!(
                                    guild_id, role_id, discord_id,
                                    "Failed to add user to role: {e}"
                                );
                                return;
                            }
                            Ok(_) => {}
                        }
                        if let Err(e) = sqlx::query(
                            "INSERT INTO role_assignments (guild_id, role_id, discord_id) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
                        )
                        .bind(&guild_id)
                        .bind(&role_id)
                        .bind(&discord_id)
                        .execute(&pool)
                        .await {
                            tracing::error!(guild_id, role_id, discord_id, "Failed to insert assignment: {e}");
                        }
                    }
                    Action::Remove { guild_id, role_id, api_token } => {
                        match rl_client.remove_user(&guild_id, &role_id, &discord_id, &api_token).await {
                            Err(AppError::RoleLinkNotFound) => {
                                delete_orphan_role_link(&guild_id, &role_id, &pool).await;
                                return;
                            }
                            Err(e) => {
                                tracing::error!(
                                    guild_id, role_id, discord_id,
                                    "Failed to remove user from role: {e}"
                                );
                                return;
                            }
                            Ok(_) => {}
                        }
                        if let Err(e) = sqlx::query(
                            "DELETE FROM role_assignments WHERE guild_id = $1 AND role_id = $2 AND discord_id = $3",
                        )
                        .bind(&guild_id)
                        .bind(&role_id)
                        .bind(&discord_id)
                        .execute(&pool)
                        .await {
                            tracing::error!(guild_id, role_id, discord_id, "Failed to delete assignment: {e}");
                        }
                    }
                }
            }
        })
        .await;

    Ok(())
}

// ---------------------------------------------------------------------------
// Per-role-link sync (bulk, after config change)
// ---------------------------------------------------------------------------

/// Re-evaluate all users for a specific role link (after config change).
/// Uses dynamic SQL query with condition WHERE clause + atomic PUT replace.
pub async fn sync_for_role_link(
    guild_id: &str,
    role_id: &str,
    state: &AppState,
) -> Result<(), AppError> {
    let pool = &state.pool;
    let rl_client = &state.rl_client;

    let link = sqlx::query_as::<_, (String, Option<String>, serde_json::Value)>(
        "SELECT api_token, channel_id, conditions FROM role_links WHERE guild_id = $1 AND role_id = $2",
    )
    .bind(guild_id)
    .bind(role_id)
    .fetch_optional(pool)
    .await?;

    let Some((api_token, Some(channel_id), raw_conditions)) = link else {
        return Ok(());
    };

    // Deserialize conditions with tolerance for old format
    let conditions: Vec<Condition> = raw_conditions
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|v| serde_json::from_value::<Condition>(v.clone()).ok())
        .collect();

    let member_ids = auth_gateway::fetch_guild_member_ids(
        &state.http,
        &state.config.auth_gateway_url,
        &state.config.internal_api_key,
        guild_id,
    )
    .await?;

    if member_ids.is_empty() {
        match rl_client.upload_users(guild_id, role_id, &[], &api_token).await {
            Ok(_) => {}
            Err(AppError::RoleLinkNotFound) => {
                delete_orphan_role_link(guild_id, role_id, pool).await;
                return Ok(());
            }
            Err(e) => return Err(e),
        }
        let mut tx = pool.begin().await?;
        sqlx::query("DELETE FROM role_assignments WHERE guild_id = $1 AND role_id = $2")
            .bind(guild_id).bind(role_id)
            .execute(&mut *tx).await?;
        tx.commit().await?;
        return Ok(());
    }

    // Query the user limit from RoleLogic
    let (_user_count, user_limit) = match rl_client
        .get_user_info(guild_id, role_id, &api_token)
        .await
    {
        Ok(v) => v,
        Err(AppError::RoleLinkNotFound) => {
            delete_orphan_role_link(guild_id, role_id, pool).await;
            return Ok(());
        }
        Err(_) => (0, 100),
    };

    // Build dynamic condition WHERE clause
    // Fixed binds: $1 = channel_id, $2 = member_ids, then condition binds, then limit
    let (cond_where, cond_binds, needs_cc) = build_condition_where(&conditions, 2);

    let limit_idx = 2 + cond_binds.len() + 1;

    let cc_join = if needs_cc {
        "LEFT JOIN channel_cache cc ON cc.discord_id = la.discord_id"
    } else {
        ""
    };

    let query_str = format!(
        "SELECT la.discord_id \
         FROM linked_accounts la \
         JOIN subscription_cache sc ON sc.discord_id = la.discord_id AND sc.channel_id = $1 \
         {cc_join} \
         WHERE la.discord_id = ANY($2::text[]) \
           AND sc.is_subscribed = TRUE \
           AND ({cond_where}) \
         ORDER BY la.linked_at ASC \
         LIMIT ${limit_idx}",
    );

    // Execute the dynamic query
    let qualifying_ids = exec_condition_query(
        &query_str,
        &channel_id,
        &member_ids,
        &cond_binds,
        user_limit,
        pool,
    )
    .await?;

    // Atomic replace (uses chunked upload if > 100k)
    match rl_client
        .upload_users(guild_id, role_id, &qualifying_ids, &api_token)
        .await
    {
        Ok(_) => {}
        Err(AppError::RoleLinkNotFound) => {
            delete_orphan_role_link(guild_id, role_id, pool).await;
            return Ok(());
        }
        Err(e) => return Err(e),
    }

    // Update local assignments to match what was actually sent
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM role_assignments WHERE guild_id = $1 AND role_id = $2")
        .bind(guild_id)
        .bind(role_id)
        .execute(&mut *tx)
        .await?;

    if !qualifying_ids.is_empty() {
        sqlx::query(
            "INSERT INTO role_assignments (guild_id, role_id, discord_id) \
             SELECT $1, $2, UNNEST($3::text[])",
        )
        .bind(guild_id)
        .bind(role_id)
        .bind(&qualifying_ids)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok(())
}

/// Execute a dynamically-built condition query with variable bind counts.
async fn exec_condition_query(
    query: &str,
    channel_id: &str,
    member_ids: &[String],
    cond_binds: &[ConditionBind],
    limit: usize,
    pool: &sqlx::PgPool,
) -> Result<Vec<String>, AppError> {
    let mut q = sqlx::query_scalar::<_, String>(query);
    q = q.bind(channel_id);        // $1
    q = q.bind(member_ids);         // $2
    for bind in cond_binds {
        match bind {
            ConditionBind::Int(v) => {
                q = q.bind(*v);
            }
            ConditionBind::Text(v) => {
                q = q.bind(v.as_str());
            }
        }
    }
    q = q.bind(limit as i64);      // last bind
    Ok(q.fetch_all(pool).await?)
}

// ---------------------------------------------------------------------------
// Account unlink
// ---------------------------------------------------------------------------

/// Remove a user from all role assignments (after account unlink).
pub async fn remove_all_assignments(
    discord_id: &str,
    state: &AppState,
) -> Result<(), AppError> {
    let pool = &state.pool;
    let rl_client = &state.rl_client;

    let assignments = sqlx::query_as::<_, (String, String, String)>(
        "SELECT ra.guild_id, ra.role_id, rl.api_token \
         FROM role_assignments ra \
         JOIN role_links rl ON rl.guild_id = ra.guild_id AND rl.role_id = ra.role_id \
         WHERE ra.discord_id = $1",
    )
    .bind(discord_id)
    .fetch_all(pool)
    .await?;

    for (guild_id, role_id, api_token) in &assignments {
        match rl_client
            .remove_user(guild_id, role_id, discord_id, api_token)
            .await
        {
            Ok(_) => {}
            Err(AppError::RoleLinkNotFound) => {
                delete_orphan_role_link(guild_id, role_id, pool).await;
            }
            Err(e) => {
                tracing::error!(
                    guild_id, role_id, discord_id,
                    "Failed to remove user during unlink: {e}"
                );
            }
        }
    }

    sqlx::query("DELETE FROM role_assignments WHERE discord_id = $1")
        .bind(discord_id)
        .execute(pool)
        .await?;

    Ok(())
}

/// Delete a role_link the RoleLogic API reports as gone (403 Invalid or
/// revoked token). CASCADE clears role_assignments. Best-effort: logs DB
/// failures, never propagates them — sync workers must not stop syncing
/// other links over a cleanup hiccup.
async fn delete_orphan_role_link(guild_id: &str, role_id: &str, pool: &sqlx::PgPool) {
    tracing::warn!(
        guild_id,
        role_id,
        "Role link not found on RoleLogic; removing orphaned local row"
    );
    if let Err(e) = sqlx::query("DELETE FROM role_links WHERE guild_id = $1 AND role_id = $2")
        .bind(guild_id)
        .bind(role_id)
        .execute(pool)
        .await
    {
        tracing::error!(guild_id, role_id, "Failed to delete orphan role_link: {e}");
    }
}
