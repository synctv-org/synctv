use crate::{
    models::{oauth2_client::OAuth2Provider, SignupMethod, User},
    service::UserService,
    Error, Result,
};

impl UserService {
    /// Create a new user for an `OAuth2` login.
    ///
    /// This method is called during `OAuth2` login flow when no existing provider
    /// mapping was found (the caller must check provider-based lookup first).
    /// It creates a new user with a random password.
    ///
    /// If the desired username is already taken (detected atomically via DB
    /// UNIQUE constraint), a numeric suffix is appended (e.g., "alice" ->
    /// "`alice_2`", "`alice_3`") to avoid collisions. This prevents account
    /// takeover where an `OAuth2` user with a matching username would silently
    /// gain access to an existing local account.
    ///
    /// Note: This method doesn't save the `OAuth2` provider mapping - that's handled
    /// by `OAuth2Service::upsert_user_provider`.
    pub async fn create_or_load_by_oauth2(
        &self,
        provider: &OAuth2Provider,
        provider_user_id: &str,
        username: &str,
    ) -> Result<User> {
        let (base_username, candidates) =
            Self::oauth2_username_candidates(provider_user_id, username)?;
        for candidate in &candidates {
            let user = User::new_with_status(
                candidate.clone(),
                SignupMethod::OAuth2,
                crate::models::UserStatus::Active,
            );
            let created = async {
                let mut tx = self.repository.pool().begin().await?;
                let created_user = self
                    .repository
                    .create_with_executor(&user, &mut *tx)
                    .await?;
                tx.commit().await?;
                Ok::<_, Error>(created_user)
            }
            .await;
            match created {
                Ok(created_user) => {
                    self.cache_username_best_effort(
                        &created_user.id,
                        candidate,
                        "create_or_load_by_oauth2",
                    )
                    .await;

                    if candidate == &base_username {
                        tracing::info!(
                            "Created new user {} (username='{}', sanitized from '{}') via OAuth2 provider {} (provider_user_id={})",
                            created_user.id,
                            candidate,
                            username,
                            provider.as_str(),
                            provider_user_id
                        );
                    } else {
                        tracing::info!(
                            "Username '{}' was taken; created user {} as '{}' (original '{}') via OAuth2 provider {} (provider_user_id={})",
                            base_username,
                            created_user.id,
                            candidate,
                            username,
                            provider.as_str(),
                            provider_user_id
                        );
                    }

                    return Ok(created_user);
                }
                Err(ref error) if Self::is_username_conflict(error) => {}
                Err(e) => return Err(e),
            }
        }

        Err(Error::Internal(format!(
            "Could not generate a unique username for base '{username}' after {} attempts",
            candidates.len()
        )))
    }
}
