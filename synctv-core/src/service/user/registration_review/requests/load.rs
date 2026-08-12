use sqlx::{Postgres, Transaction};

use crate::{
    models::{OpaquePasswordRecord, ReviewStatus, SignupMethod, UserId},
    service::UserService,
    Error, Result,
};

use super::super::super::registration_types::{
    PendingRegistrationCredential, PendingRegistrationPasskey, PendingRegistrationRequest,
    PendingRegistrationRequestRow,
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
                   oauth2_provider_type AS "oauth2_provider: crate::models::OAuth2Provider",
                   oauth2_provider_instance_name,
                   oauth2_provider_issuer,
                   oauth2_provider_user_id,
                   oauth2_provider_username,
                   oauth2_avatar_url,
                   webauthn_credential_id,
                   webauthn_passkey AS "webauthn_passkey: PendingRegistrationPasskey",
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
            let password_present = row.opaque_record.is_some()
                || row.opaque_credential_identifier.is_some()
                || row.opaque_ciphersuite.is_some()
                || row.opaque_server_setup_version.is_some();
            let oauth2_present = row.oauth2_provider.is_some()
                || row.oauth2_provider_instance_name.is_some()
                || row.oauth2_provider_issuer.is_some()
                || row.oauth2_provider_user_id.is_some()
                || row.oauth2_provider_username.is_some()
                || row.oauth2_avatar_url.is_some();
            let webauthn_present = row.webauthn_credential_id.is_some()
                || row.webauthn_passkey.is_some()
                || row.webauthn_credential_name.is_some();

            if [password_present, oauth2_present, webauthn_present]
                .into_iter()
                .filter(|present| *present)
                .count()
                != 1
            {
                return Err(Error::InvalidInput(
                    "Registration request must contain exactly one credential type".to_string(),
                ));
            }

            let username = row.username;
            let credential = match row.signup_method {
                signup_method @ (SignupMethod::Email | SignupMethod::Password)
                    if password_present =>
                {
                    PendingRegistrationCredential::Password {
                        signup_method,
                        opaque_record: OpaquePasswordRecord {
                            record: row.opaque_record.ok_or_else(|| {
                                Error::InvalidInput(
                                    "Registration request is missing OPAQUE record".to_string(),
                                )
                            })?,
                            credential_identifier: row.opaque_credential_identifier.ok_or_else(
                                || {
                                    Error::InvalidInput(
                                        "Registration request is missing OPAQUE credential identifier"
                                            .to_string(),
                                    )
                                },
                            )?,
                            ciphersuite: row.opaque_ciphersuite.ok_or_else(|| {
                                Error::InvalidInput(
                                    "Registration request is missing OPAQUE ciphersuite".to_string(),
                                )
                            })?,
                            server_setup_version: row.opaque_server_setup_version.ok_or_else(
                                || {
                                    Error::InvalidInput(
                                        "Registration request is missing OPAQUE setup version"
                                            .to_string(),
                                    )
                                },
                            )?,
                        },
                    }
                }
                SignupMethod::OAuth2 if oauth2_present => {
                    PendingRegistrationCredential::OAuth2(
                        crate::models::oauth2_client::OAuth2UserInfo {
                            provider: row.oauth2_provider.ok_or_else(|| {
                                Error::InvalidInput(
                                    "OAuth2 registration request is missing provider".to_string(),
                                )
                            })?,
                            provider_instance_name: row.oauth2_provider_instance_name.ok_or_else(
                                || {
                                    Error::InvalidInput(
                                        "OAuth2 registration request is missing provider instance name"
                                            .to_string(),
                                    )
                                },
                            )?,
                            provider_issuer: row.oauth2_provider_issuer,
                            provider_user_id: row.oauth2_provider_user_id.ok_or_else(|| {
                                Error::InvalidInput(
                                    "OAuth2 registration request is missing provider user ID"
                                        .to_string(),
                                )
                            })?,
                            username: row
                                .oauth2_provider_username
                                .unwrap_or_else(|| username.clone()),
                            avatar: row.oauth2_avatar_url,
                        },
                    )
                }
                SignupMethod::WebAuthn if webauthn_present => {
                    let credential_id = row.webauthn_credential_id.ok_or_else(|| {
                        Error::InvalidInput(
                            "Registration request is missing WebAuthn credential ID".to_string(),
                        )
                    })?;
                    let passkey = row
                        .webauthn_passkey
                        .map(PendingRegistrationPasskey::into_inner)
                        .ok_or_else(|| {
                            Error::InvalidInput(
                                "Registration request is missing WebAuthn passkey".to_string(),
                            )
                        })?;
                    if credential_id != AsRef::<[u8]>::as_ref(passkey.cred_id()) {
                        return Err(Error::InvalidInput(
                            "Registration request WebAuthn credential ID does not match passkey"
                                .to_string(),
                        ));
                    }
                    PendingRegistrationCredential::WebAuthn {
                        passkey,
                        credential_name: row.webauthn_credential_name,
                    }
                }
                _ => {
                    return Err(Error::InvalidInput(
                        "Registration request signup method does not match its credential"
                            .to_string(),
                    ));
                }
            };

            Ok(PendingRegistrationRequest {
                username,
                email: row.email,
                credential,
            })
        })
        .transpose()
    }
}
