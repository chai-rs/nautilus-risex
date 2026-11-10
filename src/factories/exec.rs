//! Execution client factory.

use std::{any::Any, cell::RefCell, rc::Rc};

use nautilus_common::{
    cache::CacheView,
    clients::ExecutionClient,
    clock::Clock,
    factories::{ClientConfig, ExecutionClientFactory},
};
use nautilus_live::ExecutionClientCore;
use nautilus_model::{
    enums::{AccountType, OmsType},
    identifiers::{ClientId, TraderId},
};

use crate::{
    common::consts::{RISEX, RISEX_VENUE},
    config::exec::RisexExecClientConfig,
    execution::RisexExecutionClient,
};

impl ClientConfig for RisexExecClientConfig {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Factory for [`RisexExecutionClient`].
#[derive(Debug, Clone, Default)]
#[cfg_attr(
    feature = "python",
    pyo3::pyclass(module = "nautilus_risex", from_py_object)
)]
pub struct RisexExecutionClientFactory;

impl ExecutionClientFactory for RisexExecutionClientFactory {
    fn create(
        &self,
        trader_id: TraderId,
        name: &str,
        config: &dyn ClientConfig,
        cache: CacheView,
        _clock: Rc<RefCell<dyn Clock>>,
    ) -> anyhow::Result<Box<dyn ExecutionClient>> {
        let config = config
            .as_any()
            .downcast_ref::<RisexExecClientConfig>()
            .ok_or_else(|| anyhow::anyhow!("expected RisexExecClientConfig, was {config:?}"))?
            .clone();
        // One position per market and a margin account.
        let core = ExecutionClientCore::new(
            trader_id,
            ClientId::from(name),
            *RISEX_VENUE,
            OmsType::Netting,
            config.account_id,
            AccountType::Margin,
            None,
            cache,
        );
        Ok(Box::new(RisexExecutionClient::new(core, config)?))
    }

    fn name(&self) -> &'static str {
        RISEX
    }

    fn config_type(&self) -> &'static str {
        "RisexExecClientConfig"
    }
}
