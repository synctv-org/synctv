// HTTP-FLV streaming primitives.
//
// Production HTTP routing lives in `synctv-api` so FLV playback follows the
// same signed provider proxy, pull-stream lifecycle, and disconnect handling as
// HLS and the other provider endpoints.

// Re-export HttpFlvSession from xiu-httpflv
pub use synctv_xiu::httpflv::HttpFlvSession;

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    #[test]
    fn test_http_flv_session_creation() {
        let (event_sender, _) = tokio::sync::mpsc::channel(64);
        let (response_tx, _response_rx) =
            mpsc::channel(synctv_xiu::httpflv::FLV_RESPONSE_CHANNEL_CAPACITY);

        let session = HttpFlvSession::new(
            "live".to_string(),
            "room123/media456".to_string(),
            event_sender,
            response_tx,
        );

        assert_eq!(session.app_name, "live");
        assert_eq!(session.stream_name, "room123/media456");
        assert!(!session.has_send_header);
        assert!(!session.has_audio);
        assert!(!session.has_video);
    }
}
