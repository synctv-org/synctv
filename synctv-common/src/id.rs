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
    use super::{generate, BASE62_ALPHABET, DEFAULT_LENGTH};

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
    fn snanoid_macro_uses_shared_base62_generator() {
        let short = crate::snanoid!(12);
        let default = crate::snanoid!();

        assert_eq!(short.len(), 12);
        assert_eq!(default.len(), DEFAULT_LENGTH);
        assert!(short.chars().all(|c| BASE62_ALPHABET.contains(&c)));
        assert!(default.chars().all(|c| BASE62_ALPHABET.contains(&c)));
        assert!(!short.contains('-'));
        assert!(!short.contains('_'));
        assert!(!default.contains('-'));
        assert!(!default.contains('_'));
    }
}
