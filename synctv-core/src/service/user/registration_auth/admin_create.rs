use crate::{
    models::{SignupMethod, User, UserId},
    repository::PasswordCredentialMaterial,
    service::UserService,
    Error, Result,
};

impl UserService {
    pub async fn create_user_with_optional_direct_password(
        &self,
        username: String,
        email: Option<String>,
        password: Option<String>,
        role: Option<crate::models::UserRole>,
        status: Option<crate::models::UserStatus>,
        banned_by: Option<&UserId>,
    ) -> Result<User> {
        let username = Self::normalize_username_for_storage(&username)?;
        if let Some(ref email) = email {
            Self::validate_email(email)?;
        }
        if let Some(password) = password.as_deref() {
            self.validate_password(password)?;
        }

        let opaque_record = password
            .as_deref()
            .map(|password| {
                let credential_identifier =
                    Self::opaque_credential_identifier_for_new_user(&username);
                self.opaque_password_service
                    .register_password(&credential_identifier, password)
            })
            .transpose()?;

        let signup_method = if opaque_record.is_some() {
            SignupMethod::Password
        } else {
            SignupMethod::AdminCreated
        };
        let mut user = User::new(username.clone(), signup_method);
        if let Some(role) = role {
            user.role = role;
        }
        if let Some(status) = status {
            user.status = status;
        }
        let mut tx = self.repository.pool().begin().await?;
        let created_user = self
            .repository
            .create_with_executor(&user, &mut *tx)
            .await
            .map_err(Self::map_registration_identity_conflict)?;
        self.user_email_repository
            .create_for_user_with_executor(&created_user, email.as_deref(), &mut *tx)
            .await
            .map_err(Self::map_registration_identity_conflict)?;
        if let Some(opaque_record) = opaque_record.as_ref() {
            self.user_password_repository
                .create_for_user_with_executor(
                    &created_user,
                    PasswordCredentialMaterial::opaque_only(opaque_record),
                    &mut *tx,
                )
                .await?;
        }
        if user.status == crate::models::UserStatus::Banned {
            sqlx::query!(
                r#"
                INSERT INTO user_bans (user_id, banned_by, reason, starts_at)
                VALUES ($1, $2, $3, CURRENT_TIMESTAMP)
                "#,
                created_user.id.as_i64(),
                banned_by.map(UserId::as_i64),
                "created with banned status",
            )
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;

        let created_user = if user.status == crate::models::UserStatus::Banned {
            self.repository
                .get_by_id(&created_user.id)
                .await?
                .ok_or_else(|| Error::NotFound(format!("User {} not found", created_user.id)))?
        } else {
            created_user
        };
        self.cache_username_best_effort(
            &created_user.id,
            &username,
            "create_user_with_optional_direct_password",
        )
        .await;
        Ok(created_user)
    }
}
