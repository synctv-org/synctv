#![allow(clippy::unwrap_used)]

use synctv_core::validation::UsernameValidator;

const RESERVED_USERNAMES: &[&str] = &[
    "admin",
    "administrator",
    "system",
    "root",
    "moderator",
    "mod",
    "support",
    "help",
    "official",
    "staff",
    "owner",
    "bot",
    "service",
    "sysop",
    "operator",
    "dev",
    "developer",
    "security",
    "team",
];

#[test]
fn test_reserved_usernames_are_rejected_case_insensitively() {
    let validator = UsernameValidator::new();

    for reserved in RESERVED_USERNAMES {
        for username in [
            reserved.to_string(),
            reserved.to_uppercase(),
            alternating_case(reserved),
        ] {
            let error = validator
                .validate(&username)
                .expect_err("reserved username must be rejected");
            assert!(
                error.to_string().contains("reserved"),
                "unexpected error for {username:?}: {error}"
            );
        }
    }
}

#[test]
fn test_non_reserved_usernames_are_accepted() {
    let validator = UsernameValidator::new();

    for username in [
        "john",
        "alice",
        "user123",
        "john_doe",
        "john-doe",
        "administratorx",
        "myadmin",
        "root_user",
    ] {
        validator
            .validate(username)
            .unwrap_or_else(|error| panic!("{username:?} should be valid: {error}"));
    }
}

fn alternating_case(input: &str) -> String {
    input
        .chars()
        .enumerate()
        .map(|(index, ch)| {
            if index % 2 == 0 {
                ch.to_ascii_uppercase()
            } else {
                ch
            }
        })
        .collect()
}
