// Stream relay module for multi-replica coordination
pub(crate) mod in_memory_registry;
pub(crate) mod publisher_manager;
pub(crate) mod registry;
pub(crate) mod registry_trait;
#[cfg(test)]
pub(crate) mod test_registry;

use std::sync::Arc;

use in_memory_registry::InMemoryStreamRegistry;

pub use registry::{PublisherInfo, RegistryConnectionRuntime, PUBLISHER_TTL_SECS};
pub use registry_trait::{ActivePublisherEntry, PublisherRefreshOutcome, StreamRegistryTrait};

#[cfg(test)]
pub(crate) use test_registry::TestStreamRegistry;

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
    Arc::new(registry::StreamRegistry::from_runtime(runtime, key_prefix))
}
