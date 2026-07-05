use synctv_core::models::UserId;

use super::ClientApiImpl;
use crate::impls::ApiError;
use synctv_proto::client::{
    DeletePasskeyRequest, DeletePasskeyResponse, ListPasskeysResponse,
    PasskeyAttestationConveyancePreference, PasskeyAttestationFormat,
    PasskeyAuthenticationCredential, PasskeyAuthenticationExtensionsClientOutputs,
    PasskeyAuthenticationExtensionsInput, PasskeyAuthenticatorAttachment,
    PasskeyAuthenticatorSelectionCriteria, PasskeyAuthenticatorTransport, PasskeyCreationChallenge,
    PasskeyCredProtectInput, PasskeyCredential, PasskeyCredentialDescriptor,
    PasskeyCredentialProtectionPolicy, PasskeyHmacGetSecretInput, PasskeyMediationRequirement,
    PasskeyPubKeyCredentialParam, PasskeyPublicKeyCredentialCreationOptions,
    PasskeyPublicKeyCredentialHint, PasskeyPublicKeyCredentialRequestOptions,
    PasskeyPublicKeyCredentialType, PasskeyRegistrationCredential,
    PasskeyRegistrationExtensionsClientOutputs, PasskeyRegistrationExtensionsInput,
    PasskeyRelyingParty, PasskeyRequestChallenge, PasskeyResidentKeyRequirement, PasskeyUserEntity,
    PasskeyUserVerificationRequirement, StartPasskeyBindRequest, StartPasskeyBindResponse,
};
use webauthn_rs::prelude::{
    CreationChallengeResponse, PublicKeyCredential, RegisterPublicKeyCredential,
    RequestChallengeResponse,
};
use webauthn_rs_proto as webauthn_proto;

pub(crate) struct PasskeyBindChallenge {
    pub session_id: String,
    pub options: CreationChallengeResponse,
}

fn passkey_unavailable_error() -> ApiError {
    ApiError::ServiceUnavailable("Passkey/WebAuthn service is not configured".to_string())
}

pub(crate) fn passkey_creation_options_to_proto(
    options: &CreationChallengeResponse,
) -> Result<PasskeyCreationChallenge, ApiError> {
    Ok(PasskeyCreationChallenge {
        public_key: Some(PasskeyPublicKeyCredentialCreationOptions {
            rp: Some(PasskeyRelyingParty {
                name: options.public_key.rp.name.clone(),
                id: options.public_key.rp.id.clone(),
            }),
            user: Some(PasskeyUserEntity {
                id: options.public_key.user.id.clone(),
                name: options.public_key.user.name.clone(),
                display_name: options.public_key.user.display_name.clone(),
            }),
            challenge: options.public_key.challenge.clone(),
            pub_key_cred_params: options
                .public_key
                .pub_key_cred_params
                .iter()
                .map(passkey_pub_key_cred_param_to_proto)
                .collect(),
            timeout: options.public_key.timeout,
            exclude_credentials: options
                .public_key
                .exclude_credentials
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(passkey_credential_descriptor_to_proto)
                .collect(),
            authenticator_selection: options
                .public_key
                .authenticator_selection
                .as_ref()
                .map(passkey_authenticator_selection_to_proto),
            hints: options
                .public_key
                .hints
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(passkey_hint_to_proto)
                .collect(),
            attestation: options.public_key.attestation.as_ref().map_or(
                PasskeyAttestationConveyancePreference::Unspecified as i32,
                passkey_attestation_to_proto,
            ),
            attestation_formats: options
                .public_key
                .attestation_formats
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(passkey_attestation_format_to_proto)
                .collect(),
            extensions: options
                .public_key
                .extensions
                .as_ref()
                .map(passkey_registration_extensions_input_to_proto),
        }),
    })
}

pub(crate) fn passkey_request_options_to_proto(
    options: &RequestChallengeResponse,
) -> Result<PasskeyRequestChallenge, ApiError> {
    Ok(PasskeyRequestChallenge {
        public_key: Some(PasskeyPublicKeyCredentialRequestOptions {
            challenge: options.public_key.challenge.clone(),
            timeout: options.public_key.timeout,
            rp_id: options.public_key.rp_id.clone(),
            allow_credentials: options
                .public_key
                .allow_credentials
                .iter()
                .map(passkey_allow_credential_to_proto)
                .collect(),
            user_verification: passkey_user_verification_to_proto(
                options.public_key.user_verification,
            ),
            hints: options
                .public_key
                .hints
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(passkey_hint_to_proto)
                .collect(),
            extensions: options
                .public_key
                .extensions
                .as_ref()
                .map(passkey_authentication_extensions_input_to_proto),
        }),
        mediation: options
            .mediation
            .as_ref()
            .map_or(PasskeyMediationRequirement::Unspecified as i32, |value| {
                passkey_mediation_to_proto(value)
            }),
    })
}

pub(crate) fn passkey_registration_credential_from_proto(
    credential: &PasskeyRegistrationCredential,
) -> Result<RegisterPublicKeyCredential, ApiError> {
    Ok(RegisterPublicKeyCredential {
        id: credential.id.clone(),
        raw_id: credential.raw_id.clone(),
        response: webauthn_proto::AuthenticatorAttestationResponseRaw {
            attestation_object: credential
                .response
                .as_ref()
                .ok_or_else(|| ApiError::Authentication("Authentication failed".to_string()))?
                .attestation_object
                .clone(),
            client_data_json: credential
                .response
                .as_ref()
                .ok_or_else(|| ApiError::Authentication("Authentication failed".to_string()))?
                .client_data_json
                .clone(),
            transports: Some(
                credential
                    .response
                    .as_ref()
                    .ok_or_else(|| ApiError::Authentication("Authentication failed".to_string()))?
                    .transports
                    .iter()
                    .copied()
                    .filter_map(passkey_transport_from_proto)
                    .collect(),
            ),
        },
        type_: passkey_type_string(credential.r#type)?.to_string(),
        extensions: passkey_registration_extensions_client_outputs_from_proto(
            credential.extensions.as_ref(),
        ),
    })
}

pub(crate) fn passkey_authentication_credential_from_proto(
    credential: &PasskeyAuthenticationCredential,
) -> Result<PublicKeyCredential, ApiError> {
    let response = credential
        .response
        .as_ref()
        .ok_or_else(|| ApiError::Authentication("Authentication failed".to_string()))?;
    Ok(PublicKeyCredential {
        id: credential.id.clone(),
        raw_id: credential.raw_id.clone(),
        response: webauthn_proto::AuthenticatorAssertionResponseRaw {
            authenticator_data: response.authenticator_data.clone(),
            client_data_json: response.client_data_json.clone(),
            signature: response.signature.clone(),
            user_handle: (!response.user_handle.is_empty()).then(|| response.user_handle.clone()),
        },
        extensions: passkey_authentication_extensions_client_outputs_from_proto(
            credential.extensions.as_ref(),
        ),
        type_: passkey_type_string(credential.r#type)?.to_string(),
    })
}

fn passkey_type_string(value: i32) -> Result<&'static str, ApiError> {
    if value == PasskeyPublicKeyCredentialType::PublicKey as i32 {
        Ok("public-key")
    } else {
        Err(ApiError::Authentication(
            "Authentication failed".to_string(),
        ))
    }
}

fn passkey_pub_key_cred_param_to_proto(
    value: &webauthn_proto::PubKeyCredParams,
) -> PasskeyPubKeyCredentialParam {
    PasskeyPubKeyCredentialParam {
        r#type: PasskeyPublicKeyCredentialType::PublicKey as i32,
        alg: value.alg,
    }
}

fn passkey_credential_descriptor_to_proto(
    value: &webauthn_proto::PublicKeyCredentialDescriptor,
) -> PasskeyCredentialDescriptor {
    PasskeyCredentialDescriptor {
        r#type: PasskeyPublicKeyCredentialType::PublicKey as i32,
        id: value.id.clone(),
        transports: value
            .transports
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(passkey_transport_to_proto)
            .collect(),
    }
}

fn passkey_allow_credential_to_proto(
    value: &webauthn_proto::AllowCredentials,
) -> PasskeyCredentialDescriptor {
    PasskeyCredentialDescriptor {
        r#type: PasskeyPublicKeyCredentialType::PublicKey as i32,
        id: value.id.clone(),
        transports: value
            .transports
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(passkey_transport_to_proto)
            .collect(),
    }
}

fn passkey_transport_to_proto(value: &webauthn_proto::AuthenticatorTransport) -> i32 {
    (match value {
        webauthn_proto::AuthenticatorTransport::Usb => PasskeyAuthenticatorTransport::Usb,
        webauthn_proto::AuthenticatorTransport::Nfc => PasskeyAuthenticatorTransport::Nfc,
        webauthn_proto::AuthenticatorTransport::Ble => PasskeyAuthenticatorTransport::Ble,
        webauthn_proto::AuthenticatorTransport::Internal => PasskeyAuthenticatorTransport::Internal,
        webauthn_proto::AuthenticatorTransport::Hybrid => PasskeyAuthenticatorTransport::Hybrid,
        webauthn_proto::AuthenticatorTransport::Test => PasskeyAuthenticatorTransport::Test,
        webauthn_proto::AuthenticatorTransport::Unknown => PasskeyAuthenticatorTransport::Unknown,
    }) as i32
}

fn passkey_transport_from_proto(value: i32) -> Option<webauthn_proto::AuthenticatorTransport> {
    match PasskeyAuthenticatorTransport::try_from(value).ok()? {
        PasskeyAuthenticatorTransport::Usb => Some(webauthn_proto::AuthenticatorTransport::Usb),
        PasskeyAuthenticatorTransport::Nfc => Some(webauthn_proto::AuthenticatorTransport::Nfc),
        PasskeyAuthenticatorTransport::Ble => Some(webauthn_proto::AuthenticatorTransport::Ble),
        PasskeyAuthenticatorTransport::Internal => {
            Some(webauthn_proto::AuthenticatorTransport::Internal)
        }
        PasskeyAuthenticatorTransport::Hybrid => {
            Some(webauthn_proto::AuthenticatorTransport::Hybrid)
        }
        PasskeyAuthenticatorTransport::Test => Some(webauthn_proto::AuthenticatorTransport::Test),
        PasskeyAuthenticatorTransport::Unknown => {
            Some(webauthn_proto::AuthenticatorTransport::Unknown)
        }
        PasskeyAuthenticatorTransport::Unspecified => None,
    }
}

fn passkey_authenticator_selection_to_proto(
    value: &webauthn_proto::AuthenticatorSelectionCriteria,
) -> PasskeyAuthenticatorSelectionCriteria {
    PasskeyAuthenticatorSelectionCriteria {
        authenticator_attachment: value.authenticator_attachment.map_or(
            PasskeyAuthenticatorAttachment::Unspecified as i32,
            passkey_authenticator_attachment_to_proto,
        ),
        resident_key: value.resident_key.map_or(
            PasskeyResidentKeyRequirement::Unspecified as i32,
            passkey_resident_key_to_proto,
        ),
        require_resident_key: value.require_resident_key,
        user_verification: passkey_user_verification_to_proto(value.user_verification),
    }
}

fn passkey_authenticator_attachment_to_proto(
    value: webauthn_proto::AuthenticatorAttachment,
) -> i32 {
    (match value {
        webauthn_proto::AuthenticatorAttachment::Platform => {
            PasskeyAuthenticatorAttachment::Platform
        }
        webauthn_proto::AuthenticatorAttachment::CrossPlatform => {
            PasskeyAuthenticatorAttachment::CrossPlatform
        }
    }) as i32
}

fn passkey_resident_key_to_proto(value: webauthn_proto::ResidentKeyRequirement) -> i32 {
    (match value {
        webauthn_proto::ResidentKeyRequirement::Discouraged => {
            PasskeyResidentKeyRequirement::Discouraged
        }
        webauthn_proto::ResidentKeyRequirement::Preferred => {
            PasskeyResidentKeyRequirement::Preferred
        }
        webauthn_proto::ResidentKeyRequirement::Required => PasskeyResidentKeyRequirement::Required,
    }) as i32
}

fn passkey_user_verification_to_proto(value: webauthn_proto::UserVerificationPolicy) -> i32 {
    (match value {
        webauthn_proto::UserVerificationPolicy::Required => {
            PasskeyUserVerificationRequirement::Required
        }
        webauthn_proto::UserVerificationPolicy::Preferred => {
            PasskeyUserVerificationRequirement::Preferred
        }
        webauthn_proto::UserVerificationPolicy::Discouraged_DO_NOT_USE => {
            PasskeyUserVerificationRequirement::Discouraged
        }
    }) as i32
}

fn passkey_hint_to_proto(value: &webauthn_proto::PublicKeyCredentialHints) -> i32 {
    (match value {
        webauthn_proto::PublicKeyCredentialHints::SecurityKey => {
            PasskeyPublicKeyCredentialHint::SecurityKey
        }
        webauthn_proto::PublicKeyCredentialHints::ClientDevice => {
            PasskeyPublicKeyCredentialHint::ClientDevice
        }
        webauthn_proto::PublicKeyCredentialHints::Hybrid => PasskeyPublicKeyCredentialHint::Hybrid,
    }) as i32
}

fn passkey_attestation_to_proto(value: &webauthn_proto::AttestationConveyancePreference) -> i32 {
    (match value {
        webauthn_proto::AttestationConveyancePreference::None => {
            PasskeyAttestationConveyancePreference::None
        }
        webauthn_proto::AttestationConveyancePreference::Indirect => {
            PasskeyAttestationConveyancePreference::Indirect
        }
        webauthn_proto::AttestationConveyancePreference::Direct => {
            PasskeyAttestationConveyancePreference::Direct
        }
    }) as i32
}

fn passkey_attestation_format_to_proto(value: &webauthn_proto::AttestationFormat) -> i32 {
    (match value {
        webauthn_proto::AttestationFormat::Packed => PasskeyAttestationFormat::Packed,
        webauthn_proto::AttestationFormat::Tpm => PasskeyAttestationFormat::Tpm,
        webauthn_proto::AttestationFormat::AndroidKey => PasskeyAttestationFormat::AndroidKey,
        webauthn_proto::AttestationFormat::AndroidSafetyNet => {
            PasskeyAttestationFormat::AndroidSafetynet
        }
        webauthn_proto::AttestationFormat::FIDOU2F => PasskeyAttestationFormat::FidoU2f,
        webauthn_proto::AttestationFormat::AppleAnonymous => PasskeyAttestationFormat::Apple,
        webauthn_proto::AttestationFormat::None => PasskeyAttestationFormat::None,
    }) as i32
}

fn passkey_registration_extensions_input_to_proto(
    value: &webauthn_proto::RequestRegistrationExtensions,
) -> PasskeyRegistrationExtensionsInput {
    PasskeyRegistrationExtensionsInput {
        cred_protect: value
            .cred_protect
            .as_ref()
            .map(|cred_protect| PasskeyCredProtectInput {
                credential_protection_policy: passkey_credential_protection_to_proto(
                    cred_protect.credential_protection_policy,
                ),
                enforce_credential_protection_policy: cred_protect
                    .enforce_credential_protection_policy,
            }),
        uvm: value.uvm,
        cred_props: value.cred_props,
        min_pin_length: value.min_pin_length,
        hmac_create_secret: value.hmac_create_secret,
    }
}

fn passkey_authentication_extensions_input_to_proto(
    value: &webauthn_proto::RequestAuthenticationExtensions,
) -> PasskeyAuthenticationExtensionsInput {
    PasskeyAuthenticationExtensionsInput {
        appid: value.appid.clone().unwrap_or_default(),
        uvm: value.uvm,
        hmac_get_secret: value
            .hmac_get_secret
            .as_ref()
            .map(|input| PasskeyHmacGetSecretInput {
                output1: input.output1.clone(),
                output2: input.output2.clone().unwrap_or_default(),
            }),
    }
}

fn passkey_credential_protection_to_proto(
    value: webauthn_proto::CredentialProtectionPolicy,
) -> i32 {
    (match value {
        webauthn_proto::CredentialProtectionPolicy::UserVerificationOptional => {
            PasskeyCredentialProtectionPolicy::UserVerificationOptional
        }
        webauthn_proto::CredentialProtectionPolicy::UserVerificationOptionalWithCredentialIDList => {
            PasskeyCredentialProtectionPolicy::UserVerificationOptionalWithCredentialIdList
        }
        webauthn_proto::CredentialProtectionPolicy::UserVerificationRequired => {
            PasskeyCredentialProtectionPolicy::UserVerificationRequired
        }
    }) as i32
}

fn passkey_credential_protection_from_proto(
    value: i32,
) -> Option<webauthn_proto::CredentialProtectionPolicy> {
    match PasskeyCredentialProtectionPolicy::try_from(value).ok()? {
        PasskeyCredentialProtectionPolicy::UserVerificationOptional => {
            Some(webauthn_proto::CredentialProtectionPolicy::UserVerificationOptional)
        }
        PasskeyCredentialProtectionPolicy::UserVerificationOptionalWithCredentialIdList => {
            Some(
                webauthn_proto::CredentialProtectionPolicy::UserVerificationOptionalWithCredentialIDList,
            )
        }
        PasskeyCredentialProtectionPolicy::UserVerificationRequired => {
            Some(webauthn_proto::CredentialProtectionPolicy::UserVerificationRequired)
        }
        PasskeyCredentialProtectionPolicy::Unspecified => None,
    }
}

fn passkey_mediation_to_proto(value: &webauthn_proto::Mediation) -> i32 {
    match value {
        webauthn_proto::Mediation::Conditional => PasskeyMediationRequirement::Conditional as i32,
    }
}

fn passkey_registration_extensions_client_outputs_from_proto(
    value: Option<&PasskeyRegistrationExtensionsClientOutputs>,
) -> webauthn_proto::RegistrationExtensionsClientOutputs {
    let Some(value) = value else {
        return webauthn_proto::RegistrationExtensionsClientOutputs::default();
    };
    webauthn_proto::RegistrationExtensionsClientOutputs {
        appid: value.appid,
        cred_props: value
            .cred_props
            .as_ref()
            .map(|props| webauthn_proto::CredProps { rk: props.rk }),
        hmac_secret: value.hmac_secret,
        cred_protect: passkey_credential_protection_from_proto(value.cred_protect),
        min_pin_length: value.min_pin_length,
    }
}

fn passkey_authentication_extensions_client_outputs_from_proto(
    value: Option<&PasskeyAuthenticationExtensionsClientOutputs>,
) -> webauthn_proto::AuthenticationExtensionsClientOutputs {
    let Some(value) = value else {
        return webauthn_proto::AuthenticationExtensionsClientOutputs::default();
    };
    webauthn_proto::AuthenticationExtensionsClientOutputs {
        appid: value.appid,
        hmac_get_secret: value.hmac_get_secret.as_ref().map(|input| {
            webauthn_proto::HmacGetSecretOutput {
                output1: input.output1.clone(),
                output2: (!input.output2.is_empty()).then(|| input.output2.clone()),
            }
        }),
    }
}

pub(crate) fn passkey_credential_to_proto(
    credential: &synctv_core::repository::WebAuthnCredential,
) -> PasskeyCredential {
    PasskeyCredential {
        credential_id: synctv_core::service::PasskeyService::encode_credential_id(
            &credential.credential_id,
        ),
        name: credential.name.clone().unwrap_or_default(),
        sign_count: credential.sign_count,
        created_at: credential.created_at.timestamp(),
        updated_at: credential.updated_at.timestamp(),
        last_used_at: credential.last_used_at.map_or(0, |value| value.timestamp()),
    }
}

impl ClientApiImpl {
    pub(crate) fn passkey_service(
        &self,
    ) -> Result<&std::sync::Arc<synctv_core::service::PasskeyService>, ApiError> {
        self.passkey_service
            .as_ref()
            .ok_or_else(passkey_unavailable_error)
    }

    pub(crate) async fn start_passkey_bind_challenge(
        &self,
        user_id: &UserId,
        name: String,
    ) -> Result<PasskeyBindChallenge, ApiError> {
        let profile = self
            .user_service
            .get_user(user_id)
            .await
            .map_err(ApiError::from)?;
        let credential_name = if name.trim().is_empty() {
            None
        } else {
            Some(name.trim().to_string())
        };
        let challenge = self
            .passkey_service()?
            .start_registration(&profile, credential_name)
            .await
            .map_err(ApiError::from)?;

        Ok(PasskeyBindChallenge {
            session_id: challenge.session_id,
            options: challenge.options,
        })
    }

    pub async fn start_passkey_bind(
        &self,
        user_id: &UserId,
        req: StartPasskeyBindRequest,
    ) -> Result<StartPasskeyBindResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let challenge = self.start_passkey_bind_challenge(user_id, req.name).await?;
        let options = passkey_creation_options_to_proto(&challenge.options)?;
        Ok(StartPasskeyBindResponse {
            session_id: challenge.session_id,
            options: Some(options),
        })
    }

    pub async fn finish_passkey_bind_request(
        &self,
        user_id: &UserId,
        req: synctv_proto::client::FinishPasskeyBindRequest,
    ) -> Result<PasskeyCredential, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let credential = req
            .credential
            .as_ref()
            .ok_or_else(|| ApiError::InvalidInput("credential is required".to_string()))
            .and_then(passkey_registration_credential_from_proto)?;
        let prepared = self
            .passkey_service()?
            .prepare_registration(&req.session_id, credential, user_id)
            .await
            .map_err(ApiError::from)?;
        self.user_service
            .consume_sensitive_operation_verification(user_id, &req.verification_id)
            .await
            .map_err(ApiError::from)?;
        let credential = self
            .passkey_service()?
            .commit_prepared_registration(prepared)
            .await
            .map_err(ApiError::from)?;

        Ok(passkey_credential_to_proto(&credential))
    }

    pub async fn list_passkeys(&self, user_id: &UserId) -> Result<ListPasskeysResponse, ApiError> {
        let credentials = self
            .passkey_service()?
            .list_credentials(user_id)
            .await
            .map_err(ApiError::from)?
            .iter()
            .map(passkey_credential_to_proto)
            .collect();
        Ok(ListPasskeysResponse { credentials })
    }

    pub async fn delete_passkey(
        &self,
        user_id: &UserId,
        req: DeletePasskeyRequest,
    ) -> Result<DeletePasskeyResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let credential_id =
            synctv_core::service::PasskeyService::decode_credential_id(&req.credential_id)
                .map_err(ApiError::from)?;
        self.user_service
            .consume_sensitive_operation_verification(user_id, &req.verification_id)
            .await
            .map_err(ApiError::from)?;
        let deleted = self
            .passkey_service()?
            .delete_credential(user_id, &credential_id)
            .await
            .map_err(ApiError::from)?;
        Ok(DeletePasskeyResponse { deleted })
    }
}
