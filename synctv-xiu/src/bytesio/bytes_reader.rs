use {
    super::{
        bytes_errors::{BytesReadError, BytesReadErrorValue},
        bytesio::TNetIO,
    },
    byteorder::{ByteOrder, ReadBytesExt},
    bytes::{BufMut, BytesMut},
    futures::FutureExt,
    std::{io::Cursor, sync::Arc, time::Duration},
    tokio::sync::Mutex,
};

/// Default timeout for read operations (10 seconds).
const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Max buffer size (10 MB) to prevent unbounded memory growth from malicious/buggy input.
const MAX_BUFFER_SIZE: usize = 10 * 1024 * 1024;

/// Create an Elapsed error without requiring a timeout future.
/// This is needed because `tokio::time::error::Elapsed::new()` is private.
fn elapsed_error() -> tokio::time::error::Elapsed {
    // Use a zero-duration timeout on a pending future to generate an Elapsed error.
    tokio::time::timeout(Duration::ZERO, std::future::pending::<()>())
        .now_or_never()
        .expect("zero-duration timeout should resolve immediately")
        .expect_err("zero-duration timeout on pending should always fail with Elapsed")
}

pub struct BytesReader {
    buffer: BytesMut,
}
impl BytesReader {
    #[must_use]
    pub const fn new(input: BytesMut) -> Self {
        Self { buffer: input }
    }

    pub fn extend_from_slice(&mut self, extend: &[u8]) -> Result<(), BytesReadError> {
        let new_len = self.buffer.len() + extend.len();
        if new_len > MAX_BUFFER_SIZE {
            return Err(BytesReadError {
                value: BytesReadErrorValue::BufferOverflow {
                    current: self.buffer.len(),
                    additional: extend.len(),
                    max: MAX_BUFFER_SIZE,
                },
            });
        }

        let remaining_mut = self.buffer.remaining_mut();
        let extend_length = extend.len();

        if extend_length > remaining_mut {
            let additional = extend_length - remaining_mut;
            self.buffer.reserve(additional);
        }

        self.buffer.extend_from_slice(extend);
        Ok(())
    }

    pub fn read_bytes(&mut self, bytes_num: usize) -> Result<BytesMut, BytesReadError> {
        if self.buffer.len() < bytes_num {
            return Err(BytesReadError {
                value: BytesReadErrorValue::NotEnoughBytes,
            });
        }
        Ok(self.buffer.split_to(bytes_num))
    }

    pub fn advance_bytes(&mut self, bytes_num: usize) -> Result<BytesMut, BytesReadError> {
        if self.buffer.len() < bytes_num {
            return Err(BytesReadError {
                value: BytesReadErrorValue::NotEnoughBytes,
            });
        }

        Ok(BytesMut::from(&self.buffer[..bytes_num]))
    }

    pub fn read_bytes_cursor(
        &mut self,
        bytes_num: usize,
    ) -> Result<Cursor<BytesMut>, BytesReadError> {
        let tmp_bytes = self.read_bytes(bytes_num)?;
        let tmp_cursor = Cursor::new(tmp_bytes);
        Ok(tmp_cursor)
    }

    pub fn advance_bytes_cursor(
        &mut self,
        bytes_num: usize,
    ) -> Result<Cursor<BytesMut>, BytesReadError> {
        let tmp_bytes = self.advance_bytes(bytes_num)?;
        let tmp_cursor = Cursor::new(tmp_bytes);
        Ok(tmp_cursor)
    }

    pub fn read_u8(&mut self) -> Result<u8, BytesReadError> {
        let mut cursor = self.read_bytes_cursor(1)?;

        Ok(cursor.read_u8()?)
    }

    pub fn advance_u8(&mut self) -> Result<u8, BytesReadError> {
        let mut cursor = self.advance_bytes_cursor(1)?;
        Ok(cursor.read_u8()?)
    }

    pub fn read_u16<T: ByteOrder>(&mut self) -> Result<u16, BytesReadError> {
        let mut cursor = self.read_bytes_cursor(2)?;
        let val = cursor.read_u16::<T>()?;
        Ok(val)
    }

    pub fn read_u24<T: ByteOrder>(&mut self) -> Result<u32, BytesReadError> {
        let mut cursor = self.read_bytes_cursor(3)?;
        let val = cursor.read_u24::<T>()?;
        Ok(val)
    }

    pub fn advance_u24<T: ByteOrder>(&mut self) -> Result<u32, BytesReadError> {
        let mut cursor = self.advance_bytes_cursor(3)?;
        Ok(cursor.read_u24::<T>()?)
    }

    pub fn read_u32<T: ByteOrder>(&mut self) -> Result<u32, BytesReadError> {
        let mut cursor = self.read_bytes_cursor(4)?;
        let val = cursor.read_u32::<T>()?;

        Ok(val)
    }

    pub fn read_u48<T: ByteOrder>(&mut self) -> Result<u64, BytesReadError> {
        let mut cursor = self.read_bytes_cursor(6)?;
        let val = cursor.read_u48::<T>()?;

        Ok(val)
    }

    pub fn read_f64<T: ByteOrder>(&mut self) -> Result<f64, BytesReadError> {
        let mut cursor = self.read_bytes_cursor(8)?;
        let val = cursor.read_f64::<T>()?;

        Ok(val)
    }

    pub fn read_u64<T: ByteOrder>(&mut self) -> Result<u64, BytesReadError> {
        let mut cursor = self.read_bytes_cursor(8)?;
        let val = cursor.read_u64::<T>()?;

        Ok(val)
    }

    pub fn get(&self, index: usize) -> Result<u8, BytesReadError> {
        if index >= self.len() {
            return Err(BytesReadError {
                value: BytesReadErrorValue::IndexOutofRange,
            });
        }

        // SAFETY: We've already verified that index < self.len() above
        Ok(self.buffer[index])
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn extract_remaining_bytes(&mut self) -> BytesMut {
        self.buffer.split_to(self.buffer.len())
    }
    #[must_use]
    pub fn get_remaining_bytes(&self) -> BytesMut {
        self.buffer.clone()
    }
}
pub struct AsyncBytesReader<T1: TNetIO> {
    pub bytes_reader: BytesReader,
    pub io: Arc<Mutex<T1>>,
    /// Timeout for read operations. If None, DEFAULT_READ_TIMEOUT is used.
    timeout: Option<Duration>,
}

impl<T1> AsyncBytesReader<T1>
where
    T1: TNetIO,
{
    pub fn new(io: Arc<Mutex<T1>>) -> Self {
        Self {
            bytes_reader: BytesReader::new(BytesMut::default()),
            io,
            timeout: None,
        }
    }

    /// Creates a new AsyncBytesReader with a custom timeout.
    pub fn with_timeout(io: Arc<Mutex<T1>>, timeout: Duration) -> Self {
        Self {
            bytes_reader: BytesReader::new(BytesMut::default()),
            io,
            timeout: Some(timeout),
        }
    }

    /// Sets a custom timeout for read operations.
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = Some(timeout);
    }

    /// Returns the current timeout duration.
    pub fn timeout(&self) -> Duration {
        self.timeout.unwrap_or(DEFAULT_READ_TIMEOUT)
    }

    pub async fn read(&mut self) -> Result<(), BytesReadError> {
        let data = self.io.lock().await.read().await?;
        self.bytes_reader.extend_from_slice(&data[..])?;
        Ok(())
    }

    /// Checks that at least `bytes_num` bytes are available in the buffer.
    /// If not, reads from the underlying IO with a timeout to prevent infinite loops.
    async fn check(&mut self, bytes_num: usize) -> Result<(), BytesReadError> {
        let timeout_duration = self.timeout();
        let start = std::time::Instant::now();

        while self.bytes_reader.len() < bytes_num {
            // Calculate remaining time for this read attempt
            let remaining = timeout_duration.saturating_sub(start.elapsed());
            if remaining.is_zero() {
                return Err(elapsed_error().into());
            }

            // Read with the remaining timeout
            let read_future = async { self.io.lock().await.read().await };

            match tokio::time::timeout(remaining, read_future).await {
                Ok(Ok(data)) => {
                    self.bytes_reader.extend_from_slice(&data[..])?;
                }
                Ok(Err(e)) => {
                    return Err(BytesReadError {
                        value: BytesReadErrorValue::BytesIOError(e),
                    });
                }
                Err(elapsed) => {
                    return Err(elapsed.into());
                }
            }
        }

        Ok(())
    }

    pub async fn read_bytes(&mut self, bytes_num: usize) -> Result<BytesMut, BytesReadError> {
        self.check(bytes_num).await?;
        self.bytes_reader.read_bytes(bytes_num)
    }

    pub async fn advance_bytes(&mut self, bytes_num: usize) -> Result<BytesMut, BytesReadError> {
        self.check(bytes_num).await?;
        self.bytes_reader.advance_bytes(bytes_num)
    }

    pub async fn read_bytes_cursor(
        &mut self,
        bytes_num: usize,
    ) -> Result<Cursor<BytesMut>, BytesReadError> {
        self.check(bytes_num).await?;
        self.bytes_reader.read_bytes_cursor(bytes_num)
    }

    pub async fn advance_bytes_cursor(
        &mut self,
        bytes_num: usize,
    ) -> Result<Cursor<BytesMut>, BytesReadError> {
        self.check(bytes_num).await?;
        self.bytes_reader.advance_bytes_cursor(bytes_num)
    }

    pub async fn read_u8(&mut self) -> Result<u8, BytesReadError> {
        self.check(1).await?;
        self.bytes_reader.read_u8()
    }

    pub async fn advance_u8(&mut self) -> Result<u8, BytesReadError> {
        self.check(1).await?;
        self.bytes_reader.advance_u8()
    }

    pub async fn read_u16<T: ByteOrder>(&mut self) -> Result<u16, BytesReadError> {
        self.check(2).await?;
        self.bytes_reader.read_u16::<T>()
    }

    pub async fn read_u24<T: ByteOrder>(&mut self) -> Result<u32, BytesReadError> {
        self.check(3).await?;
        self.bytes_reader.read_u24::<T>()
    }

    pub async fn advance_u24<T: ByteOrder>(&mut self) -> Result<u32, BytesReadError> {
        self.check(3).await?;
        self.bytes_reader.advance_u24::<T>()
    }

    pub async fn read_u32<T: ByteOrder>(&mut self) -> Result<u32, BytesReadError> {
        self.check(4).await?;
        self.bytes_reader.read_u32::<T>()
    }

    pub async fn read_f64<T: ByteOrder>(&mut self) -> Result<f64, BytesReadError> {
        self.check(8).await?;
        self.bytes_reader.read_f64::<T>()
    }
}

#[cfg(test)]
mod tests {

    use super::BytesReader;
    use bytes::BytesMut;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[test]
    fn test_rc_refcell() {
        let reader = Rc::new(RefCell::new(BytesReader::new(BytesMut::new())));
        let xs: [u8; 3] = [1, 2, 3];
        reader.borrow_mut().extend_from_slice(&xs[..]).unwrap();

        let mut rv = reader.borrow_mut().read_u8().unwrap();
        assert_eq!(rv, 1, "Incorrect value");

        rv = reader.borrow_mut().read_u8().unwrap();
        assert_eq!(rv, 2, "Incorrect value");

        rv = reader.borrow_mut().read_u8().unwrap();
        assert_eq!(rv, 3, "Incorrect value");
    }

    struct RefStruct {
        pub reader: Rc<RefCell<BytesReader>>,
    }

    impl RefStruct {
        pub fn new(reader: Rc<RefCell<BytesReader>>) -> Self {
            Self { reader }
        }

        pub fn extend_from_slice(&self, data: &[u8]) {
            self.reader.borrow_mut().extend_from_slice(data).unwrap();
        }
    }

    #[test]
    fn test_struct_rc_refcell() {
        let reader = Rc::new(RefCell::new(BytesReader::new(BytesMut::new())));

        let ref_struct = RefStruct::new(reader);

        let xs: [u8; 3] = [1, 2, 3];
        ref_struct.extend_from_slice(&xs);

        let mut reader = ref_struct.reader.borrow_mut();

        let mut rv = reader.read_u8().unwrap();
        assert_eq!(rv, 1, "Incorrect value");

        rv = reader.read_u8().unwrap();
        assert_eq!(rv, 2, "Incorrect value");

        rv = reader.read_u8().unwrap();
        assert_eq!(rv, 3, "Incorrect value");
    }
}

#[cfg(test)]
mod async_tests {
    use super::*;
    use async_trait::async_trait;
    use bytes::Bytes;
    use std::time::Duration;

    /// A mock TNetIO implementation that never returns data.
    /// This is used to test the timeout behavior of AsyncBytesReader.
    struct NeverReturningIO;

    #[async_trait]
    impl TNetIO for NeverReturningIO {
        async fn write(
            &mut self,
            _bytes: Bytes,
        ) -> Result<(), super::super::bytesio_errors::BytesIOError> {
            Ok(())
        }

        async fn read(&mut self) -> Result<BytesMut, super::super::bytesio_errors::BytesIOError> {
            // Never returns - simulates a stalled connection
            std::future::pending().await
        }

        async fn read_timeout(
            &mut self,
            _duration: Duration,
        ) -> Result<BytesMut, super::super::bytesio_errors::BytesIOError> {
            // Never returns - simulates a stalled connection
            std::future::pending().await
        }

        fn get_net_type(&self) -> super::super::bytesio::NetType {
            super::super::bytesio::NetType::TCP
        }
    }

    #[tokio::test]
    async fn test_async_bytes_reader_timeout_on_check() {
        // Create an AsyncBytesReader with a very short timeout (100ms)
        let io = Arc::new(Mutex::new(NeverReturningIO));
        let mut reader = AsyncBytesReader::with_timeout(io, Duration::from_millis(100));

        // Try to read 1 byte - this should timeout since NeverReturningIO never returns data
        let result = reader.read_bytes(1).await;

        // Verify that we got a timeout error
        assert!(result.is_err(), "Expected timeout error");
        let err = result.unwrap_err();
        matches!(err.value, BytesReadErrorValue::TimeoutError(_));
    }

    #[tokio::test]
    async fn test_async_bytes_reader_default_timeout() {
        // Create an AsyncBytesReader with default timeout
        let io = Arc::new(Mutex::new(NeverReturningIO));
        let reader = AsyncBytesReader::new(io.clone());

        // Verify default timeout is 10 seconds
        assert_eq!(reader.timeout(), Duration::from_secs(10));
    }

    #[tokio::test]
    async fn test_async_bytes_reader_custom_timeout() {
        // Create an AsyncBytesReader with custom timeout
        let io = Arc::new(Mutex::new(NeverReturningIO));
        let custom_timeout = Duration::from_millis(500);
        let reader = AsyncBytesReader::with_timeout(io.clone(), custom_timeout);

        // Verify custom timeout is set
        assert_eq!(reader.timeout(), custom_timeout);
    }

    #[tokio::test]
    async fn test_async_bytes_reader_set_timeout() {
        // Create an AsyncBytesReader and set timeout later
        let io = Arc::new(Mutex::new(NeverReturningIO));
        let mut reader = AsyncBytesReader::new(io.clone());

        // Initially should have default timeout
        assert_eq!(reader.timeout(), Duration::from_secs(10));

        // Set a custom timeout
        let custom_timeout = Duration::from_millis(200);
        reader.set_timeout(custom_timeout);

        // Verify timeout was updated
        assert_eq!(reader.timeout(), custom_timeout);
    }
}
