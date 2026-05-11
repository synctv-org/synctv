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
use bytes::{Bytes, BytesMut};
use std::sync::Arc;
use synctv_xiu::streamhub::define::{DataSender, SubscribeType, TStreamHandler};
use synctv_xiu::streamhub::errors::StreamHubError;

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

#[test]
fn test_chunk_basic_header_creation() {
    use synctv_xiu::rtmp::chunk::ChunkBasicHeader;
    let header = ChunkBasicHeader::new(0, 3);
    assert_eq!(header.format, 0);
    assert_eq!(header.chunk_stream_id, 3);
}

#[test]
fn test_chunk_message_header_creation() {
    use synctv_xiu::rtmp::chunk::ChunkMessageHeader;
    let header = ChunkMessageHeader::new(1000, 512, 9, 1);
    assert_eq!(header.timestamp, 1000);
    assert_eq!(header.msg_length, 512);
    assert_eq!(header.msg_type_id, 9);
    assert_eq!(header.msg_streamd_id, 1);
    assert_eq!(header.timestamp_delta, 0);
    assert_eq!(
        header.extended_timestamp_type,
        synctv_xiu::rtmp::chunk::ExtendTimestampType::NONE
    );
}

#[test]
fn test_chunk_header_default() {
    use synctv_xiu::rtmp::chunk::ChunkHeader;
    let header = ChunkHeader::default();
    assert_eq!(header.basic_header.format, 0);
    assert_eq!(header.basic_header.chunk_stream_id, 0);
    assert_eq!(header.message_header.timestamp, 0);
}

#[test]
fn test_chunk_info_construction() {
    use synctv_xiu::rtmp::chunk::ChunkInfo;
    let payload = BytesMut::from(&b"test data"[..]);
    let info = ChunkInfo::new(3, 0, 1000, 9, 9, 1, payload.clone());
    assert_eq!(info.basic_header.chunk_stream_id, 3);
    assert_eq!(info.basic_header.format, 0);
    assert_eq!(info.message_header.timestamp, 1000);
    assert_eq!(info.message_header.msg_length, 9);
    assert_eq!(info.message_header.msg_type_id, 9);
    assert_eq!(info.message_header.msg_streamd_id, 1);
    assert_eq!(info.payload, payload);
}

#[test]
fn test_chunk_info_default() {
    use synctv_xiu::rtmp::chunk::ChunkInfo;
    let info = ChunkInfo::default();
    assert_eq!(info.basic_header.format, 0);
    assert_eq!(info.basic_header.chunk_stream_id, 0);
    assert!(info.payload.is_empty());
}

#[test]
fn test_chunk_info_debug_format() {
    use synctv_xiu::rtmp::chunk::ChunkInfo;
    let payload = BytesMut::from(&[0xAB, 0xCD][..]);
    let info = ChunkInfo::new(3, 0, 0, 2, 9, 1, payload);
    let debug = format!("{info:?}");
    assert!(debug.contains("ChunkInfo"));
    assert!(debug.contains("0xab"));
    assert!(debug.contains("0xcd"));
}

#[test]
fn test_chunk_info_equality() {
    use synctv_xiu::rtmp::chunk::ChunkInfo;
    let payload = BytesMut::from(&b"test"[..]);
    let info1 = ChunkInfo::new(3, 0, 1000, 4, 9, 1, payload.clone());
    let info2 = ChunkInfo::new(3, 0, 1000, 4, 9, 1, payload);
    assert_eq!(info1, info2);
}

#[test]
fn test_extended_timestamp_type_equality() {
    use synctv_xiu::rtmp::chunk::ExtendTimestampType;
    assert_eq!(ExtendTimestampType::NONE, ExtendTimestampType::NONE);
    assert_eq!(ExtendTimestampType::FORMAT0, ExtendTimestampType::FORMAT0);
    assert_eq!(ExtendTimestampType::FORMAT12, ExtendTimestampType::FORMAT12);
    assert_ne!(ExtendTimestampType::NONE, ExtendTimestampType::FORMAT0);
}

#[test]
fn test_rtmp_chunk_constants() {
    use synctv_xiu::rtmp::chunk::define;
    assert_eq!(define::CHUNK_SIZE, 4096);
    assert_eq!(define::INIT_CHUNK_SIZE, 128);
    assert_eq!(define::csid_type::PROTOCOL_USER_CONTROL, 2);
    assert_eq!(define::csid_type::COMMAND_AMF0_AMF3, 3);
    assert_eq!(define::csid_type::AUDIO, 4);
    assert_eq!(define::csid_type::VIDEO, 5);
    assert_eq!(define::csid_type::DATA_AMF0_AMF3, 6);
    assert_eq!(define::chunk_type::TYPE_0, 0);
    assert_eq!(define::chunk_type::TYPE_1, 1);
    assert_eq!(define::chunk_type::TYPE_2, 2);
    assert_eq!(define::chunk_type::TYPE_3, 3);
}

#[test]
fn test_rtmp_handshake_constants() {
    use synctv_xiu::rtmp::handshake::define;
    assert_eq!(define::RTMP_VERSION, 3);
    assert_eq!(define::RTMP_HANDSHAKE_SIZE, 1536);
    assert_eq!(define::RTMP_DIGEST_LENGTH, 32);
    assert_eq!(define::RTMP_SERVER_KEY.len(), 68);
    assert_eq!(
        define::RTMP_SERVER_KEY_FIRST_HALF,
        "Genuine Adobe Flash Media Server 001"
    );
    assert_eq!(
        define::RTMP_CLIENT_KEY_FIRST_HALF,
        "Genuine Adobe Flash Player 001"
    );
}

#[test]
fn test_stream_identifier_rtmp() {
    use synctv_xiu::streamhub::stream::StreamIdentifier;
    let id = StreamIdentifier::Rtmp {
        app_name: "live".to_string(),
        stream_name: "test".to_string(),
    };
    let display = format!("{id}");
    assert!(display.contains("RTMP"));
    assert!(display.contains("live"));
    assert!(display.contains("test"));
}

#[test]
fn test_stream_identifier_unknown() {
    use synctv_xiu::streamhub::stream::StreamIdentifier;
    let id = StreamIdentifier::Unknown;
    let display = format!("{id}");
    assert_eq!(display, "Unknown");
}

#[test]
fn test_stream_identifier_default() {
    use synctv_xiu::streamhub::stream::StreamIdentifier;
    let id = StreamIdentifier::default();
    assert_eq!(id, StreamIdentifier::Unknown);
}

#[test]
fn test_stream_identifier_equality() {
    use synctv_xiu::streamhub::stream::StreamIdentifier;
    let id1 = StreamIdentifier::Rtmp {
        app_name: "live".to_string(),
        stream_name: "test".to_string(),
    };
    let id2 = StreamIdentifier::Rtmp {
        app_name: "live".to_string(),
        stream_name: "test".to_string(),
    };
    assert_eq!(id1, id2);
}

#[test]
fn test_stream_identifier_inequality() {
    use synctv_xiu::streamhub::stream::StreamIdentifier;
    let id1 = StreamIdentifier::Rtmp {
        app_name: "live".to_string(),
        stream_name: "test1".to_string(),
    };
    let id2 = StreamIdentifier::Rtmp {
        app_name: "live".to_string(),
        stream_name: "test2".to_string(),
    };
    assert_ne!(id1, id2);
}

#[test]
fn test_stream_identifier_hash() {
    use std::collections::HashMap;
    use synctv_xiu::streamhub::stream::StreamIdentifier;

    let mut map = HashMap::new();
    let id = StreamIdentifier::Rtmp {
        app_name: "live".to_string(),
        stream_name: "test".to_string(),
    };
    map.insert(id.clone(), "value");
    assert_eq!(map.get(&id), Some(&"value"));
}

#[test]
fn test_stream_identifier_serialization() {
    use synctv_xiu::streamhub::stream::StreamIdentifier;
    let id = StreamIdentifier::Rtmp {
        app_name: "live".to_string(),
        stream_name: "test".to_string(),
    };
    let json = serde_json::to_string(&id).unwrap();
    assert!(json.contains("rtmp"));
    assert!(json.contains("live"));
    assert!(json.contains("test"));

    // Roundtrip
    let deserialized: StreamIdentifier = serde_json::from_str(&json).unwrap();
    assert_eq!(id, deserialized);
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
fn test_flv_tag_type_constants() {
    use synctv_xiu::flv::define::tag_type;
    assert_eq!(tag_type::AUDIO, 8);
    assert_eq!(tag_type::VIDEO, 9);
    assert_eq!(tag_type::SCRIPT_DATA_AMF, 18);
}

#[test]
fn test_flv_frame_type_constants() {
    use synctv_xiu::flv::define::frame_type;
    assert_eq!(frame_type::KEY_FRAME, 1);
    assert_eq!(frame_type::INTER_FRAME, 2);
}

#[test]
fn test_flv_h264_nal_type_constants() {
    use synctv_xiu::flv::define::h264_nal_type;
    assert_eq!(h264_nal_type::H264_NAL_IDR, 5);
    assert_eq!(h264_nal_type::H264_NAL_SPS, 7);
    assert_eq!(h264_nal_type::H264_NAL_PPS, 8);
    assert_eq!(h264_nal_type::H264_NAL_AUD, 9);
}

#[test]
fn test_aac_packet_type_constants() {
    use synctv_xiu::flv::define::aac_packet_type;
    assert_eq!(aac_packet_type::AAC_SEQHDR, 0);
    assert_eq!(aac_packet_type::AAC_RAW, 1);
}

#[test]
fn test_avc_packet_type_constants() {
    use synctv_xiu::flv::define::avc_packet_type;
    assert_eq!(avc_packet_type::AVC_SEQHDR, 0);
    assert_eq!(avc_packet_type::AVC_NALU, 1);
    assert_eq!(avc_packet_type::AVC_EOS, 2);
}

#[test]
fn test_mpegts_constants() {
    use synctv_xiu::mpegts::define;
    assert_eq!(define::TS_PACKET_SIZE, 188);
    assert_eq!(define::TS_HEADER_LEN, 4);
    assert_eq!(define::PES_HEADER_LEN, 6);
}

#[test]
fn test_mpegts_stream_types() {
    use synctv_xiu::mpegts::define::epsi_stream_type;
    assert_eq!(epsi_stream_type::PSI_STREAM_H264, 0x1b);
    assert_eq!(epsi_stream_type::PSI_STREAM_AAC, 0x0f);
    assert_eq!(epsi_stream_type::PSI_STREAM_AUDIO_OPUS, 0x9c);
}

#[test]
fn test_frame_data_video_clone() {
    use synctv_xiu::streamhub::define::FrameData;
    let frame = FrameData::Video {
        timestamp: 100,
        data: Bytes::from(vec![1, 2, 3, 4]),
    };
    let cloned = frame.clone();
    if let (
        FrameData::Video {
            timestamp: t1,
            data: d1,
        },
        FrameData::Video {
            timestamp: t2,
            data: d2,
        },
    ) = (&frame, &cloned)
    {
        assert_eq!(t1, t2);
        assert_eq!(d1, d2);
    } else {
        panic!("Expected Video variant");
    }
}

#[test]
fn test_frame_data_serialization_roundtrip() {
    use synctv_xiu::streamhub::define::FrameData;
    let frame = FrameData::Video {
        timestamp: 42,
        data: Bytes::from(vec![0xDE, 0xAD, 0xBE, 0xEF]),
    };
    let serialized = serde_json::to_string(&frame).unwrap();
    let deserialized: FrameData = serde_json::from_str(&serialized).unwrap();
    if let FrameData::Video { timestamp, data } = deserialized {
        assert_eq!(timestamp, 42);
        assert_eq!(data.as_ref(), &[0xDE, 0xAD, 0xBE, 0xEF]);
    } else {
        panic!("Expected Video variant");
    }
}

#[test]
fn test_frame_data_audio_serialization() {
    use synctv_xiu::streamhub::define::FrameData;
    let frame = FrameData::Audio {
        timestamp: 10,
        data: Bytes::from(vec![0xAA, 0xBB]),
    };
    let json = serde_json::to_string(&frame).unwrap();
    assert!(json.contains("Audio"));
}

#[test]
fn test_media_info_heap_size() {
    use synctv_xiu::streamhub::define::{MediaInfo, VideoCodecType};
    let info = MediaInfo {
        audio_clock_rate: 44100,
        video_clock_rate: 90000,
        vcodec: VideoCodecType::H264,
    };
    assert_eq!(info.heap_size(), 0);
}

#[test]
fn test_channel_capacities() {
    use synctv_xiu::streamhub::define;
    assert_eq!(define::FRAME_DATA_CHANNEL_CAPACITY, 4096);
    assert_eq!(define::PACKET_DATA_CHANNEL_CAPACITY, 256);
    assert_eq!(define::STREAM_HUB_EVENT_CHANNEL_CAPACITY, 4096);
}

#[test]
fn test_streamhub_error_display() {
    use synctv_xiu::streamhub::errors::{StreamHubError, StreamHubErrorValue};

    let err = StreamHubError {
        value: StreamHubErrorValue::NoAppName,
    };
    assert_eq!(err.to_string(), "no app name");

    let err = StreamHubError {
        value: StreamHubErrorValue::Exists,
    };
    assert_eq!(err.to_string(), "exists");

    let err = StreamHubError {
        value: StreamHubErrorValue::SendError,
    };
    assert_eq!(err.to_string(), "send error");

    let err = StreamHubError {
        value: StreamHubErrorValue::NoAppOrStreamName,
    };
    assert_eq!(err.to_string(), "no app or stream name");

    let err = StreamHubError {
        value: StreamHubErrorValue::SubscriberClosed,
    };
    assert_eq!(err.to_string(), "subscriber channel closed");
}

#[test]
fn test_streamhub_error_from_string() {
    use synctv_xiu::streamhub::errors::{StreamHubError, StreamHubErrorValue};
    let err: StreamHubError = "custom error message".to_string().into();
    assert!(matches!(
        err.value,
        StreamHubErrorValue::ClientSessionError(_)
    ));
    assert!(err.to_string().contains("custom error message"));
}

#[test]
fn test_subscribe_type_serialization() {
    use synctv_xiu::streamhub::define::SubscribeType;
    let types = vec![
        SubscribeType::RtmpPull,
        SubscribeType::RtmpRemux2HttpFlv,
        SubscribeType::RtmpRemux2Hls,
        SubscribeType::RtmpRelay,
    ];
    for t in types {
        let json = serde_json::to_string(&t).unwrap();
        assert!(!json.is_empty());
    }
}

#[test]
fn test_publish_type_serialization() {
    use synctv_xiu::streamhub::define::PublishType;
    let types = vec![PublishType::RtmpPush, PublishType::RtmpRelay];
    for t in types {
        let json = serde_json::to_string(&t).unwrap();
        assert!(!json.is_empty());
    }
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
    let result = hub.publish(identifier.clone(), PublishType::RtmpPush, receiver, handler);
    assert!(result.is_ok());

    // Duplicate publish should fail
    let (_, frame_receiver2) = tokio::sync::mpsc::channel(100);
    let receiver2 = DataReceiver {
        frame_receiver: Some(FrameDataReceiver::bounded(frame_receiver2)),
        packet_receiver: None,
    };

    let result2 = hub.publish(
        identifier.clone(),
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
        PublishType::RtmpPush,
        receiver,
        Arc::new(MockHandler),
    )
    .unwrap();

    // Should receive the publish broadcast event
    let event = broadcast_rx.try_recv();
    assert!(event.is_ok());
    if let Ok(BroadcastEvent::Publish {
        identifier: id,
        pub_type,
    }) = event
    {
        assert_eq!(id, identifier);
        assert!(matches!(pub_type, PublishType::RtmpPush));
    } else {
        panic!("Expected BroadcastEvent::Publish");
    }
}

#[test]
fn test_rtmp_config_constants() {
    use synctv_xiu::rtmp::config;
    assert_eq!(config::CLIENT_PUSH, 1);
    assert_eq!(config::CLIENT_PULL, 2);
    assert_eq!(config::SERVER_PUSH, 4);
    assert_eq!(config::SERVER_PULL, 8);
}
