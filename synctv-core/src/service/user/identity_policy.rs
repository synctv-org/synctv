use crate::{
    models::{User, UserId},
    service::auth::TokenCredentialBinding,
    Error, Result,
};

use super::UserService;

impl UserService {
    pub(super) fn normalized_email_domain(email: &str) -> Result<String> {
        let (_, domain) = email.trim().rsplit_once('@').ok_or_else(|| {
            Error::InvalidInput("Email must include a domain for whitelist validation".to_string())
        })?;
        let domain = domain.trim().to_ascii_lowercase();
        if domain.is_empty() {
            return Err(Error::InvalidInput(
                "Email must include a domain for whitelist validation".to_string(),
            ));
        }
        Ok(domain)
    }

    pub(super) fn email_domain_allowed_by_whitelist(email: &str, whitelist: &str) -> Result<bool> {
        let domain = Self::normalized_email_domain(email)?;
        let allowed_domains =
            crate::service::SettingsRegistry::normalize_email_whitelist_domains(whitelist);

        Ok(allowed_domains.is_empty() || allowed_domains.iter().any(|allowed| allowed == &domain))
    }

    pub(super) fn validate_email_whitelist_policy(&self, email: &str) -> Result<()> {
        let Some(registry) = self.settings_registry.as_ref() else {
            return Ok(());
        };
        if registry.email_whitelist_enabled.get()? {
            let whitelist = registry.email_whitelist.get()?;
            if !Self::email_domain_allowed_by_whitelist(email, &whitelist)? {
                return Err(Error::InvalidInput(
                    "Email domain is not allowed for registration".to_string(),
                ));
            }
        }
        Ok(())
    }

    pub(super) fn validate_password(&self, password: &str) -> Result<()> {
        crate::validation::PasswordValidator::from_config(&self.password_complexity)
            .validate(password)
            .map_err(|error| Error::InvalidInput(error.to_string()))
    }

    pub(super) fn opaque_credential_identifier_for_new_user(username: &str) -> Vec<u8> {
        format!("synctv:user:{}", Self::canonical_username(username)).into_bytes()
    }

    pub(super) fn opaque_credential_identifier_for_user_id(user_id: &UserId) -> Vec<u8> {
        format!("synctv:user-id:{}", user_id.as_i64()).into_bytes()
    }

    pub(super) fn normalize_login_identifier(identifier: &str) -> String {
        let trimmed = identifier.trim();
        if trimmed.contains('@') {
            trimmed.to_ascii_lowercase()
        } else {
            Self::canonical_username(trimmed)
        }
    }

    pub(super) fn canonical_username(username: &str) -> String {
        username.trim().to_lowercase()
    }

    pub(super) fn normalize_username_for_storage(username: &str) -> Result<String> {
        let username = Self::canonical_username(username);
        Self::validate_username(&username)?;
        Ok(username)
    }

    pub(super) async fn get_by_login_identifier(&self, identifier: &str) -> Result<Option<User>> {
        let normalized = Self::normalize_login_identifier(identifier);
        if normalized.contains('@') {
            Ok(self
                .user_email_repository
                .get_by_email(&normalized)
                .await?
                .map(|user_with_email| user_with_email.user))
        } else {
            self.repository.get_by_username(&normalized).await
        }
    }
}

pub(super) const fn password_binding(version: i32) -> TokenCredentialBinding {
    TokenCredentialBinding::Password { version }
}
