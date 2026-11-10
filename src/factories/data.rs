//! Data client factory.

use std::{cell::RefCell, rc::Rc};

use nautilus_common::{
    cache::CacheView,
    clients::DataClient,
    clock::Clock,
    factories::{ClientConfig, DataClientFactory},
};
use nautilus_model::identifiers::ClientId;

use crate::{common::consts::RISEX, config::data::RisexDataClientConfig, data::RisexDataClient};

/// Creates [`RisexDataClient`]s for the live node.
#[derive(Debug, Clone, Default)]
#[cfg_attr(
    feature = "python",
    pyo3::pyclass(module = "nautilus_risex", from_py_object)
)]
pub struct RisexDataClientFactory;

impl RisexDataClientFactory {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl DataClientFactory for RisexDataClientFactory {
    fn create(
        &self,
        name: &str,
        config: &dyn ClientConfig,
        _cache: CacheView,
        _clock: Rc<RefCell<dyn Clock>>,
    ) -> anyhow::Result<Box<dyn DataClient>> {
        let config = config
            .as_any()
            .downcast_ref::<RisexDataClientConfig>()
            .ok_or_else(|| {
                anyhow::anyhow!("Invalid config type for RisexDataClientFactory: {config:?}")
            })?
            .clone();
        Ok(Box::new(RisexDataClient::new(
            ClientId::from(name),
            config,
        )?))
    }

    fn name(&self) -> &'static str {
        RISEX
    }

    fn config_type(&self) -> &'static str {
        stringify!(RisexDataClientConfig)
    }
}
