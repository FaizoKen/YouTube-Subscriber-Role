use std::collections::HashSet;

use futures_util::stream::{self, StreamExt};

use crate::error::AppError;
use crate::services::auth_gateway;
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

/// Sync roles for a single player across all guilds.
/// Checks subscription_cache for each role link's channel_id, then adds/removes roles.
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
    let role_links = sqlx::query_as::<_, (String, String, String, String)>(
        "SELECT rl.guild_id, rl.role_id, rl.api_token, rl.channel_id \
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

    // Check subscription status from cache for each role link
    enum Action {
        Add { guild_id: String, role_id: String, api_token: String },
        Remove { guild_id: String, role_id: String, api_token: String },
    }

    let mut actions: Vec<Action> = Vec::new();
    for (guild_id, role_id, api_token, channel_id) in &role_links {
        let is_subscribed = sqlx::query_scalar::<_, bool>(
            "SELECT is_subscribed FROM subscription_cache \
             WHERE discord_id = $1 AND channel_id = $2",
        )
        .bind(discord_id)
        .bind(channel_id)
        .fetch_optional(pool)
        .await?
        .unwrap_or(false);

        let currently_assigned = existing.contains(&(guild_id.clone(), role_id.clone()));
        match (is_subscribed, currently_assigned) {
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
                        if let Err(e) = rl_client.remove_user(&guild_id, &role_id, &discord_id, &api_token).await {
                            tracing::error!(
                                guild_id, role_id, discord_id,
                                "Failed to remove user from role: {e}"
                            );
                            return;
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

/// Re-evaluate all users for a specific role link (after config change).
/// Uses simple SQL query on subscription_cache + atomic PUT replace.
pub async fn sync_for_role_link(
    guild_id: &str,
    role_id: &str,
    state: &AppState,
) -> Result<(), AppError> {
    let pool = &state.pool;
    let rl_client = &state.rl_client;

    let link = sqlx::query_as::<_, (String, Option<String>)>(
        "SELECT api_token, channel_id FROM role_links WHERE guild_id = $1 AND role_id = $2",
    )
    .bind(guild_id)
    .bind(role_id)
    .fetch_optional(pool)
    .await?;

    let Some((api_token, Some(channel_id))) = link else {
        return Ok(());
    };

    let member_ids = auth_gateway::fetch_guild_member_ids(
        &state.http,
        &state.config.auth_gateway_url,
        &state.config.internal_api_key,
        guild_id,
    )
    .await?;

    if member_ids.is_empty() {
        rl_client.replace_users(guild_id, role_id, &[], &api_token).await?;
        let mut tx = pool.begin().await?;
        sqlx::query("DELETE FROM role_assignments WHERE guild_id = $1 AND role_id = $2")
            .bind(guild_id).bind(role_id)
            .execute(&mut *tx).await?;
        tx.commit().await?;
        return Ok(());
    }

    // Query the user limit from RoleLogic
    let (_user_count, user_limit) = rl_client
        .get_user_info(guild_id, role_id, &api_token)
        .await
        .unwrap_or((0, 100));

    // Simple query: all linked users in this guild who are subscribed to this channel
    let qualifying_ids = sqlx::query_scalar::<_, String>(
        "SELECT la.discord_id \
         FROM linked_accounts la \
         JOIN subscription_cache sc ON sc.discord_id = la.discord_id AND sc.channel_id = $1 \
         WHERE la.discord_id = ANY($2::text[]) \
           AND sc.is_subscribed = TRUE \
         ORDER BY la.linked_at ASC \
         LIMIT $3",
    )
    .bind(&channel_id)
    .bind(&member_ids[..])
    .bind(user_limit as i64)
    .fetch_all(pool)
    .await?;

    // Atomic replace
    rl_client
        .replace_users(guild_id, role_id, &qualifying_ids, &api_token)
        .await?;

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
        if let Err(e) = rl_client
            .remove_user(guild_id, role_id, discord_id, api_token)
            .await
        {
            tracing::error!(
                guild_id, role_id, discord_id,
                "Failed to remove user during unlink: {e}"
            );
        }
    }

    sqlx::query("DELETE FROM role_assignments WHERE discord_id = $1")
        .bind(discord_id)
        .execute(pool)
        .await?;

    Ok(())
}
