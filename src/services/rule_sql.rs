//! SQL WHERE-clause builder for bulk per-role-link sync.
//!
//! Pushes the same DNF semantics as
//! [crate::services::condition_eval::evaluate_rule_tree] down into Postgres so
//! `sync_for_role_link` filters server-side instead of loading every linked
//! member's facts into memory.
//!
//! The clause references two aliases the caller must LEFT JOIN and provide:
//!   * `sc` — subscription_cache (for the configured channel; columns may be NULL)
//!   * `cc` — channel_cache (the member's own channel stats; columns may be NULL)
//!
//! NULL-handling matches the Rust evaluator's fail-closed behavior: a member
//! with no `subscription_cache` / `channel_cache` row is treated as
//! not-subscribed / stats-unknown, so int/string comparisons against NULL
//! yield NULL (not matched).

use crate::models::condition::{Condition, ConditionOperator, ConditionTarget};
use crate::models::rule::RuleTree;

#[derive(Debug, Clone)]
pub enum Bind {
    Bool(bool),
    Int(i64),
    Text(String),
    TextArray(Vec<String>),
}

/// Returns ("clause", binds). Binds use parameter indices starting at
/// `bind_offset + 1`. `grant_on_any = true` ⇒ "TRUE"; `grant_on_any = false`
/// AND no groups ⇒ "FALSE" (match nobody).
pub fn build_rule_where(tree: &RuleTree, bind_offset: usize) -> (String, Vec<Bind>) {
    if tree.grant_on_any {
        return ("TRUE".to_string(), vec![]);
    }
    if tree.groups.is_empty() {
        return ("FALSE".to_string(), vec![]);
    }

    let mut binds: Vec<Bind> = Vec::new();
    let mut group_clauses: Vec<String> = Vec::new();

    for group in &tree.groups {
        if group.conditions.is_empty() {
            group_clauses.push("FALSE".to_string());
            continue;
        }
        let mut cond_clauses: Vec<String> = Vec::new();
        for c in &group.conditions {
            cond_clauses.push(build_condition(c, bind_offset, &mut binds));
        }
        group_clauses.push(format!("({})", cond_clauses.join(" AND ")));
    }

    (format!("({})", group_clauses.join(" OR ")), binds)
}

/// SQL expression for a target. Bools COALESCE to false; ints and the age
/// expressions stay NULL-able so comparisons fail closed.
fn target_expr(target: ConditionTarget) -> &'static str {
    use ConditionTarget::*;
    match target {
        IsSubscribed => "COALESCE(sc.is_subscribed, false)",
        SubscriptionAgeDays => "FLOOR(EXTRACT(EPOCH FROM (now() - sc.subscribed_at)) / 86400)",
        SubscriberCount => "cc.subscriber_count",
        ViewCount => "cc.view_count",
        VideoCount => "cc.video_count",
        ChannelAgeDays => "FLOOR(EXTRACT(EPOCH FROM (now() - cc.channel_created_at)) / 86400)",
        Country => "cc.country",
        HasCustomUrl => "(cc.custom_url IS NOT NULL AND cc.custom_url <> '')",
    }
}

fn build_condition(c: &Condition, bind_offset: usize, binds: &mut Vec<Bind>) -> String {
    use ConditionOperator::*;
    let next = |binds: &Vec<Bind>| bind_offset + binds.len() + 1;

    // Subscriber count is meaningless when the member hides it — match the Rust
    // evaluator, which fails closed for hidden counts.
    let guard = matches!(c.target, ConditionTarget::SubscriberCount);

    let clause = match (c.target, c.operator) {
        // --- Country is compared case-insensitively (US == us). ---
        (ConditionTarget::Country, Eq) => {
            let i = next(binds);
            binds.push(Bind::Text(
                c.value.as_str().unwrap_or("").to_uppercase(),
            ));
            format!("UPPER(cc.country) = ${i}")
        }
        (ConditionTarget::Country, In) => {
            let arr: Vec<String> = c
                .value
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_uppercase()))
                        .collect()
                })
                .unwrap_or_default();
            if arr.is_empty() {
                return "FALSE".to_string();
            }
            let i = next(binds);
            binds.push(Bind::TextArray(arr));
            format!("UPPER(cc.country) = ANY(${i}::text[])")
        }
        // --- Boolean targets (isSubscribed, hasCustomUrl). ---
        (_, Eq) if c.value.is_boolean() => {
            let i = next(binds);
            binds.push(Bind::Bool(c.value.as_bool().unwrap_or(false)));
            format!("{} = ${i}", target_expr(c.target))
        }
        // --- Numeric equality / comparison. ---
        (_, Eq) => {
            let i = next(binds);
            binds.push(Bind::Int(c.value.as_i64().unwrap_or(0)));
            format!("({}) = ${i}", target_expr(c.target))
        }
        (_, Gt | Gte | Lt | Lte) => {
            let i = next(binds);
            binds.push(Bind::Int(c.value.as_i64().unwrap_or(0)));
            let op = match c.operator {
                Gt => ">",
                Gte => ">=",
                Lt => "<",
                Lte => "<=",
                _ => unreachable!(),
            };
            format!("({}) {op} ${i}", target_expr(c.target))
        }
        (_, Between) => {
            let lo = c.value.as_i64().unwrap_or(0);
            let hi = c.value_end.as_ref().and_then(|v| v.as_i64()).unwrap_or(lo);
            let ia = next(binds);
            binds.push(Bind::Int(lo));
            let ib = next(binds);
            binds.push(Bind::Int(hi));
            let expr = target_expr(c.target);
            format!("(({expr}) >= ${ia} AND ({expr}) <= ${ib})")
        }
        // `In` only validates for string targets, all of which are Country
        // (handled above). Anything else is unreachable post-validation.
        (_, In) => "FALSE".to_string(),
    };

    if guard {
        format!("(COALESCE(cc.hidden_subscribers, false) = false AND {clause})")
    } else {
        clause
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::condition::{Condition, ConditionOperator as Op, ConditionTarget as T};
    use crate::models::rule::{ConditionGroup, RuleTree};
    use serde_json::json;

    fn cond(t: T, op: Op, v: serde_json::Value) -> Condition {
        Condition {
            target: t,
            operator: op,
            value: v,
            value_end: None,
        }
    }

    fn tree(grant_on_any: bool, groups: Vec<ConditionGroup>) -> RuleTree {
        RuleTree {
            grant_on_any,
            groups,
        }
    }

    #[test]
    fn grant_on_any_is_true() {
        let (sql, binds) = build_rule_where(&tree(true, vec![]), 2);
        assert_eq!(sql, "TRUE");
        assert!(binds.is_empty());
    }

    #[test]
    fn empty_is_false() {
        let (sql, _) = build_rule_where(&RuleTree::default(), 2);
        assert_eq!(sql, "FALSE");
    }

    #[test]
    fn single_group_ands_subscription_and_age() {
        let t = tree(
            false,
            vec![ConditionGroup {
                conditions: vec![
                    cond(T::IsSubscribed, Op::Eq, json!(true)),
                    cond(T::SubscriptionAgeDays, Op::Gte, json!(30)),
                ],
            }],
        );
        let (sql, binds) = build_rule_where(&t, 2);
        assert!(sql.contains(" AND "));
        assert!(sql.contains("COALESCE(sc.is_subscribed, false) = $3"));
        assert!(sql.contains(">= $4"));
        assert_eq!(binds.len(), 2);
        assert!(matches!(binds[0], Bind::Bool(true)));
        assert!(matches!(binds[1], Bind::Int(30)));
    }

    #[test]
    fn multi_group_ors() {
        let t = tree(
            false,
            vec![
                ConditionGroup {
                    conditions: vec![cond(T::IsSubscribed, Op::Eq, json!(true))],
                },
                ConditionGroup {
                    conditions: vec![cond(T::SubscriberCount, Op::Gte, json!(1000))],
                },
            ],
        );
        let (sql, binds) = build_rule_where(&t, 2);
        assert!(sql.contains(" OR "));
        assert_eq!(binds.len(), 2);
    }

    #[test]
    fn subscriber_count_adds_hidden_guard() {
        let t = tree(
            false,
            vec![ConditionGroup {
                conditions: vec![cond(T::SubscriberCount, Op::Gte, json!(100))],
            }],
        );
        let (sql, _) = build_rule_where(&t, 2);
        assert!(sql.contains("COALESCE(cc.hidden_subscribers, false) = false"));
        assert!(sql.contains("cc.subscriber_count"));
    }

    #[test]
    fn country_eq_is_case_insensitive() {
        let t = tree(
            false,
            vec![ConditionGroup {
                conditions: vec![cond(T::Country, Op::Eq, json!("us"))],
            }],
        );
        let (sql, binds) = build_rule_where(&t, 2);
        assert!(sql.contains("UPPER(cc.country) = $3"));
        match &binds[0] {
            Bind::Text(s) => assert_eq!(s, "US"),
            _ => panic!("expected uppercased text bind"),
        }
    }

    #[test]
    fn country_in_uses_uppercased_array() {
        let t = tree(
            false,
            vec![ConditionGroup {
                conditions: vec![cond(T::Country, Op::In, json!(["us", "Gb"]))],
            }],
        );
        let (sql, binds) = build_rule_where(&t, 2);
        assert!(sql.contains("UPPER(cc.country) = ANY($3::text[])"));
        match &binds[0] {
            Bind::TextArray(v) => assert_eq!(v, &vec!["US".to_string(), "GB".to_string()]),
            _ => panic!("expected text array bind"),
        }
    }

    #[test]
    fn has_custom_url_binds_bool() {
        let t = tree(
            false,
            vec![ConditionGroup {
                conditions: vec![cond(T::HasCustomUrl, Op::Eq, json!(true))],
            }],
        );
        let (sql, binds) = build_rule_where(&t, 0);
        assert!(sql.contains("cc.custom_url IS NOT NULL"));
        assert!(matches!(binds[0], Bind::Bool(true)));
    }

    #[test]
    fn between_emits_two_binds() {
        let mut c = cond(T::SubscriberCount, Op::Between, json!(100));
        c.value_end = Some(json!(1000));
        let t = tree(false, vec![ConditionGroup { conditions: vec![c] }]);
        let (sql, binds) = build_rule_where(&t, 0);
        assert!(sql.contains(">= $1") && sql.contains("<= $2"));
        assert_eq!(binds.len(), 2);
    }
}
