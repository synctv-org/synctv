use crate::{models::UserId, service::UserService};

#[async_trait::async_trait]
impl crate::service::ws_ticket::UserValidator for UserService {
    async fn validate_for_ticket(
        &self,
        user_id: &UserId,
    ) -> crate::Result<crate::service::ws_ticket::UserValidationResult> {
        let user = self
            .repository
            .get_by_id(user_id)
            .await?
            .ok_or_else(|| crate::Error::NotFound("User not found".to_string()))?;

        if !user.can_login() {
            return Err(crate::Error::Authorization(
                "Authentication failed".to_string(),
            ));
        }

        let password_version = self
            .user_password_repository
            .get_state(user_id)
            .await?
            .version;

        Ok(crate::service::ws_ticket::UserValidationResult { password_version })
    }
}
