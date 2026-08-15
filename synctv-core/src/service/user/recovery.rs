use crate::{
    models::{DeletionSource, RoomId, RoomMember, RoomRole, User, UserId},
    repository::RoomMemberRepository,
    Error, Result,
};

use super::UserService;

#[derive(Debug, Clone, Copy, Default)]
pub struct UserRestoreOptions {
    pub ignore_identity_conflicts: bool,
    pub restored_by: Option<UserId>,
}

#[derive(Debug, Clone)]
pub struct UserRestoreResult {
    pub user: User,
    pub released_identities: Vec<String>,
    pub restored_room_ids: Vec<RoomId>,
}

fn map_concurrent_identity_conflict(error: sqlx::Error) -> Error {
    match error {
        sqlx::Error::Database(ref database_error)
            if database_error.code().as_deref() == Some("23505") =>
        {
            Error::AlreadyExists(
                "An account identity was claimed while restoration was in progress".to_string(),
            )
        }
        other => Error::Database(other),
    }
}

impl UserService {
    pub async fn find_deleted_user_id_by_username(&self, username: &str) -> Result<Option<UserId>> {
        sqlx::query_scalar!(
            r#"SELECT id AS "id: UserId"
               FROM users
               WHERE LOWER(username) = LOWER($1)
                 AND deleted_at IS NOT NULL
               ORDER BY deleted_at DESC
               LIMIT 1"#,
            username,
        )
        .fetch_optional(self.repository.pool())
        .await
        .map_err(Error::Database)
    }

    pub async fn restore_user(
        &self,
        user_id: &UserId,
        options: UserRestoreOptions,
    ) -> Result<UserRestoreResult> {
        let mut tx = self.repository.pool().begin().await?;
        let original_username = sqlx::query_scalar!(
            "SELECT username FROM users WHERE id = $1 AND deleted_at IS NOT NULL FOR UPDATE",
            user_id.as_i64(),
        )
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| {
            Error::NotFound("Deleted user not found or retention expired".to_string())
        })?;
        let mut restored_username = original_username.clone();
        let mut released_identities = Vec::new();

        let username_conflict = sqlx::query_scalar!(
            r#"SELECT EXISTS(
                   SELECT 1
                   FROM users
                   WHERE LOWER(username) = LOWER($1)
                     AND deleted_at IS NULL
                     AND id <> $2
               ) AS "exists!""#,
            &original_username,
            user_id.as_i64(),
        )
        .fetch_one(&mut *tx)
        .await?;
        if username_conflict {
            if !options.ignore_identity_conflicts {
                return Err(Error::AlreadyExists(format!(
                    "Username '{original_username}' is already occupied"
                )));
            }
            let base_username = format!("restored_{}", user_id.as_i64());
            restored_username = sqlx::query_scalar!(
                r#"SELECT candidate AS "candidate!"
                   FROM (
                       SELECT CASE
                           WHEN suffix = 0 THEN $1
                           ELSE $1 || '_' || suffix::TEXT
                       END AS candidate,
                       suffix
                       FROM generate_series(0, 1000) AS suffix
                   ) candidates
                   WHERE LENGTH(candidate) <= 50
                     AND NOT EXISTS (
                         SELECT 1
                         FROM users
                         WHERE LOWER(username) = LOWER(candidate)
                           AND deleted_at IS NULL
                     )
                   ORDER BY suffix
                   LIMIT 1"#,
                base_username,
            )
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| Error::AlreadyExists("No recovery username is available".to_string()))?;
            released_identities.push(format!("username:{original_username}"));
        }

        let deleted_emails = sqlx::query_scalar!(
            "SELECT email FROM auth_email_identities WHERE user_id = $1 AND deleted_at IS NOT NULL AND deletion_source = $2 FOR UPDATE",
            user_id.as_i64(),
            DeletionSource::Account as DeletionSource,
        )
        .fetch_all(&mut *tx)
        .await?;
        for email in deleted_emails {
            let conflict = sqlx::query_scalar!(
                r#"SELECT EXISTS(
                       SELECT 1
                       FROM auth_email_identities
                       WHERE LOWER(email) = LOWER($1)
                         AND deleted_at IS NULL
                         AND user_id <> $2
                   ) AS "exists!""#,
                &email,
                user_id.as_i64(),
            )
            .fetch_one(&mut *tx)
            .await?;
            if conflict {
                if !options.ignore_identity_conflicts {
                    return Err(Error::AlreadyExists(format!(
                        "Email '{email}' is already occupied"
                    )));
                }
                released_identities.push(format!("email:{email}"));
            } else {
                sqlx::query!(
                    "UPDATE auth_email_identities SET deleted_at = NULL, deletion_source = NULL, updated_at = CURRENT_TIMESTAMP WHERE user_id = $1 AND LOWER(email) = LOWER($2) AND deletion_source = $3",
                    user_id.as_i64(),
                    &email,
                    DeletionSource::Account as DeletionSource,
                )
                .execute(&mut *tx)
                .await
                .map_err(map_concurrent_identity_conflict)?;
            }
        }

        let deleted_oauth = sqlx::query!(
            "SELECT id, provider_instance_name, provider_user_id FROM auth_oauth2_identities WHERE user_id = $1 AND deleted_at IS NOT NULL AND deletion_source = $2 FOR UPDATE",
            user_id.as_i64(),
            DeletionSource::Account as DeletionSource,
        )
        .fetch_all(&mut *tx)
        .await?;
        for identity in deleted_oauth {
            let conflict = sqlx::query_scalar!(
                r#"SELECT EXISTS(
                       SELECT 1
                       FROM auth_oauth2_identities
                       WHERE provider_instance_name = $1
                         AND provider_user_id = $2
                         AND deleted_at IS NULL
                         AND user_id <> $3
                   ) AS "exists!""#,
                &identity.provider_instance_name,
                &identity.provider_user_id,
                user_id.as_i64(),
            )
            .fetch_one(&mut *tx)
            .await?;
            if conflict {
                if !options.ignore_identity_conflicts {
                    return Err(Error::AlreadyExists(format!(
                        "OAuth2 identity '{}:{}' is already occupied",
                        identity.provider_instance_name, identity.provider_user_id
                    )));
                }
                released_identities.push(format!(
                    "oauth2:{}:{}",
                    identity.provider_instance_name, identity.provider_user_id
                ));
            } else {
                sqlx::query!(
                    "UPDATE auth_oauth2_identities SET deleted_at = NULL, deletion_source = NULL, updated_at = CURRENT_TIMESTAMP WHERE id = $1 AND deletion_source = $2",
                    identity.id,
                    DeletionSource::Account as DeletionSource,
                )
                .execute(&mut *tx)
                .await
                .map_err(map_concurrent_identity_conflict)?;
            }
        }

        let restored_room_ids = sqlx::query_scalar!(
            r#"SELECT id AS "id: RoomId"
               FROM rooms
               WHERE deleted_owner_id = $1
                 AND deletion_source = $2
               ORDER BY id
               FOR UPDATE"#,
            user_id.as_i64(),
            DeletionSource::Account as DeletionSource,
        )
        .fetch_all(&mut *tx)
        .await?;

        // Account deletion also marks resources created in rooms owned by
        // other users. Capture every affected room before restoring rows so
        // room caches and live subscriptions observe the full aggregate.
        let affected_room_ids = sqlx::query_scalar!(
            r#"
            SELECT room_id AS "room_id!: RoomId" FROM (
                SELECT id AS room_id
                FROM rooms
                WHERE deleted_owner_id = $1
                  AND deletion_source = $2
                UNION
                SELECT room_id
                FROM playlists
                WHERE deleted_owner_id = $1
                  AND deletion_source = $2
                UNION
                SELECT room_id
                FROM media
                WHERE deleted_owner_id = $1
                  AND deletion_source = $2
                UNION
                SELECT room_id
                FROM chat_messages
                WHERE deleted_owner_id = $1
                  AND deletion_source = $2
            ) affected
            ORDER BY room_id
            "#,
            user_id.as_i64(),
            DeletionSource::Account as DeletionSource,
        )
        .fetch_all(&mut *tx)
        .await?;

        // Creator membership is part of the recoverable owned-room aggregate.
        // Other former members use the normal admission flow after recovery.
        let room_member_repository = RoomMemberRepository::new(self.repository.pool().clone());
        let mut restored_members = Vec::with_capacity(restored_room_ids.len());
        for room_id in &restored_room_ids {
            let member = RoomMember::new(*room_id, *user_id, RoomRole::Creator);
            let member = room_member_repository
                .add_with_executor(&member, &mut tx)
                .await?;
            restored_members.push((member.room_id, member.version));
        }

        sqlx::query!(
            "UPDATE rooms SET deleted_at = NULL, deletion_source = NULL, deleted_owner_id = NULL, updated_at = CURRENT_TIMESTAMP, version = version + 1 WHERE deleted_owner_id = $1 AND deletion_source = $2",
            user_id.as_i64(),
            DeletionSource::Account as DeletionSource,
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query!(
            "UPDATE playlists SET deleted_at = NULL, deletion_source = NULL, deleted_owner_id = NULL, updated_at = CURRENT_TIMESTAMP, version = version + 1 WHERE deleted_owner_id = $1 AND deletion_source = $2",
            user_id.as_i64(),
            DeletionSource::Account as DeletionSource,
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query!(
            "UPDATE media SET deleted_at = NULL, deletion_source = NULL, deleted_owner_id = NULL, updated_at = CURRENT_TIMESTAMP, version = version + 1 WHERE deleted_owner_id = $1 AND deletion_source = $2",
            user_id.as_i64(),
            DeletionSource::Account as DeletionSource,
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query!(
            "UPDATE chat_messages SET deleted_at = NULL, deletion_source = NULL, deleted_owner_id = NULL, deleted_by = NULL, delete_reason = NULL, version = version + 1 WHERE deleted_owner_id = $1 AND deletion_source = $2",
            user_id.as_i64(),
            DeletionSource::Account as DeletionSource,
        )
        .execute(&mut *tx)
        .await?;

        sqlx::query!(
            "UPDATE users SET username = $2, deleted_at = NULL, deletion_source = NULL, deletion_reason = NULL, deleted_by = NULL, restored_at = CURRENT_TIMESTAMP, restored_by = $3, updated_at = CURRENT_TIMESTAMP, version = version + 1 WHERE id = $1",
            user_id.as_i64(),
            &restored_username,
            options.restored_by.map(|id| id.as_i64()),
        )
        .execute(&mut *tx)
        .await
        .map_err(map_concurrent_identity_conflict)?;

        tx.commit().await?;
        self.invalidate_username_cache_best_effort(user_id, "restore_user")
            .await;
        self.notify_user_invalidation(user_id).await;
        for (room_id, member_version) in restored_members {
            if let Some(permission_service) = &self.permission_service {
                permission_service
                    .seed_added_member_cache(&room_id, user_id, member_version)
                    .await;
                permission_service.invalidate_room_cache(&room_id).await;
            }
        }
        if let Some(cache_invalidation) = &self.cache_invalidation {
            for room_id in &affected_room_ids {
                if let Err(error) = cache_invalidation
                    .invalidate_and_broadcast_room(room_id)
                    .await
                {
                    tracing::warn!(
                        error = %error,
                        room_id = %room_id,
                        user_id = %user_id,
                        "Failed to invalidate restored room cache"
                    );
                }
            }
        }
        let user = self.get_user(user_id).await?;

        Ok(UserRestoreResult {
            user,
            released_identities,
            restored_room_ids,
        })
    }
}
