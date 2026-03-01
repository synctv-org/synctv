//! Common assertion macros for testing
//!
//! This module provides reusable assertion macros to reduce boilerplate
//! across tests.
#![allow(clippy::unwrap_used)]

/// Asserts that a value is within a range
///
/// # Example
///
/// ```text
/// use synctv_core_testing::assertions::assert_in_range;
///
/// let delay = calculate_delay();
/// assert_in_range!(delay.as_millis(), 200, 500);
/// ```
#[macro_export]
macro_rules! assert_in_range {
    ($value:expr, $min:expr, $max:expr) => {
        let v = $value;
        let min = $min;
        let max = $max;
        assert!(
            v >= min && v <= max,
            "Value {} is not in range [{}, {}]",
            v, min, max
        );
    };
}

/// Asserts that a Result is Ok and returns the value
///
/// # Example
///
/// ```text
/// let result = assert_ok!(some_operation());
/// ```
#[macro_export]
macro_rules! assert_ok {
    ($result:expr) => {
        match $result {
            Ok(value) => value,
            Err(e) => panic!("Expected Ok, got Err: {:?}", e),
        }
    };
}

/// Asserts that a Result is Err and returns the error
///
/// # Example
///
/// ```text
/// let error = assert_err!(some_operation());
/// ```
#[macro_export]
macro_rules! assert_err {
    ($result:expr) => {
        match $result {
            Err(e) => e,
            Ok(v) => panic!("Expected Err, got Ok: {:?}", v),
        }
    };
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_assert_error_macro() {
        let _result: Result<(), String> = Err("test error".to_string());
        // This should compile - we can't actually test it without the Error enum
        // assert_error!(result, String);
    }

    #[test]
    fn test_assert_ok_macro() {
        let result: Result<u32, String> = Ok(42);
        let value = assert_ok!(result);
        assert_eq!(value, 42);
    }

    #[test]
    fn test_assert_err_macro() {
        let result: Result<u32, String> = Err("test error".to_string());
        let error = assert_err!(result);
        assert_eq!(error, "test error");
    }

    #[test]
    fn test_assert_in_range_macro() {
        assert_in_range!(5, 1, 10);
    }

    #[test]
    #[should_panic]
    fn test_assert_in_range_out_of_bounds() {
        assert_in_range!(15, 1, 10);
    }
}
