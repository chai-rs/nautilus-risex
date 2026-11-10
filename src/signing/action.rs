//! Action hashes signed through a permit:
//! `keccak256(abi.encode(keccak256(SELECTOR), operands...))`, each operand one
//! 32-byte word.

use alloy_primitives::{B256, U256, keccak256};
use serde::Serialize;

use super::{SigningError, ser_str};

/// `priceTicks` is a 24-bit field of the packed order.
pub const MAX_PRICE_TICKS: u32 = (1 << 24) - 1;

fn action_hash(selector: &str, words: &[U256]) -> B256 {
    let mut buf = Vec::with_capacity(32 * (words.len() + 1));
    buf.extend_from_slice(keccak256(selector).as_slice());
    for word in words {
        buf.extend_from_slice(&word.to_be_bytes::<32>());
    }
    keccak256(buf)
}

/// Order fields of `POST /v1/orders/place`, with the wire names and integer
/// codes, so the signed values and the request body cannot drift apart.
///
/// Codes: `side` 0 buy / 1 sell; `order_type` 0 market / 1 limit;
/// `time_in_force` 0 GTC / 1 GTT / 2 FOK / 3 IOC; `stp_mode` 0 expire maker /
/// 1 expire taker / 2 expire both. Zero means "none" for `builder_id`,
/// `client_order_id` and `ttl_units`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlaceOrder {
    pub market_id: u16,
    pub size_steps: u32,
    pub price_ticks: u32,
    pub side: u8,
    pub post_only: bool,
    pub reduce_only: bool,
    pub stp_mode: u8,
    pub order_type: u8,
    pub time_in_force: u8,
    pub builder_id: u16,
    #[serde(serialize_with = "ser_str")]
    pub client_order_id: u64,
    pub ttl_units: u16,
}

impl PlaceOrder {
    /// The packed `uint88` order word.
    ///
    /// Out-of-range values are rejected: masking them would sign a different
    /// order than the one requested.
    pub fn order_data(&self) -> Result<u128, SigningError> {
        for (name, value, max) in [
            ("price_ticks", self.price_ticks, MAX_PRICE_TICKS),
            ("side", self.side.into(), 1),
            ("stp_mode", self.stp_mode.into(), 2),
            ("order_type", self.order_type.into(), 1),
            ("time_in_force", self.time_in_force.into(), 3),
        ] {
            if value > max {
                return Err(SigningError::OutOfRange(name));
            }
        }
        let flags = u128::from(self.side)
            | (u128::from(self.post_only) << 1)
            | (u128::from(self.reduce_only) << 2)
            | (u128::from(self.stp_mode) << 3)
            | (u128::from(self.order_type) << 5)
            | (u128::from(self.time_in_force) << 6);
        Ok((u128::from(self.market_id) << 70)
            | (u128::from(self.size_steps) << 38)
            | (u128::from(self.price_ticks) << 14)
            | (flags << 6)
            | (1 << 1)) // version 1
    }

    /// Header byte: bit 0 always set, then one bit per optional field present.
    pub fn header_flags(&self) -> u8 {
        0x01 | (u8::from(self.builder_id != 0) << 1)
            | (u8::from(self.client_order_id != 0) << 2)
            | (u8::from(self.ttl_units != 0) << 4)
    }

    // shortcut: no `builder_fee_bps` (it adds a word when > 0 and the docs give
    // no header flag for it); add it when builder fees are used.
    pub fn action_hash(&self) -> Result<B256, SigningError> {
        Ok(action_hash(
            "RISE_PERPS_PLACE_ORDER_V1",
            &[
                U256::from(self.header_flags()),
                U256::from(self.order_data()?),
                U256::from(self.builder_id),
                U256::from(self.client_order_id),
                U256::from(self.ttl_units),
            ],
        ))
    }
}

/// `resting_order_id` is the field of that name from `/v1/orders/open`
/// (`wide_order_id >> 1`), not the `order_id` sent in the cancel body.
pub fn cancel_order_hash(market_id: u16, resting_order_id: u64) -> B256 {
    action_hash(
        "RISE_PERPS_CANCEL_ORDER_V1",
        &[U256::from(market_id), U256::from(resting_order_id)],
    )
}

pub fn cancel_all_orders_hash(market_id: u16) -> B256 {
    action_hash("RISE_PERPS_CANCEL_ALL_ORDERS_V1", &[U256::from(market_id)])
}

pub fn update_leverage_hash(market_id: u16, leverage: u8) -> B256 {
    action_hash(
        "RISE_PERPS_UPDATE_LEVERAGE_V1",
        &[U256::from(market_id), U256::from(leverage)],
    )
}

/// `margin_mode`: 0 cross, 1 isolated.
pub fn update_margin_mode_hash(market_id: u16, margin_mode: u8) -> B256 {
    action_hash(
        "RISE_PERPS_UPDATE_MARGIN_MODE_V1",
        &[U256::from(market_id), U256::from(margin_mode)],
    )
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;
    use crate::signing::testing::{b256, vectors};

    fn num<T: TryFrom<u64>>(v: &Value, key: &str) -> T {
        T::try_from(v[key].as_u64().unwrap()).ok().unwrap()
    }

    fn order(v: &Value) -> PlaceOrder {
        PlaceOrder {
            market_id: num(v, "market_id"),
            size_steps: num(v, "size_steps"),
            price_ticks: num(v, "price_ticks"),
            side: num(v, "side"),
            post_only: v["post_only"].as_bool().unwrap(),
            reduce_only: v["reduce_only"].as_bool().unwrap(),
            stp_mode: num(v, "stp_mode"),
            order_type: num(v, "order_type"),
            time_in_force: num(v, "time_in_force"),
            builder_id: num(v, "builder_id"),
            client_order_id: num(v, "client_order_id"),
            ttl_units: num(v, "ttl_units"),
        }
    }

    fn cases(name: &str) -> Vec<Value> {
        let cases = vectors()[name].as_array().unwrap().clone();
        assert!(cases.len() >= 6, "{name}");
        cases
    }

    #[test]
    fn place_matches_reference() {
        for v in cases("place") {
            let o = order(&v);
            let want = v["order_data"].as_str().unwrap();
            assert_eq!(format!("{:#034x}", o.order_data().unwrap()), want);
            assert_eq!(o.header_flags(), num::<u8>(&v, "header_flags"));
            assert_eq!(o.action_hash().unwrap(), b256(&v["hash"]));
        }
    }

    #[test]
    fn other_actions_match_reference() {
        for v in cases("cancel") {
            let hash = cancel_order_hash(num(&v, "market_id"), num(&v, "resting_order_id"));
            assert_eq!(hash, b256(&v["hash"]));
        }
        for v in cases("cancel_all") {
            assert_eq!(
                cancel_all_orders_hash(num(&v, "market_id")),
                b256(&v["hash"])
            );
        }
        for v in cases("leverage") {
            let hash = update_leverage_hash(num(&v, "market_id"), num(&v, "leverage"));
            assert_eq!(hash, b256(&v["hash"]));
        }
        for v in cases("margin_mode") {
            let hash = update_margin_mode_hash(num(&v, "market_id"), num(&v, "margin_mode"));
            assert_eq!(hash, b256(&v["hash"]));
        }
    }

    #[test]
    fn out_of_range_fields_are_rejected() {
        let ok = order(&cases("place")[0]);
        let bad = [
            PlaceOrder {
                price_ticks: MAX_PRICE_TICKS + 1,
                ..ok.clone()
            },
            PlaceOrder {
                side: 2,
                ..ok.clone()
            },
            PlaceOrder {
                stp_mode: 3,
                ..ok.clone()
            },
            PlaceOrder {
                order_type: 2,
                ..ok.clone()
            },
            PlaceOrder {
                time_in_force: 4,
                ..ok.clone()
            },
        ];
        for o in bad {
            assert!(
                matches!(o.action_hash(), Err(SigningError::OutOfRange(_))),
                "{o:?}"
            );
        }
    }

    #[test]
    fn body_uses_wire_names_and_string_u64() {
        let json = serde_json::to_value(order(&cases("place")[2])).unwrap();
        assert_eq!(json["client_order_id"], "42");
        assert_eq!(json["time_in_force"], 3);
        assert_eq!(json["reduce_only"], true);
    }
}
