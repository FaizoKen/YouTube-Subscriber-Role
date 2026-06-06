//! Condition target / operator types used in the rule tree.
//!
//! - `ConditionTarget` names a fact we can read about a (member, channel) pair:
//!   either their subscription to the configured channel (`subscription_cache`)
//!   or a statistic of their *own* YouTube channel (`channel_cache`).
//! - `ConditionOperator` names a comparison.
//! - Validity of a (target, operator) combination is enforced at save time in
//!   [crate::services::rule_validator] using each target's `kind()`.
//!
//! Target keys are **camelCase** (`isSubscribed`, `subscriptionAgeDays`, …) to
//! stay compatible with the JSON already stored by earlier versions of the
//! plugin and with the migration backfill.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// What kind of data this target produces. Drives which operators are valid and
/// how the rule_validator coerces literal values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetKind {
    Bool,
    Int,
    String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConditionTarget {
    // -- subscription to the configured channel (read from subscription_cache) --
    IsSubscribed,
    SubscriptionAgeDays,

    // -- the member's own channel stats (read from channel_cache) --
    SubscriberCount,
    ViewCount,
    VideoCount,
    ChannelAgeDays,
    Country,
    HasCustomUrl,
}

impl ConditionTarget {
    pub fn kind(self) -> TargetKind {
        use ConditionTarget::*;
        match self {
            IsSubscribed | HasCustomUrl => TargetKind::Bool,
            SubscriptionAgeDays | SubscriberCount | ViewCount | VideoCount | ChannelAgeDays => {
                TargetKind::Int
            }
            Country => TargetKind::String,
        }
    }

    /// UI grouping for the rule-builder catalog: "subscription" facts vs the
    /// member's own "channel" stats.
    pub fn group(self) -> &'static str {
        use ConditionTarget::*;
        match self {
            IsSubscribed | SubscriptionAgeDays => "subscription",
            _ => "channel",
        }
    }

    pub fn as_str(self) -> &'static str {
        use ConditionTarget::*;
        match self {
            IsSubscribed => "isSubscribed",
            SubscriptionAgeDays => "subscriptionAgeDays",
            SubscriberCount => "subscriberCount",
            ViewCount => "viewCount",
            VideoCount => "videoCount",
            ChannelAgeDays => "channelAgeDays",
            Country => "country",
            HasCustomUrl => "hasCustomUrl",
        }
    }

    pub fn label(self) -> &'static str {
        use ConditionTarget::*;
        match self {
            IsSubscribed => "Subscribed to the channel",
            SubscriptionAgeDays => "Days subscribed",
            SubscriberCount => "Subscriber count (their channel)",
            ViewCount => "Total views (their channel)",
            VideoCount => "Video count (their channel)",
            ChannelAgeDays => "Channel age in days (their channel)",
            Country => "Country (their channel)",
            HasCustomUrl => "Has a custom URL (their channel)",
        }
    }

    pub fn from_key(s: &str) -> Option<Self> {
        use ConditionTarget::*;
        Some(match s {
            "isSubscribed" => IsSubscribed,
            "subscriptionAgeDays" => SubscriptionAgeDays,
            "subscriberCount" => SubscriberCount,
            "viewCount" => ViewCount,
            "videoCount" => VideoCount,
            "channelAgeDays" => ChannelAgeDays,
            "country" => Country,
            "hasCustomUrl" => HasCustomUrl,
            _ => return None,
        })
    }

    /// Whether evaluating this target needs the `channel_cache` row (the
    /// member's own channel stats). Subscription targets do not.
    pub fn needs_channel_cache(self) -> bool {
        !matches!(self, Self::IsSubscribed | Self::SubscriptionAgeDays)
    }

    /// Whether evaluating this target needs the `subscription_cache` row for
    /// the configured channel.
    pub fn needs_subscription(self) -> bool {
        matches!(self, Self::IsSubscribed | Self::SubscriptionAgeDays)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConditionOperator {
    Eq,
    Gt,
    Gte,
    Lt,
    Lte,
    Between,
    In,
}

impl ConditionOperator {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Eq => "eq",
            Self::Gt => "gt",
            Self::Gte => "gte",
            Self::Lt => "lt",
            Self::Lte => "lte",
            Self::Between => "between",
            Self::In => "in",
        }
    }

    pub fn from_key(s: &str) -> Option<Self> {
        Some(match s {
            "eq" => Self::Eq,
            "gt" => Self::Gt,
            "gte" => Self::Gte,
            "lt" => Self::Lt,
            "lte" => Self::Lte,
            "between" => Self::Between,
            "in" => Self::In,
            _ => return None,
        })
    }

    /// Operators that produce a meaningful predicate on each target kind.
    /// Save-time validation rejects mismatches.
    pub fn valid_for(self, kind: TargetKind) -> bool {
        use ConditionOperator::*;
        match kind {
            TargetKind::Bool => matches!(self, Eq),
            TargetKind::Int => matches!(self, Eq | Gt | Gte | Lt | Lte | Between),
            TargetKind::String => matches!(self, Eq | In),
        }
    }

    pub fn needs_value_end(self) -> bool {
        matches!(self, Self::Between)
    }

    pub fn value_is_list(self) -> bool {
        matches!(self, Self::In)
    }
}

/// A single condition row inside an AND-group.
///
/// `target` accepts the legacy key `field` as an alias so any condition JSON
/// written before the iframe migration still deserializes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Condition {
    #[serde(alias = "field")]
    pub target: ConditionTarget,
    pub operator: ConditionOperator,
    pub value: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_end: Option<Value>,
}
