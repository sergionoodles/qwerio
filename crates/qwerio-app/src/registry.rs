//! The explicit driver registry populated at startup.

use std::collections::BTreeMap;
use std::sync::Arc;

use qwerio_core::capabilities::DatabaseCapabilities;
use qwerio_core::connection::EngineKind;
use qwerio_core::driver::DatabaseDriver;

use crate::error::{AppError, AppResult};

#[derive(Default)]
pub struct DriverRegistry {
    drivers: BTreeMap<EngineKind, Arc<dyn DatabaseDriver>>,
}

impl DriverRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a driver, replacing any earlier one for the same engine.
    pub fn register(&mut self, driver: Arc<dyn DatabaseDriver>) {
        self.drivers.insert(driver.engine(), driver);
    }

    pub fn driver(&self, engine: EngineKind) -> AppResult<Arc<dyn DatabaseDriver>> {
        self.drivers
            .get(&engine)
            .cloned()
            .ok_or(AppError::MissingDriver(engine))
    }

    pub fn capabilities(&self, engine: EngineKind) -> Option<&DatabaseCapabilities> {
        self.drivers
            .get(&engine)
            .map(|driver| driver.capabilities())
    }

    pub fn engines(&self) -> impl Iterator<Item = EngineKind> + '_ {
        self.drivers.keys().copied()
    }
}
