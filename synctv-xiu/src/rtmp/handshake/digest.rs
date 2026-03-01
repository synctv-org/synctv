use {
    super::{
        define,
        define::SchemaVersion,
        errors::{DigestError, DigestErrorValue},
    },
    crate::bytesio::bytes_reader::BytesReader,
    bytes::BytesMut,
    hmac::{Hmac, Mac},
    sha2::Sha256,
};

pub struct DigestProcessor {
    reader: BytesReader,
    key: BytesMut,
}

impl DigestProcessor {
    #[must_use]
    pub const fn new(data: BytesMut, key: BytesMut) -> Self {
        Self {
            reader: BytesReader::new(data),
            key,
        }
    }

    /* return validate digest and schema version*/
    pub fn read_digest(&mut self) -> Result<(BytesMut, SchemaVersion), DigestError> {
        if let Ok(digest) = self.generate_and_validate(SchemaVersion::Schema0) {
            return Ok((digest, SchemaVersion::Schema0));
        }

        let digest = self.generate_and_validate(SchemaVersion::Schema1)?;
        Ok((digest, SchemaVersion::Schema1))
    }

    pub fn generate_and_fill_digest(&mut self) -> Result<Vec<u8>, DigestError> {
        let (left_part, _, right_part) = self.cook_raw_message(SchemaVersion::Schema0)?;
        let raw_message = [left_part.clone(), right_part.clone()].concat();
        let computed_digest = self.make_digest(raw_message)?;

        let result = [left_part, computed_digest, right_part].concat();

        Ok(result)
    }

    pub fn generate_digest(&mut self) -> Result<BytesMut, DigestError> {
        let (left_part, _, right_part) = self.cook_raw_message(SchemaVersion::Schema0)?;
        let raw_message = [left_part, right_part].concat();
        let digest = self.make_digest(raw_message)?;

        Ok(digest)
    }

    fn find_digest_offset(&mut self, version: SchemaVersion) -> Result<usize, DigestError> {
        let mut digest_offset: usize = 0;

        match version {
            SchemaVersion::Schema0 => {
                digest_offset += self.reader.get(772)? as usize;
                digest_offset += self.reader.get(773)? as usize;
                digest_offset += self.reader.get(774)? as usize;
                digest_offset += self.reader.get(775)? as usize;

                digest_offset %= 728;
                digest_offset += 776;
            }
            SchemaVersion::Schema1 => {
                digest_offset += self.reader.get(8)? as usize;
                digest_offset += self.reader.get(9)? as usize;
                digest_offset += self.reader.get(10)? as usize;
                digest_offset += self.reader.get(11)? as usize;

                digest_offset %= 728;
                digest_offset += 12;
            }
            SchemaVersion::Unknown => {
                return Err(DigestError {
                    value: DigestErrorValue::UnknowSchema,
                });
            }
        }

        Ok(digest_offset)
    }
    /*
      +-----------------------------------------------------------+
      |                     764 bytes                             |
    * +--------------+-----------------------+--------------------+
    * |   left part  | digest data (32 bytes)|     right part     |
    * +--------------+-----------------------+--------------------+
    *                |
                     /
                     digest offset
        pice together the left part and right part to get the raw message.
     */
    fn cook_raw_message(
        &mut self,
        version: SchemaVersion,
    ) -> Result<(BytesMut, BytesMut, BytesMut), DigestError> {
        let digest_offset: usize = self.find_digest_offset(version)?;

        let mut new_reader = BytesReader::new(self.reader.get_remaining_bytes());

        let left_part = new_reader.read_bytes(digest_offset)?;
        let digest_data = new_reader.read_bytes(define::RTMP_DIGEST_LENGTH)?;
        let right_part = new_reader.extract_remaining_bytes();

        Ok((left_part, digest_data, right_part))
    }
    pub fn make_digest(&mut self, raw_message: Vec<u8>) -> Result<BytesMut, DigestError> {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.key[..]).map_err(|_| DigestError {
            value: DigestErrorValue::HmacInitError,
        })?;
        mac.update(&raw_message);
        let result = mac.finalize().into_bytes();

        if result.len() != define::RTMP_DIGEST_LENGTH {
            return Err(DigestError {
                value: DigestErrorValue::DigestLengthNotCorrect,
            });
        }

        let mut rv = BytesMut::new();
        rv.extend_from_slice(result.as_slice());

        Ok(rv)
    }

    fn generate_and_validate(&mut self, version: SchemaVersion) -> Result<BytesMut, DigestError> {
        let (left_part, digest_data, right_part) = self.cook_raw_message(version)?;
        let raw_message = [left_part, right_part].concat();

        let computed_digest = self.make_digest(raw_message)?;

        if digest_data == computed_digest {
            return Ok(digest_data);
        }

        Err(DigestError {
            value: DigestErrorValue::CannotGenerate,
        })
    }
}
