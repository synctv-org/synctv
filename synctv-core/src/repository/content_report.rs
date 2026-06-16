use sqlx::PgPool;

use crate::{
    models::{
        ContentReport, ContentReportAdminRow, ContentReportId, ContentReportStatus,
        ContentReportTarget, ContentReportTargetType, CreateContentReport, RoomId, UserId,
    },
    Error, Result,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ContentReportListScope {
    #[default]
    AnyRelated,
    RoomContext,
    TargetRoom,
    TargetUser,
    TargetMember,
    TargetChatMessage,
}

#[derive(Debug, Clone)]
pub struct ContentReportListQuery {
    pub status: Option<ContentReportStatus>,
    pub target_type: Option<ContentReportTargetType>,
    pub reporter_user_id: Option<UserId>,
    pub room_id: Option<RoomId>,
    pub target_room_id: Option<RoomId>,
    pub target_user_id: Option<UserId>,
    pub target_member_room_id: Option<RoomId>,
    pub target_member_user_id: Option<UserId>,
    pub target_chat_message_id: Option<i64>,
    pub scope: ContentReportListScope,
    pub search: String,
    pub limit: i64,
    pub offset: i64,
}

#[derive(Debug, Clone)]
pub struct ContentReportPage {
    pub rows: Vec<ContentReportAdminRow>,
    pub total: i64,
}

#[derive(Clone)]
pub struct ContentReportRepository {
    pool: PgPool,
    read_pool: Option<PgPool>,
}

impl ContentReportRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self {
            pool,
            read_pool: None,
        }
    }

    #[must_use]
    pub const fn new_with_read_pool(pool: PgPool, read_pool: PgPool) -> Self {
        Self {
            pool,
            read_pool: Some(read_pool),
        }
    }

    fn eventually_consistent_pool(&self) -> &PgPool {
        self.read_pool.as_ref().unwrap_or(&self.pool)
    }

    pub async fn create(
        &self,
        request: CreateContentReport,
        chat_message_created_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<ContentReport> {
        let target_type = request.target.target_type();
        let room_id = request.target.room_context();
        let (target_room_id, target_user_id, target_member_room_id, target_member_user_id) =
            match request.target {
                ContentReportTarget::Room { room_id } => (Some(room_id), None, None, None),
                ContentReportTarget::User { user_id } => (None, Some(user_id), None, None),
                ContentReportTarget::RoomMember { room_id, user_id } => {
                    (None, None, Some(room_id), Some(user_id))
                }
                ContentReportTarget::ChatMessage { .. } => (None, None, None, None),
            };
        let target_chat_message_id = match request.target {
            ContentReportTarget::ChatMessage { message_id, .. } => Some(message_id),
            _ => None,
        };

        let report = sqlx::query_as::<_, ContentReport>(
            r"
            INSERT INTO content_reports (
                reporter_user_id,
                room_id,
                target_type,
                target_room_id,
                target_user_id,
                target_member_room_id,
                target_member_user_id,
                target_chat_message_id,
                target_chat_message_created_at,
                reason_code,
                reason,
                metadata,
                status
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
            RETURNING
                id,
                reporter_user_id,
                room_id,
                target_type,
                target_room_id,
                target_user_id,
                target_member_room_id,
                target_member_user_id,
                target_chat_message_id,
                target_chat_message_created_at,
                reason_code,
                reason,
                metadata,
                status,
                reviewed_by,
                reviewed_at,
                resolution_note,
                created_at,
                updated_at
            ",
        )
        .bind(request.reporter_user_id.as_i64())
        .bind(room_id.map(|id| id.as_i64()))
        .bind(i16::from(target_type))
        .bind(target_room_id.map(|id| id.as_i64()))
        .bind(target_user_id.map(|id| id.as_i64()))
        .bind(target_member_room_id.map(|id| id.as_i64()))
        .bind(target_member_user_id.map(|id| id.as_i64()))
        .bind(target_chat_message_id)
        .bind(chat_message_created_at)
        .bind(request.reason_code)
        .bind(request.reason)
        .bind(request.metadata)
        .bind(i16::from(ContentReportStatus::Open))
        .fetch_one(&self.pool)
        .await?;

        Ok(report)
    }

    pub async fn list_admin(&self, query: &ContentReportListQuery) -> Result<ContentReportPage> {
        let status = query.status.map(i16::from);
        let target_type = query.target_type.map(i16::from);
        let reporter_user_id = query.reporter_user_id.map(|id| id.as_i64());
        let room_id = query.room_id.map(|id| id.as_i64());
        let target_room_id = query.target_room_id.map(|id| id.as_i64());
        let target_user_id = query.target_user_id.map(|id| id.as_i64());
        let target_member_room_id = query.target_member_room_id.map(|id| id.as_i64());
        let target_member_user_id = query.target_member_user_id.map(|id| id.as_i64());
        let scope = i16::from(query.scope);
        let search = normalize_search(&query.search);
        let pool = self.eventually_consistent_pool();

        let total = sqlx::query_scalar!(
            r#"
            SELECT COUNT(*) AS "count!"
            FROM content_reports cr
            LEFT JOIN users reporter ON reporter.id = cr.reporter_user_id
            LEFT JOIN rooms room_ctx ON room_ctx.id = cr.room_id
            LEFT JOIN rooms target_room ON target_room.id = cr.target_room_id
            LEFT JOIN users target_user ON target_user.id = cr.target_user_id
            LEFT JOIN users target_member_user ON target_member_user.id = cr.target_member_user_id
            LEFT JOIN chat_messages chat
              ON chat.room_id = cr.room_id
             AND chat.id = cr.target_chat_message_id
             AND chat.created_at = cr.target_chat_message_created_at
            WHERE ($1::smallint IS NULL OR cr.status = $1)
              AND ($2::smallint IS NULL OR cr.target_type = $2)
              AND ($3::bigint IS NULL OR cr.reporter_user_id = $3)
              AND (
                $10::smallint = 1
                OR ($10 = 2 AND cr.target_type <> 2)
                OR ($10 = 3 AND cr.target_type = 1)
                OR ($10 = 4 AND cr.target_type = 2)
                OR ($10 = 5 AND cr.target_type = 3)
                OR ($10 = 6 AND cr.target_type = 4)
              )
              AND (
                $4::bigint IS NULL
                OR ($10 = 1 AND (cr.room_id = $4 OR cr.target_room_id = $4 OR cr.target_member_room_id = $4))
                OR ($10 = 2 AND cr.target_room_id = $4 AND cr.target_type = 1)
                OR ($10 = 2 AND cr.room_id = $4 AND cr.target_type <> 1)
                OR ($10 = 3 AND cr.target_room_id = $4)
                OR ($10 = 5 AND cr.target_member_room_id = $4)
                OR ($10 = 6 AND cr.room_id = $4)
              )
              AND ($5::bigint IS NULL OR cr.target_room_id = $5)
              AND ($6::bigint IS NULL OR cr.target_user_id = $6)
              AND ($7::bigint IS NULL OR cr.target_member_room_id = $7)
              AND ($8::bigint IS NULL OR cr.target_member_user_id = $8)
              AND ($9::bigint IS NULL OR cr.target_chat_message_id = $9)
              AND (
                $11::text IS NULL
                OR lower(cr.reason_code) LIKE $11
                OR lower(cr.reason) LIKE $11
                OR lower(COALESCE(reporter.username, '')) LIKE $11
                OR lower(COALESCE(room_ctx.name, '')) LIKE $11
                OR lower(COALESCE(target_room.name, '')) LIKE $11
                OR lower(COALESCE(target_user.username, '')) LIKE $11
                OR lower(COALESCE(target_member_user.username, '')) LIKE $11
                OR lower(COALESCE(chat.content, '')) LIKE $11
                OR cr.id::text LIKE $11
                OR cr.target_chat_message_id::text LIKE $11
              )
            "#,
            status,
            target_type,
            reporter_user_id,
            room_id,
            target_room_id,
            target_user_id,
            target_member_room_id,
            target_member_user_id,
            query.target_chat_message_id,
            scope,
            search.as_deref(),
        )
        .fetch_one(pool)
        .await?;

        let rows = sqlx::query_as!(
            ContentReportAdminRow,
            r#"
            SELECT
                cr.id AS "id!: ContentReportId",
                cr.reporter_user_id AS "reporter_user_id!: UserId",
                COALESCE(reporter.username, '') AS "reporter_username!",
                cr.room_id AS "room_id?: RoomId",
                COALESCE(room_ctx.name, '') AS "room_name!",
                cr.target_type AS "target_type!: ContentReportTargetType",
                cr.target_room_id AS "target_room_id?: RoomId",
                COALESCE(target_room.name, '') AS "target_room_name!",
                cr.target_user_id AS "target_user_id?: UserId",
                COALESCE(target_user.username, '') AS "target_username!",
                cr.target_member_room_id AS "target_member_room_id?: RoomId",
                COALESCE(target_member_room.name, '') AS "target_member_room_name!",
                cr.target_member_user_id AS "target_member_user_id?: UserId",
                COALESCE(target_member_user.username, '') AS "target_member_username!",
                cr.target_chat_message_id,
                cr.target_chat_message_created_at,
                COALESCE(left(chat.content, 240), '') AS "target_chat_message_preview!",
                cr.reason_code,
                cr.reason,
                cr.metadata,
                cr.status AS "status!: ContentReportStatus",
                cr.reviewed_by AS "reviewed_by?: UserId",
                COALESCE(reviewer.username, '') AS "reviewed_by_username!",
                cr.reviewed_at,
                cr.resolution_note,
                cr.created_at,
                cr.updated_at
            FROM content_reports cr
            LEFT JOIN users reporter ON reporter.id = cr.reporter_user_id
            LEFT JOIN users reviewer ON reviewer.id = cr.reviewed_by
            LEFT JOIN rooms room_ctx ON room_ctx.id = cr.room_id
            LEFT JOIN rooms target_room ON target_room.id = cr.target_room_id
            LEFT JOIN users target_user ON target_user.id = cr.target_user_id
            LEFT JOIN rooms target_member_room ON target_member_room.id = cr.target_member_room_id
            LEFT JOIN users target_member_user ON target_member_user.id = cr.target_member_user_id
            LEFT JOIN chat_messages chat
              ON chat.room_id = cr.room_id
             AND chat.id = cr.target_chat_message_id
             AND chat.created_at = cr.target_chat_message_created_at
            WHERE ($1::smallint IS NULL OR cr.status = $1)
              AND ($2::smallint IS NULL OR cr.target_type = $2)
              AND ($3::bigint IS NULL OR cr.reporter_user_id = $3)
              AND (
                $10::smallint = 1
                OR ($10 = 2 AND cr.target_type <> 2)
                OR ($10 = 3 AND cr.target_type = 1)
                OR ($10 = 4 AND cr.target_type = 2)
                OR ($10 = 5 AND cr.target_type = 3)
                OR ($10 = 6 AND cr.target_type = 4)
              )
              AND (
                $4::bigint IS NULL
                OR ($10 = 1 AND (cr.room_id = $4 OR cr.target_room_id = $4 OR cr.target_member_room_id = $4))
                OR ($10 = 2 AND cr.target_room_id = $4 AND cr.target_type = 1)
                OR ($10 = 2 AND cr.room_id = $4 AND cr.target_type <> 1)
                OR ($10 = 3 AND cr.target_room_id = $4)
                OR ($10 = 5 AND cr.target_member_room_id = $4)
                OR ($10 = 6 AND cr.room_id = $4)
              )
              AND ($5::bigint IS NULL OR cr.target_room_id = $5)
              AND ($6::bigint IS NULL OR cr.target_user_id = $6)
              AND ($7::bigint IS NULL OR cr.target_member_room_id = $7)
              AND ($8::bigint IS NULL OR cr.target_member_user_id = $8)
              AND ($9::bigint IS NULL OR cr.target_chat_message_id = $9)
              AND (
                $11::text IS NULL
                OR lower(cr.reason_code) LIKE $11
                OR lower(cr.reason) LIKE $11
                OR lower(COALESCE(reporter.username, '')) LIKE $11
                OR lower(COALESCE(room_ctx.name, '')) LIKE $11
                OR lower(COALESCE(target_room.name, '')) LIKE $11
                OR lower(COALESCE(target_user.username, '')) LIKE $11
                OR lower(COALESCE(target_member_user.username, '')) LIKE $11
                OR lower(COALESCE(chat.content, '')) LIKE $11
                OR cr.id::text LIKE $11
                OR cr.target_chat_message_id::text LIKE $11
              )
            ORDER BY cr.created_at DESC, cr.id DESC
            LIMIT $12 OFFSET $13
            "#,
            status,
            target_type,
            reporter_user_id,
            room_id,
            target_room_id,
            target_user_id,
            target_member_room_id,
            target_member_user_id,
            query.target_chat_message_id,
            scope,
            search.as_deref(),
            query.limit,
            query.offset,
        )
        .fetch_all(pool)
        .await?;

        Ok(ContentReportPage { rows, total })
    }

    pub async fn get_admin(&self, id: ContentReportId) -> Result<Option<ContentReportAdminRow>> {
        let row = sqlx::query_as!(
            ContentReportAdminRow,
            r#"
            SELECT
                cr.id AS "id!: ContentReportId",
                cr.reporter_user_id AS "reporter_user_id!: UserId",
                COALESCE(reporter.username, '') AS "reporter_username!",
                cr.room_id AS "room_id?: RoomId",
                COALESCE(room_ctx.name, '') AS "room_name!",
                cr.target_type AS "target_type!: ContentReportTargetType",
                cr.target_room_id AS "target_room_id?: RoomId",
                COALESCE(target_room.name, '') AS "target_room_name!",
                cr.target_user_id AS "target_user_id?: UserId",
                COALESCE(target_user.username, '') AS "target_username!",
                cr.target_member_room_id AS "target_member_room_id?: RoomId",
                COALESCE(target_member_room.name, '') AS "target_member_room_name!",
                cr.target_member_user_id AS "target_member_user_id?: UserId",
                COALESCE(target_member_user.username, '') AS "target_member_username!",
                cr.target_chat_message_id,
                cr.target_chat_message_created_at,
                COALESCE(left(chat.content, 240), '') AS "target_chat_message_preview!",
                cr.reason_code,
                cr.reason,
                cr.metadata,
                cr.status AS "status!: ContentReportStatus",
                cr.reviewed_by AS "reviewed_by?: UserId",
                COALESCE(reviewer.username, '') AS "reviewed_by_username!",
                cr.reviewed_at,
                cr.resolution_note,
                cr.created_at,
                cr.updated_at
            FROM content_reports cr
            LEFT JOIN users reporter ON reporter.id = cr.reporter_user_id
            LEFT JOIN users reviewer ON reviewer.id = cr.reviewed_by
            LEFT JOIN rooms room_ctx ON room_ctx.id = cr.room_id
            LEFT JOIN rooms target_room ON target_room.id = cr.target_room_id
            LEFT JOIN users target_user ON target_user.id = cr.target_user_id
            LEFT JOIN rooms target_member_room ON target_member_room.id = cr.target_member_room_id
            LEFT JOIN users target_member_user ON target_member_user.id = cr.target_member_user_id
            LEFT JOIN chat_messages chat
              ON chat.room_id = cr.room_id
             AND chat.id = cr.target_chat_message_id
             AND chat.created_at = cr.target_chat_message_created_at
            WHERE cr.id = $1
            "#,
            id.as_i64(),
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    pub async fn update_status(
        &self,
        id: ContentReportId,
        status: ContentReportStatus,
        reviewed_by: UserId,
        resolution_note: String,
    ) -> Result<ContentReportAdminRow> {
        let updated = sqlx::query_as!(
            ContentReportAdminRow,
            r#"
            WITH updated AS (
                UPDATE content_reports
                SET status = $2,
                    reviewed_by = $3,
                    reviewed_at = CURRENT_TIMESTAMP,
                    resolution_note = $4,
                    updated_at = CURRENT_TIMESTAMP
                WHERE id = $1
                RETURNING *
            )
            SELECT
                cr.id AS "id!: ContentReportId",
                cr.reporter_user_id AS "reporter_user_id!: UserId",
                COALESCE(reporter.username, '') AS "reporter_username!",
                cr.room_id AS "room_id?: RoomId",
                COALESCE(room_ctx.name, '') AS "room_name!",
                cr.target_type AS "target_type!: ContentReportTargetType",
                cr.target_room_id AS "target_room_id?: RoomId",
                COALESCE(target_room.name, '') AS "target_room_name!",
                cr.target_user_id AS "target_user_id?: UserId",
                COALESCE(target_user.username, '') AS "target_username!",
                cr.target_member_room_id AS "target_member_room_id?: RoomId",
                COALESCE(target_member_room.name, '') AS "target_member_room_name!",
                cr.target_member_user_id AS "target_member_user_id?: UserId",
                COALESCE(target_member_user.username, '') AS "target_member_username!",
                cr.target_chat_message_id,
                cr.target_chat_message_created_at,
                COALESCE(left(chat.content, 240), '') AS "target_chat_message_preview!",
                cr.reason_code,
                cr.reason,
                cr.metadata,
                cr.status AS "status!: ContentReportStatus",
                cr.reviewed_by AS "reviewed_by?: UserId",
                COALESCE(reviewer.username, '') AS "reviewed_by_username!",
                cr.reviewed_at,
                cr.resolution_note,
                cr.created_at,
                cr.updated_at
            FROM updated cr
            LEFT JOIN users reporter ON reporter.id = cr.reporter_user_id
            LEFT JOIN users reviewer ON reviewer.id = cr.reviewed_by
            LEFT JOIN rooms room_ctx ON room_ctx.id = cr.room_id
            LEFT JOIN rooms target_room ON target_room.id = cr.target_room_id
            LEFT JOIN users target_user ON target_user.id = cr.target_user_id
            LEFT JOIN rooms target_member_room ON target_member_room.id = cr.target_member_room_id
            LEFT JOIN users target_member_user ON target_member_user.id = cr.target_member_user_id
            LEFT JOIN chat_messages chat
              ON chat.room_id = cr.room_id
             AND chat.id = cr.target_chat_message_id
             AND chat.created_at = cr.target_chat_message_created_at
            "#,
            id.as_i64(),
            i16::from(status),
            reviewed_by.as_i64(),
            resolution_note,
        )
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| Error::NotFound("Content report not found".to_string()))?;
        Ok(updated)
    }
}

fn normalize_search(search: &str) -> Option<String> {
    let trimmed = search.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(format!("%{}%", trimmed.to_ascii_lowercase()))
    }
}

impl From<ContentReportListScope> for i16 {
    fn from(value: ContentReportListScope) -> Self {
        match value {
            ContentReportListScope::AnyRelated => 1,
            ContentReportListScope::RoomContext => 2,
            ContentReportListScope::TargetRoom => 3,
            ContentReportListScope::TargetUser => 4,
            ContentReportListScope::TargetMember => 5,
            ContentReportListScope::TargetChatMessage => 6,
        }
    }
}
