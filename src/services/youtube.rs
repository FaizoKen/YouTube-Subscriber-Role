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

/// Result of a subscription check, including when the subscription was created.
pub struct SubscriptionResult {
    pub is_subscribed: bool,
    pub subscribed_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Statistics about the user's own YouTube channel.
pub struct ChannelStats {
    pub subscriber_count: i64,
    pub view_count: i64,
    pub video_count: i64,
    pub channel_created_at: Option<chrono::DateTime<chrono::Utc>>,
    pub hidden_subscriber_count: bool,
    pub country: Option<String>,
    pub custom_url: Option<String>,
}

#[derive(serde::Deserialize)]
struct GoogleTokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
}

#[derive(serde::Deserialize)]
struct SubscriptionListResponse {
    items: Option<Vec<SubscriptionItem>>,
}

#[derive(serde::Deserialize)]
struct SubscriptionItem {
    snippet: Option<SubscriptionSnippet>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SubscriptionSnippet {
    published_at: Option<String>,
}

#[derive(serde::Deserialize)]
struct ChannelListResponse {
    items: Option<Vec<ChannelItem>>,
}

#[derive(serde::Deserialize)]
struct ChannelItem {
    snippet: Option<ChannelSnippet>,
    statistics: Option<ChannelStatistics>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChannelSnippet {
    published_at: Option<String>,
    country: Option<String>,
    custom_url: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChannelStatistics {
    subscriber_count: Option<String>,
    view_count: Option<String>,
    video_count: Option<String>,
    hidden_subscriber_count: Option<bool>,
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
    /// Returns subscription status and the date the subscription was created.
    pub async fn check_subscription(
        &self,
        access_token: &str,
        channel_id: &str,
    ) -> Result<SubscriptionResult, YouTubeError> {
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

        match body.items {
            Some(items) if !items.is_empty() => {
                let subscribed_at = items[0]
                    .snippet
                    .as_ref()
                    .and_then(|s| s.published_at.as_deref())
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                    .map(|dt| dt.with_timezone(&chrono::Utc));

                Ok(SubscriptionResult {
                    is_subscribed: true,
                    subscribed_at,
                })
            }
            _ => Ok(SubscriptionResult {
                is_subscribed: false,
                subscribed_at: None,
            }),
        }
    }

    /// Fetch the user's own YouTube channel statistics.
    /// Returns None-like stats if the user has no YouTube channel.
    pub async fn fetch_channel_stats(
        &self,
        access_token: &str,
    ) -> Result<ChannelStats, YouTubeError> {
        let resp = self
            .http
            .get("https://www.googleapis.com/youtube/v3/channels")
            .query(&[
                ("part", "statistics,snippet"),
                ("mine", "true"),
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
            if body.contains("forbidden") || body.contains("insufficientPermissions") {
                return Err(YouTubeError::TokenRevoked);
            }
            return Err(YouTubeError::ApiError(format!("403: {body}")));
        }

        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(YouTubeError::ApiError(format!("{status}: {body}")));
        }

        let body: ChannelListResponse = resp
            .json()
            .await
            .map_err(|e| YouTubeError::ApiError(format!("Failed to parse response: {e}")))?;

        let item = body.items.and_then(|mut items| {
            if items.is_empty() {
                None
            } else {
                Some(items.remove(0))
            }
        });

        let Some(item) = item else {
            // User has a Google account but no YouTube channel
            return Ok(ChannelStats {
                subscriber_count: 0,
                view_count: 0,
                video_count: 0,
                channel_created_at: None,
                hidden_subscriber_count: false,
                country: None,
                custom_url: None,
            });
        };

        let stats = item.statistics.as_ref();
        let snippet = item.snippet.as_ref();

        Ok(ChannelStats {
            subscriber_count: stats
                .and_then(|s| s.subscriber_count.as_deref())
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
            view_count: stats
                .and_then(|s| s.view_count.as_deref())
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
            video_count: stats
                .and_then(|s| s.video_count.as_deref())
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
            channel_created_at: snippet
                .and_then(|s| s.published_at.as_deref())
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.with_timezone(&chrono::Utc)),
            hidden_subscriber_count: stats
                .and_then(|s| s.hidden_subscriber_count)
                .unwrap_or(false),
            country: snippet.and_then(|s| s.country.clone()),
            custom_url: snippet.and_then(|s| s.custom_url.clone()),
        })
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
