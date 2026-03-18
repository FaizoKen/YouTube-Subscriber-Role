# RoleLogic Plugin Server Blueprint

This repository is the **reference implementation** of a RoleLogic plugin server — a Genshin Impact integration that assigns Discord roles based on player stats. Use this as the definitive template when building new plugin servers.

**Official API Reference**: https://docs-rolelogic.faizo.net/reference/role-link-api
Always fetch this URL for the latest API details before implementing.

**Tech Stack**: Rust + Axum 0.8 + PostgreSQL 16 + SQLx + Tokio

---

## Quick Start — How to Use This Blueprint

This blueprint is a **foundation, not a rulebook**. Use the architecture patterns as your starting point, but think deeply about what's best for each specific plugin. Adapt, simplify, or extend patterns when it makes sense. The best implementation is one that fits the problem — not one that follows a template blindly.

### When you receive a plugin idea:

**1. Research first.** Before designing anything:
- Study the external API/service — read their docs, understand their data model, rate limits, auth flow, and quirks
- Think about the end-user experience — how will Discord admins configure this? What makes sense for them?
- Consider the data shape — what's the most natural way to express conditions for this type of data?
- Fetch the official RoleLogic API reference (https://docs-rolelogic.faizo.net/reference/role-link-api) for the latest contract details

**2. Think independently.** For each design decision, reason about what's best for THIS plugin:
- Does polling make sense, or should you use webhooks/push if the API supports them?
- Does the user need a verification flow, or is the data already mapped to Discord IDs?
- Are the reference repo's condition types (field + operator + value) the right fit, or does this plugin need a completely different config UX?
- Should you cache aggressively or fetch on-demand?
- What error scenarios are unique to this API?

**3. Use this blueprint as a reference, not a constraint.** The patterns in this document are battle-tested and should be your default — but deviate when you have a good reason. For example:
- If the external API pushes data via webhooks, you don't need a refresh worker — use a webhook endpoint instead
- If the data source is a simple CSV/sheet with Discord IDs already mapped, you don't need a verification flow or a complex condition evaluator
- If the API returns all users in one call, you don't need per-user caching — cache the whole dataset
- If conditions are always "exists in list", you don't need field/operator/value — simplify the schema

**4. Present your design.** Before coding, briefly share your approach with the user — what you're building, how it works, and any non-obvious decisions you made. Keep it short.

**5. Build it.** Use [Section 14](#14-step-by-step-creating-a-new-plugin) as a checklist, but adapt the order and scope to fit the plugin. The core contract (4 RoleLogic endpoints + User Management API) is non-negotiable — everything else is flexible.

### Design for low hosting cost

Every plugin built from this blueprint should be deployable on the cheapest possible infrastructure. This is a core design principle, not an afterthought.

**Target**: A single $4-6/month VPS (1 vCPU, 512MB-1GB RAM) should comfortably run the plugin + PostgreSQL.

**Why Rust**: No garbage collector, no runtime overhead, no JVM warmup. A compiled Rust binary uses 5-15MB of RAM at idle. This is why the stack is Rust — not because it's trendy, but because it's the cheapest to host.

**How the reference implementation achieves this**:
- App container: 128MB memory limit (rarely uses more than 30-50MB)
- PostgreSQL: 256MB memory limit with tuned settings for small instances
- No Redis, no message queue, no external services — just the app + database
- Single binary, multi-stage Docker build, stripped + LTO for minimal image size
- Connection pool limited to 8 connections (enough for single-instance)
- Pre-rendered HTML cached as `Bytes` in memory (no template engine overhead)
- Rate-limited API calls prevent bursts that would spike CPU/memory

**Rules for keeping costs low**:
- Don't add Redis, Kafka, RabbitMQ, or other infrastructure unless you're at 100K+ users. PostgreSQL handles everything until then.
- Don't use an ORM — raw SQL via SQLx is faster, uses less memory, and gives you control
- Don't spawn unbounded tasks — use channels with fixed buffer sizes
- Don't load large datasets into memory — use SQL-side filtering and streaming
- Don't use background job frameworks — `tokio::spawn` with mpsc channels is sufficient
- If the plugin doesn't need a verification flow, don't build one — fewer routes = less code = less memory
- Only add the scaling patterns from [Section 16](#16-scaling-to-millions-of-users) when you actually need them

### What's fixed vs flexible

**Non-negotiable** (RoleLogic contract — must follow exactly):
- The 4 endpoints: POST /register, GET /config, POST /config, DELETE /config
- Auth scheme: `Authorization: Token rl_...`
- Config schema format (version, name, sections, fields, values)
- User Management API for role assignment (POST/DELETE/PUT users)

**Strong defaults** (use unless you have a specific reason not to):
- Rust + Axum + PostgreSQL + SQLx stack
- AppState with Arc, mpsc channels for async events
- RoleLogicClient copied from reference (it's universal)
- AppError enum with IntoResponse
- Background workers for async processing
- JSONB cache + denormalized columns for filtering

**Flexible** (adapt to fit the plugin):
- Verification flow — skip it, simplify it, or use external OAuth
- Condition types — design what makes sense for the data
- Config schema UX — create the best admin experience for this specific integration
- Refresh strategy — polling, webhooks, on-demand, or hybrid
- Caching strategy — per-user, per-guild, whole-dataset, or none
- Number of workers and their behavior
- Database schema beyond the core tables

---

## Table of Contents

0. [Quick Start — How to Use This Blueprint](#quick-start--how-to-use-this-blueprint)
1. [RoleLogic Plugin API Contract](#1-rolelogic-plugin-api-contract)
2. [Architecture Overview](#2-architecture-overview)
3. [Project Structure](#3-project-structure)
4. [Plugin Lifecycle](#4-plugin-lifecycle)
5. [Data Flow](#5-data-flow)
6. [Core Components](#6-core-components)
7. [Database Schema Template](#7-database-schema-template)
8. [Background Workers](#8-background-workers)
9. [Error Handling & Retry Strategy](#9-error-handling--retry-strategy)
10. [Rate Limiting & External API Handling](#10-rate-limiting--external-api-handling)
11. [Logging & Observability](#11-logging--observability)
12. [Configuration Structure](#12-configuration-structure)
13. [Deployment](#13-deployment)
14. [Step-by-Step: Creating a New Plugin](#14-step-by-step-creating-a-new-plugin)
15. [Example Skeleton Plugin](#15-example-skeleton-plugin)
16. [Scaling to Millions of Users](#16-scaling-to-millions-of-users)
17. [Conventions & Rules](#17-conventions--rules)

---

## 1. RoleLogic Plugin API Contract

A plugin server must implement **4 HTTP endpoints** that RoleLogic calls, and use the **User Management API** to assign/remove Discord roles.

### 1.1 Endpoints Your Server Must Implement

#### POST /register
- **When**: Admin creates a role link in the RoleLogic dashboard
- **Request body**: `{"guild_id": "...", "role_id": "..."}`
- **Auth header**: `Authorization: Token rl_...` — this is the API token, **store it**
- **Response**: `200 {"success": true}` — non-2xx blocks role link creation
- **Timeout**: 5 seconds
- **Action**: Upsert into `role_links` table, storing guild_id, role_id, and api_token

#### GET /config
- **When**: Dashboard loads the plugin config form
- **Auth header**: `Authorization: Token rl_...`
- **Response**: JSON config schema (see 1.3 below) — max 50KB, 5s timeout
- **Caching**: Schema cached 5 minutes by RoleLogic; `values` always fresh
- **Action**: Look up role link by token, build schema with current saved values

#### POST /config
- **When**: Admin saves configuration in the dashboard
- **Request body**: `{"guild_id": "...", "role_id": "...", "config": {...}}`
- **Auth header**: `Authorization: Token rl_...`
- **Response**: `200 {"success": true}` — errors should return 4xx with `{"error": "message"}`
- **Timeout**: 10 seconds
- **Action**: Parse and validate config, persist conditions, trigger re-sync for this role link

#### DELETE /config
- **When**: Admin deletes the role link
- **Request body**: `{"guild_id": "...", "role_id": "..."}`
- **Auth header**: `Authorization: Token rl_...`
- **Response**: Fire-and-forget (failures don't block deletion)
- **Timeout**: 5 seconds
- **Action**: Delete role link and cascade to role_assignments. Token becomes invalid after this.

### 1.2 User Management API (Your Server Calls This)

**Base URL**: `https://api-rolelogic.faizo.net`
**Auth**: `Authorization: Token rl_...` (same token from /register)

| Method | Path | Purpose |
|--------|------|---------|
| `GET` | `/api/role-link/:guildId/:roleId/users` | List users + get count/limit |
| `PUT` | `/api/role-link/:guildId/:roleId/users` | Replace entire user list (atomic) |
| `POST` | `/api/role-link/:guildId/:roleId/users/:userId` | Add single user (idempotent) |
| `DELETE` | `/api/role-link/:guildId/:roleId/users/:userId` | Remove single user (idempotent) |

**Response formats**:
- GET users: `{"data": {"user_count": N, "user_limit": N}}`
- PUT users: body is JSON array of user ID strings → `{"data": {"user_count": N}}`
- POST user: `{"data": {"added": true/false}}`
- DELETE user: `{"data": {"removed": true/false}}`

**Limits**: 100 users/role-link (free), 1,000,000 (premium). 10 role-links per server.

**Strategy**:
- Use `PUT` (replace) for bulk syncs after config changes (atomic, respects limits)
- Use `POST`/`DELETE` for real-time individual updates after data refreshes

### 1.3 Config Schema Format

The `GET /config` response must follow this structure:

```json
{
  "version": 1,
  "name": "Plugin Name",
  "description": "One-line description",
  "sections": [
    {
      "title": "Section Title",
      "description": "Optional",
      "collapsible": false,
      "default_collapsed": false,
      "fields": [
        {
          "type": "number",
          "key": "min_score",
          "label": "Minimum Score",
          "description": "Help text",
          "validation": { "required": true, "min": 0, "max": 100 },
          "condition": { "field": "other_key", "equals": "some_value" }
        }
      ]
    }
  ],
  "values": {
    "min_score": 50
  }
}
```

**Field types**: `text`, `textarea`, `number`, `select`, `radio`, `multi_select`, `checkbox`, `toggle`, `secret`, `url`, `color`, `slider`, `display` (read-only)

**Constraints**: Max 10 sections, 30 fields/section. Field keys: alphanumeric + underscores, max 100 chars.

**Conditional visibility**: Use `condition` or `conditions[]` on fields. Range pairs use `pair_with`.

### 1.4 Authentication

- Scheme: `Authorization: Token rl_...` (NOT Bearer)
- Tokens have `rl_` prefix
- Each token scoped to one role link (guild + role pair)
- Plugin must verify token on all incoming requests
- Your server URL must be HTTPS — no localhost, no private IPs, max 500 chars

---

## 2. Architecture Overview

```
                    RoleLogic Dashboard
                          │
                   POST/GET/DELETE
                          │
                          ▼
              ┌─────────────────────┐
              │   Route Handlers    │  ← /register, /config (GET/POST/DELETE)
              │  (routes/plugin.rs) │
              └────────┬────────────┘
                       │
              ┌────────▼────────────┐
              │   Config Parser &   │  ← Validate admin input, build schema
              │   Schema Builder    │
              └────────┬────────────┘
                       │
              ┌────────▼────────────┐
              │   Sync Engine       │  ← Evaluate conditions, compute deltas
              │  (services/sync.rs) │
              └────┬──────────┬─────┘
                   │          │
          ┌────────▼──┐  ┌───▼──────────┐
          │ RoleLogic  │  │ External API │  ← Your data source (Enka, Spotify, etc.)
          │ Client     │  │ Client       │
          │ (add/del)  │  │ (fetch data) │
          └────────────┘  └──────────────┘

    Background Workers (tokio::spawn):
    ┌──────────────┐  ┌──────────────┐  ┌──────────────┐
    │ Refresh      │  │ Player Sync  │  │ Config Sync  │
    │ Worker       │  │ Worker       │  │ Worker       │
    │ (periodic    │  │ (event-driven│  │ (debounced   │
    │  data fetch) │  │  per-player) │  │  per-role)   │
    └──────────────┘  └──────────────┘  └──────────────┘
```

---

## 3. Project Structure

Create this structure for a new plugin. Reference files in this repo for implementation details.

```
your-plugin/
├── src/
│   ├── main.rs                 # AppState, server setup, route registration, worker spawning
│   ├── config.rs               # AppConfig loaded from env vars
│   ├── db.rs                   # PgPool creation + migration execution
│   ├── error.rs                # AppError enum + IntoResponse
│   ├── schema.rs               # Config schema builder for GET /config
│   ├── models/
│   │   ├── mod.rs
│   │   └── condition.rs        # Your condition field/operator types
│   ├── routes/
│   │   ├── mod.rs
│   │   ├── plugin.rs           # /register, GET/POST/DELETE /config handlers
│   │   ├── verification.rs     # User-facing verification/linking flow
│   │   └── health.rs           # GET /health endpoint
│   ├── services/
│   │   ├── mod.rs
│   │   ├── rolelogic.rs        # RoleLogic User Management API client (copy from reference)
│   │   ├── your_api.rs         # Your external API client (rate-limited)
│   │   ├── condition_eval.rs   # Evaluate conditions against cached user data
│   │   ├── sync.rs             # Sync engine: per-player + per-role-link
│   │   └── discord_oauth.rs    # Discord OAuth (if using verification flow)
│   └── tasks/
│       ├── mod.rs              # Cleanup task
│       ├── refresh_worker.rs   # Periodic data refresh from external API
│       ├── player_sync_worker.rs   # Event-driven per-player sync
│       └── config_sync_worker.rs   # Debounced per-role-link sync
├── migrations/
│   ├── 001_initial_schema.sql  # Core tables (adapt from reference)
│   └── ...                     # Plugin-specific migrations
├── Cargo.toml
├── Dockerfile
├── compose.yml
└── .env.example
```

---

## 4. Plugin Lifecycle

### Registration Phase
```
Admin creates role link in dashboard
  → RoleLogic calls POST /register with {guild_id, role_id}
  → Your server stores the API token from the Authorization header
  → Token is used for all future API calls
```

### Configuration Phase
```
Admin opens config in dashboard
  → RoleLogic calls GET /config
  → Your server returns JSON schema with sections, fields, and current values
  → Admin fills in fields, clicks save
  → RoleLogic calls POST /config with {guild_id, role_id, config: {...}}
  → Your server parses and validates config, stores conditions
  → Triggers ConfigSyncEvent → config_sync_worker re-evaluates all users for this role
```

### Operation Phase (continuous)
```
Refresh worker periodically fetches fresh data from your external API
  → Stores in cache table
  → Sends PlayerSyncEvent
  → Player sync worker evaluates conditions for this user across all their role links
  → Adds/removes users via RoleLogic API
```

### Deletion Phase
```
Admin deletes role link
  → RoleLogic calls DELETE /config with {guild_id, role_id}
  → Your server deletes role_link row (cascades to role_assignments)
  → Token becomes invalid
```

---

## 5. Data Flow

```
External API  →  Cache Table  →  evaluate()  →  Sync Engine  →  RoleLogic API
                 (JSONB blob)   (in-memory,     (PUT/POST/      (Discord role
                                 no I/O,         DELETE)          add/remove)
                                 fast)
```

1. **User links account** → `linked_accounts` maps `discord_id` ↔ `external_id`
2. **Refresh worker** → picks stale cache entry, calls external API, stores fresh data, sends PlayerSyncEvent
3. **Player sync worker** → receives event, loads cache, evaluates conditions per role-link, computes add/remove deltas, calls RoleLogic API individually (POST/DELETE)
4. **Config sync worker** → receives config change event (debounced 5s), evaluates ALL users for that role-link using SQL-side filtering, calls RoleLogic API atomically (PUT replace)

---

## 6. Core Components

### 6.1 AppState

Shared state passed to all route handlers and workers via `Arc<AppState>`.

```rust
pub struct AppState {
    pub pool: PgPool,
    pub config: AppConfig,
    pub player_sync_tx: mpsc::Sender<PlayerSyncEvent>,  // channel to player sync worker
    pub config_sync_tx: mpsc::Sender<ConfigSyncEvent>,   // channel to config sync worker
    pub your_api_client: YourApiClient,                   // your external API client
    pub rl_client: RoleLogicClient,                       // RoleLogic API client
    pub http_client: reqwest::Client,                     // shared HTTP client (OAuth, etc.)
}
```

Reference: `src/main.rs:23-33`

### 6.1b Server Wiring (main.rs)

Complete example of how AppState, routes, workers, and middleware are assembled:

```rust
use std::sync::Arc;
use axum::routing::{delete, get, post};
use axum::Router;
use tokio::sync::mpsc;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "your_plugin=info,tower_http=info".into()),
        )
        .init();

    let app_config = config::AppConfig::from_env();
    let listen_addr = app_config.listen_addr.clone();

    let pool = db::create_pool(&app_config.database_url).await;
    db::run_migrations(&pool).await;
    tracing::info!("Database connected and migrations applied");

    // Event channels for async sync workers
    let (player_sync_tx, player_sync_rx) = mpsc::channel::<PlayerSyncEvent>(512);
    let (config_sync_tx, config_sync_rx) = mpsc::channel::<ConfigSyncEvent>(64);

    // Initialize your external API client
    let your_api_client = YourApiClient::new(&app_config);
    let rl_client = RoleLogicClient::new();
    let http_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .expect("Failed to build HTTP client");

    let state = Arc::new(AppState {
        pool,
        config: app_config,
        player_sync_tx,
        config_sync_tx,
        your_api_client,
        rl_client,
        http_client,
    });

    // Spawn background workers
    tokio::spawn(tasks::refresh_worker::run(Arc::clone(&state)));
    tokio::spawn(tasks::player_sync_worker::run(player_sync_rx, Arc::clone(&state)));
    tokio::spawn(tasks::config_sync_worker::run(config_sync_rx, Arc::clone(&state)));
    tokio::spawn(tasks::cleanup_expired(Arc::clone(&state)));

    // Route registration
    let app = Router::new()
        // Plugin endpoints (called by RoleLogic) — REQUIRED
        .route("/register", post(routes::plugin::register))
        .route("/config", get(routes::plugin::get_config))
        .route("/config", post(routes::plugin::post_config))
        .route("/config", delete(routes::plugin::delete_config))
        // Verification endpoints (if your plugin needs user linking)
        // .route("/verify", get(routes::verification::verify_page))
        // .route("/verify/login", get(routes::verification::login))
        // .route("/verify/callback", get(routes::verification::callback))
        // Health
        .route("/health", get(routes::health::health))
        // Middleware — CORS is REQUIRED (RoleLogic dashboard calls cross-origin)
        .layer(TraceLayer::new_for_http())
        .layer(CorsLayer::permissive())
        .with_state(state);

    tracing::info!("Server starting on {listen_addr}");

    let listener = tokio::net::TcpListener::bind(&listen_addr)
        .await
        .expect("Failed to bind listener");

    // Graceful shutdown on SIGTERM/Ctrl+C
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            tokio::signal::ctrl_c().await.ok();
            tracing::info!("Shutdown signal received, draining connections...");
        })
        .await
        .expect("Server error");
}
```

**Critical**: `CorsLayer::permissive()` is required. The RoleLogic dashboard makes cross-origin requests to your plugin server. Without CORS middleware, GET/POST /config will fail silently in the browser.

Reference: `src/main.rs`

### 6.2 RoleLogicClient

Copy `src/services/rolelogic.rs` from this repo. It implements:
- `get_user_info(guild_id, role_id, token)` → `(user_count, user_limit)`
- `add_user(guild_id, role_id, user_id, token)` → `bool`
- `remove_user(guild_id, role_id, user_id, token)` → `bool`
- `replace_users(guild_id, role_id, user_ids, token)` → `usize`

All methods use `Authorization: Token {token}` header. Base URL: `https://api-rolelogic.faizo.net`.

Reference: `src/services/rolelogic.rs`

### 6.3 Route Handlers

The 4 plugin endpoints follow this pattern:

```rust
fn extract_token(headers: &HeaderMap) -> Result<String, AppError> {
    let auth = headers.get("authorization")
        .and_then(|v| v.to_str().ok())
        .ok_or(AppError::Unauthorized)?;
    let token = auth.strip_prefix("Token ").ok_or(AppError::Unauthorized)?;
    Ok(token.to_string())
}

pub async fn register(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<RegisterBody>,
) -> Result<Json<Value>, AppError> {
    let token = extract_token(&headers)?;
    sqlx::query(
        "INSERT INTO role_links (guild_id, role_id, api_token) VALUES ($1, $2, $3) \
         ON CONFLICT (guild_id, role_id) DO UPDATE SET api_token = $3, updated_at = now()"
    )
    .bind(&body.guild_id).bind(&body.role_id).bind(&token)
    .execute(&state.pool).await?;
    Ok(Json(json!({"success": true})))
}

pub async fn get_config(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    let token = extract_token(&headers)?;
    let (guild_id, conditions) = sqlx::query_as::<_, (String, Value)>(
        "SELECT guild_id, conditions FROM role_links WHERE api_token = $1"
    )
    .bind(&token).fetch_optional(&state.pool).await?
    .ok_or(AppError::Unauthorized)?;
    let schema = build_config_schema(&conditions, &state.config.base_url, &guild_id);
    Ok(Json(schema))
}

pub async fn post_config(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<ConfigBody>,
) -> Result<Json<Value>, AppError> {
    let token = extract_token(&headers)?;
    // Verify token matches this role link
    let exists = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM role_links WHERE guild_id=$1 AND role_id=$2 AND api_token=$3)"
    ).bind(&body.guild_id).bind(&body.role_id).bind(&token)
    .fetch_one(&state.pool).await.unwrap_or(false);
    if !exists { return Err(AppError::Unauthorized); }

    let conditions = parse_config(&body.config)?;  // your validation logic
    sqlx::query("UPDATE role_links SET conditions=$1, updated_at=now() WHERE guild_id=$2 AND role_id=$3")
        .bind(sqlx::types::Json(&conditions))
        .bind(&body.guild_id).bind(&body.role_id)
        .execute(&state.pool).await?;

    // Trigger re-evaluation
    let _ = state.config_sync_tx.send(ConfigSyncEvent {
        guild_id: body.guild_id, role_id: body.role_id
    }).await;
    Ok(Json(json!({"success": true})))
}

pub async fn delete_config(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<DeleteConfigBody>,
) -> Result<Json<Value>, AppError> {
    let token = extract_token(&headers)?;
    let result = sqlx::query(
        "DELETE FROM role_links WHERE guild_id=$1 AND role_id=$2 AND api_token=$3"
    ).bind(&body.guild_id).bind(&body.role_id).bind(&token)
    .execute(&state.pool).await?;
    if result.rows_affected() == 0 { return Err(AppError::Unauthorized); }
    Ok(Json(json!({"success": true})))
}
```

Reference: `src/routes/plugin.rs`

### 6.4 Sync Engine

Two sync paths:

**Per-player sync** (lightweight, after data refresh or account link/unlink):
1. Load user's cached data from cache table
2. Load all role links for guilds the user is a member of
3. Load existing role_assignments for this user
4. Evaluate conditions locally (no I/O) for each role link
5. Compute deltas: (qualifies AND not assigned → Add), (not qualifies AND assigned → Remove)
6. Execute up to 10 concurrent POST/DELETE calls to RoleLogic API
7. Update role_assignments table

**Per-role-link sync** (heavy, after config change, debounced):
1. Load role link conditions and API token
2. Query RoleLogic API for user limit
3. Build SQL WHERE clause from conditions (push filtering to PostgreSQL)
4. Query all qualifying discord_ids with LIMIT = user_limit
5. Atomic PUT replace_users to RoleLogic API
6. Update role_assignments in a transaction

Reference: `src/services/sync.rs`

### 6.5 Condition Evaluation

Your `evaluate()` function must be:
- **Synchronous** (no async, no I/O)
- **Fast** (microseconds — called thousands of times per sync cycle)
- **Pure** (depends only on cached data and conditions)

Pattern:
```rust
pub fn evaluate_conditions(
    conditions: &[YourCondition],
    user_data: &serde_json::Value,
    // ... plugin-specific context (region, fetched_at, etc.)
) -> bool {
    conditions.iter().all(|c| evaluate_single(c, user_data))
}
```

For bulk sync, also implement a SQL WHERE clause builder that pushes filtering to PostgreSQL instead of loading all JSONB into memory:

```rust
fn build_condition_where(conditions: &[YourCondition]) -> (String, Vec<ConditionBind>) {
    // Build parameterized SQL WHERE clause
    // Use extracted columns for efficient filtering
    // Return ("column >= $1 AND column <= $2", vec![bind_values])
}
```

Reference: `src/services/condition_eval.rs` and `src/services/sync.rs:166-275`

### 6.6 External API Client

Your client for the plugin's data source. Follow this pattern:

```rust
pub struct YourApiClient {
    http: reqwest::Client,
    rate_limiter: Arc<RateLimiter<...>>,
}

impl YourApiClient {
    pub fn new(config: &AppConfig) -> Self {
        let http = reqwest::Client::builder()
            .user_agent("YourPlugin/1.0")
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .expect("Failed to build HTTP client");

        let quota = Quota::per_second(NonZeroU32::new(2).unwrap());
        let rate_limiter = Arc::new(RateLimiter::direct(quota));

        Self { http, rate_limiter }
    }

    pub async fn wait_for_permit(&self) {
        self.rate_limiter.until_ready().await;
    }

    pub async fn fetch_user_data(&self, external_id: &str) -> Result<YourResponse, YourError> {
        // Rate limit, fetch, parse, handle errors
    }
}
```

Reference: `src/services/enka.rs`

---

## 7. Database Schema Template

### 7.1 Core Tables (required for every plugin)

```sql
-- Role links: one per guild+role pair registered via POST /register
CREATE TABLE IF NOT EXISTS role_links (
    id              BIGSERIAL PRIMARY KEY,
    guild_id        TEXT NOT NULL,
    role_id         TEXT NOT NULL,
    api_token       TEXT NOT NULL,
    conditions      JSONB NOT NULL DEFAULT '[]',    -- plugin-specific condition config
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (guild_id, role_id)
);

-- Linked accounts: maps Discord user to external service identity
CREATE TABLE IF NOT EXISTS linked_accounts (
    id              BIGSERIAL PRIMARY KEY,
    discord_id      TEXT NOT NULL UNIQUE,
    external_id     TEXT NOT NULL UNIQUE,            -- UID, Spotify ID, GitHub username, etc.
    linked_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Role assignments: tracks which users currently have which roles (local mirror)
CREATE TABLE IF NOT EXISTS role_assignments (
    guild_id        TEXT NOT NULL,
    role_id         TEXT NOT NULL,
    discord_id      TEXT NOT NULL,
    assigned_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (guild_id, role_id, discord_id),
    FOREIGN KEY (guild_id, role_id) REFERENCES role_links (guild_id, role_id) ON DELETE CASCADE
);

-- OAuth states: CSRF protection for Discord OAuth flow
CREATE TABLE IF NOT EXISTS oauth_states (
    state           TEXT PRIMARY KEY,
    redirect_data   JSONB,
    expires_at      TIMESTAMPTZ NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Verification sessions: temporary codes for account linking
CREATE TABLE IF NOT EXISTS verification_sessions (
    id              BIGSERIAL PRIMARY KEY,
    discord_id      TEXT NOT NULL,
    external_id     TEXT NOT NULL,
    code            TEXT NOT NULL,
    expires_at      TIMESTAMPTZ NOT NULL,
    attempts        INTEGER NOT NULL DEFAULT 0,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_verification_discord ON verification_sessions (discord_id);
```

### 7.2 Plugin-Specific Cache Table

```sql
-- Cache for data fetched from your external API
CREATE TABLE IF NOT EXISTS user_cache (
    external_id     TEXT PRIMARY KEY,               -- same as linked_accounts.external_id
    data            JSONB NOT NULL,                 -- full API response blob
    region          TEXT,                           -- if applicable
    api_ttl         INTEGER NOT NULL DEFAULT 60,    -- TTL from API response
    fetched_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    next_fetch_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    fetch_failures  INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_user_cache_next_fetch ON user_cache (next_fetch_at ASC);
```

### 7.3 Denormalized Columns (for SQL-side filtering)

Extract frequently filtered fields from JSONB into dedicated columns. This lets the SQL WHERE clause filter without parsing JSONB for every row during bulk sync.

```sql
-- Example: extract numeric fields for efficient WHERE clauses
ALTER TABLE user_cache ADD COLUMN IF NOT EXISTS score INTEGER DEFAULT 0;
ALTER TABLE user_cache ADD COLUMN IF NOT EXISTS level INTEGER DEFAULT 0;
-- Update on data refresh:
-- UPDATE user_cache SET score = COALESCE((data->>'score')::int, 0) WHERE external_id = $1
```

Reference: `migrations/003_extract_fields.sql`

### 7.4 User Guilds (for guild membership filtering)

If your plugin needs to know which guilds a user is in (to scope sync):

```sql
CREATE TABLE IF NOT EXISTS user_guilds (
    discord_id      TEXT NOT NULL,
    guild_id        TEXT NOT NULL,
    guild_name      TEXT,
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (discord_id, guild_id)
);
```

Reference: `migrations/004_user_guilds.sql`

---

## 8. Background Workers

All workers are spawned via `tokio::spawn` in `main.rs` and receive `Arc<AppState>`.

### 8.1 Refresh Worker (periodic external API fetch)

Continuously picks the oldest stale cache entry and refreshes it. Key design:

- **Rate-limited**: wait for permit before each API call
- **Priority-based**: active users (with role_assignments) refreshed first
- **Dynamic interval**: scales with user count to stay within API rate limits
- **Failure backoff**: exponential backoff per user on repeated failures (60s, 120s, 240s... up to 1h)
- **TTL-driven**: respects API-provided TTL, applies minimum floor

```
Formula: interval = max(MIN_REFRESH_SECS, (player_count * 3600) / max_requests_per_hour)
Active users: refresh at interval
Inactive users: refresh at interval * INACTIVE_MULTIPLIER (e.g. 6x slower)
```

Reference: `src/tasks/refresh_worker.rs`

### 8.2 Player Sync Worker (event-driven per-player)

Receives events via `mpsc::channel`, calls `sync_for_player()`:

```rust
pub enum PlayerSyncEvent {
    PlayerUpdated { discord_id: String },   // after data refresh
    AccountLinked { discord_id: String },    // after verification
    AccountUnlinked { discord_id: String },  // after unlink
}
```

Reference: `src/tasks/player_sync_worker.rs`

### 8.3 Config Sync Worker (debounced per-role-link)

Receives events via `mpsc::channel`, **debounces by 5 seconds** (deduplicates by guild_id+role_id), then calls `sync_for_role_link()`:

```rust
pub struct ConfigSyncEvent {
    pub guild_id: String,
    pub role_id: String,
}
```

Debouncing prevents cascading syncs from rapid config updates. Uses `HashMap<(guild_id, role_id), Instant>` to track pending events.

Reference: `src/tasks/config_sync_worker.rs`

### 8.4 Cleanup Worker

Runs every 5 minutes, deletes expired `oauth_states` and `verification_sessions`.

Reference: `src/tasks/mod.rs`

---

## 9. Error Handling & Retry Strategy

### 9.1 AppError Pattern

```rust
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("External API error: {0}")]
    ExternalApi(String),

    #[error("RoleLogic API error: {0}")]
    RoleLogic(String),

    #[error("Role link user limit reached ({limit})")]
    UserLimitReached { limit: usize },

    #[error("Invalid request: {0}")]
    BadRequest(String),

    #[error("Unauthorized")]
    Unauthorized,

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Internal error: {0}")]
    Internal(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            AppError::Database(e) => {
                tracing::error!("Database error: {e}");
                (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error")
            }
            AppError::ExternalApi(e) => {
                tracing::error!("External API error: {e}");
                (StatusCode::BAD_GATEWAY, "Failed to fetch external data")
            }
            AppError::RoleLogic(e) => {
                tracing::error!("RoleLogic API error: {e}");
                (StatusCode::BAD_GATEWAY, "Failed to sync roles")
            }
            AppError::UserLimitReached { .. } => {
                (StatusCode::FORBIDDEN, "Role link user limit reached")
            }
            AppError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg.as_str()),
            AppError::Unauthorized => (StatusCode::UNAUTHORIZED, "Invalid or missing authorization"),
            AppError::NotFound(msg) => (StatusCode::NOT_FOUND, msg.as_str()),
            AppError::Internal(e) => {
                tracing::error!("Internal error: {e}");
                (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error")
            }
        };
        let body = serde_json::json!({"error": message});
        (status, axum::Json(body)).into_response()
    }
}
```

Reference: `src/error.rs`

### 9.2 External API Error Strategy

- **Rate limited (429)**: Back off 5 seconds, retry
- **Maintenance/Unavailable**: Back off 10 minutes, push all pending refreshes
- **Not found (404)**: Log, don't retry
- **Bad input (400)**: Log, don't retry
- **Server error (5xx)**: Exponential backoff per user via `fetch_failures` column
  - Formula: `next_fetch_at = now() + min(60s * 2^failures, 1 hour)`

### 9.3 RoleLogic API Error Strategy

- **User limit reached (400/403 with "limit")**: Log warning, skip this add
- **Other errors**: Log error, don't crash the sync — continue with remaining actions
- **Network failures**: Log and continue — sync will retry on next cycle

### 9.4 General Principle

Workers never crash. They log errors and continue. Individual failures don't block other users or role links.

---

## 10. Rate Limiting & External API Handling

Use the `governor` crate for token-bucket rate limiting:

```rust
use governor::{Quota, RateLimiter};
use std::num::NonZeroU32;

// Example: 2 requests per second
let quota = Quota::per_second(NonZeroU32::new(2).unwrap());
let rate_limiter = Arc::new(RateLimiter::direct(quota));

// Before each API call:
rate_limiter.until_ready().await;
```

**Dynamic interval calculation** (from refresh worker):
```
base_interval = max(1800s, (player_count * 3600) / max_requests_per_hour)
active_interval = base_interval
inactive_interval = base_interval * 6
```

This ensures the total request rate stays within the API's limits regardless of user count.

Reference: `src/services/enka.rs` and `src/tasks/refresh_worker.rs`

---

## 11. Logging & Observability

### Setup

```rust
tracing_subscriber::fmt()
    .with_env_filter(
        tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "your_plugin=info,tower_http=info".into()),
    )
    .init();
```

### Conventions

- `tracing::info!` for lifecycle events: registration, config changes, sync completions
- `tracing::debug!` for per-user operations: data refresh, condition evaluation
- `tracing::warn!` for recoverable issues: rate limits, user limit reached
- `tracing::error!` for failures: API errors, database errors
- Always include structured fields: `guild_id`, `role_id`, `discord_id`, `external_id`

### Health Endpoint

```rust
pub async fn health(State(state): State<Arc<AppState>>) -> Json<Value> {
    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM linked_accounts")
        .fetch_one(&state.pool).await.unwrap_or(0);
    Json(json!({
        "status": "healthy",
        "total_verified": total,
    }))
}
```

Reference: `src/routes/health.rs`

---

## 12. Configuration Structure

### 12.1 AppConfig Pattern

```rust
#[derive(Clone)]
pub struct AppConfig {
    // Core (every plugin needs these)
    pub database_url: String,
    pub discord_client_id: String,
    pub discord_client_secret: String,
    pub session_secret: String,
    pub base_url: String,
    pub listen_addr: String,

    // Plugin-specific
    pub your_api_key: String,       // example
    pub your_api_rate_limit: i64,   // example
}

impl AppConfig {
    pub fn from_env() -> Self {
        Self {
            database_url: env::var("DATABASE_URL").expect("DATABASE_URL must be set"),
            discord_client_id: env::var("DISCORD_CLIENT_ID").expect("DISCORD_CLIENT_ID must be set"),
            discord_client_secret: env::var("DISCORD_CLIENT_SECRET").expect("DISCORD_CLIENT_SECRET must be set"),
            session_secret: env::var("SESSION_SECRET").expect("SESSION_SECRET must be set"),
            base_url: env::var("BASE_URL").expect("BASE_URL must be set"),
            listen_addr: env::var("LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_string()),
            // Plugin-specific
            your_api_key: env::var("YOUR_API_KEY").expect("YOUR_API_KEY must be set"),
            your_api_rate_limit: env::var("YOUR_API_RATE_LIMIT")
                .ok().and_then(|v| v.parse().ok()).unwrap_or(360),
        }
    }
}
```

Reference: `src/config.rs`

### 12.2 Environment Variables

**Core (required for all plugins)**:
```env
DATABASE_URL=postgres://user:password@db:5432/your_plugin
DISCORD_CLIENT_ID=...
DISCORD_CLIENT_SECRET=...
SESSION_SECRET=...           # random string for HMAC session signing
BASE_URL=https://your-plugin.example.com
LISTEN_ADDR=0.0.0.0:8080    # optional, defaults to 0.0.0.0:8080
RUST_LOG=your_plugin=info    # optional, log level
```

**Plugin-specific**: Add whatever your external API requires (API keys, rate limits, etc.)

---

## 13. Deployment

### 13.1 Cargo.toml Dependencies

```toml
[package]
name = "your-plugin"
version = "0.1.0"
edition = "2021"

[dependencies]
axum = { version = "0.8", features = ["macros"] }
axum-extra = { version = "0.10", features = ["cookie", "typed-header"] }
tokio = { version = "1", features = ["rt-multi-thread", "macros", "net", "sync", "time", "signal"] }
sqlx = { version = "0.8", features = ["runtime-tokio-rustls", "postgres", "chrono", "json"] }
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }
futures-util = "0.3"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
chrono = { version = "0.4", features = ["serde"] }
rand = "0.8"
tower = "0.5"
tower-http = { version = "0.6", features = ["cors", "trace"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
dotenvy = "0.15"
governor = "0.8"
thiserror = "2"
hmac = "0.12"
sha2 = "0.10"
hex = "0.4"
time = "0.3"
urlencoding = "2"
bytes = "1"

[profile.release]
lto = true
codegen-units = 1
strip = true
panic = "abort"
```

Reference: `Cargo.toml`

### 13.2 Dockerfile

```dockerfile
FROM rust:1.88-bookworm AS builder
WORKDIR /app

# Cache dependencies in a separate layer
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs && \
    cargo build --release && \
    rm -rf src target/release/your-plugin target/release/deps/your_plugin*

# Build actual source
COPY src/ src/
COPY migrations/ migrations/
RUN cargo build --release && strip target/release/your-plugin

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && \
    rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/your-plugin /usr/local/bin/
EXPOSE 8080
CMD ["your-plugin"]
```

Reference: `Dockerfile`

### 13.3 Docker Compose

```yaml
services:
  app:
    build: .
    ports:
      - "8080:8080"
    environment:
      DATABASE_URL: postgres://app:password@db:5432/your_plugin
      DISCORD_CLIENT_ID: ${DISCORD_CLIENT_ID}
      DISCORD_CLIENT_SECRET: ${DISCORD_CLIENT_SECRET}
      SESSION_SECRET: ${SESSION_SECRET}
      BASE_URL: ${BASE_URL}
      LISTEN_ADDR: "0.0.0.0:8080"
      # Plugin-specific env vars here
    depends_on:
      db:
        condition: service_healthy
    restart: unless-stopped
    deploy:
      resources:
        limits:
          memory: 128M

  db:
    image: postgres:16-alpine
    command: >
      postgres
      -c shared_buffers=64MB
      -c work_mem=2MB
      -c maintenance_work_mem=32MB
      -c effective_cache_size=128MB
      -c max_connections=15
      -c wal_buffers=4MB
      -c checkpoint_completion_target=0.9
      -c random_page_cost=1.1
    environment:
      POSTGRES_DB: your_plugin
      POSTGRES_USER: app
      POSTGRES_PASSWORD: password
    volumes:
      - pgdata:/var/lib/postgresql/data
    shm_size: '64m'
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U app -d your_plugin"]
      interval: 5s
      timeout: 5s
      retries: 5
    deploy:
      resources:
        limits:
          memory: 256M

volumes:
  pgdata:
```

Reference: `compose.yml`

---

## 14. Step-by-Step: Creating a New Plugin

### Step 1: Initialize project
```bash
cargo init your-plugin
cd your-plugin
```

### Step 2: Copy core infrastructure from this reference repo
Copy these files and adapt names/paths:
- `Cargo.toml` — update package name, keep dependencies
- `Dockerfile` — update binary name
- `compose.yml` — update service names, env vars, db name
- `.env.example` — update with your plugin's env vars
- `src/config.rs` — update struct fields for your env vars
- `src/db.rs` — copy pool setup, update migration file paths
- `src/error.rs` — copy and adapt error variants for your external API
- `src/services/rolelogic.rs` — copy as-is (universal RoleLogic client)
- `src/routes/plugin.rs` — copy core structure, update schema/parse calls
- `src/tasks/player_sync_worker.rs` — copy as-is (generic event handler)
- `src/tasks/config_sync_worker.rs` — copy as-is (generic debounced handler)
- `src/tasks/mod.rs` — copy cleanup task

### Step 3: Design your data model
- What external data do you fetch? What does the API response look like?
- What conditions can admins configure? (e.g., "score >= 100", "rank = Gold")
- What fields should be extracted into SQL columns for filtering?

### Step 4: Create migrations
- `migrations/001_initial_schema.sql` — adapt the core tables template from section 7
- `migrations/002_your_cache.sql` — create cache table + extracted columns

### Step 5: Implement your external API client
- `src/services/your_api.rs` — HTTP client with rate limiting (follow pattern from `src/services/enka.rs`)

### Step 6: Implement condition types
- `src/models/condition.rs` — define your condition fields, operators

### Step 7: Implement schema builder + config parser
- `src/schema.rs` — build the JSON config schema for GET /config
- Include `parse_config()` to validate and normalize POST /config input
- Follow the RoleLogic schema format (sections, fields, values)

### Step 8: Implement condition evaluation
- `src/services/condition_eval.rs` — sync, fast, pure evaluation function
- Also implement `build_condition_where()` for SQL-side bulk filtering

### Step 9: Implement sync engine
- `src/services/sync.rs` — adapt from reference, replacing condition evaluation calls with yours
- Include `PlayerSyncEvent` and `ConfigSyncEvent` types
- Include `sync_for_player()` and `sync_for_role_link()` functions
- Include `remove_all_assignments()` for account unlink

### Step 10: Implement refresh worker
- `src/tasks/refresh_worker.rs` — adapt from reference, calling your API client
- Update the UPDATE query to extract your denormalized columns

### Step 11: Implement verification flow (if needed)
- If your plugin uses an external OAuth (Spotify, GitHub, etc.), implement those routes
- If using in-app verification (like Genshin's in-game signature), implement that
- If no verification needed, use a simple "enter your ID" flow

### Step 12: Wire everything in main.rs
- Create AppState with your client, channels, pool
- Register routes
- Spawn workers
- Add graceful shutdown

### Step 13: Test
1. `cargo build` — verify compilation
2. `docker compose up` — spin up PostgreSQL + app
3. Test `POST /register` manually (curl)
4. Test `GET /config` — verify schema renders
5. Test `POST /config` — verify conditions saved
6. Test the full flow: link account → data fetches → conditions evaluated → roles synced

### Step 13b: Test with curl

After `docker compose up`, verify each endpoint manually before deploying:

```bash
# 1. Health check — should return 200
curl http://localhost:8080/health

# 2. Simulate POST /register (RoleLogic sends this when admin creates a role link)
curl -X POST http://localhost:8080/register \
  -H "Content-Type: application/json" \
  -H "Authorization: Token rl_test_token_123" \
  -d '{"guild_id": "111111111111111111", "role_id": "222222222222222222"}'
# Expected: {"success": true}

# 3. GET /config — should return your schema with sections and fields
curl http://localhost:8080/config \
  -H "Authorization: Token rl_test_token_123"
# Expected: {"version": 1, "name": "...", "sections": [...], "values": {...}}

# 4. POST /config — simulate admin saving settings
curl -X POST http://localhost:8080/config \
  -H "Content-Type: application/json" \
  -H "Authorization: Token rl_test_token_123" \
  -d '{"guild_id": "111111111111111111", "role_id": "222222222222222222", "config": {"your_field": "your_value"}}'
# Expected: {"success": true}

# 5. Verify conditions were saved
curl http://localhost:8080/config \
  -H "Authorization: Token rl_test_token_123"
# Expected: values should now include the config you just saved

# 6. DELETE /config — simulate admin removing the role link
curl -X DELETE http://localhost:8080/config \
  -H "Content-Type: application/json" \
  -H "Authorization: Token rl_test_token_123" \
  -d '{"guild_id": "111111111111111111", "role_id": "222222222222222222"}'
# Expected: {"success": true}

# 7. Verify deletion — GET /config should now fail
curl http://localhost:8080/config \
  -H "Authorization: Token rl_test_token_123"
# Expected: {"error": "Invalid or missing authorization"} (401)
```

**Common issues**:
- `Connection refused` → app didn't start, check `docker compose logs app`
- `500 Internal server error` → database migration failed or env var missing
- `401 Unauthorized` → token extraction broken, check `extract_token()` handles `Token rl_...` prefix
- Schema returns but dashboard shows error → response exceeds 50KB or has invalid field types
- CORS error in browser console → missing `CorsLayer::permissive()` middleware

### Step 14: Deploy
1. Set up HTTPS (required by RoleLogic — no HTTP, no private IPs)
2. Register your plugin URL in the RoleLogic dashboard
3. Create a role link to trigger `POST /register`

---

## 15. Example Skeleton Plugin

A minimal plugin that grants roles to all verified users (no conditions, no external API):

**`src/schema.rs`**:
```rust
pub fn build_config_schema(_conditions: &serde_json::Value, verify_url: &str) -> serde_json::Value {
    serde_json::json!({
        "version": 1,
        "name": "My Plugin",
        "description": "Grants this role to all verified members.",
        "sections": [
            {
                "title": "Getting Started",
                "fields": [
                    {
                        "type": "display",
                        "key": "info",
                        "label": "How it works",
                        "value": format!(
                            "Members verify at: {verify_url}\n\
                             All verified members automatically receive this role."
                        )
                    }
                ]
            }
        ],
        "values": {}
    })
}

pub fn parse_config(_config: &std::collections::HashMap<String, serde_json::Value>)
    -> Result<Vec<()>, crate::error::AppError>
{
    Ok(vec![])  // No conditions to parse
}
```

**`src/services/condition_eval.rs`**:
```rust
pub fn evaluate_conditions(
    _conditions: &[()],
    _user_data: &serde_json::Value,
) -> bool {
    true  // Everyone qualifies
}
```

**`src/services/sync.rs` (sync_for_role_link excerpt)**:
```rust
// No conditions → all verified guild members qualify
let query = "SELECT la.discord_id FROM linked_accounts la \
             JOIN user_guilds ug ON ug.discord_id = la.discord_id AND ug.guild_id = $1 \
             ORDER BY la.linked_at ASC LIMIT $2";
```

---

## 16. Scaling to Millions of Users

The base template targets small deployments (128MB container, 8 DB connections). When your plugin needs to support hundreds of thousands or millions of users, apply these patterns progressively.

### 16.1 Scale Tiers

| Users | Tier | Key Changes |
|-------|------|-------------|
| 0–10K | **Small** | Base template as-is. Single instance, 128MB container, 8 DB connections. |
| 10K–100K | **Medium** | Increase DB pool, add indexes, tune refresh intervals, chunk PUT calls. |
| 100K–1M+ | **Large** | Multiple worker instances, Redis caching, persistent queues, DB partitioning. |

### 16.2 Database Scaling

**Connection pool sizing**:
```rust
// Small (default): 8 connections
// Medium: 20-30 connections
// Large: 50+ connections (consider PgBouncer as a connection pooler)
PgPoolOptions::new().max_connections(30)
```

**Indexing** — ensure these indexes exist for large tables:
```sql
-- user_cache: refresh worker picks stale entries
CREATE INDEX idx_user_cache_next_fetch ON user_cache (next_fetch_at ASC);
CREATE INDEX idx_user_cache_failures ON user_cache (fetch_failures ASC);

-- linked_accounts: join performance
CREATE INDEX idx_linked_accounts_external ON linked_accounts (external_id);

-- role_assignments: bulk sync lookups
CREATE INDEX idx_role_assignments_discord ON role_assignments (discord_id);
CREATE INDEX idx_role_assignments_guild_role ON role_assignments (guild_id, role_id);

-- user_guilds: guild membership lookups
CREATE INDEX idx_user_guilds_guild ON user_guilds (guild_id);
```

**Table partitioning** (1M+ users) — partition `user_cache` by region or hash:
```sql
-- Range partition by external_id hash for even distribution
CREATE TABLE user_cache (
    external_id TEXT NOT NULL,
    data JSONB NOT NULL,
    ...
) PARTITION BY HASH (external_id);

CREATE TABLE user_cache_p0 PARTITION OF user_cache FOR VALUES WITH (MODULUS 4, REMAINDER 0);
CREATE TABLE user_cache_p1 PARTITION OF user_cache FOR VALUES WITH (MODULUS 4, REMAINDER 1);
CREATE TABLE user_cache_p2 PARTITION OF user_cache FOR VALUES WITH (MODULUS 4, REMAINDER 2);
CREATE TABLE user_cache_p3 PARTITION OF user_cache FOR VALUES WITH (MODULUS 4, REMAINDER 3);
```

**PostgreSQL tuning** for large datasets:
```yaml
# compose.yml — scale up from base template
command: >
  postgres
  -c shared_buffers=256MB
  -c work_mem=8MB
  -c maintenance_work_mem=128MB
  -c effective_cache_size=512MB
  -c max_connections=100
  -c wal_buffers=16MB
  -c checkpoint_completion_target=0.9
  -c random_page_cost=1.1
deploy:
  resources:
    limits:
      memory: 1G  # up from 256MB
```

### 16.3 RoleLogic API — Batch Chunking

The PUT `/users` endpoint accepts a maximum of **50,000 users per request**. For role links with more qualifying users, chunk the PUT calls:

```rust
pub async fn replace_users_chunked(
    rl_client: &RoleLogicClient,
    guild_id: &str,
    role_id: &str,
    all_user_ids: &[String],
    token: &str,
) -> Result<(), AppError> {
    const CHUNK_SIZE: usize = 50_000;

    if all_user_ids.len() <= CHUNK_SIZE {
        // Single PUT — atomic replace
        rl_client.replace_users(guild_id, role_id, all_user_ids, token).await?;
    } else {
        // Multi-chunk strategy:
        // 1. First chunk uses PUT (replaces list)
        // 2. Remaining chunks use individual POST (adds)
        // Note: this is NOT atomic — there's a window where some users are missing
        let (first_chunk, remaining) = all_user_ids.split_at(CHUNK_SIZE);
        rl_client.replace_users(guild_id, role_id, first_chunk, token).await?;

        for chunk in remaining.chunks(CHUNK_SIZE) {
            for user_id in chunk {
                if let Err(e) = rl_client.add_user(guild_id, role_id, user_id, token).await {
                    tracing::warn!(guild_id, role_id, user_id, "Failed to add user in batch: {e}");
                }
            }
        }
    }
    Ok(())
}
```

### 16.4 Sync Engine — Streaming for Large Datasets

For bulk sync with 100K+ users, don't load all qualifying IDs into memory at once. Use cursor-based pagination or streaming:

```rust
// Instead of: fetch_all() → Vec<String> (all in memory)
// Use streaming with sqlx:
use futures_util::TryStreamExt;

let mut stream = sqlx::query_scalar::<_, String>(
    "SELECT la.discord_id FROM linked_accounts la \
     JOIN user_cache uc ON uc.external_id = la.external_id \
     JOIN user_guilds ug ON ug.discord_id = la.discord_id AND ug.guild_id = $1 \
     WHERE ... ORDER BY la.linked_at ASC"
)
.bind(guild_id)
.fetch(&state.pool);  // returns a Stream, not Vec

let mut batch = Vec::with_capacity(50_000);
while let Some(discord_id) = stream.try_next().await? {
    batch.push(discord_id);
    if batch.len() >= 50_000 {
        // Flush batch to RoleLogic API
        rl_client.replace_users(guild_id, role_id, &batch, token).await?;
        batch.clear();
    }
}
// Flush remaining
if !batch.is_empty() {
    // Use POST for remaining (PUT would replace what we already sent)
    for user_id in &batch {
        rl_client.add_user(guild_id, role_id, user_id, token).await.ok();
    }
}
```

### 16.5 Refresh Worker — Parallel Fetching

At 360 requests/hour, refreshing 1M users takes ~115 days. Scale options:

**Option A: Increase API rate limit** (if your external API allows it):
```env
YOUR_API_MAX_REQUESTS_PER_HOUR=3600  # 10x more
```

**Option B: Multiple refresh workers** with partitioned ranges:
```rust
// Partition by external_id hash — each worker handles a range
let worker_id = 0; // 0, 1, 2, ...
let total_workers = 3;

let next = sqlx::query_as::<_, (String, String, bool)>(
    "SELECT uc.external_id, la.discord_id, ... \
     FROM user_cache uc \
     JOIN linked_accounts la ON la.external_id = uc.external_id \
     WHERE uc.next_fetch_at <= now() \
       AND abs(hashtext(uc.external_id)) % $1 = $2 \
     ORDER BY is_active DESC, uc.fetch_failures ASC, uc.next_fetch_at ASC \
     LIMIT 1"
)
.bind(total_workers)
.bind(worker_id)
.fetch_optional(&state.pool).await;
```

**Option C: Tiered refresh strategy** — don't refresh everyone equally:
```
Active users (have role_assignments):   refresh every 30 min
Recently active (assignment in last 7d): refresh every 2 hours
Inactive (no recent assignments):        refresh every 24 hours
Stale (no login in 30d):                 refresh every 7 days or skip
```

```rust
let ttl = match (is_active, days_since_last_assignment) {
    (true, _) => base_interval,                    // 30 min
    (false, Some(d)) if d <= 7 => base_interval * 4,   // 2 hours
    (false, Some(d)) if d <= 30 => base_interval * 48,  // 24 hours
    _ => base_interval * 336,                       // 7 days
};
```

### 16.6 Caching Layer (Redis)

For 100K+ users, add Redis to cache hot data and reduce PostgreSQL load:

**Dependencies** (add to Cargo.toml):
```toml
redis = { version = "0.27", features = ["tokio-comp", "connection-manager"] }
```

**Use cases**:
```rust
// 1. Cache user_cache lookups (avoid DB hit on every sync)
let cache_key = format!("user_data:{external_id}");
if let Some(cached) = redis.get::<_, Option<String>>(&cache_key).await? {
    return Ok(serde_json::from_str(&cached)?);
}
let data = sqlx::query_scalar("SELECT data FROM user_cache WHERE external_id = $1")
    .bind(external_id).fetch_one(&pool).await?;
redis.set_ex(&cache_key, &data.to_string(), 300).await?; // 5 min TTL

// 2. Cache role_link conditions (avoid DB hit per evaluation)
let cond_key = format!("conditions:{guild_id}:{role_id}");

// 3. Cache guild membership (user_guilds lookups)
let guilds_key = format!("user_guilds:{discord_id}");
```

**Docker Compose addition**:
```yaml
  redis:
    image: redis:7-alpine
    command: redis-server --maxmemory 64mb --maxmemory-policy allkeys-lru
    deploy:
      resources:
        limits:
          memory: 96M
```

### 16.7 Persistent Queue (replacing mpsc channels)

At scale, in-memory mpsc channels risk losing events on crash. Replace with a persistent queue:

**Option A: PostgreSQL-based queue** (simplest, no new infrastructure):
```sql
CREATE TABLE sync_queue (
    id          BIGSERIAL PRIMARY KEY,
    event_type  TEXT NOT NULL,       -- 'player_sync', 'config_sync'
    payload     JSONB NOT NULL,      -- {discord_id} or {guild_id, role_id}
    status      TEXT NOT NULL DEFAULT 'pending',  -- pending, processing, done, failed
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    started_at  TIMESTAMPTZ,
    completed_at TIMESTAMPTZ
);
CREATE INDEX idx_sync_queue_pending ON sync_queue (status, created_at ASC) WHERE status = 'pending';
```

Workers poll with `SELECT ... FOR UPDATE SKIP LOCKED` for distributed processing:
```rust
let event = sqlx::query_as::<_, SyncQueueRow>(
    "UPDATE sync_queue SET status = 'processing', started_at = now() \
     WHERE id = (SELECT id FROM sync_queue WHERE status = 'pending' ORDER BY created_at LIMIT 1 \
     FOR UPDATE SKIP LOCKED) RETURNING *"
).fetch_optional(&pool).await?;
```

**Option B: Redis streams** (higher throughput):
```rust
// Producer
redis.xadd("player_sync", "*", &[("discord_id", &discord_id)]).await?;

// Consumer (with consumer group for multiple workers)
redis.xreadgroup("sync_workers", "worker_1", &["player_sync"], &[">"], Some(10)).await?;
```

### 16.8 Horizontal Scaling

For 500K+ users, run multiple instances of the application:

```yaml
# compose.yml — scale app to 3 instances
services:
  app:
    build: .
    deploy:
      replicas: 3
      resources:
        limits:
          memory: 256M
```

**Requirements for horizontal scaling**:
- Replace mpsc channels with persistent queue (16.7) — all instances share the same queue
- Add distributed locking for refresh worker to prevent duplicate fetches:
  ```sql
  -- Advisory lock per external_id
  SELECT pg_try_advisory_xact_lock(hashtext($1))
  ```
  Or use Redis distributed locks (Redlock pattern).
- Load balancer in front of app instances (nginx, Traefik, cloud LB)
- All instances share the same PostgreSQL and Redis

### 16.9 Memory Optimization

For large deployments, optimize memory usage:

```rust
// 1. Don't hold full JSONB in memory during bulk operations
// Use streaming queries (see 16.4)

// 2. Limit pre-rendered HTML size or generate on-demand
// For large player lists, use server-side pagination instead of pre-rendering

// 3. Tune channel buffer sizes based on expected throughput
let (player_sync_tx, player_sync_rx) = mpsc::channel::<PlayerSyncEvent>(2048); // up from 512

// 4. Use connection pooler (PgBouncer) to multiplex connections
// This lets you run more app instances without exhausting DB connections
```

**Container sizing guide**:
| Users | App Memory | DB Memory | Redis |
|-------|-----------|-----------|-------|
| 0–10K | 128MB | 256MB | none |
| 10K–100K | 256MB | 512MB | 64MB |
| 100K–500K | 512MB | 1GB | 128MB |
| 500K–1M+ | 256MB x N replicas | 2GB+ | 256MB |

### 16.10 Monitoring at Scale

Add metrics for large deployments:

```rust
// Track key health indicators
struct Metrics {
    total_users: AtomicU64,
    active_users: AtomicU64,
    pending_syncs: AtomicU64,
    refresh_lag_seconds: AtomicI64,  // how far behind the refresh worker is
    sync_errors_total: AtomicU64,
    api_calls_total: AtomicU64,
}

// Expose in /health endpoint
Json(json!({
    "status": "healthy",
    "total_verified": metrics.total_users.load(Ordering::Relaxed),
    "active_users": metrics.active_users.load(Ordering::Relaxed),
    "pending_syncs": metrics.pending_syncs.load(Ordering::Relaxed),
    "refresh_lag_seconds": metrics.refresh_lag_seconds.load(Ordering::Relaxed),
    "sync_errors_total": metrics.sync_errors_total.load(Ordering::Relaxed),
}))
```

For production observability, consider adding Prometheus metrics export:
```toml
# Cargo.toml
metrics = "0.24"
metrics-exporter-prometheus = "0.16"
```

---

## 17. Conventions & Rules

1. **One plugin per deployment** — each container/binary runs one plugin type
2. **HTTPS required** — RoleLogic rejects HTTP and private IP addresses
3. **Store the token from POST /register** — it's your only authentication credential
4. **Token scheme is `Token`, not `Bearer`** — `Authorization: Token rl_...`
5. **evaluate() must be sync and fast** — no async, no I/O, no allocations in the hot path
6. **Denormalize hot filter columns** — extract from JSONB into SQL columns for WHERE clause efficiency
7. **Use PUT for bulk syncs, POST/DELETE for real-time** — PUT is atomic and respects user limits
8. **SQL-side filtering for bulk sync** — don't load all JSONB into memory for condition evaluation
9. **Workers never crash** — log errors, continue processing, retry on next cycle
10. **Debounce config syncs** — 5-second window to prevent cascading re-evaluations
11. **Rate limit external API calls** — use `governor` crate, respect API-specific limits
12. **Exponential backoff on failures** — `60s * 2^failures` up to 1 hour cap
13. **Batch DB queries** — fetch all assignments in one query, not per-user loops
14. **Concurrent API calls** — use `stream::for_each_concurrent(10, ...)` for sync actions
15. **Connection pooling** — `PgPoolOptions::max_connections(8)` is sufficient for 128MB deployments
16. **Graceful shutdown** — handle SIGTERM, drain in-flight requests
17. **Respect timeouts** — RoleLogic enforces 5s on register/config GET/DELETE, 10s on config POST
18. **Config schema < 50KB** — RoleLogic caps the GET /config response
19. **Return current values** — GET /config `values` object must reflect saved configuration
20. **Idempotent operations** — POST (add user) and DELETE (remove user) are idempotent in the RoleLogic API
