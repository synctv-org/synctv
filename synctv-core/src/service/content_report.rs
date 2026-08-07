use sqlx::PgPool;

use crate::{
    models::{
        ContentReport, ContentReportAdminRow, ContentReportId, ContentReportStatus,
        ContentReportTarget, CreateContentReport, UserId,
    },
    repository::{
        ChatRepository, ContentReportRepository, RoomMemberRepository, RoomRepository,
        UserRepository,
    },
    Error, Result,
};

pub use crate::repository::{ContentReportListQuery, ContentReportListScope, ContentReportPage};

#[derive(Clone)]
pub struct ContentReportService {
    reports: ContentReportRepository,
    rooms: RoomRepository,
    users: UserRepository,
    members: RoomMemberRepository,
    chat: ChatRepository,
}

impl ContentReportService {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self {
            reports: ContentReportRepository::new(pool.clone()),
            rooms: RoomRepository::new(pool.clone()),
            users: UserRepository::new(pool.clone()),
            members: RoomMemberRepository::new(pool.clone()),
            chat: ChatRepository::new(pool),
        }
    }

    #[must_use]
    pub fn new_with_read_pool(pool: PgPool, read_pool: PgPool) -> Self {
        Self {
            reports: ContentReportRepository::new_with_read_pool(pool.clone(), read_pool.clone()),
            rooms: RoomRepository::new(pool.clone()),
            users: UserRepository::new(pool.clone()),
            members: RoomMemberRepository::new(pool.clone()),
            chat: ChatRepository::new_with_read_pool(pool, read_pool),
        }
    }

    pub async fn create_report(&self, mut request: CreateContentReport) -> Result<ContentReport> {
        normalize_report_text(&mut request)?;
        let chat_message_created_at = self.validate_target(&request.target).await?;
        self.reports.create(request, chat_message_created_at).await
    }

    pub async fn list_reports(
        &self,
        mut query: ContentReportListQuery,
    ) -> Result<ContentReportPage> {
        query.search = query.search.trim().to_string();
        if query.limit <= 0 || query.limit > 100 {
            return Err(Error::InvalidInput(
                "report list limit must be between 1 and 100".to_string(),
            ));
        }
        if query.offset < 0 {
            return Err(Error::InvalidInput(
                "report list offset must be at least 0".to_string(),
            ));
        }
        self.reports.list_admin(&query).await
    }

    pub async fn get_report(&self, id: ContentReportId) -> Result<ContentReportAdminRow> {
        self.reports
            .get_admin(id)
            .await?
            .ok_or_else(|| Error::NotFound("Content report not found".to_string()))
    }

    pub async fn update_status(
        &self,
        id: ContentReportId,
        status: ContentReportStatus,
        reviewed_by: UserId,
        resolution_note: String,
    ) -> Result<ContentReportAdminRow> {
        let note = normalize_resolution_note(&resolution_note)?;
        self.reports
            .update_status(id, status, reviewed_by, note)
            .await
    }

    async fn validate_target(
        &self,
        target: &ContentReportTarget,
    ) -> Result<Option<chrono::DateTime<chrono::Utc>>> {
        match target {
            ContentReportTarget::Room { room_id } => {
                self.rooms
                    .get_by_id(room_id)
                    .await?
                    .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;
                Ok(None)
            }
            ContentReportTarget::User { user_id } => {
                self.users
                    .get_by_id(user_id)
                    .await?
                    .ok_or_else(|| Error::NotFound("User not found".to_string()))?;
                Ok(None)
            }
            ContentReportTarget::RoomMember { room_id, user_id } => {
                self.rooms
                    .get_by_id(room_id)
                    .await?
                    .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;
                self.members
                    .get_any(room_id, user_id)
                    .await?
                    .ok_or_else(|| Error::NotFound("Room member not found".to_string()))?;
                Ok(None)
            }
            ContentReportTarget::ChatMessage {
                room_id,
                message_id,
            } => {
                let message = self
                    .chat
                    .get_by_room_and_id(room_id, *message_id)
                    .await?
                    .ok_or_else(|| Error::NotFound("Chat message not found".to_string()))?;
                Ok(Some(message.created_at))
            }
        }
    }
}

fn normalize_report_text(request: &mut CreateContentReport) -> Result<()> {
    request.reason_code = request.reason_code.trim().to_ascii_lowercase();
    request.reason = request.reason.trim().to_string();
    if request.reason_code.is_empty() || request.reason_code.len() > 64 {
        return Err(Error::InvalidInput(
            "report reason_code must be between 1 and 64 characters".to_string(),
        ));
    }
    if request.reason.len() > 2000 {
        return Err(Error::InvalidInput(
            "report reason must be at most 2000 characters".to_string(),
        ));
    }
    Ok(())
}

fn normalize_resolution_note(note: &str) -> Result<String> {
    let note = note.trim().to_string();
    if note.len() > 2000 {
        return Err(Error::InvalidInput(
            "report resolution_note must be at most 2000 characters".to_string(),
        ));
    }
    Ok(note)
}
