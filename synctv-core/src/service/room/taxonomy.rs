use std::collections::HashSet;

use crate::{
    models::{
        Room, RoomCategory, RoomCategoryId, RoomId, RoomLabel, RoomLabelId, UpsertRoomCategory,
        UpsertRoomLabel, UserId,
    },
    service::room::RoomService,
    Error, Result,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RoomCategoryUpdate {
    Preserve,
    Set(Option<RoomCategoryId>),
}

fn normalize_taxonomy_key(key: &str) -> Result<String> {
    let normalized = key.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return Err(Error::InvalidInput("Taxonomy key is required".to_string()));
    }
    if normalized.chars().count() > 64 {
        return Err(Error::InvalidInput(
            "Taxonomy key too long (max 64 characters)".to_string(),
        ));
    }
    if !normalized
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_' || ch == '-')
    {
        return Err(Error::InvalidInput(
            "Taxonomy key may contain lowercase letters, digits, underscores, and hyphens"
                .to_string(),
        ));
    }
    let first = normalized
        .chars()
        .next()
        .expect("normalized taxonomy key is non-empty");
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return Err(Error::InvalidInput(
            "Taxonomy key must start with a lowercase letter or digit".to_string(),
        ));
    }
    Ok(normalized)
}

fn normalize_taxonomy_name(name: &str) -> Result<String> {
    let normalized = name.trim().to_string();
    if normalized.is_empty() {
        return Err(Error::InvalidInput("Taxonomy name is required".to_string()));
    }
    if normalized.chars().count() > 80 {
        return Err(Error::InvalidInput(
            "Taxonomy name too long (max 80 characters)".to_string(),
        ));
    }
    Ok(normalized)
}

fn normalize_taxonomy_description(description: &str) -> Result<String> {
    let normalized = description.trim().to_string();
    if normalized.chars().count() > 300 {
        return Err(Error::InvalidInput(
            "Taxonomy description too long (max 300 characters)".to_string(),
        ));
    }
    Ok(normalized)
}

fn normalize_label_color(color: &str) -> Result<String> {
    let normalized = color.trim().to_string();
    if normalized.is_empty() {
        return Ok(normalized);
    }
    let valid = normalized.len() == 7
        && normalized.starts_with('#')
        && normalized.chars().skip(1).all(|ch| ch.is_ascii_hexdigit());
    if valid {
        Ok(normalized)
    } else {
        Err(Error::InvalidInput(
            "Room label color must be empty or a #RRGGBB hex color".to_string(),
        ))
    }
}

fn dedupe_label_ids(label_ids: &[RoomLabelId]) -> Vec<RoomLabelId> {
    let mut seen = HashSet::with_capacity(label_ids.len());
    let mut deduped = Vec::with_capacity(label_ids.len());
    for label_id in label_ids {
        if seen.insert(*label_id) {
            deduped.push(*label_id);
        }
    }
    deduped
}

impl RoomService {
    pub async fn list_room_categories(&self, enabled_only: bool) -> Result<Vec<RoomCategory>> {
        self.taxonomy_repo.list_categories(enabled_only).await
    }

    pub async fn list_room_labels(
        &self,
        enabled_only: bool,
        category_id: Option<RoomCategoryId>,
    ) -> Result<Vec<RoomLabel>> {
        self.taxonomy_repo
            .list_labels(enabled_only, category_id)
            .await
    }

    pub async fn upsert_room_category(&self, input: UpsertRoomCategory) -> Result<RoomCategory> {
        let normalized = UpsertRoomCategory {
            key: normalize_taxonomy_key(&input.key)?,
            name: normalize_taxonomy_name(&input.name)?,
            description: normalize_taxonomy_description(&input.description)?,
            sort_order: input.sort_order,
            is_enabled: input.is_enabled,
        };
        self.taxonomy_repo.upsert_category(&normalized).await
    }

    pub async fn upsert_room_label(&self, input: UpsertRoomLabel) -> Result<RoomLabel> {
        if let Some(category_id) = input.category_id {
            let category = self
                .taxonomy_repo
                .get_category(category_id)
                .await?
                .ok_or_else(|| Error::InvalidInput("Room label category not found".to_string()))?;
            if !category.is_enabled && input.is_enabled {
                return Err(Error::InvalidInput(
                    "Enabled labels must belong to an enabled category".to_string(),
                ));
            }
        }

        let normalized = UpsertRoomLabel {
            key: normalize_taxonomy_key(&input.key)?,
            name: normalize_taxonomy_name(&input.name)?,
            description: normalize_taxonomy_description(&input.description)?,
            color: normalize_label_color(&input.color)?,
            category_id: input.category_id,
            sort_order: input.sort_order,
            is_enabled: input.is_enabled,
        };
        self.taxonomy_repo.upsert_label(&normalized).await
    }

    pub async fn delete_room_category(&self, id: RoomCategoryId) -> Result<bool> {
        self.taxonomy_repo.delete_category(id).await
    }

    pub async fn delete_room_label(&self, id: RoomLabelId) -> Result<bool> {
        self.taxonomy_repo.delete_label(id).await
    }

    pub async fn resolve_enabled_room_taxonomy(
        &self,
        category_id: Option<RoomCategoryId>,
        label_ids: &[RoomLabelId],
    ) -> Result<(Option<RoomCategoryId>, Vec<RoomLabelId>)> {
        if let Some(category_id) = category_id {
            let category = self
                .taxonomy_repo
                .get_category(category_id)
                .await?
                .ok_or_else(|| Error::InvalidInput("Room category not found".to_string()))?;
            if !category.is_enabled {
                return Err(Error::InvalidInput("Room category is disabled".to_string()));
            }
        }

        let label_ids = dedupe_label_ids(label_ids);
        let labels = self.taxonomy_repo.labels_by_ids(&label_ids).await?;
        if labels.len() != label_ids.len() {
            return Err(Error::InvalidInput("Room label not found".to_string()));
        }
        for label_id in &label_ids {
            let label = labels
                .get(label_id)
                .ok_or_else(|| Error::InvalidInput("Room label not found".to_string()))?;
            if !label.is_enabled {
                return Err(Error::InvalidInput("Room label is disabled".to_string()));
            }
            if let Some(label_category_id) = label.category_id {
                let Some(selected_category_id) = category_id else {
                    return Err(Error::InvalidInput(
                        "Room label requires a matching room category".to_string(),
                    ));
                };
                if label_category_id != selected_category_id {
                    return Err(Error::InvalidInput(
                        "Room label does not apply to the selected category".to_string(),
                    ));
                }
            }
        }
        Ok((category_id, label_ids))
    }

    pub(crate) async fn hydrate_room_taxonomy(&self, room: &mut Room) -> Result<()> {
        self.hydrate_rooms_taxonomy(std::slice::from_mut(room))
            .await
    }

    pub(crate) async fn hydrate_rooms_taxonomy(&self, rooms: &mut [Room]) -> Result<()> {
        if rooms.is_empty() {
            return Ok(());
        }

        let room_ids: Vec<RoomId> = rooms.iter().map(|room| room.id).collect();
        let labels = self.taxonomy_repo.labels_for_rooms(&room_ids).await?;

        for room in rooms {
            room.labels = labels.get(&room.id).cloned().unwrap_or_default();
        }
        Ok(())
    }

    pub async fn update_room_taxonomy(
        &self,
        room_id: RoomId,
        category_update: RoomCategoryUpdate,
        label_ids: &[RoomLabelId],
        assigned_by: Option<UserId>,
    ) -> Result<()> {
        let label_ids = dedupe_label_ids(label_ids);
        let mut tx = self.pool.begin().await?;

        let row = sqlx::query!(
            r#"
            SELECT category_id AS "category_id: RoomCategoryId"
            FROM rooms
            WHERE id = $1 AND deleted_at IS NULL
            FOR UPDATE
            "#,
            room_id as RoomId,
        )
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            return Err(Error::NotFound(format!("Room {room_id} not found")));
        };

        // Resolve the target category and validate everything inside the tx so
        // that (a) no second connection is needed and (b) Preserve never
        // re-validates whether the existing category is still enabled.
        let category_id = match category_update {
            RoomCategoryUpdate::Preserve => {
                // Keep the room's current category.  Do NOT check is_enabled —
                // the category was valid when the room was created; an admin
                // disabling it later must not break label-only updates.
                validate_labels_for_category_in_tx(row.category_id, &label_ids, &mut tx).await?;
                row.category_id
            }
            RoomCategoryUpdate::Set(new_id) => {
                // Validate the incoming category inside the tx before writing.
                if let Some(id) = new_id {
                    let cat =
                        crate::repository::RoomTaxonomyRepository::get_category_with_executor(
                            id, &mut tx,
                        )
                        .await?
                        .ok_or_else(|| {
                            Error::InvalidInput("Room category not found".to_string())
                        })?;
                    if !cat.is_enabled {
                        return Err(Error::InvalidInput("Room category is disabled".to_string()));
                    }
                }
                validate_labels_for_category_in_tx(new_id, &label_ids, &mut tx).await?;
                new_id
            }
        };

        let updated = sqlx::query!(
            r#"
            UPDATE rooms
            SET category_id = $2,
                updated_at = CURRENT_TIMESTAMP,
                version = version + 1
            WHERE id = $1
            "#,
            room_id as RoomId,
            category_id.map(|id| id.as_i64()),
        )
        .execute(&mut *tx)
        .await?;
        debug_assert_eq!(updated.rows_affected(), 1);

        crate::repository::RoomTaxonomyRepository::assign_room_labels(
            room_id,
            &label_ids,
            assigned_by,
            &mut tx,
        )
        .await?;
        tx.commit().await?;
        self.notify_room_invalidation(&room_id).await;
        Ok(())
    }
}

/// Validate that all `label_ids` exist, are enabled, and are consistent with
/// `category_id` — entirely within the supplied transaction connection, so
/// no second pool connection is needed while a `FOR UPDATE` lock is held.
async fn validate_labels_for_category_in_tx(
    category_id: Option<RoomCategoryId>,
    label_ids: &[RoomLabelId],
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<()> {
    if label_ids.is_empty() {
        return Ok(());
    }
    let labels =
        crate::repository::RoomTaxonomyRepository::labels_by_ids_with_executor(label_ids, tx)
            .await?;
    if labels.len() != label_ids.len() {
        return Err(Error::InvalidInput("Room label not found".to_string()));
    }
    for label_id in label_ids {
        let label = labels
            .get(label_id)
            .ok_or_else(|| Error::InvalidInput("Room label not found".to_string()))?;
        if !label.is_enabled {
            return Err(Error::InvalidInput("Room label is disabled".to_string()));
        }
        if let Some(label_category_id) = label.category_id {
            let Some(selected) = category_id else {
                return Err(Error::InvalidInput(
                    "Room label requires a matching room category".to_string(),
                ));
            };
            if label_category_id != selected {
                return Err(Error::InvalidInput(
                    "Room label does not apply to the selected category".to_string(),
                ));
            }
        }
    }
    Ok(())
}
