use sqlx::PgPool;

use crate::repository::{pools::RepoPools, query_builder::ilike_contains_pattern};
use crate::{
    models::{
        ContentReport, ContentReportAdminRow, ContentReportId, ContentReportMetadata,
        ContentReportStatus, ContentReportTarget, ContentReportTargetType, CreateContentReport,
        RoomId, UserId,
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

struct ContentReportScopeFilter {
    matches_all_target_types: bool,
    target_types: Vec<i16>,
    matches_any_related_room: bool,
    matches_target_room: bool,
    matches_target_member: bool,
    matches_chat_message: bool,
}

impl ContentReportListScope {
    fn filter(self) -> ContentReportScopeFilter {
        match self {
            Self::AnyRelated => ContentReportScopeFilter {
                matches_all_target_types: true,
                target_types: Vec::new(),
                matches_any_related_room: true,
                matches_target_room: false,
                matches_target_member: false,
                matches_chat_message: false,
            },
            Self::RoomContext => ContentReportScopeFilter {
                matches_all_target_types: false,
                target_types: content_report_target_types([
                    ContentReportTargetType::Room,
                    ContentReportTargetType::RoomMember,
                    ContentReportTargetType::ChatMessage,
                ]),
                matches_any_related_room: true,
                matches_target_room: false,
                matches_target_member: false,
                matches_chat_message: false,
            },
            Self::TargetRoom => ContentReportScopeFilter {
                matches_all_target_types: false,
                target_types: content_report_target_types([ContentReportTargetType::Room]),
                matches_any_related_room: false,
                matches_target_room: true,
                matches_target_member: false,
                matches_chat_message: false,
            },
            Self::TargetUser => ContentReportScopeFilter {
                matches_all_target_types: false,
                target_types: content_report_target_types([ContentReportTargetType::User]),
                matches_any_related_room: false,
                matches_target_room: false,
                matches_target_member: false,
                matches_chat_message: false,
            },
            Self::TargetMember => ContentReportScopeFilter {
                matches_all_target_types: false,
                target_types: content_report_target_types([ContentReportTargetType::RoomMember]),
                matches_any_related_room: false,
                matches_target_room: false,
                matches_target_member: true,
                matches_chat_message: false,
            },
            Self::TargetChatMessage => ContentReportScopeFilter {
                matches_all_target_types: false,
                target_types: content_report_target_types([ContentReportTargetType::ChatMessage]),
                matches_any_related_room: false,
                matches_target_room: false,
                matches_target_member: false,
                matches_chat_message: true,
            },
        }
    }
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
    pools: RepoPools,
}

impl ContentReportRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self {
            pools: RepoPools::new(pool),
        }
    }

    #[must_use]
    pub const fn new_with_read_pool(pool: PgPool, read_pool: PgPool) -> Self {
        Self {
            pools: RepoPools::with_read(pool, read_pool),
        }
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
        let metadata = request.metadata.filter(|metadata| !metadata.is_empty());

        let report = sqlx::query_as!(
            ContentReport,
            r#"
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
                id AS "id!: ContentReportId",
                reporter_user_id AS "reporter_user_id!: UserId",
                room_id AS "room_id?: RoomId",
                target_type AS "target_type!: ContentReportTargetType",
                target_room_id AS "target_room_id?: RoomId",
                target_user_id AS "target_user_id?: UserId",
                target_member_room_id AS "target_member_room_id?: RoomId",
                target_member_user_id AS "target_member_user_id?: UserId",
                target_chat_message_id,
                target_chat_message_created_at,
                reason_code,
                reason,
                metadata AS "metadata?: ContentReportMetadata",
                status AS "status!: ContentReportStatus",
                reviewed_by AS "reviewed_by?: UserId",
                reviewed_at,
                resolution_note,
                created_at,
                updated_at
            "#,
            request.reporter_user_id as UserId,
            room_id.map(i64::from),
            i16::from(target_type),
            target_room_id.map(i64::from),
            target_user_id.map(i64::from),
            target_member_room_id.map(i64::from),
            target_member_user_id.map(i64::from),
            target_chat_message_id,
            chat_message_created_at,
            request.reason_code,
            request.reason,
            &metadata as _,
            i16::from(ContentReportStatus::Open),
        )
        .fetch_one(self.pools.primary())
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
        let scope_filter = query.scope.filter();
        let search = ilike_contains_pattern(&query.search);
        let pool = self.pools.read();

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
              AND ($10::boolean OR cr.target_type = ANY($11::smallint[]))
              AND (
                $4::bigint IS NULL
                OR ($12::boolean AND (cr.room_id = $4 OR cr.target_room_id = $4 OR cr.target_member_room_id = $4))
                OR ($13::boolean AND cr.target_room_id = $4)
                OR ($14::boolean AND cr.target_member_room_id = $4)
                OR ($15::boolean AND cr.room_id = $4)
              )
              AND ($5::bigint IS NULL OR cr.target_room_id = $5)
              AND ($6::bigint IS NULL OR cr.target_user_id = $6)
              AND ($7::bigint IS NULL OR cr.target_member_room_id = $7)
              AND ($8::bigint IS NULL OR cr.target_member_user_id = $8)
              AND ($9::bigint IS NULL OR cr.target_chat_message_id = $9)
              AND (
                $16::text IS NULL
                OR cr.reason_code ILIKE $16 ESCAPE '\'
                OR cr.reason ILIKE $16 ESCAPE '\'
                OR COALESCE(reporter.username, '') ILIKE $16 ESCAPE '\'
                OR COALESCE(room_ctx.name, '') ILIKE $16 ESCAPE '\'
                OR COALESCE(target_room.name, '') ILIKE $16 ESCAPE '\'
                OR COALESCE(target_user.username, '') ILIKE $16 ESCAPE '\'
                OR COALESCE(target_member_user.username, '') ILIKE $16 ESCAPE '\'
                OR COALESCE(chat.content, '') ILIKE $16 ESCAPE '\'
                OR cr.id::text ILIKE $16 ESCAPE '\'
                OR cr.target_chat_message_id::text ILIKE $16 ESCAPE '\'
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
            scope_filter.matches_all_target_types,
            &scope_filter.target_types,
            scope_filter.matches_any_related_room,
            scope_filter.matches_target_room,
            scope_filter.matches_target_member,
            scope_filter.matches_chat_message,
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
                cr.reason_code AS "reason_code!",
                cr.reason AS "reason!",
                cr.metadata AS "metadata?: ContentReportMetadata",
                cr.status AS "status!: ContentReportStatus",
                cr.reviewed_by AS "reviewed_by?: UserId",
                COALESCE(reviewer.username, '') AS "reviewed_by_username!",
                cr.reviewed_at AS "reviewed_at?",
                cr.resolution_note AS "resolution_note!",
                cr.created_at AS "created_at!",
                cr.updated_at AS "updated_at!"
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
              AND ($10::boolean OR cr.target_type = ANY($11::smallint[]))
              AND (
                $4::bigint IS NULL
                OR ($12::boolean AND (cr.room_id = $4 OR cr.target_room_id = $4 OR cr.target_member_room_id = $4))
                OR ($13::boolean AND cr.target_room_id = $4)
                OR ($14::boolean AND cr.target_member_room_id = $4)
                OR ($15::boolean AND cr.room_id = $4)
              )
              AND ($5::bigint IS NULL OR cr.target_room_id = $5)
              AND ($6::bigint IS NULL OR cr.target_user_id = $6)
              AND ($7::bigint IS NULL OR cr.target_member_room_id = $7)
              AND ($8::bigint IS NULL OR cr.target_member_user_id = $8)
              AND ($9::bigint IS NULL OR cr.target_chat_message_id = $9)
              AND (
                $16::text IS NULL
                OR cr.reason_code ILIKE $16 ESCAPE '\'
                OR cr.reason ILIKE $16 ESCAPE '\'
                OR COALESCE(reporter.username, '') ILIKE $16 ESCAPE '\'
                OR COALESCE(room_ctx.name, '') ILIKE $16 ESCAPE '\'
                OR COALESCE(target_room.name, '') ILIKE $16 ESCAPE '\'
                OR COALESCE(target_user.username, '') ILIKE $16 ESCAPE '\'
                OR COALESCE(target_member_user.username, '') ILIKE $16 ESCAPE '\'
                OR COALESCE(chat.content, '') ILIKE $16 ESCAPE '\'
                OR cr.id::text ILIKE $16 ESCAPE '\'
                OR cr.target_chat_message_id::text ILIKE $16 ESCAPE '\'
              )
            ORDER BY cr.created_at DESC, cr.id DESC
            LIMIT $17 OFFSET $18
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
            scope_filter.matches_all_target_types,
            &scope_filter.target_types,
            scope_filter.matches_any_related_room,
            scope_filter.matches_target_room,
            scope_filter.matches_target_member,
            scope_filter.matches_chat_message,
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
                cr.reason_code AS "reason_code!",
                cr.reason AS "reason!",
                cr.metadata AS "metadata?: ContentReportMetadata",
                cr.status AS "status!: ContentReportStatus",
                cr.reviewed_by AS "reviewed_by?: UserId",
                COALESCE(reviewer.username, '') AS "reviewed_by_username!",
                cr.reviewed_at AS "reviewed_at?",
                cr.resolution_note AS "resolution_note!",
                cr.created_at AS "created_at!",
                cr.updated_at AS "updated_at!"
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
        .fetch_optional(self.pools.primary())
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
                cr.reason_code AS "reason_code!",
                cr.reason AS "reason!",
                cr.metadata AS "metadata?: ContentReportMetadata",
                cr.status AS "status!: ContentReportStatus",
                cr.reviewed_by AS "reviewed_by?: UserId",
                COALESCE(reviewer.username, '') AS "reviewed_by_username!",
                cr.reviewed_at AS "reviewed_at?",
                cr.resolution_note AS "resolution_note!",
                cr.created_at AS "created_at!",
                cr.updated_at AS "updated_at!"
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
        .fetch_optional(self.pools.primary())
        .await?
        .ok_or_else(|| Error::NotFound("Content report not found".to_string()))?;
        Ok(updated)
    }
}

fn content_report_target_types<const N: usize>(values: [ContentReportTargetType; N]) -> Vec<i16> {
    values.into_iter().map(i16::from).collect()
}
