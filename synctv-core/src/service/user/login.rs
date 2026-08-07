use std::{net::IpAddr, time::Duration};

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use synctv_common::ExecutionControl;

use crate::{service::UserService, Error, Result};

use super::{
    identity_policy::password_binding,
    session_types::{
        AuthFactorMethod, AuthenticatedLogin, LoginSession, LoginSessionState, LoginStartChallenge,
        OpaqueLoginStartChallenge, LOGIN_SESSION_TTL_SECS, LOGIN_SESSION_TTL_SECS_I64,
    },
};

mod external;
mod oauth2;

impl UserService {
    pub async fn start_login_with_control(
        &self,
        identifier: String,
        email_login_enabled: bool,
        passkey_login_enabled: bool,
        client_ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<LoginStartChallenge> {
        let discovery_started = std::time::Instant::now();
        let brute_force_key = Self::normalize_login_identifier(&identifier);
        self.brute_force
            .check_allowed_with_control(&brute_force_key, client_ip, control)
            .await?;

        let user = self.get_by_login_identifier(&brute_force_key).await?;
        let user_existed = user.is_some();
        let (user_id, email, available_methods) = if let Some(user) = user {
            let (auth_factors, email) = tokio::try_join!(
                self.user_preferences_repository.auth_factors(&user.id),
                self.user_email_repository.get_email(&user.id),
            )?;
            let mut available_methods = Vec::with_capacity(3);
            if passkey_login_enabled && auth_factors.webauthn {
                available_methods.push(AuthFactorMethod::WebAuthn);
            }
            if auth_factors.password {
                available_methods.push(AuthFactorMethod::Password);
            }
            if email_login_enabled && auth_factors.email {
                available_methods.push(AuthFactorMethod::Email);
            }
            (Some(user.id), email, available_methods)
        } else {
            let available_methods = self.decoy_login_methods(
                &brute_force_key,
                email_login_enabled,
                passkey_login_enabled,
            );
            (None, None, available_methods)
        };

        let minimum_duration = Duration::from_millis(rand::random_range(100..=200));
        tokio::time::sleep(minimum_duration.saturating_sub(discovery_started.elapsed())).await;

        let session_id = synctv_common::snanoid!(48);
        self.login_session_store
            .store(
                &session_id,
                &LoginSession {
                    user_id,
                    brute_force_key,
                    user_existed,
                    email,
                    state: LoginSessionState::Identified {
                        available_methods: available_methods.clone(),
                    },
                },
                Duration::from_secs(LOGIN_SESSION_TTL_SECS),
            )
            .await?;

        Ok(LoginStartChallenge {
            session_id,
            available_methods,
            expires_at: crate::SystemClock.now().timestamp() + LOGIN_SESSION_TTL_SECS_I64,
        })
    }

    fn decoy_login_methods(
        &self,
        brute_force_key: &str,
        email_login_enabled: bool,
        passkey_login_enabled: bool,
    ) -> Vec<AuthFactorMethod> {
        Self::decoy_login_methods_for_key(
            &self.login_discovery_key,
            brute_force_key,
            email_login_enabled,
            passkey_login_enabled,
        )
    }

    fn decoy_login_methods_for_key(
        login_discovery_key: &[u8; 32],
        brute_force_key: &str,
        email_login_enabled: bool,
        passkey_login_enabled: bool,
    ) -> Vec<AuthFactorMethod> {
        let mut candidates = Vec::with_capacity(3);
        if passkey_login_enabled {
            candidates.push(AuthFactorMethod::WebAuthn);
        }
        candidates.push(AuthFactorMethod::Password);
        if email_login_enabled {
            candidates.push(AuthFactorMethod::Email);
        }

        let mut mac = Hmac::<Sha256>::new_from_slice(login_discovery_key)
            .expect("login discovery key has a fixed valid length");
        mac.update(b"synctv:decoy-login-methods:v1\0");
        mac.update(brute_force_key.as_bytes());
        let digest = mac.finalize().into_bytes();

        let mut methods = candidates
            .iter()
            .enumerate()
            .filter_map(|(index, method)| (digest[index] & 1 == 1).then_some(*method))
            .collect::<Vec<_>>();
        if methods.is_empty() {
            methods.push(candidates[usize::from(digest[candidates.len()]) % candidates.len()]);
        }
        methods
    }

    pub fn email_login_rate_limit_key(&self, session: &LoginSession) -> String {
        Self::email_login_rate_limit_key_for_key(
            &self.login_discovery_key,
            session.brute_force_key(),
        )
    }

    fn email_login_rate_limit_key_for_key(
        login_discovery_key: &[u8; 32],
        normalized_identifier: &str,
    ) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(login_discovery_key)
            .expect("login discovery key has a fixed valid length");
        mac.update(b"synctv:email-login-rate-limit:v1\0");
        mac.update(normalized_identifier.as_bytes());
        format!("email:login:{}", hex::encode(mac.finalize().into_bytes()))
    }

    pub async fn get_login_session_for_method(
        &self,
        session_id: &str,
        method: AuthFactorMethod,
    ) -> Result<LoginSession> {
        let Some(session) = self.login_session_store.get(session_id).await? else {
            return Err(Error::Authentication("Authentication failed".to_string()));
        };
        if !session.supports(method) {
            return Err(Error::Authentication("Authentication failed".to_string()));
        }
        Ok(session)
    }

    pub async fn consume_login_session_for_method(
        &self,
        session_id: &str,
        method: AuthFactorMethod,
    ) -> Result<LoginSession> {
        let Some(session) = self.login_session_store.consume(session_id).await? else {
            return Err(Error::Authentication("Authentication failed".to_string()));
        };
        if !session.supports(method) {
            return Err(Error::Authentication("Authentication failed".to_string()));
        }
        Ok(session)
    }

    pub async fn login_with_direct_password_transport_with_control(
        &self,
        login_session_id: &str,
        password: String,
        client_ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<AuthenticatedLogin> {
        let session = self
            .consume_login_session_for_method(login_session_id, AuthFactorMethod::Password)
            .await?;
        let normalized_identifier = session.brute_force_key;
        self.brute_force
            .check_allowed_with_control(&normalized_identifier, client_ip, control)
            .await?;

        let Some(user_id) = session.user_id else {
            self.record_direct_password_failure(
                &normalized_identifier,
                false,
                client_ip,
                control,
                Some(&password),
            )
            .await?;
            return Err(Error::Authentication("Authentication failed".to_string()));
        };
        let Some(user) = self.repository.get_by_id(&user_id).await? else {
            self.record_direct_password_failure(
                &normalized_identifier,
                true,
                client_ip,
                control,
                Some(&password),
            )
            .await?;
            return Err(Error::Authentication("Authentication failed".to_string()));
        };

        let Some(opaque_credential) = self
            .user_password_repository
            .get_opaque_credential(&user.id)
            .await?
        else {
            self.record_direct_password_failure(
                &normalized_identifier,
                true,
                client_ip,
                control,
                Some(&password),
            )
            .await?;
            return Err(Error::Authentication("Authentication failed".to_string()));
        };

        let verified = self
            .opaque_password_service
            .verify_password(&opaque_credential.record, &password)?;
        if !verified {
            self.record_direct_password_failure(
                &normalized_identifier,
                true,
                client_ip,
                control,
                None,
            )
            .await?;
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

    async fn record_direct_password_failure(
        &self,
        brute_force_key: &str,
        user_existed: bool,
        client_ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
        decoy_password: Option<&str>,
    ) -> Result<()> {
        let decoy_result = decoy_password
            .map(|password| self.opaque_password_service.verify_decoy_password(password))
            .transpose()
            .map(|_| ());

        self.record_login_failure_for_bruteforce(
            brute_force_key,
            user_existed,
            client_ip,
            control,
            "direct password",
        )
        .await?;

        if let Err(error) = decoy_result {
            tracing::error!(
                error = %error,
                "Decoy password verification failed during direct login"
            );
            return Err(error);
        }

        Ok(())
    }

    pub async fn start_opaque_login_with_control(
        &self,
        login_session_id: &str,
        credential_request: bytes::Bytes,
        client_ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<OpaqueLoginStartChallenge> {
        let mut session = self
            .get_login_session_for_method(login_session_id, AuthFactorMethod::Password)
            .await?;
        self.brute_force
            .check_allowed_with_control(&session.brute_force_key, client_ip, control)
            .await?;

        let opaque_record = if let Some(user_id) = session.user_id {
            self.user_password_repository
                .get_opaque_credential(&user_id)
                .await?
                .map(|credential| credential.record)
        } else {
            None
        };

        let fallback_identifier =
            format!("synctv:opaque-login:{}", session.brute_force_key).into_bytes();
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
        session.state = LoginSessionState::OpaqueChallenge {
            server_login_state: login_start.server_login_state,
        };
        let challenge_session_id = synctv_common::snanoid!(48);
        self.login_session_store
            .store(
                &challenge_session_id,
                &session,
                Duration::from_secs(LOGIN_SESSION_TTL_SECS),
            )
            .await?;

        Ok(OpaqueLoginStartChallenge {
            session_id: challenge_session_id,
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
    ) -> Result<()> {
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
            return Err(error);
        }
        Ok(())
    }

    pub async fn finish_opaque_login_with_control(
        &self,
        session_id: &str,
        credential_finalization: bytes::Bytes,
        client_ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<AuthenticatedLogin> {
        let Some(session) = self.login_session_store.consume(session_id).await? else {
            return Err(Error::Authentication("Authentication failed".to_string()));
        };
        let LoginSessionState::OpaqueChallenge { server_login_state } = session.state else {
            return Err(Error::Authentication("Authentication failed".to_string()));
        };

        let finish_result = self
            .opaque_password_service
            .finish_login(&server_login_state, &credential_finalization);

        let (Ok(_session_key), Some(user_id)) = (finish_result, session.user_id) else {
            self.record_login_failure_for_bruteforce(
                &session.brute_force_key,
                session.user_existed,
                client_ip,
                control,
                "opaque",
            )
            .await?;
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{AuthFactorMethod, UserService};
    use crate::service::UserServiceRuntimeOptions;

    fn method_mask(methods: &[AuthFactorMethod]) -> u8 {
        methods.iter().fold(0, |mask, method| {
            mask | match method {
                AuthFactorMethod::Password => 1,
                AuthFactorMethod::WebAuthn => 2,
                AuthFactorMethod::Email => 4,
                AuthFactorMethod::Totp | AuthFactorMethod::RecoveryCode => 0,
            }
        })
    }

    #[test]
    fn decoy_login_profiles_are_stable_and_varied() {
        let key = UserServiceRuntimeOptions::derive_login_discovery_key(b"profile-secret");
        let mut profiles = BTreeSet::new();

        for index in 0..64 {
            let identifier = format!("missing-{index}@example.com");
            let first = UserService::decoy_login_methods_for_key(&key, &identifier, true, true);
            let second = UserService::decoy_login_methods_for_key(&key, &identifier, true, true);
            assert_eq!(first, second);
            assert!(!first.is_empty());
            profiles.insert(method_mask(&first));
        }

        assert!(profiles.len() >= 4);
        assert!(profiles.iter().any(|mask| mask & 2 == 0));
        assert!(profiles.iter().any(|mask| mask & 2 != 0));
        assert!(profiles.iter().any(|mask| mask & 4 == 0));
        assert!(profiles.iter().any(|mask| mask & 4 != 0));
    }

    #[test]
    fn decoy_email_profiles_do_not_depend_on_identifier_syntax() {
        let key = UserServiceRuntimeOptions::derive_login_discovery_key(b"profile-secret");
        let username_profiles = (0..64)
            .map(|index| {
                UserService::decoy_login_methods_for_key(
                    &key,
                    &format!("missing-user-{index}"),
                    true,
                    true,
                )
            })
            .collect::<Vec<_>>();

        assert!(username_profiles
            .iter()
            .any(|methods| methods.contains(&AuthFactorMethod::Email)));
        assert!(username_profiles
            .iter()
            .any(|methods| !methods.contains(&AuthFactorMethod::Email)));
    }

    #[test]
    fn decoy_login_profiles_follow_server_capabilities_and_secret() {
        let first_key =
            UserServiceRuntimeOptions::derive_login_discovery_key(b"first-profile-secret");
        let second_key =
            UserServiceRuntimeOptions::derive_login_discovery_key(b"second-profile-secret");
        assert_eq!(
            UserService::decoy_login_methods_for_key(&first_key, "missing", false, false),
            vec![AuthFactorMethod::Password]
        );

        let differs_for_some_identifier = (0..64).any(|index| {
            let identifier = format!("missing-{index}@example.com");
            UserService::decoy_login_methods_for_key(&first_key, &identifier, true, true)
                != UserService::decoy_login_methods_for_key(&second_key, &identifier, true, true)
        });
        assert!(differs_for_some_identifier);
    }

    #[test]
    fn email_login_rate_limit_keys_are_private_stable_and_scoped() {
        let first_key =
            UserServiceRuntimeOptions::derive_login_discovery_key(b"first-profile-secret");
        let second_key =
            UserServiceRuntimeOptions::derive_login_discovery_key(b"second-profile-secret");
        let first = UserService::email_login_rate_limit_key_for_key(&first_key, "user@example.com");

        assert_eq!(
            first,
            UserService::email_login_rate_limit_key_for_key(&first_key, "user@example.com",)
        );
        assert_ne!(
            first,
            UserService::email_login_rate_limit_key_for_key(&first_key, "other@example.com",)
        );
        assert_ne!(
            first,
            UserService::email_login_rate_limit_key_for_key(&second_key, "user@example.com",)
        );
        assert!(!first.contains("user@example.com"));
    }
}
