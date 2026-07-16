//! Helper for building dynamic SQL WHERE clauses with positional parameters.
//!
//! The [`WhereClauseBuilder`] collects conditions (some with bound parameters, some
//! static literals) and can render the WHERE clause at any starting parameter index.
//! This lets the same set of conditions be used for both a `COUNT(*)` query (params
//! starting at `$1`) and a paginated `SELECT` query (params starting at `$3` because
//! `$1`/`$2` are LIMIT/OFFSET).
//!
//! **Binding values** is still done by the caller -- the builder only tracks *which*
//! conditions need a bound parameter and in what order.

use crate::{Error, Result};

pub fn trusted_dynamic_sql(sql: String) -> sqlx::AssertSqlSafe<String> {
    sqlx::AssertSqlSafe(sql)
}

/// A single condition in the WHERE clause.
enum Condition {
    /// A static SQL fragment with no bound parameter (e.g. `r.deleted_at IS NULL`).
    Literal(&'static str),
    /// A condition that references one positional parameter.
    /// The `${idx}` placeholder in the template will be replaced with `$N`.
    /// Example template: `"(r.name ILIKE ${idx} OR r.description ILIKE ${idx})"`
    Parameterized { template: &'static str },
}

/// Builds a reusable set of WHERE conditions that can be rendered at different
/// parameter offsets.
///
/// # Usage
///
/// ```text
/// let mut wb = WhereClauseBuilder::new();
/// wb.push_literal("r.deleted_at IS NULL");
/// if has_search {
///     wb.push_param("(r.name ILIKE ${idx} OR r.description ILIKE ${idx})");
/// }
///
/// // For COUNT query (params start at $1):
/// let (count_where, _) = wb.build(1)?;
///
/// // For SELECT query (params start at $3 after LIMIT/OFFSET):
/// let (list_where, _) = wb.build(3)?;
/// ```
pub struct WhereClauseBuilder {
    conditions: Vec<Condition>,
}

impl Default for WhereClauseBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl WhereClauseBuilder {
    /// Create an empty builder.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            conditions: Vec::new(),
        }
    }

    /// Add a static condition with no bound parameter.
    pub fn push_literal(&mut self, sql: &'static str) {
        self.conditions.push(Condition::Literal(sql));
    }

    /// Add a condition that uses one positional parameter.
    ///
    /// The template must contain `${idx}` wherever the parameter placeholder should appear.
    /// For example: `"status = ${idx}"` or `"(name ILIKE ${idx} OR email ILIKE ${idx})"`.
    pub fn push_param(&mut self, template: &'static str) {
        self.conditions.push(Condition::Parameterized { template });
    }

    /// Render the WHERE clause body (conditions joined with ` AND `).
    ///
    /// `start_idx` is the first `$N` index to use for parameterized conditions.
    ///
    /// Returns `(sql_fragment, next_unused_index)`.
    pub fn build(&self, start_idx: u32) -> Result<(String, u32)> {
        let mut parts: Vec<String> = Vec::with_capacity(self.conditions.len());
        let mut idx = start_idx;

        for cond in &self.conditions {
            match cond {
                Condition::Literal(sql) => {
                    parts.push((*sql).to_string());
                }
                Condition::Parameterized { template } => {
                    parts.push(template.replace("${idx}", &format!("${idx}")));
                    idx = idx.checked_add(1).ok_or_else(|| {
                        Error::Internal("WHERE clause parameter index exceeds u32::MAX".to_string())
                    })?;
                }
            }
        }

        Ok((parts.join(" AND "), idx))
    }
}

/// Escape special characters in a search string for use with SQL ILIKE.
#[must_use]
pub fn escape_ilike(search: &str) -> String {
    let escaped = search
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    format!("%{escaped}%")
}

/// Normalize user-facing database search text before building a query.
#[must_use]
pub fn normalize_search_text(search: &str) -> Option<String> {
    let trimmed = search.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Build an escaped `%...%` ILIKE pattern from normalized search text.
#[must_use]
pub fn ilike_contains_pattern(search: &str) -> Option<String> {
    normalize_search_text(search).map(|normalized| escape_ilike(&normalized))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok<T, E: std::fmt::Display>(result: std::result::Result<T, E>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => std::panic::panic_any(format!("{context}: {error}")),
        }
    }

    #[test]
    fn test_literal_only() {
        let mut wb = WhereClauseBuilder::new();
        wb.push_literal("deleted_at IS NULL");
        wb.push_literal("status = 1");

        let (sql, next) = ok(wb.build(1), "literal WHERE clause should build");
        assert_eq!(sql, "deleted_at IS NULL AND status = 1");
        assert_eq!(next, 1);
    }

    #[test]
    fn test_param_conditions() {
        let mut wb = WhereClauseBuilder::new();
        wb.push_literal("deleted_at IS NULL");
        wb.push_param("(name ILIKE ${idx} OR description ILIKE ${idx})");
        wb.push_param("status = ${idx}");

        let (sql, next) = ok(wb.build(1), "count WHERE clause should build");
        assert_eq!(
            sql,
            "deleted_at IS NULL AND (name ILIKE $1 OR description ILIKE $1) AND status = $2"
        );
        assert_eq!(next, 3);

        let (sql, next) = ok(wb.build(3), "list WHERE clause should build");
        assert_eq!(
            sql,
            "deleted_at IS NULL AND (name ILIKE $3 OR description ILIKE $3) AND status = $4"
        );
        assert_eq!(next, 5);
    }

    #[test]
    fn test_build_tracks_consumed_parameters() {
        let mut wb = WhereClauseBuilder::new();
        wb.push_literal("deleted_at IS NULL");
        wb.push_param("name ILIKE ${idx}");
        wb.push_param("status = ${idx}");
        wb.push_literal("is_banned = FALSE");

        let (_, next) = ok(wb.build(7), "WHERE clause should build");
        assert_eq!(next, 9);
    }

    #[test]
    fn test_escape_ilike() {
        assert_eq!(escape_ilike("hello"), "%hello%");
        assert_eq!(escape_ilike("100%"), "%100\\%%");
        assert_eq!(escape_ilike("under_score"), "%under\\_score%");
        assert_eq!(escape_ilike("back\\slash"), "%back\\\\slash%");
    }

    #[test]
    fn test_normalize_search_text() {
        assert_eq!(normalize_search_text("  hello  ").as_deref(), Some("hello"));
        assert_eq!(normalize_search_text(" \t\n "), None);
    }

    #[test]
    fn test_ilike_contains_pattern_normalizes_and_escapes() {
        assert_eq!(
            ilike_contains_pattern("  100%_ok  ").as_deref(),
            Some("%100\\%\\_ok%")
        );
        assert_eq!(ilike_contains_pattern("   "), None);
    }
}
