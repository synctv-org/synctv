//! Prometheus metrics for `SyncTV`
//!
//! This module exposes the metrics used by the API crate from synctv-core's
//! unified registry.

pub use synctv_core::metrics::http::{
    HTTP_REQUESTS_IN_FLIGHT, HTTP_REQUESTS_TOTAL, HTTP_REQUEST_DURATION_SECONDS,
};

pub use synctv_core::metrics::livestream::LIVESTREAM_FLV_SLOW_CLIENT_TERMINATIONS_TOTAL;

pub use synctv_core::metrics::gather_metrics;

/// Normalize an HTTP request path for metric labels.
///
/// Replaces route parameters and dynamic IDs with placeholders to avoid
/// high-cardinality labels.
#[must_use]
pub fn normalize_path(path: &str) -> String {
    let segments: Vec<&str> = path.split('/').collect();
    let mut result = Vec::with_capacity(segments.len());

    for (i, segment) in segments.iter().enumerate() {
        if segment.is_empty() {
            result.push(*segment);
            continue;
        }

        let prev = if i > 0 { segments.get(i - 1) } else { None };
        let is_id = matches!(
            prev,
            Some(
                &"rooms"
                    | &"media"
                    | &"chat"
                    | &"playlists"
                    | &"users"
                    | &"notifications"
                    | &"settings"
                    | &"members"
            )
        );

        if is_id || is_dynamic_segment(segment) {
            result.push(":id");
        } else {
            result.push(segment);
        }
    }

    result.join("/")
}

fn is_dynamic_segment(segment: &str) -> bool {
    if segment.len() == 36 && segment.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        let parts: Vec<&str> = segment.split('-').collect();
        if parts.len() == 5
            && parts[0].len() == 8
            && parts[1].len() == 4
            && parts[2].len() == 4
            && parts[3].len() == 4
            && parts[4].len() == 12
        {
            return true;
        }
    }

    if segment.chars().all(|c| c.is_ascii_digit()) && !segment.is_empty() {
        return true;
    }

    if segment.len() == 32 && segment.chars().all(|c| c.is_ascii_hexdigit()) {
        return true;
    }

    false
}

#[cfg(test)]
mod tests {
    use super::normalize_path;

    #[test]
    fn normalize_path_existing_resources() {
        assert_eq!(
            normalize_path("/api/rooms/abc123/media"),
            "/api/rooms/:id/media"
        );
        assert_eq!(normalize_path("/api/media/xyz789"), "/api/media/:id");
        assert_eq!(normalize_path("/api/chat/msg001"), "/api/chat/:id");
        assert_eq!(normalize_path("/api/playlists/pl123"), "/api/playlists/:id");
    }

    #[test]
    fn normalize_path_extended_resources() {
        assert_eq!(normalize_path("/api/users/u123"), "/api/users/:id");
        assert_eq!(
            normalize_path("/api/notifications/n456"),
            "/api/notifications/:id"
        );
        assert_eq!(normalize_path("/api/settings/s789"), "/api/settings/:id");
        assert_eq!(normalize_path("/api/members/m012"), "/api/members/:id");
    }

    #[test]
    fn normalize_path_without_id_segments() {
        assert_eq!(normalize_path("/api/rooms"), "/api/rooms");
        assert_eq!(normalize_path("/api/health"), "/api/health");
        assert_eq!(normalize_path("/metrics"), "/metrics");
    }
}
