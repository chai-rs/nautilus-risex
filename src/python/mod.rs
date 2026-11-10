//! Python bindings: configurations, factories and signer revocation.
//!
//! The pattern is the one of `nautilus-lighter`. No stub generation: the stubs
//! serve the official `nautilus_trader` wheel only.

pub mod config;
pub mod factories;

use nautilus_common::factories::{ClientConfig, DataClientFactory, ExecutionClientFactory};
use nautilus_core::python::{to_pyruntime_err, to_pyvalue_err};
use nautilus_system::get_global_pyo3_registry;
use pyo3::prelude::*;

use crate::{
    common::{consts::RISEX, enums::RisexEnvironment},
    config::{data::RisexDataClientConfig, exec::RisexExecClientConfig},
    execution::revoke::SignerRevocation,
    factories::{data::RisexDataClientFactory, exec::RisexExecutionClientFactory},
};

#[expect(clippy::needless_pass_by_value)]
fn extract_data_factory(
    py: Python<'_>,
    factory: Py<PyAny>,
) -> PyResult<Box<dyn DataClientFactory>> {
    match factory.extract::<RisexDataClientFactory>(py) {
        Ok(f) => Ok(Box::new(f)),
        Err(e) => Err(to_pyvalue_err(format!(
            "Failed to extract RisexDataClientFactory: {e}"
        ))),
    }
}

#[expect(clippy::needless_pass_by_value)]
fn extract_exec_factory(
    py: Python<'_>,
    factory: Py<PyAny>,
) -> PyResult<Box<dyn ExecutionClientFactory>> {
    match factory.extract::<RisexExecutionClientFactory>(py) {
        Ok(f) => Ok(Box::new(f)),
        Err(e) => Err(to_pyvalue_err(format!(
            "Failed to extract RisexExecutionClientFactory: {e}"
        ))),
    }
}

#[expect(clippy::needless_pass_by_value)]
fn extract_data_config(py: Python<'_>, config: Py<PyAny>) -> PyResult<Box<dyn ClientConfig>> {
    match config.extract::<RisexDataClientConfig>(py) {
        Ok(c) => Ok(Box::new(c)),
        Err(e) => Err(to_pyvalue_err(format!(
            "Failed to extract RisexDataClientConfig: {e}"
        ))),
    }
}

#[expect(clippy::needless_pass_by_value)]
fn extract_exec_config(py: Python<'_>, config: Py<PyAny>) -> PyResult<Box<dyn ClientConfig>> {
    match config.extract::<RisexExecClientConfig>(py) {
        Ok(c) => Ok(Box::new(c)),
        Err(e) => Err(to_pyvalue_err(format!(
            "Failed to extract RisexExecClientConfig: {e}"
        ))),
    }
}

/// Revokes the session signer named by the environment variables. The account
/// (wallet) key signs the request. There is no confirmation prompt.
#[pyfunction]
#[pyo3(name = "revoke_risex_signer", signature = (environment = RisexEnvironment::Mainnet))]
fn py_revoke_risex_signer(
    py: Python<'_>,
    environment: RisexEnvironment,
) -> PyResult<Bound<'_, PyAny>> {
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let revocation = SignerRevocation::from_env(environment).map_err(to_pyvalue_err)?;
        let signer = revocation.signer();
        revocation.submit().await.map_err(to_pyvalue_err)?;
        Ok(format!("revoked signer {signer}"))
    })
}

/// Adds the RISEx classes to `m` and registers the extractors of the live node.
///
/// # Errors
///
/// Returns an error if a class or an extractor cannot be registered.
#[pymodule]
pub fn risex(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add(stringify!(RISEX), RISEX)?;
    m.add_class::<RisexEnvironment>()?;
    m.add_class::<RisexDataClientConfig>()?;
    m.add_class::<RisexDataClientFactory>()?;
    m.add_class::<RisexExecClientConfig>()?;
    m.add_class::<RisexExecutionClientFactory>()?;
    m.add_function(wrap_pyfunction!(py_revoke_risex_signer, m)?)?;

    let registry = get_global_pyo3_registry();
    registry
        .register_factory_extractor(RISEX.to_string(), extract_data_factory)
        .map_err(to_pyruntime_err)?;
    registry
        .register_exec_factory_extractor(RISEX.to_string(), extract_exec_factory)
        .map_err(to_pyruntime_err)?;
    registry
        .register_config_extractor("RisexDataClientConfig".to_string(), extract_data_config)
        .map_err(to_pyruntime_err)?;
    registry
        .register_config_extractor("RisexExecClientConfig".to_string(), extract_exec_config)
        .map_err(to_pyruntime_err)?;
    Ok(())
}
