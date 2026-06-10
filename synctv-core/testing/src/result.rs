pub fn ok<T, E: std::fmt::Debug>(result: Result<T, E>, context: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => std::panic::panic_any(format!("{context}: {error:?}")),
    }
}

pub fn err<T: std::fmt::Debug, E>(result: Result<T, E>, context: &str) -> E {
    match result {
        Ok(value) => std::panic::panic_any(format!("{context}: {value:?}")),
        Err(error) => error,
    }
}

pub fn some<T>(value: Option<T>, context: &str) -> T {
    match value {
        Some(value) => value,
        None => std::panic::panic_any(context.to_string()),
    }
}

pub trait TestResultExt<T, E> {
    fn checked(self, context: &str) -> T;
    fn failed(self, context: &str) -> E
    where
        T: std::fmt::Debug;
}

impl<T, E: std::fmt::Debug> TestResultExt<T, E> for Result<T, E> {
    fn checked(self, context: &str) -> T {
        ok(self, context)
    }

    fn failed(self, context: &str) -> E
    where
        T: std::fmt::Debug,
    {
        err(self, context)
    }
}

pub trait TestOptionExt<T> {
    fn checked(self, context: &str) -> T;
}

impl<T> TestOptionExt<T> for Option<T> {
    fn checked(self, context: &str) -> T {
        some(self, context)
    }
}
