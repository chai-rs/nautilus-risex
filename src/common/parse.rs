//! Parsing helpers shared by the HTTP and WebSocket layers.

use std::str::FromStr;

use alloy_primitives::{hex, keccak256};
use nautilus_model::{
    identifiers::TradeId,
    types::{Price, Quantity},
};
use rust_decimal::Decimal;
use serde::{Deserialize, Deserializer, de};

use crate::common::consts::WEI_DECIMALS;

/// Parses a plain decimal string (every channel except `oracle`).
///
/// # Errors
///
/// Returns an error if `value` is not a decimal number.
pub fn parse_decimal(value: &str) -> anyhow::Result<Decimal> {
    Decimal::from_str(value).map_err(|e| anyhow::anyhow!("invalid decimal {value:?}: {e}"))
}

/// Parses an 18-decimal integer string (wei), as used by the `oracle` channel and
/// `/v1/positions`.
///
/// # Errors
///
/// Returns an error if `value` is not an integer or does not fit a [`Decimal`].
pub fn parse_wei(value: &str) -> anyhow::Result<Decimal> {
    let raw = value
        .parse::<i128>()
        .map_err(|e| anyhow::anyhow!("invalid wei value {value:?}: {e}"))?;
    Decimal::try_from_i128_with_scale(raw, WEI_DECIMALS)
        .map(|d| d.normalize())
        .map_err(|e| anyhow::anyhow!("wei value {value:?} out of range: {e}"))
}

/// Converts a decimal to a [`Price`], rounding to `precision`.
///
/// # Errors
///
/// Returns an error if the value does not fit a [`Price`].
pub fn price_from_decimal(value: Decimal, precision: u8) -> anyhow::Result<Price> {
    Price::from_decimal_dp(value, precision)
        .map_err(|e| anyhow::anyhow!("invalid price {value}: {e}"))
}

/// Converts a decimal to a [`Quantity`], rounding to `precision`.
///
/// # Errors
///
/// Returns an error if the value is negative or does not fit a [`Quantity`].
pub fn quantity_from_decimal(value: Decimal, precision: u8) -> anyhow::Result<Quantity> {
    Quantity::from_decimal_dp(value, precision)
        .map_err(|e| anyhow::anyhow!("invalid quantity {value}: {e}"))
}

#[derive(Deserialize)]
#[serde(untagged)]
enum NumberOrString {
    Number(u64),
    String(String),
}

/// Deserializes a market ID, which REST sends as a string on most endpoints and as a
/// number on `/v1/orders/open` and on WebSocket payloads.
///
/// # Errors
///
/// Returns an error if the value is not an integer that fits `u16`.
pub fn deserialize_market_id<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u16, D::Error> {
    let value = match NumberOrString::deserialize(deserializer)? {
        NumberOrString::Number(value) => value,
        NumberOrString::String(value) => value.parse().map_err(de::Error::custom)?,
    };
    u16::try_from(value).map_err(de::Error::custom)
}

/// Deserializes an 18-decimal integer string (wei) into a [`Decimal`].
///
/// # Errors
///
/// Returns an error if the value is not a wei integer string.
pub fn deserialize_wei<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Decimal, D::Error> {
    parse_wei(&String::deserialize(deserializer)?).map_err(de::Error::custom)
}

/// Composite order ID: the 24-byte hex the venue uses as `order_id`, split into its parts.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct RisexOrderId {
    /// Smart-contract (wide) order ID.
    pub wide_order_id: u64,
    /// Block the order was placed in.
    pub block_number: u64,
    /// Log index of the placement event.
    pub log_index: u64,
}

impl RisexOrderId {
    /// Parses the 24-byte hex form (`0x` + 48 hex digits).
    ///
    /// # Errors
    ///
    /// Returns an error if `value` is not `0x` followed by 48 hex digits.
    pub fn from_hex(value: &str) -> anyhow::Result<Self> {
        let digits = value.strip_prefix("0x").unwrap_or_default();
        anyhow::ensure!(
            digits.len() == 48 && digits.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid order id {value:?}: expected 0x + 48 hex digits"
        );
        let part = |i: usize| u64::from_str_radix(&digits[i * 16..(i + 1) * 16], 16);
        Ok(Self {
            wide_order_id: part(0)?,
            block_number: part(1)?,
            log_index: part(2)?,
        })
    }

    /// Parses the dash form `scOrderId-blockNumber-logIndex` returned when placing an order.
    ///
    /// # Errors
    ///
    /// Returns an error if `value` does not have three integer parts.
    pub fn from_dash(value: &str) -> anyhow::Result<Self> {
        // The docs give no example of this form: decimal parts are assumed and `0x` hex
        // parts are tolerated.
        let part = |text: &str| match text.strip_prefix("0x") {
            Some(digits) => u64::from_str_radix(digits, 16),
            None => text.parse::<u64>(),
        };
        let parts: Vec<&str> = value.split('-').collect();
        let [wide, block, log] = parts.as_slice() else {
            anyhow::bail!("invalid order id {value:?}: expected scOrderId-blockNumber-logIndex");
        };
        let invalid = |e| anyhow::anyhow!("invalid order id {value:?}: {e}");
        Ok(Self {
            wide_order_id: part(wide).map_err(invalid)?,
            block_number: part(block).map_err(invalid)?,
            log_index: part(log).map_err(invalid)?,
        })
    }

    /// Returns the 24-byte hex form the REST and WebSocket APIs use.
    #[must_use]
    pub fn to_hex(&self) -> String {
        format!(
            "0x{:016x}{:016x}{:016x}",
            self.wide_order_id, self.block_number, self.log_index
        )
    }

    /// Returns the ID the order has while resting in the book.
    #[must_use]
    pub const fn resting_order_id(&self) -> u64 {
        self.wide_order_id >> 1
    }
}

/// Maps a Nautilus client order ID to the venue's `u64` client order ID.
///
/// Takes the first 8 bytes of `keccak256(client_order_id)`, big-endian. Never returns 0,
/// which the venue reads as "no client order ID".
#[must_use]
pub fn client_order_id_to_u64(client_order_id: &str) -> u64 {
    let hash = keccak256(client_order_id.as_bytes());
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&hash[..8]);
    u64::from_be_bytes(bytes).max(1)
}

/// Maps a venue trade ID to a [`TradeId`].
///
/// The venue ID is `{maker order id}-{taker order id}` (101 characters), which exceeds the
/// 36-character [`TradeId`] limit, so this returns the first 16 bytes of its keccak256 as hex.
/// The mapping is the same for REST and WebSocket trades and fills.
#[must_use]
pub fn trade_id_from_venue(id: &str) -> TradeId {
    TradeId::new(hex::encode(&keccak256(id.as_bytes())[..16]))
}

#[cfg(test)]
mod tests {
    use rstest::rstest;
    use rust_decimal_macros::dec;

    use super::*;

    const ORDER_ID_HEX: &str = "0x0000000400001155000000000196f5b700000000000001b8";

    #[rstest]
    fn test_parse_decimal_and_wei() {
        assert_eq!(parse_decimal("0.55296").unwrap(), dec!(0.55296));
        assert_eq!(parse_decimal("-0.000912384").unwrap(), dec!(-0.000912384));
        assert!(parse_decimal("").is_err());
        assert_eq!(
            parse_wei("552969981808949806").unwrap(),
            dec!(0.552969981808949806)
        );
        assert_eq!(
            parse_wei("-315034155406102134007404").unwrap(),
            dec!(-315034.155406102134007404)
        );
        assert_eq!(parse_wei("50000000000000000000").unwrap(), dec!(50));
        assert_eq!(parse_wei("0").unwrap(), Decimal::ZERO);
        assert!(parse_wei("0.5").is_err());
    }

    #[rstest]
    fn test_price_and_quantity_round_to_precision() {
        // Bar prices carry more decimals than the tick.
        assert_eq!(
            price_from_decimal(dec!(0.553425), 5).unwrap(),
            Price::from("0.55342")
        );
        assert_eq!(
            quantity_from_decimal(dec!(33), 0).unwrap(),
            Quantity::from("33")
        );
        assert!(quantity_from_decimal(dec!(-1), 0).is_err());
    }

    #[rstest]
    #[case("15", Some(15))]
    #[case("\"15\"", Some(15))]
    #[case("70000", None)]
    #[case("\"abc\"", None)]
    fn test_deserialize_market_id(#[case] json: &str, #[case] expected: Option<u16>) {
        #[derive(Deserialize)]
        struct Wrapper(#[serde(deserialize_with = "deserialize_market_id")] u16);

        assert_eq!(
            serde_json::from_str::<Wrapper>(json).ok().map(|w| w.0),
            expected
        );
    }

    #[rstest]
    fn test_order_id_from_hex_matches_documented_example() {
        let id = RisexOrderId::from_hex(ORDER_ID_HEX).unwrap();
        assert_eq!(id.wide_order_id, 17_179_873_621);
        assert_eq!(id.block_number, 26_670_519);
        assert_eq!(id.log_index, 440);
        assert_eq!(id.resting_order_id(), 8_589_936_810);
        assert_eq!(id.to_hex(), ORDER_ID_HEX);
    }

    #[rstest]
    #[case("17179873621-26670519-440")]
    #[case("0x400001155-0x196f5b7-0x1b8")]
    fn test_order_id_from_dash_builds_hex(#[case] dash: &str) {
        assert_eq!(
            RisexOrderId::from_dash(dash).unwrap().to_hex(),
            ORDER_ID_HEX
        );
    }

    #[rstest]
    #[case("")]
    #[case("0000000400001155000000000196f5b700000000000001b8")]
    #[case("0x0000000400001155000000000196f5b700000000000001")]
    #[case("0x+000000400001155000000000196f5b700000000000001b8")]
    fn test_order_id_from_hex_rejects(#[case] value: &str) {
        assert!(RisexOrderId::from_hex(value).is_err());
    }

    #[rstest]
    #[case("1-2")]
    #[case("1-2-3-4")]
    #[case("a-2-3")]
    #[case("-1-2-3")]
    fn test_order_id_from_dash_rejects(#[case] value: &str) {
        assert!(RisexOrderId::from_dash(value).is_err());
    }

    #[rstest]
    fn test_client_order_id_to_u64_is_stable_and_non_zero() {
        let id = client_order_id_to_u64("O-20261110-000001-001-001-1");
        assert_ne!(id, 0);
        assert_eq!(id, client_order_id_to_u64("O-20261110-000001-001-001-1"));
        assert_ne!(id, client_order_id_to_u64("O-20261110-000001-001-001-2"));
        // keccak256("") starts with 0xc5d2460186f7233c
        assert_eq!(client_order_id_to_u64(""), 0xc5d2_4601_86f7_233c);
    }

    #[rstest]
    fn test_trade_id_from_venue_fits_trade_id() {
        let venue_id = "0x000000100000115500000000019705ea0000000000000113-0x00000000000173e600000000019705fd00000000000000d3";
        let trade_id = trade_id_from_venue(venue_id);
        assert_eq!(trade_id.as_str().len(), 32);
        assert_eq!(trade_id, trade_id_from_venue(venue_id));
        assert_ne!(trade_id, trade_id_from_venue(&venue_id[..100]));
    }
}
