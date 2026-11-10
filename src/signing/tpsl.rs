//! TP/SL orders: signed directly as EIP-712 typed data, without permit or nonce.
//!
//! Each request struct is both the signed message and the request body, so
//! the decimal strings are signed exactly as sent.

use alloy::sol;
use alloy_primitives::Address;
use serde::Serialize;

use super::{RisexSigner, SigningError, ser_str};

sol! {
    #[derive(Debug)]
    struct PlaceTpslOrder {
        address account;
        uint64 marketId;
        uint8 side;
        string size;
        uint8 stopType;
        string stopPrice;
        string limitPrice;
        uint8 orderType;
        uint8 stopPriceOption;
        uint8 tif;
        uint32 deadline;
        uint32 sizePercentBps;
    }

    #[derive(Debug)]
    struct CancelTpslOrder {
        address account;
        string orderId;
        uint32 deadline;
    }

    #[derive(Debug)]
    struct CancelAllTpslOrders {
        address account;
        uint64 marketId;
        uint32 deadline;
    }
}

/// Body of `POST /v1/orders/tpsl`, without `signer` and `signature`.
///
/// Codes (protobuf enum numbers, inferred from the OpenAPI enum order):
/// `side` 0 buy / 1 sell; `stop_type` 0 take profit / 1 stop loss;
/// `order_type` 0 market / 1 limit; `stop_price_option` 0 last traded /
/// 1 mark; `tif` 0 GTC / 1 GTT / 2 FOK / 3 IOC. `limit_price` is `"0"` for
/// market orders.
#[derive(Debug, Clone, Serialize)]
pub struct PlaceTpsl {
    pub account: Address,
    #[serde(serialize_with = "ser_str")]
    pub market_id: u64,
    pub side: u8,
    pub size: String,
    pub stop_type: u8,
    pub order_type: u8,
    pub stop_price: String,
    pub limit_price: String,
    pub stop_price_option: u8,
    pub tif: u8,
    pub deadline: u32,
    pub size_percent_bps: u32,
}

/// Body of the TP/SL cancel request, without `signer` and `signature`.
#[derive(Debug, Clone, Serialize)]
pub struct CancelTpsl {
    pub order_id: String,
    pub account: Address,
    pub deadline: u32,
}

/// Body of the TP/SL cancel-all request, without `signer` and `signature`.
#[derive(Debug, Clone, Serialize)]
pub struct CancelAllTpsl {
    pub account: Address,
    #[serde(serialize_with = "ser_str")]
    pub market_id: u64,
    pub deadline: u32,
}

/// A complete TP/SL request body.
#[derive(Debug, Clone, Serialize)]
pub struct Signed<T> {
    #[serde(flatten)]
    pub request: T,
    pub signer: Address,
    /// Base64 of the 65-byte signature.
    pub signature: String,
}

impl RisexSigner {
    pub fn place_tpsl(&self, request: PlaceTpsl) -> Result<Signed<PlaceTpsl>, SigningError> {
        let signature = self.sign_base64(&PlaceTpslOrder {
            account: request.account,
            marketId: request.market_id,
            side: request.side,
            size: request.size.clone(),
            stopType: request.stop_type,
            stopPrice: request.stop_price.clone(),
            limitPrice: request.limit_price.clone(),
            orderType: request.order_type,
            stopPriceOption: request.stop_price_option,
            tif: request.tif,
            deadline: request.deadline,
            sizePercentBps: request.size_percent_bps,
        })?;
        Ok(self.signed(request, signature))
    }

    pub fn cancel_tpsl(&self, request: CancelTpsl) -> Result<Signed<CancelTpsl>, SigningError> {
        let signature = self.sign_base64(&CancelTpslOrder {
            account: request.account,
            orderId: request.order_id.clone(),
            deadline: request.deadline,
        })?;
        Ok(self.signed(request, signature))
    }

    pub fn cancel_all_tpsl(
        &self,
        request: CancelAllTpsl,
    ) -> Result<Signed<CancelAllTpsl>, SigningError> {
        let signature = self.sign_base64(&CancelAllTpslOrders {
            account: request.account,
            marketId: request.market_id,
            deadline: request.deadline,
        })?;
        Ok(self.signed(request, signature))
    }

    fn signed<T>(&self, request: T, signature: String) -> Signed<T> {
        Signed {
            request,
            signer: self.address(),
            signature,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signing::testing::{A0, A1, DEADLINE, assert_recovers_base64, signer};

    #[test]
    fn place_recovers_and_serializes() {
        let request = PlaceTpsl {
            account: A1,
            market_id: 1,
            side: 1,
            size: "1.5".into(),
            stop_type: 1,
            order_type: 0,
            stop_price: "70000".into(),
            limit_price: "0".into(),
            stop_price_option: 1,
            tif: 3,
            deadline: DEADLINE,
            size_percent_bps: 0,
        };
        let signed = signer().place_tpsl(request).unwrap();
        assert_recovers_base64(&signed.signature, "place_tpsl");

        let json = serde_json::to_value(&signed).unwrap();
        assert_eq!(json["market_id"], "1");
        assert_eq!(json["size"], "1.5");
        assert_eq!(json["stop_price"], "70000");
        assert_eq!(json["signer"], A0.to_string().to_lowercase());
        assert_eq!(json.as_object().unwrap().len(), 14);
    }

    #[test]
    fn cancels_recover() {
        let s = signer();
        let order_id = "550e8400-e29b-41d4-a716-446655440000".to_owned();
        let one = s
            .cancel_tpsl(CancelTpsl {
                order_id,
                account: A1,
                deadline: DEADLINE,
            })
            .unwrap();
        assert_recovers_base64(&one.signature, "cancel_tpsl");
        let all = s
            .cancel_all_tpsl(CancelAllTpsl {
                account: A1,
                market_id: 1,
                deadline: DEADLINE,
            })
            .unwrap();
        assert_recovers_base64(&all.signature, "cancel_all_tpsl");
    }
}
