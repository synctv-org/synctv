// Provider Instance Repository
// Database access layer for provider instance configuration management.

use crate::credential_encryption::CredentialEncryption;
use crate::models::{
    normalize_provider_instance_name, provider_type_code_from_name, provider_type_codes_from_names,
    provider_type_name_from_code, ProviderInstance, ProviderInstanceListQuery,
    ProviderInstanceListSortBy, UserId, UserProviderCredential,
};
use crate::Result;
use sqlx::PgPool;

#[derive(Debug, sqlx::FromRow)]
struct ProviderInstanceRow {
    name: String,
    endpoint: String,
    comment: Option<String>,
    jwt_secret: Option<String>,
    custom_ca: Option<String>,
    timeout: String,
    tls: bool,
    insecure_tls: bool,
    providers: Vec<i16>,
    enabled: bool,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
}

impl TryFrom<ProviderInstanceRow> for ProviderInstance {
    type Error = crate::Error;

    fn try_from(row: ProviderInstanceRow) -> Result<Self> {
        let providers = row
            .providers
            .into_iter()
            .map(provider_type_name_from_code)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(crate::Error::InvalidInput)?;

        Ok(Self {
            name: row.name,
            endpoint: row.endpoint,
            comment: row.comment,
            jwt_secret: row.jwt_secret,
            custom_ca: row.custom_ca,
            timeout: row.timeout,
            tls: row.tls,
            insecure_tls: row.insecure_tls,
            providers,
            enabled: row.enabled,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

#[derive(Debug, sqlx::FromRow)]
struct UserProviderCredentialRow {
    id: i64,
    user_id: UserId,
    provider: i16,
    server_id: String,
    provider_instance_name: Option<String>,
    credential_data: serde_json::Value,
    expires_at: Option<chrono::DateTime<chrono::Utc>>,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
}

impl TryFrom<UserProviderCredentialRow> for UserProviderCredential {
    type Error = crate::Error;

    fn try_from(row: UserProviderCredentialRow) -> Result<Self> {
        Ok(Self {
            id: row.id,
            user_id: row.user_id,
            provider: provider_type_name_from_code(row.provider)
                .map_err(crate::Error::InvalidInput)?,
            server_id: row.server_id,
            provider_instance_name: row.provider_instance_name,
            credential_data: row.credential_data,
            expires_at: row.expires_at,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

fn provider_type_code(provider: &str) -> Result<i16> {
    provider_type_code_from_name(provider).map_err(crate::Error::InvalidInput)
}

fn provider_type_codes(providers: &[String]) -> Result<Vec<i16>> {
    provider_type_codes_from_names(providers).map_err(crate::Error::InvalidInput)
}

/// Provider Instance Repository
///
/// Encrypts sensitive fields (`jwt_secret`, `custom_ca`) using `CredentialEncryption`
/// before storage and decrypts after read. Encryption is mandatory.
#[derive(Clone)]
pub struct ProviderInstanceRepository {
    pool: PgPool,
    encryption: Option<CredentialEncryption>,
}

impl std::fmt::Debug for ProviderInstanceRepository {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderInstanceRepository")
            .field("pool", &"PgPool")
            .field("encryption", &self.encryption.is_some())
            .finish()
    }
}

impl ProviderInstanceRepository {
    const INSTANCE_SELECT_COLUMNS: &'static str = "name, endpoint, comment, jwt_secret, custom_ca, timeout, tls, insecure_tls, providers, enabled, created_at, updated_at";

    fn is_provider_instance_reference_violation(db_err: &dyn sqlx::error::DatabaseError) -> bool {
        db_err.code().as_deref() == Some("23503")
            || db_err
                .constraint()
                .is_some_and(|constraint| constraint.contains("provider_instance"))
            || db_err.message().contains("foreign key constraint")
                && db_err.message().contains("media_provider_instances")
    }

    fn push_list_order_by(
        builder: &mut sqlx::QueryBuilder<'_, sqlx::Postgres>,
        query: &ProviderInstanceListQuery,
    ) {
        use crate::models::SortDirection;

        let order_by = match (query.sort_by, query.sort_direction) {
            (ProviderInstanceListSortBy::Name, SortDirection::Asc) => {
                " ORDER BY name ASC, created_at ASC"
            }
            (ProviderInstanceListSortBy::Name, SortDirection::Desc) => {
                " ORDER BY name DESC, created_at DESC"
            }
            (ProviderInstanceListSortBy::Endpoint, SortDirection::Asc) => {
                " ORDER BY endpoint ASC, created_at ASC"
            }
            (ProviderInstanceListSortBy::Endpoint, SortDirection::Desc) => {
                " ORDER BY endpoint DESC, created_at DESC"
            }
            (ProviderInstanceListSortBy::UpdatedAt, SortDirection::Asc) => {
                " ORDER BY updated_at ASC, name ASC"
            }
            (ProviderInstanceListSortBy::UpdatedAt, SortDirection::Desc) => {
                " ORDER BY updated_at DESC, name DESC"
            }
            (ProviderInstanceListSortBy::CreatedAt, SortDirection::Asc) => {
                " ORDER BY created_at ASC, name ASC"
            }
            (ProviderInstanceListSortBy::CreatedAt, SortDirection::Desc) => {
                " ORDER BY created_at DESC, name DESC"
            }
        };
        builder.push(order_by);
    }

    fn push_list_filters(
        builder: &mut sqlx::QueryBuilder<'_, sqlx::Postgres>,
        query: &ProviderInstanceListQuery,
    ) -> Result<()> {
        builder.push(" WHERE TRUE");

        if let Some(provider_type) = &query.provider_type {
            builder.push(" AND ");
            builder.push_bind(provider_type_code(provider_type)?);
            builder.push(" = ANY(providers)");
        }
        if let Some(enabled) = query.enabled {
            builder.push(" AND enabled = ");
            builder.push_bind(enabled);
        }
        if let Some(tls) = query.tls {
            builder.push(" AND tls = ");
            builder.push_bind(tls);
        }
        if let Some(search) = &query.search {
            let pattern = super::query_builder::escape_ilike(search);
            builder.push(" AND (name ILIKE ");
            builder.push_bind(pattern.clone());
            builder.push(" ESCAPE '\\' OR endpoint ILIKE ");
            builder.push_bind(pattern.clone());
            builder.push(" ESCAPE '\\' OR COALESCE(comment, '') ILIKE ");
            builder.push_bind(pattern);
            builder.push(" ESCAPE '\\'");
            if let Ok(provider_code) = provider_type_code(search) {
                builder.push(" OR ");
                builder.push_bind(provider_code);
                builder.push(" = ANY(providers)");
            }
            builder.push(")");
        }
        Ok(())
    }

    fn sensitive_fields_present(instance: &ProviderInstance) -> bool {
        instance
            .jwt_secret
            .as_ref()
            .is_some_and(|value| !value.trim().is_empty())
            || instance
                .custom_ca
                .as_ref()
                .is_some_and(|value| !value.trim().is_empty())
    }

    /// Create a new repository without encryption
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self {
            pool,
            encryption: None,
        }
    }

    /// Create a new repository with credential encryption enabled
    #[must_use]
    pub const fn new_with_encryption(pool: PgPool, encryption: CredentialEncryption) -> Self {
        Self {
            pool,
            encryption: Some(encryption),
        }
    }

    fn encrypt_field(&self, plaintext: Option<&str>) -> Result<Option<String>> {
        match (&self.encryption, plaintext) {
            (Some(enc), Some(value)) if !value.is_empty() => {
                let json_value = serde_json::Value::String(value.to_owned());
                let encrypted = enc.encrypt(&json_value)?;
                Ok(Some(encrypted))
            }
            (None, Some(value)) if !value.trim().is_empty() => Err(crate::Error::Internal(
                "Credential encryption must be configured before storing provider instance secrets"
                    .to_string(),
            )),
            _ => Ok(None),
        }
    }

    fn ensure_encryption_for_sensitive_fields(&self, instance: &ProviderInstance) -> Result<()> {
        if self.encryption.is_none() && Self::sensitive_fields_present(instance) {
            return Err(crate::Error::Internal(
                "Credential encryption must be configured before storing provider instance secrets"
                    .to_string(),
            ));
        }
        Ok(())
    }

    fn decrypt_field(&self, stored: Option<&str>) -> Result<Option<String>> {
        match (&self.encryption, stored) {
            (Some(enc), Some(value)) if value.starts_with("enc:") => {
                let decrypted = enc.decrypt(value)?;
                match decrypted {
                    serde_json::Value::String(s) => Ok(Some(s)),
                    other => Ok(Some(other.to_string())),
                }
            }
            (Some(_), Some(value)) if !value.is_empty() => Err(crate::Error::Internal(
                "Provider instance contains plaintext sensitive data while credential encryption is enabled"
                    .to_string(),
            )),
            (None, Some(value)) if !value.trim().is_empty() => {
                Err(crate::Error::Internal(
                    "Credential encryption must be configured before reading provider instance secrets"
                        .to_string(),
                ))
            }
            _ => Ok(None),
        }
    }

    /// Decrypt sensitive fields on a `ProviderInstance` after reading from DB.
    fn decrypt_instance(&self, mut instance: ProviderInstance) -> Result<ProviderInstance> {
        instance.jwt_secret = self.decrypt_field(instance.jwt_secret.as_deref())?;
        instance.custom_ca = self.decrypt_field(instance.custom_ca.as_deref())?;
        Ok(instance)
    }

    fn decrypt_instance_row(&self, row: ProviderInstanceRow) -> Result<ProviderInstance> {
        self.decrypt_instance(row.try_into()?)
    }

    /// Decrypt sensitive fields on a list of `ProviderInstance`.
    fn decrypt_instance_rows(
        &self,
        rows: Vec<ProviderInstanceRow>,
    ) -> Result<Vec<ProviderInstance>> {
        rows.into_iter()
            .map(|row| self.decrypt_instance_row(row))
            .collect()
    }

    /// Get all provider instances (sensitive fields decrypted)
    pub async fn get_all(&self) -> Result<Vec<ProviderInstance>> {
        let rows = sqlx::query_as!(
            ProviderInstanceRow,
            r"
            SELECT name, endpoint, comment, jwt_secret, custom_ca, timeout, tls,
                   insecure_tls, providers, enabled, created_at, updated_at
            FROM media_provider_instances
            ORDER BY created_at DESC
            ",
        )
        .fetch_all(&self.pool)
        .await?;
        self.decrypt_instance_rows(rows)
    }

    /// Get all enabled provider instances (sensitive fields decrypted)
    pub async fn get_all_enabled(&self) -> Result<Vec<ProviderInstance>> {
        let rows = sqlx::query_as!(
            ProviderInstanceRow,
            r"
            SELECT name, endpoint, comment, jwt_secret, custom_ca, timeout, tls,
                   insecure_tls, providers, enabled, created_at, updated_at
            FROM media_provider_instances
            WHERE enabled = true
            ORDER BY created_at DESC
            ",
        )
        .fetch_all(&self.pool)
        .await?;
        self.decrypt_instance_rows(rows)
    }

    /// Get provider instance by name (sensitive fields decrypted)
    pub async fn get_by_name(&self, name: &str) -> Result<Option<ProviderInstance>> {
        let row = sqlx::query_as!(
            ProviderInstanceRow,
            r"
            SELECT name, endpoint, comment, jwt_secret, custom_ca, timeout, tls,
                   insecure_tls, providers, enabled, created_at, updated_at
            FROM media_provider_instances
            WHERE name = $1
            ",
            name,
        )
        .fetch_optional(&self.pool)
        .await?;
        match row {
            Some(row) => Ok(Some(self.decrypt_instance_row(row)?)),
            None => Ok(None),
        }
    }

    pub async fn list(&self, query: &ProviderInstanceListQuery) -> Result<Vec<ProviderInstance>> {
        self.list_with_total(query)
            .await
            .map(|(instances, _)| instances)
    }

    pub async fn list_with_total(
        &self,
        query: &ProviderInstanceListQuery,
    ) -> Result<(Vec<ProviderInstance>, i64)> {
        let limit = query.pagination.limit_i64()?;
        let offset = query.pagination.offset_i64()?;

        let mut count_builder = sqlx::QueryBuilder::<sqlx::Postgres>::new(
            "SELECT COUNT(*) FROM media_provider_instances",
        );
        Self::push_list_filters(&mut count_builder, query)?;
        let total = count_builder
            .build_query_scalar::<i64>()
            .fetch_one(&self.pool)
            .await?;

        let mut builder = sqlx::QueryBuilder::<sqlx::Postgres>::new("SELECT ");
        builder.push(Self::INSTANCE_SELECT_COLUMNS);
        builder.push(" FROM media_provider_instances");
        Self::push_list_filters(&mut builder, query)?;
        Self::push_list_order_by(&mut builder, query);
        builder.push(" LIMIT ");
        builder.push_bind(limit);
        builder.push(" OFFSET ");
        builder.push_bind(offset);

        let rows = builder
            .build_query_as::<ProviderInstanceRow>()
            .fetch_all(&self.pool)
            .await?;
        Ok((self.decrypt_instance_rows(rows)?, total))
    }

    /// Get instances that support a specific provider type (sensitive fields decrypted)
    pub async fn find_by_provider(&self, provider: &str) -> Result<Vec<ProviderInstance>> {
        let rows = sqlx::query_as!(
            ProviderInstanceRow,
            r"
            SELECT name, endpoint, comment, jwt_secret, custom_ca, timeout, tls,
                   insecure_tls, providers, enabled, created_at, updated_at
            FROM media_provider_instances
            WHERE $1 = ANY(providers) AND enabled = true
            ",
            provider_type_code(provider)?,
        )
        .fetch_all(&self.pool)
        .await?;
        self.decrypt_instance_rows(rows)
    }

    /// Create a new provider instance (encrypts sensitive fields before storage)
    pub async fn create(&self, instance: &ProviderInstance) -> Result<()> {
        self.ensure_encryption_for_sensitive_fields(instance)?;
        let encrypted_jwt_secret = self.encrypt_field(instance.jwt_secret.as_deref())?;
        let encrypted_custom_ca = self.encrypt_field(instance.custom_ca.as_deref())?;
        let provider_codes = provider_type_codes(&instance.providers)?;
        let result = sqlx::query!(
            r"
            INSERT INTO media_provider_instances
            (name, endpoint, comment, jwt_secret, custom_ca, timeout, tls, insecure_tls, providers, enabled)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
            ",
            instance.name.as_str(),
            instance.endpoint.as_str(),
            instance.comment.as_deref(),
            encrypted_jwt_secret.as_deref(),
            encrypted_custom_ca.as_deref(),
            instance.timeout.as_str(),
            instance.tls,
            instance.insecure_tls,
            &provider_codes,
            instance.enabled,
        )
        .execute(&self.pool)
        .await;

        match result {
            Ok(_) => Ok(()),
            Err(sqlx::Error::Database(db_err)) if db_err.code().as_deref() == Some("23505") => {
                Err(crate::Error::AlreadyExists(format!(
                    "Provider instance '{}' already exists",
                    instance.name
                )))
            }
            Err(err) => Err(err.into()),
        }
    }

    /// Update an existing provider instance (encrypts sensitive fields before storage)
    pub async fn update(&self, instance: &ProviderInstance) -> Result<()> {
        self.ensure_encryption_for_sensitive_fields(instance)?;
        let encrypted_jwt_secret = self.encrypt_field(instance.jwt_secret.as_deref())?;
        let encrypted_custom_ca = self.encrypt_field(instance.custom_ca.as_deref())?;
        let provider_codes = provider_type_codes(&instance.providers)?;

        let result = sqlx::query!(
            r"
            UPDATE media_provider_instances
            SET endpoint = $2, comment = $3, jwt_secret = $4, custom_ca = $5,
                timeout = $6, tls = $7, insecure_tls = $8, providers = $9, enabled = $10,
                updated_at = NOW()
            WHERE name = $1
            ",
            instance.name.as_str(),
            instance.endpoint.as_str(),
            instance.comment.as_deref(),
            encrypted_jwt_secret.as_deref(),
            encrypted_custom_ca.as_deref(),
            instance.timeout.as_str(),
            instance.tls,
            instance.insecure_tls,
            &provider_codes,
            instance.enabled,
        )
        .execute(&self.pool)
        .await?;

        if result.rows_affected() == 0 {
            return Err(crate::Error::NotFound(format!(
                "Provider instance '{}' not found",
                instance.name
            )));
        }

        Ok(())
    }

    /// Delete a provider instance
    pub async fn delete(&self, name: &str) -> Result<()> {
        let result = sqlx::query!("DELETE FROM media_provider_instances WHERE name = $1", name,)
            .execute(&self.pool)
            .await;

        let result = match result {
            Ok(result) => result,
            Err(sqlx::Error::Database(db_err))
                if Self::is_provider_instance_reference_violation(db_err.as_ref()) =>
            {
                return Err(crate::Error::InvalidInput(format!(
                    "Provider instance '{name}' is still referenced by media or playlists"
                )));
            }
            Err(err) => return Err(err.into()),
        };

        if result.rows_affected() == 0 {
            return Err(crate::Error::NotFound(format!(
                "Provider instance '{name}' not found"
            )));
        }

        Ok(())
    }

    /// Enable a provider instance
    pub async fn enable(&self, name: &str) -> Result<()> {
        let result = sqlx::query!(
            "UPDATE media_provider_instances SET enabled = true, updated_at = NOW() WHERE name = $1",
            name,
        )
            .execute(&self.pool)
            .await?;

        if result.rows_affected() == 0 {
            return Err(crate::Error::NotFound(format!(
                "Provider instance '{name}' not found"
            )));
        }

        Ok(())
    }

    /// Disable a provider instance
    pub async fn disable(&self, name: &str) -> Result<()> {
        let result = sqlx::query!(
            "UPDATE media_provider_instances SET enabled = false, updated_at = NOW() WHERE name = $1",
            name,
        )
            .execute(&self.pool)
            .await?;

        if result.rows_affected() == 0 {
            return Err(crate::Error::NotFound(format!(
                "Provider instance '{name}' not found"
            )));
        }

        Ok(())
    }
}

/// User Provider Credential Repository
///
/// Credentials are encrypted at rest using AES-256-GCM. Encryption is mandatory.
pub struct UserProviderCredentialRepository {
    pool: PgPool,
    encryption: Option<CredentialEncryption>,
}

impl std::fmt::Debug for UserProviderCredentialRepository {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UserProviderCredentialRepository")
            .field("pool", &"PgPool")
            .field("encryption", &self.encryption.is_some())
            .finish()
    }
}

impl UserProviderCredentialRepository {
    fn normalize_provider_instance_name_for_db(
        provider_instance_name: Option<&str>,
    ) -> Option<&str> {
        normalize_provider_instance_name(provider_instance_name)
    }

    /// Create a new repository without encryption
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self {
            pool,
            encryption: None,
        }
    }

    /// Create a new repository with credential encryption enabled
    #[must_use]
    pub const fn new_with_encryption(pool: PgPool, encryption: CredentialEncryption) -> Self {
        Self {
            pool,
            encryption: Some(encryption),
        }
    }

    fn encrypt_credential(&self, data: &serde_json::Value) -> Result<serde_json::Value> {
        match &self.encryption {
            Some(enc) => enc.encrypt_to_value(data),
            None => Err(crate::Error::Internal(
                "Credential encryption must be configured before storing provider credentials"
                    .to_string(),
            )),
        }
    }

    fn decrypt_credential(&self, data: &serde_json::Value) -> Result<serde_json::Value> {
        match &self.encryption {
            Some(enc) => enc.decrypt_value(data),
            None => Err(crate::Error::Internal(
                "Credential encryption must be configured before reading provider credentials"
                    .to_string(),
            )),
        }
    }

    /// Decrypt credentials on a `UserProviderCredential` in place.
    fn decrypt_in_credential(
        &self,
        mut cred: UserProviderCredential,
    ) -> Result<UserProviderCredential> {
        cred.credential_data = self.decrypt_credential(&cred.credential_data)?;
        Ok(cred)
    }

    fn decrypt_credential_row(
        &self,
        row: UserProviderCredentialRow,
    ) -> Result<UserProviderCredential> {
        self.decrypt_in_credential(row.try_into()?)
    }

    /// Decrypt credentials on a list of `UserProviderCredentialRow`.
    fn decrypt_credential_rows(
        &self,
        rows: Vec<UserProviderCredentialRow>,
    ) -> Result<Vec<UserProviderCredential>> {
        rows.into_iter()
            .map(|row| self.decrypt_credential_row(row))
            .collect()
    }

    /// Get all credentials for a user (decrypted)
    pub async fn get_by_user(&self, user_id: UserId) -> Result<Vec<UserProviderCredential>> {
        let rows = sqlx::query_as!(
            UserProviderCredentialRow,
            r#"
            SELECT id, user_id as "user_id: UserId", provider, server_id,
                   provider_instance_name, credential_data as "credential_data!: serde_json::Value",
                   expires_at, created_at, updated_at
            FROM user_media_provider_credentials
            WHERE user_id = $1
            ORDER BY created_at DESC
            "#,
            user_id as UserId,
        )
        .fetch_all(&self.pool)
        .await?;

        self.decrypt_credential_rows(rows)
    }

    /// Get credential by ID (decrypted)
    pub async fn get_by_id(&self, id: i64) -> Result<Option<UserProviderCredential>> {
        let row = sqlx::query_as!(
            UserProviderCredentialRow,
            r#"
            SELECT id, user_id as "user_id: UserId", provider, server_id,
                   provider_instance_name, credential_data as "credential_data!: serde_json::Value",
                   expires_at, created_at, updated_at
            FROM user_media_provider_credentials
            WHERE id = $1
            "#,
            id,
        )
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(row) => Ok(Some(self.decrypt_credential_row(row)?)),
            None => Ok(None),
        }
    }

    /// Get user credential for a specific provider and server (decrypted)
    pub async fn get_by_provider_and_server(
        &self,
        user_id: UserId,
        provider: &str,
        server_id: &str,
    ) -> Result<Option<UserProviderCredential>> {
        let row = sqlx::query_as!(
            UserProviderCredentialRow,
            r#"
            SELECT id, user_id as "user_id: UserId", provider, server_id,
                   provider_instance_name, credential_data as "credential_data!: serde_json::Value",
                   expires_at, created_at, updated_at
            FROM user_media_provider_credentials
            WHERE user_id = $1 AND provider = $2 AND server_id = $3
            "#,
            user_id as UserId,
            provider_type_code(provider)?,
            server_id,
        )
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(row) => Ok(Some(self.decrypt_credential_row(row)?)),
            None => Ok(None),
        }
    }

    /// Get all credentials for a specific provider type (decrypted)
    pub async fn get_by_provider(
        &self,
        user_id: UserId,
        provider: &str,
    ) -> Result<Vec<UserProviderCredential>> {
        let rows = sqlx::query_as!(
            UserProviderCredentialRow,
            r#"
            SELECT id, user_id as "user_id: UserId", provider, server_id,
                   provider_instance_name, credential_data as "credential_data!: serde_json::Value",
                   expires_at, created_at, updated_at
            FROM user_media_provider_credentials
            WHERE user_id = $1 AND provider = $2
            "#,
            user_id as UserId,
            provider_type_code(provider)?,
        )
        .fetch_all(&self.pool)
        .await?;

        self.decrypt_credential_rows(rows)
    }

    /// Create a new user credential (encrypts before storage)
    pub async fn create(
        &self,
        credential: &UserProviderCredential,
    ) -> Result<UserProviderCredential> {
        let encrypted_data = self.encrypt_credential(&credential.credential_data)?;
        let provider_code = provider_type_code(&credential.provider)?;

        let created = sqlx::query_as!(
            UserProviderCredentialRow,
            r#"
            INSERT INTO user_media_provider_credentials
            (user_id, provider, server_id, provider_instance_name, credential_data, expires_at)
            VALUES ($1, $2, $3, $4, $5, $6)
            RETURNING id, user_id as "user_id: UserId", provider, server_id,
                      provider_instance_name, credential_data as "credential_data!: serde_json::Value",
                      expires_at, created_at, updated_at
            "#,
            credential.user_id as UserId,
            provider_code,
            credential.server_id.as_str(),
            Self::normalize_provider_instance_name_for_db(
                credential.provider_instance_name.as_deref(),
            ),
            encrypted_data,
            credential.expires_at,
        )
        .fetch_one(&self.pool)
        .await?;

        self.decrypt_credential_row(created)
    }

    /// Insert or replace the credential for a `(user_id, provider, server_id)` binding.
    ///
    /// This is intentionally a repository-level primitive so provider login flows do not
    /// implement non-atomic delete-then-create upserts.
    pub async fn upsert_by_user_provider_server(
        &self,
        credential: &UserProviderCredential,
    ) -> Result<UserProviderCredential> {
        let encrypted_data = self.encrypt_credential(&credential.credential_data)?;
        let provider_code = provider_type_code(&credential.provider)?;

        let upserted = sqlx::query_as!(
            UserProviderCredentialRow,
            r#"
            INSERT INTO user_media_provider_credentials
            (user_id, provider, server_id, provider_instance_name, credential_data, expires_at)
            VALUES ($1, $2, $3, $4, $5, $6)
            ON CONFLICT (user_id, provider, server_id)
            DO UPDATE SET
                provider_instance_name = EXCLUDED.provider_instance_name,
                credential_data = EXCLUDED.credential_data,
                expires_at = EXCLUDED.expires_at,
                updated_at = NOW()
            RETURNING id, user_id as "user_id: UserId", provider, server_id,
                      provider_instance_name, credential_data as "credential_data!: serde_json::Value",
                      expires_at, created_at, updated_at
            "#,
            credential.user_id as UserId,
            provider_code,
            credential.server_id.as_str(),
            Self::normalize_provider_instance_name_for_db(
                credential.provider_instance_name.as_deref(),
            ),
            encrypted_data,
            credential.expires_at,
        )
        .fetch_one(&self.pool)
        .await?;

        self.decrypt_credential_row(upserted)
    }

    /// Update an existing user credential (encrypts before storage)
    pub async fn update(&self, credential: &UserProviderCredential) -> Result<()> {
        let encrypted_data = self.encrypt_credential(&credential.credential_data)?;

        let result = sqlx::query!(
            r"
            UPDATE user_media_provider_credentials
            SET provider_instance_name = $2, credential_data = $3, expires_at = $4, updated_at = NOW()
            WHERE id = $1
            ",
            credential.id,
            Self::normalize_provider_instance_name_for_db(
                credential.provider_instance_name.as_deref(),
            ),
            encrypted_data,
            credential.expires_at,
        )
        .execute(&self.pool)
        .await?;

        if result.rows_affected() == 0 {
            return Err(crate::Error::NotFound(format!(
                "User provider credential '{}' not found",
                credential.id
            )));
        }

        Ok(())
    }

    /// Delete a user credential
    pub async fn delete(&self, id: i64) -> Result<()> {
        let result = sqlx::query!(
            "DELETE FROM user_media_provider_credentials WHERE id = $1",
            id,
        )
        .execute(&self.pool)
        .await?;

        if result.rows_affected() == 0 {
            return Err(crate::Error::NotFound(format!(
                "User provider credential '{id}' not found"
            )));
        }

        Ok(())
    }

    /// Delete all credentials for a user and provider
    pub async fn delete_by_user_and_provider(&self, user_id: UserId, provider: &str) -> Result<()> {
        let result = sqlx::query!(
            "DELETE FROM user_media_provider_credentials WHERE user_id = $1 AND provider = $2",
            user_id as UserId,
            provider_type_code(provider)?,
        )
        .execute(&self.pool)
        .await?;

        if result.rows_affected() == 0 {
            return Err(crate::Error::NotFound(format!(
                "No credentials found for user '{user_id}' and provider '{provider}'"
            )));
        }

        Ok(())
    }

    /// Get all expired credentials (for cleanup jobs, decrypted)
    pub async fn get_expired(&self) -> Result<Vec<UserProviderCredential>> {
        let rows = sqlx::query_as!(
            UserProviderCredentialRow,
            r#"
            SELECT id, user_id as "user_id: UserId", provider, server_id,
                   provider_instance_name, credential_data as "credential_data!: serde_json::Value",
                   expires_at, created_at, updated_at
            FROM user_media_provider_credentials
            WHERE expires_at IS NOT NULL AND expires_at <= NOW()
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        self.decrypt_credential_rows(rows)
    }

    /// Delete all expired credentials
    pub async fn delete_expired(&self) -> Result<u64> {
        let result = sqlx::query!(
            "DELETE FROM user_media_provider_credentials WHERE expires_at IS NOT NULL AND expires_at <= NOW()"
        )
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credential_encryption::CredentialEncryption;
    use crate::models::SortDirection;
    use serde_json::json;

    // Note: These are unit tests for the repository structure.
    // Integration tests with actual database should be in tests/ directory.

    fn order_by_sql(query: &ProviderInstanceListQuery) -> String {
        let mut builder = sqlx::QueryBuilder::<sqlx::Postgres>::new("");
        ProviderInstanceRepository::push_list_order_by(&mut builder, query);
        builder.sql().to_string()
    }

    #[test]
    fn test_provider_instance_list_select_columns_are_explicit() {
        assert_eq!(
            ProviderInstanceRepository::INSTANCE_SELECT_COLUMNS,
            "name, endpoint, comment, jwt_secret, custom_ca, timeout, tls, insecure_tls, providers, enabled, created_at, updated_at"
        );
    }

    #[test]
    fn test_provider_instance_list_order_by_uses_static_sort_branches() {
        let mut query = ProviderInstanceListQuery {
            sort_by: ProviderInstanceListSortBy::Name,
            sort_direction: SortDirection::Asc,
            ..ProviderInstanceListQuery::default()
        };
        assert_eq!(order_by_sql(&query), " ORDER BY name ASC, created_at ASC");

        query.sort_by = ProviderInstanceListSortBy::Endpoint;
        query.sort_direction = SortDirection::Desc;
        assert_eq!(
            order_by_sql(&query),
            " ORDER BY endpoint DESC, created_at DESC"
        );

        query.sort_by = ProviderInstanceListSortBy::UpdatedAt;
        query.sort_direction = SortDirection::Asc;
        assert_eq!(order_by_sql(&query), " ORDER BY updated_at ASC, name ASC");

        query.sort_by = ProviderInstanceListSortBy::CreatedAt;
        query.sort_direction = SortDirection::Desc;
        assert_eq!(order_by_sql(&query), " ORDER BY created_at DESC, name DESC");
    }

    #[tokio::test]
    async fn test_provider_instance_repo_rejects_plaintext_sensitive_fields_when_encryption_enabled(
    ) {
        let pool = PgPool::connect_lazy("postgresql://test").unwrap();
        let encryption = CredentialEncryption::new(&[7u8; 32]).unwrap();
        let repo = ProviderInstanceRepository::new_with_encryption(pool, encryption);

        let err = repo.decrypt_field(Some("plaintext-secret")).unwrap_err();
        assert!(
            err.to_string().contains("plaintext sensitive data"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn test_user_provider_credential_repo_rejects_plaintext_json_when_encryption_enabled() {
        let pool = PgPool::connect_lazy("postgresql://test").unwrap();
        let encryption = CredentialEncryption::new(&[9u8; 32]).unwrap();
        let repo = UserProviderCredentialRepository::new_with_encryption(pool, encryption);

        let err = repo
            .decrypt_credential(&json!({"token": "plaintext"}))
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("Credential value must be an encrypted string"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn test_provider_instance_repo_requires_encryption_when_sensitive_fields_present() {
        let pool = PgPool::connect_lazy("postgresql://test").unwrap();
        let repo = ProviderInstanceRepository::new(pool);

        let err = repo
            .ensure_encryption_for_sensitive_fields(&ProviderInstance {
                name: "remote".to_string(),
                endpoint: "http://remote.example.com:50051".to_string(),
                comment: None,
                jwt_secret: Some("secret".to_string()),
                custom_ca: None,
                timeout: "10s".to_string(),
                tls: false,
                insecure_tls: false,
                providers: vec!["alist".to_string()],
                enabled: true,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            })
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("Credential encryption must be configured"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn test_provider_instance_repo_requires_encryption_for_sensitive_reads() {
        let pool = PgPool::connect_lazy("postgresql://test").unwrap();
        let repo = ProviderInstanceRepository::new(pool);

        let err = repo.decrypt_field(Some("enc:placeholder")).unwrap_err();
        assert!(
            err.to_string()
                .contains("Credential encryption must be configured"),
            "unexpected error: {err}"
        );
        assert_eq!(repo.decrypt_field(None).unwrap(), None);
        assert_eq!(repo.decrypt_field(Some("")).unwrap(), None);
    }

    #[tokio::test]
    async fn test_user_provider_credential_repo_requires_encryption_for_storage() {
        let pool = PgPool::connect_lazy("postgresql://test").unwrap();
        let repo = UserProviderCredentialRepository::new(pool);

        let err = repo
            .encrypt_credential(&json!({"token": "plaintext"}))
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("Credential encryption must be configured"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn test_user_provider_credential_repo_requires_encryption_for_reads() {
        let pool = PgPool::connect_lazy("postgresql://test").unwrap();
        let repo = UserProviderCredentialRepository::new(pool);

        let err = repo
            .decrypt_credential(&json!("enc:placeholder"))
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("Credential encryption must be configured"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_normalize_provider_instance_name_for_db() {
        assert_eq!(
            UserProviderCredentialRepository::normalize_provider_instance_name_for_db(None),
            None
        );
        assert_eq!(
            UserProviderCredentialRepository::normalize_provider_instance_name_for_db(Some("")),
            None
        );
        assert_eq!(
            UserProviderCredentialRepository::normalize_provider_instance_name_for_db(Some("   ")),
            None
        );
        assert_eq!(
            UserProviderCredentialRepository::normalize_provider_instance_name_for_db(Some(
                "alist-main"
            )),
            Some("alist-main")
        );
        assert_eq!(
            UserProviderCredentialRepository::normalize_provider_instance_name_for_db(Some(
                "  alist-main  "
            )),
            Some("alist-main")
        );
    }
}
