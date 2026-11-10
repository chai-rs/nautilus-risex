//! EIP-712 signing for RISEx: action hashes, permits, TP/SL typed data, signer
//! management, WebSocket auth and the permit nonce manager.
//!
//! Nothing here does I/O or reads keys; the caller supplies the key, the
//! EIP-712 domain (`GET /v1/auth/eip712-domain`) and the router address.

pub mod action;
pub mod nonce;
pub mod permit;
pub mod signer;
pub mod tpsl;

use std::{borrow::Cow, fmt};

use alloy::{
    signers::{SignerSync, local::PrivateKeySigner},
    sol_types::{Eip712Domain, SolStruct},
};
use alloy_primitives::{Address, U256};
use base64::Engine;
use serde::Serializer;

/// Errors from building or signing a payload.
#[derive(Debug, thiserror::Error)]
pub enum SigningError {
    /// A field does not fit its on-chain width or enum range.
    #[error("{0} out of range")]
    OutOfRange(&'static str),
    /// The WebSocket auth nonce is not 32 bytes of hex.
    #[error("invalid WebSocket auth nonce")]
    InvalidNonce,
    #[error("signer error: {0}")]
    Signer(#[from] alloy::signers::Error),
}

/// A private key bound to the RISEx EIP-712 domain.
///
/// Usually the session (signer) key; signer registration and revocation need
/// one built from the account key instead.
pub struct RisexSigner {
    key: PrivateKeySigner,
    domain: Eip712Domain,
}

impl RisexSigner {
    /// `verifying_contract` is the auth contract (`addresses.auth`).
    pub fn new(
        key: PrivateKeySigner,
        name: &str,
        version: &str,
        chain_id: u64,
        verifying_contract: Address,
    ) -> Self {
        let domain = Eip712Domain::new(
            Some(Cow::Owned(name.to_owned())),
            Some(Cow::Owned(version.to_owned())),
            Some(U256::from(chain_id)),
            Some(verifying_contract),
            None,
        );
        Self { key, domain }
    }

    /// Address of the wrapped key.
    pub fn address(&self) -> Address {
        self.key.address()
    }

    /// 65-byte `r || s || v` signature (`v` = 27/28) over the typed data.
    fn sign(&self, data: &impl SolStruct) -> Result<[u8; 65], SigningError> {
        let hash = data.eip712_signing_hash(&self.domain);
        Ok(self.key.sign_hash_sync(&hash)?.as_bytes())
    }

    /// Base64 signature, the encoding of protobuf `bytes` fields (permit, TP/SL).
    fn sign_base64(&self, data: &impl SolStruct) -> Result<String, SigningError> {
        Ok(base64::engine::general_purpose::STANDARD.encode(self.sign(data)?))
    }

    /// `0x` hex signature (signer management, WebSocket auth).
    fn sign_hex(&self, data: &impl SolStruct) -> Result<String, SigningError> {
        Ok(alloy_primitives::hex::encode_prefixed(self.sign(data)?))
    }
}

// Manual impl so the key can never reach logs, whatever the inner type prints.
impl fmt::Debug for RisexSigner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RisexSigner")
            .field("address", &self.key.address())
            .finish_non_exhaustive()
    }
}

/// protojson encodes 64-bit integers as strings.
fn ser_str<S: Serializer>(value: &u64, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.collect_str(value)
}

#[cfg(test)]
pub(crate) mod testing {
    use alloy_primitives::{Address, B256, Signature, address};
    use base64::Engine;
    use serde_json::Value;

    use super::RisexSigner;

    /// First Anvil/Hardhat key: public test key, never funded on a real network.
    const KEY: &str = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
    pub const A0: Address = address!("0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266");
    pub const A1: Address = address!("0x70997970C51812dc3A010C7d01b50e0d17dc79C8");
    pub const ANCHOR: u64 = 2_527_069;
    pub const DEADLINE: u32 = 1_786_097_000;

    /// Reference values from `tests/fixtures/signing/gen_vectors.py`.
    pub fn vectors() -> Value {
        serde_json::from_str(include_str!("../../tests/fixtures/signing/vectors.json")).unwrap()
    }

    pub fn b256(value: &Value) -> B256 {
        value.as_str().unwrap().parse().unwrap()
    }

    pub fn signer() -> RisexSigner {
        let v = vectors();
        let d = &v["domain"];
        RisexSigner::new(
            KEY.parse().unwrap(),
            d["name"].as_str().unwrap(),
            d["version"].as_str().unwrap(),
            d["chain_id"].as_u64().unwrap(),
            d["verifying_contract"].as_str().unwrap().parse().unwrap(),
        )
    }

    /// The signature must recover to the test key over the Python-computed digest.
    pub fn assert_recovers(signature: &[u8], digest: &str) {
        assert_eq!(signature.len(), 65);
        assert!(matches!(signature[64], 27 | 28));
        let digest = b256(&vectors()["digests"][digest]);
        let recovered = Signature::from_raw(signature)
            .unwrap()
            .recover_address_from_prehash(&digest)
            .unwrap();
        assert_eq!(recovered, A0);
    }

    pub fn assert_recovers_base64(signature: &str, digest: &str) {
        let raw = base64::engine::general_purpose::STANDARD
            .decode(signature)
            .unwrap();
        assert_recovers(&raw, digest);
    }

    pub fn assert_recovers_hex(signature: &str, digest: &str) {
        assert!(signature.starts_with("0x"));
        assert_recovers(&alloy_primitives::hex::decode(signature).unwrap(), digest);
    }

    #[test]
    fn debug_never_shows_the_key() {
        let debug = format!("{:?}", signer()).to_lowercase();
        assert!(debug.contains(&A0.to_string().to_lowercase()));
        assert!(!debug.contains(&KEY[2..]));
    }
}
