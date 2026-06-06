//! RoleLogic `GET /config` builder — iframe UI mode (BLUEPRINT §1b).
//!
//! `GET /config` returns an `embed_url` pointing at the plugin's own
//! role-config page (`/admin/{guild}/role/{role}`); all real editing happens
//! there. `POST /config` is never called by iframe-mode plugins — the handler
//! keeps a token-verified stub for contract compliance.

use serde_json::{json, Value};

/// Build the iframe-mode response returned by `GET /config`. RoleLogic appends
/// `?rl_token=<jwt>` to `embed_url` before rendering the iframe; the admin page
/// verifies that token locally (BLUEPRINT §1b.3) to authenticate the admin.
pub fn build_iframe_config(base_url: &str, guild_id: &str, role_id: &str) -> Value {
    let embed_url = format!("{base_url}/admin/{guild_id}/role/{role_id}");
    json!({
        "version": 1,
        "ui_mode": "iframe",
        "name": "YouTube Subscriber Role",
        "description": "Grant Discord roles based on a member's YouTube subscription and their own channel stats — with presets and a full rule builder.",
        "embed_url": embed_url,
    })
}

/// `POST /config` is unreachable in iframe mode — the RoleLogic backend rejects
/// it before forwarding — but the contract still expects a 200 on the off
/// chance an older backend forwards a call. The token is verified in the
/// handler before this is returned.
pub fn accept_empty_config() -> Value {
    json!({ "success": true })
}
