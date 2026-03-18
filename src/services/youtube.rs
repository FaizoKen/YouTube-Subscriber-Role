use std::num::NonZeroU32;
use std::sync::Arc;

use governor::{Quota, RateLimiter};

use crate::config::AppConfig;
use crate::error::YouTubeError;

#[derive(Clone)]
pub struct YouTubeClient {
    http: reqwest::Client,
    rate_limiter: Arc<RateLimiter<governor::state::NotKeyed, governor::state::InMemoryState, governor::clock::DefaultClock>>,
}

/// Tokens returned from Google OAuth token exchange or refresh.
pub struct GoogleTokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: i64,
}

#[derive(serde::Deserialize)]
struct GoogleTokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
}

#[derive(serde::Deserialize)]
struct SubscriptionListResponse {
    items: Option<Vec<serde_json::Value>>,
}

impl YouTubeClient {
    pub fn new() -> Self {
        let http = reqwest::Client::builder()
            .user_agent("YouTubeSubRole/1.0")
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .expect("Failed to build HTTP client");

        // 2 requests per second to stay well within YouTube limits
        let quota = Quota::per_second(NonZeroU32::new(2).unwrap());
        let rate_limiter = Arc::new(RateLimiter::direct(quota));

        Self { http, rate_limiter }
    }

    pub async fn wait_for_permit(&self) {
        self.rate_limiter.until_ready().await;
    }

    /// Check if a user is subscribed to a specific YouTube channel.
    /// Returns true if subscribed, false otherwise.
    pub async fn check_subscription(
        &self,
        access_token: &str,
        channel_id: &str,
    ) -> Result<bool, YouTubeError> {
        let resp = self
            .http
            .get("https://www.googleapis.com/youtube/v3/subscriptions")
            .query(&[
                ("part", "snippet"),
                ("mine", "true"),
                ("forChannelId", channel_id),
            ])
            .header("Authorization", format!("Bearer {access_token}"))
            .send()
            .await?;

        let status = resp.status();

        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(YouTubeError::TokenExpired);
        }

        if status == reqwest::StatusCode::FORBIDDEN {
            let body = resp.text().await.unwrap_or_default();
            if body.contains("quotaExceeded") {
                return Err(YouTubeError::QuotaExceeded);
            }
            // Could also be token revoked
            if body.contains("forbidden") || body.contains("insufficientPermissions") {
                return Err(YouTubeError::TokenRevoked);
            }
            return Err(YouTubeError::ApiError(format!("403: {body}")));
        }

        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(YouTubeError::NotFound);
        }

        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(YouTubeError::ApiError(format!("{status}: {body}")));
        }

        let body: SubscriptionListResponse = resp
            .json()
            .await
            .map_err(|e| YouTubeError::ApiError(format!("Failed to parse response: {e}")))?;

        Ok(body.items.map_or(false, |items| !items.is_empty()))
    }

    /// Exchange a Google OAuth authorization code for tokens.
    pub async fn exchange_google_code(
        &self,
        config: &AppConfig,
        code: &str,
    ) -> Result<GoogleTokens, YouTubeError> {
        let resp: GoogleTokenResponse = self
            .http
            .post("https://oauth2.googleapis.com/token")
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", &config.google_redirect_uri()),
                ("client_id", &config.google_client_id),
                ("client_secret", &config.google_client_secret),
            ])
            .send()
            .await?
            .json()
            .await
            .map_err(|e| YouTubeError::ApiError(format!("Token exchange parse failed: {e}")))?;

        Ok(GoogleTokens {
            access_token: resp.access_token,
            refresh_token: resp.refresh_token,
            expires_in: resp.expires_in.unwrap_or(3600),
        })
    }

    /// Refresh a Google access token using a stored refresh token.
    pub async fn refresh_google_token(
        &self,
        config: &AppConfig,
        refresh_token: &str,
    ) -> Result<GoogleTokens, YouTubeError> {
        let resp = self
            .http
            .post("https://oauth2.googleapis.com/token")
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
                ("client_id", &config.google_client_id),
                ("client_secret", &config.google_client_secret),
            ])
            .send()
            .await?;

        let status = resp.status();

        if status == reqwest::StatusCode::BAD_REQUEST || status == reqwest::StatusCode::UNAUTHORIZED {
            let body = resp.text().await.unwrap_or_default();
            if body.contains("invalid_grant") {
                return Err(YouTubeError::TokenRevoked);
            }
            return Err(YouTubeError::ApiError(format!("Token refresh failed: {body}")));
        }

        let token_resp: GoogleTokenResponse = resp
            .json()
            .await
            .map_err(|e| YouTubeError::ApiError(format!("Token refresh parse failed: {e}")))?;

        Ok(GoogleTokens {
            access_token: token_resp.access_token,
            refresh_token: token_resp.refresh_token,
            expires_in: token_resp.expires_in.unwrap_or(3600),
        })
    }

    /// Build Google OAuth authorize URL.
    pub fn google_authorize_url(config: &AppConfig, state: &str) -> String {
        let redirect_uri = config.google_redirect_uri();
        format!(
            "https://accounts.google.com/o/oauth2/v2/auth?client_id={}&redirect_uri={}&response_type=code&scope={}&access_type=offline&prompt=consent&state={}",
            config.google_client_id,
            urlencoding::encode(&redirect_uri),
            urlencoding::encode("https://www.googleapis.com/auth/youtube.readonly"),
            state
        )
    }
}
