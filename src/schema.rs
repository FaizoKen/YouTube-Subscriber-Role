use std::collections::HashMap;

use serde_json::Value;

use crate::error::AppError;

pub fn build_config_schema(channel_id: Option<&str>, verify_url: &str) -> Value {
    serde_json::json!({
        "version": 1,
        "name": "YouTube Subscriber Role",
        "description": "Assign a Discord role to members subscribed to a YouTube channel.",
        "sections": [
            {
                "title": "Getting Started",
                "fields": [
                    {
                        "type": "display",
                        "key": "info",
                        "label": "How it works",
                        "value": format!(
                            "This plugin assigns a role to members who are subscribed to a specific YouTube channel.\n\n\
                             **Step 1:** Members link their accounts at:\n{verify_url}\n\n\
                             **Step 2:** You configure the YouTube channel below.\n\n\
                             **Step 3:** Subscribed members automatically receive this role."
                        )
                    }
                ]
            },
            {
                "title": "YouTube Channel",
                "description": "The YouTube channel members must be subscribed to.",
                "fields": [
                    {
                        "type": "text",
                        "key": "channel_id",
                        "label": "YouTube Channel ID",
                        "description": "The channel ID (e.g. UCxxxxxx). Find it in the channel's URL or via YouTube Studio > Settings > Channel > Advanced settings.",
                        "placeholder": "UCxxxxxxxxxxxxxxxxxxxxxxxx",
                        "validation": {
                            "required": true,
                            "pattern": "^UC[\\w-]{22}$",
                            "pattern_message": "Must be a valid YouTube channel ID starting with UC (24 characters)"
                        }
                    }
                ]
            }
        ],
        "values": {
            "channel_id": channel_id.unwrap_or("")
        }
    })
}

pub fn parse_config(config: &HashMap<String, Value>) -> Result<String, AppError> {
    let channel_id = config
        .get("channel_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();

    if channel_id.is_empty() {
        return Err(AppError::BadRequest("Channel ID is required".into()));
    }

    if !channel_id.starts_with("UC") || channel_id.len() != 24 {
        return Err(AppError::BadRequest(
            "Invalid YouTube channel ID format. Must start with UC and be 24 characters.".into(),
        ));
    }

    // Validate characters: alphanumeric, hyphens, underscores
    if !channel_id[2..].chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err(AppError::BadRequest(
            "Channel ID contains invalid characters".into(),
        ));
    }

    Ok(channel_id.to_string())
}
