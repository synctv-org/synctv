use std::sync::Arc;

use opaque_ke::argon2::Argon2 as OpaqueArgon2Ksf;
use opaque_ke::ciphersuite::CipherSuite;
use opaque_ke::rand::rngs::OsRng;
use opaque_ke::{
    ClientLogin, ClientLoginFinishParameters, ClientRegistration,
    ClientRegistrationFinishParameters, CredentialFinalization, CredentialRequest,
    CredentialResponse, RegistrationRequest, RegistrationResponse, RegistrationUpload, ServerLogin,
    ServerLoginParameters, ServerRegistration, ServerSetup,
};
use rand_08::SeedableRng;
use rand_chacha_08::ChaCha20Rng;
use sha2_010::{Digest, Sha512};

use crate::{
    models::{
        OpaquePasswordRecord, OPAQUE_CIPHERSUITE_RISTRETTO255_SHA512_ARGON2ID,
        OPAQUE_SERVER_SETUP_VERSION,
    },
    Error, Result,
};

struct SyncTvOpaqueCipherSuite;

impl CipherSuite for SyncTvOpaqueCipherSuite {
    type OprfCs = opaque_ke::Ristretto255;
    type KeyExchange = opaque_ke::TripleDh<opaque_ke::Ristretto255, Sha512>;
    type Ksf = OpaqueArgon2Ksf<'static>;
}

pub struct OpaqueRegistrationStart {
    pub registration_response: bytes::Bytes,
}

pub struct OpaqueLoginStart {
    pub credential_response: bytes::Bytes,
    pub server_login_state: Vec<u8>,
}

#[derive(Clone)]
pub struct OpaquePasswordService {
    server_setup: ServerSetup<SyncTvOpaqueCipherSuite>,
    decoy_record: Arc<Result<OpaquePasswordRecord>>,
}

impl OpaquePasswordService {
    fn register_password_with_setup(
        server_setup: &ServerSetup<SyncTvOpaqueCipherSuite>,
        credential_identifier: &[u8],
        password: &str,
    ) -> Result<OpaquePasswordRecord> {
        let mut client_rng = OsRng;
        let client_start = ClientRegistration::<SyncTvOpaqueCipherSuite>::start(
            &mut client_rng,
            password.as_bytes(),
        )
        .map_err(|e| Error::Internal(format!("Failed to start OPAQUE registration: {e}")))?;

        let registration_request =
            RegistrationRequest::deserialize(&client_start.message.serialize()).map_err(|e| {
                Error::Internal(format!(
                    "Failed to deserialize OPAQUE registration request: {e}"
                ))
            })?;
        let server_start = ServerRegistration::<SyncTvOpaqueCipherSuite>::start(
            server_setup,
            registration_request,
            credential_identifier,
        )
        .map_err(|e| Error::Internal(format!("Failed to process OPAQUE registration: {e}")))?;

        let registration_response =
            RegistrationResponse::deserialize(&server_start.message.serialize()).map_err(|e| {
                Error::Internal(format!(
                    "Failed to deserialize OPAQUE registration response: {e}"
                ))
            })?;
        let client_finish = client_start
            .state
            .finish(
                &mut client_rng,
                password.as_bytes(),
                registration_response,
                ClientRegistrationFinishParameters::default(),
            )
            .map_err(|e| Error::Internal(format!("Failed to finish OPAQUE registration: {e}")))?;

        let registration_upload = RegistrationUpload::<SyncTvOpaqueCipherSuite>::deserialize(
            &client_finish.message.serialize(),
        )
        .map_err(|e| {
            Error::Internal(format!(
                "Failed to deserialize OPAQUE registration upload: {e}"
            ))
        })?;
        let server_registration =
            ServerRegistration::<SyncTvOpaqueCipherSuite>::finish(registration_upload);

        Ok(OpaquePasswordRecord {
            record: server_registration.serialize().to_vec(),
            credential_identifier: credential_identifier.to_vec(),
            ciphersuite: OPAQUE_CIPHERSUITE_RISTRETTO255_SHA512_ARGON2ID.to_string(),
            server_setup_version: OPAQUE_SERVER_SETUP_VERSION,
        })
    }

    fn new(server_setup: ServerSetup<SyncTvOpaqueCipherSuite>) -> Self {
        let decoy_record = Self::register_password_with_setup(
            &server_setup,
            b"synctv:decoy-opaque-password-record",
            "synctv-decoy-opaque-password",
        );
        Self {
            server_setup,
            decoy_record: Arc::new(decoy_record),
        }
    }

    #[must_use]
    pub fn new_ephemeral_for_process() -> Self {
        tracing::warn!(
            "using an ephemeral OPAQUE server setup; password credentials created with this setup \
             will be invalid after process restart"
        );
        let mut rng = OsRng;
        Self::new(ServerSetup::new(&mut rng))
    }

    #[must_use]
    pub fn derive_from_secret(secret: &[u8]) -> Self {
        let seed_material =
            Sha512::digest([b"synctv opaque server setup v1".as_slice(), secret].concat());
        let mut seed = [0_u8; 32];
        seed.copy_from_slice(&seed_material[..32]);
        let mut rng = ChaCha20Rng::from_seed(seed);
        Self::new(ServerSetup::new(&mut rng))
    }

    pub fn register_password(
        &self,
        credential_identifier: &[u8],
        password: &str,
    ) -> Result<OpaquePasswordRecord> {
        Self::register_password_with_setup(&self.server_setup, credential_identifier, password)
    }

    pub fn start_registration(
        &self,
        credential_identifier: &[u8],
        registration_request: &[u8],
    ) -> Result<OpaqueRegistrationStart> {
        let registration_request =
            RegistrationRequest::deserialize(registration_request).map_err(|e| {
                Error::InvalidInput(format!("Invalid OPAQUE registration request: {e}"))
            })?;
        let server_start = ServerRegistration::<SyncTvOpaqueCipherSuite>::start(
            &self.server_setup,
            registration_request,
            credential_identifier,
        )
        .map_err(|e| Error::InvalidInput(format!("Invalid OPAQUE registration start: {e}")))?;

        Ok(OpaqueRegistrationStart {
            registration_response: server_start.message.serialize().to_vec().into(),
        })
    }

    pub fn finish_registration(
        &self,
        credential_identifier: Vec<u8>,
        registration_upload: &[u8],
    ) -> Result<OpaquePasswordRecord> {
        let registration_upload =
            RegistrationUpload::<SyncTvOpaqueCipherSuite>::deserialize(registration_upload)
                .map_err(|e| {
                    Error::InvalidInput(format!("Invalid OPAQUE registration upload: {e}"))
                })?;
        let server_registration =
            ServerRegistration::<SyncTvOpaqueCipherSuite>::finish(registration_upload);

        Ok(OpaquePasswordRecord {
            record: server_registration.serialize().to_vec(),
            credential_identifier,
            ciphersuite: OPAQUE_CIPHERSUITE_RISTRETTO255_SHA512_ARGON2ID.to_string(),
            server_setup_version: OPAQUE_SERVER_SETUP_VERSION,
        })
    }

    pub fn start_login(
        &self,
        opaque_record: Option<&OpaquePasswordRecord>,
        credential_identifier: &[u8],
        credential_request: &[u8],
    ) -> Result<OpaqueLoginStart> {
        let credential_request = CredentialRequest::deserialize(credential_request)
            .map_err(|e| Error::InvalidInput(format!("Invalid OPAQUE credential request: {e}")))?;
        let password_file = opaque_record
            .map(|record| {
                ServerRegistration::<SyncTvOpaqueCipherSuite>::deserialize(&record.record)
                    .map_err(|e| Error::Internal(format!("Invalid stored OPAQUE record: {e}")))
            })
            .transpose()?;

        let mut rng = OsRng;
        let server_start = ServerLogin::start(
            &mut rng,
            &self.server_setup,
            password_file,
            credential_request,
            credential_identifier,
            ServerLoginParameters::default(),
        )
        .map_err(|e| Error::InvalidInput(format!("Invalid OPAQUE login start: {e}")))?;

        Ok(OpaqueLoginStart {
            credential_response: server_start.message.serialize().to_vec().into(),
            server_login_state: server_start.state.serialize().to_vec(),
        })
    }

    pub fn finish_login(
        &self,
        server_login_state: &[u8],
        credential_finalization: &[u8],
    ) -> Result<()> {
        let server_login = ServerLogin::<SyncTvOpaqueCipherSuite>::deserialize(server_login_state)
            .map_err(|e| Error::InvalidInput(format!("Invalid OPAQUE login session: {e}")))?;
        let credential_finalization =
            CredentialFinalization::<SyncTvOpaqueCipherSuite>::deserialize(credential_finalization)
                .map_err(|e| {
                    Error::InvalidInput(format!("Invalid OPAQUE credential finalization: {e}"))
                })?;
        server_login
            .finish(credential_finalization, ServerLoginParameters::default())
            .map_err(|_| Error::Authentication("Authentication failed".to_string()))?;
        Ok(())
    }

    pub fn verify_password(&self, record: &OpaquePasswordRecord, password: &str) -> Result<bool> {
        let mut client_rng = OsRng;
        let client_start =
            ClientLogin::<SyncTvOpaqueCipherSuite>::start(&mut client_rng, password.as_bytes())
                .map_err(|e| Error::Internal(format!("Failed to start OPAQUE login: {e}")))?;
        let credential_request = CredentialRequest::deserialize(&client_start.message.serialize())
            .map_err(|e| {
                Error::Internal(format!(
                    "Failed to deserialize OPAQUE credential request: {e}"
                ))
            })?;
        let password_file =
            ServerRegistration::<SyncTvOpaqueCipherSuite>::deserialize(&record.record)
                .map_err(|e| Error::Internal(format!("Invalid stored OPAQUE record: {e}")))?;

        let mut server_rng = OsRng;
        let server_start = ServerLogin::start(
            &mut server_rng,
            &self.server_setup,
            Some(password_file),
            credential_request,
            &record.credential_identifier,
            ServerLoginParameters::default(),
        )
        .map_err(|e| Error::Internal(format!("Failed to start OPAQUE verification: {e}")))?;
        let credential_response = CredentialResponse::<SyncTvOpaqueCipherSuite>::deserialize(
            &server_start.message.serialize(),
        )
        .map_err(|e| {
            Error::Internal(format!(
                "Failed to deserialize OPAQUE credential response: {e}"
            ))
        })?;
        let Ok(client_finish) = client_start.state.finish(
            &mut client_rng,
            password.as_bytes(),
            credential_response,
            ClientLoginFinishParameters::default(),
        ) else {
            return Ok(false);
        };
        let credential_finalization =
            CredentialFinalization::<SyncTvOpaqueCipherSuite>::deserialize(
                &client_finish.message.serialize(),
            )
            .map_err(|e| {
                Error::Internal(format!(
                    "Failed to deserialize OPAQUE credential finalization: {e}"
                ))
            })?;

        Ok(server_start
            .state
            .finish(credential_finalization, ServerLoginParameters::default())
            .is_ok())
    }

    pub fn verify_decoy_password(&self, password: &str) -> Result<bool> {
        let decoy_record = self.decoy_record.as_ref().as_ref().map_err(|error| {
            Error::Internal(format!("Decoy OPAQUE password record unavailable: {error}"))
        })?;
        self.verify_password(decoy_record, password)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opaque_ke::{
        ClientLogin, ClientLoginFinishParameters, CredentialFinalization, CredentialRequest,
        CredentialResponse, ServerLogin, ServerLoginParameters,
    };

    fn ok<T, E: std::fmt::Display>(result: std::result::Result<T, E>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => std::panic::panic_any(format!("{context}: {error}")),
        }
    }

    fn can_login_with_record(
        service: &OpaquePasswordService,
        record: &OpaquePasswordRecord,
        password: &str,
    ) -> bool {
        let mut client_rng = OsRng;
        let client_start = ok(
            ClientLogin::<SyncTvOpaqueCipherSuite>::start(&mut client_rng, password.as_bytes()),
            "client login start should succeed",
        );
        let credential_request = ok(
            CredentialRequest::deserialize(&client_start.message.serialize()),
            "credential request should deserialize",
        );
        let password_file = ok(
            ServerRegistration::<SyncTvOpaqueCipherSuite>::deserialize(&record.record),
            "stored OPAQUE record should deserialize",
        );

        let mut server_rng = OsRng;
        let server_start = ServerLogin::start(
            &mut server_rng,
            &service.server_setup,
            Some(password_file),
            credential_request,
            &record.credential_identifier,
            ServerLoginParameters::default(),
        );
        let Ok(server_start) = server_start else {
            return false;
        };

        let credential_response = ok(
            CredentialResponse::deserialize(&server_start.message.serialize()),
            "credential response should deserialize",
        );
        let Ok(client_finish) = client_start.state.finish(
            &mut client_rng,
            password.as_bytes(),
            credential_response,
            ClientLoginFinishParameters::default(),
        ) else {
            return false;
        };

        let credential_finalization = ok(
            CredentialFinalization::deserialize(&client_finish.message.serialize()),
            "credential finalization should deserialize",
        );
        let Ok(server_finish) = server_start
            .state
            .finish(credential_finalization, ServerLoginParameters::default())
        else {
            return false;
        };

        client_finish.session_key == server_finish.session_key
    }

    #[test]
    fn derived_setup_can_login_with_generated_record() {
        let service = OpaquePasswordService::derive_from_secret(b"stable-secret-32-bytes-minimum");
        let record = ok(
            service.register_password(b"synctv:user:alice", "correct horse battery staple"),
            "registration should succeed",
        );

        assert_eq!(
            record.ciphersuite,
            OPAQUE_CIPHERSUITE_RISTRETTO255_SHA512_ARGON2ID
        );
        assert_eq!(record.server_setup_version, OPAQUE_SERVER_SETUP_VERSION);
        assert!(can_login_with_record(
            &service,
            &record,
            "correct horse battery staple"
        ));
        assert!(!can_login_with_record(&service, &record, "wrong password"));
    }

    #[test]
    fn derived_setup_is_stable_for_same_secret() {
        let first = OpaquePasswordService::derive_from_secret(b"stable-secret-32-bytes-minimum");
        let second = OpaquePasswordService::derive_from_secret(b"stable-secret-32-bytes-minimum");
        let record = ok(
            first.register_password(b"synctv:user:bob", "hunter2 replacement"),
            "registration should succeed",
        );

        assert!(can_login_with_record(
            &second,
            &record,
            "hunter2 replacement"
        ));
    }

    #[test]
    fn different_setup_secret_cannot_use_existing_record() {
        let first = OpaquePasswordService::derive_from_secret(b"stable-secret-32-bytes-minimum");
        let second = OpaquePasswordService::derive_from_secret(b"different-stable-secret-value");
        let record = ok(
            first.register_password(b"synctv:user:carol", "opaque password"),
            "registration should succeed",
        );

        assert!(!can_login_with_record(&second, &record, "opaque password"));
    }

    #[test]
    fn public_registration_and_login_primitives_round_trip_without_plaintext_on_server_login() {
        let service = OpaquePasswordService::derive_from_secret(b"stable-secret-32-bytes-minimum");
        let credential_identifier = b"synctv:user:dave";
        let password = "client side opaque password";

        let mut client_rng = OsRng;
        let client_registration = ok(
            ClientRegistration::<SyncTvOpaqueCipherSuite>::start(
                &mut client_rng,
                password.as_bytes(),
            ),
            "client registration should start",
        );

        let server_registration = ok(
            service.start_registration(
                credential_identifier,
                &client_registration.message.serialize(),
            ),
            "server registration start should succeed",
        );

        let registration_response = ok(
            RegistrationResponse::deserialize(&server_registration.registration_response),
            "registration response should deserialize",
        );
        let client_registration_finish = ok(
            client_registration.state.finish(
                &mut client_rng,
                password.as_bytes(),
                registration_response,
                ClientRegistrationFinishParameters::default(),
            ),
            "client registration should finish",
        );
        let record = ok(
            service.finish_registration(
                credential_identifier.to_vec(),
                &client_registration_finish.message.serialize(),
            ),
            "server registration should finish",
        );

        let client_login = ok(
            ClientLogin::<SyncTvOpaqueCipherSuite>::start(&mut client_rng, password.as_bytes()),
            "client login should start",
        );
        let server_login = ok(
            service.start_login(
                Some(&record),
                credential_identifier,
                &client_login.message.serialize(),
            ),
            "server login should start",
        );
        let credential_response = ok(
            CredentialResponse::deserialize(&server_login.credential_response),
            "credential response should deserialize",
        );
        let client_login_finish = ok(
            client_login.state.finish(
                &mut client_rng,
                password.as_bytes(),
                credential_response,
                ClientLoginFinishParameters::default(),
            ),
            "client login should finish",
        );
        ok(
            service.finish_login(
                &server_login.server_login_state,
                &client_login_finish.message.serialize(),
            ),
            "server login should finish",
        );
    }
}
