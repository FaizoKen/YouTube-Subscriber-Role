use std::collections::HashMap;

use serde_json::Value;

use crate::error::AppError;
use crate::models::condition::{Condition, ConditionField, ConditionOperator};

pub fn build_config_schema(
    channel_id: Option<&str>,
    conditions: &[Condition],
    verify_url: &str,
) -> Value {
    // Extract current condition values for populating the form
    let (field_val, op_val) = conditions.first().map_or(("", "gte"), |c| {
        (c.field.json_key(), c.operator.key())
    });

    let mut values = serde_json::json!({
        "channel_id": channel_id.unwrap_or(""),
        "field": field_val,
        "operator": op_val
    });

    // Populate field-specific value keys from the saved condition
    if let Some(c) = conditions.first() {
        let value_key = format!("value_{}", c.field.json_key());
        values[&value_key] = c.value.clone();
        if let Some(ref end) = c.value_end {
            let end_key = format!("value_end_{}", c.field.json_key());
            values[&end_key] = end.clone();
        }
    }

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
                             **Step 2:** You configure the YouTube channel and optional conditions below.\n\n\
                             **Step 3:** Members who meet the criteria automatically receive this role."
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
                        "description": "The channel ID (e.g. UCxxxxxx). Copy it from: https://www.youtube.com/account_advanced",
                        "placeholder": "UCxxxxxxxxxxxxxxxxxxxxxxxx",
                        "validation": {
                            "required": true,
                            "pattern": "^UC[\\w-]{22}$",
                            "pattern_message": "Must be a valid YouTube channel ID starting with UC (24 characters)"
                        }
                    }
                ]
            },
            {
                "title": "Additional Condition",
                "description": "Optionally require an additional condition beyond being subscribed to the channel.",
                "collapsible": true,
                "default_collapsed": conditions.is_empty(),
                "fields": [
                    {
                        "type": "select",
                        "key": "field",
                        "label": "Condition type",
                        "description": "Leave as \"None\" for subscription-only (no additional requirement).",
                        "options": [
                            { "label": "None (subscription only)", "value": "" },
                            { "label": "Subscription Duration (days)", "value": "subscriptionAgeDays" },
                            { "label": "Subscriber Count (user's channel)", "value": "subscriberCount" },
                            { "label": "View Count (user's channel)", "value": "viewCount" },
                            { "label": "Video Count (user's channel)", "value": "videoCount" },
                            { "label": "Channel Age (days, user's channel)", "value": "channelAgeDays" },
                            { "label": "Country (user's channel)", "value": "country" },
                            { "label": "Has Custom URL (user's channel)", "value": "hasCustomUrl" }
                        ]
                    },
                    // --- Operator: only for numeric fields ---
                    {
                        "type": "select",
                        "key": "operator",
                        "label": "Comparison",
                        "default_value": "gte",
                        "condition": { "field": "field", "equals_any": ["subscriptionAgeDays", "subscriberCount", "viewCount", "videoCount", "channelAgeDays"] },
                        "options": [
                            { "label": "= equals", "value": "eq" },
                            { "label": "> greater than", "value": "gt" },
                            { "label": ">= at least", "value": "gte" },
                            { "label": "< less than", "value": "lt" },
                            { "label": "<= at most", "value": "lte" },
                            { "label": "between (range)", "value": "between" }
                        ]
                    },
                    // --- Subscription Age Days ---
                    {
                        "type": "number",
                        "key": "value_subscriptionAgeDays",
                        "label": "Days subscribed",
                        "description": "Minimum number of days since the user subscribed to the channel.",
                        "placeholder": "30",
                        "validation": { "min": 1 },
                        "condition": { "field": "field", "equals": "subscriptionAgeDays" }
                    },
                    {
                        "type": "number",
                        "key": "value_end_subscriptionAgeDays",
                        "label": "Days subscribed (max)",
                        "description": "Upper bound for the range.",
                        "validation": { "min": 1 },
                        "pair_with": "value_subscriptionAgeDays",
                        "conditions": [
                            { "field": "field", "equals": "subscriptionAgeDays" },
                            { "field": "operator", "equals": "between" }
                        ]
                    },
                    // --- Subscriber Count ---
                    {
                        "type": "number",
                        "key": "value_subscriberCount",
                        "label": "Subscriber count",
                        "description": "Number of subscribers on the user's own YouTube channel. Users who hide their subscriber count will not qualify.",
                        "placeholder": "100",
                        "validation": { "min": 0 },
                        "condition": { "field": "field", "equals": "subscriberCount" }
                    },
                    {
                        "type": "number",
                        "key": "value_end_subscriberCount",
                        "label": "Subscriber count (max)",
                        "description": "Upper bound for the range.",
                        "validation": { "min": 0 },
                        "pair_with": "value_subscriberCount",
                        "conditions": [
                            { "field": "field", "equals": "subscriberCount" },
                            { "field": "operator", "equals": "between" }
                        ]
                    },
                    // --- View Count ---
                    {
                        "type": "number",
                        "key": "value_viewCount",
                        "label": "Total view count",
                        "description": "Total views across all videos on the user's YouTube channel.",
                        "placeholder": "1000",
                        "validation": { "min": 0 },
                        "condition": { "field": "field", "equals": "viewCount" }
                    },
                    {
                        "type": "number",
                        "key": "value_end_viewCount",
                        "label": "Total view count (max)",
                        "description": "Upper bound for the range.",
                        "validation": { "min": 0 },
                        "pair_with": "value_viewCount",
                        "conditions": [
                            { "field": "field", "equals": "viewCount" },
                            { "field": "operator", "equals": "between" }
                        ]
                    },
                    // --- Video Count ---
                    {
                        "type": "number",
                        "key": "value_videoCount",
                        "label": "Video count",
                        "description": "Number of videos uploaded to the user's YouTube channel.",
                        "placeholder": "5",
                        "validation": { "min": 0 },
                        "condition": { "field": "field", "equals": "videoCount" }
                    },
                    {
                        "type": "number",
                        "key": "value_end_videoCount",
                        "label": "Video count (max)",
                        "description": "Upper bound for the range.",
                        "validation": { "min": 0 },
                        "pair_with": "value_videoCount",
                        "conditions": [
                            { "field": "field", "equals": "videoCount" },
                            { "field": "operator", "equals": "between" }
                        ]
                    },
                    // --- Channel Age Days ---
                    {
                        "type": "number",
                        "key": "value_channelAgeDays",
                        "label": "Channel age (days)",
                        "description": "Minimum age of the user's YouTube channel in days.",
                        "placeholder": "90",
                        "validation": { "min": 1 },
                        "condition": { "field": "field", "equals": "channelAgeDays" }
                    },
                    {
                        "type": "number",
                        "key": "value_end_channelAgeDays",
                        "label": "Channel age (max days)",
                        "description": "Upper bound for the range.",
                        "validation": { "min": 1 },
                        "pair_with": "value_channelAgeDays",
                        "conditions": [
                            { "field": "field", "equals": "channelAgeDays" },
                            { "field": "operator", "equals": "between" }
                        ]
                    },
                    // --- Country ---
                    {
                        "type": "text",
                        "key": "value_country",
                        "label": "Country code",
                        "description": "ISO 3166-1 alpha-2 country code set on the user's YouTube channel (e.g. US, GB, JP, DE, BR, KR, IN).",
                        "placeholder": "US",
                        "validation": {
                            "required": true,
                            "pattern": "^[A-Za-z]{2}$",
                            "pattern_message": "Must be a 2-letter country code (e.g. US, GB, JP)"
                        },
                        "condition": { "field": "field", "equals": "country" }
                    },
                    // --- Has Custom URL (no value needed — boolean check) ---
                    {
                        "type": "display",
                        "key": "value_hasCustomUrl",
                        "label": "Requirement",
                        "value": "User must have a custom URL on their YouTube channel (e.g. youtube.com/@username). YouTube requires channels to meet certain eligibility criteria to get a custom URL, so this acts as an \"established channel\" check.",
                        "condition": { "field": "field", "equals": "hasCustomUrl" }
                    }
                ]
            }
        ],
        "values": values
    })
}

/// Parse and validate config from POST /config.
/// Returns (channel_id, conditions).
pub fn parse_config(config: &HashMap<String, Value>) -> Result<(String, Vec<Condition>), AppError> {
    // 1. Validate channel_id (existing logic)
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

    // 2. Parse optional condition
    let field_key = config
        .get("field")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();

    if field_key.is_empty() {
        // No condition — subscription-only mode
        return Ok((channel_id.to_string(), vec![]));
    }

    let field = ConditionField::from_key(field_key)
        .ok_or_else(|| AppError::BadRequest(format!("Invalid condition field: '{field_key}'")))?;

    // --- HasCustomUrl: boolean field, no operator/value needed ---
    if field == ConditionField::HasCustomUrl {
        return Ok((
            channel_id.to_string(),
            vec![Condition {
                field,
                operator: ConditionOperator::Eq, // placeholder, not used in evaluation
                value: serde_json::Value::Bool(true),
                value_end: None,
            }],
        ));
    }

    // --- Country: string Eq, no comparison operator UI ---
    if field == ConditionField::Country {
        let country = config
            .get("value_country")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_uppercase();

        if country.len() != 2 || !country.chars().all(|c| c.is_ascii_alphabetic()) {
            return Err(AppError::BadRequest(
                "Country must be a 2-letter ISO code (e.g. US, GB, JP)".into(),
            ));
        }

        return Ok((
            channel_id.to_string(),
            vec![Condition {
                field,
                operator: ConditionOperator::Eq,
                value: serde_json::Value::String(country),
                value_end: None,
            }],
        ));
    }

    // --- Numeric fields: operator + value ---
    let op_key = config
        .get("operator")
        .and_then(|v| v.as_str())
        .unwrap_or("gte")
        .trim();

    let operator = ConditionOperator::from_key(op_key)
        .ok_or_else(|| AppError::BadRequest(format!("Invalid operator: '{op_key}'")))?;

    // Parse value from field-specific key
    let specific_key = format!("value_{field_key}");
    let raw_value = config.get(&specific_key).or_else(|| config.get("value"));
    let value = raw_value
        .and_then(|v| {
            // Accept both number and string-encoded number
            v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse::<i64>().ok()))
        })
        .ok_or_else(|| AppError::BadRequest("Condition value is required and must be a number".into()))?;

    if value < 0 {
        return Err(AppError::BadRequest("Condition value must be non-negative".into()));
    }

    // Parse end value for Between
    let value_end = if operator == ConditionOperator::Between {
        let end_key = format!("value_end_{field_key}");
        let raw_end = config.get(&end_key).or_else(|| config.get("value_end"));
        let end = raw_end
            .and_then(|v| {
                v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse::<i64>().ok()))
            })
            .ok_or_else(|| {
                AppError::BadRequest("End value is required for 'between' operator".into())
            })?;
        if value > end {
            return Err(AppError::BadRequest(
                "Start value must be less than or equal to end value".into(),
            ));
        }
        Some(serde_json::Value::Number(end.into()))
    } else {
        None
    };

    Ok((
        channel_id.to_string(),
        vec![Condition {
            field,
            operator,
            value: serde_json::Value::Number(value.into()),
            value_end,
        }],
    ))
}
