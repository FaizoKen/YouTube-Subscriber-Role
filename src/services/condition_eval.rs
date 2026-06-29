//! In-memory rule-tree evaluation (the per-player sync path).
//!
//! Mirrors the SQL pushdown in [crate::services::rule_sql] exactly so the
//! per-player and per-role-link paths never disagree: a group matches when all
//! its conditions hold (AND), and the tree matches when any group matches (OR).
//! Missing facts fail closed.

use chrono::{DateTime, Utc};

use crate::models::condition::{Condition, ConditionOperator, ConditionTarget};
use crate::models::rule::RuleTree;

/// Facts about one linked member, relative to the configured channel.
pub struct PlayerYouTubeData {
    pub is_subscribed: bool,
    pub subscribed_at: Option<DateTime<Utc>>,
    pub subscriber_count: Option<i64>,
    pub view_count: Option<i64>,
    pub video_count: Option<i64>,
    pub channel_created_at: Option<DateTime<Utc>>,
    pub hidden_subscribers: bool,
    pub country: Option<String>,
    pub custom_url: Option<String>,
}

/// Evaluate the rule tree (DNF). `grant_on_any` short-circuits to true; an
/// empty tree or an empty group matches nobody.
pub fn evaluate_rule_tree(tree: &RuleTree, data: &PlayerYouTubeData) -> bool {
    if tree.grant_on_any {
        return true;
    }
    tree.groups
        .iter()
        .any(|g| !g.conditions.is_empty() && g.conditions.iter().all(|c| evaluate_single(c, data)))
}

fn evaluate_single(condition: &Condition, data: &PlayerYouTubeData) -> bool {
    match condition.target {
        ConditionTarget::IsSubscribed => {
            let expected = condition.value.as_bool().unwrap_or(true);
            data.is_subscribed == expected
        }
        ConditionTarget::HasCustomUrl => {
            let has = data.custom_url.as_ref().is_some_and(|u| !u.is_empty());
            let expected = condition.value.as_bool().unwrap_or(true);
            has == expected
        }
        ConditionTarget::Country => match condition.operator {
            ConditionOperator::In => {
                let Some(country) = data.country.as_ref() else {
                    return false;
                };
                condition
                    .value
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str())
                            .any(|c| c.eq_ignore_ascii_case(country))
                    })
                    .unwrap_or(false)
            }
            _ => {
                let expected = condition.value.as_str().unwrap_or("");
                data.country
                    .as_ref()
                    .is_some_and(|c| c.eq_ignore_ascii_case(expected))
            }
        },
        // Numeric targets.
        target => {
            let actual: Option<i64> = match target {
                ConditionTarget::SubscriptionAgeDays => {
                    data.subscribed_at.map(|ts| (Utc::now() - ts).num_days())
                }
                ConditionTarget::SubscriberCount => {
                    if data.hidden_subscribers {
                        return false;
                    }
                    data.subscriber_count
                }
                ConditionTarget::ViewCount => data.view_count,
                ConditionTarget::VideoCount => data.video_count,
                ConditionTarget::ChannelAgeDays => data
                    .channel_created_at
                    .map(|ts| (Utc::now() - ts).num_days()),
                _ => unreachable!("non-numeric target handled above"),
            };
            let Some(actual) = actual else {
                return false;
            };
            let expected = condition.value.as_i64().unwrap_or(0);
            compare(actual, expected, condition.operator, &condition.value_end)
        }
    }
}

fn compare(
    actual: i64,
    expected: i64,
    op: ConditionOperator,
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
        // `In` only applies to string (country) targets, handled before this.
        ConditionOperator::In => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::condition::{Condition, ConditionOperator as Op, ConditionTarget as T};
    use crate::models::rule::{ConditionGroup, RuleTree};
    use serde_json::json;

    fn facts(is_subscribed: bool) -> PlayerYouTubeData {
        PlayerYouTubeData {
            is_subscribed,
            subscribed_at: Some(Utc::now() - chrono::Duration::days(45)),
            subscriber_count: Some(500),
            view_count: Some(10_000),
            video_count: Some(20),
            channel_created_at: Some(Utc::now() - chrono::Duration::days(400)),
            hidden_subscribers: false,
            country: Some("US".into()),
            custom_url: Some("@creator".into()),
        }
    }

    fn cond(t: T, op: Op, v: serde_json::Value) -> Condition {
        Condition {
            target: t,
            operator: op,
            value: v,
            value_end: None,
        }
    }

    fn tree(groups: Vec<Vec<Condition>>) -> RuleTree {
        RuleTree {
            grant_on_any: false,
            groups: groups
                .into_iter()
                .map(|conditions| ConditionGroup { conditions })
                .collect(),
        }
    }

    #[test]
    fn grant_on_any_matches_everyone() {
        let t = RuleTree {
            grant_on_any: true,
            groups: vec![],
        };
        assert!(evaluate_rule_tree(&t, &facts(false)));
    }

    #[test]
    fn empty_tree_matches_nobody() {
        assert!(!evaluate_rule_tree(&RuleTree::default(), &facts(true)));
    }

    #[test]
    fn subscription_with_age_gate() {
        let t = tree(vec![vec![
            cond(T::IsSubscribed, Op::Eq, json!(true)),
            cond(T::SubscriptionAgeDays, Op::Gte, json!(30)),
        ]]);
        assert!(evaluate_rule_tree(&t, &facts(true)));
        assert!(!evaluate_rule_tree(&t, &facts(false)));
    }

    #[test]
    fn or_of_groups() {
        // (subscribed) OR (>=1000 subs). Member is subscribed with 500 subs.
        let t = tree(vec![
            vec![cond(T::IsSubscribed, Op::Eq, json!(true))],
            vec![cond(T::SubscriberCount, Op::Gte, json!(1000))],
        ]);
        assert!(evaluate_rule_tree(&t, &facts(true)));
        // Not subscribed and only 500 subs → neither group matches.
        assert!(!evaluate_rule_tree(&t, &facts(false)));
    }

    #[test]
    fn subscriber_count_rule_grants_without_subscription() {
        // Stat-only rule: the member's own audience size qualifies them even
        // when they never subscribed to the configured channel.
        let t = tree(vec![vec![cond(T::SubscriberCount, Op::Gte, json!(100))]]);
        assert!(evaluate_rule_tree(&t, &facts(false)));
        // ...but still fails closed below the threshold.
        let t = tree(vec![vec![cond(T::SubscriberCount, Op::Gte, json!(501))]]);
        assert!(!evaluate_rule_tree(&t, &facts(false)));
    }

    #[test]
    fn hidden_subscribers_fails_count_condition() {
        let mut f = facts(true);
        f.hidden_subscribers = true;
        let t = tree(vec![vec![cond(T::SubscriberCount, Op::Gte, json!(100))]]);
        assert!(!evaluate_rule_tree(&t, &f));
    }

    #[test]
    fn country_in_list() {
        let t = tree(vec![vec![cond(T::Country, Op::In, json!(["gb", "us"]))]]);
        assert!(evaluate_rule_tree(&t, &facts(true)));
    }

    #[test]
    fn matches_sql_for_between() {
        let mut c = cond(T::VideoCount, Op::Between, json!(10));
        c.value_end = Some(json!(30));
        let t = tree(vec![vec![c]]);
        assert!(evaluate_rule_tree(&t, &facts(true))); // 20 in [10,30]
    }
}
