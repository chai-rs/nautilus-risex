//! Wire types for the RiseX WebSocket feed.
//!
//! Numeric strings stay `String`: the orderbook checksum is computed over the
//! exact venue text, and oracle prices are 18-decimal integers.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use super::enums::{
    RisexMaintenancePhase, RisexWsAckMethod, RisexWsChannel, RisexWsDataKind, RisexWsStatus,
    lenient,
};
use crate::common::enums::{
    RisexLiquidityIndicator, RisexOrderSide, RisexOrderStatus, RisexOrderType,
    RisexStopPriceOption, RisexStopType, RisexStpMode, RisexTimeInForce,
};

/// A frame the client sends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum RisexWsRequest {
    Subscribe(RisexWsSubscription),
    Unsubscribe { channel: RisexWsChannel },
    Ping,
}

/// The `params` of a subscribe request: one channel and its whole filter set.
///
/// The venue replaces a channel's filters on every subscribe, and an absent
/// `market_ids` means every market.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RisexWsSubscription {
    pub channel: RisexWsChannel,
    #[serde(skip_serializing_if = "BTreeSet::is_empty")]
    pub market_ids: BTreeSet<u32>,
    #[serde(skip_serializing_if = "BTreeSet::is_empty")]
    pub makers: BTreeSet<String>,
}

/// Every inbound text frame deserializes into this; deserializing never fails on valid JSON.
#[derive(Debug, Clone, PartialEq)]
pub enum RisexWsMessage {
    /// Reply to subscribe / unsubscribe / auth. Read `status`: a rejected subscribe still has `type: subscribed`.
    Ack(RisexWsAck),
    /// Reply to the JSON ping.
    Pong,
    Orderbook(RisexWsData<RisexWsOrderbook>),
    Trades(RisexWsData<RisexWsTrade>),
    Oracle(RisexWsData<RisexWsOracle>),
    ExpectedFundingRate(RisexWsData<Vec<RisexWsExpectedFundingRate>>),
    Funding(RisexWsData<RisexWsFunding>),
    System(RisexWsData<RisexWsSystem>),
    Orders(RisexWsData<Vec<RisexWsOrder>>),
    Fills(RisexWsData<RisexWsFill>),
    Positions(RisexWsData<Vec<RisexWsPosition>>),
    Account(RisexWsData<RisexWsAccount>),
    /// Emitted by the client after an automatic reconnect; never sent by the venue.
    /// Local books are stale from here until the next snapshot.
    Reconnected,
    /// A frame of no known kind, or one whose payload did not decode.
    Unknown(Value),
}

impl<'de> Deserialize<'de> for RisexWsMessage {
    // shortcut: decodes through `serde_json::Value` (two passes per frame),
    // switch to a borrowed peek of `channel` if profiling shows it matters.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;

        let typed = if value.get("status").is_some() {
            RisexWsAck::deserialize(&value).map(Self::Ack)
        } else if let Some(channel) = value.get("channel") {
            match RisexWsChannel::deserialize(channel) {
                Ok(RisexWsChannel::Orderbook) => {
                    Deserialize::deserialize(&value).map(Self::Orderbook)
                }
                Ok(RisexWsChannel::Trades) => Deserialize::deserialize(&value).map(Self::Trades),
                Ok(RisexWsChannel::Oracle) => Deserialize::deserialize(&value).map(Self::Oracle),
                Ok(RisexWsChannel::ExpectedFundingRate) => {
                    Deserialize::deserialize(&value).map(Self::ExpectedFundingRate)
                }
                Ok(RisexWsChannel::Funding) => Deserialize::deserialize(&value).map(Self::Funding),
                Ok(RisexWsChannel::System) => Deserialize::deserialize(&value).map(Self::System),
                Ok(RisexWsChannel::Orders) => Deserialize::deserialize(&value).map(Self::Orders),
                Ok(RisexWsChannel::Fills) => Deserialize::deserialize(&value).map(Self::Fills),
                Ok(RisexWsChannel::Positions) => {
                    Deserialize::deserialize(&value).map(Self::Positions)
                }
                Ok(RisexWsChannel::Account) => Deserialize::deserialize(&value).map(Self::Account),
                Err(e) => Err(e),
            }
        } else if value.get("method").and_then(Value::as_str) == Some("pong") {
            Ok(Self::Pong)
        } else {
            return Ok(Self::Unknown(value));
        };

        Ok(typed.unwrap_or_else(|e| {
            log::warn!("Undecodable RiseX frame: {e}");
            Self::Unknown(value)
        }))
    }
}

/// Reply to a client request.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RisexWsAck {
    pub method: RisexWsAckMethod,
    pub status: RisexWsStatus,
    #[serde(default)]
    pub message: String,
    /// Raw channel name: an `invalid channel` error echoes a name that is not a [`RisexWsChannel`].
    pub channel: Option<String>,
    /// The filter the venue applied (empty on unsubscribe, error and auth replies).
    #[serde(default)]
    pub data: RisexWsFilter,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct RisexWsFilter {
    #[serde(default)]
    pub market_ids: Vec<u32>,
    #[serde(default)]
    pub makers: Vec<String>,
}

/// Envelope of a channel data frame.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RisexWsData<T> {
    #[serde(rename = "type")]
    pub kind: RisexWsDataKind,
    /// Absent or null on frames that span markets (snapshots of account channels, oracle, account, system).
    pub market_id: Option<String>,
    pub data: T,
    /// CRC32 of the book after this update; orderbook updates only.
    pub checksum: Option<u32>,
    pub block_number: Option<u64>,
    pub log_index: Option<u64>,
    pub tx_hash: Option<String>,
    /// Unix nanoseconds.
    pub worker_timestamp: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RisexWsOrderbook {
    pub market_id: u32,
    pub bids: Vec<RisexWsBookLevel>,
    pub asks: Vec<RisexWsBookLevel>,
}

/// One price level; `quantity` "0" removes the level.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RisexWsBookLevel {
    pub price: String,
    pub quantity: String,
    pub order_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RisexWsTrade {
    pub id: String,
    pub maker_order_id: String,
    pub taker_order_id: String,
    pub maker: String,
    pub taker: String,
    /// 0 = buy, 1 = sell.
    pub maker_side: u8,
    pub price: String,
    pub size: String,
    pub fee_maker: String,
    pub fee_taker: String,
    pub fee_liquidation: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RisexWsOracle {
    /// Keyed by market id; prices are integers scaled by 1e18.
    pub prices: BTreeMap<String, RisexWsOraclePrice>,
    /// Unix seconds.
    pub timestamp: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RisexWsOraclePrice {
    pub index_price: Option<String>,
    pub mark_price: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RisexWsExpectedFundingRate {
    pub market_id: String,
    /// Unix nanoseconds.
    pub next_funding_time: String,
    /// Seconds.
    pub next_funding_in: String,
    pub estimated_rate: String,
}

/// A funding payment settled for one account.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RisexWsFunding {
    pub account: String,
    pub funding_payment: String,
    pub position_size: String,
    pub last_checkpoint: String,
    pub new_checkpoint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RisexWsSystem {
    pub is_maintenance_mode: bool,
    pub maintenance_time: u64,
    pub maintenance_end_time: u64,
    pub message: String,
    pub maintenance_phase: RisexMaintenancePhase,
    pub seconds_until_maintenance: u64,
    pub reminder_threshold_seconds: u64,
    pub market_warning: Option<RisexWsMarketWarning>,
    #[serde(default)]
    pub market_warnings: Vec<RisexWsMarketWarning>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RisexWsMarketWarning {
    pub market_id: u32,
    pub message: String,
    pub starts_at: u64,
    pub ends_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RisexWsOrder {
    pub id: String,
    pub resting_order_id: String,
    pub wide_order_id: String,
    pub client_order_id: String,
    pub market_id: String,
    /// Owner address, checksum case.
    pub sender: String,
    #[serde(deserialize_with = "lenient")]
    pub side: Option<RisexOrderSide>,
    #[serde(rename = "type", deserialize_with = "lenient")]
    pub order_type: Option<RisexOrderType>,
    #[serde(deserialize_with = "lenient")]
    pub time_in_force: Option<RisexTimeInForce>,
    #[serde(deserialize_with = "lenient")]
    pub status: Option<RisexOrderStatus>,
    pub price: String,
    pub size: String,
    pub filled_size: String,
    pub avg_price: String,
    pub fee_bps: String,
    pub post_only: bool,
    pub reduce_only: bool,
    pub is_liquidation: bool,
    #[serde(deserialize_with = "lenient")]
    pub stp_mode: Option<RisexStpMode>,
    #[serde(deserialize_with = "lenient")]
    pub stop_type: Option<RisexStopType>,
    pub stop_price: String,
    #[serde(deserialize_with = "lenient")]
    pub stop_price_option: Option<RisexStopPriceOption>,
    pub expiry: String,
    pub created_at: String,
    pub cancel_requested: bool,
    pub cancel_requested_at: String,
    pub cancel_reason: String,
    pub cancel_tx_hash: String,
    pub tx_hash: String,
    pub block_number: String,
    pub log_index: String,
}

/// Fields the venue omits when empty are `Option`. An enum field is `None` when the venue
/// sends a value this crate does not know.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RisexWsFill {
    pub id: String,
    pub market_id: String,
    pub order_id: String,
    pub client_order_id: Option<String>,
    #[serde(deserialize_with = "lenient")]
    pub side: Option<RisexOrderSide>,
    pub price: String,
    pub size: String,
    pub fee: String,
    #[serde(deserialize_with = "lenient")]
    pub liquidity_indicator: Option<RisexLiquidityIndicator>,
    pub time: String,
    pub is_liquidation: bool,
    pub is_otc: bool,
    pub realized_pnl: Option<String>,
    pub realized_pnl_percentage: Option<String>,
    pub avg_price: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub position_side: Option<RisexOrderSide>,
    pub leverage: Option<String>,
    /// 0 = cross, 1 = isolated.
    pub margin_mode: u8,
    pub blockchain_data: Option<RisexWsBlockchainData>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RisexWsBlockchainData {
    pub tx_hash: String,
    pub block_number: u64,
    pub log_index: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RisexWsPosition {
    pub account: String,
    pub market_id: String,
    /// `None` when flat (the venue sends `""`).
    #[serde(deserialize_with = "lenient")]
    pub side: Option<RisexOrderSide>,
    pub size: String,
    pub avg_entry_price: String,
    pub quote_amount: String,
    pub leverage: String,
    /// 0 = cross, 1 = isolated.
    pub margin_mode: u8,
    pub isolated_usdc_balance: String,
    pub last_funding_payment: String,
    pub unsettled_funding: String,
    pub block_number: String,
    pub log_index: String,
    pub worker_timestamp: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RisexWsAccount {
    pub collateral_balance: String,
    pub account_equity: String,
    pub cross_margin_balance: String,
    pub free_cross_margin_balance: String,
    pub withdrawable_usdc: String,
    pub total_isolated_margin_usage: String,
    pub total_cross_maintenance_margin: String,
    pub total_unrealized_pnl: String,
    pub usdc_balance: String,
    pub total_initial_margin: String,
    pub total_notional: String,
    pub realized_pnl: String,
    pub in_liquidation: bool,
    pub liquidation_risk: String,
    pub margin_ratio: String,
    pub total_account_value: String,
    pub margin_usage: String,
    pub account_leverage: String,
    pub margin_health: String,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn parse(text: &str) -> RisexWsMessage {
        serde_json::from_str(text).unwrap()
    }

    #[test]
    fn every_fixture_frame_is_known() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/ws/");
        let mut seen = BTreeMap::new();
        for file in ["public_aero_btc.jsonl", "account_channels_unauth.jsonl"] {
            let text = std::fs::read_to_string(format!("{dir}{file}")).unwrap();
            for (i, line) in text.lines().enumerate() {
                let kind = match parse(line) {
                    RisexWsMessage::Ack(ack) => {
                        assert_eq!(ack.status, RisexWsStatus::Success);
                        "ack"
                    }
                    RisexWsMessage::Pong => "pong",
                    RisexWsMessage::Orderbook(_) => "orderbook",
                    RisexWsMessage::Trades(_) => "trades",
                    RisexWsMessage::Oracle(_) => "oracle",
                    RisexWsMessage::ExpectedFundingRate(_) => "expected_funding_rate",
                    RisexWsMessage::Funding(_) => "funding",
                    RisexWsMessage::System(_) => "system",
                    RisexWsMessage::Orders(_) => "orders",
                    RisexWsMessage::Fills(_) => "fills",
                    RisexWsMessage::Positions(_) => "positions",
                    RisexWsMessage::Account(_) => "account",
                    other => panic!("{file}:{}: {other:?}", i + 1),
                };
                *seen.entry(kind).or_insert(0) += 1;
            }
        }
        let expected = [
            ("account", 39),
            ("ack", 10),
            ("expected_funding_rate", 8),
            ("fills", 5),
            ("funding", 1),
            ("oracle", 44),
            ("orderbook", 347),
            ("orders", 158),
            ("pong", 1),
            ("positions", 6),
            ("system", 1),
            ("trades", 10),
        ];
        assert_eq!(seen, BTreeMap::from(expected));
    }

    #[test]
    fn fixture_enum_values_are_known() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/ws/");
        let text = std::fs::read_to_string(format!("{dir}account_channels_unauth.jsonl")).unwrap();
        for line in text.lines() {
            match parse(line) {
                RisexWsMessage::Orders(frame) => {
                    for o in frame.data {
                        assert!(o.side.is_some());
                        assert!(o.order_type.is_some());
                        assert!(o.time_in_force.is_some());
                        assert!(o.status.is_some());
                        assert!(o.stp_mode.is_some());
                        assert!(o.stop_type.is_some());
                        assert!(o.stop_price_option.is_some());
                    }
                }
                RisexWsMessage::Fills(frame) => {
                    assert!(frame.data.side.is_some());
                    assert!(frame.data.liquidity_indicator.is_some());
                }
                RisexWsMessage::System(frame) => {
                    assert_ne!(frame.data.maintenance_phase, RisexMaintenancePhase::Unknown);
                }
                _ => {}
            }
        }
    }

    #[test]
    fn requests_match_documented_frames() {
        let sub = |channel, market_ids: &[u32], makers: &[&str]| {
            serde_json::to_value(RisexWsRequest::Subscribe(RisexWsSubscription {
                channel,
                market_ids: market_ids.iter().copied().collect(),
                makers: makers.iter().map(ToString::to_string).collect(),
            }))
            .unwrap()
        };

        assert_eq!(
            sub(RisexWsChannel::Orderbook, &[1], &[]),
            json!({"method": "subscribe", "params": {"channel": "orderbook", "market_ids": [1]}})
        );
        assert_eq!(
            sub(RisexWsChannel::Orders, &[1], &["0x36dc"]),
            json!({"method": "subscribe", "params": {"channel": "orders", "market_ids": [1], "makers": ["0x36dc"]}})
        );
        assert_eq!(
            sub(RisexWsChannel::ExpectedFundingRate, &[], &[]),
            json!({"method": "subscribe", "params": {"channel": "expected_funding_rate"}})
        );
        assert_eq!(
            serde_json::to_value(RisexWsRequest::Unsubscribe {
                channel: RisexWsChannel::Orderbook
            })
            .unwrap(),
            json!({"method": "unsubscribe", "params": {"channel": "orderbook"}})
        );
        assert_eq!(
            serde_json::to_string(&RisexWsRequest::Ping).unwrap(),
            r#"{"method":"ping"}"#
        );
    }

    #[test]
    fn replies_decode_by_status_not_type() {
        let RisexWsMessage::Ack(rejected) = parse(
            r#"{"type":"subscribed","method":"subscribe","status":"error","message":"Failed to subscribe to candles: invalid channel: candles","channel":"candles","data":{}}"#,
        ) else {
            panic!("not an ack")
        };
        assert_eq!(rejected.status, RisexWsStatus::Error);
        assert_eq!(rejected.channel.as_deref(), Some("candles"));

        let RisexWsMessage::Ack(unsubscribed) = parse(
            r#"{"type":"unsubscribed","method":"unsubscribe","status":"success","message":"Successfully unsubscribed to orderbook for all markets","channel":"orderbook"}"#,
        ) else {
            panic!("not an ack")
        };
        assert_eq!(unsubscribed.method, RisexWsAckMethod::Unsubscribe);
        assert_eq!(unsubscribed.data, RisexWsFilter::default());

        let RisexWsMessage::Ack(auth) = parse(
            r#"{"method":"auth","status":"success","message":"Authentication successful","data":{"account":"0x12","signer":"0xab"}}"#,
        ) else {
            panic!("not an ack")
        };
        assert_eq!(auth.method, RisexWsAckMethod::Auth);
        assert_eq!(auth.channel, None);
    }

    #[test]
    fn unknown_or_undecodable_frames_do_not_error() {
        for text in [
            r#"{"hello":"world"}"#,
            r#"[1,2]"#,
            r#"{"channel":"liquidations","type":"update","data":{}}"#,
            r#"{"channel":"trades","type":"update","data":{"id":1}}"#,
            r#"{"method":"teleport","status":"success"}"#,
        ] {
            assert!(matches!(parse(text), RisexWsMessage::Unknown(_)), "{text}");
        }
    }

    #[test]
    fn new_enum_values_do_not_drop_the_frame() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/ws/");
        let text = std::fs::read_to_string(format!("{dir}account_channels_unauth.jsonl")).unwrap();
        let line = text
            .lines()
            .find(|l| l.contains(r#""channel":"orders""#) && l.contains("ORDER_STATUS_OPEN"))
            .unwrap();
        let RisexWsMessage::Orders(frame) =
            parse(&line.replace("ORDER_STATUS_OPEN", "ORDER_STATUS_EXPIRED"))
        else {
            panic!("frame dropped")
        };
        assert!(frame.data.iter().all(|o| o.status.is_none()));
        assert!(frame.data.iter().all(|o| o.side.is_some()));
    }
}
