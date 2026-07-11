use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(
    tag = "provider",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ProviderTarget {
    Alist(AlistTarget),
    Emby(EmbyTarget),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AlistTarget {
    pub relative_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EmbyTarget {
    pub item_id: String,
}

impl ProviderTarget {
    #[must_use]
    pub fn alist(relative_path: String) -> Self {
        Self::Alist(AlistTarget { relative_path })
    }

    #[must_use]
    pub fn emby(item_id: String) -> Self {
        Self::Emby(EmbyTarget { item_id })
    }

    pub fn stable_bytes(&self) -> crate::Result<Vec<u8>> {
        fn push_field(bytes: &mut Vec<u8>, value: &str) -> crate::Result<()> {
            let value = value.as_bytes();
            let len = u32::try_from(value.len()).map_err(|_| {
                crate::Error::InvalidInput("provider target field exceeds u32::MAX".to_string())
            })?;
            bytes.extend_from_slice(&len.to_be_bytes());
            bytes.extend_from_slice(value);
            Ok(())
        }

        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"synctv.provider_target.v1\0");
        match self {
            Self::Alist(target) => {
                bytes.push(1);
                push_field(&mut bytes, &target.relative_path)?;
            }
            Self::Emby(target) => {
                bytes.push(2);
                push_field(&mut bytes, &target.item_id)?;
            }
        }
        Ok(bytes)
    }

    pub fn hash(&self) -> crate::Result<String> {
        Ok(hex::encode(Sha256::digest(self.stable_bytes()?)))
    }
}

pub fn hash_optional_provider_target(target: Option<&ProviderTarget>) -> crate::Result<String> {
    target.map_or_else(|| Ok(hash_empty_provider_target()), ProviderTarget::hash)
}

#[must_use]
pub fn hash_empty_provider_target() -> String {
    hex::encode(Sha256::digest([]))
}
