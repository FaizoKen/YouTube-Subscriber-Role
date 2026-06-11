//! Admin role-config routes (iframe UI mode, BLUEPRINT §1b).
//!
//! The dashboard embeds `GET /admin/{guild}/role/{role}` in an iframe. The page
//! is dual-mode: it authenticates either via the `?rl_token=` JWT RoleLogic
//! appends (iframe entry) or via the `rl_session` cookie + Auth-Gateway manager
//! check (direct nav). All XHRs from the page carry an `ifs:` Bearer token
//! minted at page load.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use axum_extra::extract::cookie::CookieJar;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::AppError;
use crate::models::condition::{ConditionOperator, ConditionTarget, TargetKind};
use crate::models::rule::{RuleTree, MAX_CONDITIONS_PER_GROUP, MAX_GROUPS};
use crate::services::auth::{extract_bearer, require_guild_admin, require_manager};
use crate::services::rule_sql::{self, Bind};
use crate::services::rule_validator::{self, RuleTreeBody};
use crate::services::security_headers::admin_iframe_csp;
use crate::services::sync::ConfigSyncEvent;
use crate::services::{auth_gateway, csrf, rl_token};
use crate::AppState;

const ROLE_CONFIG_TEMPLATE: &str = include_str!("../../templates/role_config.html");

// ---------------------------------------------------------------------
// Iframe role-config page (dual-mode: rl_token JWT entry OR cookie+manager)
// ---------------------------------------------------------------------

#[derive(Deserialize)]
pub struct RoleConfigPageQuery {
    #[serde(default)]
    rl_token: Option<String>,
}

pub async fn role_config_page(
    State(state): State<Arc<AppState>>,
    jar: CookieJar,
    headers: HeaderMap,
    Path((guild_id, role_id)): Path<(String, String)>,
    Query(query): Query<RoleConfigPageQuery>,
) -> Response {
    let has_rl_token = query
        .rl_token
        .as_deref()
        .map(|t| !t.is_empty())
        .unwrap_or(false);

    // Path 1: iframe entry — verify rl_token, mint an iframe-session token.
    let iframe_session = match query.rl_token.as_deref() {
        Some(token) if !token.is_empty() => {
            match verify_iframe_entry(&state, &guild_id, &role_id, token).await {
                Ok(t) => Some(t),
                Err(resp) => return resp,
            }
        }
        _ => None,
    };

    // Path 2: direct nav — cookie + manager check. A cross-site iframe will NOT
    // carry our first-party `rl_session` cookie, so landing here while embedded
    // almost always means RoleLogic never appended `?rl_token=` (usually a
    // BASE_URL / registered-plugin-URL mismatch). Surface that precisely.
    if iframe_session.is_none() {
        if let Err(e) = require_manager(&state, &jar, &guild_id).await {
            if !has_rl_token && looks_embedded(&headers) {
                tracing::warn!(
                    guild_id,
                    role_id,
                    base_url = %state.config.base_url,
                    "role_config_page reached inside an iframe with no rl_token — \
                     RoleLogic did not pass an auth token. Verify BASE_URL exactly \
                     matches the plugin URL registered in RoleLogic (https, \
                     including the /youtube-subscriber-role path prefix)."
                );
                return render_iframe_no_token(&state);
            }
            return render_signin_page(&state, &e.to_string());
        }
    }

    let body = ROLE_CONFIG_TEMPLATE
        .replace("__BASE_URL__", &state.config.base_url)
        .replace("__GUILD_ID__", &guild_id)
        .replace("__ROLE_ID__", &role_id)
        .replace("__IFRAME_TOKEN__", iframe_session.as_deref().unwrap_or(""));

    let csp = admin_iframe_csp(state.config.rl_dashboard_origin.as_deref());
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8".to_string()),
            (header::CONTENT_SECURITY_POLICY, csp),
            (
                header::CACHE_CONTROL,
                "private, max-age=300, must-revalidate".to_string(),
            ),
        ],
        body,
    )
        .into_response()
}

/// Verify `?rl_token=…` (six checks, in order) and return a freshly minted
/// iframe-session token. On failure returns a rendered error page.
async fn verify_iframe_entry(
    state: &AppState,
    guild_id: &str,
    role_id: &str,
    rl_token_str: &str,
) -> Result<String, Response> {
    let api_token: Option<String> =
        sqlx::query_scalar("SELECT api_token FROM role_links WHERE guild_id = $1 AND role_id = $2")
            .bind(guild_id)
            .bind(role_id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|e| render_inline_error(state, &format!("Database error: {e}")))?;

    let Some(api_token) = api_token else {
        return Err(render_inline_error(
            state,
            "This role link isn't registered with this plugin yet.",
        ));
    };

    let verified =
        rl_token::verify(rl_token_str, &api_token, &state.config.base_url).map_err(|e| {
            let msg = match e {
                rl_token::RlTokenError::Expired => {
                    "Your session expired. Reopen the plugin in the RoleLogic dashboard."
                }
                rl_token::RlTokenError::BadSignature | rl_token::RlTokenError::Malformed => {
                    "Invalid auth token."
                }
                rl_token::RlTokenError::WrongAudience => "Token is for a different plugin.",
                rl_token::RlTokenError::WrongIssuer => "Token was not issued by RoleLogic.",
            };
            render_inline_error(state, msg)
        })?;

    // Cross-check claims vs path (no privilege escalation across role links).
    if verified.guild_id != guild_id || verified.role_id != role_id {
        return Err(render_inline_error(
            state,
            "Token does not match this role link.",
        ));
    }

    Ok(rl_token::mint_iframe_session(
        &verified.discord_id,
        guild_id,
        role_id,
        &state.config.session_secret,
    ))
}

fn render_inline_error(state: &AppState, message: &str) -> Response {
    let base_url = &state.config.base_url;
    let msg = message
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let body = format!(
        r##"<!DOCTYPE html><html><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Cannot load configuration</title>
<link rel="icon" href="{base_url}/favicon.ico">
<style>body{{font-family:system-ui,sans-serif;background:#0e1525;color:#c8ccd4;padding:32px 24px;line-height:1.5}}
h1{{color:#fca5a5;font-size:18px;margin-bottom:10px}}p{{color:#7a8299}}</style>
</head><body><h1>Cannot load configuration</h1><p>{msg}</p>
<p style="margin-top:14px;color:#64748b">If you opened this from the RoleLogic dashboard, close and reopen the role's plugin tab.</p>
</body></html>"##
    );
    let csp = admin_iframe_csp(state.config.rl_dashboard_origin.as_deref());
    (
        StatusCode::FORBIDDEN,
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8".to_string()),
            (header::CONTENT_SECURITY_POLICY, csp),
        ],
        body,
    )
        .into_response()
}

/// Heuristic: is this the document load of a cross-site iframe? Used only to
/// pick the right *message* (never for authz).
fn looks_embedded(headers: &HeaderMap) -> bool {
    let h = |k: &str| {
        headers
            .get(k)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase()
    };
    let dest = h("sec-fetch-dest");
    dest == "iframe" || dest == "frame" || h("sec-fetch-site") == "cross-site"
}

/// Shown when the page is embedded but RoleLogic didn't append `?rl_token=`.
fn render_iframe_no_token(state: &AppState) -> Response {
    let base_url = &state.config.base_url;
    let body = format!(
        r##"<!DOCTYPE html><html><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Configuration unavailable</title>
<link rel="icon" href="{base_url}/favicon.ico">
<style>body{{font-family:system-ui,sans-serif;background:#0e1525;color:#c8ccd4;padding:32px 24px;line-height:1.55;max-width:560px}}
h1{{color:#fbbf24;font-size:18px;margin:0 0 10px}}p{{color:#7a8299;margin:8px 0}}
code{{background:#161d2e;padding:2px 6px;border-radius:4px;font-size:12px}}</style>
</head><body>
<h1>RoleLogic didn't pass an authentication token</h1>
<p>This plugin page must be opened from inside the RoleLogic dashboard, which
attaches a one-time token. None arrived with this request.</p>
<p><strong>If you're the server admin:</strong> close this tab and reopen the
role's plugin tab from RoleLogic. If it keeps happening, the plugin is
mis-registered — its <code>BASE_URL</code> must exactly match the URL configured
for this plugin in RoleLogic: HTTPS, no trailing slash, and including the
<code>/youtube-subscriber-role</code> path prefix.</p>
<p style="color:#64748b;font-size:12px;margin-top:16px">Configured BASE_URL:
<code>{base_url}</code></p>
</body></html>"##
    );
    let csp = admin_iframe_csp(state.config.rl_dashboard_origin.as_deref());
    (
        StatusCode::UNAUTHORIZED,
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8".to_string()),
            (header::CONTENT_SECURITY_POLICY, csp),
        ],
        body,
    )
        .into_response()
}

/// Direct-nav (non-iframe) sign-in prompt. Render an in-page "Login with
/// Discord" the user clicks themselves; never auto-redirect.
fn render_signin_page(state: &AppState, reason: &str) -> Response {
    let base_url = &state.config.base_url;
    let reason = reason
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let body = format!(
        r##"<!DOCTYPE html><html><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Sign in — YouTube Subscriber Role</title>
<link rel="icon" href="{base_url}/favicon.ico">
<style>body{{font-family:system-ui,sans-serif;background:#0e1525;color:#c8ccd4;padding:48px 24px;max-width:520px;margin:0 auto;line-height:1.55}}
h1{{font-size:22px;margin:0 0 12px;color:#fff}}p{{color:#7a8299}}
a.btn{{display:inline-block;margin-top:18px;background:#5865f2;color:#fff;padding:12px 22px;border-radius:8px;text-decoration:none;font-weight:600}}
.actions{{display:flex;gap:10px;align-items:center;flex-wrap:wrap;margin-top:18px}}
.actions a.btn{{margin-top:0}}
form.logout-form{{margin:0}}
button.logout{{background:none;color:#7a8299;border:1px solid #2a3548;
  padding:10px 16px;border-radius:8px;font-size:13px;font-weight:600;
  cursor:pointer;font-family:inherit}}
button.logout:hover{{color:#fca5a5;border-color:#7f1d1d}}</style>
</head><body>
<h1>Sign in to continue</h1>
<p>You need <strong>Manage Server</strong> on this server to edit its
YouTube-Subscriber-Role configuration.</p>
<p style="color:#64748b;font-size:12px">{reason}</p>
<div class="actions">
  <a class="btn" id="login">Sign in with Discord</a>
  <form class="logout-form" method="POST" action="/auth/logout">
    <button type="submit" class="logout">Sign out &amp; try another account</button>
  </form>
</div>
<script>
const ORIGIN=new URL("{base_url}").origin;
const RET=encodeURIComponent(location.pathname);
document.getElementById('login').href=ORIGIN+'/auth/login?return_to='+RET;
document.querySelectorAll('form.logout-form').forEach(f=>{{
  f.action=ORIGIN+'/auth/logout?return_to='+RET;
}});
</script>
</body></html>"##
    );
    let csp = admin_iframe_csp(state.config.rl_dashboard_origin.as_deref());
    (
        StatusCode::UNAUTHORIZED,
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8".to_string()),
            (header::CONTENT_SECURITY_POLICY, csp),
        ],
        body,
    )
        .into_response()
}

/// Dual gate: `Authorization: Bearer ifs:…` (iframe) OR cookie+manager (direct
/// nav). Returns the caller's discord_id.
async fn require_role_config_access(
    state: &Arc<AppState>,
    jar: &CookieJar,
    headers: &HeaderMap,
    guild_id: &str,
    role_id: &str,
) -> Result<String, AppError> {
    if let Some(bearer) = extract_bearer(headers) {
        let s = rl_token::verify_iframe_session(&bearer, &state.config.session_secret).ok_or_else(
            || {
                AppError::UnauthorizedWith(
                    "Your session expired. Reopen the plugin in the RoleLogic dashboard.".into(),
                )
            },
        )?;
        if s.guild_id != guild_id || s.role_id != role_id {
            return Err(AppError::Forbidden(
                "Token does not grant access to this role link.".into(),
            ));
        }
        return Ok(s.discord_id);
    }
    require_manager(state, jar, guild_id).await
}

// ---------------------------------------------------------------------
// GET /admin/{guild_id}/role/{role_id}/data
// ---------------------------------------------------------------------

pub async fn role_config_data(
    State(state): State<Arc<AppState>>,
    jar: CookieJar,
    headers: HeaderMap,
    Path((guild_id, role_id)): Path<(String, String)>,
) -> Result<Json<Value>, AppError> {
    require_role_config_access(&state, &jar, &headers, &guild_id, &role_id).await?;

    let link = sqlx::query_as::<_, (Option<String>, Value, i32)>(
        "SELECT channel_id, rule_tree, config_version \
         FROM role_links WHERE guild_id = $1 AND role_id = $2",
    )
    .bind(&guild_id)
    .bind(&role_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| {
        AppError::NotFound("This role link doesn't exist. Has it been added in RoleLogic?".into())
    })?;
    let (channel_id, raw_tree, config_version) = link;
    let tree: RuleTree = serde_json::from_value(raw_tree).unwrap_or_default();

    let view_permission: String =
        sqlx::query_scalar("SELECT view_permission FROM guild_settings WHERE guild_id = $1")
            .bind(&guild_id)
            .fetch_optional(&state.pool)
            .await?
            .unwrap_or_else(|| "members".to_string());

    Ok(Json(json!({
        "guild_id": guild_id,
        "role_id": role_id,
        "config": {
            "channel_id": channel_id,
            "grant_on_any": tree.grant_on_any,
            "groups": tree.groups,
        },
        "config_version": config_version,
        "targets": target_catalog(),
        "operators": operator_catalog(),
        "limits": {
            "max_groups": MAX_GROUPS,
            "max_conditions_per_group": MAX_CONDITIONS_PER_GROUP,
        },
        // Per-guild verify URL. The `?guild=<id>` query the verify page reads to
        // show "Verifying for <Server>" context and auto-clear any opt-out.
        "verify_url": format!("{}/verify?guild={}", state.config.base_url, guild_id),
        "subscribers": {
            "url": format!("{}/subscribers/{}", state.config.base_url, guild_id),
            "view_permission": view_permission,
        },
    })))
}

// ---------------------------------------------------------------------
// POST /admin/{guild_id}/role/{role_id}/save  (optimistic-locked)
// ---------------------------------------------------------------------

#[derive(Deserialize)]
pub struct RoleConfigSaveBody {
    pub config_version: i32,
    #[serde(flatten)]
    pub tree: RuleTreeBody,
}

pub async fn role_config_save(
    State(state): State<Arc<AppState>>,
    jar: CookieJar,
    headers: HeaderMap,
    Path((guild_id, role_id)): Path<(String, String)>,
    Json(body): Json<RoleConfigSaveBody>,
) -> Result<Json<Value>, AppError> {
    // Bearer (iframe-session) is CSRF-safe by token binding; only the cookie
    // path needs the Origin allowlist.
    if extract_bearer(&headers).is_none() {
        csrf::verify_origin(&headers, &state.config.allowed_origins)?;
    }
    require_role_config_access(&state, &jar, &headers, &guild_id, &role_id).await?;

    let expected_version = body.config_version;
    let parsed = rule_validator::parse_rule_tree(body.tree)?;

    // Subscription conditions are evaluated against a configured channel;
    // without one they can never be true, so require it. Rules built only from
    // the member's own channel stats (subscriber count etc.) are channel-
    // agnostic and save fine without one. Mirrors the preview's "nobody"
    // guard so the two never disagree.
    if !parsed.rule_tree.grant_on_any
        && rule_needs_channel(&parsed.rule_tree)
        && parsed.channel_id.is_none()
    {
        return Err(AppError::BadRequest(
            "This rule checks subscriptions to your channel — enter the YouTube channel ID it should check against before saving.".into(),
        ));
    }

    let tree_json = serde_json::to_value(&parsed.rule_tree)
        .map_err(|e| AppError::Internal(format!("serialize rule_tree: {e}")))?;

    // Optimistic lock: only update if the version still matches what the editor
    // loaded, so a second tab can't silently clobber.
    let result = sqlx::query(
        "UPDATE role_links \
         SET channel_id = $1, rule_tree = $2, \
             config_version = config_version + 1, updated_at = now() \
         WHERE guild_id = $3 AND role_id = $4 AND config_version = $5",
    )
    .bind(&parsed.channel_id)
    .bind(&tree_json)
    .bind(&guild_id)
    .bind(&role_id)
    .bind(expected_version)
    .execute(&state.pool)
    .await?;

    if result.rows_affected() == 0 {
        let exists: Option<i32> = sqlx::query_scalar(
            "SELECT config_version FROM role_links WHERE guild_id = $1 AND role_id = $2",
        )
        .bind(&guild_id)
        .bind(&role_id)
        .fetch_optional(&state.pool)
        .await?;
        return match exists {
            None => Err(AppError::NotFound(
                "This role link doesn't exist. Has it been added in RoleLogic?".into(),
            )),
            Some(_) => Err(AppError::StaleVersion),
        };
    }

    let new_version: i32 = sqlx::query_scalar(
        "SELECT config_version FROM role_links WHERE guild_id = $1 AND role_id = $2",
    )
    .bind(&guild_id)
    .bind(&role_id)
    .fetch_one(&state.pool)
    .await?;

    // Seed cache rows so the refresh worker picks up newly relevant members.
    // (This is the work that lived in the old schema-mode POST /config.)
    if let Some(ref channel_id) = parsed.channel_id {
        sqlx::query(
            "INSERT INTO subscription_cache (discord_id, channel_id, next_check_at) \
             SELECT la.discord_id, $1, now() FROM linked_accounts la \
             ON CONFLICT (discord_id, channel_id) DO NOTHING",
        )
        .bind(channel_id)
        .execute(&state.pool)
        .await?;
    }

    if parsed.rule_tree.needs_channel_cache() {
        sqlx::query(
            "INSERT INTO channel_cache (discord_id, next_check_at) \
             SELECT la.discord_id, now() FROM linked_accounts la \
             ON CONFLICT (discord_id) DO NOTHING",
        )
        .execute(&state.pool)
        .await?;
    }

    // Trigger a full re-evaluation for this role link.
    let _ = state
        .config_sync_tx
        .send(ConfigSyncEvent {
            guild_id: guild_id.clone(),
            role_id: role_id.clone(),
        })
        .await;

    tracing::info!(
        guild_id,
        role_id,
        groups = parsed.rule_tree.groups.len(),
        grant_on_any = parsed.rule_tree.grant_on_any,
        "Role rule_tree updated"
    );

    Ok(Json(
        json!({ "success": true, "config_version": new_version }),
    ))
}

// ---------------------------------------------------------------------
// Preview: how many linked members would currently match? No RoleLogic call.
// ---------------------------------------------------------------------

pub async fn role_config_preview(
    State(state): State<Arc<AppState>>,
    jar: CookieJar,
    headers: HeaderMap,
    Path((guild_id, role_id)): Path<(String, String)>,
) -> Result<Json<Value>, AppError> {
    require_role_config_access(&state, &jar, &headers, &guild_id, &role_id).await?;

    let link = sqlx::query_as::<_, (Option<String>, Value)>(
        "SELECT channel_id, rule_tree FROM role_links WHERE guild_id = $1 AND role_id = $2",
    )
    .bind(&guild_id)
    .bind(&role_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Role link not found.".into()))?;
    let (channel_id, raw_tree) = link;
    let tree: RuleTree = serde_json::from_value(raw_tree).unwrap_or_default();

    preview_count_for(&state, &guild_id, channel_id, &tree).await
}

/// POST variant: previews a proposed (unsaved) rule. Validation mirrors save,
/// except a missing channel doesn't error — it just yields "0 match" so the
/// admin sees the consequence before they commit.
pub async fn role_config_preview_edit(
    State(state): State<Arc<AppState>>,
    jar: CookieJar,
    headers: HeaderMap,
    Path((guild_id, role_id)): Path<(String, String)>,
    Json(body): Json<RuleTreeBody>,
) -> Result<Json<Value>, AppError> {
    if extract_bearer(&headers).is_none() {
        csrf::verify_origin(&headers, &state.config.allowed_origins)?;
    }
    require_role_config_access(&state, &jar, &headers, &guild_id, &role_id).await?;

    let parsed = rule_validator::parse_rule_tree(body)?;
    preview_count_for(&state, &guild_id, parsed.channel_id, &parsed.rule_tree).await
}

/// Shared core for GET (saved tree) and POST (proposed tree) previews.
async fn preview_count_for(
    state: &Arc<AppState>,
    guild_id: &str,
    channel_id: Option<String>,
    tree: &RuleTree,
) -> Result<Json<Value>, AppError> {
    // A non-grant rule with no groups, or a subscription rule with no channel,
    // grants to nobody — short-circuit so the count is honest without a query.
    let nobody = !tree.grant_on_any
        && (tree.groups.is_empty() || (channel_id.is_none() && rule_needs_channel(tree)));
    if nobody {
        return Ok(Json(
            json!({ "matching": 0, "linked": 0, "available": true }),
        ));
    }

    let member_ids = match auth_gateway::fetch_guild_member_ids(
        &state.http,
        &state.config.auth_gateway_url,
        &state.config.internal_api_key,
        guild_id,
    )
    .await
    {
        Ok(v) => v,
        Err(_) => {
            return Ok(Json(json!({
                "available": false,
                "reason": "Member list temporarily unavailable; preview will work once the Auth Gateway responds."
            })))
        }
    };
    if member_ids.is_empty() {
        return Ok(Json(
            json!({ "matching": 0, "linked": 0, "available": true }),
        ));
    }

    let linked: i64 =
        sqlx::query_scalar("SELECT count(*) FROM linked_accounts WHERE discord_id = ANY($1::text[])")
            .bind(&member_ids)
            .fetch_one(&state.pool)
            .await?;

    // Channel-agnostic "anyone who linked" rule: every linked member qualifies.
    if tree.grant_on_any {
        return Ok(Json(json!({
            "available": true,
            "matching": linked,
            "linked": linked,
        })));
    }

    let (rule_where, binds) = rule_sql::build_rule_where(tree, 2);
    let query = format!(
        "SELECT count(DISTINCT la.discord_id) \
         FROM linked_accounts la \
         LEFT JOIN subscription_cache sc ON sc.discord_id = la.discord_id AND sc.channel_id = $1 \
         LEFT JOIN channel_cache cc ON cc.discord_id = la.discord_id \
         WHERE la.discord_id = ANY($2::text[]) AND ({rule_where})"
    );
    let mut q = sqlx::query_scalar::<_, i64>(&query)
        .bind(channel_id.unwrap_or_default())
        .bind(&member_ids);
    for b in &binds {
        q = match b {
            Bind::Bool(v) => q.bind(*v),
            Bind::Int(v) => q.bind(*v),
            Bind::Text(v) => q.bind(v.clone()),
            Bind::TextArray(v) => q.bind(v.clone()),
        };
    }
    let matching: i64 = q.fetch_one(&state.pool).await?;

    Ok(Json(json!({
        "available": true,
        "matching": matching,
        "linked": linked,
    })))
}

/// Whether the tree references any subscription target (and so needs a channel).
fn rule_needs_channel(tree: &RuleTree) -> bool {
    tree.groups
        .iter()
        .flat_map(|g| &g.conditions)
        .any(|c| c.target.needs_subscription())
}

// ---------------------------------------------------------------------
// POST /admin/{guild_id}/view-permission  (server-wide subscribers-list access)
// ---------------------------------------------------------------------

#[derive(Deserialize)]
pub struct ViewPermissionBody {
    pub view_permission: String,
}

pub async fn set_view_permission(
    State(state): State<Arc<AppState>>,
    jar: CookieJar,
    headers: HeaderMap,
    Path(guild_id): Path<String>,
    Json(body): Json<ViewPermissionBody>,
) -> Result<Json<Value>, AppError> {
    if extract_bearer(&headers).is_none() {
        csrf::verify_origin(&headers, &state.config.allowed_origins)?;
    }
    require_guild_admin(&state, &jar, &headers, &guild_id).await?;

    let vp = body.view_permission.as_str();
    if !matches!(vp, "members" | "managers" | "disabled") {
        return Err(AppError::BadRequest(
            "view_permission must be 'members', 'managers', or 'disabled'.".into(),
        ));
    }

    sqlx::query(
        "INSERT INTO guild_settings (guild_id, view_permission, updated_at) \
         VALUES ($1, $2, now()) \
         ON CONFLICT (guild_id) \
         DO UPDATE SET view_permission = EXCLUDED.view_permission, updated_at = now()",
    )
    .bind(&guild_id)
    .bind(vp)
    .execute(&state.pool)
    .await?;

    Ok(Json(json!({ "success": true, "view_permission": vp })))
}

// ---------------------------------------------------------------------
// Catalogs consumed by the rule-builder front-end
// ---------------------------------------------------------------------

fn kind_str(k: TargetKind) -> &'static str {
    match k {
        TargetKind::Bool => "bool",
        TargetKind::Int => "int",
        TargetKind::String => "string",
    }
}

fn target_catalog() -> Vec<Value> {
    use ConditionTarget::*;
    let targets = [
        IsSubscribed,
        SubscriptionAgeDays,
        SubscriberCount,
        ViewCount,
        VideoCount,
        ChannelAgeDays,
        Country,
        HasCustomUrl,
    ];
    targets
        .iter()
        .map(|t| {
            json!({
                "key": t.as_str(),
                "label": t.label(),
                "kind": kind_str(t.kind()),
                "group": t.group(),
            })
        })
        .collect()
}

fn operator_catalog() -> Vec<Value> {
    use ConditionOperator::*;
    let all = [
        (Eq, "equals"),
        (Gt, "greater than"),
        (Gte, "at least"),
        (Lt, "less than"),
        (Lte, "at most"),
        (Between, "between"),
        (In, "is one of"),
    ];
    all.iter()
        .map(|(op, label)| {
            json!({
                "key": op.as_str(),
                "label": label,
                "valid_for": {
                    "bool": op.valid_for(TargetKind::Bool),
                    "int": op.valid_for(TargetKind::Int),
                    "string": op.valid_for(TargetKind::String),
                },
                "needs_value_end": op.needs_value_end(),
                "value_is_list": op.value_is_list(),
            })
        })
        .collect()
}
