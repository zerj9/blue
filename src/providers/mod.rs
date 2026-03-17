mod upcloud;

use crate::provider::{ProviderMode, ProviderRegistry};

pub fn build_registry(mode: ProviderMode) -> ProviderRegistry {
    let mut registry = ProviderRegistry::new(mode);
    registry.register("upcloud", |mode| Ok(Box::new(upcloud::Client::new(mode)?)));
    registry
}
