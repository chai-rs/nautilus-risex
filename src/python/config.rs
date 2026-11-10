//! Python bindings of the client configurations.

use std::{collections::HashMap, str::FromStr};

use nautilus_core::python::to_pyvalue_err;
use nautilus_model::identifiers::{AccountId, InstrumentId};
use pyo3::{PyResult, pymethods};

use crate::{
    common::enums::{RisexEnvironment, RisexMarginMode},
    config::{data::RisexDataClientConfig, exec::RisexExecClientConfig},
};

#[pymethods]
impl RisexDataClientConfig {
    #[new]
    #[pyo3(signature = (
        environment = None,
        base_url_http = None,
        base_url_ws = None,
        http_timeout_secs = None,
    ))]
    fn py_new(
        environment: Option<RisexEnvironment>,
        base_url_http: Option<String>,
        base_url_ws: Option<String>,
        http_timeout_secs: Option<u64>,
    ) -> Self {
        let defaults = Self::default();
        Self {
            environment: environment.unwrap_or(defaults.environment),
            base_url_http,
            base_url_ws,
            http_timeout_secs: http_timeout_secs.unwrap_or(defaults.http_timeout_secs),
        }
    }

    #[getter]
    fn environment(&self) -> RisexEnvironment {
        self.environment
    }

    #[getter]
    fn base_url_http(&self) -> Option<String> {
        self.base_url_http.clone()
    }

    #[getter]
    fn base_url_ws(&self) -> Option<String> {
        self.base_url_ws.clone()
    }

    #[getter]
    fn http_timeout_secs(&self) -> u64 {
        self.http_timeout_secs
    }

    fn __repr__(&self) -> String {
        format!("{self:?}")
    }
}

#[pymethods]
impl RisexExecClientConfig {
    /// `margin_mode` maps an instrument ID to `"CROSS"` or `"ISOLATED"`.
    #[new]
    #[pyo3(signature = (
        account_id = None,
        environment = None,
        base_url_http = None,
        base_url_ws = None,
        proxy_url = None,
        http_timeout_secs = None,
        rest_quota_per_10s = None,
        account_address = None,
        signer_private_key = None,
        market_order_slippage_bps = None,
        deadline_secs = None,
        tpsl_poll_interval_secs = None,
        builder_id = None,
        leverage = None,
        margin_mode = None,
    ))]
    #[expect(clippy::too_many_arguments)]
    fn py_new(
        account_id: Option<AccountId>,
        environment: Option<RisexEnvironment>,
        base_url_http: Option<String>,
        base_url_ws: Option<String>,
        proxy_url: Option<String>,
        http_timeout_secs: Option<u64>,
        rest_quota_per_10s: Option<u32>,
        account_address: Option<String>,
        signer_private_key: Option<String>,
        market_order_slippage_bps: Option<u32>,
        deadline_secs: Option<u32>,
        tpsl_poll_interval_secs: Option<u64>,
        builder_id: Option<u16>,
        leverage: Option<HashMap<InstrumentId, u8>>,
        margin_mode: Option<HashMap<InstrumentId, String>>,
    ) -> PyResult<Self> {
        let defaults = Self::default();
        let margin_mode = margin_mode
            .unwrap_or_default()
            .into_iter()
            .map(|(id, mode)| Ok((id, RisexMarginMode::from_str(&mode)?)))
            .collect::<Result<_, String>>()
            .map_err(to_pyvalue_err)?;
        Ok(Self {
            environment: environment.unwrap_or(defaults.environment),
            base_url_http,
            base_url_ws,
            proxy_url,
            http_timeout_secs: http_timeout_secs.unwrap_or(defaults.http_timeout_secs),
            rest_quota_per_10s,
            account_id: account_id.unwrap_or(defaults.account_id),
            account_address,
            signer_private_key,
            market_order_slippage_bps: market_order_slippage_bps
                .unwrap_or(defaults.market_order_slippage_bps),
            deadline_secs: deadline_secs.unwrap_or(defaults.deadline_secs),
            tpsl_poll_interval_secs: tpsl_poll_interval_secs
                .unwrap_or(defaults.tpsl_poll_interval_secs),
            builder_id: builder_id.unwrap_or(defaults.builder_id),
            leverage: leverage.unwrap_or_default(),
            margin_mode,
        })
    }

    #[getter]
    fn environment(&self) -> RisexEnvironment {
        self.environment
    }

    #[getter]
    fn account_id(&self) -> AccountId {
        self.account_id
    }

    #[getter]
    fn account_address(&self) -> Option<String> {
        self.account_address.clone()
    }

    #[getter]
    fn market_order_slippage_bps(&self) -> u32 {
        self.market_order_slippage_bps
    }

    #[getter]
    fn deadline_secs(&self) -> u32 {
        self.deadline_secs
    }

    // No getter for `signer_private_key`; `__repr__` redacts it.
    fn __repr__(&self) -> String {
        format!("{self:?}")
    }
}
