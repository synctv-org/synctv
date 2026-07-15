mod client;
mod types;

pub use client::{CctvClient, CctvEndpoints};
pub use types::{
    CctvChapter, CctvMedia, CctvMetadata, CctvPlayback, CctvResource, CctvStream, CctvStreamKind,
};
