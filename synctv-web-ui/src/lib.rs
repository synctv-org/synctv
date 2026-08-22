#[derive(Clone, Copy, Debug)]
pub struct Asset {
    pub path: &'static str,
    pub content_type: &'static str,
    pub etag: &'static str,
    pub bytes: &'static [u8],
    pub brotli: Option<EncodedAsset>,
    pub gzip: Option<EncodedAsset>,
}

#[derive(Clone, Copy, Debug)]
pub struct EncodedAsset {
    pub etag: &'static str,
    pub bytes: &'static [u8],
}

include!(concat!(env!("OUT_DIR"), "/web_assets.rs"));

#[cfg(test)]
#[path = "build_support.rs"]
mod build_support;
