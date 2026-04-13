-- Backfill columns that may be missing if channel_cache was created by an earlier
-- version of migration 003 before these columns were added to the CREATE TABLE.
ALTER TABLE channel_cache ADD COLUMN IF NOT EXISTS custom_url TEXT;
ALTER TABLE channel_cache ADD COLUMN IF NOT EXISTS hidden_subscribers BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE channel_cache ADD COLUMN IF NOT EXISTS country TEXT;
ALTER TABLE channel_cache ADD COLUMN IF NOT EXISTS channel_created_at TIMESTAMPTZ;
