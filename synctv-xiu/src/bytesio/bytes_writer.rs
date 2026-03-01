use {
    super::{
        bytes_errors::{BytesWriteError, BytesWriteErrorValue},
        bytesio::TNetIO,
    },
    byteorder::{ByteOrder, WriteBytesExt},
    bytes::BytesMut,
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

    pub fn write_u64<T: ByteOrder>(&mut self, bytes: u64) -> Result<(), BytesWriteError> {
        self.bytes.write_u64::<T>(bytes)?;
        Ok(())
    }

    pub fn write(&mut self, buf: &[u8]) -> Result<(), BytesWriteError> {
        self.bytes.write_all(buf)?;
        Ok(())
    }

    pub fn prepend(&mut self, buf: &[u8]) -> Result<(), BytesWriteError> {
        let tmp_bytes = self.bytes.clone();
        self.bytes.clear();
        self.bytes.write_all(buf)?;
        self.bytes.write_all(tmp_bytes.as_slice())?;
        Ok(())
    }

    pub fn append(&mut self, writer: &mut Self) {
        self.bytes.append(&mut writer.bytes);
    }

    pub fn write_random_bytes(&mut self, length: u32) -> Result<(), BytesWriteError> {
        let mut rng = rand::rng();
        for _ in 0..length {
            self.bytes.write_u8(rng.random())?;
        }
        Ok(())
    }
    pub fn extract_current_bytes(&mut self) -> BytesMut {
        BytesMut::from(std::mem::take(&mut self.bytes).as_slice())
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

    pub fn pop_bytes(&mut self, size: usize) {
        for _ in 0..size {
            self.bytes.pop();
        }
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
        let message = timeout(duration, self.io.lock().await.write(data.into())).await;

        match message {
            Ok(_) => {}
            Err(_) => {
                return Err(BytesWriteError {
                    value: BytesWriteErrorValue::Timeout,
                })
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    #[test]
    fn test_write_vec() {
        let mut v: Vec<u8> = Vec::new();

        v.push(0x01);
        assert_eq!(1, v.len());
        assert_eq!(0x01, v[0]);

        v[0] = 0x02;
        assert_eq!(0x02, v[0]);

        const FLV_HEADER: [u8; 9] = [
            0x46, // 'F'
            0x4c, //'L'
            0x56, //'V'
            0x01, //version
            0x05, //00000101  audio tag  and video tag
            0x00, 0x00, 0x00, 0x09, //flv header size
        ];

        let rv = v.write(&FLV_HEADER);

        if let Ok(val) = rv {
            print!("{val} ");
        }

        assert_eq!(10, v.len());
    }

    #[test]
    fn test_bit_opertion() {
        let pts: i64 = 1_627_702_096;

        let val = ((pts << 1) & 0xFE) as u8;

        println!("======={}=======", pts << 1);
        println!("======={val}=======");
    }

    #[test]
    fn test_bit_opertion2() {
        let flags = 0xC0;
        let pts: i64 = 1_627_702_096;

        let b9 = ((flags >> 2) & 0x30) as u8 /* 0011/0010 */ | (((pts >> 30) & 0x07) << 1) as u8 /* PTS 30-32 */ | 0x01 /* marker_bit */;
        println!("=======b9{b9}=======");

        let b10 = (pts >> 22) as u8; /* PTS 22-29 */
        println!("=======b10{b10}=======");

        let b11 = ((pts >> 14) & 0xFE) as u8 /* PTS 15-21 */ | 0x01; /* marker_bit */
        println!("=======b11{b11}=======");

        let b12 = (pts >> 7) as u8; /* PTS 7-14 */
        println!("=======b12{b12}=======");

        let b13 = ((pts << 1) & 0xFE) as u8 /* PTS 0-6 */ | 0x01; /* marker_bit */
        println!("=======b13{b13}=======");
    }

    #[test]
    fn test_bit_opertion3() {
        //let flags = 0xC0;
        let pts: i64 = 1_627_702_096;

        let b12 = ((pts & 0x7fff) << 1) | 1; /* PTS 7-14 */
        println!("=======b12{}=======", b12 >> 8_u8);
        println!("=======b13{}=======", b12 as u8);
    }
}
