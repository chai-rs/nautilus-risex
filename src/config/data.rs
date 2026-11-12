//! Data client configuration.

use std::any::Any;

use nautilus_common::factories::ClientConfig;
use serde::{Deserialize, Serialize};

use crate::common::{consts::HTTP_TIMEOUT_SECS, enums::RisexEnvironment};

/// Configuration of the RISEx data client. Market data is public: no credentials.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[cfg_attr(
    feature = "python",
    pyo3::pyclass(module = "nautilus_risex", from_py_object)
)]
pub struct RisexDataClientConfig {
    pub environment: RisexEnvironment,
    /// Replaces the REST base URL of the environment.
    pub base_url_http: Option<String>,
    /// Replaces the WebSocket URL of the environment.
    pub base_url_ws: Option<String>,
    pub http_timeout_secs: u64,
    /// Poll interval of `GET /v1/markets` and `GET /v1/system/config` while an instrument
    /// status is subscribed. The venue caches the markets for 5 minutes.
    pub instrument_status_poll_interval_secs: u64,
}

impl Default for RisexDataClientConfig {
    fn default() -> Self {
        Self {
            environment: RisexEnvironment::default(),
            base_url_http: None,
            base_url_ws: None,
            http_timeout_secs: HTTP_TIMEOUT_SECS,
            instrument_status_poll_interval_secs: 60,
        }
    }
}

impl ClientConfig for RisexDataClientConfig {
    fn as_any(&self) -> &dyn Any {
        self
    }
}
