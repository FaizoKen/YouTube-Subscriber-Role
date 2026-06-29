//! Background refresh worker.
//!
//! Re-checks each linked member's YouTube subscription (and own-channel stats)
//! to keep roles in sync, within the project's daily API quota. Every
//! quota-costing call passes through the [crate::services::quota::QuotaGovernor],
//! which paces background spend smoothly across the Pacific quota-day and
//! reserves headroom for interactive link-time checks. Design highlights:
//!
//! - **No thundering herd.** When the daily budget is reached the worker simply
//!   pauses and serves from cache. It never stamps every row to one timestamp,
//!   so quota-day rollover doesn't release a synchronized flood. On the rare
//!   real `quotaExceeded` 403 it requeues the *one* affected row past the reset
//!   with jitter.
//! - **Adaptive cadence.** Subscriptions are stable, so a row whose status
//!   hasn't changed earns an exponentially longer interval (bounded), and a row
//!   that just flipped is re-checked promptly. This concentrates scarce quota on
//!   churn — the single biggest multiplier on effective capacity.
//! - **Batched stats.** Own-channel statistics are refreshed 50-at-a-time via
//!   the public `channels.list?id=` API-key path (1 unit / 50 users) once a
//!   user's channel id is known, falling back to per-user `mine=true` to learn
//!   it (or when no API key is configured).
//! - **Horizontally scalable.** Rows are claimed with `FOR UPDATE SKIP LOCKED`
//!   and partitioned by `hashtext(discord_id) % N`, so N workers (in-process or
//!   across instances) never double-process.
//! - **Accuracy first.** `is_subscribed` is only ever written from a definite
//!   API answer. Quota/token/network failures back the row off; they never flip
//!   a fact, so a role is never stripped because we couldn't check.

use std::sync::Arc;
use std::time::Duration as StdDuration;

use chrono::{Duration, Utc};
use tokio::sync::Mutex;

use crate::error::YouTubeError;
use crate::services::quota::{Class, Outcome};
use crate::services::sync::PlayerSyncEvent;
use crate::services::youtube::CHANNEL_BATCH_MAX;
use crate::AppState;

const MIN_REFRESH_SECS: i64 = 1800; // 30 min floor
const MAX_REFRESH_SECS: i64 = 86400; // 24 hour cap
const INTERVAL_CACHE_SECS: u64 = 300; // recompute base interval every 5 minutes

/// Inactive users (no role_assignments) are refreshed this many times slower.
const INACTIVE_MULTIPLIER: i64 = 6;

/// Channel stats change slowly — refresh at 2x the subscription interval.
const CHANNEL_STATS_MULTIPLIER: i64 = 2;

/// Lease applied to a claimed row so a crash mid-check re-surfaces it after this
/// long instead of stranding it, and concurrent workers skip it meanwhile.
const SUB_LEASE_SECS: f64 = 120.0;
const STATS_LEASE_SECS: f64 = 300.0;

/// Idle nap when there's nothing due.
const IDLE_SLEEP_SECS: u64 = 5;

/// Longest the worker sleeps in one go when paused on quota, so it re-evaluates
/// periodically (e.g. picks up a raised quota or a clock correction).
const MAX_PAUSE_SECS: u64 = 900;

/// A freshly linked user we haven't yet confirmed as subscribed is re-checked
/// this often, so a just-made subscription — or one YouTube's API was briefly
/// slow to surface — is picked up within a minute or two.
pub const FAST_RETRY_SECS: i64 = 90;

/// How long after linking the fast re-check window stays open.
pub const FAST_RETRY_WINDOW_SECS: i64 = 600;

/// Stability multiplier on the base interval: 1,1,2,2,4,4,8,8,16… capped at 16×.
/// A long unchanged streak means the subscription is stable and rarely needs
/// re-confirming, so we stretch its interval and reclaim that quota for churn.
fn stability_factor(streak: i32) -> i64 {
    1i64 << (streak / 2).clamp(0, 4)
}

/// Caches the base refresh interval so we don't size it every cycle. Uses
/// Postgres' `reltuples` estimate instead of a full `COUNT(*)` so it stays
/// cheap at millions of rows — exactness doesn't matter here because the quota
/// governor, not this number, is the hard spend ceiling; this only sets the
/// target freshness / claim ordering.
struct CachedInterval {
    value: std::sync::atomic::AtomicI64,
    quota_per_day: i64,
    last_computed: Mutex<std::time::Instant>,
}

impl CachedInterval {
    fn new(quota_per_day: i64) -> Self {
        Self {
            value: std::sync::atomic::AtomicI64::new(MIN_REFRESH_SECS),
            quota_per_day: quota_per_day.max(1),
            last_computed: Mutex::new(
                std::time::Instant::now() - StdDuration::from_secs(INTERVAL_CACHE_SECS + 1),
            ),
        }
    }

    async fn get(&self, pool: &sqlx::PgPool) -> i64 {
        use std::sync::atomic::Ordering;
        let mut last = self.last_computed.lock().await;
        if last.elapsed() >= StdDuration::from_secs(INTERVAL_CACHE_SECS) {
            let est: i64 = sqlx::query_scalar(
                "SELECT GREATEST(reltuples, 0)::bigint FROM pg_class WHERE relname = 'subscription_cache'",
            )
            .fetch_one(pool)
            .await
            .unwrap_or(0);

            let interval = if est == 0 {
                MIN_REFRESH_SECS
            } else {
                ((est * 86400) / self.quota_per_day).clamp(MIN_REFRESH_SECS, MAX_REFRESH_SECS)
            };
            self.value.store(interval, Ordering::Relaxed);
            *last = std::time::Instant::now();
        }
        self.value.load(std::sync::atomic::Ordering::Relaxed)
    }
}

pub async fn run(state: Arc<AppState>, worker_id: i64, total_workers: i64) {
    let quota = state.config.youtube_quota_per_day;
    let has_api_key = !state.config.youtube_api_keys.is_empty();
    tracing::info!(
        quota,
        worker_id,
        total_workers,
        has_api_key,
        "Refresh worker started"
    );

    let cached = CachedInterval::new(quota);

    loop {
        // Don't even claim/refresh tokens while the background budget is spent —
        // serve from cache until the Pacific reset. Saves churn and is the
        // anti-herd keystone: nothing is stamped, nothing floods at rollover.
        let snap = state.quota.snapshot().await;
        if snap.exhausted || snap.used >= snap.background_budget {
            let nap = (snap.reset_in_secs as u64).clamp(IDLE_SLEEP_SECS, MAX_PAUSE_SECS);
            tracing::warn!(
                used = snap.used,
                background_budget = snap.background_budget,
                reset_in_secs = snap.reset_in_secs,
                "Daily YouTube quota budget reached; pausing background checks (serving from cache)"
            );
            tokio::time::sleep(StdDuration::from_secs(nap)).await;
            continue;
        }

        // 1) Subscription checks — highest priority for role accuracy.
        if process_one_subscription(&state, &cached, worker_id, total_workers).await {
            continue;
        }

        // 2) Batched channel stats (known channel id, API-key path, 50:1).
        if has_api_key && process_stats_batch(&state, &cached, worker_id, total_workers).await {
            continue;
        }

        // 3) Single channel stats (mine=true): learns the channel id for future
        //    batching, and is the only stats path when no API key is configured.
        if process_stats_single(&state, &cached, worker_id, total_workers, has_api_key).await {
            continue;
        }

        tokio::time::sleep(StdDuration::from_secs(IDLE_SLEEP_SECS)).await;
    }
}

/// Claim and process a single due subscription row. Returns true if a row was
/// claimed (whether or not the API call ultimately succeeded), so the caller
/// keeps cycling; false means nothing was due.
async fn process_one_subscription(
    state: &Arc<AppState>,
    cached: &CachedInterval,
    worker_id: i64,
    total_workers: i64,
) -> bool {
    let claimed = sqlx::query_as::<_, (String, String, bool, i32)>(
        "WITH claimed AS ( \
            SELECT s.discord_id, s.channel_id FROM subscription_cache s \
            WHERE s.next_check_at <= now() \
              AND ($2 = 1 OR abs(hashtext(s.discord_id)::bigint) % $2 = $3) \
            ORDER BY s.next_check_at ASC \
            LIMIT 1 FOR UPDATE SKIP LOCKED \
         ) \
         UPDATE subscription_cache sc SET next_check_at = now() + make_interval(secs => $1) \
         FROM claimed \
         WHERE sc.discord_id = claimed.discord_id AND sc.channel_id = claimed.channel_id \
         RETURNING sc.discord_id, sc.channel_id, sc.is_subscribed, sc.stable_streak",
    )
    .bind(SUB_LEASE_SECS)
    .bind(total_workers)
    .bind(worker_id)
    .fetch_optional(&state.pool)
    .await;

    let (discord_id, channel_id, was_subscribed, streak) = match claimed {
        Ok(Some(row)) => row,
        Ok(None) => return false,
        Err(e) => {
            tracing::error!("Subscription claim failed: {e}");
            tokio::time::sleep(StdDuration::from_secs(5)).await;
            return true;
        }
    };

    // Fetch the owner's tokens + activity. The discord_id index added in
    // migration 007 keeps the is_active EXISTS cheap at scale.
    let acct = sqlx::query_as::<_, (String, String, chrono::DateTime<Utc>, bool, chrono::DateTime<Utc>)>(
        "SELECT la.google_access_token, la.google_refresh_token, la.google_token_expires_at, \
         EXISTS(SELECT 1 FROM role_assignments ra WHERE ra.discord_id = la.discord_id) AS is_active, \
         la.linked_at \
         FROM linked_accounts la WHERE la.discord_id = $1",
    )
    .bind(&discord_id)
    .fetch_optional(&state.pool)
    .await;

    let (mut access_token, refresh_token, token_expires_at, is_active, linked_at) = match acct {
        Ok(Some(row)) => row,
        Ok(None) => {
            // Linked account is gone but a cache row lingered — drop the orphan.
            let _ = sqlx::query("DELETE FROM subscription_cache WHERE discord_id = $1")
                .bind(&discord_id)
                .execute(&state.pool)
                .await;
            return true;
        }
        Err(e) => {
            tracing::error!(discord_id, "Account fetch failed: {e}");
            return true;
        }
    };

    // Refresh the Google access token if expired (free — OAuth endpoint, no
    // YouTube quota). Do this before taking a permit so a refresh failure
    // doesn't burn budget.
    if token_expires_at <= Utc::now() {
        match state
            .youtube_client
            .refresh_google_token(&state.config, &refresh_token)
            .await
        {
            Ok(tokens) => {
                access_token = tokens.access_token.clone();
                let new_expires = Utc::now() + Duration::seconds(tokens.expires_in);
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
                    tracing::error!(discord_id, "Failed to persist refreshed tokens: {e}");
                }
            }
            Err(YouTubeError::TokenRevoked) => {
                tracing::warn!(discord_id, "Google token revoked, backing off 24h");
                backoff_subscription_user(&state.pool, &discord_id, Duration::hours(24)).await;
                return true;
            }
            Err(e) => {
                tracing::warn!(discord_id, "Token refresh failed: {e}");
                exp_backoff_subscription(&state.pool, &discord_id, &channel_id).await;
                return true;
            }
        }
    }

    // Take a background quota permit (paced). On exhaustion, requeue this one
    // row past the reset with jitter and pause briefly.
    if let Outcome::Exhausted { retry_after } = state.quota.acquire(Class::Background).await {
        requeue_subscription_after(&state.pool, &discord_id, &channel_id, retry_after).await;
        tokio::time::sleep(pause_dur(retry_after)).await;
        return true;
    }

    match state
        .youtube_client
        .check_subscription(&access_token, &channel_id)
        .await
    {
        Ok(result) => {
            let base = cached.get(&state.pool).await;
            let activity = if is_active { 1 } else { INACTIVE_MULTIPLIER };
            let changed = result.is_subscribed != was_subscribed;
            let new_streak = if changed { 0 } else { (streak + 1).min(100) };

            let within_fast = (Utc::now() - linked_at).num_seconds() < FAST_RETRY_WINDOW_SECS;
            let interval = if !result.is_subscribed && within_fast {
                FAST_RETRY_SECS
            } else {
                (base * activity * stability_factor(new_streak))
                    .clamp(MIN_REFRESH_SECS, MAX_REFRESH_SECS)
            };
            let next_check = Utc::now() + Duration::seconds(interval);

            if let Err(e) = sqlx::query(
                "UPDATE subscription_cache SET \
                 is_subscribed = $1, subscribed_at = $2, checked_at = now(), \
                 next_check_at = $3, check_failures = 0, stable_streak = $4 \
                 WHERE discord_id = $5 AND channel_id = $6",
            )
            .bind(result.is_subscribed)
            .bind(result.subscribed_at)
            .bind(next_check)
            .bind(new_streak)
            .bind(&discord_id)
            .bind(&channel_id)
            .execute(&state.pool)
            .await
            {
                tracing::error!(
                    discord_id,
                    channel_id,
                    "Failed to update subscription cache: {e}"
                );
                return true;
            }

            let _ = state
                .player_sync_tx
                .send(PlayerSyncEvent::PlayerUpdated {
                    discord_id: discord_id.clone(),
                })
                .await;

            tracing::debug!(
                discord_id,
                channel_id,
                is_subscribed = result.is_subscribed,
                is_active,
                new_streak,
                interval,
                "Subscription check complete"
            );
        }
        Err(YouTubeError::QuotaExceeded) => {
            // Our accounting thought we had budget but YouTube disagrees — hard
            // stop until reset and requeue this row past it with jitter.
            state.quota.mark_exhausted().await;
            let snap = state.quota.snapshot().await;
            requeue_subscription_after(
                &state.pool,
                &discord_id,
                &channel_id,
                StdDuration::from_secs(snap.reset_in_secs.max(0) as u64),
            )
            .await;
        }
        Err(YouTubeError::TokenExpired | YouTubeError::TokenRevoked) => {
            tracing::warn!(discord_id, "YouTube token invalid, backing off 24h");
            backoff_subscription_user(&state.pool, &discord_id, Duration::hours(24)).await;
        }
        Err(e) => {
            exp_backoff_subscription(&state.pool, &discord_id, &channel_id).await;
            tracing::warn!(discord_id, channel_id, "YouTube check failed: {e}");
        }
    }
    true
}

/// Claim up to 50 due channel-stat rows whose owners' channel ids we know, fetch
/// them all in one API-key call, and write the results. Returns true if any rows
/// were claimed.
async fn process_stats_batch(
    state: &Arc<AppState>,
    cached: &CachedInterval,
    worker_id: i64,
    total_workers: i64,
) -> bool {
    let claimed: Vec<String> = sqlx::query_scalar(
        "WITH claimed AS ( \
            SELECT c.discord_id FROM channel_cache c \
            JOIN linked_accounts la ON la.discord_id = c.discord_id \
            WHERE c.next_check_at <= now() AND la.youtube_channel_id IS NOT NULL \
              AND ($2 = 1 OR abs(hashtext(c.discord_id)::bigint) % $2 = $3) \
            ORDER BY c.next_check_at ASC \
            LIMIT $4 FOR UPDATE SKIP LOCKED \
         ) \
         UPDATE channel_cache cc SET next_check_at = now() + make_interval(secs => $1) \
         FROM claimed WHERE cc.discord_id = claimed.discord_id \
         RETURNING cc.discord_id",
    )
    .bind(STATS_LEASE_SECS)
    .bind(total_workers)
    .bind(worker_id)
    .bind(CHANNEL_BATCH_MAX as i64)
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default();

    if claimed.is_empty() {
        return false;
    }

    // Map discord_id → (channel_id, is_active).
    let rows = sqlx::query_as::<_, (String, String, bool)>(
        "SELECT la.discord_id, la.youtube_channel_id, \
         EXISTS(SELECT 1 FROM role_assignments ra WHERE ra.discord_id = la.discord_id) AS is_active \
         FROM linked_accounts la \
         WHERE la.discord_id = ANY($1) AND la.youtube_channel_id IS NOT NULL",
    )
    .bind(&claimed)
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default();

    if rows.is_empty() {
        return true;
    }

    let channel_ids: Vec<String> = {
        let mut v: Vec<String> = rows.iter().map(|(_, cid, _)| cid.clone()).collect();
        v.sort();
        v.dedup();
        v
    };

    let api_key = pick_api_key(state);
    if let Outcome::Exhausted { retry_after } = state.quota.acquire(Class::Background).await {
        requeue_stats_after(&state.pool, &claimed, retry_after).await;
        tokio::time::sleep(pause_dur(retry_after)).await;
        return true;
    }

    match state
        .youtube_client
        .batch_channel_stats(&api_key, &channel_ids)
        .await
    {
        Ok(stats_map) => {
            let base = cached.get(&state.pool).await;
            for (discord_id, channel_id, is_active) in &rows {
                let activity = if *is_active { 1 } else { INACTIVE_MULTIPLIER };
                let interval = (base * activity * CHANNEL_STATS_MULTIPLIER)
                    .clamp(MIN_REFRESH_SECS, MAX_REFRESH_SECS);
                let next = Utc::now() + Duration::seconds(interval);

                if let Some(stats) = stats_map.get(channel_id) {
                    write_channel_stats(&state.pool, discord_id, stats, next).await;
                    let _ = state
                        .player_sync_tx
                        .send(PlayerSyncEvent::PlayerUpdated {
                            discord_id: discord_id.clone(),
                        })
                        .await;
                } else {
                    // Channel absent from response (deleted/terminated). Keep the
                    // last-known stats; just age the row normally.
                    let _ = sqlx::query(
                        "UPDATE channel_cache SET checked_at = now(), next_check_at = $1, check_failures = 0 \
                         WHERE discord_id = $2",
                    )
                    .bind(next)
                    .bind(discord_id)
                    .execute(&state.pool)
                    .await;
                }
            }
            tracing::debug!(
                count = rows.len(),
                fetched = stats_map.len(),
                "Batched channel stats updated"
            );
        }
        Err(YouTubeError::QuotaExceeded) => {
            state.quota.mark_exhausted().await;
            let snap = state.quota.snapshot().await;
            requeue_stats_after(
                &state.pool,
                &claimed,
                StdDuration::from_secs(snap.reset_in_secs.max(0) as u64),
            )
            .await;
        }
        Err(e) => {
            tracing::warn!("Batched channel stats fetch failed: {e}");
            // Short exponential-ish backoff so a transient error doesn't hot-loop.
            for id in &claimed {
                let _ = sqlx::query(
                    "UPDATE channel_cache SET check_failures = check_failures + 1, \
                     next_check_at = now() + LEAST(INTERVAL '60 seconds' * POWER(2, check_failures), INTERVAL '6 hours') \
                     WHERE discord_id = $1",
                )
                .bind(id)
                .execute(&state.pool)
                .await;
            }
        }
    }
    true
}

/// Per-user `mine=true` stats fetch. Used to learn a user's channel id (so they
/// move to the batched path next time) and as the only stats path when no API
/// key is configured. Returns true if a row was claimed.
async fn process_stats_single(
    state: &Arc<AppState>,
    cached: &CachedInterval,
    worker_id: i64,
    total_workers: i64,
    has_api_key: bool,
) -> bool {
    // With an API key, this path only mops up rows we can't batch yet (unknown
    // channel id). Without one, it handles every stats row.
    let channel_filter = if has_api_key {
        "AND la.youtube_channel_id IS NULL"
    } else {
        ""
    };
    let claim_sql = format!(
        "WITH claimed AS ( \
            SELECT c.discord_id FROM channel_cache c \
            JOIN linked_accounts la ON la.discord_id = c.discord_id \
            WHERE c.next_check_at <= now() {channel_filter} \
              AND ($2 = 1 OR abs(hashtext(c.discord_id)::bigint) % $2 = $3) \
            ORDER BY c.next_check_at ASC \
            LIMIT 1 FOR UPDATE SKIP LOCKED \
         ) \
         UPDATE channel_cache cc SET next_check_at = now() + make_interval(secs => $1) \
         FROM claimed WHERE cc.discord_id = claimed.discord_id \
         RETURNING cc.discord_id"
    );

    let discord_id: Option<String> = sqlx::query_scalar(&claim_sql)
        .bind(STATS_LEASE_SECS)
        .bind(total_workers)
        .bind(worker_id)
        .fetch_optional(&state.pool)
        .await
        .unwrap_or(None);

    let Some(discord_id) = discord_id else {
        return false;
    };

    let acct = sqlx::query_as::<_, (String, String, chrono::DateTime<Utc>, bool)>(
        "SELECT la.google_access_token, la.google_refresh_token, la.google_token_expires_at, \
         EXISTS(SELECT 1 FROM role_assignments ra WHERE ra.discord_id = la.discord_id) AS is_active \
         FROM linked_accounts la WHERE la.discord_id = $1",
    )
    .bind(&discord_id)
    .fetch_optional(&state.pool)
    .await;

    let (mut access_token, refresh_token, token_expires_at, is_active) = match acct {
        Ok(Some(row)) => row,
        Ok(None) => {
            let _ = sqlx::query("DELETE FROM channel_cache WHERE discord_id = $1")
                .bind(&discord_id)
                .execute(&state.pool)
                .await;
            return true;
        }
        Err(e) => {
            tracing::error!(discord_id, "Account fetch failed (stats): {e}");
            return true;
        }
    };

    if token_expires_at <= Utc::now() {
        match state
            .youtube_client
            .refresh_google_token(&state.config, &refresh_token)
            .await
        {
            Ok(tokens) => {
                access_token = tokens.access_token.clone();
                let new_expires = Utc::now() + Duration::seconds(tokens.expires_in);
                let new_refresh = tokens.refresh_token.as_deref().unwrap_or(&refresh_token);
                let _ = sqlx::query(
                    "UPDATE linked_accounts SET google_access_token = $1, google_refresh_token = $2, \
                     google_token_expires_at = $3 WHERE discord_id = $4",
                )
                .bind(&tokens.access_token)
                .bind(new_refresh)
                .bind(new_expires)
                .bind(&discord_id)
                .execute(&state.pool)
                .await;
            }
            Err(YouTubeError::TokenRevoked) => {
                backoff_stats_user(&state.pool, &discord_id, Duration::hours(24)).await;
                return true;
            }
            Err(e) => {
                tracing::warn!(discord_id, "Token refresh failed (stats): {e}");
                exp_backoff_stats(&state.pool, &discord_id).await;
                return true;
            }
        }
    }

    if let Outcome::Exhausted { retry_after } = state.quota.acquire(Class::Background).await {
        requeue_stats_after(&state.pool, std::slice::from_ref(&discord_id), retry_after).await;
        tokio::time::sleep(pause_dur(retry_after)).await;
        return true;
    }

    match state
        .youtube_client
        .fetch_channel_stats(&access_token)
        .await
    {
        Ok(stats) => {
            // Persist the learned channel id so future refreshes batch.
            if let Some(ref cid) = stats.channel_id {
                let _ = sqlx::query(
                    "UPDATE linked_accounts SET youtube_channel_id = $1 WHERE discord_id = $2",
                )
                .bind(cid)
                .bind(&discord_id)
                .execute(&state.pool)
                .await;
            }
            let base = cached.get(&state.pool).await;
            let activity = if is_active { 1 } else { INACTIVE_MULTIPLIER };
            let interval = (base * activity * CHANNEL_STATS_MULTIPLIER)
                .clamp(MIN_REFRESH_SECS, MAX_REFRESH_SECS);
            let next = Utc::now() + Duration::seconds(interval);
            write_channel_stats(&state.pool, &discord_id, &stats, next).await;

            let _ = state
                .player_sync_tx
                .send(PlayerSyncEvent::PlayerUpdated {
                    discord_id: discord_id.clone(),
                })
                .await;
        }
        Err(YouTubeError::QuotaExceeded) => {
            state.quota.mark_exhausted().await;
            let snap = state.quota.snapshot().await;
            requeue_stats_after(
                &state.pool,
                std::slice::from_ref(&discord_id),
                StdDuration::from_secs(snap.reset_in_secs.max(0) as u64),
            )
            .await;
        }
        Err(YouTubeError::TokenExpired | YouTubeError::TokenRevoked) => {
            backoff_stats_user(&state.pool, &discord_id, Duration::hours(24)).await;
        }
        Err(e) => {
            tracing::warn!(discord_id, "Channel stats fetch failed: {e}");
            exp_backoff_stats(&state.pool, &discord_id).await;
        }
    }
    true
}

// --------------------------------------------------------------------------
// Helpers
// --------------------------------------------------------------------------

fn pick_api_key(state: &Arc<AppState>) -> String {
    let keys = &state.config.youtube_api_keys;
    if keys.is_empty() {
        return String::new();
    }
    // Round-robin across keys (a quota multiplier only across distinct projects).
    let idx = (Utc::now().timestamp_millis() as usize) % keys.len();
    keys[idx].clone()
}

async fn write_channel_stats(
    pool: &sqlx::PgPool,
    discord_id: &str,
    stats: &crate::services::youtube::ChannelStats,
    next: chrono::DateTime<Utc>,
) {
    if let Err(e) = sqlx::query(
        "INSERT INTO channel_cache \
         (discord_id, subscriber_count, view_count, video_count, channel_created_at, \
          hidden_subscribers, country, custom_url, checked_at, next_check_at, check_failures) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, now(), $9, 0) \
         ON CONFLICT (discord_id) DO UPDATE SET \
          subscriber_count = $2, view_count = $3, video_count = $4, channel_created_at = $5, \
          hidden_subscribers = $6, country = $7, custom_url = $8, checked_at = now(), \
          next_check_at = $9, check_failures = 0",
    )
    .bind(discord_id)
    .bind(stats.subscriber_count)
    .bind(stats.view_count)
    .bind(stats.video_count)
    .bind(stats.channel_created_at)
    .bind(stats.hidden_subscriber_count)
    .bind(&stats.country)
    .bind(&stats.custom_url)
    .bind(next)
    .execute(pool)
    .await
    {
        tracing::error!(discord_id, "Failed to update channel cache: {e}");
    }
}

/// How long to nap after an `Exhausted`, bounded so we re-evaluate periodically.
fn pause_dur(retry_after: StdDuration) -> StdDuration {
    retry_after
        .min(StdDuration::from_secs(MAX_PAUSE_SECS))
        .max(StdDuration::from_secs(IDLE_SLEEP_SECS))
}

/// Requeue a subscription row to land *after* `retry_after` plus up to 30 min of
/// jitter — so rows blocked by exhaustion spread out across the reset instead of
/// stampeding the moment quota returns.
async fn requeue_subscription_after(
    pool: &sqlx::PgPool,
    discord_id: &str,
    channel_id: &str,
    retry_after: StdDuration,
) {
    let at = Utc::now()
        + Duration::from_std(retry_after).unwrap_or_else(|_| Duration::zero())
        + Duration::seconds((rand::random::<u64>() % 1800) as i64);
    let _ = sqlx::query(
        "UPDATE subscription_cache SET next_check_at = $1 WHERE discord_id = $2 AND channel_id = $3",
    )
    .bind(at)
    .bind(discord_id)
    .bind(channel_id)
    .execute(pool)
    .await;
}

async fn requeue_stats_after(
    pool: &sqlx::PgPool,
    discord_ids: &[String],
    retry_after: StdDuration,
) {
    let at = Utc::now()
        + Duration::from_std(retry_after).unwrap_or_else(|_| Duration::zero())
        + Duration::seconds((rand::random::<u64>() % 1800) as i64);
    let _ = sqlx::query("UPDATE channel_cache SET next_check_at = $1 WHERE discord_id = ANY($2)")
        .bind(at)
        .bind(discord_ids)
        .execute(pool)
        .await;
}

/// Back off every subscription row for one user (e.g. token revoked).
async fn backoff_subscription_user(pool: &sqlx::PgPool, discord_id: &str, by: Duration) {
    let at = Utc::now() + by;
    let _ = sqlx::query(
        "UPDATE subscription_cache SET next_check_at = $1, check_failures = check_failures + 1 \
         WHERE discord_id = $2",
    )
    .bind(at)
    .bind(discord_id)
    .execute(pool)
    .await;
}

async fn exp_backoff_subscription(pool: &sqlx::PgPool, discord_id: &str, channel_id: &str) {
    let _ = sqlx::query(
        "UPDATE subscription_cache SET check_failures = check_failures + 1, \
         next_check_at = now() + LEAST(INTERVAL '60 seconds' * POWER(2, check_failures), INTERVAL '1 hour') \
         WHERE discord_id = $1 AND channel_id = $2",
    )
    .bind(discord_id)
    .bind(channel_id)
    .execute(pool)
    .await;
}

async fn backoff_stats_user(pool: &sqlx::PgPool, discord_id: &str, by: Duration) {
    let at = Utc::now() + by;
    let _ = sqlx::query(
        "UPDATE channel_cache SET next_check_at = $1, check_failures = check_failures + 1 WHERE discord_id = $2",
    )
    .bind(at)
    .bind(discord_id)
    .execute(pool)
    .await;
}

async fn exp_backoff_stats(pool: &sqlx::PgPool, discord_id: &str) {
    let _ = sqlx::query(
        "UPDATE channel_cache SET check_failures = check_failures + 1, \
         next_check_at = now() + LEAST(INTERVAL '60 seconds' * POWER(2, check_failures), INTERVAL '6 hours') \
         WHERE discord_id = $1",
    )
    .bind(discord_id)
    .execute(pool)
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stability_curve_grows_and_caps() {
        // 1,1,2,2,4,4,8,8,16,16,16… — doubles every two stable checks, caps 16×.
        assert_eq!(stability_factor(0), 1);
        assert_eq!(stability_factor(1), 1);
        assert_eq!(stability_factor(2), 2);
        assert_eq!(stability_factor(3), 2);
        assert_eq!(stability_factor(4), 4);
        assert_eq!(stability_factor(6), 8);
        assert_eq!(stability_factor(8), 16);
        assert_eq!(stability_factor(100), 16); // capped
    }

    #[test]
    fn stable_active_user_interval_is_bounded() {
        // A long-stable active user: base 30min × activity 1 × 16 = 8h, under the
        // 24h cap — i.e. ~16× fewer checks than a volatile user, never exceeding
        // the daily ceiling.
        let base = MIN_REFRESH_SECS;
        let interval = (base * 1 * stability_factor(100)).clamp(MIN_REFRESH_SECS, MAX_REFRESH_SECS);
        assert_eq!(interval, MIN_REFRESH_SECS * 16);
        assert!(interval <= MAX_REFRESH_SECS);
    }
}
