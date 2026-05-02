use std::collections::HashMap;

use serde_json::Value;

use crate::types::{Diff, OperationResult, Schema};

pub trait OperationCtx {
    fn save(&self, outputs: &Value);
}

pub trait ResourceType {
    fn schema(&self) -> &Schema;
    fn validate(&self, _inputs: &Value) -> Result<(), String> {
        Ok(())
    }
    fn create(&self, ctx: &dyn OperationCtx, inputs: Value) -> Result<OperationResult, String>;
    fn read(&self, outputs: &Value) -> Result<OperationResult, String>;
    fn update(
        &self,
        ctx: &dyn OperationCtx,
        old_outputs: &Value,
        new_inputs: Value,
    ) -> Result<OperationResult, String>;
    fn delete(&self, ctx: &dyn OperationCtx, outputs: &Value) -> Result<OperationResult, String>;
    fn customize_diff(
        &self,
        _diff: &mut Diff,
        _inputs: &Value,
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
