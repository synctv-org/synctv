use {
    super::{
        bytes_errors::{BytesWriteError, BytesWriteErrorValue},
        net_io::TNetIO,
    },
    byteorder::{ByteOrder, WriteBytesExt},
    bytes::{Bytes, BytesMut},
    rand::RngExt,
    std::{io::Write, sync::Arc, time::Duration},
    tokio::{sync::Mutex, time::timeout},
};

pub struct BytesWriter {
    pub bytes: Vec<u8>,
}

impl Default for BytesWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl BytesWriter {
    #[must_use]
    pub const fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    pub fn write_u8(&mut self, byte: u8) -> Result<(), BytesWriteError> {
        self.bytes.write_u8(byte)?;
        Ok(())
    }

    pub fn or_u8_at(&mut self, position: usize, byte: u8) -> Result<(), BytesWriteError> {
        if position >= self.bytes.len() {
            return Err(BytesWriteError {
                value: BytesWriteErrorValue::OutofIndex,
            });
        }
        self.bytes[position] |= byte;

        Ok(())
    }

    pub fn add_u8_at(&mut self, position: usize, byte: u8) -> Result<(), BytesWriteError> {
        if position >= self.bytes.len() {
            return Err(BytesWriteError {
                value: BytesWriteErrorValue::OutofIndex,
            });
        }
        self.bytes[position] = self.bytes[position].wrapping_add(byte);

        Ok(())
    }

    pub fn write_u8_at(&mut self, position: usize, byte: u8) -> Result<(), BytesWriteError> {
        if position >= self.bytes.len() {
            return Err(BytesWriteError {
                value: BytesWriteErrorValue::OutofIndex,
            });
        }
        self.bytes[position] = byte;

        Ok(())
    }

    pub fn get(&mut self, position: usize) -> Option<&u8> {
        self.bytes.get(position)
    }

    pub fn write_u16<T: ByteOrder>(&mut self, bytes: u16) -> Result<(), BytesWriteError> {
        self.bytes.write_u16::<T>(bytes)?;
        Ok(())
    }

    pub fn write_u24<T: ByteOrder>(&mut self, bytes: u32) -> Result<(), BytesWriteError> {
        self.bytes.write_u24::<T>(bytes)?;

        Ok(())
    }

    pub fn write_u32<T: ByteOrder>(&mut self, bytes: u32) -> Result<(), BytesWriteError> {
        self.bytes.write_u32::<T>(bytes)?;
        Ok(())
    }

    pub fn write_f64<T: ByteOrder>(&mut self, bytes: f64) -> Result<(), BytesWriteError> {
        self.bytes.write_f64::<T>(bytes)?;
        Ok(())
    }

    pub fn write(&mut self, buf: &[u8]) -> Result<(), BytesWriteError> {
        self.bytes.write_all(buf)?;
        Ok(())
    }

    pub fn write_repeat(&mut self, byte: u8, length: usize) -> Result<(), BytesWriteError> {
        let end = self
            .bytes
            .len()
            .checked_add(length)
            .ok_or(BytesWriteError {
                value: BytesWriteErrorValue::OutofIndex,
            })?;
        self.bytes.resize(end, byte);
        Ok(())
    }

    pub fn prepend(&mut self, buf: &[u8]) -> Result<(), BytesWriteError> {
        self.bytes.reserve(buf.len());
        self.bytes.splice(0..0, buf.iter().copied());
        Ok(())
    }

    pub fn append(&mut self, writer: &mut Self) {
        self.bytes.append(&mut writer.bytes);
    }

    pub fn write_random_bytes(&mut self, length: u32) -> Result<(), BytesWriteError> {
        let length = usize::try_from(length).map_err(|_| BytesWriteError {
            value: BytesWriteErrorValue::OutofIndex,
        })?;
        let start = self.bytes.len();
        let end = start.checked_add(length).ok_or(BytesWriteError {
            value: BytesWriteErrorValue::OutofIndex,
        })?;
        self.bytes.resize(end, 0);
        let mut rng = rand::rng();
        rng.fill(&mut self.bytes[start..]);
        Ok(())
    }
    pub fn extract_current_bytes(&mut self) -> BytesMut {
        BytesMut::from(Bytes::from(std::mem::take(&mut self.bytes)))
    }

    pub fn extract_current_bytes_frozen(&mut self) -> Bytes {
        Bytes::from(std::mem::take(&mut self.bytes))
    }

    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        self.bytes.as_slice()
    }

    pub fn clear(&mut self) {
        self.bytes.clear();
    }

    #[must_use]
    pub fn get_current_bytes(&self) -> BytesMut {
        let mut rv_data = BytesMut::new();
        rv_data.extend_from_slice(&self.bytes[..]);
        rv_data
    }

    #[must_use]
    pub const fn len(&self) -> usize {
        self.bytes.len()
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

pub struct AsyncBytesWriter {
    pub bytes_writer: BytesWriter,
    pub io: Arc<Mutex<Box<dyn TNetIO + Send + Sync>>>,
}

impl AsyncBytesWriter {
    pub fn new(io: Arc<Mutex<Box<dyn TNetIO + Send + Sync>>>) -> Self {
        Self {
            bytes_writer: BytesWriter::new(),
            io,
        }
    }

    pub fn write_u8(&mut self, byte: u8) -> Result<(), BytesWriteError> {
        self.bytes_writer.write_u8(byte)
    }

    pub fn write_u16<T: ByteOrder>(&mut self, bytes: u16) -> Result<(), BytesWriteError> {
        self.bytes_writer.write_u16::<T>(bytes)
    }

    pub fn write_u24<T: ByteOrder>(&mut self, bytes: u32) -> Result<(), BytesWriteError> {
        self.bytes_writer.write_u24::<T>(bytes)
    }

    pub fn write_u32<T: ByteOrder>(&mut self, bytes: u32) -> Result<(), BytesWriteError> {
        self.bytes_writer.write_u32::<T>(bytes)
    }

    pub fn write_f64<T: ByteOrder>(&mut self, bytes: f64) -> Result<(), BytesWriteError> {
        self.bytes_writer.write_f64::<T>(bytes)
    }

    pub fn write(&mut self, buf: &[u8]) -> Result<(), BytesWriteError> {
        self.bytes_writer.write(buf)
    }

    pub fn write_random_bytes(&mut self, length: u32) -> Result<(), BytesWriteError> {
        self.bytes_writer.write_random_bytes(length)
    }

    pub fn extract_current_bytes(&mut self) -> BytesMut {
        self.bytes_writer.extract_current_bytes()
    }

    pub async fn flush(&mut self) -> Result<(), BytesWriteError> {
        let data = std::mem::take(&mut self.bytes_writer.bytes);
        self.io.lock().await.write(data.into()).await?;
        Ok(())
    }

    pub async fn flush_timeout(&mut self, duration: Duration) -> Result<(), BytesWriteError> {
        let data = std::mem::take(&mut self.bytes_writer.bytes);
        timeout(duration, async {
            self.io.lock().await.write(data.into()).await
        })
        .await
        .map_err(|_| BytesWriteError {
            value: BytesWriteErrorValue::Timeout,
        })??;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    const FLV_HEADER: [u8; 9] = [
        0x46, // 'F'
        0x4c, //'L'
        0x56, //'V'
        0x01, //version
        0x05, //00000101  audio tag  and video tag
        0x00, 0x00, 0x00, 0x09, //flv header size
    ];

    #[test]
    fn test_write_vec() {
        let mut v: Vec<u8> = Vec::new();

        v.push(0x01);
        assert_eq!(1, v.len());
        assert_eq!(0x01, v[0]);

        v[0] = 0x02;
        assert_eq!(0x02, v[0]);

        let rv = v.write(&FLV_HEADER);

        assert!(rv.is_ok(), "FLV header should write to vector");
        assert_eq!(10, v.len());
    }

    #[test]
    fn test_write_repeat_appends_exact_bytes() {
        let mut writer = super::BytesWriter::new();
        writer.write_u8(0x01).unwrap();
        writer.write_repeat(0xFF, 4).unwrap();

        assert_eq!(writer.as_slice(), &[0x01, 0xFF, 0xFF, 0xFF, 0xFF]);
    }
}
