#[cfg(all(feature = "tls-aws-lc", feature = "tls-ring"))]
compile_error!("features \"tls-aws-lc\" and \"tls-ring\" are mutually exclusive - use only one");

pub mod error;
pub mod fanout;
pub mod grpc;
pub mod sync;

pub use error::{Error, Result};
