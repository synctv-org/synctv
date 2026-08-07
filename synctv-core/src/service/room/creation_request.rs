use sqlx::{Postgres, Transaction};

use crate::{
    models::{
        OpaquePasswordRecord, ReviewStatus, Room, RoomCategoryId, RoomId, RoomLabelId,
        RoomSettings, UserId,
    },
    service::room::RoomService,
    Error, Result,
};

pub(super) struct PendingRoomCreationRequest {
    pub(super) id: RoomId,
    pub(super) requested_by: UserId,
    pub(super) name: String,
    pub(super) description: String,
    pub(super) category_id: Option<RoomCategoryId>,
    pub(super) label_ids: Vec<RoomLabelId>,
    pub(super) settings: RoomSettings,
    pub(super) opaque_password_record: Option<OpaquePasswordRecord>,
}

struct PendingRoomCreationRequestRow {
    id: RoomId,
    requested_by: UserId,
    name: String,
    description: String,
    category_id: Option<RoomCategoryId>,
    settings_payload: Option<RoomSettings>,
    opaque_password_record: Option<Vec<u8>>,
    opaque_password_credential_identifier: Option<Vec<u8>>,
    opaque_password_ciphersuite: Option<String>,
    opaque_password_server_setup_version: Option<i32>,
}

impl PendingRoomCreationRequestRow {
    fn into_request(self) -> std::result::Result<PendingRoomCreationRequest, sqlx::Error> {
        let settings = self.settings_payload.unwrap_or_default();
        let opaque_password_record = match (
            self.opaque_password_record,
            self.opaque_password_credential_identifier,
            self.opaque_password_ciphersuite,
            self.opaque_password_server_setup_version,
        ) {
            (Some(record), Some(credential_identifier), Some(ciphersuite), Some(version)) => {
                Some(OpaquePasswordRecord {
                    record,
                    credential_identifier,
                    ciphersuite,
                    server_setup_version: version,
                })
            }
            (None, None, None, None) => None,
            _ => {
                return Err(sqlx::Error::Decode(
                    "Incomplete pending room creation OPAQUE password material".into(),
                ));
            }
        };

        Ok(PendingRoomCreationRequest {
            id: self.id,
            requested_by: self.requested_by,
            name: self.name,
            description: self.description,
            category_id: self.category_id,
            label_ids: Vec::new(),
            settings,
            opaque_password_record,
        })
    }
}

pub(super) struct RoomCreationRequestDraft<'a> {
    pub(super) requested_by: UserId,
    pub(super) name: &'a str,
    pub(super) description: &'a str,
    pub(super) category_id: Option<RoomCategoryId>,
    pub(super) label_ids: &'a [RoomLabelId],
    pub(super) settings: &'a RoomSettings,
    pub(super) password: Option<&'a str>,
}

impl RoomService {
    pub(super) async fn create_room_creation_request_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        draft: RoomCreationRequestDraft<'_>,
    ) -> Result<Room> {
        let RoomCreationRequestDraft {
            requested_by,
            name,
            description,
            category_id,
            label_ids,
            settings,
            password,
        } = draft;
        let request_id = sqlx::query_scalar!(
            r"
            INSERT INTO room_creation_requests (
                requested_by, name, description, category_id, settings_payload, status, requested_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, CURRENT_TIMESTAMP)
            RETURNING id
            ",
            requested_by.as_i64(),
            name,
            description,
            category_id.map(|id| id.as_i64()),
            settings as &RoomSettings,
            i16::from(ReviewStatus::Pending)
        )
        .fetch_one(&mut **tx)
        .await?;

        let mut room =
            Room::new_with_description(name.to_string(), description.to_string(), requested_by);
        room.id = RoomId::try_from(request_id).map_err(Error::Internal)?;
        if let Some(category_id) = category_id {
            room.category = self
                .taxonomy_repo
                .categories_by_ids(&[category_id])
                .await?
                .remove(&category_id);
        }
        room.labels = self
            .taxonomy_repo
            .labels_by_ids(label_ids)
            .await?
            .into_values()
            .collect();
        crate::repository::RoomTaxonomyRepository::assign_room_creation_request_labels(
            room.id, label_ids, &mut *tx,
        )
        .await?;
        if let Some(password) = password {
            let opaque_record = self.opaque_password_service.register_password(
                &super::password::room_opaque_credential_identifier(&room.id),
                password,
            )?;
            sqlx::query!(
                r"
                UPDATE room_creation_requests
                SET opaque_password_record = $2,
                    opaque_password_credential_identifier = $3,
                    opaque_password_ciphersuite = $4,
                    opaque_password_server_setup_version = $5
                WHERE id = $1
                ",
                room.id.as_i64(),
                opaque_record.record.as_slice(),
                opaque_record.credential_identifier.as_slice(),
                opaque_record.ciphersuite.as_str(),
                opaque_record.server_setup_version
            )
            .execute(&mut **tx)
            .await?;
        }
        Ok(room)
    }

    pub(super) async fn load_pending_room_creation_request_for_update(
        request_id: &RoomId,
        tx: &mut Transaction<'_, Postgres>,
    ) -> Result<Option<PendingRoomCreationRequest>> {
        let row = sqlx::query_as!(
            PendingRoomCreationRequestRow,
            r#"
            SELECT id AS "id: RoomId",
                   requested_by AS "requested_by: UserId",
                   name,
                   description,
                   category_id AS "category_id: RoomCategoryId",
                   settings_payload AS "settings_payload?: RoomSettings",
                   opaque_password_record,
                   opaque_password_credential_identifier,
                   opaque_password_ciphersuite,
                   opaque_password_server_setup_version
            FROM room_creation_requests
            WHERE id = $1 AND reviewed_at IS NULL AND status = $2
            FOR UPDATE
            "#,
            request_id.as_i64(),
            i16::from(ReviewStatus::Pending)
        )
        .fetch_optional(&mut **tx)
        .await?;

        let Some(row) = row else {
            return Ok(None);
        };
        let mut request = row.into_request().map_err(Error::Database)?;
        request.label_ids = sqlx::query_scalar!(
            r#"
            SELECT label_id AS "label_id: RoomLabelId"
            FROM room_creation_request_labels
            WHERE request_id = $1
            ORDER BY label_id ASC
            "#,
            request_id as &RoomId
        )
        .fetch_all(&mut **tx)
        .await?;
        Ok(Some(request))
    }
}
