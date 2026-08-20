use {
    super::{bits_errors::BitError, bytes_writer::BytesWriter},
    bytes::BytesMut,
};

pub struct BitsWriter {
    writer: BytesWriter,
    cur_byte: u8,
    cur_bit_num: u8,
}

impl BitsWriter {
    #[must_use]
    pub const fn new(writer: BytesWriter) -> Self {
        Self {
            writer,
            cur_byte: 0,
            cur_bit_num: 0,
        }
    }

    pub fn write_bytes(&mut self, data: &BytesMut) -> Result<(), BitError> {
        self.writer.write(&data[..])?;
        Ok(())
    }

    pub fn write_bit(&mut self, b: u8) -> Result<(), BitError> {
        self.cur_byte |= b << (7 - self.cur_bit_num);
        self.cur_bit_num += 1;

        if self.cur_bit_num == 8 {
            self.writer.write_u8(self.cur_byte)?;
            self.cur_bit_num = 0;
            self.cur_byte = 0;
        }

        Ok(())
    }

    fn flush(&mut self) -> Result<(), BitError> {
        if self.cur_bit_num == 8 {
            self.writer.write_u8(self.cur_byte)?;
            self.cur_bit_num = 0;
            self.cur_byte = 0;
        } else {
            tracing::trace!("cannot flush: {}", self.cur_bit_num);
        }

        Ok(())
    }

    pub fn bits_aligment_8(&mut self) -> Result<(), BitError> {
        self.cur_bit_num = 8;
        self.flush()?;
        Ok(())
    }

    #[must_use]
    pub fn get_current_bytes(&self) -> BytesMut {
        self.writer.get_current_bytes()
    }

    #[must_use]
    pub const fn len(&self) -> usize {
        self.writer.len() * 8 + self.cur_bit_num as usize
    }
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {

    use super::BitsWriter;
    use super::BytesWriter;

    #[test]
    fn test_write_bit() {
        let bytes_writer = BytesWriter::new();
        let mut bit_writer = BitsWriter::new(bytes_writer);

        bit_writer.write_bit(0).unwrap();
        bit_writer.write_bit(0).unwrap();
        bit_writer.write_bit(0).unwrap();
        bit_writer.write_bit(0).unwrap();

        bit_writer.write_bit(0).unwrap();
        bit_writer.write_bit(0).unwrap();
        bit_writer.write_bit(1).unwrap();
        bit_writer.write_bit(0).unwrap();

        let byte = bit_writer.get_current_bytes();
        assert_eq!(byte.to_vec()[0], 0x2);

        bit_writer.write_bit(1).unwrap();
        bit_writer.write_bit(1).unwrap();

        assert_eq!(bit_writer.cur_bit_num, 2);
        assert_eq!(bit_writer.cur_byte, 0xC0); //0x11000000
    }

    #[test]
    fn test_bits_aligment_8() {
        let bytes_writer = BytesWriter::new();
        let mut bit_writer = BitsWriter::new(bytes_writer);

        bit_writer.write_bit(1).unwrap();
        bit_writer.write_bit(1).unwrap();
        bit_writer.write_bit(0).unwrap();

        bit_writer.bits_aligment_8().unwrap();

        let byte = bit_writer.get_current_bytes();
        assert_eq!(byte.to_vec()[0], 0xC0); //0x11000000

        bit_writer.write_bit(1).unwrap();
        bit_writer.write_bit(1).unwrap();
        bit_writer.write_bit(0).unwrap();

        assert_eq!(bit_writer.cur_bit_num, 3);
        assert_eq!(bit_writer.cur_byte, 0xC0); //0x11000000
    }
}
