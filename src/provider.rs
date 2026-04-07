use std::collections::HashMap;

use crate::config;
use crate::schema::Schema;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ProviderMode {
    SchemaOnly,
    Live,
}

#[derive(Debug)]
pub enum OperationResult {
    Complete { outputs: serde_json::Value },
    InProgress { outputs: serde_json::Value },
    Updating { outputs: serde_json::Value },
    Failed { error: String },
}

pub trait Provider {
    fn resolve_data_source(
        &self,
        data_type: &str,
        filters: serde_json::Value,
    ) -> Result<serde_json::Value, Box<dyn std::error::Error>>;

    fn create_resource(
        &self,
        resource_type: &str,
        properties: serde_json::Value,
    ) -> Result<OperationResult, Box<dyn std::error::Error>>;

    fn read_resource(
        &self,
        resource_type: &str,
        outputs: &serde_json::Value,
    ) -> Result<OperationResult, Box<dyn std::error::Error>>;

    fn update_resource(
        &self,
        resource_type: &str,
        old_outputs: &serde_json::Value,
        new_properties: serde_json::Value,
    ) -> Result<OperationResult, Box<dyn std::error::Error>>;

    fn delete_resource(
        &self,
        resource_type: &str,
        outputs: &serde_json::Value,
    ) -> Result<OperationResult, Box<dyn std::error::Error>>;

    fn resource_schema(&self, resource_type: &str) -> Option<&Schema>;

    fn data_source_schema(&self, data_type: &str) -> Option<&Schema>;
}

type ProviderFactory = fn(ProviderMode) -> Result<Box<dyn Provider>, Box<dyn std::error::Error>>;

pub struct ProviderRegistry {
    mode: ProviderMode,
    factories: HashMap<String, ProviderFactory>,
    providers: HashMap<String, Box<dyn Provider>>,
}

impl ProviderRegistry {
    pub fn new(mode: ProviderMode) -> Self {
        Self {
            mode,
            factories: HashMap::new(),
            providers: HashMap::new(),
        }
    }

    pub fn register(&mut self, name: &str, factory: ProviderFactory) {
        self.factories.insert(name.to_string(), factory);
    }

    fn get_or_init(&mut self, name: &str) -> Result<&dyn Provider, Box<dyn std::error::Error>> {
        if !self.providers.contains_key(name) {
            let factory = self
                .factories
                .get(name)
                .ok_or_else(|| format!("unknown provider '{name}'"))?;
            let provider = factory(self.mode)?;
            self.providers.insert(name.to_string(), provider);
        }
        Ok(self.providers.get(name).unwrap().as_ref())
    }

    pub fn create_resource(
        &mut self,
        full_type: &str,
        properties: serde_json::Value,
    ) -> Result<OperationResult, Box<dyn std::error::Error>> {
        let (provider_name, resource_type) = config::split_provider_type(full_type)?;
        let provider = self.get_or_init(provider_name)?;
        provider.create_resource(resource_type, properties)
    }

    pub fn read_resource(
        &mut self,
        full_type: &str,
        outputs: &serde_json::Value,
    ) -> Result<OperationResult, Box<dyn std::error::Error>> {
        let (provider_name, resource_type) = config::split_provider_type(full_type)?;
        let provider = self.get_or_init(provider_name)?;
        provider.read_resource(resource_type, outputs)
    }

    pub fn delete_resource(
        &mut self,
        full_type: &str,
        outputs: &serde_json::Value,
    ) -> Result<OperationResult, Box<dyn std::error::Error>> {
        let (provider_name, resource_type) = config::split_provider_type(full_type)?;
        let provider = self.get_or_init(provider_name)?;
        provider.delete_resource(resource_type, outputs)
    }

    pub fn update_resource(
        &mut self,
        full_type: &str,
        old_outputs: &serde_json::Value,
        new_properties: serde_json::Value,
    ) -> Result<OperationResult, Box<dyn std::error::Error>> {
        let (provider_name, resource_type) = config::split_provider_type(full_type)?;
        let provider = self.get_or_init(provider_name)?;
        provider.update_resource(resource_type, old_outputs, new_properties)
    }

    pub fn resource_schema(
        &mut self,
        full_type: &str,
    ) -> Result<Option<&Schema>, Box<dyn std::error::Error>> {
        let (provider_name, resource_type) = config::split_provider_type(full_type)?;
        let provider = self.get_or_init(provider_name)?;
        Ok(provider.resource_schema(resource_type))
    }

    pub fn resolve_single_data_source(
        &mut self,
        provider_name: &str,
        data_type: &str,
        filters: serde_json::Value,
    ) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
        let provider = self.get_or_init(provider_name)?;
        provider.resolve_data_source(data_type, filters)
    }

    pub fn data_source_schema_for(
        &mut self,
        provider_name: &str,
        data_type: &str,
    ) -> Option<&Schema> {
        let provider = self.get_or_init(provider_name).ok()?;
        provider.data_source_schema(data_type)
    }

    pub fn ensure_providers(&mut self, names: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
        for name in names {
            self.get_or_init(name)?;
        }
        Ok(())
    }

    pub fn resource_schema_ref(
        &self,
        full_type: &str,
    ) -> Result<Option<&Schema>, Box<dyn std::error::Error>> {
        let (provider_name, resource_type) = config::split_provider_type(full_type)?;
        let provider = self
            .providers
            .get(provider_name)
            .ok_or_else(|| format!("provider '{provider_name}' not initialized"))?;
        Ok(provider.resource_schema(resource_type))
    }

    pub fn data_source_schema_ref(
        &self,
        full_type: &str,
    ) -> Result<Option<&Schema>, Box<dyn std::error::Error>> {
        let (provider_name, data_type) = config::split_provider_type(full_type)?;
        let provider = self
            .providers
            .get(provider_name)
            .ok_or_else(|| format!("provider '{provider_name}' not initialized"))?;
        Ok(provider.data_source_schema(data_type))
    }

}
