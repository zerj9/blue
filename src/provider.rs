use std::collections::HashMap;

use serde_json::Value;

use crate::resolvable::Resolvable;
use crate::state::SchemaResolver;
use crate::types::{Diff, OperationResult, Schema};

pub trait OperationCtx {
    fn save(&self, outputs: &Value);
}

pub trait ResourceType {
    fn schema(&self) -> &Schema;

    /// Plan-time validation of resolved inputs. Receives a `Resolvable` so
    /// providers can decide how to handle partially-resolved inputs (those
    /// containing `{{ }}` refs to not-yet-deployed resources).
    ///
    /// Common pattern for providers that only validate fully concrete
    /// inputs:
    ///
    /// ```ignore
    /// fn validate(&self, inputs: &Resolvable) -> Result<(), String> {
    ///     let Some(inputs) = inputs.as_concrete() else { return Ok(()) };
    ///     // ...existing concrete-Value validation
    /// }
    /// ```
    ///
    /// Providers wanting plan-time checks on pending values can match on
    /// the variants directly. Validation runs again at deploy time after
    /// strict re-resolution, when `inputs` is guaranteed concrete.
    fn validate(&self, _inputs: &Resolvable) -> Result<(), String> {
        Ok(())
    }
    fn create(&self, ctx: &dyn OperationCtx, inputs: Value) -> Result<OperationResult, String>;
    fn read(&self, outputs: &Value) -> Result<OperationResult, String>;
    fn update(
        &self,
        ctx: &dyn OperationCtx,
        old_inputs: &Value,
        old_outputs: &Value,
        new_inputs: Value,
    ) -> Result<OperationResult, String>;
    fn delete(&self, ctx: &dyn OperationCtx, outputs: &Value) -> Result<OperationResult, String>;

    /// Plan-time hook to customize the computed diff (e.g. promote an
    /// Update to a Replace based on field-level rules). Same `Resolvable`
    /// rationale as `validate` — providers choose how to handle pending
    /// inputs via the same `as_concrete()` early-return pattern.
    fn customize_diff(
        &self,
        _diff: &mut Diff,
        _inputs: &Resolvable,
        _outputs: &Value,
    ) -> Result<(), String> {
        Ok(())
    }
}

pub trait DataSourceType {
    fn schema(&self) -> &Schema;
    fn read(&self, inputs: Value) -> Result<Value, String>;
}

pub trait ProviderInstance {
    fn resource_type(&self, name: &str) -> Option<&dyn ResourceType>;
    fn data_source_type(&self, name: &str) -> Option<&dyn DataSourceType>;
}

pub struct Providers {
    instances: HashMap<String, Box<dyn ProviderInstance>>,
}

impl Providers {
    pub fn new() -> Self {
        Providers {
            instances: HashMap::new(),
        }
    }

    pub fn register(&mut self, name: &str, instance: Box<dyn ProviderInstance>) {
        self.instances.insert(name.to_string(), instance);
    }

    /// Look up a resource type by full type string (e.g. "upcloud.server" or "blue.script")
    pub fn resource_type(&self, type_str: &str) -> Option<&dyn ResourceType> {
        let (provider, name) = type_str.split_once('.')?;
        self.instances.get(provider)?.resource_type(name)
    }

    /// Look up a data source type by full type string
    pub fn data_source_type(&self, type_str: &str) -> Option<&dyn DataSourceType> {
        let (provider, name) = type_str.split_once('.')?;
        self.instances.get(provider)?.data_source_type(name)
    }
}

impl SchemaResolver for Providers {
    fn schema(&self, type_name: &str) -> Option<&Schema> {
        self.resource_type(type_name).map(|rt| rt.schema())
    }
}
