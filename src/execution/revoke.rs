//! Revocation of the session signer, shared by the `risex-signer-revoke`
//! binary and the Python binding.

use std::{env, fmt};

use alloy::signers::local::PrivateKeySigner;
use alloy_primitives::Address;
use anyhow::Context;
use zeroize::Zeroizing;

use crate::{
    common::{consts::HTTP_TIMEOUT_SECS, credential::RisexCredential, enums::RisexEnvironment},
    http::{client::RisexRawHttpClient, models::RisexRevokeSignerRequest},
    signing::{RisexSigner, nonce::NonceManager},
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
        let account = self.account.to_string();
        let domain = self.raw.get_eip712_domain().await?;
        let verifying_contract: Address = domain
            .verifying_contract
            .parse()
            .context("invalid verifying contract")?;
        let nonce_state = self.raw.get_nonce_state(&account).await?;
        let nonces = NonceManager::new(nonce_state.nonce_anchor);
        nonces.resync(nonce_state.nonce_anchor, nonce_state.current_bitmap_index);
        let lease = nonces.next().await?;

        // unverified: the body shape and the signature were never sent to the venue.
        let account_signature = RisexSigner::new(
            self.account_key,
            &domain.name,
            &domain.version,
            domain.chain_id,
            verifying_contract,
        )
        .revoke_signer(self.signer, lease.nonce)?;
        let response = self
            .raw
            .revoke_signer(&RisexRevokeSignerRequest {
                account,
                signer: self.signer.to_string(),
                nonce_anchor: lease.nonce.anchor.to_string(),
                nonce_bitmap_index: lease.nonce.index,
                account_signature,
            })
            .await?;
        anyhow::ensure!(response.success, "the venue did not confirm the revocation");
        Ok(())
    }
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
