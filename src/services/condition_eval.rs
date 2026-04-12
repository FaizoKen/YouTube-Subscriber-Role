use chrono::{DateTime, Utc};

use crate::models::condition::{Condition, ConditionField, ConditionOperator};

/// Data needed for in-memory condition evaluation.
pub struct PlayerYouTubeData {
    pub subscribed_at: Option<DateTime<Utc>>,
    pub subscriber_count: Option<i64>,
    pub view_count: Option<i64>,
    pub video_count: Option<i64>,
    pub channel_created_at: Option<DateTime<Utc>>,
    pub hidden_subscribers: bool,
    pub country: Option<String>,
    pub custom_url: Option<String>,
}

/// Evaluate all conditions (AND logic). Returns true if all pass.
/// An empty conditions slice returns true (no extra filtering).
pub fn evaluate_conditions(conditions: &[Condition], data: &PlayerYouTubeData) -> bool {
    conditions.iter().all(|c| evaluate_single(c, data))
}

fn evaluate_single(condition: &Condition, data: &PlayerYouTubeData) -> bool {
    match &condition.field {
        // --- Boolean field: HasCustomUrl ---
        ConditionField::HasCustomUrl => {
            data.custom_url.as_ref().map_or(false, |u| !u.is_empty())
        }

        // --- String field: Country (Eq only) ---
        ConditionField::Country => {
            let expected = condition.value.as_str().unwrap_or("");
            data.country
                .as_ref()
                .map_or(false, |c| c.eq_ignore_ascii_case(expected))
        }

        // --- Numeric fields ---
        field => {
            let actual: Option<i64> = match field {
                ConditionField::SubscriptionAgeDays => {
                    data.subscribed_at.map(|ts| (Utc::now() - ts).num_days())
                }
                ConditionField::SubscriberCount => {
                    if data.hidden_subscribers {
                        return false;
                    }
                    data.subscriber_count
                }
                ConditionField::ViewCount => data.view_count,
                ConditionField::VideoCount => data.video_count,
                ConditionField::ChannelAgeDays => {
                    data.channel_created_at.map(|ts| (Utc::now() - ts).num_days())
                }
                ConditionField::Country | ConditionField::HasCustomUrl => unreachable!(),
            };

            let Some(actual) = actual else {
                return false;
            };
            let expected = condition.value.as_i64().unwrap_or(0);
            compare(actual, expected, &condition.operator, &condition.value_end)
        }
    }
}

fn compare(
    actual: i64,
    expected: i64,
    op: &ConditionOperator,
    value_end: &Option<serde_json::Value>,
) -> bool {
    match op {
        ConditionOperator::Eq => actual == expected,
        ConditionOperator::Gt => actual > expected,
        ConditionOperator::Gte => actual >= expected,
        ConditionOperator::Lt => actual < expected,
        ConditionOperator::Lte => actual <= expected,
        ConditionOperator::Between => {
            let end = value_end
                .as_ref()
                .and_then(|v| v.as_i64())
                .unwrap_or(expected);
            actual >= expected && actual <= end
        }
    }
}
