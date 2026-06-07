use std::{net::IpAddr, time::Duration};

use synctv_common::ExecutionControl;

use crate::{service::user::UserService, Error, Result};

use super::{
    identity_policy::password_binding,
    session_types::{
        AuthFactorMethod, AuthenticatedLogin, OpaqueLoginSession, OpaqueLoginStartChallenge,
        OPAQUE_LOGIN_SESSION_TTL_SECS,
    },
};

mod external;
mod oauth2;

impl UserService {
    pub async fn login_with_direct_password_transport_with_control(
        &self,
        identifier: String,
        password: String,
        client_ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<AuthenticatedLogin> {
        let normalized_identifier = Self::normalize_login_identifier(&identifier);
        self.brute_force
            .check_allowed_with_control(&normalized_identifier, client_ip, control)
            .await?;

        let maybe_user = self.get_by_login_identifier(&normalized_identifier).await?;
        let Some(user) = maybe_user else {
            let _ = self
                .opaque_password_service
                .verify_decoy_password(&password);
            self.record_login_failure_for_bruteforce(
                &normalized_identifier,
                false,
                client_ip,
                control,
                "direct password",
            )
            .await;
            return Err(Error::Authentication("Authentication failed".to_string()));
        };

        let Some(opaque_credential) = self
            .user_password_repository
            .get_opaque_credential(&user.id)
            .await?
        else {
            let _ = self
                .opaque_password_service
                .verify_decoy_password(&password);
            self.record_login_failure_for_bruteforce(
                &normalized_identifier,
                true,
                client_ip,
                control,
                "direct password",
            )
            .await;
            return Err(Error::Authentication("Authentication failed".to_string()));
        };

        let verified = self
            .opaque_password_service
            .verify_password(&opaque_credential.record, &password)?;
        if !verified {
            self.record_login_failure_for_bruteforce(
                &normalized_identifier,
                true,
                client_ip,
                control,
                "direct password",
            )
            .await;
            return Err(Error::Authentication("Authentication failed".to_string()));
        }

        let credential_state = self
            .user_password_repository
            .get_state(&user.id)
            .await
            .map_err(|error| match error {
                Error::NotFound(_) => Error::Authentication("Authentication failed".to_string()),
                other => other,
            })?;
        let credential_binding = password_binding(credential_state.version);
        self.complete_authenticated_login_with_control(
            user,
            AuthFactorMethod::Password,
            credential_binding,
            &normalized_identifier,
            client_ip,
            control,
        )
        .await
    }

    pub async fn start_opaque_login_with_control(
        &self,
        identifier: String,
        credential_request: Vec<u8>,
        client_ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<OpaqueLoginStartChallenge> {
        let normalized_identifier = Self::normalize_login_identifier(&identifier);
        self.brute_force
            .check_allowed_with_control(&normalized_identifier, client_ip, control)
            .await?;

        let maybe_user = self.get_by_login_identifier(&normalized_identifier).await?;
        let user_existed = maybe_user.is_some();

        let (user_id, opaque_record) = if let Some(user) = maybe_user {
            let opaque = self
                .user_password_repository
                .get_opaque_credential(&user.id)
                .await?
                .map(|credential| credential.record);
            (Some(user.id), opaque)
        } else {
            (None, None)
        };

        let fallback_identifier =
            format!("synctv:opaque-login:{normalized_identifier}").into_bytes();
        let credential_identifier = opaque_record
            .as_ref()
            .map_or(fallback_identifier.as_slice(), |record| {
                record.credential_identifier.as_slice()
            });
        let login_start = self.opaque_password_service.start_login(
            opaque_record.as_ref(),
            credential_identifier,
            &credential_request,
        )?;
        let session_id = synctv_common::snanoid!(48);
        self.opaque_login_session_store
            .store(
                &session_id,
                &OpaqueLoginSession {
                    user_id,
                    brute_force_key: normalized_identifier,
                    user_existed,
                    server_login_state: login_start.server_login_state,
                },
                Duration::from_secs(OPAQUE_LOGIN_SESSION_TTL_SECS),
            )
            .await?;

        Ok(OpaqueLoginStartChallenge {
            session_id,
            credential_response: login_start.credential_response,
        })
    }

    pub(in crate::service::user) async fn record_login_failure_for_bruteforce(
        &self,
        brute_force_key: &str,
        user_existed: bool,
        client_ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
        login_flow: &'static str,
    ) {
        let record_result = if user_existed {
            self.brute_force
                .record_failure_with_control(brute_force_key, client_ip, control)
                .await
        } else {
            self.brute_force
                .record_ip_failure_with_control(client_ip, control)
                .await
        };
        if let Err(error) = record_result {
            tracing::warn!(error = %error, login_flow, "Failed to record login failure for brute-force tracking");
        }
    }

    pub async fn finish_opaque_login_with_control(
        &self,
        session_id: &str,
        credential_finalization: Vec<u8>,
        client_ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<AuthenticatedLogin> {
        let Some(session) = self.opaque_login_session_store.consume(session_id).await? else {
            return Err(Error::Authentication("Authentication failed".to_string()));
        };

        let finish_result = self
            .opaque_password_service
            .finish_login(&session.server_login_state, &credential_finalization);

        let (Ok(_session_key), Some(user_id)) = (finish_result, session.user_id) else {
            self.record_login_failure_for_bruteforce(
                &session.brute_force_key,
                session.user_existed,
                client_ip,
                control,
                "opaque",
            )
            .await;
            return Err(Error::Authentication("Authentication failed".to_string()));
        };

        let user = self
            .repository
            .get_by_id(&user_id)
            .await?
            .ok_or_else(|| Error::Authentication("Authentication failed".to_string()))?;

        let credential_binding = password_binding(
            self.user_password_repository
                .get_state(&user.id)
                .await?
                .version,
        );
        self.complete_authenticated_login_with_control(
            user,
            AuthFactorMethod::Password,
            credential_binding,
            &session.brute_force_key,
            client_ip,
            control,
        )
        .await
    }
}
