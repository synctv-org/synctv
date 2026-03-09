use serde::{Serialize, Serializer};
use std::fmt;

#[derive(Debug, Clone, Eq, PartialEq, Hash, Copy)]
pub struct Uuid(uuid::Uuid);

impl Default for Uuid {
    fn default() -> Self {
        Self(uuid::Uuid::nil())
    }
}

impl Serialize for Uuid {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl Uuid {
    #[must_use]
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

impl fmt::Display for Uuid {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::Uuid;

    #[test]
    fn test_uuid() {
        let id = Uuid::new();
        let s = id.to_string();
        let serialized = serde_json::to_string(&id).expect("Uuid should always serialize to JSON");
        assert!(!s.is_empty());
        assert!(serialized.contains(&s));

        // Ensure uniqueness
        let id2 = Uuid::new();
        assert_ne!(id, id2);
    }
}
