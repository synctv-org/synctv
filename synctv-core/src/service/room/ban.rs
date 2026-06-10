use crate::{
    models::{AuditAction, AuditTargetType, Room, RoomId, UserId},
    repository::realtime_outbox::NewRealtimeOutboxEvent,
    service::RoomService,
    Error, Result,
};

impl RoomService {
    pub async fn ban_room(&self, room_id: &RoomId, admin_user_id: &UserId) -> Result<Room> {
        self.ban_room_with_outbox(room_id, admin_user_id, None)
            .await
    }

    pub async fn ban_room_with_outbox(
        &self,
        room_id: &RoomId,
        admin_user_id: &UserId,
        outbox_event: Option<NewRealtimeOutboxEvent>,
    ) -> Result<Room> {
        let room = self
            .room_repo
            .get_by_id(room_id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;

        if room.is_banned {
            return Err(Error::InvalidInput("Room is already banned".to_string()));
        }

        let mut tx = self.pool.begin().await?;
        let updated_room = crate::repository::RoomRepository::update_ban_status_with_executor(
            room_id, true, &mut tx,
        )
        .await?;
        self.insert_realtime_outbox_tx(&mut tx, outbox_event.as_ref())
            .await?;
        tx.commit().await?;
        self.notify_room_invalidation(room_id).await;

        self.write_audit_event(
            admin_user_id,
            &admin_user_id.to_string(),
            AuditAction::RoomBanned,
            AuditTargetType::Room,
            Some(room_id.to_string()),
            serde_json::json!({"reason": "Room banned by admin"}),
        )
        .await?;

        Ok(updated_room)
    }

    pub async fn unban_room(&self, room_id: &RoomId, admin_user_id: &UserId) -> Result<Room> {
        let room = self
            .room_repo
            .get_by_id(room_id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;

        if !room.is_banned {
            return Err(Error::InvalidInput("Room is not banned".to_string()));
        }

        let updated_room = self.room_repo.update_ban_status(room_id, false).await?;
        self.notify_room_invalidation(room_id).await;

        self.write_audit_event(
            admin_user_id,
            &admin_user_id.to_string(),
            AuditAction::RoomUnbanned,
            AuditTargetType::Room,
            Some(room_id.to_string()),
            serde_json::json!({"reason": "Room unbanned by admin"}),
        )
        .await?;

        Ok(updated_room)
    }

    pub async fn batch_ban_rooms(
        &self,
        room_ids: &[RoomId],
        admin_user_id: &UserId,
    ) -> crate::Result<Vec<(RoomId, crate::Result<()>)>> {
        if room_ids.is_empty() {
            return Err(Error::InvalidInput("room_ids cannot be empty".to_string()));
        }
        if room_ids.len() > Self::BATCH_SIZE_LIMIT {
            return Err(Error::InvalidInput(format!(
                "Batch size {} exceeds limit of {}",
                room_ids.len(),
                Self::BATCH_SIZE_LIMIT
            )));
        }

        let mut results = Vec::with_capacity(room_ids.len());

        for room_id in room_ids {
            let result = self.ban_room(room_id, admin_user_id).await.map(|_| ());
            results.push((*room_id, result));
        }

        Ok(results)
    }
}
