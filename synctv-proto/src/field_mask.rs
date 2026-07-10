use std::fmt;

use serde::{de::Visitor, Deserialize, Deserializer, Serialize, Serializer};

#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct FieldMask {
    #[prost(string, repeated, tag = "1")]
    pub paths: Vec<String>,
}

impl Serialize for FieldMask {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        use serde::ser::Error as _;

        let paths = self
            .paths
            .iter()
            .map(|path| path_to_json(path).map_err(S::Error::custom))
            .collect::<Result<Vec<_>, _>>()?;
        serializer.serialize_str(&paths.join(","))
    }
}

impl<'de> Deserialize<'de> for FieldMask {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_str(FieldMaskVisitor)
    }
}

struct FieldMaskVisitor;

impl Visitor<'_> for FieldMaskVisitor {
    type Value = FieldMask;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a comma-separated lowerCamel FieldMask string")
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        if value.contains('_') {
            return Err(E::custom("FieldMask JSON paths cannot contain underscores"));
        }
        let paths = if value.is_empty() {
            Vec::new()
        } else {
            value.split(',').map(path_from_json).collect()
        };
        Ok(FieldMask { paths })
    }
}

fn path_to_json(path: &str) -> Result<String, &'static str> {
    let mut output = String::with_capacity(path.len());
    let mut chars = path.chars();
    while let Some(character) = chars.next() {
        if character.is_ascii_uppercase() {
            return Err("FieldMask paths must use canonical snake_case");
        }
        if character == '_' {
            let next = chars
                .next()
                .filter(char::is_ascii_lowercase)
                .ok_or("FieldMask path cannot round-trip through ProtoJSON")?;
            output.push(next.to_ascii_uppercase());
        } else {
            output.push(character);
        }
    }
    Ok(output)
}

fn path_from_json(path: &str) -> String {
    let mut output = String::with_capacity(path.len());
    for character in path.chars() {
        if character.is_ascii_uppercase() {
            output.push('_');
            output.push(character.to_ascii_lowercase());
        } else {
            output.push(character);
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_mask_uses_canonical_proto_json() {
        let mask = FieldMask {
            paths: vec![
                "email.smtp_proxy".to_string(),
                "room_creation.max_rooms_per_user".to_string(),
            ],
        };
        let json = serde_json::to_string(&mask).expect("FieldMask should serialize");
        assert_eq!(json, r#""email.smtpProxy,roomCreation.maxRoomsPerUser""#);

        let decoded: FieldMask = serde_json::from_str(&json).expect("FieldMask should deserialize");
        assert_eq!(decoded, mask);
    }

    #[test]
    fn field_mask_rejects_noncanonical_json() {
        let error = serde_json::from_str::<FieldMask>(r#""email.smtp_proxy""#)
            .expect_err("snake_case JSON path should reject");
        assert!(error.to_string().contains("cannot contain underscores"));
    }
}
