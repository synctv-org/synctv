//! Pre-decode inbound frame-length budget for the HTTP/3 listener.
//!
//! h3 0.0.8 assembles a non-DATA frame to its attacker-declared length
//! before any size check (`max_field_section_size` is applied post-assembly
//! and the listener cannot rely on it for pre-decode buffering), so a peer
//! declares a multi-MiB HEADERS frame and the server buffers it byte by
//! byte. The adapter here follows frame boundaries on every accepted stream
//! and inspects each frame header as it starts - frame type varint, and for
//! length-prefixed frames the length varint - and rejects with
//! `H3_EXCESSIVE_LOAD` before the payload can be buffered. DATA frames pass
//! through (bodies are capped by `H3RecvBody::REQUEST_BODY_LIMIT`
//! downstream).
//!
//! The first frame alone is not enough: a peer may follow a small HEADERS
//! frame with an oversized one, and a unidirectional stream starts with its
//! stream type, which would pass as a DATA frame type and leave the control
//! stream's SETTINGS unchecked. WebTransport data carries no frames after
//! its session id, and QPACK, WebTransport and unknown unidirectional
//! streams carry none at all, so the bytes after those points pass unparsed.

use bytes::{Buf, Bytes};
use h3::proto::stream::StreamId;
use h3::quic::WriteBuf;
use h3::quic::{self, ConnectionErrorIncoming, StreamErrorIncoming};
use h3_datagram::quic_traits::DatagramConnectionExt;

mod header;
pub use header::MAX_CONTROL_FRAME_LEN;
use header::{MAX_FRAME_HEADER, Verdict, verdict};

/// h3 `H3_EXCESSIVE_LOAD` error code (h3 `error/codes.rs`).
const H3_EXCESSIVE_LOAD: u64 = 0x107;

/// Longest encoding of a QUIC variable-length integer.
const VARINT_MAX_SIZE: usize = 8;

/// Decodes the QUIC variable-length integer that starts at `start`.
/// `None` when the encoding does not fit in `buf`.
fn varint(buf: &[u8], start: usize) -> Option<(u64, usize)> {
    let first = *buf.get(start)?;
    let width = 1_usize << (first >> 6);
    let bytes = buf.get(start..start + width)?;
    let value = bytes[1..]
        .iter()
        .fold(u64::from(first & 63), |value, byte| {
            (value << 8) | u64::from(*byte)
        });
    Some((value, width))
}

#[derive(Debug)]
enum State {
    /// A unidirectional stream starts with its stream type, which is not a frame.
    StreamType,
    /// The bytes at the forwarding position start a frame header.
    Header,
    /// The bytes at the forwarding position are the payload of an admitted frame.
    Payload(u64),
    /// Nothing from here on is framed.
    Raw,
}

#[derive(Debug)]
enum Payload {
    /// The bytes that follow are not framed.
    Raw,
    /// Payload bytes that follow the header of a frame within the budget.
    Framed(u64),
}

#[derive(Debug)]
enum Step {
    /// The header is incomplete.
    NeedMore,
    /// A header of `header_len` bytes was admitted.
    Forward { header_len: usize, payload: Payload },
}

/// Frame accounting for one receiving direction.
#[derive(Debug)]
struct Budget {
    state: State,
    /// Start of the unit at the forwarding position that is not a complete
    /// header yet. It always holds the bytes right before that position.
    header: Vec<u8>,
}

impl Budget {
    fn between_frames() -> Self {
        Self {
            state: State::Header,
            header: Vec::new(),
        }
    }

    fn at_stream_type() -> Self {
        Self {
            state: State::StreamType,
            header: Vec::new(),
        }
    }

    fn unframed() -> Self {
        Self {
            state: State::Raw,
            header: Vec::new(),
        }
    }

    /// Admits every frame header that starts in `chunk`. The caller forwards
    /// all of `chunk`, so headers are matched against the bytes preceding the
    /// forwarding position and payloads are only counted.
    fn admit(&mut self, chunk: &[u8]) -> Result<(), String> {
        let mut pos = 0;
        loop {
            match self.state {
                State::Raw => return Ok(()),
                State::Payload(ref mut remaining) => {
                    let ahead = (chunk.len() - pos) as u64;
                    if *remaining > ahead {
                        *remaining -= ahead;
                        return Ok(());
                    }
                    pos += usize::try_from(*remaining).unwrap_or(chunk.len());
                    self.state = State::Header;
                }
                State::StreamType | State::Header => {
                    let limit = match self.state {
                        State::StreamType => VARINT_MAX_SIZE,
                        _ => MAX_FRAME_HEADER,
                    };
                    let take = (limit - self.header.len()).min(chunk.len() - pos);
                    self.header.extend_from_slice(&chunk[pos..pos + take]);
                    pos += take;
                    let Step::Forward {
                        header_len,
                        payload,
                    } = self.inspect()?
                    else {
                        // The rest of the chunk belongs to the header.
                        return Ok(());
                    };
                    // Header bytes lie right before the forwarding position,
                    // whether this or an earlier chunk supplied them.
                    let rest = self.header.len() - header_len;
                    self.header.drain(..header_len);
                    match payload {
                        Payload::Raw => {
                            if matches!(self.state, State::Raw) {
                                self.header.clear();
                            }
                        }
                        Payload::Framed(length) => {
                            if rest as u64 >= length {
                                // The payload ended among the forwarded bytes;
                                // what follows starts the next frame header.
                                self.header.drain(..usize::try_from(length).unwrap_or(rest));
                            } else {
                                self.header.clear();
                                self.state = State::Payload(length - rest as u64);
                            }
                        }
                    }
                }
            }
        }
    }

    fn inspect(&mut self) -> Result<Step, String> {
        match self.state {
            State::StreamType => match varint(&self.header, 0) {
                // Control and push streams carry frames. QPACK, WebTransport
                // and unknown streams carry bare bytes.
                Some((stream_type, header_len)) => {
                    self.state = if stream_type <= 0x01 {
                        State::Header
                    } else {
                        State::Raw
                    };
                    Ok(Step::Forward {
                        header_len,
                        payload: Payload::Raw,
                    })
                }
                None if self.header.len() >= VARINT_MAX_SIZE => Err("malformed stream type".into()),
                None => Ok(Step::NeedMore),
            },
            State::Header => {
                let Some((frame_type, type_len)) = varint(&self.header, 0) else {
                    return Ok(Step::NeedMore);
                };
                if frame_type >= 0x40 {
                    // WebTransport data: nothing after the session id is framed.
                    self.state = State::Raw;
                    return Ok(Step::Forward {
                        header_len: type_len,
                        payload: Payload::Raw,
                    });
                }
                let Some((length, length_len)) = varint(&self.header, type_len) else {
                    return if self.header.len() >= MAX_FRAME_HEADER {
                        Err("malformed frame header".into())
                    } else {
                        Ok(Step::NeedMore)
                    };
                };
                match verdict(&self.header) {
                    Verdict::Ok => Ok(Step::Forward {
                        header_len: type_len + length_len,
                        payload: Payload::Framed(length),
                    }),
                    Verdict::TooBig(length) => Err(format!(
                        "frame length {length} exceeds {MAX_CONTROL_FRAME_LEN}"
                    )),
                    // Both varints are complete, so the verdict is never `NeedMore` here.
                    Verdict::NeedMore => Err("malformed frame header".into()),
                }
            }
            State::Payload(_) | State::Raw => Err("payload is not inspected".into()),
        }
    }
}

/// Receives stream bytes while refusing to buffer a non-DATA frame whose
/// declared length exceeds [`MAX_CONTROL_FRAME_LEN`]. Wraps any h3 quic
/// receive half whose buffer type is `Bytes`.
pub struct BudgetedRecvStream<S> {
    inner: S,
    budget: Budget,
}

impl<S> BudgetedRecvStream<S> {
    /// Wraps an accepted unidirectional stream.
    fn new(inner: S) -> Self {
        Self {
            inner,
            budget: Budget::at_stream_type(),
        }
    }
}

fn poll_budgeted<S: quic::RecvStream<Buf = Bytes>>(
    inner: &mut S,
    budget: &mut Budget,
    cx: &mut std::task::Context<'_>,
) -> std::task::Poll<Result<Option<Bytes>, StreamErrorIncoming>> {
    let chunk = std::task::ready!(inner.poll_data(cx))?;
    if let Some(chunk) = chunk.as_ref()
        && let Err(reason) = budget.admit(chunk)
    {
        inner.stop_sending(H3_EXCESSIVE_LOAD);
        return std::task::Poll::Ready(Err(StreamErrorIncoming::Unknown(reason.into())));
    }
    std::task::Poll::Ready(Ok(chunk))
}

impl<S: quic::RecvStream<Buf = Bytes>> quic::RecvStream for BudgetedRecvStream<S> {
    type Buf = Bytes;

    fn poll_data(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<Option<Self::Buf>, StreamErrorIncoming>> {
        poll_budgeted(&mut self.inner, &mut self.budget, cx)
    }

    fn stop_sending(&mut self, error_code: u64) {
        self.inner.stop_sending(error_code);
    }

    fn recv_id(&self) -> StreamId {
        self.inner.recv_id()
    }
}

/// Transparent send-half wrapper so `split()` halves keep working.
pub struct BudgetedSendStream<S> {
    inner: S,
}

impl<S> BudgetedSendStream<S> {
    fn new(inner: S) -> Self {
        Self { inner }
    }
}

impl<B: Buf, S: quic::SendStream<B>> quic::SendStream<B> for BudgetedSendStream<S> {
    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), StreamErrorIncoming>> {
        self.inner.poll_ready(cx)
    }

    fn send_data<T: Into<WriteBuf<B>>>(&mut self, data: T) -> Result<(), StreamErrorIncoming> {
        self.inner.send_data(data)
    }

    fn poll_finish(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), StreamErrorIncoming>> {
        self.inner.poll_finish(cx)
    }

    fn reset(&mut self, reset_code: u64) {
        self.inner.reset(reset_code);
    }

    fn send_id(&self) -> StreamId {
        self.inner.send_id()
    }
}

impl<B: Buf, S: quic::SendStreamUnframed<B>> quic::SendStreamUnframed<B> for BudgetedSendStream<S> {
    fn poll_send<D: Buf>(
        &mut self,
        cx: &mut std::task::Context<'_>,
        buf: &mut D,
    ) -> std::task::Poll<Result<usize, StreamErrorIncoming>> {
        self.inner.poll_send(cx, buf)
    }
}

/// Bidirectional stream whose receive side is frame-budget checked.
pub struct BudgetedBidiStream<B: Buf> {
    inner: h3_quinn::BidiStream<B>,
    budget: Budget,
}

impl<B: Buf> BudgetedBidiStream<B> {
    /// Wraps a stream opened by the peer. A request starts with a frame.
    fn accepted(inner: h3_quinn::BidiStream<B>) -> Self {
        Self {
            inner,
            budget: Budget::between_frames(),
        }
    }

    /// Wraps a stream opened locally. The peer sends no frames on it.
    fn opened(inner: h3_quinn::BidiStream<B>) -> Self {
        Self {
            inner,
            budget: Budget::unframed(),
        }
    }
}

impl<B: Buf> quic::BidiStream<B> for BudgetedBidiStream<B> {
    type SendStream = BudgetedSendStream<h3_quinn::SendStream<B>>;
    type RecvStream = BudgetedRecvStream<h3_quinn::RecvStream>;

    fn split(self) -> (Self::SendStream, Self::RecvStream) {
        let (send, recv) = quic::BidiStream::<B>::split(self.inner);
        (
            BudgetedSendStream::new(send),
            BudgetedRecvStream {
                inner: recv,
                budget: self.budget,
            },
        )
    }
}

impl<B: Buf> quic::SendStream<B> for BudgetedBidiStream<B> {
    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), StreamErrorIncoming>> {
        self.inner.poll_ready(cx)
    }

    fn send_data<T: Into<WriteBuf<B>>>(&mut self, data: T) -> Result<(), StreamErrorIncoming> {
        self.inner.send_data(data)
    }

    fn poll_finish(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), StreamErrorIncoming>> {
        self.inner.poll_finish(cx)
    }

    fn reset(&mut self, reset_code: u64) {
        self.inner.reset(reset_code);
    }

    fn send_id(&self) -> StreamId {
        self.inner.send_id()
    }
}

impl<B: Buf> quic::SendStreamUnframed<B> for BudgetedBidiStream<B> {
    fn poll_send<D: Buf>(
        &mut self,
        cx: &mut std::task::Context<'_>,
        buf: &mut D,
    ) -> std::task::Poll<Result<usize, StreamErrorIncoming>> {
        self.inner.poll_send(cx, buf)
    }
}

impl<B: Buf> quic::RecvStream for BudgetedBidiStream<B> {
    type Buf = Bytes;

    fn poll_data(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<Option<Self::Buf>, StreamErrorIncoming>> {
        poll_budgeted(&mut self.inner, &mut self.budget, cx)
    }

    fn stop_sending(&mut self, error_code: u64) {
        self.inner.stop_sending(error_code);
    }

    fn recv_id(&self) -> StreamId {
        self.inner.recv_id()
    }
}

/// `h3::quic::Connection` around [`h3_quinn::Connection`] whose every
/// accepted incoming stream (bidirectional and unidirectional - the control
/// stream's SETTINGS frame arrives on a unidirectional stream) is wrapped
/// with the pre-decode frame budget.
pub struct BudgetedConnection {
    inner: h3_quinn::Connection,
}

impl BudgetedConnection {
    pub fn new(inner: h3_quinn::Connection) -> Self {
        Self { inner }
    }
}

impl quic::OpenStreams<Bytes> for BudgetedConnection {
    type BidiStream = BudgetedBidiStream<Bytes>;
    type SendStream = BudgetedSendStream<h3_quinn::SendStream<Bytes>>;

    fn poll_open_bidi(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<Self::BidiStream, StreamErrorIncoming>> {
        let stream = std::task::ready!(quic::OpenStreams::<Bytes>::poll_open_bidi(
            &mut self.inner,
            cx
        ))?;
        std::task::Poll::Ready(Ok(BudgetedBidiStream::opened(stream)))
    }

    fn poll_open_send(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<Self::SendStream, StreamErrorIncoming>> {
        let stream = std::task::ready!(quic::OpenStreams::<Bytes>::poll_open_send(
            &mut self.inner,
            cx
        ))?;
        std::task::Poll::Ready(Ok(BudgetedSendStream::new(stream)))
    }

    fn close(&mut self, code: h3::error::Code, reason: &[u8]) {
        quic::OpenStreams::<Bytes>::close(&mut self.inner, code, reason);
    }
}

impl quic::Connection<Bytes> for BudgetedConnection {
    type RecvStream = BudgetedRecvStream<h3_quinn::RecvStream>;
    type OpenStreams = BudgetedOpenStreams;

    fn poll_accept_recv(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<Self::RecvStream, ConnectionErrorIncoming>> {
        let stream = std::task::ready!(quic::Connection::<Bytes>::poll_accept_recv(
            &mut self.inner,
            cx
        ))?;
        std::task::Poll::Ready(Ok(BudgetedRecvStream::new(stream)))
    }

    fn poll_accept_bidi(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<Self::BidiStream, ConnectionErrorIncoming>> {
        let stream = std::task::ready!(quic::Connection::<Bytes>::poll_accept_bidi(
            &mut self.inner,
            cx
        ))?;
        std::task::Poll::Ready(Ok(BudgetedBidiStream::accepted(stream)))
    }

    fn opener(&self) -> Self::OpenStreams {
        BudgetedOpenStreams {
            inner: quic::Connection::<Bytes>::opener(&self.inner),
        }
    }
}

impl DatagramConnectionExt<Bytes> for BudgetedConnection {
    type SendDatagramHandler =
        <h3_quinn::Connection as DatagramConnectionExt<Bytes>>::SendDatagramHandler;
    type RecvDatagramHandler =
        <h3_quinn::Connection as DatagramConnectionExt<Bytes>>::RecvDatagramHandler;

    fn send_datagram_handler(&self) -> Self::SendDatagramHandler {
        <h3_quinn::Connection as DatagramConnectionExt<Bytes>>::send_datagram_handler(&self.inner)
    }

    fn recv_datagram_handler(&self) -> Self::RecvDatagramHandler {
        <h3_quinn::Connection as DatagramConnectionExt<Bytes>>::recv_datagram_handler(&self.inner)
    }
}

/// Opener returned by [`BudgetedConnection::opener`].
pub struct BudgetedOpenStreams {
    inner: h3_quinn::OpenStreams,
}

impl quic::OpenStreams<Bytes> for BudgetedOpenStreams {
    type BidiStream = BudgetedBidiStream<Bytes>;
    type SendStream = BudgetedSendStream<h3_quinn::SendStream<Bytes>>;

    fn poll_open_bidi(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<Self::BidiStream, StreamErrorIncoming>> {
        let stream = std::task::ready!(quic::OpenStreams::<Bytes>::poll_open_bidi(
            &mut self.inner,
            cx
        ))?;
        std::task::Poll::Ready(Ok(BudgetedBidiStream::opened(stream)))
    }

    fn poll_open_send(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<Self::SendStream, StreamErrorIncoming>> {
        let stream = std::task::ready!(quic::OpenStreams::<Bytes>::poll_open_send(
            &mut self.inner,
            cx
        ))?;
        std::task::Poll::Ready(Ok(BudgetedSendStream::new(stream)))
    }

    fn close(&mut self, code: h3::error::Code, reason: &[u8]) {
        quic::OpenStreams::<Bytes>::close(&mut self.inner, code, reason);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn varint(value: u64) -> Vec<u8> {
        // QUIC variable-length integer, shortest encoding.
        if value < 64 {
            vec![u8::try_from(value).expect("fits in one byte")]
        } else if value < (1 << 14) {
            u16::try_from(0x4000 | value)
                .expect("fits in two bytes")
                .to_be_bytes()
                .to_vec()
        } else if value < (1 << 30) {
            u32::try_from(0x8000_0000 | value)
                .expect("fits in four bytes")
                .to_be_bytes()
                .to_vec()
        } else {
            (0xC000_0000_0000_0000 | value).to_be_bytes().to_vec()
        }
    }

    #[test]
    fn data_and_webtransport_frames_pass() {
        assert!(matches!(verdict(&varint(0x0)), Verdict::Ok));
        assert!(matches!(verdict(&varint(0x40)), Verdict::Ok));
    }

    #[test]
    fn headers_frame_within_budget_passes() {
        // HEADERS (0x1) with a small length.
        let mut header = varint(0x1);
        header.extend(varint(1024));
        assert!(matches!(verdict(&header), Verdict::Ok));
    }

    #[test]
    fn oversized_headers_frame_is_rejected_before_payload() {
        let mut header = varint(0x1);
        header.extend(varint(MAX_CONTROL_FRAME_LEN as u64 + 1));
        assert!(matches!(verdict(&header), Verdict::TooBig(_)));
        // A maximal multi-MiB declaration (the finding's payload shape).
        let mut header = varint(0x1);
        header.extend(varint(16 * 1024 * 1024));
        assert!(matches!(verdict(&header), Verdict::TooBig(_)));
    }

    #[test]
    fn incomplete_header_needs_more_then_decides() {
        // Type varint alone: cannot decide yet.
        assert!(matches!(verdict(&varint(0x1)), Verdict::NeedMore));
        // Settings frame (0x4) with empty payload: within budget.
        let mut header = varint(0x4);
        header.extend(varint(0));
        assert!(matches!(verdict(&header), Verdict::Ok));
    }

    #[test]
    fn varint_decodes_each_width_and_waits_for_whole_encodings() {
        for value in [
            0,
            63,
            64,
            (1 << 14) - 1,
            1 << 14,
            (1 << 30) - 1,
            1 << 30,
            (1 << 62) - 1,
        ] {
            let encoded = varint(value);
            assert_eq!(super::varint(&encoded, 0), Some((value, encoded.len())));
            for seen in 0..encoded.len() {
                assert_eq!(
                    super::varint(&encoded[..seen], 0),
                    None,
                    "{seen} of {value}"
                );
            }
            // An offset skips what comes before the integer.
            let mut padded = vec![0xFF];
            padded.extend(&encoded);
            assert_eq!(super::varint(&padded, 1), Some((value, encoded.len())));
        }
    }

    struct ScriptedStream {
        chunks: std::collections::VecDeque<Bytes>,
        stopped: Vec<u64>,
    }

    impl quic::RecvStream for ScriptedStream {
        type Buf = Bytes;

        fn poll_data(
            &mut self,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<Option<Self::Buf>, StreamErrorIncoming>> {
            std::task::Poll::Ready(Ok(self.chunks.pop_front()))
        }

        fn stop_sending(&mut self, error_code: u64) {
            self.stopped.push(error_code);
        }

        fn recv_id(&self) -> StreamId {
            StreamId::from(h3::proto::varint::VarInt::from_u32(0))
        }
    }

    fn frame(frame_type: u64, length: u64, payload: usize) -> Vec<u8> {
        let mut bytes = varint(frame_type);
        bytes.extend(varint(length));
        bytes.extend(std::iter::repeat_n(0xA5, payload));
        bytes
    }

    /// Receives `bytes` as chunks of `width` bytes, starting from `budget`.
    fn receive(budget: Budget, bytes: &[u8], width: usize) -> (Vec<u8>, Option<String>, Vec<u64>) {
        let mut stream = BudgetedRecvStream {
            inner: ScriptedStream {
                chunks: bytes
                    .chunks(width)
                    .map(<[u8]>::to_vec)
                    .map(Bytes::from)
                    .collect(),
                stopped: Vec::new(),
            },
            budget,
        };
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        let mut forwarded = Vec::new();
        let error = loop {
            match quic::RecvStream::poll_data(&mut stream, &mut cx) {
                std::task::Poll::Ready(Ok(Some(chunk))) => forwarded.extend_from_slice(&chunk),
                std::task::Poll::Ready(Ok(None)) => break None,
                std::task::Poll::Ready(Err(error)) => break Some(format!("{error:?}")),
                std::task::Poll::Pending => break Some("pending".into()),
            }
        };
        (forwarded, error, stream.inner.stopped)
    }

    /// Every way of splitting `bytes` into chunks must forward it unchanged.
    fn assert_forwarded_unchanged(budget: fn() -> Budget, bytes: &[u8]) {
        for width in 1..=bytes.len() {
            let (forwarded, error, stopped) = receive(budget(), bytes, width);
            assert_eq!(forwarded, bytes, "chunk width {width}");
            assert_eq!(error, None, "chunk width {width}");
            assert!(stopped.is_empty(), "chunk width {width}");
        }
    }

    /// Every way of splitting `bytes` into chunks must reject the oversized
    /// header and stop the peer.
    fn assert_rejected(budget: fn() -> Budget, bytes: &[u8]) {
        for width in 1..=bytes.len() {
            let (_, error, stopped) = receive(budget(), bytes, width);
            assert!(error.is_some(), "chunk width {width}");
            assert_eq!(stopped, vec![H3_EXCESSIVE_LOAD], "chunk width {width}");
        }
    }

    #[test]
    fn control_stream_settings_length_is_bounded() {
        // Control stream type, then SETTINGS declaring 1 GiB.
        let mut bytes = vec![0x00];
        bytes.extend(varint(0x04));
        bytes.extend(varint(1 << 30));
        assert_rejected(Budget::at_stream_type, &bytes);
    }

    #[test]
    fn control_stream_rejects_an_oversized_later_frame() {
        let mut bytes = vec![0x00];
        bytes.extend(frame(0x04, 4, 4));
        bytes.extend(varint(0x07));
        bytes.extend(varint(1 << 30));
        assert_rejected(Budget::at_stream_type, &bytes);
    }

    #[test]
    fn control_stream_frames_are_forwarded_unchanged() {
        // SETTINGS, GOAWAY and MAX_PUSH_ID on a control stream.
        let mut bytes = vec![0x00];
        bytes.extend(frame(0x04, 4, 4));
        bytes.extend(frame(0x07, 4, 4));
        bytes.extend(frame(0x0d, 2, 2));
        assert_forwarded_unchanged(Budget::at_stream_type, &bytes);
    }

    #[test]
    fn frame_after_the_first_is_bounded_on_request_streams() {
        let mut bytes = frame(0x01, 8, 8);
        bytes.extend(frame(0x00, 4, 4));
        // A reserved type with an oversized length, sent without its payload.
        bytes.extend(varint(0x21));
        bytes.extend(varint(1 << 20));
        assert_rejected(Budget::between_frames, &bytes);
    }

    #[test]
    fn request_stream_frames_are_forwarded_unchanged() {
        // HEADERS, DATA, a zero-length frame of a reserved type, then trailers.
        let mut bytes = frame(0x01, 8, 8);
        bytes.extend(frame(0x00, 4, 4));
        bytes.extend(frame(0x21, 0, 0));
        bytes.extend(frame(0x01, 2, 2));
        assert_forwarded_unchanged(Budget::between_frames, &bytes);
    }

    #[test]
    fn data_frames_pass_whatever_length_they_declare() {
        let mut bytes = frame(0x01, 0, 0);
        bytes.extend(frame(0x00, 1 << 40, 5));
        bytes.extend([0xFF; 9]);
        assert_forwarded_unchanged(Budget::between_frames, &bytes);
    }

    #[test]
    fn qpack_streams_are_not_parsed_as_frames() {
        // Encoder stream type, then bytes that would read as an oversized frame.
        let mut bytes = vec![0x02];
        bytes.extend(varint(0x04));
        bytes.extend(varint(1 << 30));
        assert_forwarded_unchanged(Budget::at_stream_type, &bytes);
    }

    #[test]
    fn webtransport_unidirectional_streams_are_not_parsed_as_frames() {
        // WEBTRANSPORT_UNIDIRECTIONAL (0x54), a session id, then raw bytes.
        let mut bytes = vec![0x54, 0x07];
        bytes.extend(varint(0x04));
        bytes.extend(varint(1 << 30));
        assert_forwarded_unchanged(Budget::at_stream_type, &bytes);
    }

    #[test]
    fn webtransport_bidi_stream_data_is_not_parsed_as_frames() {
        let mut bytes = frame(0x01, 4, 4);
        bytes.push(0x41);
        bytes.extend(varint(0x04));
        bytes.extend(varint(1 << 30));
        assert_forwarded_unchanged(Budget::between_frames, &bytes);
    }

    #[test]
    fn locally_opened_streams_are_not_parsed_as_frames() {
        let mut bytes = varint(0x04);
        bytes.extend(varint(1 << 30));
        assert_forwarded_unchanged(Budget::unframed, &bytes);
    }
}
