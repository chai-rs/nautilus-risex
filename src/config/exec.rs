//! Execution client configuration.

use std::{collections::HashMap, fmt};

use nautilus_model::identifiers::{AccountId, InstrumentId};

use crate::common::{
    consts::HTTP_TIMEOUT_SECS,
    enums::{RisexEnvironment, RisexMarginMode},
};

/// Configuration of the RISEx execution client.
#[derive(Clone)]
#[cfg_attr(
    feature = "python",
    pyo3::pyclass(module = "nautilus_risex", from_py_object)
)]
pub struct RisexExecClientConfig {
    pub environment: RisexEnvironment,
    /// Overrides the REST base URL of the environment.
    pub base_url_http: Option<String>,
    /// Overrides the WebSocket URL of the environment.
    pub base_url_ws: Option<String>,
    pub proxy_url: Option<String>,
    pub http_timeout_secs: u64,
    /// REST requests per 10 seconds; `None` uses the venue limit.
    pub rest_quota_per_10s: Option<u32>,
    pub account_id: AccountId,
    /// Account (wallet) address; falls back to the environment variable.
    pub account_address: Option<String>,
    /// Session signer key; falls back to the environment variable. Never the account key.
    pub signer_private_key: Option<String>,
    /// Price cap of a market order, in basis points past the opposite top of book.
    pub market_order_slippage_bps: u32,
    /// Permit lifetime: `deadline` = now + this many seconds.
    pub deadline_secs: u32,
    /// Poll interval of `GET /v1/orders/tpsl` while TP/SL orders are open.
    pub tpsl_poll_interval_secs: u64,
    /// Builder code attached to every order; 0 is none.
    pub builder_id: u16,
    /// Leverage set at connect, per instrument.
    pub leverage: HashMap<InstrumentId, u8>,
    /// Margin mode set at connect, per instrument.
    pub margin_mode: HashMap<InstrumentId, RisexMarginMode>,
}

impl Default for RisexExecClientConfig {
    fn default() -> Self {
        Self {
            environment: RisexEnvironment::default(),
            base_url_http: None,
            base_url_ws: None,
            proxy_url: None,
            http_timeout_secs: HTTP_TIMEOUT_SECS,
            rest_quota_per_10s: None,
            account_id: AccountId::from("RISEX-001"),
            account_address: None,
            signer_private_key: None,
            market_order_slippage_bps: 50,
            deadline_secs: 60,
            tpsl_poll_interval_secs: 5,
            builder_id: 0,
            leverage: HashMap::new(),
            margin_mode: HashMap::new(),
        }
    }
}

// Manual impl so the signer key can never reach logs.
impl fmt::Debug for RisexExecClientConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RisexExecClientConfig")
            .field("environment", &self.environment)
            .field("base_url_http", &self.base_url_http)
            .field("base_url_ws", &self.base_url_ws)
            .field("account_id", &self.account_id)
            .field("account_address", &self.account_address)
            .field(
                "signer_private_key",
                &self.signer_private_key.as_ref().map(|_| "<redacted>"),
            )
            .field("market_order_slippage_bps", &self.market_order_slippage_bps)
            .field("deadline_secs", &self.deadline_secs)
            .field("builder_id", &self.builder_id)
            .field("leverage", &self.leverage)
            .field("margin_mode", &self.margin_mode)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_hides_the_signer_key() {
        let config = RisexExecClientConfig {
            signer_private_key: Some("0xdeadbeef".to_string()),
            ..Default::default()
        };
        let debug = format!("{config:?}");
        assert!(!debug.contains("deadbeef"));
        assert!(debug.contains("<redacted>"));
    }
}
