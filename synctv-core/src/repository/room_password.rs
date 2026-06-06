use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::{
    models::{OpaquePasswordRecord, RoomId},
    Error, Result,
};

#[derive(sqlx::FromRow)]
struct RoomPasswordCredentialStateRow {
    room_id: RoomId,
    enabled: bool,
    changed_at: DateTime<Utc>,
    version: i32,
}

struct RoomOpaqueCredentialRow {
    room_id: RoomId,
    opaque_record: Vec<u8>,
    opaque_credential_identifier: Vec<u8>,
    opaque_ciphersuite: String,
    opaque_server_setup_version: i32,
    enabled: bool,
    changed_at: DateTime<Utc>,
    version: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoomPasswordCredentialState {
    pub room_id: RoomId,
    pub enabled: bool,
    pub changed_at: DateTime<Utc>,
    pub version: i32,
}

impl From<RoomPasswordCredentialStateRow> for RoomPasswordCredentialState {
    fn from(row: RoomPasswordCredentialStateRow) -> Self {
        Self {
            room_id: row.room_id,
            enabled: row.enabled,
            changed_at: row.changed_at,
            version: row.version,
        }
    }
}

#[derive(Debug, Clone)]
pub struct StoredRoomPasswordCredential {
    pub record: OpaquePasswordRecord,
    pub state: RoomPasswordCredentialState,
}

#[derive(Clone)]
pub struct RoomPasswordRepository {
    pool: PgPool,
}

impl RoomPasswordRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    #[must_use]
    pub const fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub async fn get_opaque_credential(
        &self,
        room_id: &RoomId,
    ) -> Result<Option<StoredRoomPasswordCredential>> {
        let row = sqlx::query_as!(
            RoomOpaqueCredentialRow,
            r#"
            SELECT room_id AS "room_id: RoomId",
                   opaque_record AS "opaque_record!",
                   opaque_credential_identifier AS "opaque_credential_identifier!",
                   opaque_ciphersuite AS "opaque_ciphersuite!",
                   opaque_server_setup_version AS "opaque_server_setup_version!",
                   enabled,
                   changed_at,
                   version
            FROM room_password_credentials
            WHERE room_id = $1
              AND opaque_record IS NOT NULL
              AND opaque_credential_identifier IS NOT NULL
              AND opaque_ciphersuite IS NOT NULL
              AND opaque_server_setup_version IS NOT NULL
            "#,
            room_id as &RoomId
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|row| StoredRoomPasswordCredential {
            record: OpaquePasswordRecord {
                record: row.opaque_record,
                credential_identifier: row.opaque_credential_identifier,
                ciphersuite: row.opaque_ciphersuite,
                server_setup_version: row.opaque_server_setup_version,
            },
            state: RoomPasswordCredentialState {
                room_id: row.room_id,
                enabled: row.enabled,
                changed_at: row.changed_at,
                version: row.version,
            },
        }))
    }

    pub async fn get_state(&self, room_id: &RoomId) -> Result<Option<RoomPasswordCredentialState>> {
        let row = sqlx::query_as!(
            RoomPasswordCredentialStateRow,
            r#"
            SELECT room_id AS "room_id: RoomId", enabled, changed_at, version
            FROM room_password_credentials
            WHERE room_id = $1
            "#,
            room_id as &RoomId
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(Into::into))
    }

    pub async fn set_opaque_credential_with_executor<'e, E>(
        &self,
        room_id: &RoomId,
        opaque_record: &OpaquePasswordRecord,
        executor: E,
    ) -> Result<RoomPasswordCredentialState>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let now = Utc::now();
        let row = sqlx::query_as!(
            RoomPasswordCredentialStateRow,
            r#"
            WITH existing_room AS (
                SELECT id
                FROM rooms
                WHERE id = $1 AND deleted_at IS NULL
            )
            INSERT INTO room_password_credentials (
                room_id, opaque_record, opaque_credential_identifier, opaque_ciphersuite,
                opaque_server_setup_version, enabled, changed_at, version, created_at, updated_at
            )
            SELECT r.id, $2, $3, $4, $5, true, $6, COALESCE(existing_rpc.version, 0) + 1, $6, $6
            FROM existing_room r
            LEFT JOIN room_password_credentials existing_rpc ON existing_rpc.room_id = r.id
            ON CONFLICT (room_id) DO UPDATE
            SET opaque_record = EXCLUDED.opaque_record,
                opaque_credential_identifier = EXCLUDED.opaque_credential_identifier,
                opaque_ciphersuite = EXCLUDED.opaque_ciphersuite,
                opaque_server_setup_version = EXCLUDED.opaque_server_setup_version,
                enabled = true,
                changed_at = EXCLUDED.changed_at,
                version = room_password_credentials.version + 1,
                updated_at = EXCLUDED.updated_at
            RETURNING room_id AS "room_id!: RoomId", enabled, changed_at, version
            "#,
            room_id as &RoomId,
            opaque_record.record.as_slice(),
            opaque_record.credential_identifier.as_slice(),
            opaque_record.ciphersuite.as_str(),
            opaque_record.server_setup_version,
            now
        )
        .fetch_optional(executor)
        .await?
        .ok_or_else(|| Error::NotFound(format!("Room {room_id} not found")))?;

        Ok(row.into())
    }

    pub async fn disable_with_executor<'e, E>(
        &self,
        room_id: &RoomId,
        executor: E,
    ) -> Result<RoomPasswordCredentialState>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let now = Utc::now();
        let row = sqlx::query_as!(
            RoomPasswordCredentialStateRow,
            r#"
            WITH existing_room AS (
                SELECT id
                FROM rooms
                WHERE id = $1 AND deleted_at IS NULL
            )
            INSERT INTO room_password_credentials (
                room_id, enabled, changed_at, version, created_at, updated_at
            )
            SELECT r.id, false, $2, COALESCE(existing_rpc.version, 0) + 1, $2, $2
            FROM existing_room r
            LEFT JOIN room_password_credentials existing_rpc ON existing_rpc.room_id = r.id
            ON CONFLICT (room_id) DO UPDATE
            SET enabled = false,
                changed_at = EXCLUDED.changed_at,
                version = room_password_credentials.version + 1,
                updated_at = EXCLUDED.updated_at
            RETURNING room_id AS "room_id!: RoomId", enabled, changed_at, version
            "#,
            room_id as &RoomId,
            now
        )
        .fetch_optional(executor)
        .await?
        .ok_or_else(|| Error::NotFound(format!("Room {room_id} not found")))?;

        Ok(row.into())
    }
}
