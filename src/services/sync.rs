use std::collections::HashSet;

use futures_util::stream::{self, StreamExt};

use crate::error::AppError;
use crate::models::rule::RuleTree;
use crate::services::auth_gateway;
use crate::services::condition_eval::{self, PlayerYouTubeData};
use crate::services::rule_sql::{self, Bind};
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

/// Parse the stored `rule_tree` JSONB, tolerating malformed rows (treated as
/// "grant to nobody" — the safe default).
fn parse_rule_tree(raw: &serde_json::Value) -> RuleTree {
    serde_json::from_value::<RuleTree>(raw.clone()).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Per-player sync
// ---------------------------------------------------------------------------

/// Sync roles for a single player across all guilds.
/// Evaluates each role link's rule tree against the player's cached facts, then
/// adds/removes roles to match.
pub async fn sync_for_player(discord_id: &str, state: &AppState) -> Result<(), AppError> {
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

    // All role links for guilds this user is a member of. Channel may be NULL
    // for channel-agnostic ("anyone who linked") rules.
    let role_links =
        sqlx::query_as::<_, (String, String, String, Option<String>, serde_json::Value)>(
            "SELECT rl.guild_id, rl.role_id, rl.api_token, rl.channel_id, rl.rule_tree \
         FROM role_links rl \
         WHERE rl.guild_id = ANY($1)",
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

    // Pre-load channel_cache data for this user (the member's own channel stats)
    let channel_data = sqlx::query_as::<_, (Option<i64>, Option<i64>, Option<i64>, Option<chrono::DateTime<chrono::Utc>>, bool, Option<String>, Option<String>)>(
        "SELECT subscriber_count, view_count, video_count, channel_created_at, hidden_subscribers, \
         country, custom_url \
         FROM channel_cache WHERE discord_id = $1",
    )
    .bind(discord_id)
    .fetch_optional(pool)
    .await?;

    enum Action {
        Add {
            guild_id: String,
            role_id: String,
            api_token: String,
        },
        Remove {
            guild_id: String,
            role_id: String,
            api_token: String,
        },
    }

    let mut actions: Vec<Action> = Vec::new();
    for (guild_id, role_id, api_token, channel_id, raw_tree) in &role_links {
        let tree = parse_rule_tree(raw_tree);

        // Subscription facts for this link's configured channel (if any).
        let (is_subscribed, subscribed_at) = if let Some(channel_id) = channel_id {
            sqlx::query_as::<_, (bool, Option<chrono::DateTime<chrono::Utc>>)>(
                "SELECT is_subscribed, subscribed_at FROM subscription_cache \
                 WHERE discord_id = $1 AND channel_id = $2",
            )
            .bind(discord_id)
            .bind(channel_id)
            .fetch_optional(pool)
            .await?
            .unwrap_or((false, None))
        } else {
            (false, None)
        };

        let facts = PlayerYouTubeData {
            is_subscribed,
            subscribed_at,
            subscriber_count: channel_data.as_ref().and_then(|d| d.0),
            view_count: channel_data.as_ref().and_then(|d| d.1),
            video_count: channel_data.as_ref().and_then(|d| d.2),
            channel_created_at: channel_data.as_ref().and_then(|d| d.3),
            hidden_subscribers: channel_data.as_ref().is_some_and(|d| d.4),
            country: channel_data.as_ref().and_then(|d| d.5.clone()),
            custom_url: channel_data.as_ref().and_then(|d| d.6.clone()),
        };

        let qualifies = condition_eval::evaluate_rule_tree(&tree, &facts);

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

/// Re-evaluate all members for a specific role link (after config change).
/// Pushes the rule tree down to a single SQL query + atomic PUT replace.
pub async fn sync_for_role_link(
    guild_id: &str,
    role_id: &str,
    state: &AppState,
) -> Result<(), AppError> {
    let pool = &state.pool;
    let rl_client = &state.rl_client;

    let link = sqlx::query_as::<_, (String, Option<String>, serde_json::Value)>(
        "SELECT api_token, channel_id, rule_tree FROM role_links WHERE guild_id = $1 AND role_id = $2",
    )
    .bind(guild_id)
    .bind(role_id)
    .fetch_optional(pool)
    .await?;

    let Some((api_token, channel_id, raw_tree)) = link else {
        return Ok(());
    };
    let tree = parse_rule_tree(&raw_tree);

    let member_ids = auth_gateway::fetch_guild_member_ids(
        &state.http,
        &state.config.auth_gateway_url,
        &state.config.internal_api_key,
        guild_id,
    )
    .await?;

    if member_ids.is_empty() {
        match rl_client
            .upload_users(guild_id, role_id, &[], &api_token)
            .await
        {
            Ok(_) => {}
            Err(AppError::RoleLinkNotFound) => {
                delete_orphan_role_link(guild_id, role_id, pool).await;
                return Ok(());
            }
            Err(e) => return Err(e),
        }
        let mut tx = pool.begin().await?;
        sqlx::query("DELETE FROM role_assignments WHERE guild_id = $1 AND role_id = $2")
            .bind(guild_id)
            .bind(role_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        return Ok(());
    }

    // Query the user limit from RoleLogic
    let (_user_count, user_limit) =
        match rl_client.get_user_info(guild_id, role_id, &api_token).await {
            Ok(v) => v,
            Err(AppError::RoleLinkNotFound) => {
                delete_orphan_role_link(guild_id, role_id, pool).await;
                return Ok(());
            }
            Err(_) => (0, 100),
        };

    // Build the DNF WHERE clause. Fixed binds: $1 = channel_id (empty when the
    // rule is channel-agnostic — subscription conditions then fail closed),
    // $2 = member_ids, then rule binds (offset 2), then limit.
    let (rule_where, rule_binds) = rule_sql::build_rule_where(&tree, 2);
    let limit_idx = 2 + rule_binds.len() + 1;

    let query_str = format!(
        "SELECT la.discord_id \
         FROM linked_accounts la \
         LEFT JOIN subscription_cache sc ON sc.discord_id = la.discord_id AND sc.channel_id = $1 \
         LEFT JOIN channel_cache cc ON cc.discord_id = la.discord_id \
         WHERE la.discord_id = ANY($2::text[]) \
           AND ({rule_where}) \
         ORDER BY la.linked_at ASC \
         LIMIT ${limit_idx}",
    );

    let qualifying_ids = exec_rule_query(
        &query_str,
        channel_id.as_deref().unwrap_or(""),
        &member_ids,
        &rule_binds,
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

/// Execute a dynamically-built rule query with variable bind counts.
async fn exec_rule_query(
    query: &str,
    channel_id: &str,
    member_ids: &[String],
    rule_binds: &[Bind],
    limit: usize,
    pool: &sqlx::PgPool,
) -> Result<Vec<String>, AppError> {
    let mut q = sqlx::query_scalar::<_, String>(query);
    q = q.bind(channel_id.to_string()); // $1
    q = q.bind(member_ids.to_vec()); // $2
    for bind in rule_binds {
        q = match bind {
            Bind::Bool(v) => q.bind(*v),
            Bind::Int(v) => q.bind(*v),
            Bind::Text(v) => q.bind(v.clone()),
            Bind::TextArray(v) => q.bind(v.clone()),
        };
    }
    q = q.bind(limit as i64); // last bind
    Ok(q.fetch_all(pool).await?)
}

// ---------------------------------------------------------------------------
// Account unlink
// ---------------------------------------------------------------------------

/// Remove a user from all role assignments (after account unlink).
pub async fn remove_all_assignments(discord_id: &str, state: &AppState) -> Result<(), AppError> {
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
                    guild_id,
                    role_id,
                    discord_id,
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
