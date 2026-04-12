use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::Mutex;

use crate::error::YouTubeError;
use crate::models::condition::Condition;
use crate::services::sync::{self, PlayerSyncEvent};
use crate::AppState;

const MIN_REFRESH_SECS: i64 = 1800; // 30 min floor
const MAX_REFRESH_SECS: i64 = 86400; // 24 hour cap
const INTERVAL_CACHE_SECS: u64 = 300; // recompute every 5 minutes

/// Inactive users (no role_assignments) are refreshed this many times slower.
const INACTIVE_MULTIPLIER: i64 = 6;

/// Channel stats change slowly — refresh at 2x the subscription interval.
const CHANNEL_STATS_MULTIPLIER: i64 = 2;

/// Caches the refresh interval to avoid running COUNT(*) on every fetch cycle.
struct CachedInterval {
    value: AtomicI64,
    quota_per_day: i64,
    needs_channel_stats: AtomicBool,
    last_computed: Mutex<Instant>,
}

impl CachedInterval {
    fn new(quota_per_day: i64) -> Self {
        Self {
            value: AtomicI64::new(MIN_REFRESH_SECS),
            quota_per_day,
            needs_channel_stats: AtomicBool::new(false),
            last_computed: Mutex::new(Instant::now() - std::time::Duration::from_secs(INTERVAL_CACHE_SECS + 1)),
        }
    }

    async fn get(&self, pool: &sqlx::PgPool) -> i64 {
        let mut last = self.last_computed.lock().await;
        if last.elapsed() >= std::time::Duration::from_secs(INTERVAL_CACHE_SECS) {
            // Count the number of subscription checks needed
            let check_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM subscription_cache")
                .fetch_one(pool)
                .await
                .unwrap_or(0);

            // Check if any role_link conditions need channel stats
            let needs_cs = self.check_needs_channel_stats(pool).await;
            self.needs_channel_stats.store(needs_cs, Ordering::Relaxed);

            // Each full check costs 1 unit (subscription) + 1 unit if channel stats needed
            let units_per_check: i64 = if needs_cs { 2 } else { 1 };

            let interval = if check_count == 0 {
                MIN_REFRESH_SECS
            } else {
                ((check_count * units_per_check * 86400) / self.quota_per_day)
                    .clamp(MIN_REFRESH_SECS, MAX_REFRESH_SECS)
            };

            self.value.store(interval, Ordering::Relaxed);
            *last = Instant::now();
        }
        self.value.load(Ordering::Relaxed)
    }

    fn needs_channel_stats(&self) -> bool {
        self.needs_channel_stats.load(Ordering::Relaxed)
    }

    /// Check if any role_link has conditions that require channel_cache data.
    async fn check_needs_channel_stats(&self, pool: &sqlx::PgPool) -> bool {
        let rows: Vec<serde_json::Value> = sqlx::query_scalar(
            "SELECT conditions FROM role_links WHERE channel_id IS NOT NULL AND conditions != '[]'::jsonb",
        )
        .fetch_all(pool)
        .await
        .unwrap_or_default();

        for raw in &rows {
            let conditions: Vec<Condition> = raw
                .as_array()
                .unwrap_or(&vec![])
                .iter()
                .filter_map(|v| serde_json::from_value::<Condition>(v.clone()).ok())
                .collect();
            if sync::conditions_need_channel_cache(&conditions) {
                return true;
            }
        }
        false
    }
}

pub async fn run(state: Arc<AppState>) {
    let quota = state.config.youtube_quota_per_day;
    tracing::info!(quota, "Refresh worker started");

    let cached_interval = CachedInterval::new(quota);

    loop {
        // Wait for rate limiter
        state.youtube_client.wait_for_permit().await;

        // Get next subscription check due, prioritizing active users
        let next = sqlx::query_as::<_, (String, String, String, String, chrono::DateTime<chrono::Utc>, bool)>(
            "SELECT sc.discord_id, sc.channel_id, la.google_access_token, la.google_refresh_token, \
             la.google_token_expires_at, \
             EXISTS(SELECT 1 FROM role_assignments ra WHERE ra.discord_id = sc.discord_id) as is_active \
             FROM subscription_cache sc \
             JOIN linked_accounts la ON la.discord_id = sc.discord_id \
             WHERE sc.next_check_at <= now() \
             ORDER BY is_active DESC, sc.check_failures ASC, sc.next_check_at ASC \
             LIMIT 1",
        )
        .fetch_optional(&state.pool)
        .await;

        let (discord_id, channel_id, mut access_token, refresh_token, token_expires_at, is_active) = match next {
            Ok(Some(row)) => row,
            Ok(None) => {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                continue;
            }
            Err(e) => {
                tracing::error!("Refresh worker DB error: {e}");
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                continue;
            }
        };

        // Refresh Google access token if expired
        if token_expires_at <= chrono::Utc::now() {
            match state.youtube_client.refresh_google_token(&state.config, &refresh_token).await {
                Ok(tokens) => {
                    access_token = tokens.access_token.clone();
                    let new_expires = chrono::Utc::now() + chrono::Duration::seconds(tokens.expires_in);

                    // Update tokens in database
                    let new_refresh = tokens.refresh_token.as_deref().unwrap_or(&refresh_token);
                    if let Err(e) = sqlx::query(
                        "UPDATE linked_accounts SET google_access_token = $1, google_refresh_token = $2, \
                         google_token_expires_at = $3 WHERE discord_id = $4",
                    )
                    .bind(&tokens.access_token)
                    .bind(new_refresh)
                    .bind(new_expires)
                    .bind(&discord_id)
                    .execute(&state.pool)
                    .await
                    {
                        tracing::error!(discord_id, "Failed to update Google tokens: {e}");
                        continue;
                    }
                }
                Err(YouTubeError::TokenRevoked) => {
                    tracing::warn!(discord_id, "Google token revoked, backing off");
                    // Set a long backoff for all this user's subscription checks
                    let backoff = chrono::Utc::now() + chrono::Duration::hours(24);
                    let _ = sqlx::query(
                        "UPDATE subscription_cache SET next_check_at = $1, check_failures = check_failures + 1 \
                         WHERE discord_id = $2",
                    )
                    .bind(backoff)
                    .bind(&discord_id)
                    .execute(&state.pool)
                    .await;
                    continue;
                }
                Err(e) => {
                    tracing::warn!(discord_id, "Google token refresh failed: {e}");
                    // Exponential backoff for this check
                    let _ = sqlx::query(
                        "UPDATE subscription_cache SET check_failures = check_failures + 1, \
                         next_check_at = now() + LEAST(INTERVAL '60 seconds' * POWER(2, check_failures), INTERVAL '1 hour') \
                         WHERE discord_id = $1 AND channel_id = $2",
                    )
                    .bind(&discord_id)
                    .bind(&channel_id)
                    .execute(&state.pool)
                    .await;
                    continue;
                }
            }
        }

        tracing::debug!(discord_id, channel_id, is_active, "Checking YouTube subscription");

        match state.youtube_client.check_subscription(&access_token, &channel_id).await {
            Ok(result) => {
                let base_interval = cached_interval.get(&state.pool).await;
                let multiplier = if is_active { 1 } else { INACTIVE_MULTIPLIER };
                let interval = base_interval * multiplier;
                let next_check = chrono::Utc::now() + chrono::Duration::seconds(interval);

                if let Err(e) = sqlx::query(
                    "UPDATE subscription_cache SET \
                     is_subscribed = $1, subscribed_at = $2, checked_at = now(), next_check_at = $3, check_failures = 0 \
                     WHERE discord_id = $4 AND channel_id = $5",
                )
                .bind(result.is_subscribed)
                .bind(result.subscribed_at)
                .bind(next_check)
                .bind(&discord_id)
                .bind(&channel_id)
                .execute(&state.pool)
                .await
                {
                    tracing::error!(discord_id, channel_id, "Failed to update subscription cache: {e}");
                    continue;
                }

                // Fetch channel stats if any conditions need them
                if cached_interval.needs_channel_stats() {
                    // Check if channel_cache needs refresh for this user
                    let needs_refresh = sqlx::query_scalar::<_, bool>(
                        "SELECT COALESCE( \
                            (SELECT cc.next_check_at <= now() FROM channel_cache cc WHERE cc.discord_id = $1), \
                            TRUE \
                         )",
                    )
                    .bind(&discord_id)
                    .fetch_one(&state.pool)
                    .await
                    .unwrap_or(true);

                    if needs_refresh {
                        // Wait for another rate limiter permit (this is a second API call)
                        state.youtube_client.wait_for_permit().await;

                        match state.youtube_client.fetch_channel_stats(&access_token).await {
                            Ok(stats) => {
                                let cc_interval = base_interval * multiplier * CHANNEL_STATS_MULTIPLIER;
                                let cc_next = chrono::Utc::now() + chrono::Duration::seconds(cc_interval);

                                if let Err(e) = sqlx::query(
                                    "INSERT INTO channel_cache \
                                     (discord_id, subscriber_count, view_count, video_count, channel_created_at, \
                                      hidden_subscribers, country, custom_url, checked_at, next_check_at, check_failures) \
                                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, now(), $9, 0) \
                                     ON CONFLICT (discord_id) DO UPDATE SET \
                                      subscriber_count = $2, view_count = $3, video_count = $4, \
                                      channel_created_at = $5, hidden_subscribers = $6, \
                                      country = $7, custom_url = $8, \
                                      checked_at = now(), next_check_at = $9, check_failures = 0",
                                )
                                .bind(&discord_id)
                                .bind(stats.subscriber_count)
                                .bind(stats.view_count)
                                .bind(stats.video_count)
                                .bind(stats.channel_created_at)
                                .bind(stats.hidden_subscriber_count)
                                .bind(&stats.country)
                                .bind(&stats.custom_url)
                                .bind(cc_next)
                                .execute(&state.pool)
                                .await
                                {
                                    tracing::error!(discord_id, "Failed to update channel cache: {e}");
                                }

                                tracing::debug!(
                                    discord_id,
                                    subscribers = stats.subscriber_count,
                                    views = stats.view_count,
                                    videos = stats.video_count,
                                    "Channel stats updated"
                                );
                            }
                            Err(YouTubeError::QuotaExceeded) => {
                                tracing::warn!("YouTube API quota exceeded during channel stats fetch");
                                // Don't back off all checks — just skip the channel stats
                            }
                            Err(e) => {
                                tracing::warn!(discord_id, "Failed to fetch channel stats: {e}");
                                // Exponential backoff for channel_cache
                                let _ = sqlx::query(
                                    "INSERT INTO channel_cache (discord_id, check_failures, next_check_at) \
                                     VALUES ($1, 1, now() + INTERVAL '1 hour') \
                                     ON CONFLICT (discord_id) DO UPDATE SET \
                                      check_failures = channel_cache.check_failures + 1, \
                                      next_check_at = now() + LEAST(INTERVAL '60 seconds' * POWER(2, channel_cache.check_failures), INTERVAL '6 hours')",
                                )
                                .bind(&discord_id)
                                .execute(&state.pool)
                                .await;
                            }
                        }
                    }
                }

                // Trigger sync for this player
                let _ = state
                    .player_sync_tx
                    .send(PlayerSyncEvent::PlayerUpdated {
                        discord_id: discord_id.clone(),
                    })
                    .await;

                tracing::debug!(
                    discord_id, channel_id,
                    is_subscribed = result.is_subscribed,
                    is_active,
                    "Subscription check complete"
                );
            }
            Err(YouTubeError::QuotaExceeded) => {
                tracing::warn!("YouTube API quota exceeded, backing off until reset");
                // Back off all pending checks by 1 hour
                let backoff = chrono::Utc::now() + chrono::Duration::hours(1);
                let _ = sqlx::query(
                    "UPDATE subscription_cache SET next_check_at = $1 WHERE next_check_at <= now()",
                )
                .bind(backoff)
                .execute(&state.pool)
                .await;
                tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
            }
            Err(YouTubeError::TokenExpired | YouTubeError::TokenRevoked) => {
                tracing::warn!(discord_id, "YouTube token invalid, backing off");
                let backoff = chrono::Utc::now() + chrono::Duration::hours(24);
                let _ = sqlx::query(
                    "UPDATE subscription_cache SET next_check_at = $1, check_failures = check_failures + 1 \
                     WHERE discord_id = $2",
                )
                .bind(backoff)
                .bind(&discord_id)
                .execute(&state.pool)
                .await;
            }
            Err(e) => {
                // Exponential backoff for this check
                let failures = sqlx::query_scalar::<_, i32>(
                    "UPDATE subscription_cache SET check_failures = check_failures + 1, \
                     next_check_at = now() + LEAST(INTERVAL '60 seconds' * POWER(2, check_failures), INTERVAL '1 hour') \
                     WHERE discord_id = $1 AND channel_id = $2 \
                     RETURNING check_failures",
                )
                .bind(&discord_id)
                .bind(&channel_id)
                .fetch_optional(&state.pool)
                .await
                .ok()
                .flatten()
                .unwrap_or(0);

                tracing::warn!(discord_id, channel_id, failures, "YouTube check failed: {e}");
            }
        }
    }
}
