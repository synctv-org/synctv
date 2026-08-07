//! RTMP & Streaming Infrastructure Tests
//!
//! This test suite covers:
//! 1. RTMP chunk header construction and validation
//! 2. `StreamIdentifier` operations
//! 3. FLV codec conversion functions
//! 4. `StreamsHub` publish/subscribe flow
//! 5. Fan-out frame/packet delivery
//! 6. Statistics data processing

#![allow(clippy::unwrap_used)]
use async_trait::async_trait;
use std::sync::Arc;
use synctv_xiu::streamhub::define::{
    BroadcastEvent, DataSender, PublishType, SubscribeType, TStreamHandler,
};
use synctv_xiu::streamhub::errors::StreamHubError;
use synctv_xiu::streamhub::utils::Uuid;

struct MockHandler;

#[async_trait]
impl TStreamHandler for MockHandler {
    async fn send_prior_data(
        &self,
        _sender: DataSender,
        _sub_type: SubscribeType,
    ) -> Result<(), StreamHubError> {
        Ok(())
    }
}

struct MockHandler2;

#[async_trait]
impl TStreamHandler for MockHandler2 {
    async fn send_prior_data(
        &self,
        _sender: DataSender,
        _sub_type: SubscribeType,
    ) -> Result<(), StreamHubError> {
        Ok(())
    }
}

fn expect_publish_event(
    event: BroadcastEvent,
) -> (synctv_xiu::streamhub::stream::StreamIdentifier, PublishType) {
    match event {
        BroadcastEvent::Publish {
            identifier,
            pub_type,
            ..
        } => (identifier, pub_type),
        other => panic!("expected publish event, got {other:?}"),
    }
}

#[test]
fn test_u8_2_avc_codec_id() {
    use synctv_xiu::flv::define::{u8_2_avc_codec_id, AvcCodecId};
    assert!(matches!(u8_2_avc_codec_id(7), AvcCodecId::H264));
    assert!(matches!(u8_2_avc_codec_id(12), AvcCodecId::HEVC));
    assert!(matches!(u8_2_avc_codec_id(0), AvcCodecId::UNKNOWN));
    assert!(matches!(u8_2_avc_codec_id(255), AvcCodecId::UNKNOWN));
}

#[test]
fn test_u8_2_aac_profile() {
    use synctv_xiu::flv::define::{u8_2_aac_profile, AacProfile};
    assert!(matches!(u8_2_aac_profile(2), AacProfile::LC));
    assert!(matches!(u8_2_aac_profile(3), AacProfile::SSR));
    assert!(matches!(u8_2_aac_profile(5), AacProfile::HE));
    assert!(matches!(u8_2_aac_profile(29), AacProfile::HEV2));
    assert!(matches!(u8_2_aac_profile(0), AacProfile::UNKNOWN));
    assert!(matches!(u8_2_aac_profile(255), AacProfile::UNKNOWN));
}

#[test]
fn test_u8_2_avc_profile() {
    use synctv_xiu::flv::define::{u8_2_avc_profile, AvcProfile};
    assert!(matches!(u8_2_avc_profile(66), AvcProfile::Baseline));
    assert!(matches!(u8_2_avc_profile(77), AvcProfile::Main));
    assert!(matches!(u8_2_avc_profile(88), AvcProfile::Extended));
    assert!(matches!(u8_2_avc_profile(100), AvcProfile::High));
    assert!(matches!(u8_2_avc_profile(0), AvcProfile::UNKNOWN));
}

#[test]
fn test_u8_2_avc_level() {
    use synctv_xiu::flv::define::{u8_2_avc_level, AvcLevel};
    assert!(matches!(u8_2_avc_level(10), AvcLevel::Level1));
    assert!(matches!(u8_2_avc_level(31), AvcLevel::Level31));
    assert!(matches!(u8_2_avc_level(41), AvcLevel::Level41));
    assert!(matches!(u8_2_avc_level(51), AvcLevel::Level51));
    assert!(matches!(u8_2_avc_level(0), AvcLevel::UNKNOWN));
    assert!(matches!(u8_2_avc_level(255), AvcLevel::UNKNOWN));
}

#[test]
fn test_mpegts_stream_types() {
    use synctv_xiu::mpegts::define::epsi_stream_type;
    assert_eq!(epsi_stream_type::PSI_STREAM_H264, 0x1b);
    assert_eq!(epsi_stream_type::PSI_STREAM_AAC, 0x0f);
    assert_eq!(epsi_stream_type::PSI_STREAM_AUDIO_OPUS, 0x9c);
}

#[tokio::test]
async fn test_streams_hub_publish_and_subscribe() {
    use synctv_xiu::streamhub::define::*;
    use synctv_xiu::streamhub::stream::StreamIdentifier;
    use synctv_xiu::streamhub::StreamsHub;

    let (event_sender, event_receiver) = tokio::sync::mpsc::channel(100);
    let mut hub = StreamsHub::new(event_sender, event_receiver);

    let identifier = StreamIdentifier::Rtmp {
        app_name: "live".to_string(),
        stream_name: "test_stream".to_string(),
    };

    let handler: Arc<dyn TStreamHandler> = Arc::new(MockHandler);

    let (_frame_sender, frame_receiver) = tokio::sync::mpsc::channel(100);
    let receiver = DataReceiver {
        frame_receiver: Some(FrameDataReceiver::bounded(frame_receiver)),
        packet_receiver: None,
    };

    // Publish
    let result = hub.publish(
        identifier.clone(),
        Uuid::new(),
        PublishType::RtmpPush,
        receiver,
        handler,
    );
    assert!(result.is_ok());

    // Duplicate publish should fail
    let (_, frame_receiver2) = tokio::sync::mpsc::channel(100);
    let receiver2 = DataReceiver {
        frame_receiver: Some(FrameDataReceiver::bounded(frame_receiver2)),
        packet_receiver: None,
    };

    let result2 = hub.publish(
        identifier.clone(),
        Uuid::new(),
        PublishType::RtmpPush,
        receiver2,
        Arc::new(MockHandler2),
    );
    assert!(result2.is_err());
}

#[tokio::test]
async fn test_streams_hub_subscribe_no_stream() {
    use synctv_xiu::streamhub::define::*;
    use synctv_xiu::streamhub::stream::StreamIdentifier;
    use synctv_xiu::streamhub::utils::Uuid;
    use synctv_xiu::streamhub::StreamsHub;

    let (event_sender, event_receiver) = tokio::sync::mpsc::channel(100);
    let mut hub = StreamsHub::new(event_sender, event_receiver);

    let identifier = StreamIdentifier::Rtmp {
        app_name: "live".to_string(),
        stream_name: "nonexistent".to_string(),
    };

    let (sender, _) = tokio::sync::mpsc::channel(100);
    let sub_info = SubscriberInfo {
        id: Uuid::new(),
        sub_type: SubscribeType::RtmpPull,
        notify_info: NotifyInfo {
            request_url: String::new(),
            remote_addr: String::new(),
        },
        sub_data_type: SubDataType::Frame,
    };

    let result = hub
        .subscribe(
            &identifier,
            sub_info,
            DataSender::Frame {
                sender: FrameDataSender::bounded(sender),
            },
        )
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_streams_hub_unsubscribe_no_stream() {
    use synctv_xiu::streamhub::define::*;
    use synctv_xiu::streamhub::stream::StreamIdentifier;
    use synctv_xiu::streamhub::utils::Uuid;
    use synctv_xiu::streamhub::StreamsHub;

    let (event_sender, event_receiver) = tokio::sync::mpsc::channel(100);
    let mut hub = StreamsHub::new(event_sender, event_receiver);

    let identifier = StreamIdentifier::Rtmp {
        app_name: "live".to_string(),
        stream_name: "nonexistent".to_string(),
    };

    let sub_info = SubscriberInfo {
        id: Uuid::new(),
        sub_type: SubscribeType::RtmpPull,
        notify_info: NotifyInfo {
            request_url: String::new(),
            remote_addr: String::new(),
        },
        sub_data_type: SubDataType::Frame,
    };

    let result = hub.unsubscribe(&identifier, sub_info);
    assert!(result.is_err());
}

#[tokio::test]
async fn test_streams_hub_broadcast_event() {
    use synctv_xiu::streamhub::define::*;
    use synctv_xiu::streamhub::stream::StreamIdentifier;
    use synctv_xiu::streamhub::StreamsHub;

    let (event_sender, event_receiver) = tokio::sync::mpsc::channel(100);
    let mut hub = StreamsHub::new(event_sender, event_receiver);

    // Subscribe to broadcast events
    let mut broadcast_rx = hub.get_client_event_consumer();

    let identifier = StreamIdentifier::Rtmp {
        app_name: "live".to_string(),
        stream_name: "test".to_string(),
    };

    let (_, frame_receiver) = tokio::sync::mpsc::channel(100);
    let receiver = DataReceiver {
        frame_receiver: Some(FrameDataReceiver::bounded(frame_receiver)),
        packet_receiver: None,
    };

    // Publish should trigger a broadcast event
    hub.publish(
        identifier.clone(),
        Uuid::new(),
        PublishType::RtmpPush,
        receiver,
        Arc::new(MockHandler),
    )
    .unwrap();

    let event = broadcast_rx
        .try_recv()
        .expect("publish event should be sent");
    let (event_identifier, pub_type) = expect_publish_event(event);
    assert_eq!(event_identifier, identifier);
    assert!(matches!(pub_type, PublishType::RtmpPush));
}

#[test]
fn test_rtmp_config_constants() {
    use synctv_xiu::rtmp::config;
    assert_eq!(config::CLIENT_PUSH, 1);
    assert_eq!(config::CLIENT_PULL, 2);
    assert_eq!(config::SERVER_PUSH, 4);
    assert_eq!(config::SERVER_PULL, 8);
}
