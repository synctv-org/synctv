use {
    super::{
        bits_errors::{BitError, BitErrorValue},
        bytes_writer::BytesWriter,
    },
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

    pub fn write_8bit(&mut self, b: u8) -> Result<(), BitError> {
        if self.cur_bit_num != 0 {
            return Err(BitError {
                value: BitErrorValue::CannotWrite8Bit,
            });
        }

        self.writer.write_u8(b)?;
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

    // 0x02 4
    pub fn write_n_bits(&mut self, data: u64, bit_num: usize) -> Result<(), BitError> {
        if bit_num > 64 {
            return Err(BitError {
                value: BitErrorValue::TooBig,
            });
        }
        let mut bit_num_mut = bit_num;
        let mut data_mut = data;

        //read left bits  for current byte
        data_mut <<= 64 - bit_num;
        self.cur_byte |= (data_mut >> (56 + self.cur_bit_num)) as u8;

        let cur_byte_left_bit_num = 8 - self.cur_bit_num as usize;
        if bit_num_mut >= cur_byte_left_bit_num {
            // the bits for current byte is full, then flush
            data_mut <<= cur_byte_left_bit_num;
            bit_num_mut -= cur_byte_left_bit_num;
            self.cur_bit_num = 8;
            self.flush()?;
        } else {
            // not full, only update bit num
            self.cur_bit_num += bit_num_mut as u8;
            return Ok(());
        }

        while bit_num_mut > 0 {
            self.cur_byte = (data_mut >> 56) as u8;

            if bit_num_mut > 8 {
                self.cur_bit_num = 8;
                self.flush()?;
                data_mut <<= 8;
                bit_num_mut -= 8;
            } else {
                self.cur_bit_num = bit_num_mut as u8;
                break;
            }
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
        assert!(byte.to_vec()[0] == 0x2);

        bit_writer.write_bit(1).unwrap();
        bit_writer.write_bit(1).unwrap();

        println!("=={}=={}==", bit_writer.cur_bit_num, bit_writer.cur_byte);
        assert!(bit_writer.cur_bit_num == 2);
        assert!(bit_writer.cur_byte == 0xC0); //0x11000000
    }

    #[test]
    fn test_write_n_bits() {
        let bytes_writer = BytesWriter::new();
        let mut bit_writer = BitsWriter::new(bytes_writer);

        bit_writer.write_bit(1).unwrap();
        bit_writer.write_bit(1).unwrap();
        bit_writer.write_bit(0).unwrap();

        bit_writer.write_n_bits(0x03, 7).unwrap();

        let byte = bit_writer.get_current_bytes();

        //0x11000000 0x11

        println!("=={}=={}==", bit_writer.cur_bit_num, bit_writer.cur_byte);
        println!("=={}==", byte.to_vec()[0]);

        assert!(byte.to_vec()[0] == 0xC0); //0x11000000

        assert!(bit_writer.cur_bit_num == 2);
        assert!(bit_writer.cur_byte == 0xC0); //0x11000000
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
        assert!(byte.to_vec()[0] == 0xC0); //0x11000000

        bit_writer.write_bit(1).unwrap();
        bit_writer.write_bit(1).unwrap();
        bit_writer.write_bit(0).unwrap();

        assert!(bit_writer.cur_bit_num == 3);
        assert!(bit_writer.cur_byte == 0xC0); //0x11000000
    }
}
