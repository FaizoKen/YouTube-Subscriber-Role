use std::sync::Arc;

use axum::extract::{Query, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::Json;
use axum_extra::extract::cookie::{Cookie, CookieJar};
use rand::Rng;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::AppError;
use crate::services::quota::{Class, Outcome};
use crate::services::session;
use crate::services::sync::{self, PlayerSyncEvent};
use crate::services::youtube::YouTubeClient;
use crate::AppState;

const SESSION_COOKIE: &str = "rl_session";

/// How far ahead to schedule the next background subscription check after the
/// inline check done at link time. Matches the worker's 30-min floor, so we
/// don't immediately re-spend quota re-checking a row we just checked.
const INITIAL_RECHECK_SECS: i64 = 1800;

/// When no role needs channel statistics, the subscribers-list stats are a
/// nice-to-have — seed the row this far in the future so it fills in lazily
/// instead of costing a YouTube call per user during a mass-verify spike.
const DEFERRED_CHANNEL_STATS_HOURS: i64 = 6;

/// Returns (discord_id, display_name)
fn get_session(jar: &CookieJar, secret: &str) -> Result<(String, String), AppError> {
    let cookie = jar
        .get(SESSION_COOKIE)
        .ok_or(AppError::Unauthorized)?;

    session::verify_session(cookie.value(), secret)
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
        .btn-recheck {{ background: transparent; color: #74b9ff; border: 1px solid #1e3a5f; font-size: 13px; padding: 8px 16px; }}
        .btn-recheck:hover {{ background: #1e3a5f33; }}
        .refresh-note {{ font-size: 13px; color: #94a3b8; margin-top: 12px; min-height: 18px; transition: color .15s; }}
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
        .btn-logout {{ background: transparent; color: #94a3b8; border: 1px solid #1e293b; padding: 5px 12px; border-radius: 6px; font-size: 12px; cursor: pointer; font-family: inherit; transition: all .15s; }}
        .btn-logout:hover {{ color: #f87171; border-color: #7f1d1d; background: #7f1d1d22; }}
        .guild-ctx {{ display: none; align-items: center; gap: 10px; background: #052e16; border: 1px solid #14532d; color: #86efac; padding: 8px 14px; border-radius: 8px; margin: 12px 0 6px; font-size: 13px; line-height: 1.5; }}
        .guild-ctx.show {{ display: flex; }}
        .guild-ctx.warn {{ background: #1c1208; border-color: #422006; color: #fbbf24; }}
        .guild-ctx .gctx-icon {{ flex-shrink: 0; }}
        .guild-ctx .gctx-name {{ color: #fff; font-weight: 600; }}
        .channel-list {{ display: flex; flex-direction: column; gap: 8px; margin-top: 8px; }}
        .channel-row {{ display: flex; align-items: center; justify-content: space-between; gap: 12px; background: #111827; border: 1px solid #1e2a3d; border-radius: 8px; padding: 10px 12px; }}
        .channel-meta {{ display: flex; flex-direction: column; gap: 2px; min-width: 0; }}
        .channel-name {{ color: #fff; font-weight: 600; font-size: 14px; }}
        .channel-id {{ color: #64748b; font-size: 11px; font-family: ui-monospace, SFMono-Regular, Menlo, monospace; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }}
        a.channel-open {{ flex-shrink: 0; background: #ea4335; color: #fff; text-decoration: none; font-size: 13px; font-weight: 600; padding: 8px 14px; border-radius: 6px; transition: background .15s; }}
        a.channel-open:hover {{ background: #c5221f; }}
    </style>
</head>
<body>
    <div style="display:flex; align-items:center; justify-content:space-between; margin-bottom:4px;">
        <div style="display:flex; align-items:center; gap:10px;">
            <h1 style="margin:0;">YouTube Sub Role</h1>
            <span style="font-size:11px; color:#64748b; background:#1e293b; padding:2px 8px; border-radius:4px;">Powered by <a href="https://rolelogic.faizo.net" target="_blank" rel="noopener" style="color:#74b9ff; text-decoration:none;">RoleLogic</a></span>
        </div>
        <button id="logout-btn" class="btn-logout hidden" onclick="doLogout()">Logout</button>
    </div>
    <p class="subtitle">Everything's on this page: subscribe on YouTube, link your Discord + YouTube accounts, and your server roles are assigned automatically.</p>

    <!-- Server context banner: only shown when ?guild=<id> is present in the URL.
         Lets a server admin share a per-guild link that both verifies the user
         AND auto-enables the role for that specific server in one shot. -->
    <div id="guild-ctx" class="guild-ctx">
        <svg class="gctx-icon" width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"><polyline points="20 6 9 17 4 12"/></svg>
        <span id="guild-ctx-text"></span>
    </div>

    <!-- Step 1: subscribe. Always visible (instructional — a subscription can
         only be confirmed after the user links YouTube), lists the channel(s)
         this server's roles check against, each linking straight to YouTube so
         the admin doesn't have to paste the link separately. -->
    <div id="subscribe-section" class="card">
        <h2>Step 1: Subscribe on YouTube</h2>
        <p>Open the channel below and hit <strong>Subscribe</strong>. Already subscribed? Skip ahead — we detect it once your accounts are linked.</p>
        <div class="channel-list" id="channel-list">
            <p style="color:#64748b; font-size:13px;">Loading channel…</p>
        </div>
    </div>

    <!-- Loading -->
    <div id="loading-section" class="card">
        <p style="color: #64748b;">Loading...</p>
    </div>

    <!-- Login -->
    <div id="login-section" class="card hidden">
        <h2>Step 2: Sign in with Discord</h2>
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
            <h2 style="margin:0;">Step 3: Link YouTube</h2>
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
        <p id="refresh-note" class="refresh-note"></p>
        <p style="margin-top:14px; font-size:13px; color:#94a3b8;">
            Receiving YouTube roles in servers you didn't intend?
            <a href="/auth/my_servers?from=/youtube-subscriber-role/verify" style="color:#74b9ff;">Choose which servers receive roles →</a>
        </p>
        <hr class="divider">
        <div class="actions">
            <button class="btn btn-recheck" onclick="doRefresh(false)">Re-check now</button>
            <button class="btn btn-danger" onclick="doUnlink()">Unlink Account</button>
        </div>
    </div>

    <!-- Messages -->
    <div id="msg" class="hidden"></div>

    <noscript><p style="color:#f87171; margin-top:20px;">JavaScript is required.</p></noscript>

    <script>
    const API = '{base_url}';
    const PLUGIN_SLUG = 'youtube-subscriber-role';

    // Optional ?guild=<id> tells us the user came from a per-guild verify
    // link an admin shared in their Discord. We use it to (a) show a
    // contextual banner so the user knows which server this is for and
    // (b) automatically clear any existing opt-out (both per-plugin and
    // the guild-wide master) once they're authenticated — so a returning
    // user who'd previously disabled this server doesn't have to find
    // /auth/my_servers to re-enable it.
    const guildId = (() => {{
        try {{
            const v = new URLSearchParams(window.location.search).get('guild');
            return v && /^[0-9]{{5,25}}$/.test(v) ? v : '';
        }} catch (e) {{ return ''; }}
    }})();

    // Preserve the guild context across the Discord OAuth round-trip so
    // an unauth visitor who logs in lands back on this same per-guild URL.
    (function patchLoginHref() {{
        if (!guildId) return;
        const link = document.querySelector('#login-section a.btn-discord');
        if (!link) return;
        const returnTo = '/youtube-subscriber-role/verify?guild=' + encodeURIComponent(guildId);
        link.href = '/auth/login?return_to=' + encodeURIComponent(returnTo);
    }})();

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

    // Gateway-absolute API helper for /auth/* (cookie-authed via the
    // shared rl_session). Same shape as `api()` but doesn't prefix with
    // the plugin's base_url.
    async function gatewayApi(method, path, body) {{
        const opts = {{ method, headers: {{}}, credentials: 'include' }};
        if (body) {{
            opts.headers['Content-Type'] = 'application/json';
            opts.body = JSON.stringify(body);
        }}
        const res = await fetch(path, opts);
        const data = await res.json().catch(() => ({{}}));
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

    function showGuildCtx(text, isWarning) {{
        const el = document.getElementById('guild-ctx');
        document.getElementById('guild-ctx-text').innerHTML = text;
        el.classList.toggle('warn', !!isWarning);
        el.classList.add('show');
    }}

    let isLinked = false;

    // Resolve guildId → display name via the gateway, then clear any
    // opt-out blocking this plugin from assigning roles in that server.
    // Idempotent: clearing rows that don't exist is a no-op on the server.
    async function applyGuildContext() {{
        if (!guildId) return;
        let prefs;
        try {{
            prefs = await gatewayApi('GET', '/auth/preferences?ensure_guild=' + encodeURIComponent(guildId));
        }} catch (e) {{
            // Not a fatal failure for the verify flow — just skip the banner.
            return;
        }}
        const g = (prefs.guilds || []).find(x => x.guild_id === guildId);
        if (!g) {{
            // Either the user isn't in that guild, or the gateway hasn't
            // refreshed their guild list yet. Surface it gently — verify
            // still works; the role just won't apply until they're a member.
            showGuildCtx("You're not in that server yet — join it on Discord, then refresh.", true);
            return;
        }}
        const safeName = (g.guild_name || '(unnamed server)')
            .replace(/[&<>"']/g, c => ({{'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}})[c]);
        const wasDisabled = g.master_optout || (g.plugin_optouts || []).includes(PLUGIN_SLUG);
        // Always clear both — the master toggle wins over per-plugin
        // overrides, so we need to remove it too even if only the
        // per-plugin row was set on this server.
        try {{
            if (g.master_optout) {{
                await gatewayApi('POST', '/auth/preferences', {{
                    guild_id: guildId, plugin: null, enabled: true,
                }});
            }}
            if ((g.plugin_optouts || []).includes(PLUGIN_SLUG)) {{
                await gatewayApi('POST', '/auth/preferences', {{
                    guild_id: guildId, plugin: PLUGIN_SLUG, enabled: true,
                }});
            }}
        }} catch (e) {{
            // Even if the clear failed, still show the banner so the user
            // knows where they are. The role will simply not apply until
            // they fix it manually via /auth/my_servers.
        }}
        const nameHtml = '<span class="gctx-name">' + safeName + '</span>';
        if (wasDisabled) {{
            showGuildCtx(isLinked
                ? 'Enabled YouTube roles for ' + nameHtml + ' — roles apply on the next sync.'
                : 'Enabled YouTube roles for ' + nameHtml + ' — finish linking below to receive roles.');
        }} else {{
            showGuildCtx(isLinked
                ? 'YouTube roles are active in ' + nameHtml + '.'
                : 'Once linked, YouTube roles will apply in ' + nameHtml + '.');
        }}
    }}

    function escHtml(s) {{
        return String(s).replace(/[&<>"']/g, c => ({{'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}})[c]);
    }}

    // Step 1 ("subscribe"): list the YouTube channel(s) this server's roles
    // check against so the user can subscribe straight from here. Public
    // endpoint — runs before sign-in. Best-effort; falls back to generic copy.
    async function loadChannels() {{
        const list = document.getElementById('channel-list');
        if (!list) return;
        try {{
            const url = API + '/verify/channels' + (guildId ? ('?guild=' + encodeURIComponent(guildId)) : '');
            const res = await fetch(url, {{ cache: 'no-store' }});
            const d = await res.json().catch(() => ({{}}));
            renderChannels((d && d.channels) || []);
        }} catch (e) {{
            renderChannels([]);
        }}
    }}

    function renderChannels(channels) {{
        const list = document.getElementById('channel-list');
        if (!list) return;
        if (!channels.length) {{
            list.innerHTML = '<p style="color:#94a3b8; font-size:13px;">' + (guildId
                ? "This server hasn't set its YouTube channel yet. You can still sign in and link below — your role applies once it does."
                : "Open the YouTube channel your server uses, hit Subscribe, then continue below.") + '</p>';
            return;
        }}
        list.innerHTML = channels.map(c => {{
            const id = c.channel_id || '';
            const href = 'https://www.youtube.com/channel/' + encodeURIComponent(id);
            return '<div class="channel-row">' +
                '<span class="channel-meta"><span class="channel-name">YouTube channel</span>' +
                '<span class="channel-id">' + escHtml(id) + '</span></span>' +
                '<a class="channel-open" href="' + href + '" target="_blank" rel="noopener">Subscribe &rarr;</a>' +
            '</div>';
        }}).join('');
    }}

    async function init() {{
        try {{
            const s = await api('GET', '/verify/status');
            isLinked = !!s.linked;
            document.getElementById('logout-btn').classList.remove('hidden');
            if (s.linked) {{
                document.getElementById('linked-discord').textContent = s.display_name;
                document.getElementById('linked-youtube').textContent = 'Connected';
                showSection('linked-section');
                // Visiting the page re-checks your latest subscription data so
                // roles self-correct without an unlink/re-link. Best-effort.
                doRefresh(true);
            }} else {{
                document.getElementById('yt-discord-badge').textContent = s.display_name;
                showSection('youtube-section');
            }}
            // Session is valid — apply the per-guild side effects (if any).
            applyGuildContext();
        }} catch (e) {{
            showSection('login-section');
        }}
    }}

    async function doLogout() {{
        clearMsg();
        try {{
            await api('POST', '/verify/logout');
            document.getElementById('logout-btn').classList.add('hidden');
            showSection('login-section');
            showMsg('Logged out.', 'success');
        }} catch (e) {{ showMsg(e.message, 'error'); }}
    }}

    // Nudge the server to re-fetch this user's YouTube data ahead of schedule.
    // `silent` is used for the automatic call on page load — it shows the
    // working/result note but stays quiet on transient errors. The explicit
    // "Re-check now" button passes false so failures surface.
    let refreshing = false;
    async function doRefresh(silent) {{
        const note = document.getElementById('refresh-note');
        if (refreshing) return;
        refreshing = true;
        note.style.color = '#94a3b8';
        note.textContent = 'Checking your latest subscription status…';
        try {{
            const r = await api('POST', '/verify/refresh');
            note.style.color = '#4ade80';
            note.textContent = r.refreshed
                ? '✓ Re-checking now — your roles update within a minute.'
                : '✓ Your status is already up to date.';
        }} catch (e) {{
            if (silent) {{
                note.textContent = '';
            }} else {{
                note.style.color = '#f87171';
                note.textContent = 'Could not refresh right now — try again shortly.';
            }}
        }} finally {{
            refreshing = false;
        }}
    }}

    async function doUnlink() {{
        clearMsg();
        if (!confirm('Unlink your account? You will lose all assigned roles.')) return;
        try {{
            await api('POST', '/verify/unlink');
            document.getElementById('yt-discord-badge').textContent = document.getElementById('linked-discord').textContent;
            showSection('youtube-section');
            showMsg('Account unlinked.', 'success');
        }} catch (e) {{ showMsg(e.message, 'error'); }}
    }}

    loadChannels();
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

pub async fn login(State(_state): State<Arc<AppState>>) -> Response {
    let return_to = "/youtube-subscriber-role/verify";
    let url = format!(
        "/auth/login?return_to={}",
        urlencoding::encode(return_to),
    );
    Redirect::temporary(&url).into_response()
}

#[derive(Deserialize)]
pub struct CallbackQuery {
    pub code: Option<String>,
    pub state: String,
    pub error: Option<String>,
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

    // Resolve guild membership from the Auth Gateway (over HTTP), then filter
    // role_links locally to find which YouTube channels matter for this user.
    let guild_ids = crate::services::auth_gateway::fetch_user_guild_ids(
        &state.http,
        &state.config.auth_gateway_url,
        &state.config.internal_api_key,
        &discord_id,
    )
    .await
    .unwrap_or_default();

    // Whether any role link in the user's guilds uses a rule that depends on
    // the member's own channel statistics. Plain "is subscribed" roles (the
    // common case) don't, so we can skip the extra channels.list API call per
    // user — which halves YouTube quota spend during a mass-verify spike.
    // Default to eager on error.
    let needs_channel_stats = if guild_ids.is_empty() {
        false
    } else {
        let trees: Vec<Value> = sqlx::query_scalar(
            "SELECT rule_tree FROM role_links WHERE guild_id = ANY($1)",
        )
        .bind(&guild_ids)
        .fetch_all(&state.pool)
        .await
        .unwrap_or_default();
        // On a DB error we get an empty list → not eager; but the more likely
        // failure (a malformed tree) is handled per-row by defaulting to false.
        trees.iter().any(|raw| {
            serde_json::from_value::<crate::models::rule::RuleTree>(raw.clone())
                .map(|t| t.needs_channel_cache())
                .unwrap_or(false)
        })
    };

    // Inline subscription check using the access token we just obtained. This is
    // the key to instant roles: instead of seeding next_check_at = now() and
    // waiting for the single, rate-limited background worker to reach this row
    // (which can lag for hours when hundreds verify at once after an @everyone
    // ping), we check the user's subscription right here, write the real result,
    // and sync their roles before this request returns. Best-effort: on any API
    // error we fall back to seeding the row for the worker to retry.
    let mut inline_status_known = false;
    if !guild_ids.is_empty() {
        let channel_ids: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT rl.channel_id FROM role_links rl \
             WHERE rl.guild_id = ANY($1) AND rl.channel_id IS NOT NULL",
        )
        .bind(&guild_ids)
        .fetch_all(&state.pool)
        .await
        .unwrap_or_default();

        for channel_id in &channel_ids {
            // The user is actively watching this page, so spend from the
            // interactive quota reserve (which background re-checks can't touch).
            // If even that is exhausted, skip the inline call and seed the row so
            // the worker confirms it shortly — the role still lands, just not
            // before the page reloads.
            if let Outcome::Exhausted { .. } = state.quota.acquire(Class::Interactive).await {
                let _ = sqlx::query(
                    "INSERT INTO subscription_cache (discord_id, channel_id, next_check_at) \
                     VALUES ($1, $2, now()) \
                     ON CONFLICT (discord_id, channel_id) DO UPDATE SET next_check_at = now()",
                )
                .bind(&discord_id)
                .bind(channel_id)
                .execute(&state.pool)
                .await;
                continue;
            }
            match state
                .youtube_client
                .check_subscription(&tokens.access_token, channel_id)
                .await
            {
                Ok(result) => {
                    // If they're not subscribed yet (linked before subscribing, or
                    // YouTube's API hasn't surfaced a just-made subscription), start
                    // the worker's fast re-check cadence so the role lands within a
                    // minute or two with no user action; otherwise use the normal
                    // post-link interval.
                    let next_check = chrono::Utc::now()
                        + chrono::Duration::seconds(if result.is_subscribed {
                            INITIAL_RECHECK_SECS
                        } else {
                            crate::tasks::refresh_worker::FAST_RETRY_SECS
                        });
                    let _ = sqlx::query(
                        "INSERT INTO subscription_cache \
                         (discord_id, channel_id, is_subscribed, subscribed_at, checked_at, next_check_at, check_failures) \
                         VALUES ($1, $2, $3, $4, now(), $5, 0) \
                         ON CONFLICT (discord_id, channel_id) DO UPDATE SET \
                           is_subscribed = $3, subscribed_at = $4, checked_at = now(), \
                           next_check_at = $5, check_failures = 0",
                    )
                    .bind(&discord_id)
                    .bind(channel_id)
                    .bind(result.is_subscribed)
                    .bind(result.subscribed_at)
                    .bind(next_check)
                    .execute(&state.pool)
                    .await;
                    inline_status_known = true;
                }
                Err(e) => {
                    tracing::warn!(
                        discord_id, channel_id,
                        "Inline subscription check failed; deferring to worker: {e}"
                    );
                    let _ = sqlx::query(
                        "INSERT INTO subscription_cache (discord_id, channel_id, next_check_at) \
                         VALUES ($1, $2, now()) \
                         ON CONFLICT (discord_id, channel_id) DO UPDATE SET next_check_at = now()",
                    )
                    .bind(&discord_id)
                    .bind(channel_id)
                    .execute(&state.pool)
                    .await;
                }
            }
        }
    }

    // Channel stats power both stat-based conditions and the subscribers-list
    // page. Fetch them inline only when a condition needs them (so the inline
    // role sync below evaluates correctly); otherwise seed a deferred row so the
    // subscribers page fills in later without spending quota during the spike.
    if needs_channel_stats {
        let granted =
            matches!(state.quota.acquire(Class::Interactive).await, Outcome::Granted);
        match (granted, if granted {
            Some(state.youtube_client.fetch_channel_stats(&tokens.access_token).await)
        } else {
            None
        }) {
            (true, Some(Ok(stats))) => {
                // Capture the user's own channel id so the worker can refresh
                // their stats via the batched (50:1) path from now on.
                if let Some(ref cid) = stats.channel_id {
                    let _ = sqlx::query(
                        "UPDATE linked_accounts SET youtube_channel_id = $1 WHERE discord_id = $2",
                    )
                    .bind(cid)
                    .bind(&discord_id)
                    .execute(&state.pool)
                    .await;
                }
                let cc_next =
                    chrono::Utc::now() + chrono::Duration::seconds(INITIAL_RECHECK_SECS * 2);
                let _ = sqlx::query(
                    "INSERT INTO channel_cache \
                     (discord_id, subscriber_count, view_count, video_count, channel_created_at, \
                      hidden_subscribers, country, custom_url, checked_at, next_check_at, check_failures) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, now(), $9, 0) \
                     ON CONFLICT (discord_id) DO UPDATE SET \
                       subscriber_count = $2, view_count = $3, video_count = $4, \
                       channel_created_at = $5, hidden_subscribers = $6, country = $7, \
                       custom_url = $8, checked_at = now(), next_check_at = $9, check_failures = 0",
                )
                .bind(&discord_id)
                .bind(stats.subscriber_count)
                .bind(stats.view_count)
                .bind(stats.video_count)
                .bind(stats.channel_created_at)
                .bind(stats.hidden_subscriber_count)
                .bind(&stats.country)
                .bind(&stats.custom_url)
                .bind(cc_next)
                .execute(&state.pool)
                .await;
            }
            (_, maybe_err) => {
                if let Some(Err(e)) = maybe_err {
                    tracing::warn!(discord_id, "Inline channel stats fetch failed; deferring: {e}");
                }
                // Either the fetch failed or interactive budget was exhausted —
                // seed a due row for the worker to fill in.
                let _ = sqlx::query(
                    "INSERT INTO channel_cache (discord_id, next_check_at) VALUES ($1, now()) \
                     ON CONFLICT (discord_id) DO NOTHING",
                )
                .bind(&discord_id)
                .execute(&state.pool)
                .await;
            }
        }
    } else {
        let deferred =
            chrono::Utc::now() + chrono::Duration::hours(DEFERRED_CHANNEL_STATS_HOURS);
        let _ = sqlx::query(
            "INSERT INTO channel_cache (discord_id, next_check_at) VALUES ($1, $2) \
             ON CONFLICT (discord_id) DO NOTHING",
        )
        .bind(&discord_id)
        .bind(deferred)
        .execute(&state.pool)
        .await;
    }

    // Apply roles now. When we know the live subscription status, sync inline so
    // the role is granted before the page reloads — and so a burst of linkers is
    // handled in parallel across request tasks rather than funnelled through the
    // single background sync worker. Fall back to the worker event otherwise.
    if inline_status_known {
        if let Err(e) = sync::sync_for_player(&discord_id, &state).await {
            tracing::error!(discord_id, "Inline role sync after link failed: {e}");
            let _ = state
                .player_sync_tx
                .send(PlayerSyncEvent::AccountLinked {
                    discord_id: discord_id.clone(),
                })
                .await;
        }
    } else {
        let _ = state
            .player_sync_tx
            .send(PlayerSyncEvent::AccountLinked {
                discord_id: discord_id.clone(),
            })
            .await;
    }

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

#[derive(Deserialize)]
pub struct VerifyChannelsQuery {
    pub guild: Option<String>,
}

/// Public (no auth): the YouTube channel(s) this guild's roles require a
/// subscription to, so the verify page can render its "subscribe" step before
/// the user signs in. Returns only the channel IDs the admin already
/// advertises ("subscribe to our channel") — nothing sensitive. An invalid or
/// missing `guild` yields an empty list, so the page falls back to generic copy
/// without a wasted query.
pub async fn verify_channels(
    State(state): State<Arc<AppState>>,
    Query(q): Query<VerifyChannelsQuery>,
) -> Result<Json<Value>, AppError> {
    let guild_id = q.guild.unwrap_or_default();
    let valid =
        (5..=25).contains(&guild_id.len()) && guild_id.bytes().all(|b| b.is_ascii_digit());
    if !valid {
        return Ok(Json(json!({ "channels": [] })));
    }

    let channel_ids: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT channel_id FROM role_links \
         WHERE guild_id = $1 AND channel_id IS NOT NULL \
         ORDER BY channel_id LIMIT 50",
    )
    .bind(&guild_id)
    .fetch_all(&state.pool)
    .await?;

    let channels: Vec<Value> = channel_ids
        .into_iter()
        .map(|id| json!({ "channel_id": id }))
        .collect();
    Ok(Json(json!({ "channels": channels })))
}

pub async fn logout(jar: CookieJar) -> (CookieJar, Json<Value>) {
    let cookie = Cookie::build(SESSION_COOKIE)
        .path("/");
    let jar = jar.remove(cookie);
    (jar, Json(json!({"success": true})))
}

/// Per-user floor between member-triggered re-checks. The refresh worker
/// already rate-limits API calls; this just stops a page reload loop from
/// re-forcing a check the worker only just completed and burning quota.
const REFRESH_COOLDOWN_SECS: f64 = 60.0;

/// Member-triggered "re-check my data now". When a linked user opens the
/// verify page the page calls this so their YouTube subscription + channel
/// stats get re-fetched ahead of schedule and their roles are corrected —
/// no unlink/re-link needed. We don't fetch inline (that would bypass the
/// rate limiter); we just bring the worker's `next_check_at` forward for
/// rows that aren't already fresh, and the worker re-syncs roles after it
/// re-checks. Idempotent: forcing a row that's already due is a no-op.
pub async fn refresh(
    State(state): State<Arc<AppState>>,
    jar: CookieJar,
) -> Result<Json<Value>, AppError> {
    let (discord_id, _) = get_session(&jar, &state.config.session_secret)?;

    let subs = sqlx::query(
        "UPDATE subscription_cache SET next_check_at = now() \
         WHERE discord_id = $1 \
           AND (checked_at IS NULL OR checked_at < now() - make_interval(secs => $2))",
    )
    .bind(&discord_id)
    .bind(REFRESH_COOLDOWN_SECS)
    .execute(&state.pool)
    .await?
    .rows_affected();

    let chan = sqlx::query(
        "UPDATE channel_cache SET next_check_at = now() \
         WHERE discord_id = $1 \
           AND (checked_at IS NULL OR checked_at < now() - make_interval(secs => $2))",
    )
    .bind(&discord_id)
    .bind(REFRESH_COOLDOWN_SECS)
    .execute(&state.pool)
    .await?
    .rows_affected();

    Ok(Json(json!({ "refreshed": subs + chan > 0 })))
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
