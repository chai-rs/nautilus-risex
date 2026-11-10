//! The `VerifyWitness` permit attached to every signed action.

use alloy::sol;
use alloy_primitives::{Address, B256, aliases::U48};
use serde::Serialize;

use super::{RisexSigner, SigningError, nonce::Nonce, ser_str};

sol! {
    #[derive(Debug)]
    struct VerifyWitness {
        address account;
        address target;
        bytes32 hash;
        uint48 nonceAnchor;
        uint8 nonceBitmap;
        uint32 deadline;
    }
}

/// The `permit` (`permit_params` on the leverage and margin endpoints) object
/// of a request body.
#[derive(Debug, Clone, Serialize)]
pub struct Permit {
    pub account: Address,
    pub signer: Address,
    #[serde(serialize_with = "ser_str")]
    pub nonce_anchor: u64,
    pub nonce_bitmap_index: u8,
    pub deadline: u32,
    /// Base64 of the 65-byte signature.
    pub signature: String,
}

impl RisexSigner {
    /// Signs `action_hash` for `account`. `target` is the router
    /// (`addresses.router`); `deadline` is unix seconds.
    pub fn permit(
        &self,
        account: Address,
        target: Address,
        action_hash: B256,
        nonce: Nonce,
        deadline: u32,
    ) -> Result<Permit, SigningError> {
        let witness = VerifyWitness {
            account,
            target,
            hash: action_hash,
            nonceAnchor: anchor(nonce)?,
            nonceBitmap: nonce.index,
            deadline,
        };
        Ok(Permit {
            account,
            signer: self.address(),
            nonce_anchor: nonce.anchor,
            nonce_bitmap_index: nonce.index,
            deadline,
            signature: self.sign_base64(&witness)?,
        })
    }
}

/// The anchor as the contract's `uint48`; an error rather than a silent truncation.
pub(super) fn anchor(nonce: Nonce) -> Result<U48, SigningError> {
    U48::try_from(nonce.anchor).map_err(|_| SigningError::OutOfRange("nonce_anchor"))
}

#[cfg(test)]
mod tests {
    use alloy::sol_types::SolStruct;
    use alloy_primitives::{address, b256, keccak256};

    use super::*;
    use crate::signing::testing::{
        A0, A1, ANCHOR, DEADLINE, assert_recovers_base64, b256 as hash, signer, vectors,
    };

    #[test]
    fn type_hash_matches_contract() {
        assert_eq!(
            keccak256(VerifyWitness::eip712_encode_type().as_bytes()),
            b256!("0x055e6bcbf2ba5ff1c2ba5dc95b6648a5de6aaab3185251a34e3b88c11e116821")
        );
    }

    #[test]
    fn permit_recovers_and_serializes() {
        let router = address!("0xaadde0cea454f2bcb26f46ed54c5709b7bb34a7e");
        let action = hash(&vectors()["place"][0]["hash"]);
        let nonce = Nonce {
            anchor: ANCHOR,
            index: 7,
        };
        let permit = signer()
            .permit(A1, router, action, nonce, DEADLINE)
            .unwrap();
        assert_recovers_base64(&permit.signature, "verify_witness");

        let json = serde_json::to_value(&permit).unwrap();
        assert_eq!(json["account"], A1.to_string().to_lowercase());
        assert_eq!(json["signer"], A0.to_string().to_lowercase());
        assert_eq!(json["nonce_anchor"], ANCHOR.to_string());
        assert_eq!(json["nonce_bitmap_index"], 7);
        assert_eq!(json["deadline"], DEADLINE);
    }

    /// Real router calldata (mainnet block 26670519): our hashes must recover
    /// the session key that signed them. This pins the whole scheme with no key.
    #[test]
    fn mainnet_payloads_recover_their_signer() {
        use alloy_primitives::{Signature, hex};

        use crate::signing::action::{PlaceOrder, cancel_order_hash};

        let v: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/signing/mainnet_payloads.json"
        ))
        .unwrap();
        let addr = |key: &str| v[key].as_str().unwrap().parse::<Address>().unwrap();
        let domain = RisexSigner::new(
            alloy::signers::local::PrivateKeySigner::random(),
            "RISEx",
            "1",
            v["chain_id"].as_u64().unwrap(),
            addr("auth"),
        )
        .domain;
        // Both payloads end with: account(20) | deadline(4) | anchor(6) | bitmap(1) | sig(64).
        let recover = |payload: &[u8], action: B256| {
            let tail = &payload[payload.len() - 95..];
            let witness = VerifyWitness {
                account: Address::from_slice(&tail[..20]),
                target: addr("router"),
                hash: action,
                nonceAnchor: U48::from_be_slice(&tail[24..30]),
                nonceBitmap: tail[30],
                deadline: u32::from_be_bytes(tail[20..24].try_into().unwrap()),
            };
            Signature::from_erc2098(&tail[31..])
                .recover_address_from_prehash(&witness.eip712_signing_hash(&domain))
                .unwrap()
        };

        let place = hex::decode(v["place"]["payload"].as_str().unwrap()).unwrap();
        let order = PlaceOrder {
            market_id: 15,
            size_steps: 11_658,
            price_ticks: 55_381,
            side: 0,
            post_only: true,
            reduce_only: false,
            stp_mode: 0,
            order_type: 1,
            time_in_force: 0,
            builder_id: 0,
            client_order_id: v["place"]["client_order_id"].as_u64().unwrap(),
            ttl_units: 0,
        };
        assert_eq!(order.order_data().unwrap().to_be_bytes()[5..], place[1..12]);
        // The relayer sets bit 0x20 on chain; it is not part of the signed flags.
        assert_eq!(order.header_flags(), place[0] & 0x1f);
        assert_eq!(
            recover(&place, order.action_hash().unwrap()),
            addr("expected_signer")
        );

        let cancel = hex::decode(v["cancel"]["payload"].as_str().unwrap()).unwrap();
        let action = cancel_order_hash(39, 25_769_805_994);
        assert_eq!(recover(&cancel, action), addr("expected_signer"));
    }

    #[test]
    fn anchor_wider_than_uint48_is_rejected() {
        let nonce = Nonce {
            anchor: 1 << 48,
            index: 0,
        };
        let result = signer().permit(A1, A1, B256::ZERO, nonce, DEADLINE);
        assert!(matches!(
            result,
            Err(SigningError::OutOfRange("nonce_anchor"))
        ));
    }
}
