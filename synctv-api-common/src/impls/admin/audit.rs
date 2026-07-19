use synctv_core::models::{AuditAction, AuditDetails, AuditTargetType, UserId};
use synctv_core::service::AuditEventParams;

use super::{AdminApiImpl, RequestContext};

impl AdminApiImpl {
    /// Best-effort admin audit log helper.
    pub(in crate::impls::admin) async fn log_admin_action(
        &self,
        admin_user_id: &UserId,
        action: AuditAction,
        target_type: AuditTargetType,
        target_id: Option<String>,
        details: AuditDetails,
        ctx: &RequestContext,
    ) {
        let admin_username = match self.load_admin_actor(admin_user_id).await {
            Ok(actor) => actor.username,
            Err(error) => {
                tracing::error!(
                    error = %error,
                    admin_user_id = %admin_user_id,
                    action = %action,
                    "AUDIT LOG SKIPPED: failed to resolve admin actor username. Manual review required.",
                );
                return;
            }
        };

        if let Err(e) = self
            .audit_service
            .log(AuditEventParams {
                actor_id: admin_user_id.to_string(),
                actor_username: admin_username.clone(),
                action,
                target_type,
                target_id,
                details,
                ip_address: ctx.ip_address.clone(),
                user_agent: ctx.user_agent.clone(),
            })
            .await
        {
            tracing::error!(
                error = %e,
                admin_user_id = %admin_user_id,
                admin_username = %admin_username,
                "AUDIT LOG FAILURE: failed to record admin action. Manual review required.",
            );
        }
    }
}
