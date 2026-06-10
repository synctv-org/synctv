use {
    super::{
        define,
        define::SchemaVersion,
        errors::{DigestError, DigestErrorValue},
    },
    crate::bytesio::bytes_reader::BytesReader,
    bytes::BytesMut,
    hmac::{Hmac, KeyInit, Mac},
    sha2::Sha256,
};

pub struct DigestProcessor {
    reader: BytesReader,
    key: BytesMut,
}

struct HandshakeDigestParts {
    bytes_before_digest: BytesMut,
    digest: BytesMut,
    bytes_after_digest: BytesMut,
}

impl DigestProcessor {
    #[must_use]
    pub const fn new(data: BytesMut, key: BytesMut) -> Self {
        Self {
            reader: BytesReader::new(data),
            key,
        }
    }

    pub fn read_digest(&mut self) -> Result<(BytesMut, SchemaVersion), DigestError> {
        if let Ok(digest) = self.generate_and_validate(SchemaVersion::Schema0) {
            return Ok((digest, SchemaVersion::Schema0));
        }

        let digest = self.generate_and_validate(SchemaVersion::Schema1)?;
        Ok((digest, SchemaVersion::Schema1))
    }

    pub fn generate_and_fill_digest(&mut self) -> Result<Vec<u8>, DigestError> {
        let parts = self.cook_raw_message(SchemaVersion::Schema0)?;
        let raw_message = [
            parts.bytes_before_digest.clone(),
            parts.bytes_after_digest.clone(),
        ]
        .concat();
        let computed_digest = self.make_digest(&raw_message)?;

        let result = [
            parts.bytes_before_digest,
            computed_digest,
            parts.bytes_after_digest,
        ]
        .concat();

        Ok(result)
    }

    fn find_digest_offset(&self, version: &SchemaVersion) -> Result<usize, DigestError> {
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
        }

        Ok(digest_offset)
    }
    fn cook_raw_message(
        &mut self,
        version: SchemaVersion,
    ) -> Result<HandshakeDigestParts, DigestError> {
        let digest_offset: usize = self.find_digest_offset(&version)?;

        let mut new_reader = BytesReader::new(self.reader.get_remaining_bytes());

        let bytes_before_digest = new_reader.read_bytes(digest_offset)?;
        let digest = new_reader.read_bytes(define::RTMP_DIGEST_LENGTH)?;
        let bytes_after_digest = new_reader.extract_remaining_bytes();

        Ok(HandshakeDigestParts {
            bytes_before_digest,
            digest,
            bytes_after_digest,
        })
    }

    pub fn make_digest(&self, raw_message: &[u8]) -> Result<BytesMut, DigestError> {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.key[..]).map_err(|_| DigestError {
            value: DigestErrorValue::HmacInitError,
        })?;
        mac.update(raw_message);
        let result = mac.finalize().into_bytes();

        if result.len() != define::RTMP_DIGEST_LENGTH {
            return Err(DigestError {
                value: DigestErrorValue::InvalidDigestLength,
            });
        }

        let mut rv = BytesMut::new();
        rv.extend_from_slice(result.as_slice());

        Ok(rv)
    }

    fn generate_and_validate(&mut self, version: SchemaVersion) -> Result<BytesMut, DigestError> {
        let parts = self.cook_raw_message(version)?;
        let raw_message = [parts.bytes_before_digest, parts.bytes_after_digest].concat();

        let computed_digest = self.make_digest(&raw_message)?;

        if parts.digest == computed_digest {
            return Ok(parts.digest);
        }

        Err(DigestError {
            value: DigestErrorValue::CannotGenerate,
        })
    }
}
