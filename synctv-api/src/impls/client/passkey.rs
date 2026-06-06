use synctv_core::models::UserId;

use super::ClientApiImpl;
use crate::impls::ApiError;
use crate::proto::client::{
    DeletePasskeyRequest, DeletePasskeyResponse, ListPasskeysResponse, PasskeyCredential,
    PasskeyCredentialResponse, StartPasskeyBindRequest, StartPasskeyBindResponse,
};

pub(crate) struct PasskeyBindChallenge {
    pub session_id: String,
    pub options_json: Vec<u8>,
}

fn passkey_unavailable_error() -> ApiError {
    ApiError::ServiceUnavailable("Passkey/WebAuthn service is not configured".to_string())
}

pub(crate) fn passkey_options_to_json_bytes(options_json: Vec<u8>) -> Result<Vec<u8>, ApiError> {
    serde_json::from_slice::<serde_json::Value>(&options_json)
        .map_err(|error| ApiError::Internal(format!("Invalid passkey challenge JSON: {error}")))?;
    Ok(options_json)
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
            options_json: challenge.options_json,
        })
    }

    pub async fn start_passkey_bind(
        &self,
        user_id: &UserId,
        req: StartPasskeyBindRequest,
    ) -> Result<StartPasskeyBindResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let challenge = self.start_passkey_bind_challenge(user_id, req.name).await?;
        let options = passkey_options_to_json_bytes(challenge.options_json)?;
        Ok(StartPasskeyBindResponse {
            session_id: challenge.session_id,
            options,
        })
    }

    pub async fn finish_passkey_bind_request(
        &self,
        user_id: &UserId,
        req: crate::proto::client::FinishPasskeyBindRequest,
    ) -> Result<PasskeyCredentialResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let prepared = self
            .passkey_service()?
            .prepare_registration(&req.session_id, &req.credential, user_id)
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

        Ok(PasskeyCredentialResponse {
            credential: Some(passkey_credential_to_proto(&credential)),
        })
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
