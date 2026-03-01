// HTTP-FLV session: subscribes to StreamHub and sends FLV data over a bounded channel
//
// This is a generic, reusable component. The HTTP routing layer
// (which may depend on application-specific state like Redis) lives
// in the downstream crate (e.g., synctv-livestream).

use crate::flv::amf0::amf0_writer::Amf0Writer;
use crate::flv::muxer::{FlvMuxer, HEADER_LENGTH};
use crate::streamhub::{
    define::{
        FrameData, FrameDataReceiver, NotifyInfo, StreamHubEvent, StreamHubEventSender,
        SubDataType, SubscribeType, SubscriberInfo,
    },
    stream::StreamIdentifier,
    utils::Uuid,
};
use bytes::BytesMut;
use tokio::sync::{mpsc, oneshot};
use tracing::{error, info, warn};

/// Capacity for the HTTP response channel (bounded to prevent OOM with slow clients).
/// At ~8KB per FLV tag (typical video frame), 512 entries ≈ 4MB buffer per client.
pub const FLV_RESPONSE_CHANNEL_CAPACITY: usize = 512;

/// HTTP-FLV session (per-client connection)
pub struct HttpFlvSession {
    pub app_name: String,
    pub stream_name: String,
    event_producer: StreamHubEventSender,
    /// Initialized to None; set by `subscribe_from_stream_hub`.
    /// Calling `send_media_stream` without subscribing first is an error.
    data_receiver: Option<FrameDataReceiver>,
    response_producer: mpsc::Sender<Result<bytes::Bytes, std::io::Error>>,
    subscriber_id: Uuid,
    muxer: FlvMuxer,
    pub has_audio: bool,
    pub has_video: bool,
    pub has_send_header: bool,
}

impl HttpFlvSession {
    #[must_use]
    pub fn new(
        app_name: String,
        stream_name: String,
        event_producer: StreamHubEventSender,
        response_producer: mpsc::Sender<Result<bytes::Bytes, std::io::Error>>,
    ) -> Self {
        let subscriber_id = Uuid::new();

        Self {
            app_name,
            stream_name,
            event_producer,
            data_receiver: None,
            response_producer,
            subscriber_id,
            muxer: FlvMuxer::new(),
            has_audio: false,
            has_video: false,
            has_send_header: false,
        }
    }

    pub async fn run(&mut self) -> anyhow::Result<()> {
        // Subscribe to StreamHub
        self.subscribe_from_stream_hub().await?;

        // Send media stream
        self.send_media_stream().await?;

        Ok(())
    }

    async fn send_media_stream(&mut self) -> anyhow::Result<()> {
        let mut data_receiver = self.data_receiver.take().ok_or_else(|| {
            anyhow::anyhow!("send_media_stream called before subscribe_from_stream_hub")
        })?;

        let mut max_av_frame_num_to_guess_av = 0;
        let mut cached_frames = Vec::new();

        // Use a timeout-based approach for stream end detection
        // This is more reliable than counting retries
        const RECV_TIMEOUT_SECS: u64 = 5; // 5 seconds of no data = stream ended

        loop {
            match tokio::time::timeout(
                std::time::Duration::from_secs(RECV_TIMEOUT_SECS),
                data_receiver.recv(),
            )
            .await
            {
                Ok(Some(data)) => {
                    // Detect audio/video before sending header
                    if !self.has_send_header {
                        max_av_frame_num_to_guess_av += 1;

                        match data {
                            FrameData::Audio { .. } => {
                                self.has_audio = true;
                                cached_frames.push(data);
                            }
                            FrameData::Video { .. } => {
                                self.has_video = true;
                                cached_frames.push(data);
                            }
                            FrameData::MetaData { .. } => cached_frames.push(data),
                            _ => {}
                        }

                        // Send header after detecting A/V or after 10 frames
                        if (self.has_audio && self.has_video) || max_av_frame_num_to_guess_av > 10 {
                            self.has_send_header = true;

                            // Write FLV header
                            self.muxer
                                .write_flv_header(self.has_audio, self.has_video)
                                .map_err(|e| {
                                    anyhow::anyhow!("Failed to write FLV header: {e:?}")
                                })?;
                            self.muxer
                                .write_previous_tag_size(0)
                                .map_err(|e| anyhow::anyhow!("Failed to write tag size: {e:?}"))?;
                            self.flush_response_data()?;

                            // Write cached frames
                            for frame in &cached_frames {
                                self.write_flv_tag(frame.clone())?;
                            }
                            cached_frames.clear();
                        }

                        continue;
                    }

                    // Write FLV tag
                    if let Err(e) = self.write_flv_tag(data) {
                        error!("Failed to write FLV tag: {}", e);
                    }
                }
                Ok(None) => {
                    // Channel closed - stream truly ended
                    info!("Stream channel closed");
                    break;
                }
                Err(_timeout) => {
                    // Timeout - no data for 5 seconds, consider stream ended
                    info!("Stream timeout (no data for {}s)", RECV_TIMEOUT_SECS);
                    break;
                }
            }
        }

        self.unsubscribe_from_stream_hub().await?;
        Ok(())
    }

    fn write_flv_tag(&mut self, frame_data: FrameData) -> anyhow::Result<()> {
        let (data, timestamp, tag_type) = match frame_data {
            FrameData::Audio { timestamp, data } => (BytesMut::from(&data[..]), timestamp, 8), // AUDIO
            FrameData::Video { timestamp, data } => (BytesMut::from(&data[..]), timestamp, 9), // VIDEO
            FrameData::MetaData { timestamp, data } => {
                // Remove @setDataFrame from RTMP's metadata
                let mut amf_writer = Amf0Writer::new();
                amf_writer
                    .write_string(&String::from("@setDataFrame"))
                    .map_err(|e| anyhow::anyhow!("Failed to write AMF string: {e:?}"))?;
                let right = &data[amf_writer.len()..];
                (BytesMut::from(right), timestamp, 18) // SCRIPT_DATA_AMF
            }
            _ => return Ok(()),
        };

        let data_len = data.len() as u32;

        self.muxer
            .write_flv_tag_header(tag_type, data_len, timestamp)
            .map_err(|e| anyhow::anyhow!("Failed to write FLV tag header: {e:?}"))?;
        self.muxer
            .write_flv_tag_body(data)
            .map_err(|e| anyhow::anyhow!("Failed to write FLV tag body: {e:?}"))?;
        self.muxer
            .write_previous_tag_size(data_len + HEADER_LENGTH)
            .map_err(|e| anyhow::anyhow!("Failed to write tag size: {e:?}"))?;

        self.flush_response_data()?;

        Ok(())
    }

    fn flush_response_data(&mut self) -> anyhow::Result<()> {
        let data = self.muxer.writer.extract_current_bytes();
        let bytes = bytes::Bytes::from(data.to_vec());

        // Use try_send to apply backpressure: if the client is too slow and the
        // channel is full, drop the frame rather than accumulating unbounded memory.
        match self.response_producer.try_send(Ok(bytes)) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                warn!(stream = %self.stream_name, "FLV response channel full, dropping frame (slow client)");
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                return Err(anyhow::anyhow!("Response channel closed"));
            }
        }

        Ok(())
    }

    async fn subscribe_from_stream_hub(&mut self) -> anyhow::Result<()> {
        let sub_info = SubscriberInfo {
            id: self.subscriber_id,
            sub_type: SubscribeType::RtmpRemux2HttpFlv,
            sub_data_type: SubDataType::Frame,
            notify_info: NotifyInfo {
                request_url: format!("/live/{}.flv", self.stream_name),
                remote_addr: String::new(),
            },
        };

        let identifier = StreamIdentifier::Rtmp {
            app_name: self.app_name.clone(),
            stream_name: self.stream_name.clone(),
        };

        let (event_result_sender, event_result_receiver) = oneshot::channel();

        let subscribe_event = StreamHubEvent::Subscribe {
            identifier,
            info: sub_info,
            result_sender: event_result_sender,
        };

        self.event_producer
            .try_send(subscribe_event)
            .map_err(|_| anyhow::anyhow!("Failed to send subscribe event"))?;

        let result = event_result_receiver
            .await
            .map_err(|e| anyhow::anyhow!("Event result channel error: {e}"))?
            .map_err(|e| anyhow::anyhow!("Subscribe failed: {e:?}"))?;
        self.data_receiver = Some(
            result
                .0
                .frame_receiver
                .ok_or_else(|| anyhow::anyhow!("No frame receiver"))?,
        );

        info!(
            subscriber_id = %self.subscriber_id,
            stream = %self.stream_name,
            "Subscribed to StreamHub"
        );

        Ok(())
    }

    async fn unsubscribe_from_stream_hub(&mut self) -> anyhow::Result<()> {
        let sub_info = SubscriberInfo {
            id: self.subscriber_id,
            sub_type: SubscribeType::RtmpRemux2HttpFlv,
            sub_data_type: SubDataType::Frame,
            notify_info: NotifyInfo {
                request_url: format!("/live/{}.flv", self.stream_name),
                remote_addr: String::new(),
            },
        };

        let identifier = StreamIdentifier::Rtmp {
            app_name: self.app_name.clone(),
            stream_name: self.stream_name.clone(),
        };

        let unsubscribe_event = StreamHubEvent::UnSubscribe {
            identifier,
            info: sub_info,
        };

        if let Err(e) = self.event_producer.try_send(unsubscribe_event) {
            warn!("Failed to send unsubscribe event: {}", e);
        }

        info!(
            subscriber_id = %self.subscriber_id,
            stream = %self.stream_name,
            "Unsubscribed from StreamHub"
        );

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_http_flv_session_creation() {
        let (event_sender, _) = tokio::sync::mpsc::channel(64);
        let (response_tx, _response_rx) = mpsc::channel(FLV_RESPONSE_CHANNEL_CAPACITY);

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

    #[test]
    fn test_flv_session_defaults() {
        let (event_sender, _) = tokio::sync::mpsc::channel(64);
        let (response_tx, _response_rx) = mpsc::channel(FLV_RESPONSE_CHANNEL_CAPACITY);

        let session = HttpFlvSession::new(
            "live".to_string(),
            "test/stream".to_string(),
            event_sender,
            response_tx,
        );

        // Verify default states
        assert!(!session.has_send_header);
        assert!(!session.has_audio);
        assert!(!session.has_video);
    }
}
