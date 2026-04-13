// Stream relay module for multi-replica coordination
pub mod in_memory_registry;
#[cfg(test)]
pub mod mock_registry;
pub mod publisher_manager;
pub mod registry;
pub mod registry_trait;

pub use in_memory_registry::InMemoryStreamRegistry;
pub use registry::{
    PublisherInfo, RegistryConnectionRuntime, StreamRegistry, HEARTBEAT_INTERVAL_SECS,
    PUBLISHER_TTL_SECS,
};
pub use registry_trait::StreamRegistryTrait;

#[cfg(test)]
pub use mock_registry::MockStreamRegistry;

pub use publisher_manager::PublisherManager;
