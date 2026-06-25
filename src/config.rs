use std::env;

#[derive(Clone)]
pub struct AppConfig {
    pub database_url: String,
    pub google_client_id: String,
    pub google_client_secret: String,
    pub session_secret: String,
    pub base_url: String,
    pub listen_addr: String,
    pub youtube_quota_per_day: i64,
    /// Public YouTube Data API key(s) for batched, OAuth-free `channels.list?id=`
    /// statistics lookups (50 channels / 1 unit). Empty disables batching and
    /// falls back to the per-user `mine=true` path. Multiple keys (comma-
    /// separated) are round-robined — only a quota multiplier if they belong to
    /// *different* Cloud projects; keys in one project share that project's quota.
    pub youtube_api_keys: Vec<String>,
    /// Fraction of the daily quota reserved for interactive (link-time) checks a
    /// user is actively waiting on. Background re-checks can't touch it, so a
    /// verify spike never starves real-time verification. Default 0.20.
    pub quota_interactive_reserve: f64,
    /// Fraction of the nominal quota the governor will actually spend, leaving
    /// headroom for accounting skew / external project usage. Default 0.95.
    pub quota_safety_fraction: f64,
    /// Number of background refresh workers to run. They share the governor's
    /// budget and partition rows by `hashtext(discord_id) % N`. Default 1.
    pub refresh_workers: i64,
    /// Base URL of the Auth Gateway (no trailing slash, no `/auth` suffix).
    /// Prod: usually the same origin as `BASE_URL` (derived if unset).
    /// Local dev: set to the gateway's local listener, e.g. http://localhost:8090
    pub auth_gateway_url: String,
    /// Shared secret for plugin → gateway /auth/internal/* calls
    /// (sent in the `X-Internal-Key` header). Must match INTERNAL_API_KEY on the gateway.
    pub internal_api_key: String,
    /// Origin of the RoleLogic dashboard that embeds the iframe role-config page,
    /// used for the `frame-ancestors` CSP. `None` falls back to `*` (dev /
    /// self-hosted RoleLogic). Set explicitly in prod, e.g. https://app.rolelogic.com.
    pub rl_dashboard_origin: Option<String>,
    /// Origins accepted on cookie-authenticated state-changing admin requests
    /// (server-side CSRF defense). Our own origin (derived from BASE_URL) plus
    /// the dashboard origin when configured.
    pub allowed_origins: Vec<String>,
}

/// Extract the origin (scheme://host[:port]) from BASE_URL, dropping any path prefix.
fn derive_origin(base_url: &str) -> String {
    if let Some(scheme_end) = base_url.find("://") {
        let after_scheme = scheme_end + 3;
        if let Some(path_slash) = base_url[after_scheme..].find('/') {
            return base_url[..after_scheme + path_slash].to_string();
        }
    }
    base_url.to_string()
}

impl AppConfig {
    pub fn from_env() -> Self {
        let base_url = env::var("BASE_URL").expect("BASE_URL must be set");
        let auth_gateway_url = env::var("AUTH_GATEWAY_URL")
            .ok()
            .map(|s| s.trim_end_matches('/').to_string())
            .unwrap_or_else(|| derive_origin(&base_url));

        let rl_dashboard_origin = env::var("RL_DASHBOARD_ORIGIN")
            .ok()
            .map(|s| s.trim().trim_end_matches('/').to_string())
            .filter(|s| !s.is_empty());

        // Server-side CSRF allowlist: our own origin always, plus the
        // dashboard origin when set (the dashboard never posts directly to our
        // admin XHRs — those carry the iframe-session Bearer — but allowlisting
        // it keeps direct-nav-from-dashboard edge cases working).
        let mut allowed_origins = vec![derive_origin(&base_url)];
        if let Some(ref origin) = rl_dashboard_origin {
            if !allowed_origins.iter().any(|o| o == origin) {
                allowed_origins.push(origin.clone());
            }
        }

        Self {
            database_url: env::var("DATABASE_URL").expect("DATABASE_URL must be set"),
            google_client_id: env::var("GOOGLE_CLIENT_ID")
                .expect("GOOGLE_CLIENT_ID must be set"),
            google_client_secret: env::var("GOOGLE_CLIENT_SECRET")
                .expect("GOOGLE_CLIENT_SECRET must be set"),
            session_secret: env::var("SESSION_SECRET").expect("SESSION_SECRET must be set"),
            base_url,
            listen_addr: env::var("LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_string()),
            youtube_quota_per_day: env::var("YOUTUBE_QUOTA_PER_DAY")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(10000),
            youtube_api_keys: env::var("YOUTUBE_API_KEY")
                .or_else(|_| env::var("YOUTUBE_API_KEYS"))
                .map(|v| {
                    v.split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                })
                .unwrap_or_default(),
            quota_interactive_reserve: env::var("QUOTA_INTERACTIVE_RESERVE")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0.20),
            quota_safety_fraction: env::var("QUOTA_SAFETY_FRACTION")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0.95),
            refresh_workers: env::var("REFRESH_WORKERS")
                .ok()
                .and_then(|v| v.parse::<i64>().ok())
                .map(|n| n.clamp(1, 64))
                .unwrap_or(1),
            auth_gateway_url,
            internal_api_key: env::var("INTERNAL_API_KEY")
                .expect("INTERNAL_API_KEY must be set (must match the Auth Gateway's value)"),
            rl_dashboard_origin,
            allowed_origins,
        }
    }

    pub fn google_redirect_uri(&self) -> String {
        format!("{}/verify/youtube/callback", self.base_url)
    }
}
