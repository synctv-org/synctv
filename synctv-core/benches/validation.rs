//! CPU benchmarks for input validation (`synctv_core::validation`) and the
//! path-traversal validator it delegates to (`synctv_common::validation`).
//!
//! Run with: cargo bench -p synctv-core --bench validation
//!
//! Coverage matrix: small/medium/large inputs, ASCII vs CJK (multi-byte)
//! shapes, worst-case scans (failure at the end of the input), and
//! adversarial inputs (HTML markup, URL-encoded traversal, percent-heavy
//! safe paths). All inputs are deterministic constants.

#![allow(clippy::unwrap_used)]

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use synctv_common::validation::validate_path_for_traversal;
use synctv_core::validation::{
    validate_media_name_input, validate_room_description_input, validate_room_name_input,
    validate_room_password_for_set, EmailValidator, PasswordValidator, RoomNameValidator,
    UsernameValidator, Validator,
};

fn cjk(n: usize) -> String {
    "\u{4e00}".repeat(n)
}

/// `<b>x</b>` repeated until roughly `target_bytes` characters.
fn html_adversarial(target_chars: usize) -> String {
    let unit = "<b>x</b>";
    let units = target_chars / unit.chars().count();
    let mut s = unit.repeat(units);
    while s.chars().count() < target_chars {
        s.push('x');
    }
    s
}

/// Benign text that still contains `<`, forcing the HTML-inspection path.
fn lt_benign_adversarial() -> String {
    let unit = "a < b & c > d | ";
    unit.repeat(6)
}

fn bench_username(c: &mut Criterion) {
    let validator = UsernameValidator::new();
    let short = "alice";
    let max_ascii = "a".repeat(50);
    let max_cjk = cjk(50);
    let len51 = "a".repeat(51);
    let charset_fail_late = format!("{}{}", "a".repeat(49), '@');
    let reserved_case = "AdMiN";

    let mut group = c.benchmark_group("username/validate");
    for (id, input) in [
        ("short", short),
        ("max50_ascii", max_ascii.as_str()),
        ("max50_cjk", max_cjk.as_str()),
        ("len51_fail", len51.as_str()),
        ("charset_fail_late", charset_fail_late.as_str()),
        ("reserved_case", reserved_case),
    ] {
        group.bench_with_input(BenchmarkId::new("validate", id), input, |b, input| {
            b.iter(|| {
                let result = validator.validate(black_box(input));
                let _ = black_box(result);
            });
        });
    }
    group.finish();
}

fn bench_password(c: &mut Criterion) {
    let validator = PasswordValidator::new();
    let typical = "Password123";
    let max_ascii = "Ab1".repeat(42) + "Ab"; // 128 chars, no long repeats
    let max_cjk = format!("{}Aa1", cjk(125)); // 128 chars, all checks pass
    let fail_no_digit_late = format!("Abc{}", "x".repeat(125)); // no digit anywhere
    // 123 chars with no long repeats, then a run of four 'a' at the very end.
    let repeated_fail_late = format!("{}aaaa", "Ab1".repeat(41));

    let mut group = c.benchmark_group("password/validate");
    for (id, input) in [
        ("typical", typical),
        ("max128_ascii", max_ascii.as_str()),
        ("max128_cjk", max_cjk.as_str()),
        ("fail_no_digit_late", fail_no_digit_late.as_str()),
        ("repeated_fail_late", repeated_fail_late.as_str()),
    ] {
        group.bench_with_input(BenchmarkId::new("validate", id), input, |b, input| {
            b.iter(|| {
                let result = validator.validate(black_box(input));
                let _ = black_box(result);
            });
        });
    }
    group.finish();
}

fn bench_email(c: &mut Criterion) {
    let validator = EmailValidator::new();
    let short = "a@b.co";
    let len32 = "user.name+tag@example.com";
    let len64_long = format!("{}@sub.domain.example.co.uk", "u".repeat(40));
    let invalid_no_at_long = "user.example.dot.com".repeat(3);
    let invalid_tld_digit = "user@example.c0m";
    let dots_adversarial = "a.b.c.d.e.f.g.h@i.j.k.l.mn.opq";

    let mut group = c.benchmark_group("email/validate");
    for (id, input) in [
        ("short", short),
        ("len32", len32),
        ("len64_long", len64_long.as_str()),
        ("invalid_no_at_long", invalid_no_at_long.as_str()),
        ("invalid_tld_digit", invalid_tld_digit),
        ("dots_adversarial", dots_adversarial),
    ] {
        group.bench_with_input(BenchmarkId::new("validate", id), input, |b, input| {
            b.iter(|| {
                let result = validator.validate(black_box(input));
                let _ = black_box(result);
            });
        });
    }
    group.finish();
}

fn bench_room_name_input(c: &mut Criterion) {
    let short = "My Room";
    let max_ascii = "a".repeat(100);
    let max_cjk = cjk(100);
    let html = html_adversarial(100);
    let lt_benign = lt_benign_adversarial();
    let mixed_title = "ROSÉ & Bruno Mars - APT. (Official Music Video)";

    let mut group = c.benchmark_group("room_name/input");
    for (id, input) in [
        ("short", short),
        ("max100_ascii", max_ascii.as_str()),
        ("max100_cjk", max_cjk.as_str()),
        ("html_adversarial", html.as_str()),
        ("lt_benign_adversarial", lt_benign.as_str()),
        ("mixed_unicode_title", mixed_title),
    ] {
        group.bench_with_input(BenchmarkId::new("validate", id), input, |b, input| {
            b.iter(|| {
                let result = validate_room_name_input(black_box(input));
                let _ = black_box(result);
            });
        });
    }
    group.finish();
}

fn bench_room_name_validator(c: &mut Criterion) {
    let validator = RoomNameValidator::new();
    let max_ascii = "a".repeat(100);
    let control_fail_late = format!("{}{}", "a".repeat(99), '\u{0007}');

    let mut group = c.benchmark_group("room_name/validate");
    for (id, input) in [
        ("max100_ascii", max_ascii.as_str()),
        ("control_fail_late", control_fail_late.as_str()),
    ] {
        group.bench_with_input(BenchmarkId::new("validate", id), input, |b, input| {
            b.iter(|| {
                let result = validator.validate(black_box(input));
                let _ = black_box(result);
            });
        });
    }
    group.finish();
}

fn bench_media_name_input(c: &mut Criterion) {
    let short = "Episode 01";
    let max_ascii = "a".repeat(500);
    let max_cjk = cjk(500);
    let html = html_adversarial(500);

    let mut group = c.benchmark_group("media_name/input");
    for (id, input) in [
        ("short", short),
        ("max500_ascii", max_ascii.as_str()),
        ("max500_cjk", max_cjk.as_str()),
        ("html_adversarial", html.as_str()),
    ] {
        group.bench_with_input(BenchmarkId::new("validate", id), input, |b, input| {
            b.iter(|| {
                let result = validate_media_name_input(black_box(input));
                let _ = black_box(result);
            });
        });
    }
    group.finish();
}

fn bench_room_description_input(c: &mut Criterion) {
    let len2500 = "lorem ipsum dolor sit amet ".repeat(100);
    let max_ascii = "a".repeat(5000);
    let max_cjk = cjk(5000);
    let html = html_adversarial(5000);

    let mut group = c.benchmark_group("room_description/input");
    for (id, input) in [
        ("len2500_ascii", len2500.as_str()),
        ("max5000_ascii", max_ascii.as_str()),
        ("max5000_cjk", max_cjk.as_str()),
        ("html_adversarial_5000", html.as_str()),
    ] {
        group.bench_with_input(BenchmarkId::new("validate", id), input, |b, input| {
            b.iter(|| {
                let result = validate_room_description_input(black_box(input));
                let _ = black_box(result);
            });
        });
    }
    group.finish();
}

fn bench_room_password(c: &mut Criterion) {
    let min4 = " abcd ";
    let max128 = "a".repeat(128);
    let too_short = " abc ";

    let mut group = c.benchmark_group("room_password/validate_set");
    for (id, input) in [
        ("min4_padded", min4),
        ("max128", max128.as_str()),
        ("too_short_fail", too_short),
    ] {
        group.bench_with_input(BenchmarkId::new("validate", id), input, |b, input| {
            b.iter(|| {
                let result = validate_room_password_for_set(black_box(input));
                let _ = black_box(result);
            });
        });
    }
    group.finish();
}

fn bench_path_traversal(c: &mut Criterion) {
    let safe_short = "media/movies";
    let safe_long = "a".repeat(1024);
    // Percent-heavy but safe: %41 decodes to 'A', forcing the full
    // percent-decode scan without an early rejection.
    let percent_heavy_safe = "file%41name/dir%42/".repeat(26);
    let adversarial_encoded = "%2e%2e/%252e%252e/etc/passwd";
    let unicode_safe = "unicode-dir/résumé 中文.txt";

    let mut group = c.benchmark_group("path_traversal/validate");
    for (id, input) in [
        ("safe_short", safe_short),
        ("safe_long_1k", safe_long.as_str()),
        ("percent_heavy_safe_512", percent_heavy_safe.as_str()),
        ("adversarial_encoded", adversarial_encoded),
        ("unicode_safe", unicode_safe),
    ] {
        group.bench_with_input(BenchmarkId::new("validate", id), input, |b, input| {
            b.iter(|| {
                let result = validate_path_for_traversal(black_box(input));
                let _ = black_box(result);
            });
        });
    }
    group.finish();
}

fn bench_pipeline(c: &mut Criterion) {
    let username = UsernameValidator::new();
    let email = EmailValidator::new();
    let password = PasswordValidator::new();

    let mut group = c.benchmark_group("pipeline/batch");
    group.bench_function("all_valid", |b| {
        b.iter(|| {
            let mut validator = Validator::new();
            validator
                .validate_field("username", username.validate(black_box("valid_user")))
                .validate_field("email", email.validate(black_box("user@example.com")))
                .validate_field("password", password.validate(black_box("Password123")));
            black_box(validator.is_valid());
        });
    });
    group.bench_function("some_invalid", |b| {
        b.iter(|| {
            let mut validator = Validator::new();
            validator
                .validate_field("username", username.validate(black_box("ab")))
                .validate_field("email", email.validate(black_box("user@example.com")))
                .validate_field("password", password.validate(black_box("weak")));
            black_box(validator.is_valid());
        });
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_username,
    bench_password,
    bench_email,
    bench_room_name_input,
    bench_room_name_validator,
    bench_media_name_input,
    bench_room_description_input,
    bench_room_password,
    bench_path_traversal,
    bench_pipeline,
);
criterion_main!(benches);
