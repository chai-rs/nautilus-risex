//! Signer (session key) registration, verification and revocation, and
//! WebSocket `auth_v2`. All signatures are `0x` hex.

use alloy::sol;
use alloy_primitives::{Address, B256, U256};

use super::{RisexSigner, SigningError, nonce::Nonce, permit::anchor};

sol! {
    #[derive(Debug)]
    struct RegisterSigner {
        address account;
        address signer;
        string message;
        uint32 expiration;
        uint48 nonceAnchor;
        uint8 nonceBitmap;
    }

    #[derive(Debug)]
    struct VerifySigner {
        address account;
        uint48 nonceAnchor;
        uint8 nonceBitmap;
    }

    #[derive(Debug)]
    struct RevokeSigner {
        address account;
        address signer;
        uint48 nonceAnchor;
        uint8 nonceBitmap;
    }

    #[derive(Debug)]
    struct RegisterV2 {
        address signer;
        string message;
        uint256 nonce;
    }
}

impl RisexSigner {
    /// `account_signature` of a register request. `self` holds the ACCOUNT key.
    pub fn register_signer(
        &self,
        signer: Address,
        message: &str,
        expiration: u32,
        nonce: Nonce,
    ) -> Result<String, SigningError> {
        self.sign_hex(&RegisterSigner {
            account: self.address(),
            signer,
            message: message.to_owned(),
            expiration,
            nonceAnchor: anchor(nonce)?,
            nonceBitmap: nonce.index,
        })
    }

    /// `signer_signature` of a register request, with the same nonce as the
    /// account signature. `self` holds the signer key.
    pub fn verify_signer(&self, account: Address, nonce: Nonce) -> Result<String, SigningError> {
        self.sign_hex(&VerifySigner {
            account,
            nonceAnchor: anchor(nonce)?,
            nonceBitmap: nonce.index,
        })
    }

    /// `account_signature` of a revoke request. `self` holds the ACCOUNT key.
    pub fn revoke_signer(&self, signer: Address, nonce: Nonce) -> Result<String, SigningError> {
        self.sign_hex(&RevokeSigner {
            account: self.address(),
            signer,
            nonceAnchor: anchor(nonce)?,
            nonceBitmap: nonce.index,
        })
    }

    /// `signature` of a WebSocket `auth_v2` frame. `nonce` is the 32-byte hex
    /// string from `GET /v1/auth/nonce`, with or without `0x`; send the same
    /// `message` and `nonce` in the frame.
    pub fn ws_auth(&self, message: &str, nonce: &str) -> Result<String, SigningError> {
        let nonce: B256 = nonce.parse().map_err(|_| SigningError::InvalidNonce)?;
        self.sign_hex(&RegisterV2 {
            signer: self.address(),
            message: message.to_owned(),
            nonce: U256::from_be_bytes(nonce.0),
        })
    }
}

#[cfg(test)]
mod tests {
    use alloy::sol_types::SolStruct;
    use alloy_primitives::{b256, keccak256};

    use super::*;
    use crate::signing::testing::{A1, ANCHOR, assert_recovers_hex, signer};

    const WS_NONCE: &str = "deadbeefcafebabe0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn type_hashes_match_contract() {
        assert_eq!(
            keccak256(RegisterSigner::eip712_encode_type().as_bytes()),
            b256!("0xa526f63b3968e56ae1b177ce9b3dc29766e0891e6397a9c23cf8c53ee8fc8f62")
        );
        assert_eq!(
            keccak256(VerifySigner::eip712_encode_type().as_bytes()),
            b256!("0x4d298dcceb691695f582cc337308236426a0c97201a31834625e8eadc44d4230")
        );
        assert_eq!(
            keccak256(RevokeSigner::eip712_encode_type().as_bytes()),
            b256!("0x36db7f392f548b56f37d89469115d138685addf06be45684f9e5b0e8b5d28000")
        );
    }

    #[test]
    fn signer_management_signatures_recover() {
        let s = signer();
        let nonce = Nonce {
            anchor: ANCHOR,
            index: 3,
        };
        let sig = s
            .register_signer(A1, "RISEx signer", 1_800_000_000, nonce)
            .unwrap();
        assert_recovers_hex(&sig, "register_signer");
        assert_recovers_hex(&s.verify_signer(A1, nonce).unwrap(), "verify_signer");
        let nonce = Nonce {
            anchor: ANCHOR,
            index: 4,
        };
        assert_recovers_hex(&s.revoke_signer(A1, nonce).unwrap(), "revoke_signer");
    }

    #[test]
    fn ws_auth_recovers_with_or_without_prefix() {
        let s = signer();
        let bare = s.ws_auth("sign in with RISEx", WS_NONCE).unwrap();
        assert_recovers_hex(&bare, "register_v2");
        let prefixed = s
            .ws_auth("sign in with RISEx", &format!("0x{WS_NONCE}"))
            .unwrap();
        assert_eq!(bare, prefixed);
    }

    #[test]
    fn ws_auth_rejects_bad_nonce() {
        for nonce in ["", "0x", "zz", &WS_NONCE[2..]] {
            let result = signer().ws_auth("m", nonce);
            assert!(
                matches!(result, Err(SigningError::InvalidNonce)),
                "{nonce:?}"
            );
        }
    }
}
