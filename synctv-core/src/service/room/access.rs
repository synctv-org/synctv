use sqlx::PgPool;
use std::sync::Arc;

use crate::{
    models::{
        MemberStatus, PageParams, Room, RoomId, RoomListQuery, RoomPermission, RoomRole,
        RoomSettings, RoomWithCount, UserId,
    },
    service::{
        room::RoomService, room_settings::RoomSettingsService, user::UserService,
        PermissionService, PlaylistService,
    },
    Error, Result,
};

impl RoomService {
    #[must_use]
    pub const fn pool(&self) -> &PgPool {
        &self.pool
    }

    #[must_use]
    pub const fn playlist_service(&self) -> &PlaylistService {
        &self.playlist_service
    }

    #[must_use]
    pub fn file_storage_service(&self) -> Option<&Arc<dyn crate::service::FileStorageService>> {
        self.room_file_storage_service.as_ref()
    }

    #[must_use]
    pub const fn permission_service(&self) -> &PermissionService {
        &self.permission_service
    }

    #[must_use]
    pub const fn room_settings_service(&self) -> &RoomSettingsService {
        &self.room_settings_service
    }

    #[must_use]
    pub const fn user_service(&self) -> &UserService {
        &self.user_service
    }

    pub async fn room_exists(&self, room_id: &RoomId) -> Result<bool> {
        self.room_repo.exists(room_id).await
    }

    pub async fn get_room(&self, room_id: &RoomId) -> Result<Room> {
        let mut room = self
            .room_repo
            .get_by_id(room_id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;
        self.hydrate_room_taxonomy(&mut room).await?;
        Ok(room)
    }

    pub async fn get_room_with_settings(&self, room_id: &RoomId) -> Result<(Room, RoomSettings)> {
        let room = self.get_room(room_id).await?;
        let settings = self.room_settings_service.get(room_id).await?;
        Ok((room, settings))
    }

    pub async fn get_room_settings(&self, room_id: &RoomId) -> Result<RoomSettings> {
        self.room_settings_service.get(room_id).await
    }

    pub async fn get_room_settings_with_version(
        &self,
        room_id: &RoomId,
    ) -> Result<(RoomSettings, i64)> {
        let snapshot = self.room_settings_service.get_with_version(room_id).await?;
        Ok((snapshot.settings, snapshot.version))
    }

    pub async fn get_room_guest_version(&self, room_id: &RoomId) -> Result<i64> {
        let key = self
            .user_service
            .key_builder()
            .room_guest_version(&room_id.to_string());
        Ok(self
            .user_service
            .token_blacklist_store()
            .get_version_checked(&key)
            .await?
            .unwrap_or(0))
    }

    pub(crate) async fn resolve_actor_username(&self, user_id: &UserId) -> Result<String> {
        self.user_service
            .get_user(user_id)
            .await
            .map(|user| user.username)
    }

    pub async fn get_room_settings_batch(
        &self,
        room_ids: &[RoomId],
    ) -> Result<std::collections::HashMap<RoomId, RoomSettings>> {
        self.room_settings_repo.get_batch(room_ids).await
    }

    pub async fn update_room_description(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        description: String,
    ) -> Result<Room> {
        if description.chars().count() > 500 {
            return Err(Error::InvalidInput(
                "Room description too long (max 500 characters)".to_string(),
            ));
        }

        self.permission_service
            .check_permission(room_id, user_id, RoomPermission::SET_ROOM_SETTINGS)
            .await?;

        let room = self
            .room_repo
            .update_description(room_id, &description)
            .await?;
        self.notify_room_invalidation(room_id).await;
        Ok(room)
    }

    pub async fn list_rooms(&self, query: &RoomListQuery) -> Result<(Vec<Room>, i64)> {
        query.pagination.validate()?;
        let (mut rooms, total) = self.room_repo.list(query).await?;
        self.hydrate_rooms_taxonomy(&mut rooms).await?;
        Ok((rooms, total))
    }

    pub async fn list_active_unbanned_rooms_by_ids(
        &self,
        room_ids: &[RoomId],
    ) -> Result<Vec<Room>> {
        let mut rooms = self.room_repo.list_active_unbanned_by_ids(room_ids).await?;
        self.hydrate_rooms_taxonomy(&mut rooms).await?;
        Ok(rooms)
    }

    pub async fn list_accessible_rooms(&self, query: &RoomListQuery) -> Result<(Vec<Room>, i64)> {
        query.pagination.validate()?;
        let (mut rooms, total) = self.room_repo.list_accessible(query).await?;
        self.hydrate_rooms_taxonomy(&mut rooms).await?;
        Ok((rooms, total))
    }

    pub async fn list_related_rooms_for_user(
        &self,
        user_id: &UserId,
        query: &RoomListQuery,
    ) -> Result<(Vec<Room>, i64)> {
        query.pagination.validate()?;
        let (mut rooms, total) = self.room_repo.list_related_to_user(user_id, query).await?;
        self.hydrate_rooms_taxonomy(&mut rooms).await?;
        Ok((rooms, total))
    }

    pub async fn list_rooms_with_count(
        &self,
        query: &RoomListQuery,
    ) -> Result<(Vec<RoomWithCount>, i64)> {
        query.pagination.validate()?;
        let (mut rooms, total) = self.room_repo.list_with_count(query).await?;
        self.hydrate_room_with_count_items(&mut rooms).await?;
        Ok((rooms, total))
    }

    pub async fn list_rooms_by_creator(
        &self,
        creator_id: &UserId,
        pagination: PageParams,
    ) -> Result<(Vec<Room>, i64)> {
        pagination.validate()?;
        let (mut rooms, total) = self
            .room_repo
            .list_by_creator(creator_id, pagination)
            .await?;
        self.hydrate_rooms_taxonomy(&mut rooms).await?;
        Ok((rooms, total))
    }

    pub async fn list_rooms_by_creator_with_count(
        &self,
        creator_id: &UserId,
        pagination: PageParams,
    ) -> Result<(Vec<RoomWithCount>, i64)> {
        pagination.validate()?;
        let (mut rooms, total) = self
            .room_repo
            .list_by_creator_with_count(creator_id, pagination)
            .await?;
        self.hydrate_room_with_count_items(&mut rooms).await?;
        Ok((rooms, total))
    }

    pub async fn list_joined_rooms(
        &self,
        user_id: &UserId,
        pagination: PageParams,
    ) -> Result<(Vec<RoomId>, i64)> {
        pagination.validate()?;
        self.member_service
            .list_user_rooms(user_id, pagination)
            .await
    }

    pub async fn list_joined_rooms_with_details(
        &self,
        user_id: &UserId,
        pagination: PageParams,
    ) -> Result<(Vec<(Room, RoomRole, MemberStatus, i32)>, i64)> {
        pagination.validate()?;
        let (mut rooms, total) = self
            .member_service
            .list_user_rooms_with_details(user_id, pagination)
            .await?;
        self.hydrate_room_member_items(&mut rooms).await?;
        Ok((rooms, total))
    }

    pub async fn list_joined_rooms_with_query(
        &self,
        user_id: &UserId,
        query: &crate::models::MyRoomListQuery,
    ) -> Result<(Vec<(Room, RoomRole, MemberStatus, i32)>, i64)> {
        query.pagination.validate()?;
        let (mut rooms, total) = self
            .member_service
            .list_user_rooms_with_details_query(user_id, query)
            .await?;
        self.hydrate_room_member_items(&mut rooms).await?;
        Ok((rooms, total))
    }

    pub async fn list_accessible_joined_rooms_with_query(
        &self,
        user_id: &UserId,
        query: &crate::models::MyRoomListQuery,
    ) -> Result<(Vec<(Room, RoomRole, MemberStatus, i32)>, i64)> {
        query.pagination.validate()?;
        let (mut rooms, total) = self
            .member_repo
            .list_accessible_by_user_with_query(user_id, query)
            .await?;
        self.hydrate_room_member_items(&mut rooms).await?;
        Ok((rooms, total))
    }

    async fn hydrate_room_with_count_items(&self, items: &mut [RoomWithCount]) -> Result<()> {
        let mut rooms: Vec<Room> = items.iter().map(|item| item.room.clone()).collect();
        self.hydrate_rooms_taxonomy(&mut rooms).await?;
        for (item, room) in items.iter_mut().zip(rooms) {
            item.room = room;
        }
        Ok(())
    }

    async fn hydrate_room_member_items(
        &self,
        items: &mut [(Room, RoomRole, MemberStatus, i32)],
    ) -> Result<()> {
        let mut rooms: Vec<Room> = items.iter().map(|(room, _, _, _)| room.clone()).collect();
        self.hydrate_rooms_taxonomy(&mut rooms).await?;
        for ((room, _, _, _), hydrated) in items.iter_mut().zip(rooms) {
            *room = hydrated;
        }
        Ok(())
    }
}
