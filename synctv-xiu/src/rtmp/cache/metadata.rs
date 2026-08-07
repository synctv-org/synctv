use {
    crate::bytesio::bytes_reader::BytesReader,
    crate::flv::amf0::{amf0_reader::Amf0Reader, Amf0ValueType},
    bytes::{Bytes, BytesMut},
};
#[derive(Clone)]
pub struct MetaData {
    chunk_body: Bytes,
}

impl Default for MetaData {
    fn default() -> Self {
        Self::new()
    }
}

impl MetaData {
    #[must_use]
    pub fn new() -> Self {
        Self {
            chunk_body: Bytes::new(),
        }
    }

    pub fn save(&mut self, body: &Bytes) {
        if self.is_metadata(body) {
            self.chunk_body = body.clone();
        }
    }

    pub fn is_metadata(&self, body: &[u8]) -> bool {
        let reader = BytesReader::new(BytesMut::from(body));
        let result = Amf0Reader::new(reader).read_all();

        let values: Vec<Amf0ValueType> = match result {
            Ok(values) => values,
            Err(_) => return false,
        };

        if values.is_empty() {
            return false;
        }

        tracing::debug!("metadata: {values:?}");

        let first = match values.first() {
            Some(Amf0ValueType::UTF8String(s)) => s.as_str(),
            _ => return false,
        };

        match first {
            "@setDataFrame" => match values.get(1) {
                Some(Amf0ValueType::UTF8String(s)) => s == "onMetaData",
                _ => false,
            },
            "onMetaData" => true,
            _ => false,
        }
    }

    #[must_use]
    pub fn get_chunk_body(&self) -> Bytes {
        self.chunk_body.clone()
    }
}
