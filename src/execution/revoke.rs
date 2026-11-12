//! Revocation of the session signer, shared by the `risex-signer-revoke`
//! binary and the Python binding.

use std::{env, fmt};

use alloy::signers::local::PrivateKeySigner;
use alloy_primitives::Address;
use anyhow::Context;
use zeroize::Zeroizing;

use crate::{
    common::{consts::HTTP_TIMEOUT_SECS, credential::RisexCredential, enums::RisexEnvironment},
    http::{
        client::RisexRawHttpClient,
        models::{RisexEip712Domain, RisexRevokeSignerRequest},
    },
    signing::{
        RisexSigner,
        nonce::{Nonce, NonceManager},
    },
};

/// A signer revocation that is resolved from the environment and not sent yet.
pub struct SignerRevocation {
    account: Address,
    signer: Address,
    account_key: PrivateKeySigner,
    raw: RisexRawHttpClient,
}

impl SignerRevocation {
    /// Reads the account, the signer to revoke and the account (wallet) key from
    /// the environment: `RISEX_ACCOUNT_ADDRESS`, `RISEX_SIGNER_PRIVATE_KEY` and
    /// `RISEX_ACCOUNT_PRIVATE_KEY` (testnet: the `RISEX_TESTNET_*` variants).
    ///
    /// # Errors
    ///
    /// Returns an error if a variable is missing or invalid, or if the account
    /// key is not the key of the account.
    pub fn from_env(environment: RisexEnvironment) -> anyhow::Result<Self> {
        let key_var = match environment {
            RisexEnvironment::Mainnet => "RISEX_ACCOUNT_PRIVATE_KEY",
            RisexEnvironment::Testnet => "RISEX_TESTNET_ACCOUNT_PRIVATE_KEY",
        };
        let credential = RisexCredential::resolve(None, None, environment)?
            .context("no RISEx credentials in env")?;
        let account: Address = credential
            .account_address()
            .parse()
            .context("invalid account address")?;

        // Both errors are dropped on purpose: they can carry key material.
        let signer: PrivateKeySigner = credential
            .signer_private_key()
            .parse()
            .map_err(|_| anyhow::anyhow!("invalid signer private key"))?;
        let account_key: PrivateKeySigner =
            Zeroizing::new(env::var(key_var).map_err(|_| anyhow::anyhow!("{key_var} is not set"))?)
                .parse()
                .map_err(|_| anyhow::anyhow!("invalid account private key in {key_var}"))?;
        anyhow::ensure!(
            account_key.address() == account,
            "{key_var} is not the key of account {account}"
        );

        Ok(Self {
            account,
            signer: signer.address(),
            account_key,
            raw: RisexRawHttpClient::new(environment, None, HTTP_TIMEOUT_SECS, None)?,
        })
    }

    pub fn account(&self) -> Address {
        self.account
    }

    /// Address of the signer to revoke.
    pub fn signer(&self) -> Address {
        self.signer
    }

    /// Status of the signer at the venue (`GET /v1/auth/session-key-status`).
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails.
    pub async fn status(&self) -> anyhow::Result<String> {
        let status = self
            .raw
            .get_session_key_status(&self.account.to_string(), &self.signer.to_string())
            .await?;
        Ok(status.status.to_string())
    }

    /// Signs the revocation with the account key and sends it.
    ///
    /// # Errors
    ///
    /// Returns an error if a request fails or the venue does not confirm.
    pub async fn submit(self) -> anyhow::Result<()> {
        let domain = self.raw.get_eip712_domain().await?;
        let nonce_state = self.raw.get_nonce_state(&self.account.to_string()).await?;
        let nonces = NonceManager::new(nonce_state.nonce_anchor);
        nonces.resync(nonce_state.nonce_anchor, nonce_state.current_bitmap_index);
        let lease = nonces.next().await?;

        let request = revoke_request(self.account_key, self.signer, &domain, lease.nonce)?;
        let response = self.raw.revoke_signer(&request).await?;
        anyhow::ensure!(response.success, "the venue did not confirm the revocation");
        Ok(())
    }
}

/// Body of `POST /v1/auth/revoke-signer` for `signer`, signed with the account key.
fn revoke_request(
    account_key: PrivateKeySigner,
    signer: Address,
    domain: &RisexEip712Domain,
    nonce: Nonce,
) -> anyhow::Result<RisexRevokeSignerRequest> {
    let verifying_contract: Address = domain
        .verifying_contract
        .parse()
        .context("invalid verifying contract")?;
    let account = account_key.address().to_string();

    // unverified: the body shape and the signature were never sent to the venue.
    let account_signature = RisexSigner::new(
        account_key,
        &domain.name,
        &domain.version,
        domain.chain_id,
        verifying_contract,
    )
    .revoke_signer(signer, nonce)?;
    Ok(RisexRevokeSignerRequest {
        account,
        signer: signer.to_string(),
        nonce_anchor: nonce.anchor.to_string(),
        nonce_bitmap_index: nonce.index,
        account_signature,
    })
}

// Manual impl so the account key can never reach logs.
impl fmt::Debug for SignerRevocation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SignerRevocation")
            .field("account", &self.account)
            .field("signer", &self.signer)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        http::models::tests::fixture,
        signing::testing::{A0, A1, ANCHOR, KEY, assert_recovers_hex},
    };

    #[test]
    fn request_body_is_signed_by_the_account_key() {
        // The recorded domain is the domain of the Python reference digest.
        let domain: RisexEip712Domain = fixture("eip712_domain");
        let nonce = Nonce {
            anchor: ANCHOR,
            index: 4,
        };
        let request = revoke_request(KEY.parse().unwrap(), A1, &domain, nonce).unwrap();

        assert_eq!(request.account, A0.to_string());
        assert_eq!(request.signer, A1.to_string());
        assert_recovers_hex(&request.account_signature, "revoke_signer");
        let body = serde_json::to_value(&request).unwrap();
        assert_eq!(
            body,
            serde_json::json!({
                "account": "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266",
                "signer": "0x70997970C51812dc3A010C7d01b50e0d17dc79C8",
                "nonce_anchor": "2527069",
                "nonce_bitmap_index": 4,
                "account_signature": request.account_signature,
            })
        );
    }

    #[test]
    fn invalid_verifying_contract_is_an_error() {
        let mut domain: RisexEip712Domain = fixture("eip712_domain");
        domain.verifying_contract = "0x12".to_string();
        let nonce = Nonce {
            anchor: ANCHOR,
            index: 4,
        };
        let e = revoke_request(KEY.parse().unwrap(), A1, &domain, nonce).unwrap_err();
        assert_eq!(e.to_string(), "invalid verifying contract");
    }
}
