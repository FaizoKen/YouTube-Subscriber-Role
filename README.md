# YouTube Subscriber Role

A [RoleLogic](https://rolelogic.faizo.net) plugin server that automatically assigns Discord roles to members who are subscribed to a specific YouTube channel.

## How It Works

1. **Admin** creates a role link in the RoleLogic dashboard and configures a YouTube Channel ID
2. **Members** visit the verification page, sign in with Discord, then link their YouTube account via Google OAuth
3. **Plugin** periodically checks each member's subscription status using the YouTube Data API
4. **Subscribed members** automatically receive the configured Discord role; unsubscribed members have it removed

## Tech Stack

- **Rust** + **Axum 0.8** — lightweight, low-memory HTTP server
- **PostgreSQL 16** — persistent storage
- **SQLx** — async, compile-time checked SQL
- **Tokio** — async runtime with background workers

Targets a single **$4-6/month VPS** (128MB app + 256MB database).

## Setup

### Prerequisites

- [Rust](https://rustup.rs/) 1.88+
- PostgreSQL 16
- Docker & Docker Compose (for deployment)
- [Discord Application](https://discord.com/developers/applications) with OAuth2 (scopes: `identify`, `guilds`)
- [Google Cloud Project](https://console.cloud.google.com/) with YouTube Data API v3 enabled and OAuth2 credentials

### Environment Variables

Copy `.env.example` to `.env` and fill in:

```env
DATABASE_URL=postgres://ysr:password@db:5432/youtube_sub_role
DISCORD_CLIENT_ID=your_discord_client_id
DISCORD_CLIENT_SECRET=your_discord_client_secret
GOOGLE_CLIENT_ID=your_google_client_id
GOOGLE_CLIENT_SECRET=your_google_client_secret
SESSION_SECRET=random_secret_string
BASE_URL=https://your-domain.com
LISTEN_ADDR=0.0.0.0:8080
YOUTUBE_QUOTA_PER_DAY=10000
```

### Google OAuth Setup

1. Go to [Google Cloud Console](https://console.cloud.google.com/)
2. Enable **YouTube Data API v3**
3. Create **OAuth 2.0 Client ID** (Web application)
4. Add authorized redirect URI: `https://your-domain.com/verify/youtube/callback`
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

### RoleLogic Plugin (called by RoleLogic dashboard)

| Method | Path | Purpose |
|--------|------|---------|
| `POST` | `/register` | Acknowledge role link creation |
| `GET` | `/config` | Return config form schema |
| `POST` | `/config` | Save YouTube Channel ID |
| `DELETE` | `/config` | Clean up on role link deletion |

### User Verification

| Method | Path | Purpose |
|--------|------|---------|
| `GET` | `/verify` | Verification page (HTML) |
| `GET` | `/verify/login` | Discord OAuth redirect |
| `GET` | `/verify/callback` | Discord OAuth callback |
| `GET` | `/verify/youtube` | Google OAuth redirect |
| `GET` | `/verify/youtube/callback` | Google OAuth callback |
| `GET` | `/verify/status` | Current link status (JSON) |
| `POST` | `/verify/unlink` | Unlink account |

### Health

| Method | Path | Purpose |
|--------|------|---------|
| `GET` | `/health` | Health check |

## Refresh Timing

The plugin checks subscription status periodically, scaled by user count and YouTube API quota (default 10,000 units/day):

| Users | Active Check Interval | Inactive Check Interval |
|-------|----------------------|------------------------|
| 1-5 | 30 min | 3 hours |
| 100 | ~14 min | ~1.4 hours |
| 1,000 | ~2.4 hours | ~14.4 hours |

Active users (those with an assigned role) are checked **6x more frequently**.

## License

[MIT](LICENSE)
