//! Python bindings of the client factories.

use pyo3::prelude::*;

use crate::{
    common::consts::RISEX,
    factories::{data::RisexDataClientFactory, exec::RisexExecutionClientFactory},
};

#[pymethods]
impl RisexDataClientFactory {
    #[new]
    fn py_new() -> Self {
        Self
    }

    #[pyo3(name = "name")]
    fn py_name(&self) -> &'static str {
        RISEX
    }
}

#[pymethods]
impl RisexExecutionClientFactory {
    #[new]
    fn py_new() -> Self {
        Self
    }

    #[pyo3(name = "name")]
    fn py_name(&self) -> &'static str {
        RISEX
    }
}
