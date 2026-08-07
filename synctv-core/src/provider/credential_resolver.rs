//! Credential Resolver
//!
//! Resolves provider credentials from the database without storing credential
//! owner references in media `source_config`.

use crate::models::ProviderCredential;

#[derive(Debug, Clone)]
pub struct ResolvedProviderCredential {
    pub credential: ProviderCredential,
    /// Opaque credential version suitable for cache partitioning.
    ///
    /// The row id protects against same-timestamp replacements and the
    /// microsecond timestamp changes on normal updates.
    pub revision: String,
}

#[must_use]
pub fn credential_revision(id: i64, updated_at: chrono::DateTime<chrono::Utc>) -> String {
    format!("{id}:{}", updated_at.timestamp_micros())
}

#[cfg(test)]
mod tests {
    use crate::test_helpers::some;

    use chrono::{TimeZone, Utc};

    #[test]
    fn credential_revision_includes_row_id_and_microsecond_timestamp() {
        let updated_at = some(
            Utc.timestamp_opt(1_700_000_000, 123_456_000).single(),
            "valid timestamp",
        );

        assert_eq!(
            super::credential_revision(42, updated_at),
            "42:1700000000123456"
        );
    }
}
