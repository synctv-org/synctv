use {
    crate::bytesio::bytes_reader::BytesReader,
    crate::flv::amf0::{amf0_reader::Amf0Reader, Amf0ValueType},
    bytes::BytesMut,
};
#[derive(Clone)]
pub struct MetaData {
    chunk_body: BytesMut,
    // values: Vec<Amf0ValueType>,
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
            chunk_body: BytesMut::new(),
            //values: Vec::new(),
        }
    }
    //, values: Vec<Amf0ValueType>
    pub fn save(&mut self, body: &BytesMut) {
        if self.is_metadata(body) {
            self.chunk_body = body.clone();
        }
    }

    pub fn is_metadata(&mut self, body: &BytesMut) -> bool {
        let reader = BytesReader::new(body.clone());
        let result = Amf0Reader::new(reader).read_all();

        let mut values: Vec<Amf0ValueType> = Vec::new();

        match result {
            Ok(v) => {
                values.extend_from_slice(&v[..]);
            }
            Err(_) => return false,
        }

        if values.is_empty() {
            return false;
        }

        tracing::debug!("metadata: {values:?}");

        let first = match &values[0] {
            Amf0ValueType::UTF8String(s) => s.as_str(),
            _ => return false,
        };

        // RTMP metadata can be:
        // 1. "@setDataFrame" followed by "onMetaData" (2 strings minimum)
        // 2. "onMetaData" alone
        match first {
            "@setDataFrame" => {
                // Must have at least 2 values and second must be "onMetaData"
                if values.len() < 2 {
                    return false;
                }
                match &values[1] {
                    Amf0ValueType::UTF8String(s) => s == "onMetaData",
                    _ => false,
                }
            }
            "onMetaData" => true,
            _ => false,
        }
    }

    #[must_use]
    pub fn get_chunk_body(&self) -> BytesMut {
        self.chunk_body.clone()
    }
}
