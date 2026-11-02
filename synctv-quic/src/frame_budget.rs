//! Pre-decode inbound frame-length budget for the HTTP/3 listener.
//!
//! h3 0.0.8 assembles a non-DATA frame to its attacker-declared length
//! before any size check (`max_field_section_size` is applied post-assembly
//! and the listener cannot rely on it for pre-decode buffering), so a peer
//! declares a multi-MiB HEADERS frame and the server buffers it byte by
//! byte. The adapter here inspects the first frame header on every accepted
//! stream - frame type varint, and for length-prefixed non-DATA types the
//! length varint - and rejects with `H3_EXCESSIVE_LOAD` before the payload
//! can be buffered. DATA frames pass through (bodies are capped by
//! `H3RecvBody::REQUEST_BODY_LIMIT` downstream) and WebTransport stream
//! types carry only a session id, so both are exempt.

use bytes::{Buf, Bytes};
use h3::proto::stream::StreamId;
use h3::quic::WriteBuf;
use h3::quic::{self, ConnectionErrorIncoming, StreamErrorIncoming};
use h3_datagram::quic_traits::DatagramConnectionExt;

/// Maximum declared length accepted for a non-DATA frame before the request
/// resolves. Mirrors the inbound frame cap used by the synctv frame codec.
pub const MAX_CONTROL_FRAME_LEN: usize = 64 * 1024;

/// h3 `H3_EXCESSIVE_LOAD` error code (h3 `error/codes.rs`).
const H3_EXCESSIVE_LOAD: u64 = 0x107;

/// A frame type varint plus a length varint never exceed 16 bytes.
const MAX_FRAME_HEADER: usize = 16;

enum Verdict {
    /// Not enough bytes buffered yet to decide.
    NeedMore,
    /// The first frame is within budget (or carries no length to police).
    Ok,
    /// The declared length exceeds the budget.
    TooBig(u64),
}

fn read_varint(buf: &[u8]) -> Option<(u64, usize)> {
    let first = *buf.first()?;
    let len = 1_usize << (first >> 6);
    if buf.len() < len {
        return None;
    }
    let mut value = u64::from(first & 0x3f);
    for byte in &buf[1..len] {
        value = (value << 8) | u64::from(*byte);
    }
    Some((value, len))
}

fn verdict(header: &[u8]) -> Verdict {
    let Some((frame_type, type_len)) = read_varint(header) else {
        return Verdict::NeedMore;
    };
    // DATA (0x0) has its own downstream body cap; extended stream types
    // (>= 0x40, e.g. WebTransport streams) carry only a session id.
    if frame_type == 0x0 || frame_type >= 0x40 {
        return Verdict::Ok;
    }
    let Some((length, length_len)) = read_varint(&header[type_len..]) else {
        return Verdict::NeedMore;
    };
    let declared = usize::try_from(length).unwrap_or(usize::MAX);
    match type_len
        .checked_add(length_len)
        .and_then(|prefix| prefix.checked_add(declared))
    {
        Some(total) if total > MAX_CONTROL_FRAME_LEN => Verdict::TooBig(length),
        _ => Verdict::Ok,
    }
}

/// Receives stream bytes while refusing to buffer a non-DATA frame whose
/// declared length exceeds [`MAX_CONTROL_FRAME_LEN`]. Wraps any h3 quic
/// receive half whose buffer type is `Bytes`.
pub struct BudgetedRecvStream<S> {
    inner: S,
    header: Vec<u8>,
    checked: bool,
}

impl<S> BudgetedRecvStream<S> {
    fn new(inner: S) -> Self {
        Self {
            inner,
            header: Vec::new(),
            checked: false,
        }
    }
}

impl<S: quic::RecvStream<Buf = Bytes>> quic::RecvStream for BudgetedRecvStream<S> {
    type Buf = Bytes;

    fn poll_data(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<Option<Self::Buf>, StreamErrorIncoming>> {
        let chunk = std::task::ready!(self.inner.poll_data(cx))?;
        if !self.checked {
            if let Some(chunk) = chunk.as_ref() {
                let want = MAX_FRAME_HEADER - self.header.len();
                let bytes = chunk.chunk();
                self.header
                    .extend_from_slice(&bytes[..bytes.len().min(want)]);
            }
            match verdict(&self.header) {
                // Two varints cannot span 16 bytes: a header that never
                // completes within them is hostile.
                Verdict::NeedMore if self.header.len() >= MAX_FRAME_HEADER => {
                    self.inner.stop_sending(H3_EXCESSIVE_LOAD);
                    return std::task::Poll::Ready(Err(StreamErrorIncoming::Unknown(
                        "malformed frame header".into(),
                    )));
                }
                Verdict::TooBig(length) => {
                    self.inner.stop_sending(H3_EXCESSIVE_LOAD);
                    return std::task::Poll::Ready(Err(StreamErrorIncoming::Unknown(
                        format!("frame length {length} exceeds {MAX_CONTROL_FRAME_LEN}").into(),
                    )));
                }
                Verdict::Ok => self.checked = true,
                Verdict::NeedMore => {}
            }
        }
        std::task::Poll::Ready(Ok(chunk))
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
    header: Vec<u8>,
    checked: bool,
}

impl<B: Buf> BudgetedBidiStream<B> {
    fn new(inner: h3_quinn::BidiStream<B>) -> Self {
        Self {
            inner,
            header: Vec::new(),
            checked: false,
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
                header: self.header,
                checked: self.checked,
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
        let chunk = std::task::ready!(quic::RecvStream::poll_data(&mut self.inner, cx))?;
        if !self.checked {
            if let Some(chunk) = chunk.as_ref() {
                let want = MAX_FRAME_HEADER - self.header.len();
                let bytes = chunk.chunk();
                self.header
                    .extend_from_slice(&bytes[..bytes.len().min(want)]);
            }
            match verdict(&self.header) {
                Verdict::NeedMore if self.header.len() >= MAX_FRAME_HEADER => {
                    self.inner.stop_sending(H3_EXCESSIVE_LOAD);
                    return std::task::Poll::Ready(Err(StreamErrorIncoming::Unknown(
                        "malformed frame header".into(),
                    )));
                }
                Verdict::TooBig(length) => {
                    self.inner.stop_sending(H3_EXCESSIVE_LOAD);
                    return std::task::Poll::Ready(Err(StreamErrorIncoming::Unknown(
                        format!("frame length {length} exceeds {MAX_CONTROL_FRAME_LEN}").into(),
                    )));
                }
                Verdict::Ok => self.checked = true,
                Verdict::NeedMore => {}
            }
        }
        std::task::Poll::Ready(Ok(chunk))
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
        std::task::Poll::Ready(Ok(BudgetedBidiStream::new(stream)))
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
        std::task::Poll::Ready(Ok(BudgetedBidiStream::new(stream)))
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
        std::task::Poll::Ready(Ok(BudgetedBidiStream::new(stream)))
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
}
