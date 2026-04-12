-- Add subscription creation timestamp (extracted from YouTube API snippet.publishedAt)
ALTER TABLE subscription_cache ADD COLUMN IF NOT EXISTS subscribed_at TIMESTAMPTZ;

-- Channel cache: stores the user's own YouTube channel statistics
-- Keyed on discord_id (one YouTube account per Discord user, independent of target channel)
CREATE TABLE IF NOT EXISTS channel_cache (
    discord_id          TEXT PRIMARY KEY,
    subscriber_count    BIGINT,
    view_count          BIGINT,
    video_count         BIGINT,
    channel_created_at  TIMESTAMPTZ,
    country             TEXT,
    custom_url          TEXT,
    hidden_subscribers  BOOLEAN NOT NULL DEFAULT FALSE,
    checked_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    next_check_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    check_failures      INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_channel_cache_next_check ON channel_cache (next_check_at ASC);
