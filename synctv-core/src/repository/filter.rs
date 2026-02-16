//! Type-safe SQL query builder using `SeaQuery`
//!
//! This module provides a safe, composable way to build SQL WHERE clauses
//! using `SeaQuery`. All queries are properly parameterized to prevent SQL injection.
//!
//! TODO: This module is currently unused. Either integrate it into repository
//! query methods or remove it to reduce dead code.

use sea_query::{Cond, Expr, Iden, SimpleExpr, Value as SeaValue};
use sea_query::extension::postgres::PgExpr;

/// SQL condition builder
///
/// This is a wrapper around `SeaQuery`'s `Cond` that provides a more ergonomic API
/// for building database queries in a type-safe manner.
#[derive(Clone, Debug)]
pub struct Filter {
    condition: Cond,
}

impl Filter {
    /// Create a new filter with no conditions (matches everything)
    #[must_use] 
    pub fn new() -> Self {
        Self {
            condition: Cond::all(),
        }
    }

    /// Add an equality condition: column = value
    pub fn eq(mut self, column: impl Into<ColumnRef>, value: impl Into<FilterValue>) -> Self {
        let col = column.into();
        let val = value.into();
        self.condition = self.condition.add(Expr::col(col).eq(val.into_sea_value()));
        self
    }

    /// Add an inequality condition: column != value
    pub fn ne(mut self, column: impl Into<ColumnRef>, value: impl Into<FilterValue>) -> Self {
        let col = column.into();
        let val = value.into();
        self.condition = self.condition.add(Expr::col(col).ne(val.into_sea_value()));
        self
    }

    /// Add a greater than condition: column > value
    pub fn gt(mut self, column: impl Into<ColumnRef>, value: impl Into<FilterValue>) -> Self {
        let col = column.into();
        let val = value.into();
        self.condition = self.condition.add(Expr::col(col).gt(val.into_sea_value()));
        self
    }

    /// Add a greater or equal condition: column >= value
    pub fn ge(mut self, column: impl Into<ColumnRef>, value: impl Into<FilterValue>) -> Self {
        let col = column.into();
        let val = value.into();
        self.condition = self.condition.add(Expr::col(col).gte(val.into_sea_value()));
        self
    }

    /// Add a less than condition: column < value
    pub fn lt(mut self, column: impl Into<ColumnRef>, value: impl Into<FilterValue>) -> Self {
        let col = column.into();
        let val = value.into();
        self.condition = self.condition.add(Expr::col(col).lt(val.into_sea_value()));
        self
    }

    /// Add a less or equal condition: column <= value
    pub fn le(mut self, column: impl Into<ColumnRef>, value: impl Into<FilterValue>) -> Self {
        let col = column.into();
        let val = value.into();
        self.condition = self.condition.add(Expr::col(col).lte(val.into_sea_value()));
        self
    }

    /// Add a LIKE condition: column LIKE pattern
    pub fn like(mut self, column: impl Into<ColumnRef>, pattern: impl Into<String>) -> Self {
        let col = column.into();
        self.condition = self.condition.add(Expr::col(col).like(pattern.into()));
        self
    }

    /// Add an ILIKE (case-insensitive) condition: column ILIKE pattern
    pub fn ilike(mut self, column: impl Into<ColumnRef>, pattern: impl Into<String>) -> Self {
        let col = column.into();
        let expr: SimpleExpr = Expr::col(col).ilike(pattern.into());
        self.condition = self.condition.add(expr);
        self
    }

    /// Add an IN condition: column IN (values)
    pub fn in_list(mut self, column: impl Into<ColumnRef>, values: Vec<FilterValue>) -> Self {
        let col = column.into();
        let sea_values: Vec<SeaValue> = values.into_iter().map(FilterValue::into_sea_value).collect();
        self.condition = self.condition.add(Expr::col(col).is_in(sea_values));
        self
    }

    /// Add a BETWEEN condition: column BETWEEN low AND high
    pub fn between(
        mut self,
        column: impl Into<ColumnRef>,
        low: impl Into<FilterValue>,
        high: impl Into<FilterValue>,
    ) -> Self {
        let col = column.into();
        let low_val = low.into();
        let high_val = high.into();
        self.condition = self.condition.add(
            Expr::col(col).between(low_val.into_sea_value(), high_val.into_sea_value())
        );
        self
    }

    /// Add an IS NULL condition: column IS NULL
    pub fn is_null(mut self, column: impl Into<ColumnRef>) -> Self {
        let col = column.into();
        self.condition = self.condition.add(Expr::col(col).is_null());
        self
    }

    /// Add an IS NOT NULL condition: column IS NOT NULL
    pub fn is_not_null(mut self, column: impl Into<ColumnRef>) -> Self {
        let col = column.into();
        self.condition = self.condition.add(Expr::col(col).is_not_null());
        self
    }

    /// Build the SQL condition and return as `SeaQuery` Cond
    #[must_use] 
    pub fn build(self) -> Cond {
        self.condition
    }
}

impl Default for Filter {
    fn default() -> Self {
        Self::new()
    }
}

/// Reference to a database column
///
/// This type ensures column names are type-safe and prevents SQL injection.
#[derive(Clone, Debug)]
pub enum ColumnRef {
    /// Simple column reference (e.g., "username")
    Simple(String),
    /// Table-qualified column (e.g., "users.username")
    Qualified { table: String, column: String },
}

impl From<&str> for ColumnRef {
    fn from(s: &str) -> Self {
        Self::Simple(s.to_string())
    }
}

impl From<String> for ColumnRef {
    fn from(s: String) -> Self {
        Self::Simple(s)
    }
}

impl Iden for ColumnRef {
    fn unquoted(&self, s: &mut dyn std::fmt::Write) {
        match self {
            Self::Simple(name) => {
                // write! to fmt::Write only fails if the underlying writer fails,
                // which doesn't happen with SeaQuery's internal string buffer.
                let _ = write!(s, "{name}");
            }
            Self::Qualified { table, column } => {
                let _ = write!(s, "{table}.{column}");
            }
        }
    }
}

/// Value that can be used in filter conditions
///
/// This enum wraps values that can be safely parameterized in SQL queries.
#[derive(Clone, Debug)]
pub enum FilterValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    // Note: DateTime<Utc> can be added via String
}

impl FilterValue {
    /// Convert to `SeaQuery` Value (for parameterized queries)
    fn into_sea_value(self) -> SeaValue {
        match self {
            Self::Null => SeaValue::String(None),
            Self::Bool(b) => SeaValue::Bool(Some(b)),
            Self::Int(i) => SeaValue::BigInt(Some(i)),
            Self::Float(f) => SeaValue::Double(Some(f)),
            Self::String(s) => SeaValue::String(Some(Box::new(s))),
        }
    }
}

impl From<bool> for FilterValue {
    fn from(b: bool) -> Self {
        Self::Bool(b)
    }
}

impl From<i32> for FilterValue {
    fn from(i: i32) -> Self {
        Self::Int(i64::from(i))
    }
}

impl From<i64> for FilterValue {
    fn from(i: i64) -> Self {
        Self::Int(i)
    }
}

impl From<f64> for FilterValue {
    fn from(f: f64) -> Self {
        Self::Float(f)
    }
}

impl From<&str> for FilterValue {
    fn from(s: &str) -> Self {
        Self::String(s.to_string())
    }
}

impl From<String> for FilterValue {
    fn from(s: String) -> Self {
        Self::String(s)
    }
}

impl From<Option<bool>> for FilterValue {
    fn from(opt: Option<bool>) -> Self {
        match opt {
            Some(b) => Self::Bool(b),
            None => Self::Null,
        }
    }
}

impl From<Option<i32>> for FilterValue {
    fn from(opt: Option<i32>) -> Self {
        match opt {
            Some(i) => Self::Int(i64::from(i)),
            None => Self::Null,
        }
    }
}

impl From<Option<i64>> for FilterValue {
    fn from(opt: Option<i64>) -> Self {
        match opt {
            Some(i) => Self::Int(i),
            None => Self::Null,
        }
    }
}

impl From<Option<String>> for FilterValue {
    fn from(opt: Option<String>) -> Self {
        match opt {
            Some(s) => Self::String(s),
            None => Self::Null,
        }
    }
}

impl From<Option<&str>> for FilterValue {
    fn from(opt: Option<&str>) -> Self {
        match opt {
            Some(s) => Self::String(s.to_string()),
            None => Self::Null,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_filter_eq() {
        let filter = Filter::new()
            .eq("username", "alice")
            .build();

        let cond_str = format!("{:?}", filter);
        assert!(cond_str.contains("username"));
    }

    #[test]
    fn test_filter_multiple_conditions() {
        let filter = Filter::new()
            .eq("status", "active")
            .ge("age", 18)
            .is_null("deleted_at")
            .build();

        // The filter should successfully build
        let _ = filter;
    }

    #[test]
    fn test_filter_like() {
        let filter = Filter::new()
            .like("username", "alice%")
            .build();

        let _ = filter;
    }

    #[test]
    fn test_filter_in_list() {
        let filter = Filter::new()
            .in_list("id", vec![
                FilterValue::Int(1),
                FilterValue::Int(2),
                FilterValue::Int(3),
            ])
            .build();

        let _ = filter;
    }

    #[test]
    fn test_filter_between() {
        let filter = Filter::new()
            .between("created_at", "2024-01-01", "2024-12-31")
            .build();

        let _ = filter;
    }

    #[test]
    fn test_column_ref_from_str() {
        let col: ColumnRef = "username".into();
        assert!(
            matches!(&col, ColumnRef::Simple(name) if name == "username"),
            "Expected Simple column with name 'username', got {col:?}"
        );
    }

    #[test]
    fn test_filter_value_from_various_types() {
        let _: FilterValue = true.into();
        let _: FilterValue = 42.into();
        let _: FilterValue = std::f64::consts::PI.into();
        let _: FilterValue = "test".into();
        let _: FilterValue = String::from("test").into();
    }

    #[test]
    fn test_filter_value_from_option() {
        let _: FilterValue = Some(true).into();
        let _: FilterValue = Some(42).into();
        let _: FilterValue = Some("test").into();
        let _: FilterValue = None::<bool>.into();
    }
}
