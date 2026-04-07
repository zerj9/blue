pub mod managed_object_storage;
pub mod managed_object_storage_service;
pub mod server;
pub mod storage;

use std::sync::OnceLock;

use crate::provider::{OperationResult, Provider, ProviderMode};
use crate::schema::Schema;

static SERVER_SCHEMA: OnceLock<Schema> = OnceLock::new();

fn server_schema() -> &'static Schema {
    SERVER_SCHEMA.get_or_init(|| {
        Schema::from_toml(include_str!("schemas/server.toml"))
            .expect("built-in server schema is invalid")
    })
}

static STORAGE_DATA_SCHEMA: OnceLock<Schema> = OnceLock::new();

fn storage_data_schema() -> &'static Schema {
    STORAGE_DATA_SCHEMA.get_or_init(|| {
        Schema::from_toml(include_str!("schemas/storage_data.toml"))
            .expect("built-in storage data schema is invalid")
    })
}

static MANAGED_OBJECT_STORAGE_REGIONS_SCHEMA: OnceLock<Schema> = OnceLock::new();

fn managed_object_storage_regions_schema() -> &'static Schema {
    MANAGED_OBJECT_STORAGE_REGIONS_SCHEMA.get_or_init(|| {
        Schema::from_toml(include_str!("schemas/managed_object_storage_regions.toml"))
            .expect("built-in managed object storage regions schema is invalid")
    })
}

static MANAGED_OBJECT_STORAGE_SERVICE_SCHEMA: OnceLock<Schema> = OnceLock::new();

fn managed_object_storage_service_schema() -> &'static Schema {
    MANAGED_OBJECT_STORAGE_SERVICE_SCHEMA.get_or_init(|| {
        Schema::from_toml(include_str!("schemas/managed_object_storage_service.toml"))
            .expect("built-in managed object storage service schema is invalid")
    })
}

pub struct Client {
    pub(crate) http: reqwest::blocking::Client,
    pub(crate) base_url: String,
    pub(crate) token: String,
}

impl Client {
    pub fn new(mode: ProviderMode) -> Result<Self, Box<dyn std::error::Error>> {
        let token = match mode {
            ProviderMode::Live => std::env::var("UPCLOUD_TOKEN")
                .map_err(|_| "UPCLOUD_TOKEN environment variable not set")?,
            ProviderMode::SchemaOnly => String::new(),
        };

        Ok(Self {
            http: reqwest::blocking::Client::new(),
            base_url: "https://api.upcloud.com".to_string(),
            token,
        })
    }
}

impl Provider for Client {
    fn resolve_data_source(
        &self,
        data_type: &str,
        filters: serde_json::Value,
    ) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
        match data_type {
            "storage" => storage::resolve(self, filters),
            "managed_object_storage_regions" => {
                managed_object_storage::resolve_regions(self, filters)
            }
            other => Err(format!("unknown upcloud data source type: {other}").into()),
        }
    }

    fn create_resource(
        &self,
        resource_type: &str,
        properties: serde_json::Value,
    ) -> Result<OperationResult, Box<dyn std::error::Error>> {
        match resource_type {
            "server" => server::create(self, properties),
            "managed_object_storage_service" => managed_object_storage_service::create(self, properties),
            other => Err(format!("unknown upcloud resource type: {other}").into()),
        }
    }

    fn read_resource(
        &self,
        resource_type: &str,
        outputs: &serde_json::Value,
    ) -> Result<OperationResult, Box<dyn std::error::Error>> {
        match resource_type {
            "server" => server::read(self, outputs),
            "managed_object_storage_service" => managed_object_storage_service::read(self, outputs),
            other => Err(format!("unknown upcloud resource type: {other}").into()),
        }
    }

    fn delete_resource(
        &self,
        resource_type: &str,
        outputs: &serde_json::Value,
    ) -> Result<OperationResult, Box<dyn std::error::Error>> {
        match resource_type {
            "server" => server::delete(self, outputs),
            "managed_object_storage_service" => managed_object_storage_service::delete(self, outputs),
            other => Err(format!("unknown upcloud resource type: {other}").into()),
        }
    }

    fn update_resource(
        &self,
        resource_type: &str,
        old_outputs: &serde_json::Value,
        new_properties: serde_json::Value,
    ) -> Result<OperationResult, Box<dyn std::error::Error>> {
        match resource_type {
            "server" => server::update(self, old_outputs, new_properties),
            "managed_object_storage_service" => managed_object_storage_service::update(self, old_outputs, new_properties),
            other => Err(format!("update not supported for upcloud resource type: {other}").into()),
        }
    }

    fn resource_schema(&self, resource_type: &str) -> Option<&Schema> {
        match resource_type {
            "server" => Some(server_schema()),
            "managed_object_storage_service" => Some(managed_object_storage_service_schema()),
            _ => None,
        }
    }

    fn data_source_schema(&self, data_type: &str) -> Option<&Schema> {
        match data_type {
            "storage" => Some(storage_data_schema()),
            "managed_object_storage_regions" => Some(managed_object_storage_regions_schema()),
            _ => None,
        }
    }
}
