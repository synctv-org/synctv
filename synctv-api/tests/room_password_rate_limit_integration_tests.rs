#![allow(clippy::unwrap_used)]

mod support;

use chrono::Utc;
use opaque_ke::argon2::Argon2 as OpaqueArgon2Ksf;
use opaque_ke::ciphersuite::CipherSuite;
use opaque_ke::rand::rngs::OsRng;
use opaque_ke::{
    ClientLogin, ClientLoginFinishParameters, ClientRegistration,
    ClientRegistrationFinishParameters, CredentialResponse, RegistrationResponse,
};
use std::sync::Arc;
use synctv_core::{
    cache::{KeyBuilder, UsernameCache},
    models::{SignupMethod, User, UserId, UserRole, UserStatus},
    repository::UserRepository,
    service::{
        auth::{BruteForceProtection, JwtService},
        room::RoomServiceOptions,
        InMemoryTokenBlacklistStore, RoomService, UserService,
    },
    Config,
};
use synctv_realtime::sync::{ConnectionLimits, ConnectionManager};

struct PendingOpaqueRoomLogin {
    session_id: String,
    credential_finalization: Vec<u8>,
}

struct TestOpaqueCipherSuite;

impl CipherSuite for TestOpaqueCipherSuite {
    type OprfCs = opaque_ke::Ristretto255;
    type KeyExchange = opaque_ke::TripleDh<opaque_ke::Ristretto255, sha2_010::Sha512>;
    type Ksf = OpaqueArgon2Ksf<'static>;
}

fn make_user(username: &str) -> User {
    let now = Utc::now();
    User {
        id: UserId::new(),
        username: username.to_string(),
        role: UserRole::User,
        avatar_file_reference_id: None,
        status: UserStatus::Active,
        is_banned: false,
        banned_at: None,
        banned_by: None,
        banned_reason: None,
        signup_method: SignupMethod::Email,
        created_at: now,
        updated_at: now,
        version: 0,
        deleted_at: None,
    }
}

fn make_user_service(pool: &sqlx::PgPool) -> UserService {
    let jwt_service = JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap();
    let username_cache = UsernameCache::local_only("test:username:".to_string(), 100, 60);
    let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(1000, 3600, 86400));
    UserService::new_for_tests(
        pool,
        jwt_service,
        username_cache,
        token_blacklist,
        KeyBuilder::new("test"),
        BruteForceProtection::in_memory("test:user".to_string()),
    )
}

async fn opaque_room_login(
    client_api: &synctv_api::impls::ClientApiImpl,
    user_id: &UserId,
    room_id: &str,
    password: &str,
    client_ip: &str,
) -> Result<synctv_proto::client::JoinRoomResponse, synctv_api::impls::ApiError> {
    let mut rng = OsRng;
    let client_start = ClientLogin::<TestOpaqueCipherSuite>::start(&mut rng, password.as_bytes())
        .expect("client OPAQUE login start should succeed");
    let challenge = client_api
        .start_room_password_login_with_control(
            user_id,
            synctv_proto::client::StartRoomPasswordLoginRequest {
                room_id: room_id.to_string(),
                credential_request: client_start.message.serialize().to_vec(),
            },
            Some(client_ip),
            None,
        )
        .await?;
    let credential_response =
        CredentialResponse::<TestOpaqueCipherSuite>::deserialize(&challenge.credential_response)
            .expect("server credential response should deserialize");
    let client_finish = client_start
        .state
        .finish(
            &mut rng,
            password.as_bytes(),
            credential_response,
            ClientLoginFinishParameters::default(),
        )
        .map_err(|_| synctv_api::impls::ApiError::Authentication("Authentication failed".into()))?;

    client_api
        .finish_room_password_login_with_control(
            user_id,
            None,
            synctv_proto::client::FinishRoomPasswordLoginRequest {
                session_id: challenge.session_id,
                credential_finalization: client_finish.message.serialize().to_vec(),
            },
            Some(client_ip),
        )
        .await
}

async fn start_opaque_room_login(
    client_api: &synctv_api::impls::ClientApiImpl,
    user_id: &UserId,
    room_id: &str,
    password: &str,
    client_ip: &str,
) -> Result<PendingOpaqueRoomLogin, synctv_api::impls::ApiError> {
    let mut rng = OsRng;
    let client_start = ClientLogin::<TestOpaqueCipherSuite>::start(&mut rng, password.as_bytes())
        .expect("client OPAQUE login start should succeed");
    let challenge = client_api
        .start_room_password_login_with_control(
            user_id,
            synctv_proto::client::StartRoomPasswordLoginRequest {
                room_id: room_id.to_string(),
                credential_request: client_start.message.serialize().to_vec(),
            },
            Some(client_ip),
            None,
        )
        .await?;
    let credential_response =
        CredentialResponse::<TestOpaqueCipherSuite>::deserialize(&challenge.credential_response)
            .expect("server credential response should deserialize");
    let client_finish = client_start
        .state
        .finish(
            &mut rng,
            password.as_bytes(),
            credential_response,
            ClientLoginFinishParameters::default(),
        )
        .map_err(|_| synctv_api::impls::ApiError::Authentication("Authentication failed".into()))?;

    Ok(PendingOpaqueRoomLogin {
        session_id: challenge.session_id,
        credential_finalization: client_finish.message.serialize().to_vec(),
    })
}

async fn start_tampered_opaque_room_login(
    client_api: &synctv_api::impls::ClientApiImpl,
    user_id: &UserId,
    room_id: &str,
    password: &str,
    client_ip: &str,
) -> Result<PendingOpaqueRoomLogin, synctv_api::impls::ApiError> {
    let mut login =
        start_opaque_room_login(client_api, user_id, room_id, password, client_ip).await?;
    if let Some(first) = login.credential_finalization.first_mut() {
        *first ^= 0x01;
    } else {
        login.credential_finalization = vec![1];
    }
    Ok(login)
}

async fn finish_opaque_room_login(
    client_api: &synctv_api::impls::ClientApiImpl,
    user_id: &UserId,
    login: PendingOpaqueRoomLogin,
    client_ip: &str,
) -> Result<synctv_proto::client::JoinRoomResponse, synctv_api::impls::ApiError> {
    client_api
        .finish_room_password_login_with_control(
            user_id,
            None,
            synctv_proto::client::FinishRoomPasswordLoginRequest {
                session_id: login.session_id,
                credential_finalization: login.credential_finalization,
            },
            Some(client_ip),
        )
        .await
}

fn make_client_api(
    user_service: Arc<UserService>,
    room_service: Arc<RoomService>,
) -> synctv_api::impls::ClientApiImpl {
    let connection_manager = Arc::new(ConnectionManager::new(ConnectionLimits::default()));

    synctv_api::impls::ClientApiImpl::new_with_runtime(
        synctv_api::impls::ClientApiConfig {
            read_pool: None,
            user_service,
            room_service,
            connection_service: connection_manager,
            config: Arc::new(Config::default()),
            publish_key_service: None,
            jwt_service: JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap(),
            live_streaming_infrastructure: None,
            settings_registry: None,
            public_id_codec: Arc::new(synctv_core::PublicIdCodec::plain()),
            chat_service: None,
            provider_stores: Arc::new(synctv_core::provider::ProviderStoreRegistry::local_only(
                "test:provider:",
            )),
            email_api: None,
            passkey_service: None,
        },
        support::client_api_runtime(),
    )
}

async fn opaque_room_password_registration_upload(
    client_api: &synctv_api::impls::ClientApiImpl,
    user_id: &UserId,
    room_id: &str,
    password: &str,
) -> (
    String,
    synctv_proto::client::FinishRoomPasswordRegistrationRequest,
) {
    let mut rng = OsRng;
    let client_start =
        ClientRegistration::<TestOpaqueCipherSuite>::start(&mut rng, password.as_bytes())
            .expect("client OPAQUE registration start should succeed");
    let challenge = client_api
        .start_room_password_registration(
            user_id,
            room_id,
            synctv_proto::client::StartRoomPasswordRegistrationRequest {
                registration_request: client_start.message.serialize().to_vec(),
            },
        )
        .await
        .expect("room password registration start should succeed");
    let registration_response = RegistrationResponse::<TestOpaqueCipherSuite>::deserialize(
        &challenge.registration_response,
    )
    .expect("server registration response should deserialize");
    let client_finish = client_start
        .state
        .finish(
            &mut rng,
            password.as_bytes(),
            registration_response,
            ClientRegistrationFinishParameters::default(),
        )
        .expect("client OPAQUE registration finish should succeed");

    (
        challenge.session_id.clone(),
        synctv_proto::client::FinishRoomPasswordRegistrationRequest {
            session_id: challenge.session_id,
            registration_upload: client_finish.message.serialize().to_vec(),
        },
    )
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_finish_room_password_registration_rejects_session_for_different_room() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let user_service = Arc::new(make_user_service(&pool));
    let room_service = Arc::new(
        RoomService::new_for_tests(pool.clone(), (*user_service).clone())
            .expect("room service should build"),
    );
    let owner = user_repo
        .create(&make_user("password_registration_owner"))
        .await
        .unwrap();
    let (room_a, _) = room_service
        .create_room(
            "Password Registration A".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();
    let (room_b, _) = room_service
        .create_room(
            "Password Registration B".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();

    let client_api = make_client_api(user_service, room_service.clone());
    let codec = synctv_core::PublicIdCodec::plain();
    let room_a_public_id = codec.encode_room_id(room_a.id).unwrap();
    let room_b_public_id = codec.encode_room_id(room_b.id).unwrap();
    let (_session_id, finish_req) = opaque_room_password_registration_upload(
        &client_api,
        &owner.id,
        &room_a_public_id,
        "RoomPassword123",
    )
    .await;

    let error = client_api
        .finish_room_password_registration(&owner.id, &room_b_public_id, finish_req)
        .await
        .expect_err("room password registration session must be bound to one room");
    assert!(
        matches!(error, synctv_api::impls::ApiError::InvalidInput(ref message)
            if message.contains("does not match room")),
        "unexpected error: {error}"
    );

    assert!(
        !room_service
            .is_room_password_enabled(&room_b.id)
            .await
            .unwrap(),
        "wrong-room finish must leave target room password disabled"
    );
    assert!(
        !room_service
            .is_room_password_enabled(&room_a.id)
            .await
            .unwrap(),
        "wrong-room finish must leave source room password disabled"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_finish_room_password_login_rejects_session_for_different_room_before_join() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let user_service = Arc::new(make_user_service(&pool));
    let room_service = Arc::new(
        RoomService::new_for_tests(pool.clone(), (*user_service).clone())
            .expect("room service should build"),
    );
    let owner = user_repo
        .create(&make_user("password_login_owner"))
        .await
        .unwrap();
    let joining_user = user_repo
        .create(&make_user("password_login_member"))
        .await
        .unwrap();
    let (room_a, _) = room_service
        .create_room(
            "Password Login A".to_string(),
            String::new(),
            owner.id,
            Some("RoomPassword123".to_string()),
            None,
        )
        .await
        .unwrap();
    let (room_b, _) = room_service
        .create_room(
            "Password Login B".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();

    let client_api = make_client_api(user_service, room_service.clone());
    let codec = synctv_core::PublicIdCodec::plain();
    let room_a_public_id = codec.encode_room_id(room_a.id).unwrap();
    let room_b_public_id = codec.encode_room_id(room_b.id).unwrap();

    let mut rng = OsRng;
    let client_start = ClientLogin::<TestOpaqueCipherSuite>::start(&mut rng, b"RoomPassword123")
        .expect("client OPAQUE login start should succeed");
    let challenge = client_api
        .start_room_password_login_with_control(
            &joining_user.id,
            synctv_proto::client::StartRoomPasswordLoginRequest {
                room_id: room_a_public_id,
                credential_request: client_start.message.serialize().to_vec(),
            },
            Some("192.168.1.101"),
            None,
        )
        .await
        .expect("room password login start should succeed");
    let credential_response =
        CredentialResponse::<TestOpaqueCipherSuite>::deserialize(&challenge.credential_response)
            .expect("server credential response should deserialize");
    let client_finish = client_start
        .state
        .finish(
            &mut rng,
            b"RoomPassword123",
            credential_response,
            ClientLoginFinishParameters::default(),
        )
        .expect("client OPAQUE login finish should succeed");

    let error = client_api
        .finish_room_password_login_with_control(
            &joining_user.id,
            Some(&room_b_public_id),
            synctv_proto::client::FinishRoomPasswordLoginRequest {
                session_id: challenge.session_id,
                credential_finalization: client_finish.message.serialize().to_vec(),
            },
            Some("192.168.1.101"),
        )
        .await
        .expect_err("room password login session must be bound to one room");
    assert!(
        matches!(error, synctv_api::impls::ApiError::InvalidInput(ref message)
            if message.contains("does not match room")),
        "unexpected error: {error}"
    );
    assert!(
        room_service
            .get_member(&room_a.id, &joining_user.id)
            .await
            .unwrap()
            .is_none(),
        "wrong-room finish must not join the session room"
    );
    assert!(
        room_service
            .get_member(&room_b.id, &joining_user.id)
            .await
            .unwrap()
            .is_none(),
        "wrong-room finish must not join the path room"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_client_api_room_password_success_resets_bruteforce_counter() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let user_service = Arc::new(make_user_service(&pool));
    let room_service = RoomService::new_with_options(
        pool.clone(),
        (*user_service).clone(),
        RoomServiceOptions {
            brute_force_service: Some(Arc::new(BruteForceProtection::in_memory(
                "test:room-password".to_string(),
            ))),
            ..RoomServiceOptions::test_defaults_with_settings(pool.clone())
        },
    )
    .expect("room service should build");
    let room_service = Arc::new(room_service);

    let owner = user_repo.create(&make_user("room_owner")).await.unwrap();
    let member = user_repo.create(&make_user("room_member")).await.unwrap();
    let (room, _member) = room_service
        .create_room(
            "Protected Room".to_string(),
            "Room with password".to_string(),
            owner.id,
            Some("CorrectPassword123".to_string()),
            None,
        )
        .await
        .unwrap();

    let client_api = make_client_api(user_service, room_service.clone());

    let room_public_id = synctv_core::PublicIdCodec::plain()
        .encode_room_id(room.id)
        .unwrap();

    for _attempt in 0..4 {
        let err = opaque_room_login(
            &client_api,
            &member.id,
            &room_public_id,
            "WrongPassword",
            "192.168.1.100",
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string().contains("Authentication failed"),
            "wrong password should stay user-readable: {err}"
        );
    }

    opaque_room_login(
        &client_api,
        &member.id,
        &room_public_id,
        "CorrectPassword123",
        "192.168.1.100",
    )
    .await
    .expect("successful password check should pass");

    room_service
        .leave_room(room.id, member.id)
        .await
        .expect("member should be able to leave after successful join");

    let err = opaque_room_login(
        &client_api,
        &member.id,
        &room_public_id,
        "WrongPassword",
        "192.168.1.100",
    )
    .await
    .expect_err("successful join must reset room password brute-force counter");
    assert!(
        err.to_string().contains("Authentication failed"),
        "counter should be reset so next wrong password attempt is checked normally: {err}"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_preissued_room_password_opaque_sessions_cannot_bypass_finish_lockout() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let user_service = Arc::new(make_user_service(&pool));
    let room_service = RoomService::new_with_options(
        pool.clone(),
        (*user_service).clone(),
        RoomServiceOptions {
            brute_force_service: Some(Arc::new(BruteForceProtection::in_memory(
                "test:room-password-preissued".to_string(),
            ))),
            ..RoomServiceOptions::test_defaults_with_settings(pool.clone())
        },
    )
    .expect("room service should build");
    let room_service = Arc::new(room_service);

    let owner = user_repo
        .create(&make_user("preissued_room_owner"))
        .await
        .unwrap();
    let member = user_repo
        .create(&make_user("preissued_room_member"))
        .await
        .unwrap();
    let (room, _member) = room_service
        .create_room(
            "Preissued Protected Room".to_string(),
            "Room with password".to_string(),
            owner.id,
            Some("CorrectPassword123".to_string()),
            None,
        )
        .await
        .unwrap();

    let client_api = make_client_api(user_service, room_service.clone());
    let room_public_id = synctv_core::PublicIdCodec::plain()
        .encode_room_id(room.id)
        .unwrap();
    let client_ip = "192.168.1.102";

    let mut logins = Vec::new();
    for _attempt in 0..6 {
        logins.push(
            start_tampered_opaque_room_login(
                &client_api,
                &member.id,
                &room_public_id,
                "CorrectPassword123",
                client_ip,
            )
            .await
            .expect("preissued OPAQUE login start should pass before failures are recorded"),
        );
    }

    for attempt in 0..5 {
        let err = finish_opaque_room_login(&client_api, &member.id, logins.remove(0), client_ip)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("Authentication failed"),
            "wrong password finish {attempt} should count as an authentication failure: {err}"
        );
    }

    let err = finish_opaque_room_login(&client_api, &member.id, logins.remove(0), client_ip)
        .await
        .expect_err("preissued session must be blocked after room password lockout");
    let msg = err.to_string();
    assert!(
        msg.contains("Too many failed") || msg.contains("locked") || msg.contains("try again"),
        "6th preissued finish should be blocked by room password brute-force protection: {msg}"
    );
    assert!(
        room_service
            .get_member(&room.id, &member.id)
            .await
            .unwrap()
            .is_none(),
        "locked out preissued finish must not join the room"
    );
}
