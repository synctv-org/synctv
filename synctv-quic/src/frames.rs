//! Length-prefixed frame codec for WebTransport bidi streams.
//!
//! Frames are `4-byte big-endian length + payload` — the same wire shape the
//! realtime transports use. Inbound frames are bounded to the WebSocket
//! message cap (a hostile-client bound); outbound frames get a larger
//! budget so legitimate snapshots (playlists, chat replay) that the
//! WebSocket path delivers fine do not kill a WebTransport session.

use std::io;

use bytes::{BufMut, Bytes, BytesMut};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Maximum single inbound frame payload: matches the WebSocket message size
/// cap and bounds what a hostile client can make the server allocate.
pub const MAX_FRAME_BYTES: usize = 64 * 1024;

/// Maximum single outbound frame payload: generous enough for large
/// resource snapshots, small enough to bound reassembly on the client.
pub const MAX_OUTBOUND_FRAME_BYTES: usize = 1024 * 1024;

const LENGTH_PREFIX_BYTES: usize = 4;

/// Reads one frame. Returns `None` on clean EOF at a frame boundary.
///
/// Oversized or truncated frames return an error; callers treat that as a
/// protocol violation and drop the session.
pub async fn read_frame<R>(read: &mut R) -> io::Result<Option<Bytes>>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut prefix = [0u8; LENGTH_PREFIX_BYTES];
    match read.read_exact(&mut prefix).await {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error),
    }
    let length = u32::from_be_bytes(prefix) as usize;
    if length > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame payload {length} exceeds {MAX_FRAME_BYTES} bytes"),
        ));
    }
    let mut payload = vec![0u8; length];
    read.read_exact(&mut payload).await?;
    Ok(Some(Bytes::from(payload)))
}

/// Writes one frame.
pub async fn write_frame<W>(write: &mut W, payload: &[u8]) -> io::Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    if payload.len() > MAX_OUTBOUND_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "frame payload {} exceeds {MAX_OUTBOUND_FRAME_BYTES} bytes",
                payload.len()
            ),
        ));
    }
    let mut buffer = BytesMut::with_capacity(LENGTH_PREFIX_BYTES + payload.len());
    buffer.put_u32(u32::try_from(payload.len()).expect("checked above"));
    buffer.put_slice(payload);
    write.write_all(&buffer).await
}
