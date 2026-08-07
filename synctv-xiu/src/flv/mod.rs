pub mod amf0;
pub mod define;
pub mod demuxer;
pub mod errors;
pub mod flv_tag_header;
pub mod mpeg4_aac;
pub mod mpeg4_avc;
pub mod mpeg4_hevc;
pub mod muxer;

pub trait Unmarshal<T1, T2> {
    fn unmarshal(reader: T1) -> T2
    where
        Self: Sized;
}
pub trait Marshal<T> {
    fn marshal(&self) -> T;
}
