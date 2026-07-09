//! Internal boundary for upstream provider transport DTOs.
//!
//! Provider adapters keep business-facing DTOs in their own modules. Conversion
//! to upstream transport shapes is isolated behind this private module.

pub(crate) use synctv_media_providers::transport_dto::{alist, bilibili, emby};
