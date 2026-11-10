//! Query parameters of the RISEx REST endpoints.
//!
//! Unset fields are omitted, and list parameters are sent as repeated keys.

use crate::common::enums::{
    RisexBarInterval, RisexOrderStatus, RisexStopType, RisexTpslOrderStatus,
};

/// Query string as ordered key-value pairs.
pub type QueryParams = Vec<(&'static str, String)>;

fn push<T: ToString>(params: &mut QueryParams, key: &'static str, value: Option<T>) {
    if let Some(value) = value {
        params.push((key, value.to_string()));
    }
}

fn push_all<T: ToString>(params: &mut QueryParams, key: &'static str, values: &[T]) {
    params.extend(values.iter().map(|value| (key, value.to_string())));
}

/// Time window and page for market trades and funding rates. Times are in nanoseconds.
#[derive(Clone, Debug, Default)]
pub struct RisexPageQuery {
    pub start_time: Option<u64>,
    pub end_time: Option<u64>,
    /// 1-based.
    pub page: Option<u32>,
    pub limit: Option<u32>,
}

impl RisexPageQuery {
    #[must_use]
    pub fn params(&self) -> QueryParams {
        let mut params = Vec::new();
        push(&mut params, "start_time", self.start_time);
        push(&mut params, "end_time", self.end_time);
        push(&mut params, "page", self.page);
        push(&mut params, "limit", self.limit);
        params
    }
}

/// `GET /v1/markets/id/{market_id}/trading-view-data`. Times are in nanoseconds.
#[derive(Clone, Debug)]
pub struct RisexBarsQuery {
    pub interval: RisexBarInterval,
    pub from: Option<u64>,
    pub to: Option<u64>,
}

impl RisexBarsQuery {
    #[must_use]
    pub fn params(&self) -> QueryParams {
        let mut params = vec![("interval", self.interval.as_nanos().to_string())];
        push(&mut params, "from", self.from);
        push(&mut params, "to", self.to);
        params
    }
}

/// `GET /v1/orders`.
#[derive(Clone, Debug, Default)]
pub struct RisexOrdersQuery {
    pub account: String,
    pub market_id: Option<u16>,
    pub start_time: Option<u64>,
    pub end_time: Option<u64>,
    pub statuses: Vec<RisexOrderStatus>,
    pub page: Option<u32>,
    pub limit: Option<u32>,
    /// `created_at` (ascending) or `-created_at`.
    pub sorted_by: Option<String>,
    /// Composite order ids (hex), at most 100.
    pub order_ids: Vec<String>,
}

impl RisexOrdersQuery {
    #[must_use]
    pub fn params(&self) -> QueryParams {
        let mut params = vec![("account", self.account.clone())];
        push(&mut params, "market_id", self.market_id);
        push(&mut params, "start_time", self.start_time);
        push(&mut params, "end_time", self.end_time);
        push_all(&mut params, "statuses", &self.statuses);
        push(&mut params, "page", self.page);
        push(&mut params, "limit", self.limit);
        push(&mut params, "sorted_by", self.sorted_by.as_ref());
        push_all(&mut params, "order_ids", &self.order_ids);
        params
    }
}

/// `GET /v1/orders/open`.
#[derive(Clone, Debug, Default)]
pub struct RisexOpenOrdersQuery {
    pub account: String,
    pub market_id: Option<u16>,
    /// Pagination offset.
    pub start_index: Option<u64>,
    pub limit: Option<u32>,
    /// Composite order ids (hex), at most 100.
    pub order_ids: Vec<String>,
}

impl RisexOpenOrdersQuery {
    #[must_use]
    pub fn params(&self) -> QueryParams {
        let mut params = vec![("account", self.account.clone())];
        push(&mut params, "market_id", self.market_id);
        push(&mut params, "start_index", self.start_index);
        push(&mut params, "limit", self.limit);
        push_all(&mut params, "order_ids", &self.order_ids);
        params
    }
}

/// `GET /v1/trade-history`.
#[derive(Clone, Debug, Default)]
pub struct RisexAccountTradesQuery {
    pub account: String,
    pub market_id: Option<u16>,
    pub start_time: Option<u64>,
    pub end_time: Option<u64>,
    pub page: Option<u32>,
    pub limit: Option<u32>,
    /// `time` (ascending) or `-time`.
    pub sorted_by: Option<String>,
}

impl RisexAccountTradesQuery {
    #[must_use]
    pub fn params(&self) -> QueryParams {
        let mut params = vec![("account", self.account.clone())];
        push(&mut params, "market_id", self.market_id);
        push(&mut params, "start_time", self.start_time);
        push(&mut params, "end_time", self.end_time);
        push(&mut params, "page", self.page);
        push(&mut params, "limit", self.limit);
        push(&mut params, "sorted_by", self.sorted_by.as_ref());
        params
    }
}

/// `GET /v1/positions`.
#[derive(Clone, Debug, Default)]
pub struct RisexPositionsQuery {
    pub account: String,
    pub market_id: Option<u16>,
    pub page: Option<u32>,
    pub page_size: Option<u32>,
}

impl RisexPositionsQuery {
    #[must_use]
    pub fn params(&self) -> QueryParams {
        let mut params = vec![("account", self.account.clone())];
        push(&mut params, "market_id", self.market_id);
        push(&mut params, "page", self.page);
        push(&mut params, "page_size", self.page_size);
        params
    }
}

/// `GET /v1/orders/tpsl`.
#[derive(Clone, Debug, Default)]
pub struct RisexTpslOrdersQuery {
    pub account: String,
    pub market_id: Option<u16>,
    pub start_time: Option<u64>,
    pub end_time: Option<u64>,
    pub page: Option<u32>,
    pub limit: Option<u32>,
    pub statuses: Vec<RisexTpslOrderStatus>,
    pub stop_type: Option<RisexStopType>,
}

impl RisexTpslOrdersQuery {
    #[must_use]
    pub fn params(&self) -> QueryParams {
        let mut params = vec![("account", self.account.clone())];
        push(&mut params, "market_id", self.market_id);
        push(&mut params, "start_time", self.start_time);
        push(&mut params, "end_time", self.end_time);
        push(&mut params, "page", self.page);
        push(&mut params, "limit", self.limit);
        push_all(&mut params, "statuses", &self.statuses);
        push(&mut params, "stop_type", self.stop_type);
        params
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    fn test_unset_fields_are_omitted() {
        assert!(RisexPageQuery::default().params().is_empty());

        let query = RisexPageQuery {
            page: Some(2),
            limit: Some(50),
            ..Default::default()
        };
        assert_eq!(
            query.params(),
            vec![("page", "2".to_string()), ("limit", "50".to_string())]
        );
    }

    #[rstest]
    fn test_lists_are_repeated_keys() {
        let query = RisexOrdersQuery {
            account: "0xabc".to_string(),
            statuses: vec![RisexOrderStatus::Open, RisexOrderStatus::Filled],
            order_ids: vec!["0x01".to_string()],
            ..Default::default()
        };

        assert_eq!(
            query.params(),
            vec![
                ("account", "0xabc".to_string()),
                ("statuses", "ORDER_STATUS_OPEN".to_string()),
                ("statuses", "ORDER_STATUS_FILLED".to_string()),
                ("order_ids", "0x01".to_string()),
            ]
        );
    }

    #[rstest]
    fn test_bar_interval_is_sent_in_nanoseconds() {
        let query = RisexBarsQuery {
            interval: RisexBarInterval::Minute5,
            from: Some(1),
            to: None,
        };

        assert_eq!(
            query.params(),
            vec![
                ("interval", "300000000000".to_string()),
                ("from", "1".to_string())
            ]
        );
    }
}
