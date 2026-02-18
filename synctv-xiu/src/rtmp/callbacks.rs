//! Stream event callbacks for decoupling metrics/instrumentation from the RTMP layer.
//!
//! Instead of directly referencing `synctv-core` metrics, the RTMP server accepts
//! optional callbacks that are invoked when publishers/viewers connect or disconnect.
//! The caller (e.g., `synctv-livestream`) injects the actual metric updates.

use std::sync::Arc;

/// Callbacks for stream lifecycle events.
///
/// All callbacks are optional. When `None`, the event is simply ignored.
/// Callbacks must be `Send + Sync` because they are shared across async tasks.
pub struct StreamEventCallbacks {
    pub on_publisher_start: Option<Arc<dyn Fn() + Send + Sync>>,
    pub on_publisher_stop: Option<Arc<dyn Fn() + Send + Sync>>,
    pub on_viewer_join: Option<Arc<dyn Fn() + Send + Sync>>,
    pub on_viewer_leave: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl Default for StreamEventCallbacks {
    fn default() -> Self {
        Self {
            on_publisher_start: None,
            on_publisher_stop: None,
            on_viewer_join: None,
            on_viewer_leave: None,
        }
    }
}

impl std::fmt::Debug for StreamEventCallbacks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamEventCallbacks")
            .field("on_publisher_start", &self.on_publisher_start.is_some())
            .field("on_publisher_stop", &self.on_publisher_stop.is_some())
            .field("on_viewer_join", &self.on_viewer_join.is_some())
            .field("on_viewer_leave", &self.on_viewer_leave.is_some())
            .finish()
    }
}
