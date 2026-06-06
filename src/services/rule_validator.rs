//! Parse and validate the rule-tree payload sent by the iframe UI on save.
//!
//! Returns a clean `RuleTree` ready to persist as `role_links.rule_tree` JSONB.

use serde::Deserialize;
use serde_json::Value;

use crate::error::AppError;
use crate::models::condition::{Condition, ConditionOperator, ConditionTarget, TargetKind};
use crate::models::rule::{ConditionGroup, RuleTree, MAX_CONDITIONS_PER_GROUP, MAX_GROUPS};

#[derive(Debug, Deserialize)]
pub struct RuleTreeBody {
    /// The configured YouTube channel (`UC…`). `None` lets an admin save a
    /// channel-agnostic "anyone who linked" rule; subscription-based groups
    /// require it (enforced by the caller).
    #[serde(default)]
    pub channel_id: Option<String>,
    #[serde(default)]
    pub grant_on_any: bool,
    #[serde(default)]
    pub groups: Vec<ConditionGroupInput>,
}

#[derive(Debug, Deserialize)]
pub struct ConditionGroupInput {
    #[serde(default)]
    pub conditions: Vec<ConditionInput>,
}

#[derive(Debug, Deserialize)]
pub struct ConditionInput {
    pub target: String,
    pub operator: String,
    #[serde(default)]
    pub value: Value,
    #[serde(default)]
    pub value_end: Option<Value>,
}

pub struct ParsedRule {
    pub channel_id: Option<String>,
    pub rule_tree: RuleTree,
}

/// Validate the YouTube channel ID format (`UC` + 22 url-safe chars).
pub fn validate_channel_id(raw: &str) -> Result<String, AppError> {
    let id = raw.trim();
    if !id.starts_with("UC") || id.len() != 24 {
        return Err(AppError::BadRequest(
            "Invalid YouTube channel ID. It must start with UC and be 24 characters.".into(),
        ));
    }
    if !id[2..]
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(AppError::BadRequest(
            "Channel ID contains invalid characters.".into(),
        ));
    }
    Ok(id.to_string())
}

pub fn parse_rule_tree(body: RuleTreeBody) -> Result<ParsedRule, AppError> {
    let channel_id = match body.channel_id.as_deref() {
        Some(c) if !c.trim().is_empty() => Some(validate_channel_id(c)?),
        _ => None,
    };

    if !body.grant_on_any {
        if body.groups.is_empty() {
            return Err(AppError::BadRequest(
                "Add at least one rule group, or pick \"anyone who linked YouTube\".".into(),
            ));
        }
        if body.groups.len() > MAX_GROUPS {
            return Err(AppError::BadRequest(format!(
                "At most {MAX_GROUPS} rule groups per role."
            )));
        }
    }

    let mut groups: Vec<ConditionGroup> = Vec::with_capacity(body.groups.len());
    if !body.grant_on_any {
        for (gi, raw_group) in body.groups.into_iter().enumerate() {
            let group_num = gi + 1;
            if raw_group.conditions.is_empty() {
                return Err(AppError::BadRequest(format!(
                    "Group #{group_num}: add at least one condition (or remove the group)."
                )));
            }
            if raw_group.conditions.len() > MAX_CONDITIONS_PER_GROUP {
                return Err(AppError::BadRequest(format!(
                    "Group #{group_num}: at most {MAX_CONDITIONS_PER_GROUP} conditions per group."
                )));
            }
            let mut conditions: Vec<Condition> = Vec::with_capacity(raw_group.conditions.len());
            for (ci, raw) in raw_group.conditions.into_iter().enumerate() {
                conditions.push(validate_condition(group_num, ci + 1, raw)?);
            }
            groups.push(ConditionGroup { conditions });
        }
    }

    Ok(ParsedRule {
        channel_id,
        rule_tree: RuleTree {
            grant_on_any: body.grant_on_any,
            groups,
        },
    })
}

fn validate_condition(
    group_num: usize,
    cond_num: usize,
    raw: ConditionInput,
) -> Result<Condition, AppError> {
    let where_ = format!("Group #{group_num}, condition #{cond_num}");

    let target = ConditionTarget::from_key(raw.target.trim()).ok_or_else(|| {
        AppError::BadRequest(format!("{where_}: unknown target '{}'.", raw.target))
    })?;

    let operator = ConditionOperator::from_key(raw.operator.trim()).ok_or_else(|| {
        AppError::BadRequest(format!("{where_}: unknown operator '{}'.", raw.operator))
    })?;

    if !operator.valid_for(target.kind()) {
        return Err(AppError::BadRequest(format!(
            "{where_}: operator '{}' is not valid for '{}'.",
            operator.as_str(),
            target.as_str()
        )));
    }

    let value = normalize_value(&where_, target, operator, raw.value)?;
    let value_end = match (operator, raw.value_end) {
        (ConditionOperator::Between, Some(end)) => {
            let v = normalize_value(&where_, target, operator, end)?;
            // Enforce min <= max so the SQL/eval BETWEEN can't silently match
            // nothing on an inverted range.
            if let (Some(lo), Some(hi)) = (value.as_i64(), v.as_i64()) {
                if lo > hi {
                    return Err(AppError::BadRequest(format!(
                        "{where_}: the minimum must be less than or equal to the maximum."
                    )));
                }
            }
            Some(v)
        }
        (ConditionOperator::Between, None) => {
            return Err(AppError::BadRequest(format!(
                "{where_}: \"between\" needs both a minimum and a maximum value."
            )));
        }
        _ => None,
    };

    Ok(Condition {
        target,
        operator,
        value,
        value_end,
    })
}

fn normalize_value(
    where_: &str,
    target: ConditionTarget,
    op: ConditionOperator,
    raw: Value,
) -> Result<Value, AppError> {
    match (target.kind(), op) {
        (TargetKind::Bool, _) => match &raw {
            Value::Bool(_) => Ok(raw),
            Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
                "true" | "1" | "yes" => Ok(Value::Bool(true)),
                "false" | "0" | "no" => Ok(Value::Bool(false)),
                _ => Err(AppError::BadRequest(format!(
                    "{where_}: a true/false value is required."
                ))),
            },
            _ => Err(AppError::BadRequest(format!(
                "{where_}: a true/false value is required."
            ))),
        },
        (TargetKind::Int, _) => {
            let n = match &raw {
                Value::Number(num) => num.as_i64().or_else(|| num.as_f64().map(|f| f as i64)),
                Value::String(s) => s.trim().parse::<i64>().ok(),
                _ => None,
            };
            let n = n.ok_or_else(|| {
                AppError::BadRequest(format!("{where_}: a whole number is required."))
            })?;
            if n < 0 {
                return Err(AppError::BadRequest(format!(
                    "{where_}: the value must be zero or greater."
                )));
            }
            Ok(Value::from(n))
        }
        (TargetKind::String, ConditionOperator::In) => {
            let arr: Vec<Value> = match raw {
                Value::Array(a) => a
                    .into_iter()
                    .filter(|v| !matches!(v, Value::Null))
                    .filter(|v| !v.as_str().is_some_and(str::is_empty))
                    .collect(),
                Value::String(s) => s
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(|s| Value::String(s.to_string()))
                    .collect(),
                Value::Null => vec![],
                other => vec![other],
            };
            if arr.is_empty() {
                return Err(AppError::BadRequest(format!(
                    "{where_}: enter at least one value."
                )));
            }
            // Country codes are two-letter ISO codes.
            if target == ConditionTarget::Country {
                for v in &arr {
                    check_country(where_, v.as_str().unwrap_or(""))?;
                }
            }
            Ok(Value::Array(arr))
        }
        (TargetKind::String, _) => {
            let s = match raw {
                Value::String(s) => s,
                Value::Number(num) => num.to_string(),
                _ => return Err(AppError::BadRequest(format!("{where_}: a value is required."))),
            };
            if s.trim().is_empty() {
                return Err(AppError::BadRequest(format!("{where_}: a value is required.")));
            }
            if target == ConditionTarget::Country {
                check_country(where_, &s)?;
            }
            Ok(Value::String(s.trim().to_string()))
        }
    }
}

fn check_country(where_: &str, code: &str) -> Result<(), AppError> {
    let c = code.trim();
    if c.len() != 2 || !c.chars().all(|ch| ch.is_ascii_alphabetic()) {
        return Err(AppError::BadRequest(format!(
            "{where_}: country must be a 2-letter code (e.g. US, GB, JP)."
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn input(target: &str, operator: &str, value: Value) -> ConditionInput {
        ConditionInput {
            target: target.into(),
            operator: operator.into(),
            value,
            value_end: None,
        }
    }

    fn one_group(conds: Vec<ConditionInput>) -> RuleTreeBody {
        RuleTreeBody {
            channel_id: Some("UCabcdefghijklmnopqrstuv".into()),
            grant_on_any: false,
            groups: vec![ConditionGroupInput { conditions: conds }],
        }
    }

    #[test]
    fn grant_on_any_no_groups_ok() {
        let body = RuleTreeBody {
            channel_id: None,
            grant_on_any: true,
            groups: vec![],
        };
        let parsed = parse_rule_tree(body).unwrap();
        assert!(parsed.rule_tree.grant_on_any);
        assert!(parsed.rule_tree.groups.is_empty());
    }

    #[test]
    fn rejects_no_groups_no_grant() {
        let body = RuleTreeBody {
            channel_id: Some("UCabcdefghijklmnopqrstuv".into()),
            grant_on_any: false,
            groups: vec![],
        };
        assert!(matches!(parse_rule_tree(body), Err(AppError::BadRequest(_))));
    }

    #[test]
    fn accepts_subscription_rule() {
        let body = one_group(vec![
            input("isSubscribed", "eq", json!(true)),
            input("subscriptionAgeDays", "gte", json!(30)),
        ]);
        let parsed = parse_rule_tree(body).unwrap();
        assert_eq!(parsed.rule_tree.groups[0].conditions.len(), 2);
    }

    #[test]
    fn rejects_unknown_target() {
        let body = one_group(vec![input("not_a_target", "eq", json!(true))]);
        assert!(matches!(parse_rule_tree(body), Err(AppError::BadRequest(_))));
    }

    #[test]
    fn rejects_operator_target_mismatch() {
        let body = one_group(vec![input("isSubscribed", "gt", json!(0))]);
        assert!(matches!(parse_rule_tree(body), Err(AppError::BadRequest(_))));
    }

    #[test]
    fn int_coerces_from_string() {
        let body = one_group(vec![input("subscriberCount", "gte", json!("100"))]);
        let parsed = parse_rule_tree(body).unwrap();
        assert_eq!(parsed.rule_tree.groups[0].conditions[0].value, json!(100));
    }

    #[test]
    fn rejects_negative_int() {
        let body = one_group(vec![input("subscriberCount", "gte", json!(-5))]);
        assert!(matches!(parse_rule_tree(body), Err(AppError::BadRequest(_))));
    }

    #[test]
    fn between_requires_value_end() {
        let body = one_group(vec![input("subscriberCount", "between", json!(3))]);
        assert!(matches!(parse_rule_tree(body), Err(AppError::BadRequest(_))));
    }

    #[test]
    fn between_rejects_inverted_range() {
        let mut c = input("subscriberCount", "between", json!(100));
        c.value_end = Some(json!(10));
        let body = one_group(vec![c]);
        assert!(matches!(parse_rule_tree(body), Err(AppError::BadRequest(_))));
    }

    #[test]
    fn country_in_normalizes_csv() {
        let body = one_group(vec![input("country", "in", json!("US,CA,GB"))]);
        let parsed = parse_rule_tree(body).unwrap();
        assert_eq!(
            parsed.rule_tree.groups[0].conditions[0].value,
            json!(["US", "CA", "GB"])
        );
    }

    #[test]
    fn country_rejects_bad_code() {
        let body = one_group(vec![input("country", "eq", json!("USA"))]);
        assert!(matches!(parse_rule_tree(body), Err(AppError::BadRequest(_))));
    }

    #[test]
    fn caps_max_groups() {
        let mut groups = Vec::new();
        for _ in 0..(MAX_GROUPS + 1) {
            groups.push(ConditionGroupInput {
                conditions: vec![input("isSubscribed", "eq", json!(true))],
            });
        }
        let body = RuleTreeBody {
            channel_id: Some("UCabcdefghijklmnopqrstuv".into()),
            grant_on_any: false,
            groups,
        };
        assert!(matches!(parse_rule_tree(body), Err(AppError::BadRequest(_))));
    }

    #[test]
    fn rejects_bad_channel_id() {
        let body = RuleTreeBody {
            channel_id: Some("notachannel".into()),
            grant_on_any: false,
            groups: vec![ConditionGroupInput {
                conditions: vec![input("isSubscribed", "eq", json!(true))],
            }],
        };
        assert!(matches!(parse_rule_tree(body), Err(AppError::BadRequest(_))));
    }
}
