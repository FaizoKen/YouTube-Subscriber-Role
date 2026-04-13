use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use axum::Json;
use axum_extra::extract::CookieJar;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::AppError;
use crate::services::session::verify_session;
use crate::AppState;

#[derive(Deserialize)]
pub struct SubscribersQuery {
    page: Option<i64>,
    per_page: Option<i64>,
    sort: Option<String>,
    order: Option<String>,
    search: Option<String>,
}

fn sort_column(key: &str) -> Option<&'static str> {
    match key {
        "discord_name" => Some("la.discord_name"),
        "custom_url" => Some("cc.custom_url"),
        "subscriber_count" => Some("cc.subscriber_count"),
        "view_count" => Some("cc.view_count"),
        "video_count" => Some("cc.video_count"),
        "channel_age" => Some("cc.channel_created_at"),
        "country" => Some("cc.country"),
        "linked_at" => Some("la.linked_at"),
        "checked_at" => Some("cc.checked_at"),
        _ => None,
    }
}

pub fn render_subscribers_page(base_url: &str) -> String {
    format!(
        r##"<!DOCTYPE html>
<html>
<head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <title>YouTube Roles - Subscriber List</title>
    <link rel="icon" href="{base_url}/favicon.ico" type="image/x-icon">
    <meta name="description" content="View verified YouTube subscribers in this Discord Server.">
    <meta name="theme-color" content="#ff0000">
    <style>
        * {{ box-sizing: border-box; margin: 0; padding: 0; }}
        body {{ font-family: system-ui, -apple-system, sans-serif; max-width: 1060px; margin: 0 auto; padding: 32px 20px; background: #0e1525; color: #c8ccd4; min-height: 100vh; }}
        .header {{ margin-bottom: 24px; }}
        .header-top {{ display: flex; align-items: center; gap: 10px; margin-bottom: 6px; justify-content: space-between; }}
        .header-title {{ display: flex; align-items: center; gap: 10px; }}
        .header-top h1 {{ color: #ff4444; font-size: 24px; }}
        .powered {{ font-size: 11px; color: #64748b; background: #1e293b; padding: 2px 8px; border-radius: 4px; }}
        .powered a {{ color: #74b9ff; text-decoration: none; }}
        .guild-name {{ color: #e2e8f0; font-size: 18px; font-weight: 600; }}
        .guild-label {{ color: #64748b; font-size: 13px; margin-top: 2px; }}

        .card {{ background: #161d2e; padding: 22px; border-radius: 10px; border: 1px solid #1e2a3d; }}
        .msg {{ padding: 10px 14px; border-radius: 6px; margin: 12px 0; font-size: 13px; line-height: 1.5; }}
        .msg-error {{ background: #1c0a0a; color: #fca5a5; border: 1px solid #7f1d1d; }}
        .hidden {{ display: none !important; }}

        .toolbar {{ display: flex; align-items: center; justify-content: space-between; flex-wrap: wrap; gap: 10px; margin-bottom: 16px; }}
        .search-wrap {{ position: relative; flex: 1; max-width: 340px; }}
        .search-wrap svg {{ position: absolute; left: 10px; top: 50%; transform: translateY(-50%); color: #475569; pointer-events: none; }}
        .search-wrap input {{ width: 100%; padding: 8px 12px 8px 34px; font-size: 13px; border-radius: 6px; border: 1px solid #2a3548; background: #0e1525; color: #e0e0e0; font-family: inherit; transition: border-color .15s; }}
        .search-wrap input:focus {{ outline: none; border-color: #3b82f6; }}
        .search-hint {{ color: #475569; font-size: 11px; margin-top: 4px; }}
        .badge {{ display: inline-flex; align-items: center; gap: 5px; padding: 4px 12px; border-radius: 20px; font-size: 12px; font-weight: 500; background: #1e293b; color: #94a3b8; border: 1px solid #334155; white-space: nowrap; }}

        .table-wrap {{ overflow-x: auto; }}
        table {{ width: 100%; border-collapse: collapse; font-size: 13px; }}
        th, td {{ padding: 9px 12px; text-align: left; white-space: nowrap; }}
        th {{ color: #64748b; font-weight: 600; font-size: 11px; text-transform: uppercase; letter-spacing: 0.5px; border-bottom: 2px solid #1e2a3d; cursor: pointer; user-select: none; transition: color .15s; }}
        th:hover {{ color: #94a3b8; }}
        th.sorted-asc::after {{ content: ' \25B2'; font-size: 9px; }}
        th.sorted-desc::after {{ content: ' \25BC'; font-size: 9px; }}
        td {{ border-bottom: 1px solid #111827; }}
        tr:hover td {{ background: #1a2236; }}
        .col-discord {{ color: #7c85f5; font-size: 12px; }}
        .col-discord a {{ color: #7c85f5; text-decoration: none; }}
        .col-discord a:hover {{ text-decoration: underline; }}
        .col-channel a {{ color: #ff6666; text-decoration: none; }}
        .col-channel a:hover {{ text-decoration: underline; }}
        .col-region {{ color: #94a3b8; }}
        .col-num {{ color: #ff4444; text-align: right; }}
        th.col-num {{ text-align: right; }}
        .col-date {{ color: #64748b; font-size: 12px; }}
        .col-hidden {{ color: #475569; font-style: italic; }}

        .empty-state {{ text-align: center; padding: 40px 20px; color: #475569; }}
        .empty-state p {{ font-size: 14px; margin-bottom: 4px; }}
        .empty-state .hint {{ font-size: 12px; }}

        .pagination {{ display: flex; align-items: center; justify-content: center; gap: 8px; margin-top: 16px; font-size: 13px; }}
        .pagination button {{ padding: 6px 14px; border-radius: 6px; border: 1px solid #2a3548; background: #0e1525; color: #c8ccd4; cursor: pointer; font-family: inherit; font-size: 13px; transition: all .15s; }}
        .pagination button:hover:not(:disabled) {{ background: #1e293b; border-color: #3b82f6; }}
        .pagination button:disabled {{ opacity: 0.3; cursor: not-allowed; }}
        .pagination .page-info {{ color: #64748b; }}

        .logout-form {{ margin: 0; }}
        .logout-btn {{ padding: 6px 14px; border-radius: 6px; border: 1px solid #2a3548; background: #0e1525; color: #c8ccd4; cursor: pointer; font-family: inherit; font-size: 12px; transition: all .15s; }}
        .logout-btn:hover {{ background: #1e293b; border-color: #ef4444; color: #fca5a5; }}

        .login-btn {{ display: inline-block; padding: 10px 22px; border-radius: 6px; background: #5865f2; color: #fff; text-decoration: none; font-weight: 600; font-size: 14px; font-family: inherit; transition: background .15s; }}
        .login-btn:hover {{ background: #4752c4; }}
    </style>
</head>
<body>
    <div class="header">
        <div class="header-top">
            <div class="header-title">
                <h1>YouTube Roles</h1>
                <span class="powered">Powered by <a href="https://rolelogic.faizo.net" target="_blank" rel="noopener">RoleLogic</a></span>
            </div>
            <form id="logout-form" class="logout-form" method="POST" action="/auth/logout">
                <button type="submit" class="logout-btn">Logout</button>
            </form>
        </div>
        <p class="guild-name" id="guild-name">Verified Subscribers</p>
        <p class="guild-label" id="guild-label">Loading guild info...</p>
    </div>

    <div id="loading" class="card"><p style="color:#64748b;">Loading subscriber data...</p></div>
    <div id="error-msg" class="hidden"></div>

    <div id="login-prompt" class="card hidden" style="text-align:center;">
        <p style="color:#e2e8f0; font-size:15px; margin-bottom:6px;">You are not signed in.</p>
        <p style="color:#64748b; font-size:13px; margin-bottom:18px;">Sign in with Discord to view this server's verified YouTube subscribers.</p>
        <a id="login-link" class="login-btn" href="#">Login with Discord</a>
    </div>

    <div id="content" class="hidden">
        <div class="card">
            <div class="toolbar">
                <div>
                    <div class="search-wrap">
                        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="11" cy="11" r="8"/><line x1="21" y1="21" x2="16.65" y2="16.65"/></svg>
                        <input type="text" id="search" placeholder="Search subscribers..." />
                    </div>
                    <p class="search-hint">Search by Discord name, Discord ID, channel URL, or country</p>
                </div>
                <span class="badge" id="sub-count"></span>
            </div>
            <div class="table-wrap">
                <table>
                    <thead>
                        <tr>
                            <th data-key="discord_name">Discord</th>
                            <th data-key="custom_url">Channel</th>
                            <th data-key="subscriber_count" class="col-num">Subscribers</th>
                            <th data-key="view_count" class="col-num">Views</th>
                            <th data-key="video_count" class="col-num">Videos</th>
                            <th data-key="channel_age">Channel Age</th>
                            <th data-key="country">Country</th>
                            <th data-key="linked_at">Linked</th>
                            <th data-key="checked_at">Last Checked</th>
                        </tr>
                    </thead>
                    <tbody id="tbody"></tbody>
                </table>
            </div>
            <div id="empty-state" class="empty-state hidden">
                <p>No subscribers found</p>
                <p class="hint" id="empty-hint">Try a different search term</p>
            </div>
            <div class="pagination" id="pagination">
                <button id="btn-prev" onclick="goPage(state.page-1)">Prev</button>
                <span class="page-info" id="page-info"></span>
                <button id="btn-next" onclick="goPage(state.page+1)">Next</button>
            </div>
        </div>
    </div>

    <script>
    const parts = window.location.pathname.split('/').filter(Boolean);
    const guildId = parts[parts.indexOf('subscribers') + 1] || '';
    const PER_PAGE = 20;
    const NUM_COLS = ['subscriber_count','view_count','video_count'];

    (function setupAuthLinks() {{
        const returnTo = window.location.pathname + window.location.search;
        const form = document.getElementById('logout-form');
        if (form) form.action = '/auth/logout?return_to=' + encodeURIComponent(returnTo);
        const loginLink = document.getElementById('login-link');
        if (loginLink) loginLink.href = '/auth/login?return_to=' + encodeURIComponent(returnTo);
    }})();

    const state = {{ page: 1, sort: 'subscriber_count', order: 'desc', search: '', total: 0 }};
    let debounceTimer = null;

    function formatAge(iso) {{
        if (!iso) return '-';
        const diff = Date.now() - new Date(iso).getTime();
        const days = Math.floor(diff / 86400000);
        if (days < 1) return '<1d';
        if (days < 30) return days + 'd';
        const months = Math.floor(days / 30.44);
        if (months < 12) return months + 'mo';
        const years = Math.floor(months / 12);
        const rem = months % 12;
        return rem > 0 ? years + 'y ' + rem + 'mo' : years + 'y';
    }}

    function timeAgo(iso) {{
        if (!iso) return '-';
        const diff = Date.now() - new Date(iso).getTime();
        const mins = Math.floor(diff / 60000);
        if (mins < 1) return 'just now';
        if (mins < 60) return mins + 'm ago';
        const hrs = Math.floor(mins / 60);
        if (hrs < 24) return hrs + 'h ago';
        const days = Math.floor(hrs / 24);
        return days + 'd ago';
    }}

    function fmtNum(n) {{
        if (n == null) return '-';
        return Number(n).toLocaleString();
    }}

    function esc(s) {{
        const d = document.createElement('div');
        d.textContent = s;
        return d.innerHTML;
    }}

    function render(subscribers) {{
        const tbody = document.getElementById('tbody');
        const emptyEl = document.getElementById('empty-state');
        tbody.innerHTML = '';
        if (subscribers.length === 0) {{
            emptyEl.classList.remove('hidden');
            document.getElementById('empty-hint').textContent = state.search
                ? 'No results for "' + state.search + '"'
                : 'No verified subscribers in this guild yet';
        }} else {{
            emptyEl.classList.add('hidden');
        }}
        subscribers.forEach(s => {{
            const tr = document.createElement('tr');
            const discordText = s.discord_name || s.discord_id || '-';
            const discord = s.discord_id
                ? '<a href="https://discord.com/users/' + esc(s.discord_id) + '" target="_blank" rel="noopener">' + esc(discordText) + '</a>'
                : esc(discordText);
            const rawUrl = s.custom_url || '';
            const handle = rawUrl.startsWith('@') ? rawUrl : '@' + rawUrl;
            const urlPath = rawUrl.startsWith('@') ? rawUrl : '@' + rawUrl;
            const channel = s.custom_url
                ? '<a href="https://youtube.com/' + esc(urlPath) + '" target="_blank" rel="noopener">' + esc(handle) + '</a>'
                : '-';
            const subs = s.hidden_subscribers
                ? '<span class="col-hidden">Hidden</span>'
                : fmtNum(s.subscriber_count);
            tr.innerHTML =
                '<td class="col-discord">' + discord + '</td>' +
                '<td class="col-channel">' + channel + '</td>' +
                '<td class="col-num">' + subs + '</td>' +
                '<td class="col-num">' + fmtNum(s.view_count) + '</td>' +
                '<td class="col-num">' + fmtNum(s.video_count) + '</td>' +
                '<td class="col-date">' + formatAge(s.channel_created_at) + '</td>' +
                '<td class="col-region">' + esc(s.country || '-') + '</td>' +
                '<td class="col-date">' + timeAgo(s.linked_at) + '</td>' +
                '<td class="col-date">' + timeAgo(s.checked_at) + '</td>';
            tbody.appendChild(tr);
        }});
    }}

    function updatePagination() {{
        const totalPages = Math.max(1, Math.ceil(state.total / PER_PAGE));
        document.getElementById('sub-count').textContent = state.total + ' subscriber' + (state.total !== 1 ? 's' : '');
        document.getElementById('page-info').textContent = 'Page ' + state.page + ' of ' + totalPages;
        document.getElementById('btn-prev').disabled = state.page <= 1;
        document.getElementById('btn-next').disabled = state.page >= totalPages;
        document.getElementById('pagination').classList.toggle('hidden', state.total <= PER_PAGE);
    }}

    function updateSortUI() {{
        document.querySelectorAll('th[data-key]').forEach(h => {{
            h.classList.remove('sorted-asc', 'sorted-desc');
            if (h.dataset.key === state.sort) h.classList.add('sorted-' + state.order);
        }});
    }}

    async function fetchData() {{
        const params = new URLSearchParams({{
            page: state.page, per_page: PER_PAGE,
            sort: state.sort, order: state.order
        }});
        if (state.search) params.set('search', state.search);
        const res = await fetch('{base_url}/subscribers/' + encodeURIComponent(guildId) + '/data?' + params, {{ credentials: 'same-origin' }});
        if (res.status === 401) {{
            const data = await res.json().catch(() => ({{}}));
            const err = new Error(data.error || 'You are not signed in.');
            err.authRequired = true;
            throw err;
        }}
        if (!res.ok) {{
            const data = await res.json().catch(() => ({{}}));
            throw new Error(data.error || 'Failed to load subscriber data');
        }}
        return res.json();
    }}

    async function load() {{
        try {{
            const data = await fetchData();
            state.total = data.total;
            if (data.guild_name) {{
                document.getElementById('guild-name').textContent = data.guild_name;
                document.getElementById('guild-label').textContent = 'Verified YouTube subscribers';
                document.title = data.guild_name + ' - YouTube Roles';
            }} else {{
                document.getElementById('guild-name').textContent = 'Verified Subscribers';
                document.getElementById('guild-label').textContent = 'YouTube subscriber list';
            }}
            render(data.subscribers);
            updatePagination();
            updateSortUI();
            document.getElementById('loading').classList.add('hidden');
            document.getElementById('content').classList.remove('hidden');
            document.getElementById('error-msg').classList.add('hidden');
        }} catch (e) {{
            document.getElementById('loading').classList.add('hidden');
            if (e && e.authRequired) {{
                document.getElementById('login-prompt').classList.remove('hidden');
                document.getElementById('error-msg').classList.add('hidden');
                document.getElementById('content').classList.add('hidden');
                const form = document.getElementById('logout-form');
                if (form) form.classList.add('hidden');
                document.getElementById('guild-name').textContent = 'Verified Subscribers';
                document.getElementById('guild-label').textContent = 'Sign in to view this server\'s subscriber list';
            }} else {{
                document.getElementById('guild-name').textContent = 'Verified Subscribers';
                document.getElementById('guild-label').textContent = '';
                const el = document.getElementById('error-msg');
                el.className = 'msg msg-error';
                el.textContent = e.message;
                el.classList.remove('hidden');
            }}
        }}
    }}

    function goPage(p) {{
        const totalPages = Math.max(1, Math.ceil(state.total / PER_PAGE));
        state.page = Math.max(1, Math.min(p, totalPages));
        load();
    }}

    document.querySelectorAll('th[data-key]').forEach(th => {{
        th.addEventListener('click', () => {{
            const key = th.dataset.key;
            if (state.sort === key) {{
                state.order = state.order === 'asc' ? 'desc' : 'asc';
            }} else {{
                state.sort = key;
                state.order = NUM_COLS.includes(key) ? 'desc' : 'asc';
            }}
            state.page = 1;
            load();
        }});
    }});

    document.getElementById('search').addEventListener('input', e => {{
        clearTimeout(debounceTimer);
        debounceTimer = setTimeout(() => {{
            state.search = e.target.value.trim();
            state.page = 1;
            load();
        }}, 300);
    }});

    load();
    </script>
</body>
</html>"##
    )
}

pub async fn subscribers_page(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    (
        [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
        state.subscribers_html.clone(),
    )
}

// ---------------------------------------------------------------------------
// Auth Gateway helpers (cookie-forwarding, user-facing)
// ---------------------------------------------------------------------------

/// Send an authenticated GET to the Auth Gateway, forwarding the viewer's
/// `rl_session` cookie. Returns parsed JSON on 2xx, maps 401 to a specific
/// error, and treats anything else as Internal.
async fn auth_gateway_get(
    state: &Arc<AppState>,
    path_and_query: &str,
    session_cookie_value: &str,
) -> Result<Value, AppError> {
    let url = format!("{}{path_and_query}", state.config.auth_gateway_url);

    // Re-encode so the gateway's parse_encoded round-trips correctly.
    let outgoing = axum_extra::extract::cookie::Cookie::build((
        "rl_session",
        session_cookie_value.to_string(),
    ))
    .build();
    let cookie_header = outgoing.encoded().to_string();

    let cookie_len = session_cookie_value.len();
    let cookie_fp = if cookie_len >= 12 {
        format!(
            "{}…{}",
            &session_cookie_value[..6],
            &session_cookie_value[cookie_len - 6..]
        )
    } else {
        "<short>".to_string()
    };
    tracing::debug!(
        url = %url,
        cookie_len,
        cookie_fp = %cookie_fp,
        "auth_gateway_get: forwarding cookie to gateway"
    );

    let resp = state
        .http
        .get(&url)
        .header(axum::http::header::COOKIE, cookie_header)
        .send()
        .await
        .map_err(|e| {
            tracing::error!(error = %e, url = %url, "Auth Gateway request failed");
            AppError::Internal(format!("Auth Gateway unreachable: {e}"))
        })?;

    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        tracing::error!(
            url = %url,
            "Auth Gateway rejected the forwarded rl_session cookie"
        );
        return Err(AppError::UnauthorizedWith(format!(
            "The Auth Gateway at {} rejected the session cookie. \
             Most likely AUTH_GATEWAY_URL points at a different gateway, \
             or SESSION_SECRETs don't match.",
            state.config.auth_gateway_url
        )));
    }
    if !status.is_success() {
        let body_text = resp.text().await.unwrap_or_default();
        tracing::error!(status = %status, url = %url, body = %body_text, "Auth Gateway returned error");
        return Err(AppError::Internal(format!(
            "Auth Gateway returned {status}"
        )));
    }

    resp.json::<Value>().await.map_err(|e| {
        tracing::error!(error = %e, "Failed to parse Auth Gateway response");
        AppError::Internal(format!("Auth Gateway parse error: {e}"))
    })
}

/// Returns `(is_member, is_manager)`.
async fn fetch_guild_permission(
    state: &Arc<AppState>,
    guild_id: &str,
    session_cookie_value: &str,
) -> Result<(bool, bool), AppError> {
    let path = format!(
        "/auth/guild_permission?guild_id={}",
        urlencoding::encode(guild_id)
    );
    let body = auth_gateway_get(state, &path, session_cookie_value).await?;

    let is_member = body
        .get("is_member")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let is_manager = body
        .get("is_manager")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    Ok((is_member, is_manager))
}

/// Returns `(member_discord_ids, optional_guild_name)`.
async fn fetch_guild_members(
    state: &Arc<AppState>,
    guild_id: &str,
    session_cookie_value: &str,
) -> Result<(Vec<String>, Option<String>), AppError> {
    let path = format!(
        "/auth/guild_members?guild_id={}",
        urlencoding::encode(guild_id)
    );
    let body = auth_gateway_get(state, &path, session_cookie_value).await?;

    let discord_ids: Vec<String> = body
        .get("discord_ids")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let guild_name = body
        .get("guild_name")
        .and_then(|v| v.as_str())
        .map(String::from);

    Ok((discord_ids, guild_name))
}

// ---------------------------------------------------------------------------
// Data endpoint
// ---------------------------------------------------------------------------

pub async fn subscribers_data(
    State(state): State<Arc<AppState>>,
    Path(guild_id): Path<String>,
    jar: CookieJar,
    Query(query): Query<SubscribersQuery>,
) -> Result<Json<Value>, AppError> {
    // 1. Require a valid session cookie.
    let session_cookie = jar.get("rl_session").ok_or_else(|| {
        tracing::info!(guild_id, "subscribers_data: no rl_session cookie");
        AppError::UnauthorizedWith(
            "No session cookie found. Please log in.".into(),
        )
    })?;

    let cookie_value = session_cookie.value();
    let cookie_len = cookie_value.len();
    let cookie_fp = if cookie_len >= 12 {
        format!("{}…{}", &cookie_value[..6], &cookie_value[cookie_len - 6..])
    } else {
        "<short>".to_string()
    };

    let (viewer_discord_id, _) =
        verify_session(cookie_value, &state.config.session_secret).ok_or_else(|| {
            tracing::warn!(
                guild_id,
                cookie_len,
                cookie_fp = %cookie_fp,
                "subscribers_data: session verification failed"
            );
            AppError::UnauthorizedWith(format!(
                "Session cookie present ({cookie_len} bytes, fp={cookie_fp}) but verification \
                 failed. SESSION_SECRET may not match the Auth Gateway, or cookie expired."
            ))
        })?;

    tracing::debug!(guild_id, viewer = %viewer_discord_id, "subscribers_data: session verified");

    // 2. Check guild has role links + fetch view_permission.
    let guild_row: Option<(bool, String)> = sqlx::query_as(
        "SELECT \
           EXISTS(SELECT 1 FROM role_links WHERE guild_id = $1) AS has_link, \
           COALESCE( \
             (SELECT view_permission FROM guild_settings WHERE guild_id = $1), \
             'members' \
           ) AS view_permission",
    )
    .bind(&guild_id)
    .fetch_optional(&state.pool)
    .await?;

    let (has_link, view_permission) =
        guild_row.unwrap_or((false, "members".to_string()));
    if !has_link {
        return Err(AppError::NotFound(
            "No subscriber list is configured for this server.".into(),
        ));
    }
    let members_allowed = view_permission == "members";

    // 3. Ask Auth Gateway for guild membership and permissions.
    let (_, is_manager) =
        fetch_guild_permission(&state, &guild_id, session_cookie.value()).await?;

    let (member_ids, ag_guild_name) =
        fetch_guild_members(&state, &guild_id, session_cookie.value()).await?;

    if member_ids.is_empty() {
        return Err(AppError::Forbidden(
            "You must be a member of this server to view its subscriber list.".into(),
        ));
    }

    if !members_allowed && !is_manager {
        tracing::debug!(
            guild_id,
            viewer = %viewer_discord_id,
            "subscribers_data: managers-only policy, viewer is not a manager"
        );
        return Err(AppError::Forbidden(
            "Only server managers can view this subscriber list.".into(),
        ));
    }

    // 4. Query subscriber data.
    let page = query.page.unwrap_or(1).max(1);
    let per_page = query.per_page.unwrap_or(20).clamp(1, 100);
    let offset = (page - 1) * per_page;

    let order_col = query
        .sort
        .as_deref()
        .and_then(sort_column)
        .unwrap_or("cc.subscriber_count");
    let order_dir = match query.order.as_deref() {
        Some("asc") => "ASC",
        _ => "DESC",
    };

    let search = query.search.as_deref().unwrap_or("").trim();
    let has_search = !search.is_empty();
    let search_pattern = format!("%{search}%");

    let sql = format!(
        "SELECT la.discord_id, \
                la.discord_name, \
                la.linked_at, \
                cc.custom_url, \
                cc.subscriber_count, \
                cc.view_count, \
                cc.video_count, \
                cc.channel_created_at, \
                cc.country, \
                cc.hidden_subscribers, \
                cc.checked_at, \
                COUNT(*) OVER() AS total_count \
         FROM linked_accounts la \
         LEFT JOIN channel_cache cc ON cc.discord_id = la.discord_id \
         WHERE la.discord_id = ANY($1) {search_clause} \
         ORDER BY {order_col} {order_dir} NULLS LAST \
         LIMIT $2 OFFSET $3",
        search_clause = if has_search {
            "AND (la.discord_name ILIKE $4 \
             OR la.discord_id ILIKE $4 \
             OR cc.custom_url ILIKE $4 \
             OR cc.country ILIKE $4)"
        } else {
            ""
        },
        order_col = order_col,
        order_dir = order_dir,
    );

    use sqlx::Row;
    let rows = if has_search {
        sqlx::query(&sql)
            .bind(&member_ids)
            .bind(per_page)
            .bind(offset)
            .bind(&search_pattern)
            .fetch_all(&state.pool)
            .await?
    } else {
        sqlx::query(&sql)
            .bind(&member_ids)
            .bind(per_page)
            .bind(offset)
            .fetch_all(&state.pool)
            .await?
    };

    let total: i64 = rows.first().map(|r| r.get("total_count")).unwrap_or(0);

    let subscribers: Vec<Value> = rows
        .iter()
        .map(|r| {
            let linked_at: chrono::DateTime<chrono::Utc> = r.get("linked_at");
            let checked_at: Option<chrono::DateTime<chrono::Utc>> = r.get("checked_at");
            let channel_created_at: Option<chrono::DateTime<chrono::Utc>> =
                r.get("channel_created_at");
            let hidden_subscribers: bool =
                r.get::<Option<bool>, _>("hidden_subscribers").unwrap_or(false);
            json!({
                "discord_id": r.get::<String, _>("discord_id"),
                "discord_name": r.get::<Option<String>, _>("discord_name"),
                "linked_at": linked_at,
                "custom_url": r.get::<Option<String>, _>("custom_url"),
                "subscriber_count": r.get::<Option<i64>, _>("subscriber_count"),
                "view_count": r.get::<Option<i64>, _>("view_count"),
                "video_count": r.get::<Option<i64>, _>("video_count"),
                "channel_created_at": channel_created_at,
                "country": r.get::<Option<String>, _>("country"),
                "hidden_subscribers": hidden_subscribers,
                "checked_at": checked_at,
            })
        })
        .collect();

    Ok(Json(json!({
        "subscribers": subscribers,
        "total": total,
        "page": page,
        "per_page": per_page,
        "guild_name": ag_guild_name,
    })))
}
