use serde::Serialize;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimestampedL2Envelope<T> {
    pub payload: T,
    pub updated_at_ms: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionedL2Envelope<T> {
    pub payload: T,
    pub cache_version: i64,
}

#[derive(Debug, Serialize)]
pub struct UnversionedL2Envelope<T> {
    pub payload: T,
}

pub fn timestamped_l2_envelope<T: Serialize>(payload: T, updated_at_ms: i64) -> String {
    serde_json::to_string(&TimestampedL2Envelope {
        payload,
        updated_at_ms,
    })
    .expect("timestamped L2 envelope should serialize")
}

pub fn versioned_l2_envelope<T: Serialize>(payload: T, cache_version: i64) -> String {
    serde_json::to_string(&VersionedL2Envelope {
        payload,
        cache_version,
    })
    .expect("versioned L2 envelope should serialize")
}

pub fn unversioned_l2_envelope<T: Serialize>(payload: T) -> String {
    serde_json::to_string(&UnversionedL2Envelope { payload })
        .expect("unversioned L2 envelope should serialize")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Serialize)]
    struct Payload {
        name: &'static str,
    }

    #[test]
    fn typed_l2_envelopes_serialize_canonical_fields() {
        let timestamped = timestamped_l2_envelope(Payload { name: "fresh" }, 123);
        let timestamped: serde_json::Value =
            serde_json::from_str(&timestamped).expect("timestamped envelope should parse");
        assert_eq!(timestamped["payload"]["name"], "fresh");
        assert_eq!(timestamped["updatedAtMs"], 123);

        let versioned = versioned_l2_envelope(Payload { name: "v2" }, 2);
        let versioned: serde_json::Value =
            serde_json::from_str(&versioned).expect("versioned envelope should parse");
        assert_eq!(versioned["payload"]["name"], "v2");
        assert_eq!(versioned["cacheVersion"], 2);
    }
}
