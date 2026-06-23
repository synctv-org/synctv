use std::collections::HashMap;

use sqlx::{PgConnection, PgPool, Postgres, QueryBuilder};

use crate::{
    models::{
        RoomCategory, RoomCategoryId, RoomId, RoomLabel, RoomLabelId, UpsertRoomCategory,
        UpsertRoomLabel, UserId,
    },
    Result,
};

#[derive(Debug)]
pub(super) struct OptionalRoomCategoryRowParts {
    pub id: Option<RoomCategoryId>,
    pub key: Option<String>,
    pub name: Option<String>,
    pub description: Option<String>,
    pub sort_order: Option<i32>,
    pub is_enabled: Option<bool>,
    pub created_at: Option<chrono::DateTime<chrono::Utc>>,
    pub updated_at: Option<chrono::DateTime<chrono::Utc>>,
}

pub(super) fn optional_room_category_from_parts(
    parts: OptionalRoomCategoryRowParts,
) -> Option<RoomCategory> {
    match (
        parts.id,
        parts.key,
        parts.name,
        parts.description,
        parts.sort_order,
        parts.is_enabled,
        parts.created_at,
        parts.updated_at,
    ) {
        (
            Some(id),
            Some(key),
            Some(name),
            Some(description),
            Some(sort_order),
            Some(is_enabled),
            Some(created_at),
            Some(updated_at),
        ) => Some(RoomCategory {
            id,
            key,
            name,
            description,
            sort_order,
            is_enabled,
            created_at,
            updated_at,
        }),
        _ => None,
    }
}

#[derive(Debug, sqlx::FromRow)]
struct RoomCategoryRow {
    id: RoomCategoryId,
    key: String,
    name: String,
    description: String,
    sort_order: i32,
    is_enabled: bool,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
}

impl From<RoomCategoryRow> for RoomCategory {
    fn from(row: RoomCategoryRow) -> Self {
        Self {
            id: row.id,
            key: row.key,
            name: row.name,
            description: row.description,
            sort_order: row.sort_order,
            is_enabled: row.is_enabled,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

#[derive(Debug, sqlx::FromRow)]
struct RoomLabelRow {
    id: RoomLabelId,
    key: String,
    name: String,
    description: String,
    color: String,
    category_id: Option<RoomCategoryId>,
    sort_order: i32,
    is_enabled: bool,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, sqlx::FromRow)]
struct RoomLabelAssignmentRow {
    room_id: RoomId,
    id: RoomLabelId,
    key: String,
    name: String,
    description: String,
    color: String,
    category_id: Option<RoomCategoryId>,
    sort_order: i32,
    is_enabled: bool,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
}

impl From<RoomLabelRow> for RoomLabel {
    fn from(row: RoomLabelRow) -> Self {
        Self {
            id: row.id,
            key: row.key,
            name: row.name,
            description: row.description,
            color: row.color,
            category_id: row.category_id,
            sort_order: row.sort_order,
            is_enabled: row.is_enabled,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RoomTaxonomyAssignment {
    pub category_id: Option<RoomCategoryId>,
    pub label_ids: Vec<RoomLabelId>,
}

#[derive(Clone)]
pub struct RoomTaxonomyRepository {
    pool: PgPool,
}

impl RoomTaxonomyRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn category_id_by_key(&self, key: &str) -> Result<Option<RoomCategoryId>> {
        let id = sqlx::query_scalar!(
            r#"
            SELECT id AS "id: RoomCategoryId"
            FROM room_categories
            WHERE key = $1
            "#,
            key
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(id)
    }

    pub async fn list_categories(&self, enabled_only: bool) -> Result<Vec<RoomCategory>> {
        let rows = sqlx::query_as!(
            RoomCategoryRow,
            r#"
            SELECT id AS "id: RoomCategoryId",
                   key,
                   name,
                   description,
                   sort_order,
                   is_enabled,
                   created_at,
                   updated_at
            FROM room_categories
            WHERE (NOT $1 OR is_enabled)
            ORDER BY sort_order ASC, id ASC
            "#,
            enabled_only
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub async fn list_labels(
        &self,
        enabled_only: bool,
        category_id: Option<RoomCategoryId>,
    ) -> Result<Vec<RoomLabel>> {
        let rows = sqlx::query_as!(
            RoomLabelRow,
            r#"
            SELECT id AS "id: RoomLabelId",
                   key,
                   name,
                   description,
                   color,
                   category_id AS "category_id: RoomCategoryId",
                   sort_order,
                   is_enabled,
                   created_at,
                   updated_at
            FROM room_labels
            WHERE (NOT $1 OR is_enabled)
              AND ($2::BIGINT IS NULL OR category_id IS NULL OR category_id = $2)
            ORDER BY sort_order ASC, id ASC
            "#,
            enabled_only,
            category_id.map(|id| id.as_i64())
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub async fn get_category(&self, id: RoomCategoryId) -> Result<Option<RoomCategory>> {
        let row = sqlx::query_as!(
            RoomCategoryRow,
            r#"
            SELECT id AS "id: RoomCategoryId",
                   key,
                   name,
                   description,
                   sort_order,
                   is_enabled,
                   created_at,
                   updated_at
            FROM room_categories
            WHERE id = $1
            "#,
            id.as_i64()
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(Into::into))
    }

    pub async fn get_label(&self, id: RoomLabelId) -> Result<Option<RoomLabel>> {
        let row = sqlx::query_as!(
            RoomLabelRow,
            r#"
            SELECT id AS "id: RoomLabelId",
                   key,
                   name,
                   description,
                   color,
                   category_id AS "category_id: RoomCategoryId",
                   sort_order,
                   is_enabled,
                   created_at,
                   updated_at
            FROM room_labels
            WHERE id = $1
            "#,
            id.as_i64()
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(Into::into))
    }

    pub async fn categories_by_ids(
        &self,
        category_ids: &[RoomCategoryId],
    ) -> Result<HashMap<RoomCategoryId, RoomCategory>> {
        if category_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let ids: Vec<i64> = category_ids.iter().map(RoomCategoryId::as_i64).collect();
        let rows = sqlx::query_as!(
            RoomCategoryRow,
            r#"
            SELECT id AS "id: RoomCategoryId",
                   key,
                   name,
                   description,
                   sort_order,
                   is_enabled,
                   created_at,
                   updated_at
            FROM room_categories
            WHERE id = ANY($1)
            "#,
            &ids
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| {
                let category: RoomCategory = row.into();
                (category.id, category)
            })
            .collect())
    }

    pub async fn labels_by_ids(
        &self,
        label_ids: &[RoomLabelId],
    ) -> Result<HashMap<RoomLabelId, RoomLabel>> {
        if label_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let ids: Vec<i64> = label_ids.iter().map(RoomLabelId::as_i64).collect();
        let rows = sqlx::query_as!(
            RoomLabelRow,
            r#"
            SELECT id AS "id: RoomLabelId",
                   key,
                   name,
                   description,
                   color,
                   category_id AS "category_id: RoomCategoryId",
                   sort_order,
                   is_enabled,
                   created_at,
                   updated_at
            FROM room_labels
            WHERE id = ANY($1)
            "#,
            &ids
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| {
                let label: RoomLabel = row.into();
                (label.id, label)
            })
            .collect())
    }

    pub async fn labels_for_rooms(
        &self,
        room_ids: &[RoomId],
    ) -> Result<HashMap<RoomId, Vec<RoomLabel>>> {
        if room_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let ids: Vec<i64> = room_ids.iter().map(RoomId::as_i64).collect();

        let rows = sqlx::query_as!(
            RoomLabelAssignmentRow,
            r#"
            SELECT rla.room_id AS "room_id: RoomId",
                   rl.id AS "id: RoomLabelId",
                   rl.key,
                   rl.name,
                   rl.description,
                   rl.color,
                   rl.category_id AS "category_id: RoomCategoryId",
                   rl.sort_order,
                   rl.is_enabled,
                   rl.created_at,
                   rl.updated_at
            FROM room_label_assignments rla
            JOIN room_labels rl ON rl.id = rla.label_id
            WHERE rla.room_id = ANY($1)
            ORDER BY rl.sort_order ASC, rl.id ASC
            "#,
            &ids
        )
        .fetch_all(&self.pool)
        .await?;

        let mut labels_by_room = HashMap::new();
        for row in rows {
            labels_by_room
                .entry(row.room_id)
                .or_insert_with(Vec::new)
                .push(RoomLabel {
                    id: row.id,
                    key: row.key,
                    name: row.name,
                    description: row.description,
                    color: row.color,
                    category_id: row.category_id,
                    sort_order: row.sort_order,
                    is_enabled: row.is_enabled,
                    created_at: row.created_at,
                    updated_at: row.updated_at,
                });
        }
        Ok(labels_by_room)
    }

    pub async fn upsert_category(&self, input: &UpsertRoomCategory) -> Result<RoomCategory> {
        let row = sqlx::query_as!(
            RoomCategoryRow,
            r#"
            INSERT INTO room_categories (key, name, description, sort_order, is_enabled)
            VALUES ($1, $2, $3, $4, $5)
            ON CONFLICT (key) DO UPDATE SET
                name = EXCLUDED.name,
                description = EXCLUDED.description,
                sort_order = EXCLUDED.sort_order,
                is_enabled = EXCLUDED.is_enabled,
                updated_at = CURRENT_TIMESTAMP
            RETURNING id AS "id: RoomCategoryId",
                      key,
                      name,
                      description,
                      sort_order,
                      is_enabled,
                      created_at,
                      updated_at
            "#,
            input.key.trim(),
            input.name.trim(),
            input.description.trim(),
            input.sort_order,
            input.is_enabled
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(row.into())
    }

    pub async fn upsert_label(&self, input: &UpsertRoomLabel) -> Result<RoomLabel> {
        let row = sqlx::query_as!(
            RoomLabelRow,
            r#"
            INSERT INTO room_labels (
                key, name, description, color, category_id, sort_order, is_enabled
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            ON CONFLICT (key) DO UPDATE SET
                name = EXCLUDED.name,
                description = EXCLUDED.description,
                color = EXCLUDED.color,
                category_id = EXCLUDED.category_id,
                sort_order = EXCLUDED.sort_order,
                is_enabled = EXCLUDED.is_enabled,
                updated_at = CURRENT_TIMESTAMP
            RETURNING id AS "id: RoomLabelId",
                      key,
                      name,
                      description,
                      color,
                      category_id AS "category_id: RoomCategoryId",
                      sort_order,
                      is_enabled,
                      created_at,
                      updated_at
            "#,
            input.key.trim(),
            input.name.trim(),
            input.description.trim(),
            input.color.trim(),
            input.category_id.map(|id| id.as_i64()),
            input.sort_order,
            input.is_enabled
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(row.into())
    }

    pub async fn delete_category(&self, id: RoomCategoryId) -> Result<bool> {
        let deleted = sqlx::query!(
            r#"
            DELETE FROM room_categories
            WHERE id = $1
              AND NOT EXISTS (SELECT 1 FROM rooms WHERE category_id = $1)
              AND NOT EXISTS (SELECT 1 FROM room_creation_requests WHERE category_id = $1)
              AND NOT EXISTS (SELECT 1 FROM room_labels WHERE category_id = $1)
            "#,
            id.as_i64()
        )
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(deleted > 0)
    }

    pub async fn delete_label(&self, id: RoomLabelId) -> Result<bool> {
        let deleted = sqlx::query!(
            r#"
            DELETE FROM room_labels
            WHERE id = $1
              AND NOT EXISTS (
                  SELECT 1 FROM room_label_assignments WHERE label_id = $1
              )
              AND NOT EXISTS (
                  SELECT 1 FROM room_creation_request_labels WHERE label_id = $1
              )
            "#,
            id.as_i64()
        )
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(deleted > 0)
    }

    pub async fn get_category_with_executor(
        id: RoomCategoryId,
        executor: &mut PgConnection,
    ) -> Result<Option<RoomCategory>> {
        let row = sqlx::query_as!(
            RoomCategoryRow,
            r#"
            SELECT id AS "id: RoomCategoryId",
                   key,
                   name,
                   description,
                   sort_order,
                   is_enabled,
                   created_at,
                   updated_at
            FROM room_categories
            WHERE id = $1
            "#,
            id.as_i64()
        )
        .fetch_optional(&mut *executor)
        .await?;
        Ok(row.map(Into::into))
    }

    pub async fn labels_by_ids_with_executor(
        label_ids: &[RoomLabelId],
        executor: &mut PgConnection,
    ) -> Result<HashMap<RoomLabelId, RoomLabel>> {
        if label_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let ids: Vec<i64> = label_ids.iter().map(RoomLabelId::as_i64).collect();
        let rows = sqlx::query_as!(
            RoomLabelRow,
            r#"
            SELECT id AS "id: RoomLabelId",
                   key,
                   name,
                   description,
                   color,
                   category_id AS "category_id: RoomCategoryId",
                   sort_order,
                   is_enabled,
                   created_at,
                   updated_at
            FROM room_labels
            WHERE id = ANY($1)
            "#,
            &ids
        )
        .fetch_all(&mut *executor)
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| {
                let label: RoomLabel = row.into();
                (label.id, label)
            })
            .collect())
    }

    pub async fn assign_room_labels(
        room_id: RoomId,
        label_ids: &[RoomLabelId],
        assigned_by: Option<UserId>,
        executor: &mut PgConnection,
    ) -> Result<()> {
        sqlx::query!(
            "DELETE FROM room_label_assignments WHERE room_id = $1",
            room_id as RoomId
        )
        .execute(&mut *executor)
        .await?;

        if !label_ids.is_empty() {
            let mut builder = QueryBuilder::<Postgres>::new(
                "INSERT INTO room_label_assignments (room_id, label_id, assigned_by) ",
            );
            builder.push_values(label_ids, |mut values, label_id| {
                values
                    .push_bind(room_id)
                    .push_bind(*label_id)
                    .push_bind(assigned_by);
            });
            builder.build().execute(&mut *executor).await?;
        }
        Ok(())
    }

    pub async fn assign_room_creation_request_labels(
        request_id: RoomId,
        label_ids: &[RoomLabelId],
        executor: &mut PgConnection,
    ) -> Result<()> {
        sqlx::query!(
            "DELETE FROM room_creation_request_labels WHERE request_id = $1",
            request_id as RoomId
        )
        .execute(&mut *executor)
        .await?;

        if !label_ids.is_empty() {
            let mut builder = QueryBuilder::<Postgres>::new(
                "INSERT INTO room_creation_request_labels (request_id, label_id) ",
            );
            builder.push_values(label_ids, |mut values, label_id| {
                values.push_bind(request_id).push_bind(*label_id);
            });
            builder.build().execute(&mut *executor).await?;
        }
        Ok(())
    }

    pub async fn labels_for_room_creation_request(
        &self,
        request_id: RoomId,
    ) -> Result<Vec<RoomLabelId>> {
        let labels = sqlx::query_scalar!(
            r#"
            SELECT label_id AS "label_id: RoomLabelId"
            FROM room_creation_request_labels
            WHERE request_id = $1
            ORDER BY label_id ASC
            "#,
            request_id as RoomId
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(labels)
    }
}
