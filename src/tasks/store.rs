use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::model::Task;
use super::require;

#[derive(Default, Deserialize, Serialize)]
pub(super) struct Registry {
    pub schema_version: u32,
    pub sequence: u64,
    pub tasks: Vec<Task>,
}

impl Registry {
    pub fn task(&self, id: &str) -> Result<&Task> {
        self.tasks
            .iter()
            .find(|task| task.id == id)
            .context(super::Fault {
                code: "task_not_found",
                message: "task does not exist in this Herdr session",
            })
    }

    pub fn task_mut(&mut self, id: &str) -> Result<&mut Task> {
        self.tasks
            .iter_mut()
            .find(|task| task.id == id)
            .context(super::Fault {
                code: "task_not_found",
                message: "task does not exist in this Herdr session",
            })
    }
}

#[derive(Clone)]
pub(super) struct Store {
    pub path: PathBuf,
}

impl Store {
    fn decode(bytes: &[u8]) -> Result<Registry> {
        let registry: Registry = serde_json::from_slice(bytes)
            .context("task registry is invalid; preserve it for recovery")?;
        require(
            registry.schema_version == 1,
            "unsupported_schema",
            "unsupported task registry schema",
        )?;
        Ok(registry)
    }

    pub fn read(&self) -> Result<Registry> {
        match fs::read(&self.path) {
            Ok(bytes) => Self::decode(&bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Registry {
                schema_version: 1,
                ..Registry::default()
            }),
            Err(e) => Err(e).context("cannot read task registry"),
        }
    }

    pub fn update<T>(&self, f: impl FnOnce(&mut Registry) -> Result<T>) -> Result<T> {
        crate::state::update_private(&self.path, |bytes| {
            let mut registry = match bytes {
                Some(bytes) => Self::decode(bytes)?,
                // update_private intentionally collapses read errors for its other callers.
                None => self.read()?,
            };
            let value = f(&mut registry)?;
            Ok((serde_json::to_vec_pretty(&registry)?, value))
        })
    }

    pub fn edit(
        &self,
        id: &str,
        generation: u64,
        f: impl FnOnce(&mut Task) -> Result<()>,
    ) -> Result<Task> {
        self.update(|registry| {
            let task = registry.task_mut(id)?;
            task.generation_is(generation)?;
            f(task)?;
            task.updated_at = crate::state::now();
            Ok(task.clone())
        })
    }
}
