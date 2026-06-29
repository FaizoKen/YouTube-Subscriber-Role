use std::collections::HashMap;

use crate::config::AppConfig;
use crate::error::YouTubeError;

/// Max channel ids per `channels.list` call (YouTube hard limit, still 1 unit).
pub const CHANNEL_BATCH_MAX: usize = 50;

#[derive(Clone)]
pub struct YouTubeClient {
    http: reqwest::Client,
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
    /// The channel's own id. Captured so later refreshes can use the batched,
    /// API-key `channels.list?id=` path (50 channels / 1 unit) instead of a
    /// per-user `mine=true` OAuth call.
    pub channel_id: Option<String>,
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
    id: Option<String>,
    snippet: Option<ChannelSnippet>,
    statistics: Option<ChannelStatistics>,
}

impl ChannelItem {
    /// Project a parsed API item into our `ChannelStats`. Shared by the
    /// per-user (`mine=true`) and batched (`id=`) fetch paths so they never
    /// diverge.
    fn into_stats(self) -> ChannelStats {
        let stats = self.statistics;
        let snippet = self.snippet;
        ChannelStats {
            subscriber_count: stats
                .as_ref()
                .and_then(|s| s.subscriber_count.as_deref())
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
            view_count: stats
                .as_ref()
                .and_then(|s| s.view_count.as_deref())
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
            video_count: stats
                .as_ref()
                .and_then(|s| s.video_count.as_deref())
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
            channel_created_at: snippet
                .as_ref()
                .and_then(|s| s.published_at.as_deref())
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.with_timezone(&chrono::Utc)),
            hidden_subscriber_count: stats
                .as_ref()
                .and_then(|s| s.hidden_subscriber_count)
                .unwrap_or(false),
            country: snippet.as_ref().and_then(|s| s.country.clone()),
            custom_url: snippet.as_ref().and_then(|s| s.custom_url.clone()),
            channel_id: self.id,
        }
    }
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

        // Daily-budget pacing and the short-term burst cap now live in the
        // QuotaGovernor (services/quota.rs), which every quota-costing call
        // passes through. The client just speaks HTTP.
        Self { http }
    }

    /// Send a GET with a couple of jittered retries on transport-level
    /// failures (connect/timeout). HTTP error *statuses* are returned to the
    /// caller untouched — quota/auth handling depends on them. GETs carry no
    /// body, so `try_clone` always succeeds.
    async fn send_with_retry(
        &self,
        req: reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, reqwest::Error> {
        let mut attempt: u32 = 0;
        loop {
            let this = req
                .try_clone()
                .expect("GET requests have no body and are always cloneable");
            match this.send().await {
                Ok(resp) => return Ok(resp),
                Err(e) => {
                    attempt += 1;
                    if attempt >= 3 || !(e.is_timeout() || e.is_connect()) {
                        return Err(e);
                    }
                    // 100–400ms × attempt, jittered, to ride out a blip without
                    // synchronizing retries across concurrent callers.
                    let jitter = 100 + (rand::random::<u64>() % 300);
                    tokio::time::sleep(std::time::Duration::from_millis(jitter * attempt as u64))
                        .await;
                }
            }
        }
    }

    /// Check if a user is subscribed to a specific YouTube channel.
    /// Returns subscription status and the date the subscription was created.
    pub async fn check_subscription(
        &self,
        access_token: &str,
        channel_id: &str,
    ) -> Result<SubscriptionResult, YouTubeError> {
        let req = self
            .http
            .get("https://www.googleapis.com/youtube/v3/subscriptions")
            .query(&[
                ("part", "snippet"),
                ("mine", "true"),
                ("forChannelId", channel_id),
            ])
            .header("Authorization", format!("Bearer {access_token}"));
        let resp = self.send_with_retry(req).await?;

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
        let req = self
            .http
            .get("https://www.googleapis.com/youtube/v3/channels")
            .query(&[("part", "statistics,snippet"), ("mine", "true")])
            .header("Authorization", format!("Bearer {access_token}"));
        let resp = self.send_with_retry(req).await?;

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
                channel_id: None,
            });
        };

        Ok(item.into_stats())
    }

    /// Fetch statistics for up to [`CHANNEL_BATCH_MAX`] channels in a single
    /// call (still **1 quota unit**) using a public **API key** — channel stats
    /// are public, so no per-user OAuth token is involved. This is the 50×
    /// quota multiplier for stat-based rules versus per-user `mine=true` calls,
    /// and it sidesteps token-refresh churn entirely.
    ///
    /// Returns a map keyed by channel id. Ids YouTube omits from the response
    /// (deleted / terminated channels) are simply absent from the map; the
    /// caller decides how to age those rows.
    pub async fn batch_channel_stats(
        &self,
        api_key: &str,
        channel_ids: &[String],
    ) -> Result<HashMap<String, ChannelStats>, YouTubeError> {
        if channel_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let ids = channel_ids.join(",");
        let req = self
            .http
            .get("https://www.googleapis.com/youtube/v3/channels")
            .query(&[
                ("part", "statistics,snippet"),
                ("id", ids.as_str()),
                ("maxResults", "50"),
                ("key", api_key),
            ]);
        let resp = self.send_with_retry(req).await?;

        let status = resp.status();
        if status == reqwest::StatusCode::FORBIDDEN {
            let body = resp.text().await.unwrap_or_default();
            if body.contains("quotaExceeded") {
                return Err(YouTubeError::QuotaExceeded);
            }
            return Err(YouTubeError::ApiError(format!("403: {body}")));
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(YouTubeError::ApiError(format!("{status}: {body}")));
        }

        let body: ChannelListResponse = resp.json().await.map_err(|e| {
            YouTubeError::ApiError(format!("Failed to parse batch channels response: {e}"))
        })?;

        let mut out = HashMap::with_capacity(channel_ids.len());
        for item in body.items.unwrap_or_default() {
            let stats = item.into_stats();
            if let Some(id) = stats.channel_id.clone() {
                out.insert(id, stats);
            }
        }
        Ok(out)
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

        if status == reqwest::StatusCode::BAD_REQUEST || status == reqwest::StatusCode::UNAUTHORIZED
        {
            let body = resp.text().await.unwrap_or_default();
            if body.contains("invalid_grant") {
                return Err(YouTubeError::TokenRevoked);
            }
            return Err(YouTubeError::ApiError(format!(
                "Token refresh failed: {body}"
            )));
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
