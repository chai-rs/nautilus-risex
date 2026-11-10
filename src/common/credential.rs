//! RISEx credential storage.

use std::fmt::Debug;

use nautilus_core::{env::get_or_env_var_opt, string::secret::REDACTED};
use zeroize::Zeroizing;

use crate::common::enums::RisexEnvironment;

/// Returns the `(account address, signer private key)` environment variable names.
#[must_use]
pub const fn credential_env_vars(environment: RisexEnvironment) -> (&'static str, &'static str) {
    match environment {
        RisexEnvironment::Mainnet => ("RISEX_ACCOUNT_ADDRESS", "RISEX_SIGNER_PRIVATE_KEY"),
        RisexEnvironment::Testnet => (
            "RISEX_TESTNET_ACCOUNT_ADDRESS",
            "RISEX_TESTNET_SIGNER_PRIVATE_KEY",
        ),
    }
}

/// Account address plus the session-key (signer) private key.
///
/// The key is wiped from memory on drop and never shown by `Debug`.
#[derive(Clone)]
pub struct RisexCredential {
    account_address: String,
    signer_private_key: Zeroizing<String>,
}

impl Debug for RisexCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct(stringify!(RisexCredential))
            .field("account_address", &self.account_address)
            .field("signer_private_key", &REDACTED)
            .finish()
    }
}

impl RisexCredential {
    /// Creates a new [`RisexCredential`].
    #[must_use]
    pub fn new(account_address: impl Into<String>, signer_private_key: impl Into<String>) -> Self {
        Self {
            account_address: account_address.into(),
            signer_private_key: Zeroizing::new(signer_private_key.into()),
        }
    }

    /// Resolves credentials from explicit values, falling back to the environment
    /// variables of `environment`. Returns `None` when neither value is available.
    ///
    /// # Errors
    ///
    /// Returns an error if only one of the two values is available.
    pub fn resolve(
        account_address: Option<String>,
        signer_private_key: Option<String>,
        environment: RisexEnvironment,
    ) -> anyhow::Result<Option<Self>> {
        let (address_var, key_var) = credential_env_vars(environment);
        Self::from_resolved_values(
            get_or_env_var_opt(account_address, address_var),
            get_or_env_var_opt(signer_private_key, key_var),
            environment,
        )
    }

    fn from_resolved_values(
        account_address: Option<String>,
        signer_private_key: Option<String>,
        environment: RisexEnvironment,
    ) -> anyhow::Result<Option<Self>> {
        let (address_var, key_var) = credential_env_vars(environment);
        match (account_address, signer_private_key) {
            (Some(address), Some(key)) => Ok(Some(Self::new(address, key))),
            (None, None) => Ok(None),
            (Some(_), None) => anyhow::bail!("incomplete RISEx credentials: {key_var} is not set"),
            (None, Some(_)) => {
                anyhow::bail!("incomplete RISEx credentials: {address_var} is not set")
            }
        }
    }

    /// Returns the account address.
    #[must_use]
    pub fn account_address(&self) -> &str {
        &self.account_address
    }

    /// Returns the signer private key. Do not log it.
    #[must_use]
    pub fn signer_private_key(&self) -> &str {
        &self.signer_private_key
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    // Placeholder strings only: these tests never read the environment or a real key.
    const ADDRESS: &str = "0x00000000000000000000000000000000000000aa";
    const FAKE_KEY: &str = "not-a-real-key";

    #[rstest]
    fn test_credential_env_vars() {
        assert_eq!(
            credential_env_vars(RisexEnvironment::Mainnet),
            ("RISEX_ACCOUNT_ADDRESS", "RISEX_SIGNER_PRIVATE_KEY")
        );
        assert_eq!(
            credential_env_vars(RisexEnvironment::Testnet),
            (
                "RISEX_TESTNET_ACCOUNT_ADDRESS",
                "RISEX_TESTNET_SIGNER_PRIVATE_KEY"
            )
        );
    }

    #[rstest]
    fn test_from_resolved_values() {
        let env = RisexEnvironment::Mainnet;
        let credential = RisexCredential::from_resolved_values(
            Some(ADDRESS.to_string()),
            Some(FAKE_KEY.to_string()),
            env,
        )
        .unwrap()
        .unwrap();
        assert_eq!(credential.account_address(), ADDRESS);
        assert_eq!(credential.signer_private_key(), FAKE_KEY);

        assert!(
            RisexCredential::from_resolved_values(None, None, env)
                .unwrap()
                .is_none()
        );
    }

    #[rstest]
    fn test_from_resolved_values_rejects_partial_values() {
        let env = RisexEnvironment::Testnet;
        let missing_key =
            RisexCredential::from_resolved_values(Some(ADDRESS.to_string()), None, env)
                .unwrap_err()
                .to_string();
        assert!(missing_key.contains("RISEX_TESTNET_SIGNER_PRIVATE_KEY"));

        let missing_address =
            RisexCredential::from_resolved_values(None, Some(FAKE_KEY.to_string()), env)
                .unwrap_err()
                .to_string();
        assert!(missing_address.contains("RISEX_TESTNET_ACCOUNT_ADDRESS"));
        assert!(!missing_address.contains(FAKE_KEY));
    }

    #[rstest]
    fn test_debug_redacts_signer_private_key() {
        let debug = format!("{:?}", RisexCredential::new(ADDRESS, FAKE_KEY));
        assert!(debug.contains(ADDRESS));
        assert!(debug.contains(REDACTED));
        assert!(!debug.contains(FAKE_KEY));
    }
}
