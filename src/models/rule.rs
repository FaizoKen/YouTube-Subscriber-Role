//! The rule tree: OR of AND-groups (DNF).
//!
//! Stored verbatim as the JSONB `rule_tree` column on `role_links`. The
//! two-level structure keeps validation, SQL translation, and the iframe
//! rule-builder UI simple while still expressing any boolean rule (every
//! boolean expression has a DNF form).
//!
//! Invariant: an unconfigured role link grants the role to nobody.
//! `grant_on_any = false` AND `groups.is_empty()` means "match nobody" — both
//! [crate::services::condition_eval::evaluate_rule_tree] and the SQL builder
//! ([crate::services::rule_sql::build_rule_where]) enforce this before
//! inspecting groups. An empty group likewise matches nobody.

use serde::{Deserialize, Serialize};

use crate::models::condition::Condition;

/// Maximum top-level OR-groups per role.
pub const MAX_GROUPS: usize = 8;
/// Maximum AND-conditions per group.
pub const MAX_CONDITIONS_PER_GROUP: usize = 12;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RuleTree {
    /// Grants to every linked member regardless of subscription (channel-agnostic).
    #[serde(default)]
    pub grant_on_any: bool,
    /// OR of AND-groups. Empty (with `grant_on_any = false`) matches nobody.
    #[serde(default)]
    pub groups: Vec<ConditionGroup>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConditionGroup {
    #[serde(default)]
    pub conditions: Vec<Condition>,
}

impl RuleTree {
    /// Whether any condition in any group reads the member's own channel stats
    /// (`channel_cache`). Drives lazy vs eager channel-stat fetching.
    pub fn needs_channel_cache(&self) -> bool {
        self.groups
            .iter()
            .flat_map(|g| &g.conditions)
            .any(|c| c.target.needs_channel_cache())
    }
}
