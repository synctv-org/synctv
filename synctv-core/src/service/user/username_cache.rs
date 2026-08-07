use std::collections::HashMap;

use crate::{models::UserId, service::UserService, Error, Result};

impl UserService {
    pub(crate) async fn cache_username_best_effort(
        &self,
        user_id: &UserId,
        username: &str,
        operation: &'static str,
    ) {
        if let Err(error) = self.username_cache.set(user_id, username).await {
            Self::log_username_cache_write_failure(user_id, operation, &error);
        }
    }

    fn log_username_cache_write_failure(user_id: &UserId, operation: &'static str, error: &Error) {
        tracing::warn!(
            error = %error,
            user_id = %user_id,
            operation,
            "Username cache update failed after primary user mutation; continuing with durable result"
        );
    }

    pub(crate) fn oauth2_username_candidates(
        provider_user_id: &str,
        username: &str,
    ) -> Result<(String, Vec<String>)> {
        let base_username = Self::normalize_oauth2_username_base(provider_user_id, username);
        Self::validate_username(&base_username)?;

        let max_attempts = 10;
        let mut candidates = Vec::with_capacity(max_attempts);
        candidates.push(base_username.clone());
        for _ in 1..max_attempts {
            let max_base_len = 42;
            let base = if base_username.chars().count() > max_base_len {
                base_username.chars().take(max_base_len).collect::<String>()
            } else {
                base_username.clone()
            };
            let suffix = synctv_common::snanoid!(6);
            candidates.push(format!("{base}_{suffix}"));
        }

        Ok((base_username, candidates))
    }

    fn normalize_oauth2_username_base(provider_user_id: &str, username: &str) -> String {
        let normalized = username
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
            .collect::<String>()
            .trim()
            .to_lowercase();

        if normalized.is_empty() {
            format!(
                "user_{}",
                &provider_user_id[..provider_user_id.len().min(20)]
            )
        } else {
            normalized
        }
    }

    pub(crate) async fn invalidate_username_cache_best_effort(
        &self,
        user_id: &UserId,
        operation: &'static str,
    ) {
        if let Err(error) = self.invalidate_username_cache(user_id).await {
            Self::log_username_cache_write_failure(user_id, operation, &error);
        }
    }

    pub(super) fn validate_username(username: &str) -> Result<()> {
        crate::validation::UsernameValidator::new()
            .validate(username)
            .map_err(|e| Error::InvalidInput(e.to_string()))
    }

    pub(super) fn validate_email(email: &str) -> Result<()> {
        let email = email.trim();
        if email.is_empty() {
            return Err(Error::InvalidInput("Email cannot be empty".to_string()));
        }
        crate::validation::EmailValidator::new()
            .validate(email)
            .map_err(|e| Error::InvalidInput(e.to_string()))
    }

    pub async fn get_username(&self, user_id: &UserId) -> Result<Option<String>> {
        match self.username_cache.get(user_id).await {
            Ok(Some(username)) => return Ok(Some(username)),
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    user_id = %user_id,
                    "Username cache read failed; falling back to database"
                );
            }
        }

        if let Some(user) = self.repository.get_by_id(user_id).await? {
            let username = user.username.clone();
            self.cache_username_best_effort(user_id, &username, "get_username")
                .await;
            Ok(Some(username))
        } else {
            Ok(None)
        }
    }

    pub async fn get_usernames(&self, user_ids: &[UserId]) -> Result<HashMap<UserId, String>> {
        let mut result = match self.username_cache.get_batch(user_ids).await {
            Ok(result) => result,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    requested = user_ids.len(),
                    "Username cache batch read failed; falling back to database"
                );
                HashMap::new()
            }
        };
        let missing_ids: Vec<UserId> = user_ids
            .iter()
            .filter(|id| !result.contains_key(*id))
            .copied()
            .collect();

        if !missing_ids.is_empty() {
            let users = self.repository.get_by_ids(&missing_ids).await?;
            for user in users {
                let user_id = user.id;
                let username = user.username.clone();
                self.cache_username_best_effort(&user_id, &username, "get_usernames")
                    .await;
                result.insert(user_id, username);
            }
        }

        Ok(result)
    }

    pub async fn invalidate_username_cache(&self, user_id: &UserId) -> Result<()> {
        self.username_cache.invalidate(user_id).await
    }

    pub(crate) async fn notify_user_invalidation(&self, user_id: &UserId) {
        if let Some(ref service) = self.cache_invalidation {
            if let Err(e) = service.invalidate_and_broadcast_user(user_id).await {
                tracing::warn!(
                    error = %e,
                    user_id = %user_id,
                    "Failed to broadcast user cache invalidation"
                );
            }
        }
    }
}
