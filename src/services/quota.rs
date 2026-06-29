//! Central YouTube Data API quota governor.
//!
//! The single hardest constraint in this plugin: the YouTube Data API bills
//! quota **per Google Cloud project** (default 10,000 units/day), and a
//! `subscriptions.list` check costs **1 unit per user with no batch API** —
//! subscription data is private to each user and needs their OAuth token. So the
//! project quota is a hard ceiling shared by every user and every code path.
//!
//! Before this module the only thing keeping spend under the daily cap was the
//! per-row `next_check_at` spacing, plus a naive `governor` limiter set to
//! 10 req/s — which is ~864k/day, 86× the cap. Two failure modes followed:
//!
//!   1. **Thundering herd.** On a `quotaExceeded` 403 the worker stamped every
//!      due row to `now() + 1h` and slept 1h. An hour later they all came due at
//!      once, drained at 10/s, and instantly re-exceeded quota — the repeating
//!      "quota exceeded, backing off until reset" log. Worse, quota resets at
//!      *midnight Pacific*, not "1 hour later", so the retry kept failing until
//!      reset.
//!   2. **Unaccounted inline spend.** Inline link-time checks made API calls
//!      with no daily accounting at all, so a verify spike (an @everyone ping)
//!      could burn the whole day's quota in minutes, blind to the worker.
//!
//! The governor fixes both by being the *one* place every quota-costing call
//! must pass through. It:
//!   - Tracks units spent in the current **Pacific** quota-day, persisted to
//!     `api_quota_usage` so a restart doesn't reset the counter and over-spend.
//!   - Splits the budget into an **interactive reserve** (link-time checks a
//!     user is actively waiting on) and a **background** pool (routine
//!     re-checks). Background can never eat the reserve, so a verify spike never
//!     starves real-time verification; interactive may borrow idle background
//!     headroom.
//!   - **Paces** background calls smoothly across the whole day
//!     (`spacing = time_to_reset / remaining_background_budget`), so spend is a
//!     gentle trickle instead of bursts — no herd, ever.
//!   - Stops *itself* before YouTube has to (`Outcome::Exhausted`), and treats a
//!     real 403 as a hard stop until the true Pacific reset.
//!
//! Multi-instance note: `used` is reconciled against the DB on every flush via
//! an atomic delta-add, so N processes converge on the shared project total
//! within one flush interval. Per-process pacing is independent, so for true
//! horizontal scale either run one refresh process or divide the configured
//! quota across instances. Single-process (the default) is exact.

use std::sync::Arc;
use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, NaiveDate, Utc};
use sqlx::PgPool;
use tokio::sync::Mutex;

use crate::services::pacific;

/// Which budget pool a call draws from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// A user is actively waiting on this result (link-time inline check).
    Interactive,
    /// Routine background re-check.
    Background,
}

/// Result of requesting permission to spend one quota unit.
#[derive(Debug)]
pub enum Outcome {
    /// Go ahead — one unit has been reserved (and, for background, paced).
    Granted,
    /// No budget left in this pool; try again after `retry_after`.
    Exhausted { retry_after: StdDuration },
}

/// Longest a single background call will be paced to wait. Keeps the worker
/// responsive (and able to notice a quota bump) even when the trickle is slow.
const MAX_SPACING_MS: i64 = 60_000;

/// How often the in-memory counter is persisted to the durable ledger.
const FLUSH_INTERVAL: StdDuration = StdDuration::from_secs(10);

/// Short-term burst cap shared by all classes — politeness to the API and a
/// smoother retry profile. Far above the daily-budget trickle; it only clips
/// genuine bursts (a verify spike draining the interactive reserve).
const BURST_PER_SEC: u32 = 25;

struct Inner {
    /// The Pacific quota-day these counters belong to.
    date: NaiveDate,
    /// Units reserved so far today (interactive + background).
    used: i64,
    /// Units reserved but not yet written to the ledger.
    unflushed: i64,
    /// Earliest instant the next background call may proceed (pacing gate).
    bg_next_at: DateTime<Utc>,
    /// Hard stop set when YouTube itself reports quotaExceeded.
    exhausted_until: Option<DateTime<Utc>>,
}

pub struct QuotaGovernor {
    pool: PgPool,
    /// Usable daily budget = configured quota × safety fraction.
    total_budget: i64,
    /// Ceiling for background calls; the gap up to `total_budget` is the
    /// interactive-only reserve.
    background_budget: i64,
    inner: Mutex<Inner>,
    burst: governor::RateLimiter<
        governor::state::NotKeyed,
        governor::state::InMemoryState,
        governor::clock::DefaultClock,
    >,
}

/// A point-in-time view for health/observability.
#[derive(Debug, Clone)]
pub struct QuotaSnapshot {
    pub date: NaiveDate,
    pub used: i64,
    pub total_budget: i64,
    pub background_budget: i64,
    pub reset_in_secs: i64,
    pub exhausted: bool,
}

impl QuotaSnapshot {
    pub fn remaining(&self) -> i64 {
        (self.total_budget - self.used).max(0)
    }
}

impl QuotaGovernor {
    /// Build the governor, loading today's already-spent units from the durable
    /// ledger so a restart resumes accounting instead of resetting to zero.
    pub async fn new(
        pool: PgPool,
        quota_per_day: i64,
        reserve_frac: f64,
        safety_frac: f64,
    ) -> Arc<Self> {
        let safety = safety_frac.clamp(0.5, 1.0);
        let reserve = reserve_frac.clamp(0.0, 0.9);
        let total_budget = ((quota_per_day as f64) * safety).floor().max(1.0) as i64;
        let background_budget = ((total_budget as f64) * (1.0 - reserve)).floor().max(1.0) as i64;

        let now = Utc::now();
        let date = pacific::pacific_date(now);
        let used = sqlx::query_scalar::<_, i64>(
            "SELECT used_units FROM api_quota_usage WHERE quota_date = $1",
        )
        .bind(date)
        .fetch_optional(&pool)
        .await
        .ok()
        .flatten()
        .unwrap_or(0);

        // Burst cap scales with the budget: ~4× the average sustainable rate, so
        // it only clips genuine spikes (a verify storm draining the reserve) and
        // never becomes the throughput ceiling when the quota is raised for
        // large deployments. Floored at BURST_PER_SEC for small quotas.
        let burst_rate =
            (((total_budget * 4) / 86_400).max(BURST_PER_SEC as i64)).min(100_000) as u32;
        let burst_quota =
            governor::Quota::per_second(std::num::NonZeroU32::new(burst_rate.max(1)).unwrap());
        let burst = governor::RateLimiter::direct(burst_quota);

        tracing::info!(
            total_budget,
            background_budget,
            interactive_reserve = total_budget - background_budget,
            used_today = used,
            "Quota governor initialized"
        );

        Arc::new(Self {
            pool,
            total_budget,
            background_budget,
            inner: Mutex::new(Inner {
                date,
                used,
                unflushed: 0,
                bg_next_at: now,
                exhausted_until: None,
            }),
            burst,
        })
    }

    fn next_reset(&self, now: DateTime<Utc>) -> DateTime<Utc> {
        pacific::next_reset(now)
    }

    /// Roll the in-memory counters over when the Pacific day changes. The prior
    /// day's final tally is already in the ledger from the last flush.
    fn roll_over(&self, g: &mut Inner, now: DateTime<Utc>) {
        let today = pacific::pacific_date(now);
        if g.date != today {
            g.date = today;
            g.used = 0;
            g.unflushed = 0;
            g.bg_next_at = now;
            g.exhausted_until = None;
        }
    }

    /// Request permission to spend one quota unit. On `Granted`, exactly one
    /// unit has been reserved; background calls additionally sleep to honor the
    /// daily pacing. Reserve-on-grant (no refund) keeps concurrent interactive
    /// callers from over-committing — a rare failed call just costs us one unit
    /// of conservatism, never an over-spend.
    pub async fn acquire(&self, class: Class) -> Outcome {
        // Politeness burst cap first, so a spike queues here, not at the API.
        self.burst.until_ready().await;

        let now = Utc::now();
        let wait_ms: i64;
        {
            let mut g = self.inner.lock().await;
            self.roll_over(&mut g, now);

            if let Some(until) = g.exhausted_until {
                if now < until {
                    return Outcome::Exhausted {
                        retry_after: (until - now).to_std().unwrap_or(StdDuration::ZERO),
                    };
                }
                g.exhausted_until = None;
            }

            let ceiling = match class {
                Class::Background => self.background_budget,
                Class::Interactive => self.total_budget,
            };
            if g.used >= ceiling {
                return Outcome::Exhausted {
                    retry_after: (self.next_reset(now) - now)
                        .to_std()
                        .unwrap_or(StdDuration::ZERO),
                };
            }

            // Reserve the unit.
            g.used += 1;
            g.unflushed += 1;

            wait_ms = match class {
                Class::Interactive => 0,
                Class::Background => {
                    let remaining = (self.background_budget - g.used).max(1);
                    let secs_to_reset = (self.next_reset(now) - now).num_seconds().max(1);
                    let spacing = ((secs_to_reset * 1000) / remaining).clamp(0, MAX_SPACING_MS);
                    let base = if g.bg_next_at > now {
                        g.bg_next_at
                    } else {
                        now
                    };
                    let w = (base - now).num_milliseconds().max(0);
                    g.bg_next_at = base + Duration::milliseconds(spacing);
                    w
                }
            };
        }

        if wait_ms > 0 {
            tokio::time::sleep(StdDuration::from_millis(wait_ms as u64)).await;
        }
        Outcome::Granted
    }

    /// Safety net: YouTube returned `quotaExceeded` despite our accounting
    /// (budget set too high, or external spend on the same project). Stop all
    /// calls until the true Pacific reset.
    pub async fn mark_exhausted(&self) {
        let now = Utc::now();
        let until = self.next_reset(now);
        let mut g = self.inner.lock().await;
        g.exhausted_until = Some(until);
        g.used = g.used.max(self.total_budget);
        tracing::warn!(
            reset_in_secs = (until - now).num_seconds(),
            "YouTube reported quotaExceeded — hard-stopping API calls until Pacific reset"
        );
    }

    /// Persist the unflushed delta to the durable ledger and reconcile the
    /// in-memory total against the shared project total (multi-instance safe).
    pub async fn flush(&self) {
        let (date, delta) = {
            let mut g = self.inner.lock().await;
            let d = g.unflushed;
            g.unflushed = 0;
            (g.date, d)
        };
        if delta <= 0 {
            return;
        }
        match sqlx::query_scalar::<_, i64>(
            "INSERT INTO api_quota_usage (quota_date, used_units, updated_at) \
             VALUES ($1, $2, now()) \
             ON CONFLICT (quota_date) DO UPDATE SET \
               used_units = api_quota_usage.used_units + EXCLUDED.used_units, updated_at = now() \
             RETURNING used_units",
        )
        .bind(date)
        .bind(delta)
        .fetch_one(&self.pool)
        .await
        {
            Ok(db_used) => {
                let mut g = self.inner.lock().await;
                if g.date == date {
                    g.used = g.used.max(db_used);
                }
            }
            Err(e) => {
                tracing::error!("Quota ledger flush failed: {e}");
                let mut g = self.inner.lock().await;
                g.unflushed += delta; // retry next interval
            }
        }
    }

    pub async fn snapshot(&self) -> QuotaSnapshot {
        let now = Utc::now();
        let mut g = self.inner.lock().await;
        // Roll the day over here too: the background worker polls snapshot while
        // paused on exhaustion and never calls acquire, so without this it could
        // stay paused after the Pacific reset until some interactive call rolled
        // the counter for it.
        self.roll_over(&mut g, now);
        QuotaSnapshot {
            date: g.date,
            used: g.used,
            total_budget: self.total_budget,
            background_budget: self.background_budget,
            reset_in_secs: (self.next_reset(now) - now).num_seconds().max(0),
            exhausted: g.exhausted_until.is_some_and(|u| now < u) || g.used >= self.total_budget,
        }
    }

    /// Background task: periodically persist the counter.
    pub async fn run_flusher(self: Arc<Self>) {
        tracing::info!("Quota ledger flusher started");
        loop {
            tokio::time::sleep(FLUSH_INTERVAL).await;
            self.flush().await;
        }
    }
}
