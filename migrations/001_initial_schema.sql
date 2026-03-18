-- Role links: one per guild+role pair registered via POST /register
CREATE TABLE IF NOT EXISTS role_links (
    id              BIGSERIAL PRIMARY KEY,
    guild_id        TEXT NOT NULL,
    role_id         TEXT NOT NULL,
    api_token       TEXT NOT NULL,
    channel_id      TEXT,
    conditions      JSONB NOT NULL DEFAULT '[]',
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (guild_id, role_id)
);

-- Linked accounts: Discord <-> Google/YouTube
CREATE TABLE IF NOT EXISTS linked_accounts (
    id                      BIGSERIAL PRIMARY KEY,
    discord_id              TEXT NOT NULL UNIQUE,
    discord_name            TEXT,
    google_access_token     TEXT NOT NULL,
    google_refresh_token    TEXT NOT NULL,
    google_token_expires_at TIMESTAMPTZ NOT NULL,
    linked_at               TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Subscription cache: tracks whether a user is subscribed to a channel
CREATE TABLE IF NOT EXISTS subscription_cache (
    discord_id      TEXT NOT NULL,
    channel_id      TEXT NOT NULL,
    is_subscribed   BOOLEAN NOT NULL DEFAULT FALSE,
    checked_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    next_check_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    check_failures  INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (discord_id, channel_id)
);
CREATE INDEX IF NOT EXISTS idx_sub_cache_next_check ON subscription_cache (next_check_at ASC);

-- Role assignments: tracks which users currently have which roles (local mirror)
CREATE TABLE IF NOT EXISTS role_assignments (
    guild_id        TEXT NOT NULL,
    role_id         TEXT NOT NULL,
    discord_id      TEXT NOT NULL,
    assigned_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (guild_id, role_id, discord_id),
    FOREIGN KEY (guild_id, role_id) REFERENCES role_links (guild_id, role_id) ON DELETE CASCADE
);

-- OAuth states: CSRF protection for Discord and Google OAuth flows
CREATE TABLE IF NOT EXISTS oauth_states (
    state           TEXT PRIMARY KEY,
    redirect_data   JSONB,
    expires_at      TIMESTAMPTZ NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- User guilds: tracks which guilds each Discord user belongs to
CREATE TABLE IF NOT EXISTS user_guilds (
    discord_id      TEXT NOT NULL,
    guild_id        TEXT NOT NULL,
    guild_name      TEXT,
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (discord_id, guild_id)
);
CREATE INDEX IF NOT EXISTS idx_user_guilds_guild ON user_guilds (guild_id);

-- Discord tokens: stored for periodic guild membership refresh
CREATE TABLE IF NOT EXISTS discord_tokens (
    discord_id          TEXT PRIMARY KEY,
    refresh_token       TEXT NOT NULL,
    guilds_refreshed_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
