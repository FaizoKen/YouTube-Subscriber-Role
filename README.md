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

## Refresh Timing

The **first** check happens inline the moment a member links their account, so they receive the role within seconds regardless of how many others are verifying at the same time. After that, the plugin re-checks subscription status periodically, scaled by user count and YouTube API quota (default 10,000 units/day):

| Users | Active Check Interval | Inactive Check Interval |
| ----- | --------------------- | ----------------------- |
| 1-5   | 30 min                | 3 hours                 |
| 100   | ~14 min               | ~1.4 hours              |
| 1,000 | ~2.4 hours            | ~14.4 hours             |

Active users (those with an assigned role) are checked **6x more frequently**.

## License

[MIT](LICENSE)
