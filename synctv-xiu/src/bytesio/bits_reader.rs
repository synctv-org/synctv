use {
    super::bits_errors::{BitError, BitErrorValue},
    super::bytes_reader::BytesReader,
    bytes::BytesMut,
};

pub struct BitsReader {
    reader: BytesReader,
    cur_byte: u8,
    cur_bit_left: u8,
}

impl BitsReader {
    #[must_use]
    pub const fn new(reader: BytesReader) -> Self {
        Self {
            reader,
            cur_byte: 0,
            cur_bit_left: 0,
        }
    }

    pub fn extend_data(&mut self, bytes: &BytesMut) -> Result<(), BitError> {
        self.reader.extend_from_slice(&bytes[..])?;
        Ok(())
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.reader.len() * 8 + self.cur_bit_left as usize
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn read_byte(&mut self) -> Result<u8, BitError> {
        if self.cur_bit_left != 0 {
            return Err(BitError {
                value: BitErrorValue::CannotReadByte,
            });
        }

        let byte = self.reader.read_u8()?;
        Ok(byte)
    }

    pub fn read_bit(&mut self) -> Result<u8, BitError> {
        if self.cur_bit_left == 0 {
            self.cur_byte = self.reader.read_u8()?;
            self.cur_bit_left = 8;
        }
        self.cur_bit_left -= 1;
        Ok((self.cur_byte >> self.cur_bit_left) & 0x01)
    }

    pub fn read_n_bits(&mut self, n: usize) -> Result<u64, BitError> {
        let mut result: u64 = 0;
        for _ in 0..n {
            result <<= 1;
            let cur_bit = self.read_bit()?;
            result |= u64::from(cur_bit);
        }
        Ok(result)
    }

    pub const fn bits_aligment_8(&mut self) {
        self.cur_bit_left = 0;
    }
}

#[cfg(test)]
mod tests {

    use super::BitsReader;
    use super::BytesReader;
    use bytes::BytesMut;

    #[test]
    fn test_read_bit() {
        let mut bytes_reader = BytesReader::new(BytesMut::new());

        let data_0 = 2u8;
        bytes_reader.extend_from_slice(&[data_0]).unwrap();
        let data_1 = 7u8;
        bytes_reader.extend_from_slice(&[data_1]).unwrap();

        let mut bit_reader = BitsReader::new(bytes_reader);

        assert!(bit_reader.read_bit().unwrap() == 0);
        assert!(bit_reader.read_bit().unwrap() == 0);
        assert!(bit_reader.read_bit().unwrap() == 0);
        assert!(bit_reader.read_bit().unwrap() == 0);

        assert!(bit_reader.read_bit().unwrap() == 0);
        assert!(bit_reader.read_bit().unwrap() == 0);
        assert!(bit_reader.read_bit().unwrap() == 1);
        assert!(bit_reader.read_bit().unwrap() == 0);

        assert!(bit_reader.read_bit().unwrap() == 0);
        assert!(bit_reader.read_bit().unwrap() == 0);
        assert!(bit_reader.read_bit().unwrap() == 0);
        assert!(bit_reader.read_bit().unwrap() == 0);

        assert!(bit_reader.read_bit().unwrap() == 0);
        assert!(bit_reader.read_bit().unwrap() == 1);
        assert!(bit_reader.read_bit().unwrap() == 1);
        assert!(bit_reader.read_bit().unwrap() == 1);
    }
    #[test]
    fn test_read_n_bits() {
        let mut bytes_reader = BytesReader::new(BytesMut::new());

        let data_0 = 2u8;
        bytes_reader.extend_from_slice(&[data_0]).unwrap();
        let data_1 = 7u8;
        bytes_reader.extend_from_slice(&[data_1]).unwrap();
        bytes_reader.extend_from_slice(&[0b0000_0010]).unwrap();

        let mut bit_reader = BitsReader::new(bytes_reader);
        assert!(bit_reader.read_n_bits(16).unwrap() == 0x207);

        assert!(bit_reader.read_n_bits(5).unwrap() == 0);

        assert!(bit_reader.read_n_bits(3).unwrap() == 2);
    }

    #[test]
    fn test_bits_aligment_8() {
        let mut bytes_reader = BytesReader::new(BytesMut::new());
        let data_0 = 2u8;
        bytes_reader.extend_from_slice(&[data_0]).unwrap();
        let data_1 = 7u8;
        bytes_reader.extend_from_slice(&[data_1]).unwrap();

        let mut bit_reader = BitsReader::new(bytes_reader);

        assert!(bit_reader.read_bit().unwrap() == 0);
        assert!(bit_reader.read_bit().unwrap() == 0);
        assert!(bit_reader.read_bit().unwrap() == 0);
        assert!(bit_reader.read_bit().unwrap() == 0);

        bit_reader.bits_aligment_8();

        assert!(bit_reader.read_bit().unwrap() == 0);
        assert!(bit_reader.read_bit().unwrap() == 0);
        assert!(bit_reader.read_bit().unwrap() == 0);
        assert!(bit_reader.read_bit().unwrap() == 0);

        assert!(bit_reader.read_bit().unwrap() == 0);
        assert!(bit_reader.read_bit().unwrap() == 1);
        assert!(bit_reader.read_bit().unwrap() == 1);
        assert!(bit_reader.read_bit().unwrap() == 1);
    }
}
