use nanoid::nanoid;

pub const DEFAULT_LENGTH: usize = 21;
pub const BASE62_ALPHABET: [char; 62] = [
    '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'I',
    'J', 'K', 'L', 'M', 'N', 'O', 'P', 'Q', 'R', 'S', 'T', 'U', 'V', 'W', 'X', 'Y', 'Z', 'a', 'b',
    'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j', 'k', 'l', 'm', 'n', 'o', 'p', 'q', 'r', 's', 't', 'u',
    'v', 'w', 'x', 'y', 'z',
];

#[must_use]
pub fn generate(size: usize) -> String {
    nanoid!(size, &BASE62_ALPHABET)
}

#[must_use]
pub fn generate_default() -> String {
    generate(DEFAULT_LENGTH)
}

#[must_use]
pub fn is_valid(value: &str) -> bool {
    !value.is_empty() && value.chars().all(|c| c.is_ascii_alphanumeric())
}

#[must_use]
pub fn is_valid_with_len(value: &str, len: usize) -> bool {
    value.len() == len && is_valid(value)
}

#[macro_export]
macro_rules! snanoid {
    () => {
        $crate::id::generate_default()
    };
    ($size:expr) => {
        $crate::id::generate($size)
    };
}

#[cfg(test)]
mod tests {
    use super::{generate, is_valid, is_valid_with_len, BASE62_ALPHABET, DEFAULT_LENGTH};

    #[test]
    fn generated_ids_only_use_base62() {
        for len in [1, 8, 12, 16, 21, 32, 64] {
            let id = generate(len);
            assert_eq!(id.len(), len);
            assert!(id.chars().all(|c| BASE62_ALPHABET.contains(&c)));
            assert!(!id.contains('-'));
            assert!(!id.contains('_'));
        }
    }

    #[test]
    fn validation_rejects_non_base62_characters() {
        assert!(is_valid("AbC123"));
        assert!(!is_valid("abc-123"));
        assert!(!is_valid("abc_123"));
        assert!(!is_valid(""));
    }

    #[test]
    fn snanoid_macro_uses_shared_base62_generator() {
        let short = crate::snanoid!(12);
        let default = crate::snanoid!();

        assert_eq!(short.len(), 12);
        assert_eq!(default.len(), DEFAULT_LENGTH);
        assert!(is_valid(&short));
        assert!(is_valid(&default));
        assert!(!short.contains('-'));
        assert!(!short.contains('_'));
        assert!(!default.contains('-'));
        assert!(!default.contains('_'));
    }

    #[test]
    fn validation_can_require_specific_length() {
        assert!(is_valid_with_len("AbC123", 6));
        assert!(!is_valid_with_len("AbC123", 7));
        assert!(!is_valid_with_len("AbC_23", 6));
        assert!(!is_valid_with_len("", 0));
    }
}
