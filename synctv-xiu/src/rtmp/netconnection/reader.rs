use {crate::bytesio::bytes_reader::BytesReader, crate::flv::amf0::amf0_reader::Amf0Reader};

#[allow(dead_code)]
pub struct NetConnectionReader {
    reader: BytesReader,
    amf0_reader: Amf0Reader,
}

impl NetConnectionReader {
    #[allow(dead_code)]
    const fn onconnect(&self) {}
}
