use std::env;

#[derive(Clone)]
pub struct AppConfig {
    pub database_url: String,
    pub discord_client_id: String,
    pub discord_client_secret: String,
    pub google_client_id: String,
    pub google_client_secret: String,
    pub session_secret: String,
    pub base_url: String,
    pub listen_addr: String,
    pub youtube_quota_per_day: i64,
}

impl AppConfig {
    pub fn from_env() -> Self {
        Self {
            database_url: env::var("DATABASE_URL").expect("DATABASE_URL must be set"),
            discord_client_id: env::var("DISCORD_CLIENT_ID")
                .expect("DISCORD_CLIENT_ID must be set"),
            discord_client_secret: env::var("DISCORD_CLIENT_SECRET")
                .expect("DISCORD_CLIENT_SECRET must be set"),
            google_client_id: env::var("GOOGLE_CLIENT_ID")
                .expect("GOOGLE_CLIENT_ID must be set"),
            google_client_secret: env::var("GOOGLE_CLIENT_SECRET")
                .expect("GOOGLE_CLIENT_SECRET must be set"),
            session_secret: env::var("SESSION_SECRET").expect("SESSION_SECRET must be set"),
            base_url: env::var("BASE_URL").expect("BASE_URL must be set"),
            listen_addr: env::var("LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_string()),
            youtube_quota_per_day: env::var("YOUTUBE_QUOTA_PER_DAY")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(10000),
        }
    }

    pub fn oauth_redirect_uri(&self) -> String {
        format!("{}/verify/callback", self.base_url)
    }

    pub fn google_redirect_uri(&self) -> String {
        format!("{}/verify/youtube/callback", self.base_url)
    }
}
