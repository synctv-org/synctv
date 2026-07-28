//! User Provider Credential Repository Tests
//!
//! Integration tests for the `UserProviderCredentialRepository`.
//!
//! (Requires Docker)

use chrono::{Duration, Utc};
use synctv_core::{
    credential_encryption::CredentialEncryption,
    models::{
        ProviderCredential, ProviderInstance, ProviderType, SignupMethod, SourceProvider,
        SynologyApiBinding, User, UserId, UserProviderCredential,
    },
    provider::{AlistProvider, BilibiliProvider},
    repository::{ProviderInstanceRepository, UserProviderCredentialRepository, UserRepository},
};
use synctv_core_testing::create_test_pool;
use synctv_core_testing::{TestOptionExt, TestResultExt};

fn test_encryption() -> CredentialEncryption {
    CredentialEncryption::new(&[0x42; 32]).checked("test encryption should be created")
}
fn make_user(username: &str) -> User {
    User::new(username.to_string(), SignupMethod::Email)
}

fn bilibili_server_id() -> String {
    BilibiliProvider::credential_server_id()
}

fn provider_code(provider: ProviderType) -> i16 {
    provider.as_i16()
}

fn make_credential(user_id: UserId, provider: &str, server_id: &str) -> UserProviderCredential {
    let now = Utc::now();
    UserProviderCredential {
        id: 0,
        user_id,
        provider: provider.to_string(),
        server_id: server_id.to_string(),
        provider_instance_name: None,
        credential_data: match provider {
            "bilibili" => ProviderCredential::Bilibili {
                cookies: std::collections::HashMap::from([(
                    "SESSDATA".to_string(),
                    "test_session".to_string(),
                )]),
            },
            "alist" => ProviderCredential::Alist {
                host: "https://alist.example.com".to_string(),
                username: "alice".to_string(),
                password: "hashed_password".to_string(),
                otp_secret: None,
            },
            "emby" => ProviderCredential::Emby {
                host: "https://emby.example.com".to_string(),
                api_key: "api_key".to_string(),
                emby_user_id: "emby_user".to_string(),
            },
            "twitch" => ProviderCredential::Twitch {
                login: "synctv".to_string(),
                twitch_user_id: "123456".to_string(),
                client_id: "client-id".to_string(),
                scopes: vec!["user:read:follows".to_string()],
                auth_token: "oauth-token".to_string(),
                device_id: Some("device-id".to_string()),
                client_integrity: Some("integrity-token".to_string()),
            },
            "fnos" => ProviderCredential::Fnos {
                endpoint: "wss://fnos.example.com:5667/websocket?type=main".to_string(),
                webdav_endpoint: Some("https://fnos.example.com:5006/dav".to_string()),
                username: "alice".to_string(),
                password: "fnos-password".to_string(),
                token: "short-token".to_string(),
                long_token: Some("long-token".to_string()),
                secret: "c2VjcmV0".to_string(),
                media_endpoint: Some("https://fnos.example.com:5667".to_string()),
                media_token: Some("media-token".to_string()),
            },
            "qnap" => ProviderCredential::Qnap {
                endpoint: "https://qnap.example.com".to_string(),
                username: "alice".to_string(),
                password: "qnap-password".to_string(),
                sid: "qnap-session".to_string(),
                server_name: "QNAP".to_string(),
                version: Some("5.2".to_string()),
                support_rtt: true,
            },
            "synology" => ProviderCredential::Synology {
                endpoint: "https://dsm.example.com:5001".to_string(),
                username: "alice".to_string(),
                password: "dsm-password".to_string(),
                file_sid: "file-session".to_string(),
                video_sid: Some("video-session".to_string()),
                device_id: Some("device-token".to_string()),
                synotoken: Some("syno-token".to_string()),
                apis: std::collections::HashMap::from([(
                    "SYNO.API.Auth".to_string(),
                    SynologyApiBinding {
                        path: "auth.cgi".to_string(),
                        min_version: 1,
                        max_version: 7,
                    },
                )]),
            },
            "nextcloud" => ProviderCredential::Nextcloud {
                endpoint: "https://cloud.example.com/nextcloud".to_string(),
                username: "alice".to_string(),
                user_id: "alice-id".to_string(),
                app_password: "nextcloud-app-password".to_string(),
                version: "32.0.1".to_string(),
                edition: "Enterprise".to_string(),
                capabilities: serde_json::json!({"dav": {"chunking": "1.0"}}),
            },
            "seafile" => ProviderCredential::Seafile {
                endpoint: "https://seafile.example.com".to_string(),
                username: "alice@example.com".to_string(),
                token: "seafile-api-token".to_string(),
                version: "11.0.12".to_string(),
                features: vec!["seafile-basic".to_string()],
                library_passwords: std::collections::HashMap::from([(
                    "repo-encrypted".to_string(),
                    "library-password".to_string(),
                )]),
            },
            "truenas" => ProviderCredential::TrueNas {
                endpoint: "https://truenas.example.com".to_string(),
                api_key: "truenas-api-key".to_string(),
                hostname: "truenas".to_string(),
                version: "25.10".to_string(),
                system_product: "TrueNAS SCALE".to_string(),
            },
            "youtube" => ProviderCredential::Youtube {
                label: "Browser session".to_string(),
                visitor_data: Some("visitor-data-secret".to_string()),
                po_token: Some("po-token-secret".to_string()),
                cookie: Some("LOGIN_INFO=login; SAPISID=cookie-secret".to_string()),
            },
            "douyin" => ProviderCredential::Douyin {
                label: "Browser session".to_string(),
                cookie: "sessionid=douyin-cookie-secret; ttwid=device-secret".to_string(),
            },
            "tiktok" => ProviderCredential::TikTok {
                label: "Browser session".to_string(),
                cookie: "sessionid=tiktok-cookie-secret; tt_csrf_token=csrf-secret".to_string(),
            },
            _ => ProviderCredential::default(),
        },
        expires_at: None,
        created_at: now,
        updated_at: now,
    }
}

fn make_credential_with_instance(
    user_id: UserId,
    provider: &str,
    server_id: &str,
    provider_instance_name: Option<&str>,
) -> UserProviderCredential {
    let mut credential = make_credential(user_id, provider, server_id);
    credential.provider_instance_name = provider_instance_name.map(ToString::to_string);
    credential
}

fn make_provider_instance(name: &str, providers: &[&str]) -> ProviderInstance {
    ProviderInstance {
        name: name.to_string(),
        endpoint: format!("http://{name}.example.com:50051"),
        comment: None,
        jwt_secret: None,
        custom_ca: None,
        timeout: "10s".to_string(),
        tls: false,
        insecure_tls: false,
        providers: providers
            .iter()
            .map(|provider| {
                provider
                    .parse::<SourceProvider>()
                    .checked("test provider should be known")
            })
            .collect(),
        enabled: true,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_create_credential() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let cred_repo = UserProviderCredentialRepository::new_with_encryption(pool, test_encryption());

    let user = user_repo
        .create(&make_user("cred_user1"))
        .await
        .checked("test operation should succeed");

    let bilibili_server_id = bilibili_server_id();
    let cred = make_credential(user.id, "bilibili", &bilibili_server_id);
    cred_repo
        .create(&cred)
        .await
        .checked("test operation should succeed");

    // Retrieve it
    let found = cred_repo
        .get_by_provider_and_server(user.id, "bilibili", &bilibili_server_id)
        .await
        .checked("test operation should succeed");

    assert!(found.is_some());
    let found = found.checked("test operation should succeed");
    assert_eq!(found.user_id, user.id);
    assert_eq!(found.provider, "bilibili");
    assert_eq!(found.server_id, bilibili_server_id);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_twitch_credential_encrypted_postgres_round_trip() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let cred_repo = UserProviderCredentialRepository::new_with_encryption(pool, test_encryption());
    let user = user_repo
        .create(&make_user("twitch_credential_user"))
        .await
        .checked("test user should be created");
    let server_id = "twitch-instance-server-id";
    cred_repo
        .create(&make_credential(user.id, "twitch", server_id))
        .await
        .checked("Twitch credential should be persisted");

    let stored = cred_repo
        .get_by_provider_and_server(user.id, "twitch", server_id)
        .await
        .checked("Twitch credential lookup should succeed")
        .checked("Twitch credential should exist");
    let ProviderCredential::Twitch {
        login,
        twitch_user_id,
        client_id,
        scopes,
        auth_token,
        device_id,
        client_integrity,
    } = stored.credential_data
    else {
        panic!("expected Twitch credential");
    };
    assert_eq!(login, "synctv");
    assert_eq!(twitch_user_id, "123456");
    assert_eq!(client_id, "client-id");
    assert_eq!(scopes, ["user:read:follows"]);
    assert_eq!(auth_token, "oauth-token");
    assert_eq!(device_id.as_deref(), Some("device-id"));
    assert_eq!(client_integrity.as_deref(), Some("integrity-token"));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_youtube_credential_encrypted_postgres_round_trip() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let cred_repo = UserProviderCredentialRepository::new_with_encryption(pool, test_encryption());
    let user = user_repo
        .create(&make_user("youtube_credential_user"))
        .await
        .checked("test user should be created");
    let server_id = "youtube-instance-server-id";
    cred_repo
        .create(&make_credential(user.id, "youtube", server_id))
        .await
        .checked("YouTube credential should be persisted");

    let stored = cred_repo
        .get_by_provider_and_server(user.id, "youtube", server_id)
        .await
        .checked("YouTube credential lookup should succeed")
        .checked("YouTube credential should exist");
    let ProviderCredential::Youtube {
        label,
        visitor_data,
        po_token,
        cookie,
    } = stored.credential_data
    else {
        panic!("expected YouTube credential");
    };
    assert_eq!(label, "Browser session");
    assert_eq!(visitor_data.as_deref(), Some("visitor-data-secret"));
    assert_eq!(po_token.as_deref(), Some("po-token-secret"));
    assert_eq!(
        cookie.as_deref(),
        Some("LOGIN_INFO=login; SAPISID=cookie-secret")
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_douyin_credential_encrypted_postgres_round_trip() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let cred_repo =
        UserProviderCredentialRepository::new_with_encryption(pool.clone(), test_encryption());
    let user = user_repo
        .create(&make_user("douyin_credential_user"))
        .await
        .checked("test user should be created");
    let server_id = "douyin-instance-server-id";
    cred_repo
        .create(&make_credential(user.id, "douyin", server_id))
        .await
        .checked("Douyin credential should be persisted");

    let raw: String = sqlx::query_scalar(
        "SELECT credential_data::text FROM user_media_provider_credentials \
         WHERE user_id = $1 AND provider = $2 AND server_id = $3",
    )
    .bind(user.id.as_i64())
    .bind(provider_code(SourceProvider::Douyin))
    .bind(server_id)
    .fetch_one(&pool)
    .await
    .checked("raw Douyin credential should be readable");
    assert!(!raw.contains("douyin-cookie-secret"));
    assert!(!raw.contains("device-secret"));

    let stored = cred_repo
        .get_by_provider_and_server(user.id, "douyin", server_id)
        .await
        .checked("Douyin credential lookup should succeed")
        .checked("Douyin credential should exist");
    let ProviderCredential::Douyin { label, cookie } = stored.credential_data else {
        panic!("expected Douyin credential");
    };
    assert_eq!(label, "Browser session");
    assert_eq!(
        cookie,
        "sessionid=douyin-cookie-secret; ttwid=device-secret"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_tiktok_credential_encrypted_postgres_round_trip() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let cred_repo =
        UserProviderCredentialRepository::new_with_encryption(pool.clone(), test_encryption());
    let user = user_repo
        .create(&make_user("tiktok_credential_user"))
        .await
        .checked("test user should be created");
    let server_id = "tiktok-instance-server-id";
    cred_repo
        .create(&make_credential(user.id, "tiktok", server_id))
        .await
        .checked("TikTok credential should be persisted");

    let raw: String = sqlx::query_scalar(
        "SELECT credential_data::text FROM user_media_provider_credentials \
         WHERE user_id = $1 AND provider = $2 AND server_id = $3",
    )
    .bind(user.id.as_i64())
    .bind(provider_code(SourceProvider::TikTok))
    .bind(server_id)
    .fetch_one(&pool)
    .await
    .checked("raw TikTok credential should be readable");
    assert!(!raw.contains("tiktok-cookie-secret"));
    assert!(!raw.contains("csrf-secret"));

    let stored = cred_repo
        .get_by_provider_and_server(user.id, "tiktok", server_id)
        .await
        .checked("TikTok credential lookup should succeed")
        .checked("TikTok credential should exist");
    let ProviderCredential::TikTok { label, cookie } = stored.credential_data else {
        panic!("expected TikTok credential");
    };
    assert_eq!(label, "Browser session");
    assert_eq!(
        cookie,
        "sessionid=tiktok-cookie-secret; tt_csrf_token=csrf-secret"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_fnos_credential_encrypted_postgres_round_trip() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let cred_repo = UserProviderCredentialRepository::new_with_encryption(pool, test_encryption());
    let user = user_repo
        .create(&make_user("fnos_credential_user"))
        .await
        .checked("test user should be created");
    let server_id = "fnos-instance-server-id";
    cred_repo
        .create(&make_credential(user.id, "fnos", server_id))
        .await
        .checked("FNOS credential should be persisted");

    let stored = cred_repo
        .get_by_provider_and_server(user.id, "fnos", server_id)
        .await
        .checked("FNOS credential lookup should succeed")
        .checked("FNOS credential should exist");
    let ProviderCredential::Fnos {
        endpoint,
        webdav_endpoint,
        username,
        password,
        token,
        long_token,
        secret,
        media_endpoint,
        media_token,
    } = stored.credential_data
    else {
        panic!("expected FNOS credential");
    };
    assert_eq!(endpoint, "wss://fnos.example.com:5667/websocket?type=main");
    assert_eq!(
        webdav_endpoint.as_deref(),
        Some("https://fnos.example.com:5006/dav")
    );
    assert_eq!(username, "alice");
    assert_eq!(password, "fnos-password");
    assert_eq!(token, "short-token");
    assert_eq!(long_token.as_deref(), Some("long-token"));
    assert_eq!(secret, "c2VjcmV0");
    assert_eq!(
        media_endpoint.as_deref(),
        Some("https://fnos.example.com:5667")
    );
    assert_eq!(media_token.as_deref(), Some("media-token"));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_qnap_credential_encrypted_postgres_round_trip() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let cred_repo = UserProviderCredentialRepository::new_with_encryption(pool, test_encryption());
    let user = user_repo
        .create(&make_user("qnap_credential_user"))
        .await
        .checked("test user should be created");
    let server_id = "qnap-instance-server-id";
    cred_repo
        .create(&make_credential(user.id, "qnap", server_id))
        .await
        .checked("QNAP credential should be persisted");
    let stored = cred_repo
        .get_by_provider_and_server(user.id, "qnap", server_id)
        .await
        .checked("QNAP credential lookup should succeed")
        .checked("QNAP credential should exist");
    assert!(matches!(
        stored.credential_data,
        ProviderCredential::Qnap {
            endpoint,
            username,
            password,
            sid,
            server_name,
            version,
            support_rtt: true,
        } if endpoint == "https://qnap.example.com"
            && username == "alice"
            && password == "qnap-password"
            && sid == "qnap-session"
            && server_name == "QNAP"
            && version.as_deref() == Some("5.2")
    ));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_synology_credential_encrypted_postgres_round_trip() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let cred_repo = UserProviderCredentialRepository::new_with_encryption(pool, test_encryption());
    let user = user_repo
        .create(&make_user("synology_credential_user"))
        .await
        .checked("test user should be created");
    let server_id = "synology-instance-server-id";
    cred_repo
        .create(&make_credential(user.id, "synology", server_id))
        .await
        .checked("Synology credential should be persisted");

    let stored = cred_repo
        .get_by_provider_and_server(user.id, "synology", server_id)
        .await
        .checked("Synology credential lookup should succeed")
        .checked("Synology credential should exist");
    let ProviderCredential::Synology {
        endpoint,
        username,
        password,
        file_sid,
        video_sid,
        device_id,
        synotoken,
        apis,
    } = stored.credential_data
    else {
        panic!("expected Synology credential");
    };
    assert_eq!(endpoint, "https://dsm.example.com:5001");
    assert_eq!(username, "alice");
    assert_eq!(password, "dsm-password");
    assert_eq!(file_sid, "file-session");
    assert_eq!(video_sid.as_deref(), Some("video-session"));
    assert_eq!(device_id.as_deref(), Some("device-token"));
    assert_eq!(synotoken.as_deref(), Some("syno-token"));
    assert_eq!(apis["SYNO.API.Auth"].max_version, 7);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_nextcloud_credential_encrypted_postgres_round_trip() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let cred_repo = UserProviderCredentialRepository::new_with_encryption(pool, test_encryption());
    let user = user_repo
        .create(&make_user("nextcloud_credential_user"))
        .await
        .checked("test user should be created");
    let server_id = "nextcloud-instance-server-id";
    cred_repo
        .create(&make_credential(user.id, "nextcloud", server_id))
        .await
        .checked("Nextcloud credential should be persisted");
    let stored = cred_repo
        .get_by_provider_and_server(user.id, "nextcloud", server_id)
        .await
        .checked("Nextcloud credential lookup should succeed")
        .checked("Nextcloud credential should exist");
    let ProviderCredential::Nextcloud {
        endpoint,
        username,
        user_id,
        app_password,
        version,
        edition,
        capabilities,
    } = stored.credential_data
    else {
        panic!("expected Nextcloud credential");
    };
    assert_eq!(endpoint, "https://cloud.example.com/nextcloud");
    assert_eq!(username, "alice");
    assert_eq!(user_id, "alice-id");
    assert_eq!(app_password, "nextcloud-app-password");
    assert_eq!(version, "32.0.1");
    assert_eq!(edition, "Enterprise");
    assert_eq!(capabilities["dav"]["chunking"], "1.0");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_seafile_credential_encrypted_postgres_round_trip() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let cred_repo = UserProviderCredentialRepository::new_with_encryption(pool, test_encryption());
    let user = user_repo
        .create(&make_user("seafile_credential_user"))
        .await
        .checked("test user should be created");
    let server_id = "seafile-instance-server-id";
    cred_repo
        .create(&make_credential(user.id, "seafile", server_id))
        .await
        .checked("Seafile credential should be persisted");
    let stored = cred_repo
        .get_by_provider_and_server(user.id, "seafile", server_id)
        .await
        .checked("Seafile credential lookup should succeed")
        .checked("Seafile credential should exist");
    let ProviderCredential::Seafile {
        endpoint,
        username,
        token,
        version,
        features,
        library_passwords,
    } = stored.credential_data
    else {
        panic!("expected Seafile credential");
    };
    assert_eq!(endpoint, "https://seafile.example.com");
    assert_eq!(username, "alice@example.com");
    assert_eq!(token, "seafile-api-token");
    assert_eq!(version, "11.0.12");
    assert_eq!(features, ["seafile-basic"]);
    assert_eq!(
        library_passwords.get("repo-encrypted").map(String::as_str),
        Some("library-password")
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_truenas_credential_encrypted_postgres_round_trip() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let cred_repo = UserProviderCredentialRepository::new_with_encryption(pool, test_encryption());
    let user = user_repo
        .create(&make_user("truenas_credential_user"))
        .await
        .checked("test user should be created");
    let server_id = "truenas-instance-server-id";
    cred_repo
        .create(&make_credential(user.id, "truenas", server_id))
        .await
        .checked("TrueNAS credential should be persisted");
    let stored = cred_repo
        .get_by_provider_and_server(user.id, "truenas", server_id)
        .await
        .checked("TrueNAS credential lookup should succeed")
        .checked("TrueNAS credential should exist");
    let ProviderCredential::TrueNas {
        endpoint,
        api_key,
        hostname,
        version,
        system_product,
    } = stored.credential_data
    else {
        panic!("expected TrueNAS credential");
    };
    assert_eq!(endpoint, "https://truenas.example.com");
    assert_eq!(api_key, "truenas-api-key");
    assert_eq!(hostname, "truenas");
    assert_eq!(version, "25.10");
    assert_eq!(system_product, "TrueNAS SCALE");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_create_multiple_providers() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let cred_repo = UserProviderCredentialRepository::new_with_encryption(pool, test_encryption());

    let user = user_repo
        .create(&make_user("multi_cred_user"))
        .await
        .checked("test operation should succeed");

    let bilibili = make_credential(user.id, "bilibili", &bilibili_server_id());
    let alist = make_credential(user.id, "alist", "server1_hash");
    let emby = make_credential(user.id, "emby", "server2_hash");

    cred_repo
        .create(&bilibili)
        .await
        .checked("test operation should succeed");
    cred_repo
        .create(&alist)
        .await
        .checked("test operation should succeed");
    cred_repo
        .create(&emby)
        .await
        .checked("test operation should succeed");

    // List all
    let all = cred_repo
        .get_by_user(user.id)
        .await
        .checked("test operation should succeed");
    assert_eq!(all.len(), 3);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_by_id() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let cred_repo = UserProviderCredentialRepository::new_with_encryption(pool, test_encryption());

    let user = user_repo
        .create(&make_user("getbyid_user"))
        .await
        .checked("test operation should succeed");
    let bilibili_server_id = bilibili_server_id();
    let cred = make_credential(user.id, "bilibili", &bilibili_server_id);
    let cred = cred_repo
        .create(&cred)
        .await
        .checked("test operation should succeed");

    let found = cred_repo
        .get_by_id(cred.id)
        .await
        .checked("test operation should succeed");
    assert!(found.is_some());
    assert_eq!(found.checked("test operation should succeed").id, cred.id);

    // Non-existent ID
    let not_found = cred_repo
        .get_by_id(i64::MAX)
        .await
        .checked("test operation should succeed");
    assert!(not_found.is_none());
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_by_provider() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let cred_repo = UserProviderCredentialRepository::new_with_encryption(pool, test_encryption());

    let user = user_repo
        .create(&make_user("getbyprov_user"))
        .await
        .checked("test operation should succeed");

    let alist1 = make_credential(user.id, "alist", "server1");
    let alist2 = make_credential(user.id, "alist", "server2");
    let bilibili = make_credential(user.id, "bilibili", &bilibili_server_id());

    cred_repo
        .create(&alist1)
        .await
        .checked("test operation should succeed");
    cred_repo
        .create(&alist2)
        .await
        .checked("test operation should succeed");
    cred_repo
        .create(&bilibili)
        .await
        .checked("test operation should succeed");

    // Get only Alist
    let alist_creds = cred_repo
        .get_by_provider(user.id, "alist")
        .await
        .checked("test operation should succeed");
    assert_eq!(alist_creds.len(), 2);

    // Get only Bilibili
    let bilibili_creds = cred_repo
        .get_by_provider(user.id, "bilibili")
        .await
        .checked("test operation should succeed");
    assert_eq!(bilibili_creds.len(), 1);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_credential() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let cred_repo = UserProviderCredentialRepository::new_with_encryption(pool, test_encryption());

    let user = user_repo
        .create(&make_user("update_user"))
        .await
        .checked("test operation should succeed");
    let bilibili_server_id = bilibili_server_id();
    let mut cred = make_credential(user.id, "bilibili", &bilibili_server_id);
    cred = cred_repo
        .create(&cred)
        .await
        .checked("test operation should succeed");

    // Update credential data
    cred.credential_data = ProviderCredential::Bilibili {
        cookies: std::collections::HashMap::from([(
            "SESSDATA".to_string(),
            "new_session_value".to_string(),
        )]),
    };
    cred_repo
        .update(&cred)
        .await
        .checked("test operation should succeed");

    // Verify update
    let found = cred_repo
        .get_by_provider_and_server(user.id, "bilibili", &bilibili_server_id)
        .await
        .checked("test operation should succeed")
        .checked("test operation should succeed");

    let ProviderCredential::Bilibili { cookies } = found.credential_data else {
        panic!("expected bilibili credential");
    };
    assert_eq!(
        cookies.get("SESSDATA").map(String::as_str),
        Some("new_session_value")
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_credential_rejects_aad_binding_mismatch() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let cred_repo = UserProviderCredentialRepository::new_with_encryption(pool, test_encryption());

    let user = user_repo
        .create(&make_user("update_binding_user"))
        .await
        .checked("test user should be created");
    let server_id = bilibili_server_id();
    let stored = cred_repo
        .create(&make_credential(user.id, "bilibili", &server_id))
        .await
        .checked("credential should be created");

    let mut mismatched = stored;
    mismatched.server_id = "different-server".to_string();
    let error = cred_repo
        .update(&mismatched)
        .await
        .expect_err("binding mismatch should reject the update");
    assert!(matches!(error, synctv_core::Error::NotFound(_)));

    let unchanged = cred_repo
        .get_by_provider_and_server(user.id, "bilibili", &server_id)
        .await
        .checked("credential lookup should succeed")
        .checked("original credential should remain readable");
    assert!(matches!(
        unchanged.credential_data,
        ProviderCredential::Bilibili { ref cookies }
            if cookies.get("SESSDATA").map(String::as_str) == Some("test_session")
    ));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_credential_with_expiration() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let cred_repo = UserProviderCredentialRepository::new_with_encryption(pool, test_encryption());

    let user = user_repo
        .create(&make_user("expire_user"))
        .await
        .checked("test operation should succeed");
    let bilibili_server_id = bilibili_server_id();
    let mut cred = make_credential(user.id, "bilibili", &bilibili_server_id);
    cred = cred_repo
        .create(&cred)
        .await
        .checked("test operation should succeed");

    // Set expiration
    let expires = Utc::now() + Duration::hours(24);
    cred.expires_at = Some(expires);
    cred_repo
        .update(&cred)
        .await
        .checked("test operation should succeed");

    // Verify expiration
    let found = cred_repo
        .get_by_provider_and_server(user.id, "bilibili", &bilibili_server_id)
        .await
        .checked("test operation should succeed")
        .checked("test operation should succeed");

    assert!(found.expires_at.is_some());
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_credential() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let cred_repo = UserProviderCredentialRepository::new_with_encryption(pool, test_encryption());

    let user = user_repo
        .create(&make_user("delete_user"))
        .await
        .checked("test operation should succeed");
    let cred = make_credential(user.id, "bilibili", &bilibili_server_id());
    let cred = cred_repo
        .create(&cred)
        .await
        .checked("test operation should succeed");

    // Delete
    cred_repo
        .delete(cred.id)
        .await
        .checked("test operation should succeed");

    // Verify deleted
    let found = cred_repo
        .get_by_id(cred.id)
        .await
        .checked("test operation should succeed");
    assert!(found.is_none());
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_by_user_and_provider() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let cred_repo = UserProviderCredentialRepository::new_with_encryption(pool, test_encryption());

    let user = user_repo
        .create(&make_user("delprov_user"))
        .await
        .checked("test operation should succeed");

    let alist1 = make_credential(user.id, "alist", "server1");
    let alist2 = make_credential(user.id, "alist", "server2");
    let bilibili = make_credential(user.id, "bilibili", &bilibili_server_id());

    cred_repo
        .create(&alist1)
        .await
        .checked("test operation should succeed");
    cred_repo
        .create(&alist2)
        .await
        .checked("test operation should succeed");
    cred_repo
        .create(&bilibili)
        .await
        .checked("test operation should succeed");

    // Delete all Alist
    cred_repo
        .delete_by_user_and_provider(user.id, "alist")
        .await
        .checked("test operation should succeed");

    // Verify Alist deleted but Bilibili remains
    let alist = cred_repo
        .get_by_provider(user.id, "alist")
        .await
        .checked("test operation should succeed");
    assert!(alist.is_empty());

    let bilibili = cred_repo
        .get_by_provider(user.id, "bilibili")
        .await
        .checked("test operation should succeed");
    assert_eq!(bilibili.len(), 1);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_unique_constraint_user_provider_server() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let cred_repo = UserProviderCredentialRepository::new_with_encryption(pool, test_encryption());

    let user = user_repo
        .create(&make_user("unique_user"))
        .await
        .checked("test operation should succeed");

    let bilibili_server_id = bilibili_server_id();
    let cred1 = make_credential(user.id, "bilibili", &bilibili_server_id);
    cred_repo
        .create(&cred1)
        .await
        .checked("test operation should succeed");

    // Try to create duplicate (same user + provider + server_id)
    let cred2 = make_credential(user.id, "bilibili", &bilibili_server_id);
    let result = cred_repo.create(&cred2).await;

    assert!(result.is_err(), "Should fail due to unique constraint");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_same_provider_host_can_be_stored_for_different_instances() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let provider_repo =
        ProviderInstanceRepository::new_with_encryption(pool.clone(), test_encryption());
    let cred_repo = UserProviderCredentialRepository::new_with_encryption(pool, test_encryption());

    let user = user_repo
        .create(&make_user("instance_scoped_user"))
        .await
        .checked("test operation should succeed");

    provider_repo
        .create(&make_provider_instance("alist-main", &["alist"]))
        .await
        .checked("test operation should succeed");
    provider_repo
        .create(&make_provider_instance("alist-backup", &["alist"]))
        .await
        .checked("test operation should succeed");

    let server_main = AlistProvider::credential_server_id_for_instance(
        "https://alist.example.com",
        Some("alist-main"),
    );
    let server_backup = AlistProvider::credential_server_id_for_instance(
        "https://alist.example.com",
        Some("alist-backup"),
    );

    let main = make_credential_with_instance(user.id, "alist", &server_main, Some("alist-main"));
    let backup =
        make_credential_with_instance(user.id, "alist", &server_backup, Some("alist-backup"));

    cred_repo
        .create(&main)
        .await
        .checked("test operation should succeed");
    cred_repo
        .create(&backup)
        .await
        .checked("test operation should succeed");

    let all = cred_repo
        .get_by_provider(user.id, "alist")
        .await
        .checked("test operation should succeed");
    assert_eq!(all.len(), 2);

    let main_found = cred_repo
        .get_by_provider_and_server(user.id, "alist", &server_main)
        .await
        .checked("test operation should succeed")
        .checked("main credential should exist");
    let backup_found = cred_repo
        .get_by_provider_and_server(user.id, "alist", &server_backup)
        .await
        .checked("test operation should succeed")
        .checked("backup credential should exist");

    assert_eq!(
        main_found.provider_instance_name.as_deref(),
        Some("alist-main")
    );
    assert_eq!(
        backup_found.provider_instance_name.as_deref(),
        Some("alist-backup")
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_provider_instance_cascades_instance_credentials() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let provider_repo =
        ProviderInstanceRepository::new_with_encryption(pool.clone(), test_encryption());
    let cred_repo =
        UserProviderCredentialRepository::new_with_encryption(pool.clone(), test_encryption());

    let user = user_repo
        .create(&make_user("instance_delete_user"))
        .await
        .checked("test operation should succeed");
    provider_repo
        .create(&make_provider_instance("alist-delete-me", &["alist"]))
        .await
        .checked("test operation should succeed");

    let server_id = AlistProvider::credential_server_id_for_instance(
        "https://alist-delete.example.com",
        Some("alist-delete-me"),
    );
    let credential =
        make_credential_with_instance(user.id, "alist", &server_id, Some("alist-delete-me"));
    let credential = cred_repo
        .create(&credential)
        .await
        .checked("test operation should succeed");

    provider_repo
        .delete("alist-delete-me")
        .await
        .checked("test operation should succeed");

    let found = cred_repo
        .get_by_id(credential.id)
        .await
        .checked("test operation should succeed");
    assert!(
        found.is_none(),
        "deleting a provider instance must remove credentials bound to that instance"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_blank_provider_instance_name_is_normalized_to_null() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let cred_repo =
        UserProviderCredentialRepository::new_with_encryption(pool.clone(), test_encryption());

    let user = user_repo
        .create(&make_user("blank_instance_user"))
        .await
        .checked("test operation should succeed");

    let server_id =
        AlistProvider::credential_server_id_for_instance("https://alist.example.com", None);
    let credential = make_credential_with_instance(user.id, "alist", &server_id, Some("   "));

    let credential = cred_repo
        .create(&credential)
        .await
        .checked("test operation should succeed");

    let stored: Option<Option<String>> = sqlx::query_scalar!(
        "SELECT provider_instance_name FROM user_media_provider_credentials WHERE id = $1",
        credential.id
    )
    .fetch_optional(&pool)
    .await
    .checked("test operation should succeed");
    assert_eq!(stored, Some(None));

    let found = cred_repo
        .get_by_provider_and_server(user.id, "alist", &server_id)
        .await
        .checked("test operation should succeed")
        .checked("credential should exist");
    assert_eq!(found.provider_instance_name, None);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_upsert_by_user_provider_server_replaces_existing_credential_atomically() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let cred_repo =
        UserProviderCredentialRepository::new_with_encryption(pool.clone(), test_encryption());

    let user = user_repo
        .create(&make_user("credential_upsert_user"))
        .await
        .checked("test operation should succeed");
    let server_id = bilibili_server_id();
    let first = make_credential(user.id, "bilibili", &server_id);
    let first = cred_repo
        .upsert_by_user_provider_server(&first)
        .await
        .checked("test operation should succeed");

    let mut replacement = make_credential(user.id, "bilibili", &server_id);
    replacement.credential_data = ProviderCredential::Bilibili {
        cookies: std::collections::HashMap::from([(
            "SESSDATA".to_string(),
            "replacement_session".to_string(),
        )]),
    };
    cred_repo
        .upsert_by_user_provider_server(&replacement)
        .await
        .checked("test operation should succeed");

    let count: i64 = sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "count!" FROM user_media_provider_credentials WHERE user_id = $1 AND provider = $2 AND server_id = $3"#,
        user.id.as_i64(),
        provider_code(ProviderType::Bilibili),
        &server_id
    )
    .fetch_one(&pool)
    .await
    .checked("test operation should succeed");
    assert_eq!(count, 1);

    let found = cred_repo
        .get_by_provider_and_server(user.id, "bilibili", &server_id)
        .await
        .checked("test operation should succeed")
        .checked("upserted credential should exist");
    assert_eq!(
        found.id, first.id,
        "upsert should keep the stable credential id"
    );
    let ProviderCredential::Bilibili { cookies } = found.credential_data else {
        panic!("expected bilibili credential");
    };
    assert_eq!(
        cookies.get("SESSDATA").map(String::as_str),
        Some("replacement_session")
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_credentials_deleted_when_user_deleted() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let cred_repo = UserProviderCredentialRepository::new_with_encryption(pool, test_encryption());

    let user = user_repo
        .create(&make_user("cascade_user"))
        .await
        .checked("test operation should succeed");

    let cred = make_credential(user.id, "bilibili", &bilibili_server_id());
    let cred = cred_repo
        .create(&cred)
        .await
        .checked("test operation should succeed");

    // Delete user (soft delete first, then hard delete would cascade)
    // Note: Soft delete does NOT cascade delete credentials
    user_repo
        .delete(&user.id)
        .await
        .checked("test operation should succeed");

    // Credentials should still exist (soft delete)
    let found = cred_repo
        .get_by_id(cred.id)
        .await
        .checked("test operation should succeed");
    // The credentials remain in DB even after user soft-delete
    // because the FK constraint uses ON DELETE CASCADE for hard delete only
    assert!(found.is_some());
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_expired_credentials() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let cred_repo = UserProviderCredentialRepository::new_with_encryption(pool, test_encryption());

    let user = user_repo
        .create(&make_user("expire_del_user"))
        .await
        .checked("test operation should succeed");

    let mut expired = make_credential(user.id, "alist", "expired_server");
    expired.expires_at = Some(Utc::now() - Duration::hours(1));
    cred_repo
        .create(&expired)
        .await
        .checked("test operation should succeed");

    let mut valid = make_credential(user.id, "bilibili", &bilibili_server_id());
    valid.expires_at = Some(Utc::now() + Duration::hours(1));
    cred_repo
        .create(&valid)
        .await
        .checked("test operation should succeed");

    // Delete expired
    let deleted = cred_repo
        .delete_expired()
        .await
        .checked("test operation should succeed");
    assert_eq!(deleted, 1);

    // Verify only valid remains
    let all = cred_repo
        .get_by_user(user.id)
        .await
        .checked("test operation should succeed");
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].provider, "bilibili");
}
