mod client;
mod types;

pub use client::TrueNasClient;
pub use types::{
    TrueNasDownloadTicket, TrueNasFileItem, TrueNasFileStat, TrueNasList, TrueNasSystemInfo,
};
