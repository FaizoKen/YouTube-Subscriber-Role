-- 007: Quota governor + scaling hardening.
--
-- Adds durable daily-quota accounting (survives restarts, shared across
-- workers/instances), the member's own channel id for batched channels.list
-- lookups (50 ids → 1 unit), per-row subscription stability tracking for an
-- adaptive re-check cadence, and the indexes the claim / active-priority paths
-- need once these tables hold millions of rows.

-- Durable per-day YouTube Data API quota ledger. One row per quota-day, keyed
-- by the Pacific-time reset date Google bills against, so accounting survives a
-- restart (in-memory counters would otherwise reset to 0 and over-spend). Every
-- quota-costing call increments used_units; the governor reloads today's value
-- on boot. BIGINT so a raised quota (millions/day) can never overflow.
CREATE TABLE IF NOT EXISTS api_quota_usage (
    quota_date   DATE PRIMARY KEY,
    used_units   BIGINT NOT NULL DEFAULT 0,
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- The member's OWN YouTube channel id, captured once at link time. Lets the
-- refresh worker fetch channel statistics in batches of up to 50 via
-- channels.list?id=<csv> (public data, API key, 1 unit per 50) instead of one
-- mine=true OAuth call per user. NULL until first observed; the worker falls
-- back to the per-user mine=true path for those rows.
ALTER TABLE linked_accounts ADD COLUMN IF NOT EXISTS youtube_channel_id TEXT;

-- Adaptive cadence: how many consecutive checks have returned the SAME
-- subscription status. Subscriptions are very stable, so a high streak earns an
-- exponentially longer interval (bounded), concentrating scarce quota on churn
-- instead of re-confirming long-time subscribers. Reset to 0 whenever the
-- status flips so a fresh unsubscribe is caught quickly.
ALTER TABLE subscription_cache ADD COLUMN IF NOT EXISTS stable_streak INTEGER NOT NULL DEFAULT 0;

-- The EXISTS(role_assignments WHERE discord_id = ...) "is this user active"
-- probe runs on every refresh claim. role_assignments' PK is
-- (guild_id, role_id, discord_id), so a discord_id-only lookup was a scan.
CREATE INDEX IF NOT EXISTS idx_role_assignments_discord ON role_assignments (discord_id);

-- Batched-stats claim groups due channel_cache rows by their owner's channel id.
CREATE INDEX IF NOT EXISTS idx_linked_accounts_yt_channel
    ON linked_accounts (youtube_channel_id)
    WHERE youtube_channel_id IS NOT NULL;
