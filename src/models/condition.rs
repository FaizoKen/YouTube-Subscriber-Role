use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum ConditionField {
    SubscriptionAgeDays,
    SubscriberCount,
    ViewCount,
    VideoCount,
    ChannelAgeDays,
    Country,
    HasCustomUrl,
}

impl ConditionField {
    pub fn json_key(&self) -> &'static str {
        match self {
            Self::SubscriptionAgeDays => "subscriptionAgeDays",
            Self::SubscriberCount => "subscriberCount",
            Self::ViewCount => "viewCount",
            Self::VideoCount => "videoCount",
            Self::ChannelAgeDays => "channelAgeDays",
            Self::Country => "country",
            Self::HasCustomUrl => "hasCustomUrl",
        }
    }

    /// SQL expression for this field.
    /// `sc` = subscription_cache alias, `cc` = channel_cache alias.
    /// Returns None for boolean fields that use custom WHERE clauses.
    pub fn sql_expr(&self) -> Option<&'static str> {
        match self {
            Self::SubscriptionAgeDays => {
                Some("EXTRACT(EPOCH FROM now() - sc.subscribed_at)::bigint / 86400")
            }
            Self::SubscriberCount => Some("cc.subscriber_count"),
            Self::ViewCount => Some("cc.view_count"),
            Self::VideoCount => Some("cc.video_count"),
            Self::ChannelAgeDays => {
                Some("EXTRACT(EPOCH FROM now() - cc.channel_created_at)::bigint / 86400")
            }
            Self::Country => Some("cc.country"),
            Self::HasCustomUrl => None, // uses custom clause
        }
    }

    /// Whether this field requires the channel_cache table to be joined.
    pub fn needs_channel_cache(&self) -> bool {
        !matches!(self, Self::SubscriptionAgeDays)
    }

    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "subscriptionAgeDays" => Some(Self::SubscriptionAgeDays),
            "subscriberCount" => Some(Self::SubscriberCount),
            "viewCount" => Some(Self::ViewCount),
            "videoCount" => Some(Self::VideoCount),
            "channelAgeDays" => Some(Self::ChannelAgeDays),
            "country" => Some(Self::Country),
            "hasCustomUrl" => Some(Self::HasCustomUrl),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ConditionOperator {
    Eq,
    Gt,
    Gte,
    Lt,
    Lte,
    Between,
}

impl ConditionOperator {
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "eq" => Some(Self::Eq),
            "gt" => Some(Self::Gt),
            "gte" => Some(Self::Gte),
            "lt" => Some(Self::Lt),
            "lte" => Some(Self::Lte),
            "between" => Some(Self::Between),
            _ => None,
        }
    }

    pub fn key(&self) -> &'static str {
        match self {
            Self::Eq => "eq",
            Self::Gt => "gt",
            Self::Gte => "gte",
            Self::Lt => "lt",
            Self::Lte => "lte",
            Self::Between => "between",
        }
    }

    pub fn sql_operator(&self) -> &'static str {
        match self {
            Self::Eq => "=",
            Self::Gt => ">",
            Self::Gte => ">=",
            Self::Lt => "<",
            Self::Lte => "<=",
            Self::Between => "BETWEEN",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Condition {
    pub field: ConditionField,
    pub operator: ConditionOperator,
    pub value: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value_end: Option<serde_json::Value>,
}
