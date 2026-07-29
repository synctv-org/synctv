use tracing::info;

use crate::{
    models::{SignupMethod, User},
    repository::query_builder::trusted_dynamic_sql,
    service::{
        oauth2::{OAuth2LinkResult, OAuth2PendingRegistration, OAuth2Service, OAuth2UserInfo},
        user::PendingRegistrationConflict,
        UserService,
    },
    Error, InternalExt, Result,
};

impl OAuth2Service {
    fn user_service(&self) -> Result<&crate::service::UserService> {
        self.user_service.as_deref().ok_or_else(|| {
            Error::ServiceUnavailable("OAuth2 user service is not configured".to_string())
        })
    }

    /// Find an existing user by `OAuth2` provider, or create a new one and link the provider,
    /// all within a single database transaction.
    ///
    /// This prevents the race condition where two concurrent `OAuth2` logins for the same
    /// provider identity both find no existing user and both create separate user records.
    pub async fn find_or_create_and_link(
        &self,
        instance_name: &str,
        user_info: &OAuth2UserInfo,
    ) -> Result<OAuth2LinkResult> {
        let repository = self.repository()?;
        if let Some(user_id) = self
            .find_user_by_provider_instance(instance_name, &user_info.provider_user_id)
            .await?
        {
            return Ok(OAuth2LinkResult::Linked {
                user_id,
                is_new: false,
            });
        }

        let signup_policy = self.signup_policy_for(instance_name).await?;
        if !signup_policy.enable_signup {
            return Err(Error::Authorization(
                "OAuth2 registration is disabled for this provider".to_string(),
            ));
        }

        let pool = repository.pool();
        let mut tx = pool.begin().await?;

        let advisory_lock_key = format!("oauth2:{instance_name}:{}", user_info.provider_user_id);
        sqlx::query!(
            "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
            advisory_lock_key,
        )
        .fetch_one(&mut *tx)
        .await
        .internal_with_err("Failed to acquire OAuth2 identity advisory lock")?;

        let existing = repository
            .find_by_provider_instance_with_executor(
                instance_name,
                &user_info.provider_user_id,
                &mut *tx,
            )
            .await?;

        if let Some(mapping) = existing {
            tx.rollback().await?;
            return Ok(OAuth2LinkResult::Linked {
                user_id: mapping.user_id,
                is_new: false,
            });
        }

        let (base_username, candidates) = UserService::oauth2_username_candidates(
            &user_info.provider_user_id,
            &user_info.username,
        )?;
        if signup_policy.signup_need_review {
            return self
                .create_pending_registration_with_review(tx, instance_name, user_info, &candidates)
                .await;
        }

        let new_user = self
            .create_oauth2_user_with_candidates(&mut tx, user_info, &base_username, &candidates)
            .await?;

        match self
            .upsert_user_provider_with_executor(&new_user.id, user_info, &mut *tx)
            .await
        {
            Ok(()) => {}
            Err(Error::AlreadyExists(_)) => {
                tx.rollback().await?;
                let existing = repository
                    .find_by_provider_instance(instance_name, &user_info.provider_user_id)
                    .await?
                    .ok_or_else(|| {
                        Error::Internal(
                            "OAuth2 mapping conflicted but could not be reloaded".to_string(),
                        )
                    })?;
                return Ok(OAuth2LinkResult::Linked {
                    user_id: existing.user_id,
                    is_new: false,
                });
            }
            Err(err) => return Err(err),
        }

        tx.commit().await?;

        info!(
            user_id = %new_user.id,
            provider = %user_info.provider.as_str(),
            provider_instance = %instance_name,
            "Created new user via OAuth2 and linked provider in single transaction"
        );

        Ok(OAuth2LinkResult::Linked {
            user_id: new_user.id,
            is_new: true,
        })
    }

    async fn create_pending_registration_with_review(
        &self,
        mut tx: sqlx::Transaction<'_, sqlx::Postgres>,
        instance_name: &str,
        user_info: &OAuth2UserInfo,
        candidates: &[String],
    ) -> Result<OAuth2LinkResult> {
        let user_service = self.user_service()?;

        let mut pending_request_id = None;
        for candidate in candidates {
            let username_in_use = sqlx::query_scalar!(
                r#"
                SELECT EXISTS(
                    SELECT 1
                    FROM users
                    WHERE LOWER(username) = LOWER($1)
                      AND deleted_at IS NULL
                )
                "#,
                candidate,
            )
            .fetch_one(&mut *tx)
            .await?;
            if username_in_use.unwrap_or(false) {
                continue;
            }

            UserService::lock_oauth2_pending_registration_identity(
                &mut tx,
                candidate,
                None,
                instance_name,
                &user_info.provider_user_id,
            )
            .await
            .internal_with_err("Failed to acquire OAuth2 pending-registration locks")?;

            match user_service
                .pending_oauth2_registration_conflict(
                    candidate,
                    None,
                    instance_name,
                    &user_info.provider_user_id,
                    &mut *tx,
                )
                .await
            {
                Ok(Some(PendingRegistrationConflict::OAuth2Identity(request_id))) => {
                    tx.rollback().await?;
                    return Ok(OAuth2LinkResult::PendingReview(OAuth2PendingRegistration {
                        request_id,
                    }));
                }
                Ok(Some(PendingRegistrationConflict::Username)) => {
                    continue;
                }
                Ok(Some(PendingRegistrationConflict::Email)) => {
                    tx.rollback().await?;
                    return Err(Error::AlreadyExists(
                        synctv_common::messages::USERNAME_OR_EMAIL_ALREADY_TAKEN.to_string(),
                    ));
                }
                Ok(None) => {}
                Err(err) => {
                    tx.rollback().await?;
                    return Err(err);
                }
            }

            match user_service
                .create_oauth2_registration_request_with_executor(
                    candidate,
                    &user_info.provider_user_id,
                    user_info,
                    &mut *tx,
                )
                .await
            {
                Ok(request_id) => {
                    pending_request_id = Some(request_id);
                    break;
                }
                Err(Error::AlreadyExists(message)) => {
                    tx.rollback().await?;
                    return Err(Error::AlreadyExists(message));
                }
                Err(err) => {
                    tx.rollback().await?;
                    return Err(err);
                }
            }
        }

        let request_id = pending_request_id.ok_or_else(|| {
            Error::Internal(format!(
                "Could not generate a unique username for base '{}' after {} attempts",
                user_info.username,
                candidates.len()
            ))
        })?;
        tx.commit().await?;
        Ok(OAuth2LinkResult::PendingReview(OAuth2PendingRegistration {
            request_id,
        }))
    }

    async fn create_oauth2_user_with_candidates(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        user_info: &OAuth2UserInfo,
        base_username: &str,
        candidates: &[String],
    ) -> Result<User> {
        let user_service = self.user_service()?;
        let mut new_user = None;
        for (attempt, candidate) in candidates.iter().enumerate() {
            let savepoint = format!("oauth2_user_create_{attempt}");
            sqlx::query(trusted_dynamic_sql(format!("SAVEPOINT {savepoint}")))
                .execute(&mut **tx)
                .await
                .internal_with_err("Failed to create OAuth2 user savepoint")?;

            let user = User::new_with_status(
                candidate.clone(),
                SignupMethod::OAuth2,
                crate::models::UserStatus::Active,
            );
            match user_service
                .repository
                .create_with_executor(&user, &mut **tx)
                .await
            {
                Ok(created_user) => {
                    sqlx::query(trusted_dynamic_sql(format!(
                        "RELEASE SAVEPOINT {savepoint}"
                    )))
                    .execute(&mut **tx)
                    .await
                    .internal_with_err("Failed to release OAuth2 user savepoint")?;

                    user_service
                        .cache_username_best_effort(
                            &created_user.id,
                            candidate,
                            "create_or_load_by_oauth2",
                        )
                        .await;

                    if candidate == base_username {
                        tracing::info!(
                            "Created new user {} (username='{}', sanitized from '{}') via OAuth2 provider {} (provider_user_id={})",
                            created_user.id,
                            candidate,
                            user_info.username,
                            user_info.provider.as_str(),
                            user_info.provider_user_id
                        );
                    } else {
                        tracing::info!(
                            "Username '{}' was taken; created user {} as '{}' (original '{}') via OAuth2 provider {} (provider_user_id={})",
                            base_username,
                            created_user.id,
                            candidate,
                            user_info.username,
                            user_info.provider.as_str(),
                            user_info.provider_user_id
                        );
                    }

                    new_user = Some(created_user);
                    break;
                }
                Err(error) if UserService::is_username_conflict(&error) => {
                    sqlx::query(trusted_dynamic_sql(format!(
                        "ROLLBACK TO SAVEPOINT {savepoint}"
                    )))
                    .execute(&mut **tx)
                    .await
                    .internal_with_err(
                        "Failed to roll back OAuth2 user savepoint after username collision",
                    )?;
                }
                Err(err) => {
                    sqlx::query(trusted_dynamic_sql(format!(
                        "ROLLBACK TO SAVEPOINT {savepoint}"
                    )))
                    .execute(&mut **tx)
                    .await
                    .internal_with_err(
                        "Failed to roll back OAuth2 user savepoint after create error",
                    )?;
                    return Err(err);
                }
            }
        }

        new_user.ok_or_else(|| {
            Error::Internal(format!(
                "Could not generate a unique username for base '{}' after {} attempts",
                user_info.username,
                candidates.len()
            ))
        })
    }
}
