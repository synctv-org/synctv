use std::net::IpAddr;

use synctv_common::ExecutionControl;

use crate::{
    models::{User, UserId},
    service::{
        user::{
            session_types::{AuthFactorMethod, AuthenticatedLogin},
            UserService,
        },
        TokenCredentialBinding,
    },
    Error, Result,
};

impl UserService {
    pub async fn start_verified_external_login_with_control(
        &self,
        identifier: &str,
        client_ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<Option<User>> {
        let normalized_identifier = Self::normalize_login_identifier(identifier);
        self.brute_force
            .check_allowed_with_control(&normalized_identifier, client_ip, control)
            .await?;
        self.get_by_login_identifier(&normalized_identifier).await
    }

    pub(crate) async fn check_passkey_discoverable_login_allowed_with_control(
        &self,
        client_ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<()> {
        self.brute_force
            .check_ip_allowed_with_control(client_ip, control)
            .await
    }

    pub(crate) async fn check_external_login_allowed_with_control(
        &self,
        brute_force_key: &str,
        client_ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<()> {
        self.brute_force
            .check_allowed_with_control(brute_force_key, client_ip, control)
            .await
    }

    pub(crate) async fn record_passkey_discoverable_login_failure_with_control(
        &self,
        client_ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<()> {
        self.brute_force
            .record_ip_failure_with_control(client_ip, control)
            .await
    }

    pub fn normalize_external_login_identifier(identifier: &str) -> String {
        Self::normalize_login_identifier(identifier)
    }

    pub async fn record_external_login_failure_with_control(
        &self,
        brute_force_key: &str,
        user_existed: bool,
        client_ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<()> {
        self.record_login_failure_for_bruteforce(
            brute_force_key,
            user_existed,
            client_ip,
            control,
            "external",
        )
        .await
    }

    pub async fn login_with_verified_external_credential_with_control(
        &self,
        user_id: &UserId,
        credential_id: &[u8],
        brute_force_key: &str,
        client_ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<AuthenticatedLogin> {
        let user = self
            .repository
            .get_by_id(user_id)
            .await?
            .ok_or_else(|| Error::Authentication("Authentication failed".to_string()))?;

        self.complete_authenticated_login_with_control(
            user,
            AuthFactorMethod::WebAuthn,
            TokenCredentialBinding::WebAuthn {
                credential_id: credential_id.to_vec(),
            },
            brute_force_key,
            client_ip,
            control,
        )
        .await
    }
}
