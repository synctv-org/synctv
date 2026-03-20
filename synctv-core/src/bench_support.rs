use std::env;

#[must_use]
pub fn is_nextest_list_mode_from_args<I, S>(args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    args.into_iter().any(|arg| arg.as_ref() == "--list")
}

#[must_use]
pub fn is_nextest_list_mode() -> bool {
    is_nextest_list_mode_from_args(env::args())
}

#[cfg(test)]
mod tests {
    use super::is_nextest_list_mode_from_args;

    #[test]
    fn detects_list_mode_argument() {
        assert!(is_nextest_list_mode_from_args([
            "database_benchmarks",
            "--list",
            "--format",
            "terse",
        ]));
    }

    #[test]
    fn ignores_non_list_invocations() {
        assert!(!is_nextest_list_mode_from_args([
            "database_benchmarks",
            "--bench",
        ]));
    }
}
