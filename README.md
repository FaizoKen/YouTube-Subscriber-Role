# YouTube Subscriber Role

A [RoleLogic](https://rolelogic.faizo.net) plugin server that automatically assigns Discord roles to members who are subscribed to a specific YouTube channel.

> **Requires [Auth Gateway](https://github.com/FaizoKen/Auth-Gateway)** — Discord login is handled by the centralized Auth Gateway. This plugin reads the shared `rl_session` cookie set by the gateway. Google OAuth for YouTube linking is handled directly by this plugin.

## How It Works

1. **Admin** creates a role link in the RoleLogic dashboard. The plugin uses **iframe UI mode** (BLUEPRINT §1b): RoleLogic embeds the plugin's own rule-builder page, where the admin enters the YouTube Channel ID and picks a preset (*Anyone subscribed*, *Long-time subscribers ≥ N days*, *Subscribers with an established channel ≥ N subs*, *Creators with an audience ≥ N subs on their own channel — no subscription needed*, *Anyone who linked YouTube*) or builds an advanced **OR-of-AND** rule across subscription + channel-stat conditions, with a live "X of Y members match" preview. The Channel ID is only required when the rule actually checks subscriptions — stat-only rules (e.g. the member's own subscriber count) work without one
2. **Members** visit the verification page, sign in with Discord (via Auth Gateway), then link their YouTube account via Google OAuth
3. **Plugin** checks the member's subscription **immediately** at link time (inline, using the freshly-issued token) and assigns the role before the page reloads — no waiting on the background worker
4. **Plugin** then re-checks each member's subscription status periodically using the YouTube Data API to keep roles in sync
5. **Qualifying members** keep the configured Discord role; those who no longer match have it removed

## Tech Stack

- **Rust** + **Axum 0.8** — lightweight, low-memory HTTP server
- **PostgreSQL 16** — persistent storage
- **SQLx** — async, compile-time checked SQL
- **Tokio** — async runtime with background workers

## Setup

### Prerequisites

- [Rust](https://rustup.rs/) 1.88+
- PostgreSQL 16
- Docker & Docker Compose (for deployment)
- [Auth Gateway](https://github.com/FaizoKen/Auth-Gateway) running on `your-domain.com/auth/*`
- [Google Cloud Project](https://console.cloud.google.com/) with YouTube Data API v3 enabled and OAuth2 credentials

### Environment Variables

Copy `.env.example` to `.env` and fill in:

```env
DATABASE_URL=postgres://ysr:password@db:5432/youtube_sub_role
GOOGLE_CLIENT_ID=your_google_client_id
GOOGLE_CLIENT_SECRET=your_google_client_secret
SESSION_SECRET=random_secret_string       # must match Auth Gateway
BASE_URL=https://your-domain.com/youtube-subscriber-role
LISTEN_ADDR=0.0.0.0:8080
YOUTUBE_QUOTA_PER_DAY=10000
# Origin of the RoleLogic dashboard that embeds the iframe config page (CSP
# frame-ancestors). Leave unset for `*` in dev; set explicitly in production.
RL_DASHBOARD_ORIGIN=https://app.rolelogic.com
```

### Google OAuth Setup

1. Go to [Google Cloud Console](https://console.cloud.google.com/)
2. Enable **YouTube Data API v3**
3. Create **OAuth 2.0 Client ID** (Web application)
4. Add authorized redirect URI: `https://your-domain.com/youtube-subscriber-role/verify/youtube/callback`
5. Copy Client ID and Client Secret to `.env`

### Run with Docker Compose

```bash
docker compose up -d
```

### Run Locally

```bash
cargo run
```

## API Endpoints

All routes are nested under `/youtube-subscriber-role`:

### RoleLogic Plugin (called by RoleLogic dashboard)

| Method   | Path        | Purpose                                              |
| -------- | ----------- | --------------------------------------------------- |
| `POST`   | `/register` | Acknowledge role link creation                      |
| `GET`    | `/config`   | Return iframe-mode config (`ui_mode: "iframe"`)     |
| `POST`   | `/config`   | No-op stub (token-verified; never called in iframe) |
| `DELETE` | `/config`   | Clean up on role link deletion                      |

### Admin Rule Builder (iframe UI, dual-mode auth)

Embedded by the dashboard; also reachable directly (cookie + Manage Server).

| Method     | Path                                       | Purpose                                       |
| ---------- | ------------------------------------------ | --------------------------------------------- |
| `GET`      | `/admin/{guild}/role/{role}`               | Rule-builder page (verifies `?rl_token=` JWT) |
| `GET`      | `/admin/{guild}/role/{role}/data`          | Current rule + target/operator catalogs       |
| `POST`     | `/admin/{guild}/role/{role}/save`          | Save rule tree (optimistic-locked)            |
| `GET/POST` | `/admin/{guild}/role/{role}/preview`       | Count matching members (saved / proposed)     |
| `POST`     | `/admin/{guild}/view-permission`           | Set subscribers-list visibility               |

### User Verification

| Method | Path                       | Purpose                                     |
| ------ | -------------------------- | ------------------------------------------- |
| `GET`  | `/verify`                  | Verification page (HTML)                    |
| `GET`  | `/verify/login`            | Redirects to Auth Gateway for Discord login |
| `GET`  | `/verify/youtube`          | Google OAuth redirect                       |
| `GET`  | `/verify/youtube/callback` | Google OAuth callback                       |
| `GET`  | `/verify/status`           | Current link status (JSON)                  |
| `POST` | `/verify/unlink`           | Unlink account                              |

### Health

| Method | Path      | Purpose      |
| ------ | --------- | ------------ |
| `GET`  | `/health` | Health check |

## Refresh Timing & Quota Governor

The YouTube Data API bills quota **per Google Cloud project** (default 10,000 units/day, reset at midnight Pacific), and a subscription check costs **1 unit per user with no batch API**. That project ceiling — not CPU or DB — is the hard limit on how many users can be kept fresh. Everything below is about spending each unit where it matters and never falling over when it runs out.

**The first check is inline.** The moment a member links, their subscription is checked using the freshly-issued token and the role is granted before the page reloads — independent of how many others verify at the same time.

**A central quota governor** ([`src/services/quota.rs`](src/services/quota.rs)) is the single gate every API call passes through. It:

- Tracks units spent in the current Pacific quota-day, **persisted to the DB** so a restart resumes accounting instead of over-spending.
- Splits the budget into an **interactive reserve** (link-time checks a user is waiting on — default 20%) and a **background** pool. Background re-checks can never touch the reserve, so a verify spike (e.g. an `@everyone` ping) never starves real-time verification.
- **Paces** background spend smoothly across the whole day (`spacing = time_to_reset / remaining_budget`) instead of bursting — so there is no thundering herd, and quota-day rollover doesn't release a synchronized flood.
- Stops *itself* before YouTube has to; a real `quotaExceeded` is a hard stop until the true Pacific reset, with the one affected row requeued past it with jitter.

**Adaptive cadence.** Subscriptions are stable, so a row whose status hasn't changed earns an exponentially longer interval (up to ~16×, capped at 24h), while a row that just flipped is re-checked promptly. Active users (with an assigned role) are still checked more often than inactive ones. This concentrates scarce quota on churn — the biggest multiplier on effective capacity.

**Batched stats.** Own-channel statistics (for stat-based rules and the subscribers list) are refreshed **50 at a time** via the public `channels.list?id=` API-key path (1 unit per 50 users), once a user's channel id is known. Set `YOUTUBE_API_KEY` to enable this; without it, stats fall back to 1 unit per user.

**Graceful degradation.** When the budget is spent, roles keep being served from cache; `is_subscribed` is only ever written from a definite API answer, so a role is **never** stripped because a check couldn't run.

**Scaling.** Rows are claimed with `FOR UPDATE SKIP LOCKED` and partitioned by `hashtext(discord_id) % N`, so `REFRESH_WORKERS` can be raised (and, with care, multiple instances run) without double-processing. To genuinely serve millions, raise the project quota (`YOUTUBE_QUOTA_PER_DAY`) via a Google quota increase and/or add API keys from additional projects — the governor treats the quota as a configurable, multipliable budget.

See [`.env.example`](.env.example) for `QUOTA_INTERACTIVE_RESERVE`, `QUOTA_SAFETY_FRACTION`, and `REFRESH_WORKERS`.

## License

[MIT](LICENSE)
