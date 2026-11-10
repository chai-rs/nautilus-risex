//! Python binding tests: an embedded interpreter, no network.

#![cfg(feature = "python")]

use nautilus_model::identifiers::{AccountId, InstrumentId};
use nautilus_risex::{
    common::{
        consts::RISEX,
        enums::{RisexEnvironment, RisexMarginMode},
    },
    config::{data::RisexDataClientConfig, exec::RisexExecClientConfig},
    factories::{data::RisexDataClientFactory, exec::RisexExecutionClientFactory},
    python,
};
use nautilus_system::get_global_pyo3_registry;
use pyo3::{
    Bound, Py, Python,
    types::{PyAnyMethods, PyDict, PyDictMethods, PyModule},
};

const AERO: &str = "AERO-USDC-PERP.RISEX";

fn module(py: Python<'_>) -> Bound<'_, PyModule> {
    let module = PyModule::new(py, "risex").unwrap();
    python::risex(&module).unwrap();
    module
}

// One test: the global registry accepts each extractor once per process.
#[test]
fn module_registers_classes_and_extractors() {
    Python::initialize();
    Python::attach(|py| {
        let module = module(py);
        configs_construct_from_python(&module);
        factories_and_configs_extract_through_the_registry(py);
    });
}

fn configs_construct_from_python(module: &Bound<'_, PyModule>) {
    let py = module.py();
    let data = module
        .getattr("RisexDataClientConfig")
        .unwrap()
        .call0()
        .unwrap()
        .extract::<RisexDataClientConfig>()
        .unwrap();
    assert_eq!(data.environment, RisexEnvironment::Mainnet);
    assert_eq!(data.http_timeout_secs, 10);

    let aero = || Py::new(py, InstrumentId::from(AERO)).unwrap();
    let exec_class = module.getattr("RisexExecClientConfig").unwrap();
    let kwargs = PyDict::new(py);
    kwargs
        .set_item(
            "account_id",
            Py::new(py, AccountId::from("RISEX-007")).unwrap(),
        )
        .unwrap();
    kwargs
        .set_item(
            "environment",
            module
                .getattr("RisexEnvironment")
                .unwrap()
                .getattr("TESTNET")
                .unwrap(),
        )
        .unwrap();
    kwargs.set_item("signer_private_key", "0xdeadbeef").unwrap();
    let leverage = PyDict::new(py);
    leverage.set_item(aero(), 3).unwrap();
    kwargs.set_item("leverage", leverage).unwrap();
    let margin_mode = PyDict::new(py);
    margin_mode.set_item(aero(), "ISOLATED").unwrap();
    kwargs.set_item("margin_mode", &margin_mode).unwrap();

    let object = exec_class.call((), Some(&kwargs)).unwrap();
    let repr = object.repr().unwrap().to_string();
    assert!(!repr.contains("deadbeef"), "{repr}");
    let exec = object.extract::<RisexExecClientConfig>().unwrap();
    let id = InstrumentId::from(AERO);
    assert_eq!(exec.account_id, AccountId::from("RISEX-007"));
    assert_eq!(exec.environment, RisexEnvironment::Testnet);
    assert_eq!(exec.leverage[&id], 3);
    assert_eq!(exec.margin_mode[&id], RisexMarginMode::Isolated);
    assert_eq!(exec.deadline_secs, 60);

    margin_mode.set_item(aero(), "HEDGED").unwrap();
    assert!(exec_class.call((), Some(&kwargs)).is_err());
}

fn factories_and_configs_extract_through_the_registry(py: Python<'_>) {
    let registry = get_global_pyo3_registry();

    let factory = Py::new(py, RisexDataClientFactory::new())
        .unwrap()
        .into_any();
    let factory = registry.extract_factory(py, factory).unwrap();
    assert_eq!(factory.name(), RISEX);
    assert_eq!(factory.config_type(), "RisexDataClientConfig");

    let factory = Py::new(py, RisexExecutionClientFactory).unwrap().into_any();
    let factory = registry.extract_exec_factory(py, factory).unwrap();
    assert_eq!(factory.name(), RISEX);
    assert_eq!(factory.config_type(), "RisexExecClientConfig");

    let config = RisexDataClientConfig {
        http_timeout_secs: 7,
        ..Default::default()
    };
    let config = Py::new(py, config).unwrap().into_any();
    let config = registry.extract_config(py, config).unwrap();
    let config = config
        .as_any()
        .downcast_ref::<RisexDataClientConfig>()
        .unwrap();
    assert_eq!(config.http_timeout_secs, 7);

    let config = RisexExecClientConfig {
        builder_id: 9,
        ..Default::default()
    };
    let config = Py::new(py, config).unwrap().into_any();
    let config = registry.extract_config(py, config).unwrap();
    let config = config
        .as_any()
        .downcast_ref::<RisexExecClientConfig>()
        .unwrap();
    assert_eq!(config.builder_id, 9);
}
