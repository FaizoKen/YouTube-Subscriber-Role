use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::Json;
use axum_extra::extract::cookie::{Cookie, CookieJar};
use rand::Rng;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::AppError;
use crate::services::discord_oauth::{self, DiscordOAuth};
use crate::services::sync::PlayerSyncEvent;
use crate::services::youtube::YouTubeClient;
use crate::AppState;

const SESSION_COOKIE: &str = "ysr_session";

/// Returns (discord_id, display_name)
fn get_session(jar: &CookieJar, secret: &str) -> Result<(String, String), AppError> {
    let cookie = jar
        .get(SESSION_COOKIE)
        .ok_or(AppError::Unauthorized)?;

    discord_oauth::verify_session(cookie.value(), secret)
        .ok_or(AppError::Unauthorized)
}

pub fn render_verify_page(base_url: &str) -> String {
    let login_url = format!("{base_url}/verify/login");

    format!(
        r##"<!DOCTYPE html>
<html>
<head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <title>YouTube Sub Role - Link Account</title>
    <link rel="icon" href="{base_url}/favicon.ico" type="image/x-icon">
    <meta name="description" content="Link your Discord and YouTube accounts to automatically receive server roles based on your YouTube subscriptions.">
    <meta property="og:type" content="website">
    <meta property="og:title" content="YouTube Sub Role - Link Account">
    <meta property="og:description" content="Link your Discord and YouTube accounts to automatically receive server roles based on your YouTube subscriptions.">
    <meta property="og:url" content="{base_url}/verify">
    <meta name="theme-color" content="#ff0000">
    <style>
        * {{ box-sizing: border-box; margin: 0; padding: 0; }}
        body {{ font-family: system-ui, -apple-system, sans-serif; max-width: 580px; margin: 0 auto; padding: 32px 20px; background: #0e1525; color: #c8ccd4; min-height: 100vh; }}
        h1 {{ color: #ff4444; font-size: 24px; margin-bottom: 4px; }}
        h2 {{ color: #fff; font-size: 17px; margin-bottom: 14px; }}
        p {{ line-height: 1.6; margin: 6px 0; font-size: 14px; }}
        a {{ color: #74b9ff; }}
        .subtitle {{ color: #7a8299; font-size: 14px; margin-bottom: 20px; }}
        .card {{ background: #161d2e; padding: 22px; border-radius: 10px; margin: 14px 0; border: 1px solid #1e2a3d; }}
        .btn {{ display: inline-flex; align-items: center; gap: 8px; padding: 10px 22px; color: #fff; text-decoration: none; border-radius: 6px; font-size: 14px; font-weight: 500; border: none; cursor: pointer; font-family: inherit; transition: background .15s; }}
        .btn-discord {{ background: #5865f2; }}
        .btn-discord:hover {{ background: #4752c4; }}
        .btn-google {{ background: #ea4335; }}
        .btn-google:hover {{ background: #c5221f; }}
        .btn-danger {{ background: transparent; color: #f87171; border: 1px solid #7f1d1d; font-size: 13px; padding: 8px 16px; }}
        .btn-danger:hover {{ background: #7f1d1d33; }}
        .btn:disabled {{ opacity: 0.5; cursor: not-allowed; }}
        .badge {{ display: inline-block; padding: 3px 10px; border-radius: 20px; font-size: 12px; font-weight: 500; }}
        .badge-ok {{ background: #052e16; color: #4ade80; border: 1px solid #14532d; }}
        .badge-discord {{ background: #1e1b4b; color: #a5b4fc; border: 1px solid #312e81; }}
        .msg {{ padding: 10px 14px; border-radius: 6px; margin: 12px 0; font-size: 13px; line-height: 1.5; }}
        .msg-error {{ background: #1c0a0a; color: #fca5a5; border: 1px solid #7f1d1d; }}
        .msg-success {{ background: #052e16; color: #86efac; border: 1px solid #14532d; }}
        .info-row {{ display: flex; align-items: center; gap: 8px; margin: 6px 0; font-size: 14px; }}
        .info-row .label {{ color: #64748b; min-width: 80px; }}
        .info-row .val {{ color: #ff4444; font-weight: 600; }}
        .actions {{ display: flex; gap: 8px; margin-top: 16px; flex-wrap: wrap; }}
        .hidden {{ display: none !important; }}
        .divider {{ border: none; border-top: 1px solid #1e293b; margin: 16px 0; }}
        .trust-note {{ font-size: 13px; color: #94a3b8; background: #111827; border-left: 3px solid #3b82f6; padding: 10px 14px; border-radius: 0 6px 6px 0; margin: 10px 0; line-height: 1.6; }}
        .trust-note strong {{ color: #e2e8f0; }}
    </style>
</head>
<body>
    <div style="display:flex; align-items:center; gap:10px; margin-bottom:4px;">
        <h1 style="margin:0;">YouTube Sub Role</h1>
        <span style="font-size:11px; color:#64748b; background:#1e293b; padding:2px 8px; border-radius:4px;">Powered by <a href="https://rolelogic.faizo.net" target="_blank" rel="noopener" style="color:#74b9ff; text-decoration:none;">RoleLogic</a></span>
    </div>
    <p class="subtitle">Link your Discord and YouTube accounts to automatically receive server roles based on your YouTube subscriptions.</p>

    <!-- Loading -->
    <div id="loading-section" class="card">
        <p style="color: #64748b;">Loading...</p>
    </div>

    <!-- Login -->
    <div id="login-section" class="card hidden">
        <h2>Step 1: Sign in with Discord</h2>
        <p>Sign in so we know which Discord account to assign roles to.</p>
        <p class="trust-note">We request the <strong>identify</strong> and <strong>guilds</strong> scopes only.</p>
        <div class="actions">
            <a href="{login_url}" class="btn btn-discord">
                <svg width="20" height="15" viewBox="0 0 71 55" fill="white"><path d="M60.1 4.9A58.5 58.5 0 0045.4.2a.2.2 0 00-.2.1 40.8 40.8 0 00-1.8 3.7 54 54 0 00-16.2 0A37.3 37.3 0 0025.4.3a.2.2 0 00-.2-.1A58.4 58.4 0 0010.6 4.9a.2.2 0 00-.1.1C1.5 18 -.9 30.6.3 43a.2.2 0 00.1.2 58.7 58.7 0 0017.7 9 .2.2 0 00.3-.1 42 42 0 003.6-5.9.2.2 0 00-.1-.3 38.6 38.6 0 01-5.5-2.6.2.2 0 01 0-.4l1.1-.9a.2.2 0 01.2 0 41.9 41.9 0 0035.6 0 .2.2 0 01.2 0l1.1.9a.2.2 0 010 .3 36.3 36.3 0 01-5.5 2.7.2.2 0 00-.1.3 47.2 47.2 0 003.6 5.9.2.2 0 00.3.1A58.5 58.5 0 0070.3 43a.2.2 0 00.1-.2c1.4-14.7-2.4-27.5-10.2-38.8a.2.2 0 00-.1 0zM23.7 35.3c-3.4 0-6.1-3.1-6.1-6.8s2.7-6.9 6.1-6.9 6.2 3.1 6.1 6.9c0 3.7-2.7 6.8-6.1 6.8zm22.6 0c-3.4 0-6.1-3.1-6.1-6.8s2.7-6.9 6.1-6.9 6.2 3.1 6.1 6.9c0 3.7-2.7 6.8-6.1 6.8z"/></svg>
                Login with Discord
            </a>
        </div>
    </div>

    <!-- YouTube link step -->
    <div id="youtube-section" class="card hidden">
        <div style="display:flex; align-items:center; gap:10px; margin-bottom:14px;">
            <h2 style="margin:0;">Step 2: Link YouTube</h2>
            <span class="badge badge-discord" id="yt-discord-badge"></span>
        </div>
        <p>Connect your YouTube account so we can check your subscriptions.</p>
        <p class="trust-note">We request <strong>read-only</strong> access to your YouTube subscriptions. We cannot modify your account or post on your behalf.</p>
        <div class="actions">
            <a href="{base_url}/verify/youtube" class="btn btn-google">
                <svg width="18" height="18" viewBox="0 0 24 24" fill="white"><path d="M23.5 6.2a3 3 0 00-2.1-2.1C19.5 3.5 12 3.5 12 3.5s-7.5 0-9.4.6A3 3 0 00.5 6.2 31.4 31.4 0 000 12a31.4 31.4 0 00.5 5.8 3 3 0 002.1 2.1c1.9.6 9.4.6 9.4.6s7.5 0 9.4-.6a3 3 0 002.1-2.1A31.4 31.4 0 0024 12a31.4 31.4 0 00-.5-5.8zM9.6 15.6V8.4l6.3 3.6-6.3 3.6z"/></svg>
                Link YouTube Account
            </a>
        </div>
    </div>

    <!-- Linked -->
    <div id="linked-section" class="card hidden">
        <div style="display:flex; align-items:center; gap:10px; margin-bottom:14px;">
            <h2 style="margin:0;">Account Linked</h2>
            <span class="badge badge-ok">Active</span>
        </div>
        <div class="info-row"><span class="label">Discord</span> <span class="val" id="linked-discord" style="color:#94a3b8;font-weight:400;font-size:13px;"></span></div>
        <div class="info-row"><span class="label">YouTube</span> <span class="val" id="linked-youtube" style="color:#ff4444;font-size:13px;"></span></div>
        <p style="color:#4ade80; margin-top:12px; font-size:13px;">Your roles are assigned automatically based on your YouTube subscriptions.</p>
        <hr class="divider">
        <div class="actions">
            <button class="btn btn-danger" onclick="doUnlink()">Unlink Account</button>
        </div>
    </div>

    <!-- Messages -->
    <div id="msg" class="hidden"></div>

    <noscript><p style="color:#f87171; margin-top:20px;">JavaScript is required.</p></noscript>

    <script>
    const API = '';

    async function api(method, path, body) {{
        const opts = {{ method, headers: {{}}, credentials: 'include' }};
        if (body) {{
            opts.headers['Content-Type'] = 'application/json';
            opts.body = JSON.stringify(body);
        }}
        const res = await fetch(API + path, opts);
        const data = await res.json();
        if (!res.ok) throw new Error(data.error || 'Request failed');
        return data;
    }}

    function showSection(id) {{
        ['loading-section','login-section','youtube-section','linked-section'].forEach(s =>
            document.getElementById(s).classList.add('hidden')
        );
        document.getElementById(id).classList.remove('hidden');
    }}

    function showMsg(text, type) {{
        const el = document.getElementById('msg');
        el.className = 'msg msg-' + type;
        el.textContent = text;
        el.classList.remove('hidden');
        if (type === 'success') setTimeout(() => el.classList.add('hidden'), 6000);
    }}

    function clearMsg() {{ document.getElementById('msg').classList.add('hidden'); }}

    async function init() {{
        try {{
            const s = await api('GET', '/verify/status');
            if (s.linked) {{
                document.getElementById('linked-discord').textContent = s.display_name;
                document.getElementById('linked-youtube').textContent = 'Connected';
                showSection('linked-section');
            }} else {{
                document.getElementById('yt-discord-badge').textContent = s.display_name;
                showSection('youtube-section');
            }}
        }} catch (e) {{
            showSection('login-section');
        }}
    }}

    async function doUnlink() {{
        clearMsg();
        if (!confirm('Unlink your account? You will lose all assigned roles.')) return;
        try {{
            await api('POST', '/verify/unlink');
            showSection('login-section');
            showMsg('Account unlinked.', 'success');
        }} catch (e) {{ showMsg(e.message, 'error'); }}
    }}

    init();
    </script>
</body>
</html>"##
    )
}

pub async fn verify_page(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    (
        [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
        state.verify_html.clone(),
    )
}

pub async fn login(State(state): State<Arc<AppState>>) -> Response {
    let state_param: String = rand::thread_rng()
        .sample_iter(&rand::distributions::Alphanumeric)
        .take(32)
        .map(char::from)
        .collect();

    let expires = chrono::Utc::now() + chrono::Duration::minutes(10);

    if let Err(e) = sqlx::query(
        "INSERT INTO oauth_states (state, redirect_data, expires_at) VALUES ($1, $2, $3)",
    )
    .bind(&state_param)
    .bind(serde_json::json!({"provider": "discord"}))
    .bind(expires)
    .execute(&state.pool)
    .await
    {
        tracing::error!("Failed to store OAuth state: {e}");
        return (StatusCode::INTERNAL_SERVER_ERROR, "Internal error").into_response();
    }

    let url = DiscordOAuth::authorize_url(&state.config, &state_param);
    Redirect::temporary(&url).into_response()
}

#[derive(Deserialize)]
pub struct CallbackQuery {
    pub code: Option<String>,
    pub state: String,
    pub error: Option<String>,
}

pub async fn callback(
    State(state): State<Arc<AppState>>,
    jar: CookieJar,
    Query(query): Query<CallbackQuery>,
) -> Result<(CookieJar, Redirect), AppError> {
    if query.error.is_some() || query.code.is_none() {
        return Ok((jar, Redirect::to(&format!("{}/verify", state.config.base_url))));
    }
    let code = query.code.unwrap();

    // Validate state (CSRF protection)
    let valid = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM oauth_states WHERE state = $1 AND expires_at > now())",
    )
    .bind(&query.state)
    .fetch_one(&state.pool)
    .await
    .unwrap_or(false);

    if !valid {
        return Err(AppError::BadRequest("Invalid or expired OAuth state".into()));
    }

    sqlx::query("DELETE FROM oauth_states WHERE state = $1")
        .bind(&query.state)
        .execute(&state.pool)
        .await?;

    // Exchange code for token and get user info
    let oauth = DiscordOAuth::with_client(state.oauth_http.clone());
    let (access_token, refresh_token) = oauth.exchange_code(&state.config, &code).await?;
    let (discord_id, display_name) = oauth.get_user(&access_token).await?;

    // Store refresh token
    if let Some(ref rt) = refresh_token {
        let _ = sqlx::query(
            "INSERT INTO discord_tokens (discord_id, refresh_token) VALUES ($1, $2) \
             ON CONFLICT (discord_id) DO UPDATE SET refresh_token = $2",
        )
        .bind(&discord_id)
        .bind(rt)
        .execute(&state.pool)
        .await;
    }

    // Fetch and store guild memberships
    match oauth.get_user_guilds(&access_token).await {
        Ok(guilds) if !guilds.is_empty() => {
            let guild_ids: Vec<&str> = guilds.iter().map(|(id, _)| id.as_str()).collect();
            let guild_names: Vec<&str> = guilds.iter().map(|(_, name)| name.as_str()).collect();
            let mut tx = state.pool.begin().await?;
            sqlx::query("DELETE FROM user_guilds WHERE discord_id = $1")
                .bind(&discord_id)
                .execute(&mut *tx)
                .await?;
            sqlx::query(
                "INSERT INTO user_guilds (discord_id, guild_id, guild_name, updated_at) \
                 SELECT $1, UNNEST($2::text[]), UNNEST($3::text[]), now()",
            )
            .bind(&discord_id)
            .bind(&guild_ids)
            .bind(&guild_names)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
        }
        Ok(_) => {}
        Err(e) => {
            tracing::warn!(discord_id, "Failed to fetch user guilds: {e}");
        }
    }

    // Create session cookie
    let session_value = discord_oauth::sign_session(&discord_id, &display_name, &state.config.session_secret);

    let cookie = Cookie::build((SESSION_COOKIE, session_value))
        .path("/")
        .http_only(true)
        .same_site(axum_extra::extract::cookie::SameSite::Lax)
        .max_age(time::Duration::hours(1));

    let jar = jar.add(cookie);

    Ok((jar, Redirect::to(&format!("{}/verify", state.config.base_url))))
}

/// Redirect to Google OAuth for YouTube account linking.
pub async fn youtube_login(
    State(state): State<Arc<AppState>>,
    jar: CookieJar,
) -> Result<Response, AppError> {
    // Must be logged in via Discord first
    let _ = get_session(&jar, &state.config.session_secret)?;

    let state_param: String = rand::thread_rng()
        .sample_iter(&rand::distributions::Alphanumeric)
        .take(32)
        .map(char::from)
        .collect();

    let expires = chrono::Utc::now() + chrono::Duration::minutes(10);

    sqlx::query(
        "INSERT INTO oauth_states (state, redirect_data, expires_at) VALUES ($1, $2, $3)",
    )
    .bind(&state_param)
    .bind(serde_json::json!({"provider": "google"}))
    .bind(expires)
    .execute(&state.pool)
    .await?;

    let url = YouTubeClient::google_authorize_url(&state.config, &state_param);
    Ok(Redirect::temporary(&url).into_response())
}

/// Google OAuth callback — exchanges code for tokens and links the YouTube account.
pub async fn youtube_callback(
    State(state): State<Arc<AppState>>,
    jar: CookieJar,
    Query(query): Query<CallbackQuery>,
) -> Result<(CookieJar, Redirect), AppError> {
    let (discord_id, display_name) = get_session(&jar, &state.config.session_secret)?;

    if query.error.is_some() || query.code.is_none() {
        return Ok((jar, Redirect::to(&format!("{}/verify", state.config.base_url))));
    }
    let code = query.code.unwrap();

    // Validate state
    let valid = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM oauth_states WHERE state = $1 AND expires_at > now())",
    )
    .bind(&query.state)
    .fetch_one(&state.pool)
    .await
    .unwrap_or(false);

    if !valid {
        return Err(AppError::BadRequest("Invalid or expired OAuth state".into()));
    }

    sqlx::query("DELETE FROM oauth_states WHERE state = $1")
        .bind(&query.state)
        .execute(&state.pool)
        .await?;

    // Exchange code for Google tokens
    let tokens = state.youtube_client.exchange_google_code(&state.config, &code).await
        .map_err(|e| AppError::Internal(format!("Google token exchange failed: {e}")))?;

    let refresh_token = tokens.refresh_token
        .ok_or_else(|| AppError::Internal("Google did not return a refresh token. Try unlinking and re-linking.".into()))?;

    let expires_at = chrono::Utc::now() + chrono::Duration::seconds(tokens.expires_in);

    // Store linked account
    sqlx::query(
        "INSERT INTO linked_accounts (discord_id, discord_name, google_access_token, google_refresh_token, google_token_expires_at) \
         VALUES ($1, $2, $3, $4, $5) \
         ON CONFLICT (discord_id) DO UPDATE SET \
         discord_name = $2, google_access_token = $3, google_refresh_token = $4, google_token_expires_at = $5, linked_at = now()",
    )
    .bind(&discord_id)
    .bind(&display_name)
    .bind(&tokens.access_token)
    .bind(&refresh_token)
    .bind(expires_at)
    .execute(&state.pool)
    .await?;

    // Seed subscription_cache for all channels configured in guilds this user is in
    sqlx::query(
        "INSERT INTO subscription_cache (discord_id, channel_id, next_check_at) \
         SELECT $1, rl.channel_id, now() \
         FROM role_links rl \
         JOIN user_guilds ug ON ug.guild_id = rl.guild_id \
         WHERE ug.discord_id = $1 AND rl.channel_id IS NOT NULL \
         ON CONFLICT (discord_id, channel_id) DO UPDATE SET next_check_at = now()",
    )
    .bind(&discord_id)
    .execute(&state.pool)
    .await?;

    // Trigger role sync
    let _ = state
        .player_sync_tx
        .send(PlayerSyncEvent::AccountLinked {
            discord_id: discord_id.clone(),
        })
        .await;

    tracing::info!(discord_id, "YouTube account linked");

    Ok((jar, Redirect::to(&format!("{}/verify", state.config.base_url))))
}

pub async fn status(
    State(state): State<Arc<AppState>>,
    jar: CookieJar,
) -> Result<Json<Value>, AppError> {
    let (discord_id, display_name) = get_session(&jar, &state.config.session_secret)?;

    let account = sqlx::query_as::<_, (i64,)>(
        "SELECT id FROM linked_accounts WHERE discord_id = $1",
    )
    .bind(&discord_id)
    .fetch_optional(&state.pool)
    .await?;

    Ok(Json(json!({
        "discord_id": discord_id,
        "display_name": display_name,
        "linked": account.is_some(),
    })))
}

pub async fn unlink(
    State(state): State<Arc<AppState>>,
    jar: CookieJar,
) -> Result<Json<Value>, AppError> {
    let (discord_id, _) = get_session(&jar, &state.config.session_secret)?;

    let existed = sqlx::query(
        "DELETE FROM linked_accounts WHERE discord_id = $1",
    )
    .bind(&discord_id)
    .execute(&state.pool)
    .await?
    .rows_affected() > 0;

    if !existed {
        return Err(AppError::NotFound("No linked account found".into()));
    }

    // Clean up subscription cache
    sqlx::query("DELETE FROM subscription_cache WHERE discord_id = $1")
        .bind(&discord_id)
        .execute(&state.pool)
        .await?;

    // Trigger removal from all roles
    let _ = state
        .player_sync_tx
        .send(PlayerSyncEvent::AccountUnlinked {
            discord_id: discord_id.clone(),
        })
        .await;

    tracing::info!(discord_id, "Account unlinked");

    Ok(Json(json!({"success": true})))
}
