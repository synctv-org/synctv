// Stream relay module for multi-replica coordination
pub mod in_memory_registry;
pub mod publisher_manager;
pub mod registry;
pub mod registry_trait;
#[cfg(test)]
pub(crate) mod test_registry;

use std::sync::Arc;

pub use in_memory_registry::InMemoryStreamRegistry;
pub use registry::{
    PublisherInfo, RedisOperationTimeout, RegistryConnectionRuntime, StreamRegistry,
    HEARTBEAT_INTERVAL_SECS, PUBLISHER_TTL_SECS,
};
pub use registry_trait::{ActivePublisherEntry, StreamRegistryTrait};

#[cfg(test)]
pub(crate) use test_registry::TestStreamRegistry;

pub use publisher_manager::PublisherManager;

/// Build a local-only stream registry behind the trait abstraction.
#[must_use]
pub fn local_stream_registry() -> Arc<dyn StreamRegistryTrait> {
    Arc::new(InMemoryStreamRegistry::new())
}

/// Build a shared stream registry behind the trait abstraction.
#[must_use]
pub fn shared_stream_registry(
    runtime: Arc<dyn RegistryConnectionRuntime>,
    key_prefix: impl Into<String>,
) -> Arc<dyn StreamRegistryTrait> {
    Arc::new(StreamRegistry::from_runtime(runtime, key_prefix))
}
