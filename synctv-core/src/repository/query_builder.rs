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

/// A single condition in the WHERE clause.
enum Condition {
    /// A static SQL fragment with no bound parameter (e.g. `r.deleted_at IS NULL`).
    Literal(&'static str),
    /// A condition that references one positional parameter.
    /// The `${idx}` placeholder in the template will be replaced with `$N`.
    /// Example template: `"(r.name ILIKE ${idx} OR r.description ILIKE ${idx})"`
    Parameterized {
        template: &'static str,
    },
}

/// Builds a reusable set of WHERE conditions that can be rendered at different
/// parameter offsets.
///
/// # Usage
///
/// ```ignore
/// let mut wb = WhereClauseBuilder::new();
/// wb.push_literal("r.deleted_at IS NULL");
/// if has_search {
///     wb.push_param("(r.name ILIKE ${idx} OR r.description ILIKE ${idx})");
/// }
///
/// // For COUNT query (params start at $1):
/// let (count_where, _) = wb.build(1);
///
/// // For SELECT query (params start at $3 after LIMIT/OFFSET):
/// let (list_where, _) = wb.build(3);
/// ```
pub struct WhereClauseBuilder {
    conditions: Vec<Condition>,
}

impl WhereClauseBuilder {
    /// Create an empty builder.
    pub fn new() -> Self {
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

    /// The number of bound parameters this builder will consume.
    pub fn param_count(&self) -> u32 {
        self.conditions
            .iter()
            .filter(|c| matches!(c, Condition::Parameterized { .. }))
            .count() as u32
    }

    /// Render the WHERE clause body (conditions joined with ` AND `).
    ///
    /// `start_idx` is the first `$N` index to use for parameterized conditions.
    ///
    /// Returns `(sql_fragment, next_unused_index)`.
    pub fn build(&self, start_idx: u32) -> (String, u32) {
        let mut parts: Vec<String> = Vec::with_capacity(self.conditions.len());
        let mut idx = start_idx;

        for cond in &self.conditions {
            match cond {
                Condition::Literal(sql) => {
                    parts.push((*sql).to_string());
                }
                Condition::Parameterized { template } => {
                    parts.push(template.replace("${idx}", &format!("${idx}")));
                    idx += 1;
                }
            }
        }

        (parts.join(" AND "), idx)
    }

    /// Convenience: build `WHERE <conditions>` string.  Returns empty string if
    /// there are no conditions.
    pub fn build_where(&self, start_idx: u32) -> (String, u32) {
        let (body, next) = self.build(start_idx);
        if body.is_empty() {
            (String::new(), next)
        } else {
            (format!("WHERE {body}"), next)
        }
    }
}

/// Escape special characters in a search string for use with SQL ILIKE.
pub fn escape_ilike(search: &str) -> String {
    let escaped = search
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    format!("%{escaped}%")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_literal_only() {
        let mut wb = WhereClauseBuilder::new();
        wb.push_literal("deleted_at IS NULL");
        wb.push_literal("status = 1");

        let (sql, next) = wb.build(1);
        assert_eq!(sql, "deleted_at IS NULL AND status = 1");
        assert_eq!(next, 1); // no params consumed
    }

    #[test]
    fn test_param_conditions() {
        let mut wb = WhereClauseBuilder::new();
        wb.push_literal("deleted_at IS NULL");
        wb.push_param("(name ILIKE ${idx} OR description ILIKE ${idx})");
        wb.push_param("status = ${idx}");

        // Count query: start at $1
        let (sql, next) = wb.build(1);
        assert_eq!(
            sql,
            "deleted_at IS NULL AND (name ILIKE $1 OR description ILIKE $1) AND status = $2"
        );
        assert_eq!(next, 3);

        // List query: start at $3
        let (sql, next) = wb.build(3);
        assert_eq!(
            sql,
            "deleted_at IS NULL AND (name ILIKE $3 OR description ILIKE $3) AND status = $4"
        );
        assert_eq!(next, 5);
    }

    #[test]
    fn test_param_count() {
        let mut wb = WhereClauseBuilder::new();
        wb.push_literal("deleted_at IS NULL");
        wb.push_param("name ILIKE ${idx}");
        wb.push_param("status = ${idx}");
        wb.push_literal("is_banned = FALSE");

        assert_eq!(wb.param_count(), 2);
    }

    #[test]
    fn test_build_where() {
        let mut wb = WhereClauseBuilder::new();
        wb.push_literal("deleted_at IS NULL");

        let (sql, _) = wb.build_where(1);
        assert_eq!(sql, "WHERE deleted_at IS NULL");
    }

    #[test]
    fn test_empty_builder() {
        let wb = WhereClauseBuilder::new();
        let (sql, next) = wb.build_where(1);
        assert_eq!(sql, "");
        assert_eq!(next, 1);
    }

    #[test]
    fn test_escape_ilike() {
        assert_eq!(escape_ilike("hello"), "%hello%");
        assert_eq!(escape_ilike("100%"), "%100\\%%");
        assert_eq!(escape_ilike("under_score"), "%under\\_score%");
        assert_eq!(escape_ilike("back\\slash"), "%back\\\\slash%");
    }
}
