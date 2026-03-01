use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, QueryBuilder};

use crate::models::PageParams;
use crate::Result;

/// Audit log entry as read from the database
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AuditLogRow {
    pub id: i64,
    pub actor_id: Option<String>,
    pub actor_username: Option<String>,
    pub action: String,
    pub target_type: Option<String>,
    pub target_id: Option<String>,
    pub details: Option<serde_json::Value>,
    pub ip_address: Option<String>,
    pub user_agent: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Query parameters for listing audit logs
#[derive(Debug, Clone, Default)]
pub struct AuditLogQuery {
    pub actor_id: Option<String>,
    pub action: Option<String>,
    pub target_type: Option<String>,
    pub target_id: Option<String>,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub page: PageParams,
}

/// Audit log repository for reading audit log entries
#[derive(Clone)]
pub struct AuditLogRepository {
    pool: PgPool,
}

impl AuditLogRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Append the shared WHERE clause filters to a `QueryBuilder`.
    ///
    /// The caller must have already pushed the prefix (e.g. `"SELECT ... WHERE "`)
    /// so that the first condition can be appended directly.
    fn push_filters<'q>(
        builder: &mut QueryBuilder<'q, Postgres>,
        query: &'q AuditLogQuery,
        effective_from: DateTime<Utc>,
    ) {
        // Always add time range lower bound for partition pruning
        builder.push("created_at >= ");
        builder.push_bind(effective_from);

        if let Some(ref v) = query.actor_id {
            builder.push(" AND actor_id = ");
            builder.push_bind(v);
        }
        if let Some(ref v) = query.action {
            builder.push(" AND action = ");
            builder.push_bind(v);
        }
        if let Some(ref v) = query.target_type {
            builder.push(" AND target_type = ");
            builder.push_bind(v);
        }
        if let Some(ref v) = query.target_id {
            builder.push(" AND target_id = ");
            builder.push_bind(v);
        }
        if let Some(ref v) = query.to {
            builder.push(" AND created_at <= ");
            builder.push_bind(v);
        }
    }

    /// List audit logs with filters and pagination.
    ///
    /// Returns `(rows, total_count)`.
    ///
    /// When no `from` time is specified, defaults to the last 90 days to enable
    /// partition pruning on the `audit_logs` table.
    ///
    /// Uses `sqlx::QueryBuilder` for all dynamic SQL construction to ensure
    /// all values are properly parameterized and immune to SQL injection.
    pub async fn list(&self, query: &AuditLogQuery) -> Result<(Vec<AuditLogRow>, i64)> {
        let default_from = Utc::now() - chrono::Duration::days(90);
        let effective_from = query.from.unwrap_or(default_from);

        // ── Count query ───────────────────────────────────────────────────────
        let mut count_builder: QueryBuilder<Postgres> =
            QueryBuilder::new("SELECT COUNT(*) FROM audit_logs WHERE ");
        Self::push_filters(&mut count_builder, query, effective_from);

        let total: i64 = count_builder
            .build_query_scalar()
            .fetch_one(&self.pool)
            .await?;

        // ── List query ────────────────────────────────────────────────────────
        let mut list_builder: QueryBuilder<Postgres> = QueryBuilder::new(
            "SELECT id, actor_id, actor_username, action, target_type, target_id, \
             details, ip_address, user_agent, created_at \
             FROM audit_logs WHERE ",
        );
        Self::push_filters(&mut list_builder, query, effective_from);

        list_builder.push(" ORDER BY created_at DESC LIMIT ");
        list_builder.push_bind(query.page.limit() as i64);
        list_builder.push(" OFFSET ");
        list_builder.push_bind(query.page.offset() as i64);

        let rows = list_builder
            .build_query_as::<AuditLogRow>()
            .fetch_all(&self.pool)
            .await?;

        Ok((rows, total))
    }

    /// Get a single audit log entry by ID.
    ///
    /// Scans recent partitions only (last 365 days) to avoid full partition scan.
    pub async fn get_by_id(&self, id: i64) -> Result<Option<AuditLogRow>> {
        let row = sqlx::query_as::<_, AuditLogRow>(
            "SELECT id, actor_id, actor_username, action, target_type, target_id, \
             details, ip_address, user_agent, created_at \
             FROM audit_logs \
             WHERE id = $1 AND created_at >= NOW() - INTERVAL '365 days'",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row)
    }
}
