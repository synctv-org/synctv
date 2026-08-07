use std::{
    future::Future,
    io::{ErrorKind, SeekFrom},
    pin::Pin,
    task::{Context, Poll},
};

use bytes::Bytes;
use futures::FutureExt;
use tokio::io::{AsyncRead, AsyncSeek, ReadBuf};

use crate::{Error, Result};

type RangeReadFuture = Pin<Box<dyn Future<Output = Result<Bytes>> + Send + 'static>>;

pub(crate) struct RangeSeekReader {
    size_bytes: i64,
    position: i64,
    chunk_size: usize,
    read_at: Box<dyn Fn(i64, usize) -> RangeReadFuture + Send + Sync>,
    pending: Option<RangeReadFuture>,
    buffered: Bytes,
}

impl RangeSeekReader {
    pub(crate) fn new(
        size_bytes: i64,
        chunk_size: usize,
        read_at: impl Fn(i64, usize) -> RangeReadFuture + Send + Sync + 'static,
    ) -> Result<Self> {
        if size_bytes < 0 || chunk_size == 0 {
            return Err(Error::InvalidInput(
                "file reader size and chunk size are invalid".to_string(),
            ));
        }
        Ok(Self {
            size_bytes,
            position: 0,
            chunk_size,
            read_at: Box::new(read_at),
            pending: None,
            buffered: Bytes::new(),
        })
    }

    fn io_error(error: &Error) -> std::io::Error {
        std::io::Error::other(error.to_string())
    }
}

impl AsyncRead for RangeSeekReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if buf.remaining() == 0 || self.position >= self.size_bytes {
            return Poll::Ready(Ok(()));
        }
        if self.buffered.is_empty() && self.pending.is_none() {
            let remaining = usize::try_from(self.size_bytes - self.position).unwrap_or(usize::MAX);
            let length = remaining.min(self.chunk_size);
            self.pending = Some((self.read_at)(self.position, length));
        }
        if let Some(pending) = self.pending.as_mut() {
            match pending.poll_unpin(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Ok(bytes)) => {
                    self.pending = None;
                    self.buffered = bytes;
                }
                Poll::Ready(Err(error)) => {
                    self.pending = None;
                    return Poll::Ready(Err(Self::io_error(&error)));
                }
            }
        }
        if self.buffered.is_empty() {
            return Poll::Ready(Ok(()));
        }
        let take = self.buffered.len().min(buf.remaining());
        let chunk = self.buffered.split_to(take);
        buf.put_slice(&chunk);
        self.position += i64::try_from(take).map_err(|_| {
            std::io::Error::new(ErrorKind::InvalidData, "file reader position overflow")
        })?;
        Poll::Ready(Ok(()))
    }
}

impl AsyncSeek for RangeSeekReader {
    fn start_seek(mut self: Pin<&mut Self>, position: SeekFrom) -> std::io::Result<()> {
        let target = match position {
            SeekFrom::Start(offset) => i64::try_from(offset).map_err(|_| {
                std::io::Error::new(ErrorKind::InvalidInput, "file seek offset is invalid")
            })?,
            SeekFrom::End(offset) => self.size_bytes.checked_add(offset).ok_or_else(|| {
                std::io::Error::new(ErrorKind::InvalidInput, "file seek offset overflow")
            })?,
            SeekFrom::Current(offset) => self.position.checked_add(offset).ok_or_else(|| {
                std::io::Error::new(ErrorKind::InvalidInput, "file seek offset overflow")
            })?,
        };
        if target < 0 {
            return Err(std::io::Error::new(
                ErrorKind::InvalidInput,
                "file seek target is negative",
            ));
        }
        self.position = target.min(self.size_bytes);
        self.pending = None;
        self.buffered = Bytes::new();
        Ok(())
    }

    fn poll_complete(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<u64>> {
        Poll::Ready(u64::try_from(self.position).map_err(|_| {
            std::io::Error::new(ErrorKind::InvalidInput, "file seek position is invalid")
        }))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::io::{AsyncReadExt, AsyncSeekExt};

    use super::*;

    #[tokio::test]
    async fn range_seek_reader_reads_and_seeks_without_buffering_whole_object() {
        let data = Arc::new(Bytes::from_static(b"abcdefghijklmnopqrstuvwxyz"));
        let data_len = i64::try_from(data.len()).expect("test data length should fit");
        let mut reader = RangeSeekReader::new(data_len, 5, move |offset, length| {
            let data = Arc::clone(&data);
            Box::pin(async move {
                let start = usize::try_from(offset).expect("offset should fit");
                let end = start + length;
                Ok(data.slice(start..end.min(data.len())))
            })
        })
        .expect("reader should be created");

        let mut prefix = [0_u8; 7];
        reader
            .read_exact(&mut prefix)
            .await
            .expect("prefix should read");
        assert_eq!(&prefix, b"abcdefg");

        reader
            .seek(SeekFrom::Start(20))
            .await
            .expect("seek should work");
        let mut suffix = Vec::new();
        reader
            .read_to_end(&mut suffix)
            .await
            .expect("suffix should read");
        assert_eq!(suffix, b"uvwxyz");

        reader
            .seek(SeekFrom::End(-3))
            .await
            .expect("relative seek should work");
        let mut tail = [0_u8; 3];
        reader
            .read_exact(&mut tail)
            .await
            .expect("tail should read");
        assert_eq!(&tail, b"xyz");
    }
}
