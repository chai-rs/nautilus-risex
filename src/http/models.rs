//! Serde models for RISEx REST payloads.
//!
//! Unknown fields are ignored. Integers the venue sends as strings are decoded to integers,
//! and decimal fields that can be an empty string are decoded as zero or `None`.

use nautilus_core::serialization::{
    deserialize_decimal, deserialize_optional_decimal, deserialize_string_to_u64,
};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::common::{
    enums::{
        RisexLiquidityIndicator, RisexMarginMode, RisexOrderSide, RisexOrderStatus, RisexOrderType,
        RisexSessionKeyStatus, RisexStopPriceOption, RisexStopType, RisexStpMode, RisexTimeInForce,
        RisexTpslOrderStatus,
    },
    parse::{deserialize_market_id, deserialize_wei},
};
use crate::signing::{action::PlaceOrder, permit::Permit};

/// Success envelope of every REST response.
#[derive(Clone, Debug, Deserialize)]
pub struct RisexResponse<T> {
    pub data: T,
    #[serde(default)]
    pub request_id: Option<String>,
}

/// Error envelope of every REST response.
#[derive(Clone, Debug, Deserialize)]
pub struct RisexErrorResponse {
    pub error: RisexErrorBody,
    #[serde(default)]
    pub request_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RisexErrorBody {
    pub code: String,
    #[serde(default)]
    pub message: String,
}

/// `GET /v1/markets`.
#[derive(Clone, Debug, Deserialize)]
pub struct RisexMarkets {
    pub markets: Vec<RisexMarket>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RisexMarket {
    #[serde(deserialize_with = "deserialize_market_id")]
    pub market_id: u16,
    pub config: RisexMarketConfig,
    #[serde(default)]
    pub quote_asset_symbol: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default, deserialize_with = "deserialize_optional_decimal")]
    pub last_price: Option<Decimal>,
    #[serde(default, deserialize_with = "deserialize_optional_decimal")]
    pub mark_price: Option<Decimal>,
    #[serde(default, deserialize_with = "deserialize_optional_decimal")]
    pub index_price: Option<Decimal>,
    #[serde(default, deserialize_with = "deserialize_optional_decimal")]
    pub current_funding_rate: Option<Decimal>,
    /// Nanoseconds.
    #[serde(default, deserialize_with = "deserialize_string_to_u64")]
    pub funding_interval: u64,
    #[serde(default, deserialize_with = "deserialize_string_to_u64")]
    pub next_funding_time: u64,
    #[serde(default)]
    pub post_only: bool,
    #[serde(default)]
    pub active: bool,
    #[serde(default)]
    pub reduce_only: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RisexMarketConfig {
    /// Venue market name, for example `AERO/USDC`.
    pub name: String,
    /// Quote token address.
    #[serde(default)]
    pub quote: String,
    pub step_size: Decimal,
    pub step_price: Decimal,
    pub maintenance_margin_factor: Decimal,
    pub max_leverage: Decimal,
    pub min_order_size: Decimal,
    #[serde(default)]
    pub unlocked: bool,
    #[serde(default, deserialize_with = "deserialize_optional_decimal")]
    pub open_interest_limit: Option<Decimal>,
    #[serde(default, deserialize_with = "deserialize_optional_decimal")]
    pub match_price_band: Option<Decimal>,
}

/// `GET /v1/system/config`.
#[derive(Clone, Debug, Deserialize)]
pub struct RisexSystemConfig {
    pub chain: RisexChainConfig,
    pub addresses: RisexAddresses,
    #[serde(default)]
    pub is_maintenance_mode: bool,
    #[serde(default)]
    pub maintenance_phase: String,
    #[serde(default)]
    pub maintenance_message: String,
    #[serde(default)]
    pub market_warnings: Vec<RisexMarketWarning>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RisexChainConfig {
    pub name: String,
    #[serde(deserialize_with = "deserialize_string_to_u64")]
    pub chain_id: u64,
}

/// Contract addresses. `usdc` is the `token` of `GET /v1/account/balance`.
#[derive(Clone, Debug, Deserialize)]
pub struct RisexAddresses {
    pub router: String,
    pub auth: String,
    pub perps_manager: String,
    pub orders_manager: String,
    pub collateral_manager: String,
    pub fee_manager: String,
    pub markets_config: String,
    pub usdc: String,
    pub oracle: String,
    pub stork: String,
    pub deposit: String,
    pub multicall3: String,
    pub operator_hub: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RisexMarketWarning {
    #[serde(deserialize_with = "deserialize_market_id")]
    pub market_id: u16,
    pub message: String,
    /// Seconds.
    #[serde(default, deserialize_with = "deserialize_string_to_u64")]
    pub starts_at: u64,
    /// Seconds.
    #[serde(default, deserialize_with = "deserialize_string_to_u64")]
    pub ends_at: u64,
}

/// `GET /v1/auth/eip712-domain`.
#[derive(Clone, Debug, Deserialize)]
pub struct RisexEip712Domain {
    pub name: String,
    pub version: String,
    #[serde(deserialize_with = "deserialize_string_to_u64")]
    pub chain_id: u64,
    pub verifying_contract: String,
}

/// `GET /v1/error-codes`.
#[derive(Clone, Debug, Deserialize)]
pub struct RisexErrorCodes {
    pub errors: Vec<RisexErrorCode>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RisexErrorCode {
    #[serde(rename = "ref")]
    pub reference: String,
    pub code: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub grpc_code: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub action: String,
    #[serde(default)]
    pub severity: String,
    #[serde(default)]
    pub retry_allowed: bool,
    #[serde(default)]
    pub http_status: u16,
}

/// `GET /v1/orderbook`. Bids are sorted best (highest) first, asks best (lowest) first.
#[derive(Clone, Debug, Deserialize)]
pub struct RisexOrderBook {
    #[serde(deserialize_with = "deserialize_market_id")]
    pub market_id: u16,
    pub bids: Vec<RisexBookLevel>,
    pub asks: Vec<RisexBookLevel>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RisexBookLevel {
    pub price: Decimal,
    pub quantity: Decimal,
    #[serde(default)]
    pub order_count: u32,
}

/// `GET /v1/markets/id/{market_id}/trade-history`. Newest first.
#[derive(Clone, Debug, Deserialize)]
pub struct RisexTrades {
    #[serde(deserialize_with = "deserialize_market_id")]
    pub market_id: u16,
    pub trades: Vec<RisexTrade>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RisexTrade {
    /// `{maker order id}-{taker order id}`.
    pub id: String,
    pub maker_side: RisexOrderSide,
    pub price: Decimal,
    pub size: Decimal,
    /// Nanoseconds.
    #[serde(deserialize_with = "deserialize_string_to_u64")]
    pub time: u64,
    #[serde(default, deserialize_with = "deserialize_string_to_u64")]
    pub block_number: u64,
    #[serde(default, deserialize_with = "deserialize_string_to_u64")]
    pub log_index: u64,
}

/// `GET /v1/markets/id/{market_id}/trading-view-data`. Oldest first.
#[derive(Clone, Debug, Deserialize)]
pub struct RisexBars {
    pub data: Vec<RisexBar>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RisexBar {
    #[serde(deserialize_with = "deserialize_market_id")]
    pub market_id: u16,
    /// Interval the server used, for example `1m`. It differs from the request when the
    /// requested interval is not supported.
    pub interval: String,
    /// Nanoseconds.
    #[serde(deserialize_with = "deserialize_string_to_u64")]
    pub time: u64,
    pub low: Decimal,
    pub high: Decimal,
    pub open: Decimal,
    pub close: Decimal,
    pub volume: Decimal,
}

/// `GET /v1/markets/id/{market_id}/funding-rate-history`. Newest first.
#[derive(Clone, Debug, Deserialize)]
pub struct RisexFundingRates {
    #[serde(deserialize_with = "deserialize_market_id")]
    pub market_id: u16,
    pub records: Vec<RisexFundingRate>,
    #[serde(default)]
    pub page: u32,
    #[serde(default)]
    pub has_next_page: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RisexFundingRate {
    pub funding_rate: Decimal,
    #[serde(default, deserialize_with = "deserialize_optional_decimal")]
    pub accumulated_funding: Option<Decimal>,
    #[serde(default, deserialize_with = "deserialize_optional_decimal")]
    pub index_price: Option<Decimal>,
    /// Nanoseconds.
    #[serde(deserialize_with = "deserialize_string_to_u64")]
    pub start_time: u64,
    /// Nanoseconds.
    #[serde(deserialize_with = "deserialize_string_to_u64")]
    pub end_time: u64,
    #[serde(default, deserialize_with = "deserialize_string_to_u64")]
    pub block_number: u64,
    #[serde(default, deserialize_with = "deserialize_string_to_u64")]
    pub block_time: u64,
    #[serde(default)]
    pub tx_hash: String,
}

/// `GET /v1/nonce-state/{account}`.
#[derive(Clone, Debug, Deserialize)]
pub struct RisexNonceState {
    #[serde(deserialize_with = "deserialize_string_to_u64")]
    pub nonce_anchor: u64,
    pub current_bitmap_index: u32,
    /// Hex bitmap of the used nonces at `current_bitmap_index`.
    pub bitmap: String,
}

/// `GET /v1/orders`.
#[derive(Clone, Debug, Deserialize)]
pub struct RisexOrders {
    pub orders: Vec<RisexOrder>,
    #[serde(default)]
    pub page: u32,
    #[serde(default)]
    pub has_next_page: bool,
}

/// `GET /v1/orders/by-id/{order_id}`.
#[derive(Clone, Debug, Deserialize)]
pub struct RisexOrderResponse {
    pub order: RisexOrder,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RisexOrder {
    /// Composite order id (hex).
    pub id: String,
    #[serde(deserialize_with = "deserialize_decimal")]
    pub price: Decimal,
    #[serde(deserialize_with = "deserialize_decimal")]
    pub size: Decimal,
    #[serde(deserialize_with = "deserialize_market_id")]
    pub market_id: u16,
    pub side: RisexOrderSide,
    #[serde(rename = "type")]
    pub order_type: RisexOrderType,
    pub time_in_force: RisexTimeInForce,
    /// Nanoseconds, `0` when the order does not expire.
    #[serde(default, deserialize_with = "deserialize_string_to_u64")]
    pub expiry: u64,
    #[serde(default)]
    pub post_only: bool,
    #[serde(default)]
    pub reduce_only: bool,
    #[serde(default)]
    pub cancel_reason: String,
    #[serde(default, deserialize_with = "deserialize_string_to_u64")]
    pub block_number: u64,
    #[serde(default, deserialize_with = "deserialize_string_to_u64")]
    pub log_index: u64,
    #[serde(default)]
    pub stp_mode: Option<RisexStpMode>,
    #[serde(default, deserialize_with = "deserialize_decimal")]
    pub filled_size: Decimal,
    pub status: RisexOrderStatus,
    #[serde(default)]
    pub sender: String,
    #[serde(default, deserialize_with = "deserialize_decimal")]
    pub avg_price: Decimal,
    #[serde(default)]
    pub cancel_requested: bool,
    /// Nanoseconds.
    #[serde(default, deserialize_with = "deserialize_string_to_u64")]
    pub created_at: u64,
    #[serde(default)]
    pub tx_hash: String,
    /// The `u64` sent at order placement, `0` when none.
    #[serde(default, deserialize_with = "deserialize_string_to_u64")]
    pub client_order_id: u64,
    /// Meaningful only when `stop_price` is set: plain orders carry a default value here.
    #[serde(default)]
    pub stop_type: Option<RisexStopType>,
    #[serde(default, deserialize_with = "deserialize_optional_decimal")]
    pub stop_price: Option<Decimal>,
    #[serde(default)]
    pub stop_price_option: Option<RisexStopPriceOption>,
    #[serde(default)]
    pub is_liquidation: bool,
    #[serde(default, deserialize_with = "deserialize_string_to_u64")]
    pub wide_order_id: u64,
    #[serde(default, deserialize_with = "deserialize_string_to_u64")]
    pub resting_order_id: u64,
}

/// `GET /v1/orders/open`. Rows come from the matching engine: integer steps and ticks, no fill
/// state and no timestamps.
#[derive(Clone, Debug, Deserialize)]
pub struct RisexOpenOrders {
    pub orders: Vec<RisexOpenOrder>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RisexOpenOrder {
    pub order_id: String,
    #[serde(deserialize_with = "deserialize_string_to_u64")]
    pub wide_order_id: u64,
    #[serde(deserialize_with = "deserialize_string_to_u64")]
    pub resting_order_id: u64,
    #[serde(deserialize_with = "deserialize_market_id")]
    pub market_id: u16,
    #[serde(default)]
    pub account: String,
    pub side: RisexOrderSide,
    /// Size in units of the market `step_size`.
    pub size_steps: u64,
    /// Price in units of the market `step_price`.
    pub price_ticks: u64,
    pub order_type: RisexOrderType,
    pub time_in_force: RisexTimeInForce,
    #[serde(default)]
    pub post_only: bool,
    #[serde(default)]
    pub reduce_only: bool,
    #[serde(default, deserialize_with = "deserialize_string_to_u64")]
    pub client_order_id: u64,
}

/// `GET /v1/trade-history`.
#[derive(Clone, Debug, Deserialize)]
pub struct RisexAccountTrades {
    pub trades: Vec<RisexAccountTrade>,
    #[serde(default)]
    pub page: u32,
    #[serde(default)]
    pub has_next_page: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RisexAccountTrade {
    /// `{maker order id}-{taker order id}`.
    pub id: String,
    #[serde(deserialize_with = "deserialize_market_id")]
    pub market_id: u16,
    /// Composite id (hex) of the account's own order.
    pub order_id: String,
    pub side: RisexOrderSide,
    pub price: Decimal,
    pub size: Decimal,
    /// Signed: negative is a rebate.
    #[serde(deserialize_with = "deserialize_decimal")]
    pub fee: Decimal,
    pub liquidity_indicator: RisexLiquidityIndicator,
    /// Nanoseconds.
    #[serde(deserialize_with = "deserialize_string_to_u64")]
    pub time: u64,
    #[serde(default)]
    pub blockchain_data: Option<RisexBlockchainData>,
    #[serde(default)]
    pub is_liquidation: bool,
    #[serde(default, deserialize_with = "deserialize_decimal")]
    pub realized_pnl: Decimal,
    #[serde(default)]
    pub margin_mode: Option<RisexMarginMode>,
    #[serde(default, deserialize_with = "deserialize_string_to_u64")]
    pub client_order_id: u64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RisexBlockchainData {
    #[serde(default, deserialize_with = "deserialize_string_to_u64")]
    pub block_number: u64,
    #[serde(default)]
    pub tx_hash: String,
    #[serde(default, deserialize_with = "deserialize_string_to_u64")]
    pub log_index: u64,
}

/// `GET /v1/positions`. Amounts are sent in wei (18 decimals) and decoded to decimals here.
#[derive(Clone, Debug, Deserialize)]
pub struct RisexPositions {
    pub positions: Vec<RisexPosition>,
    #[serde(default)]
    pub total_count: u32,
    #[serde(default)]
    pub page: u32,
    #[serde(default)]
    pub page_size: u32,
    #[serde(default)]
    pub has_next_page: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RisexPosition {
    #[serde(default)]
    pub account: String,
    #[serde(deserialize_with = "deserialize_market_id")]
    pub market_id: u16,
    /// Signed: negative for a short position.
    #[serde(deserialize_with = "deserialize_wei")]
    pub size: Decimal,
    #[serde(deserialize_with = "deserialize_wei")]
    pub quote_amount: Decimal,
    #[serde(default)]
    pub margin_mode: Option<RisexMarginMode>,
    pub side: RisexOrderSide,
    #[serde(deserialize_with = "deserialize_wei")]
    pub isolated_usdc_balance: Decimal,
    #[serde(deserialize_with = "deserialize_wei")]
    pub last_funding_payment: Decimal,
    #[serde(deserialize_with = "deserialize_wei")]
    pub leverage: Decimal,
    #[serde(deserialize_with = "deserialize_wei")]
    pub avg_entry_price: Decimal,
    #[serde(deserialize_with = "deserialize_wei")]
    pub unsettled_funding: Decimal,
}

/// `GET /v1/account/position`. Amounts are plain decimals, unlike `GET /v1/positions`.
#[derive(Clone, Debug, Deserialize)]
pub struct RisexAccountPositionResponse {
    pub position: RisexAccountPosition,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RisexAccountPosition {
    #[serde(deserialize_with = "deserialize_decimal")]
    pub size: Decimal,
    #[serde(default, deserialize_with = "deserialize_decimal")]
    pub quote_amount: Decimal,
    #[serde(default, deserialize_with = "deserialize_decimal")]
    pub last_funding_payment: Decimal,
    #[serde(default)]
    pub margin_mode: Option<RisexMarginMode>,
    pub side: RisexOrderSide,
    #[serde(default, deserialize_with = "deserialize_decimal")]
    pub isolated_usdc_balance: Decimal,
    #[serde(default, deserialize_with = "deserialize_decimal")]
    pub leverage: Decimal,
    #[serde(default, deserialize_with = "deserialize_optional_decimal")]
    pub avg_entry_price: Option<Decimal>,
    #[serde(default, deserialize_with = "deserialize_optional_decimal")]
    pub mark_price: Option<Decimal>,
    #[serde(default, deserialize_with = "deserialize_optional_decimal")]
    pub unrealized_pnl: Option<Decimal>,
    #[serde(default, deserialize_with = "deserialize_optional_decimal")]
    pub liquidation_price: Option<Decimal>,
    #[serde(default)]
    pub in_isolated_liquidation: bool,
}

/// `GET /v1/account/balance` and `GET /v1/account/cross-margin-balance`.
#[derive(Clone, Debug, Deserialize)]
pub struct RisexBalance {
    #[serde(deserialize_with = "deserialize_decimal")]
    pub balance: Decimal,
}

/// `GET /v1/orders/tpsl`.
#[derive(Clone, Debug, Deserialize)]
pub struct RisexTpslOrders {
    pub orders: Vec<RisexTpslOrder>,
    #[serde(default, deserialize_with = "deserialize_string_to_u64")]
    pub total: u64,
    #[serde(default)]
    pub page: u32,
    #[serde(default)]
    pub limit: u32,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RisexTpslOrder {
    /// UUID, not a composite order id.
    pub order_id: String,
    #[serde(default)]
    pub account: String,
    #[serde(deserialize_with = "deserialize_market_id")]
    pub market_id: u16,
    pub side: RisexOrderSide,
    #[serde(deserialize_with = "deserialize_decimal")]
    pub size: Decimal,
    pub stop_type: RisexStopType,
    pub order_type: RisexOrderType,
    #[serde(default, deserialize_with = "deserialize_optional_decimal")]
    pub stop_price: Option<Decimal>,
    #[serde(default, deserialize_with = "deserialize_optional_decimal")]
    pub limit_price: Option<Decimal>,
    #[serde(default)]
    pub stop_price_option: Option<RisexStopPriceOption>,
    pub status: RisexTpslOrderStatus,
    pub tif: RisexTimeInForce,
    /// Seconds, unlike the nanosecond timestamps of regular orders.
    #[serde(default, deserialize_with = "deserialize_string_to_u64")]
    pub created_at: u64,
    #[serde(default, deserialize_with = "deserialize_string_to_u64")]
    pub expires_at: u64,
    #[serde(default, deserialize_with = "deserialize_string_to_u64")]
    pub triggered_at: u64,
    #[serde(default, deserialize_with = "deserialize_optional_decimal")]
    pub triggered_price: Option<Decimal>,
    #[serde(default)]
    pub triggered_order_id: String,
    #[serde(default)]
    pub cancel_reason: String,
    #[serde(default)]
    pub size_percent_bps: u32,
    #[serde(default, deserialize_with = "deserialize_decimal")]
    pub filled_size: Decimal,
    #[serde(default)]
    pub parent_order_id: String,
    #[serde(default)]
    pub reduce_only: bool,
    #[serde(default)]
    pub slippage_bps: u32,
}

/// `GET /v1/auth/session-key-status`.
#[derive(Clone, Debug, Deserialize)]
pub struct RisexSessionKeyStatusResponse {
    pub status: RisexSessionKeyStatus,
    #[serde(default)]
    pub status_description: String,
}

// Signed POST bodies and responses.
//
// unverified: the shapes follow the docs. No signed request was sent to the venue, so the
// field names, the `permit_params` shape and the response fields may differ.

/// Body of `POST /v1/orders/place`.
#[derive(Clone, Debug, Serialize)]
pub struct RisexPlaceOrderRequest {
    #[serde(flatten)]
    pub order: PlaceOrder,
    pub permit: Permit,
}

/// Response of `POST /v1/orders/place`. `order_id` is `scOrderId-blockNumber-logIndex`.
#[derive(Clone, Debug, Deserialize)]
pub struct RisexPlaceOrderResponse {
    pub order_id: String,
    #[serde(default)]
    pub message: String,
}

/// Body of `POST /v1/orders/cancel`. `order_id` is the 0x hex id.
#[derive(Clone, Debug, Serialize)]
pub struct RisexCancelOrderRequest {
    pub market_id: u16,
    pub order_id: String,
    pub permit: Permit,
    pub no_retry: bool,
}

/// Body of `POST /v1/orders/cancel-all`.
#[derive(Clone, Debug, Serialize)]
pub struct RisexCancelAllRequest {
    pub market_id: u16,
    pub permit: Permit,
}

/// Body of `POST /v1/account/leverage`.
#[derive(Clone, Debug, Serialize)]
pub struct RisexLeverageRequest {
    pub market_id: String,
    pub leverage: String,
    pub permit_params: Permit,
}

/// Body of `POST /v1/account/margin-mode`.
#[derive(Clone, Debug, Serialize)]
pub struct RisexMarginModeRequest {
    pub market_id: String,
    pub margin_mode: u8,
    pub permit_params: Permit,
}

/// Body of `POST /v1/auth/revoke-signer`. `account_signature` is 0x hex, by the account key.
#[derive(Clone, Debug, Serialize)]
pub struct RisexRevokeSignerRequest {
    pub account: String,
    pub signer: String,
    pub nonce_anchor: String,
    pub nonce_bitmap_index: u8,
    pub account_signature: String,
}

/// Response of the cancel, cancel-all, TP/SL cancel and revoke endpoints. An absent
/// `success` counts as success: the HTTP status already was 2xx.
#[derive(Clone, Debug, Deserialize)]
pub struct RisexSuccessResponse {
    #[serde(default = "default_true")]
    pub success: bool,
}

const fn default_true() -> bool {
    true
}

/// Response of `POST /v1/orders/tpsl`. `order_id` is a UUID.
#[derive(Clone, Debug, Deserialize)]
pub struct RisexTpslPlaceResponse {
    pub order_id: String,
}

#[cfg(test)]
pub(crate) mod tests {
    use rstest::rstest;
    use rust_decimal_macros::dec;
    use serde::de::DeserializeOwned;

    use super::*;

    /// Loads `tests/fixtures/http/{name}.json` and unwraps the envelope.
    pub(crate) fn fixture<T: DeserializeOwned>(name: &str) -> T {
        let path = format!(
            "{}/tests/fixtures/http/{name}.json",
            env!("CARGO_MANIFEST_DIR")
        );
        let json = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
        serde_json::from_str::<RisexResponse<T>>(&json)
            .unwrap_or_else(|e| panic!("{path}: {e}"))
            .data
    }

    #[rstest]
    fn test_markets_fixture() {
        let markets: RisexMarkets = fixture("markets");
        assert_eq!(markets.markets.len(), 54);

        let aero = markets.markets.iter().find(|m| m.market_id == 15).unwrap();
        assert_eq!(aero.config.name, "AERO/USDC");
        assert_eq!(aero.config.step_size, dec!(1));
        assert_eq!(aero.config.step_price, dec!(0.00001));
        assert_eq!(aero.config.min_order_size, dec!(20));
        assert_eq!(aero.config.max_leverage, dec!(3));
        assert_eq!(aero.funding_interval, 3_600_000_000_000);
        assert!(aero.active);

        let inactive: Vec<u16> = markets
            .markets
            .iter()
            .filter(|m| !m.active)
            .map(|m| m.market_id)
            .collect();
        assert_eq!(inactive, vec![13]);
    }

    #[rstest]
    fn test_config_fixtures() {
        let config: RisexSystemConfig = fixture("system_config");
        assert_eq!(config.chain.chain_id, 4153);
        assert_eq!(
            config.addresses.usdc,
            "0xe436820ba0c69702c1d3e601d421c0ef38262739"
        );
        assert!(!config.is_maintenance_mode);
        assert_eq!(config.market_warnings[0].market_id, 25);

        let domain: RisexEip712Domain = fixture("eip712_domain");
        assert_eq!(
            (domain.name.as_str(), domain.version.as_str()),
            ("RISEx", "1")
        );
        assert_eq!(domain.chain_id, 4153);
        assert_eq!(
            domain.verifying_contract,
            "0x0D919DAA3f12AE715744Eb648c00066c5DBd66f0"
        );

        let codes: RisexErrorCodes = fixture("error_codes");
        assert_eq!(codes.errors[0].reference, "E0001_AuthRequired");
        assert_eq!(codes.errors[0].http_status, 401);
    }

    #[rstest]
    fn test_market_data_fixtures() {
        let book: RisexOrderBook = fixture("orderbook_aero");
        assert_eq!(book.market_id, 15);
        assert_eq!(
            (book.bids[0].price, book.bids[0].quantity),
            (dec!(0.55296), dec!(12155))
        );
        assert_eq!(
            (book.asks[0].price, book.asks[0].quantity),
            (dec!(0.55313), dec!(12708))
        );

        let trades: RisexTrades = fixture("trades_aero");
        assert_eq!(trades.trades.len(), 50);
        assert_eq!(trades.trades[0].maker_side, RisexOrderSide::Buy);
        assert_eq!(trades.trades[0].time, 1_794_280_444_000_000_000);
        assert_eq!(trades.trades[0].log_index, 210);

        let bars: RisexBars = fixture("bars_aero_1m");
        assert_eq!(bars.data.len(), 60);
        assert!(
            bars.data
                .iter()
                .all(|b| b.interval == "1m" && b.market_id == 15)
        );
        assert_eq!(bars.data[0].open, dec!(0.553425));

        let funding: RisexFundingRates = fixture("funding_aero");
        assert_eq!(funding.records.len(), 50);
        assert!(funding.has_next_page);
        assert_eq!(funding.records[0].funding_rate, dec!(0.000040527567452405));
        assert_eq!(funding.records[0].end_time, 1_794_279_600_000_000_000);
    }

    #[rstest]
    fn test_order_fixtures() {
        let orders: RisexOrders = fixture("orders_history");
        assert_eq!(orders.orders.len(), 20);
        let order = &orders.orders[0];
        assert_eq!(order.market_id, 19);
        assert_eq!(order.order_type, RisexOrderType::Limit);
        assert_eq!(order.time_in_force, RisexTimeInForce::Gtc);
        assert_eq!(order.status, RisexOrderStatus::Open);
        assert_eq!(order.stop_price, None);
        assert_eq!(order.client_order_id, 1_794_280_473_050_412_575);
        assert_eq!(order.wide_order_id, 85_899_350_359);

        let open: RisexOpenOrders = fixture("orders_open");
        assert_eq!(open.orders.len(), 635);
        let order = &open.orders[0];
        assert_eq!((order.market_id, order.side), (5, RisexOrderSide::Sell));
        assert_eq!((order.size_steps, order.price_ticks), (71_231, 74_109));
        assert_eq!(order.order_type, RisexOrderType::Limit);
        assert_eq!(order.time_in_force, RisexTimeInForce::Gtc);

        let tpsl: RisexTpslOrders = fixture("tpsl_orders");
        assert_eq!(tpsl.total, 9);
        let order = &tpsl.orders[0];
        assert_eq!(order.stop_type, RisexStopType::StopLoss);
        assert_eq!(order.status, RisexTpslOrderStatus::Cancelled);
        assert_eq!(order.tif, RisexTimeInForce::Ioc);
        assert_eq!(order.stop_price, Some(dec!(0.16)));
        assert_eq!(order.triggered_price, None);
        assert_eq!(order.created_at, 1_790_049_416);
    }

    #[rstest]
    fn test_account_fixtures() {
        let trades: RisexAccountTrades = fixture("account_trades");
        assert_eq!(trades.trades.len(), 20);
        let trade = &trades.trades[0];
        assert_eq!(trade.fee, dec!(-0.000912384));
        assert_eq!(trade.liquidity_indicator, RisexLiquidityIndicator::Maker);
        assert_eq!(trade.margin_mode, Some(RisexMarginMode::Cross));
        assert_eq!(
            trade.blockchain_data.as_ref().unwrap().block_number,
            26_674_716
        );

        let positions: RisexPositions = fixture("positions");
        assert_eq!(positions.positions.len(), 53);
        let position = &positions.positions[0];
        assert_eq!(position.size, dec!(4.208681));
        assert_eq!(position.leverage, dec!(50));
        assert_eq!(position.avg_entry_price, dec!(74943.503816206781632597));

        let position: RisexAccountPositionResponse = fixture("position_aero");
        assert_eq!(position.position.size, dec!(99299));
        assert_eq!(position.position.side, RisexOrderSide::Buy);
        assert_eq!(position.position.avg_entry_price, None);

        let balance: RisexBalance = fixture("balance");
        assert_eq!(balance.balance, dec!(1306817.015899829987975656));
        let cross: RisexBalance = fixture("cross_margin_balance");
        assert_eq!(cross.balance, dec!(1179950.284106973748053944));

        let nonce: RisexNonceState = fixture("nonce_state");
        assert_eq!(
            (nonce.nonce_anchor, nonce.current_bitmap_index),
            (2_527_068, 195)
        );
        assert!(nonce.bitmap.starts_with("0x7fff"));

        let status: RisexSessionKeyStatusResponse = fixture("session_key_status");
        assert_eq!(status.status, RisexSessionKeyStatus::NotExist);
    }
}
