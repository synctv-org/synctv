use chrono::{DateTime, Utc};
use sqlx::PgPool;

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

    /// List audit logs with filters and pagination.
    ///
    /// Returns `(rows, total_count)`.
    ///
    /// When no `from` time is specified, defaults to the last 90 days to enable
    /// partition pruning on the `audit_logs` table.
    pub async fn list(&self, query: &AuditLogQuery) -> Result<(Vec<AuditLogRow>, i64)> {
        let default_from = Utc::now() - chrono::Duration::days(90);
        let effective_from = query.from.unwrap_or(default_from);

        // Build WHERE conditions dynamically
        let mut conditions: Vec<String> = Vec::new();
        let mut param_idx: u32 = 1;

        if query.actor_id.is_some() {
            conditions.push(format!("actor_id = ${param_idx}"));
            param_idx += 1;
        }
        if query.action.is_some() {
            conditions.push(format!("action = ${param_idx}"));
            param_idx += 1;
        }
        if query.target_type.is_some() {
            conditions.push(format!("target_type = ${param_idx}"));
            param_idx += 1;
        }
        if query.target_id.is_some() {
            conditions.push(format!("target_id = ${param_idx}"));
            param_idx += 1;
        }
        // Always add time range lower bound for partition pruning
        conditions.push(format!("created_at >= ${param_idx}"));
        param_idx += 1;
        if query.to.is_some() {
            conditions.push(format!("created_at <= ${param_idx}"));
            param_idx += 1;
        }

        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", conditions.join(" AND "))
        };

        // Count query
        let count_sql = format!(
            "SELECT COUNT(*) FROM audit_logs {where_clause}"
        );

        // List query with ip_address cast to text for sqlx compatibility
        let limit_param = param_idx;
        let offset_param = param_idx + 1;
        let list_sql = format!(
            "SELECT id, actor_id, actor_username, action, target_type, target_id, \
             details, host(ip_address)::text AS ip_address, user_agent, created_at \
             FROM audit_logs {where_clause} \
             ORDER BY created_at DESC \
             LIMIT ${limit_param} OFFSET ${offset_param}"
        );

        // Bind parameters for count query
        let mut count_query = sqlx::query_scalar::<_, i64>(&count_sql);
        if let Some(ref v) = query.actor_id { count_query = count_query.bind(v); }
        if let Some(ref v) = query.action { count_query = count_query.bind(v); }
        if let Some(ref v) = query.target_type { count_query = count_query.bind(v); }
        if let Some(ref v) = query.target_id { count_query = count_query.bind(v); }
        count_query = count_query.bind(effective_from);
        if let Some(ref v) = query.to { count_query = count_query.bind(v); }

        let total: i64 = count_query.fetch_one(&self.pool).await?;

        // Bind parameters for list query
        let mut list_query = sqlx::query_as::<_, AuditLogRow>(&list_sql);
        if let Some(ref v) = query.actor_id { list_query = list_query.bind(v); }
        if let Some(ref v) = query.action { list_query = list_query.bind(v); }
        if let Some(ref v) = query.target_type { list_query = list_query.bind(v); }
        if let Some(ref v) = query.target_id { list_query = list_query.bind(v); }
        list_query = list_query.bind(effective_from);
        if let Some(ref v) = query.to { list_query = list_query.bind(v); }
        list_query = list_query.bind(query.page.limit() as i64);
        list_query = list_query.bind(query.page.offset() as i64);

        let rows = list_query.fetch_all(&self.pool).await?;

        Ok((rows, total))
    }

    /// Get a single audit log entry by ID.
    ///
    /// Scans recent partitions only (last 365 days) to avoid full partition scan.
    pub async fn get_by_id(&self, id: i64) -> Result<Option<AuditLogRow>> {
        let row = sqlx::query_as::<_, AuditLogRow>(
            "SELECT id, actor_id, actor_username, action, target_type, target_id, \
             details, host(ip_address)::text AS ip_address, user_agent, created_at \
             FROM audit_logs \
             WHERE id = $1 AND created_at >= NOW() - INTERVAL '365 days'"
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row)
    }
}
