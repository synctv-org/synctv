use sqlx::{Postgres, Transaction};

use crate::{
    models::{oauth2_client::OAuth2Provider, ReviewStatus, SignupMethod, UserId},
    service::user::{
        registration_types::{PendingRegistrationRequest, PendingRegistrationRequestRow},
        UserService,
    },
    Error, InternalExt, Result,
};

impl UserService {
    pub(in crate::service::user) async fn load_pending_registration_request_for_update(
        request_id: &UserId,
        tx: &mut Transaction<'_, Postgres>,
    ) -> Result<Option<PendingRegistrationRequest>> {
        let row = sqlx::query_as!(
            PendingRegistrationRequestRow,
            r#"SELECT username,
                   email,
                   opaque_record,
                   opaque_credential_identifier,
                   opaque_ciphersuite,
                   opaque_server_setup_version,
                   signup_method AS "signup_method: SignupMethod",
                   oauth2_provider_type AS "oauth2_provider: crate::models::OAuth2ProviderTypeName",
                   oauth2_provider_instance_name,
                   oauth2_provider_issuer,
                   oauth2_provider_user_id,
                   oauth2_provider_username,
                   oauth2_avatar_url,
                   oauth2_email_trusted,
                   webauthn_credential_id,
                   webauthn_passkey,
                   webauthn_credential_name
            FROM user_registration_requests
            WHERE id = $1 AND reviewed_at IS NULL AND status = $2
            FOR UPDATE
            "#,
            request_id.as_i64(),
            i16::from(ReviewStatus::Pending)
        )
        .fetch_optional(&mut **tx)
        .await?;

        row.map(|row| {
            let oauth2_provider = row
                .oauth2_provider
                .map(|provider| {
                    OAuth2Provider::from_str_name(&provider.0).ok_or_else(|| {
                        Error::InvalidInput(format!(
                            "Unsupported OAuth2 provider in registration request: {}",
                            provider.0
                        ))
                    })
                })
                .transpose()?;
            let webauthn_passkey = row
                .webauthn_passkey
                .map(serde_json::from_value)
                .transpose()
                .internal_with_err("Failed to deserialize pending WebAuthn passkey")?;
            let oauth2_email_trusted = match row.signup_method {
                SignupMethod::OAuth2 => row.oauth2_email_trusted.ok_or_else(|| {
                    Error::Internal(
                        "OAuth2 registration request is missing email trust state".to_string(),
                    )
                })?,
                _ => false,
            };
            Ok(PendingRegistrationRequest {
                username: row.username,
                email: row.email,
                opaque_record: row.opaque_record,
                opaque_credential_identifier: row.opaque_credential_identifier,
                opaque_ciphersuite: row.opaque_ciphersuite,
                opaque_server_setup_version: row.opaque_server_setup_version,
                oauth2_provider,
                oauth2_provider_instance_name: row.oauth2_provider_instance_name,
                oauth2_provider_issuer: row.oauth2_provider_issuer,
                oauth2_provider_user_id: row.oauth2_provider_user_id,
                oauth2_provider_username: row.oauth2_provider_username,
                oauth2_avatar_url: row.oauth2_avatar_url,
                oauth2_email_trusted,
                webauthn_credential_id: row.webauthn_credential_id,
                webauthn_passkey,
                webauthn_credential_name: row.webauthn_credential_name,
                signup_method: row.signup_method,
            })
        })
        .transpose()
    }
}
