use super::state_store::run_oauth_state_redis_op;
use super::*;
use crate::oauth2::OAuth2Authorization;
use crate::oauth2::Provider as OAuth2ProviderTrait;
use crate::test_helpers::failing_redis_runtime;
use crate::{Error, SharedStateMode, SharedStateProfile};
use async_trait::async_trait;

#[tokio::test]
async fn test_redis_oauth_state_store_accepts_trait_object_runtime() {
    let runtime = failing_redis_runtime();
    let store = RedisOAuthStateStore::from_runtime(runtime.clone(), "synctv:");

    assert!(
        store.runtime_ptr_eq(&runtime),
        "OAuth2 Redis store should retain the injected runtime object"
    );
}

#[test]
fn test_state_store_from_shared_state_profile_uses_memory_without_shared_runtime() {
    let profile = SharedStateProfile::from_runtime(None, "test:", false);

    let store = state_store_from_shared_state_profile(&profile)
        .expect("standalone mode should allow local OAuth2 state storage");

    assert!(
        !store.supports_cross_node_single_use(),
        "local store must not claim cross-node single-use guarantees"
    );
}

#[test]
fn test_state_store_from_shared_state_profile_requires_shared_runtime_in_cluster_mode() {
    let profile = SharedStateProfile::from_runtime(None, "test:", true);

    let Err(error) = state_store_from_shared_state_profile(&profile) else {
        panic!("cluster mode must reject local OAuth2 state storage");
    };

    assert!(
        error
            .to_string()
            .contains("distributed runtime requires shared single-use OAuth2 state storage"),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn test_state_store_from_shared_state_profile_accepts_trait_object_runtime() {
    let runtime = failing_redis_runtime();
    let profile =
        SharedStateProfile::new(SharedStateMode::SharedBestEffort, Some(runtime), "test:");

    let store = state_store_from_shared_state_profile(&profile)
        .expect("shared runtime profile should yield a distributed OAuth2 state store");

    assert!(
        store.supports_cross_node_single_use(),
        "shared store must claim cross-node single-use guarantees"
    );
}

#[derive(Clone)]
struct TestOAuth2Provider {
    auth_url: String,
    pkce_verifier: String,
    user_info: Option<crate::oauth2::OAuth2UserInfo>,
    exchange_error: Option<String>,
}

impl TestOAuth2Provider {
    fn new() -> Self {
        Self {
            auth_url: "https://provider.example.com/auth?client_id=test".to_string(),
            pkce_verifier: "test_pkce_verifier_abc123".to_string(),
            user_info: Some(crate::oauth2::OAuth2UserInfo {
                provider_user_id: "provider_user_42".to_string(),
                username: "testuser".to_string(),
                email: Some("test@example.com".to_string()),
                avatar: Some("https://avatar.example.com/42.png".to_string()),
                email_verified: true,
            }),
            exchange_error: None,
        }
    }

    fn with_exchange_error(mut self, err: &str) -> Self {
        self.exchange_error = Some(err.to_string());
        self
    }
}

#[async_trait]
impl OAuth2ProviderTrait for TestOAuth2Provider {
    fn provider_type(&self) -> &'static str {
        "test"
    }

    async fn new_auth_url(
        &self,
        state: &str,
        redirect_url: Option<&str>,
    ) -> Result<OAuth2Authorization> {
        let mut url = format!("{}&state={state}", self.auth_url);
        if let Some(redirect_url) = redirect_url {
            url.push_str("&redirect_uri=");
            url.push_str(redirect_url);
        }
        Ok(OAuth2Authorization::new(url, self.pkce_verifier.clone()))
    }

    async fn get_user_info(
        &self,
        _code: &str,
        _redirect_url: Option<&str>,
        _pkce_verifier: &str,
        _nonce: Option<&str>,
    ) -> Result<crate::oauth2::OAuth2UserInfo> {
        if let Some(ref err) = self.exchange_error {
            return Err(Error::Internal(err.clone()));
        }
        self.user_info
            .clone()
            .ok_or_else(|| Error::Internal("No user info configured in mock".to_string()))
    }
}

fn create_test_service() -> OAuth2Service {
    create_test_service_with_cluster_mode(false)
}

fn create_test_service_with_cluster_mode(cluster_mode: bool) -> OAuth2Service {
    create_test_service_with_runtime(cluster_mode, OAuth2ServiceRuntime::default())
}

fn create_test_service_with_runtime(
    cluster_mode: bool,
    runtime: OAuth2ServiceRuntime,
) -> OAuth2Service {
    let state_store = local_oauth_state_store();
    OAuth2Service::new_without_repository_for_tests(
        state_store,
        crate::oauth2::ProviderRegistry::new(),
        synctv_common::ssrf::SsrfGuard::strict_policy(),
        cluster_mode,
        runtime,
    )
    .expect("Failed to create OAuth2 service")
}

fn create_test_service_with_domains(domains: Vec<String>) -> OAuth2Service {
    create_test_service_with_runtime(
        false,
        OAuth2ServiceRuntime {
            allowed_redirect_domains: domains,
            ..OAuth2ServiceRuntime::default()
        },
    )
}

fn create_test_settings_registry(guard: &synctv_common::ssrf::SsrfGuard) -> Arc<SettingsRegistry> {
    Arc::new(SettingsRegistry::new_for_tests_with_ssrf_guard(guard))
}

#[test]
fn test_redirect_relative_path_rejected() {
    let result = OAuth2Service::validate_redirect_url_with_allowlist("/dashboard", &[]);
    assert!(result.is_err());
}

#[test]
fn test_redirect_relative_path_with_query_rejected() {
    let result = OAuth2Service::validate_redirect_url_with_allowlist("/rooms?sort=name", &[]);
    assert!(result.is_err());
}

#[test]
fn test_redirect_protocol_relative_url_rejected() {
    let result = OAuth2Service::validate_redirect_url_with_allowlist("//evil.com/steal", &[]);
    assert!(result.is_err());
}

#[test]
fn test_redirect_empty_url_rejected() {
    let result = OAuth2Service::validate_redirect_url_with_allowlist("", &[]);
    assert!(result.is_err());
}

#[test]
fn test_redirect_whitespace_only_rejected() {
    let result = OAuth2Service::validate_redirect_url_with_allowlist("   ", &[]);
    assert!(result.is_err());
}

#[test]
fn test_redirect_absolute_url_rejected_when_no_domains_configured() {
    let result =
        OAuth2Service::validate_redirect_url_with_allowlist("https://example.com/callback", &[]);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(matches!(&err, Error::InvalidInput(msg) if msg.contains("allowlist")));
}

#[test]
fn test_redirect_absolute_url_allowed_when_domain_matches() {
    let domains = vec!["example.com".to_string()];
    let result = OAuth2Service::validate_redirect_url_with_allowlist(
        "https://example.com/callback",
        &domains,
    );
    assert!(result.is_ok());
}

#[test]
fn test_redirect_absolute_url_allowed_for_subdomain() {
    let domains = vec!["example.com".to_string()];
    let result = OAuth2Service::validate_redirect_url_with_allowlist(
        "https://app.example.com/callback",
        &domains,
    );
    assert!(result.is_ok());
}

#[test]
fn test_redirect_http_url_rejected_for_non_loopback_host() {
    let domains = vec!["example.com".to_string()];
    let result = OAuth2Service::validate_redirect_url_with_allowlist(
        "http://example.com/callback",
        &domains,
    );
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(matches!(&err, Error::InvalidInput(msg) if msg.contains("HTTPS")));
}

#[test]
fn test_redirect_absolute_url_rejected_for_wrong_domain() {
    let domains = vec!["example.com".to_string()];
    let result =
        OAuth2Service::validate_redirect_url_with_allowlist("https://evil.com/callback", &domains);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(matches!(&err, Error::InvalidInput(msg) if msg.contains("not in the allowed")));
}

#[test]
fn test_redirect_javascript_scheme_rejected() {
    let domains = vec!["example.com".to_string()];
    let result =
        OAuth2Service::validate_redirect_url_with_allowlist("javascript:alert(1)", &domains);
    assert!(result.is_err());
}

#[test]
fn test_redirect_ftp_scheme_rejected() {
    let domains = vec!["example.com".to_string()];
    let result =
        OAuth2Service::validate_redirect_url_with_allowlist("ftp://example.com/file", &domains);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(matches!(&err, Error::InvalidInput(msg) if msg.contains("Invalid URL scheme")));
}

#[test]
fn test_redirect_url_with_credentials_rejected() {
    let domains = vec!["example.com".to_string()];
    let result = OAuth2Service::validate_redirect_url_with_allowlist(
        "https://user:pass@example.com/callback",
        &domains,
    );
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(matches!(&err, Error::InvalidInput(msg) if msg.contains("credentials")));
}

#[test]
fn test_redirect_malformed_url_rejected() {
    let domains = vec!["example.com".to_string()];
    let result =
        OAuth2Service::validate_redirect_url_with_allowlist("not a valid url at all", &domains);
    assert!(result.is_err());
}

#[test]
fn test_redirect_tld_only_domain_rejected() {
    // Adding "com" to allowlist should NOT allow all.com domains
    let domains = vec!["com".to_string()];
    let result =
        OAuth2Service::validate_redirect_url_with_allowlist("https://evil.com/callback", &domains);
    assert!(result.is_err());
}

#[test]
fn test_redirect_deep_subdomain_rejected() {
    // Only single-level subdomains are allowed
    let domains = vec!["example.com".to_string()];
    let result = OAuth2Service::validate_redirect_url_with_allowlist(
        "https://deep.sub.example.com/callback",
        &domains,
    );
    assert!(result.is_err());
}

#[test]
fn test_redirect_native_custom_scheme_rejected_without_allowlist() {
    let result = OAuth2Service::validate_redirect_url_with_allowlist("native-app://callback", &[]);
    assert!(result.is_err());
}

#[test]
fn test_redirect_native_custom_scheme_rejected_even_with_allowlist() {
    let domains = vec!["github.io".to_string()];
    let result =
        OAuth2Service::validate_redirect_url_with_allowlist("native-app://callback", &domains);
    assert!(result.is_err());
}

#[test]
fn test_redirect_dangerous_non_http_schemes_rejected() {
    for url in [
        "javascript:alert(1)",
        "data:text/html,hello",
        "file:///tmp/callback",
        "ftp://example.com/callback",
    ] {
        let result = OAuth2Service::validate_redirect_url_with_allowlist(url, &[]);
        assert!(result.is_err(), "{url} must be rejected");
    }
}

#[test]
fn test_redirect_loopback_url_allowed_without_domain_allowlist() {
    let localhost = OAuth2Service::validate_redirect_url_with_allowlist(
        "http://127.0.0.1:34567/oauth/callback",
        &[],
    );
    assert!(localhost.is_ok());

    let hostname =
        OAuth2Service::validate_redirect_url_with_allowlist("http://localhost:8080/cb", &[]);
    assert!(hostname.is_ok());
}

// Tests: State Management (in-memory, no Redis required)

#[tokio::test]
async fn test_store_and_consume_state() {
    let service = create_test_service();
    let state = OAuth2State {
        instance_name: "github".to_string(),
        redirect_url: Some("http://127.0.0.1:34567/dashboard".to_string()),
        created_at: chrono::Utc::now(),
        bind_user_id: None,
        pkce_verifier: "verifier123".to_string(),
        nonce: None,
    };

    service.store_state("token_abc", &state).await.unwrap();
    let retrieved = service.consume_state("token_abc").await.unwrap();

    assert_eq!(retrieved.instance_name, "github");
    assert_eq!(retrieved.pkce_verifier, "verifier123");
    assert_eq!(
        retrieved.redirect_url.as_deref(),
        Some("http://127.0.0.1:34567/dashboard")
    );
    assert!(retrieved.bind_user_id.is_none());
}

#[tokio::test]
async fn test_state_single_use_consumed_on_first_retrieval() {
    let service = create_test_service();
    let state = OAuth2State {
        instance_name: "google".to_string(),
        redirect_url: None,
        created_at: chrono::Utc::now(),
        bind_user_id: None,
        pkce_verifier: "v".to_string(),
        nonce: None,
    };

    service.store_state("token_once", &state).await.unwrap();

    // First consume succeeds
    let result = service.consume_state("token_once").await;
    assert!(result.is_ok());

    // Second consume fails (state was removed)
    let result = service.consume_state("token_once").await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        matches!(&err, Error::Authentication(msg) if msg.contains("Invalid or expired")),
        "Expected authentication error for replayed state, got: {err}"
    );
}

#[tokio::test]
async fn test_state_invalid_token_rejected() {
    let service = create_test_service();

    let result = service.consume_state("nonexistent_token").await;
    assert!(result.is_err());
    assert!(
        matches!(result.unwrap_err(), Error::Authentication(msg) if msg.contains("Invalid or expired"))
    );
}

#[tokio::test]
async fn test_state_preserves_bind_user_id() {
    let service = create_test_service();
    let user_id = UserId::expect_positive(93_001);
    let state = OAuth2State {
        instance_name: "logto".to_string(),
        redirect_url: None,
        created_at: chrono::Utc::now(),
        bind_user_id: Some(user_id),
        pkce_verifier: "bind_verifier".to_string(),
        nonce: None,
    };

    service.store_state("bind_token", &state).await.unwrap();
    let retrieved = service.consume_state("bind_token").await.unwrap();

    assert_eq!(
        retrieved.bind_user_id.as_ref().unwrap().to_string(),
        "93001"
    );
}

#[tokio::test]
async fn test_verify_state_consumes_token() {
    let service = create_test_service();
    let state = OAuth2State {
        instance_name: "oidc".to_string(),
        redirect_url: None,
        created_at: chrono::Utc::now(),
        bind_user_id: None,
        pkce_verifier: "pkce_v".to_string(),
        nonce: None,
    };

    service.store_state("verify_tok", &state).await.unwrap();

    // verify_state delegates to consume_state
    let result = service.verify_state("verify_tok").await;
    assert!(result.is_ok());

    // Replay fails
    let result = service.verify_state("verify_tok").await;
    assert!(result.is_err());
}

// Tests: Provider Registration and Listing

#[tokio::test]
async fn test_register_and_list_providers() {
    let service = create_test_service();

    // Initially empty
    let providers = service.list_available_instances().await.unwrap();
    assert!(providers.is_empty());

    // Register a mock provider
    service
        .register_provider(
            "github".to_string(),
            OAuth2Provider::GitHub,
            Box::new(TestOAuth2Provider::new()),
        )
        .await;

    let providers = service.list_available_instances().await.unwrap();
    assert_eq!(providers.len(), 1);
    assert_eq!(providers[0].0, "github");
    assert_eq!(providers[0].1, OAuth2Provider::GitHub);
}

#[tokio::test]
async fn test_signup_policy_for_registered_provider() {
    let service = create_test_service();

    service
        .register_provider(
            "github".to_string(),
            OAuth2Provider::GitHub,
            Box::new(TestOAuth2Provider::new()),
        )
        .await;

    let policy = service
        .signup_policy_for("github")
        .await
        .expect("registered provider should have a signup policy");
    assert_eq!(policy, OAuth2SignupPolicy::default());
}

#[tokio::test]
async fn test_signup_policy_for_missing_provider_returns_error() {
    let service = create_test_service();

    let error = service
        .signup_policy_for("missing")
        .await
        .expect_err("missing provider should fail");
    assert!(error
        .to_string()
        .contains("OAuth2 provider instance not found: missing"));
}

#[tokio::test]
async fn test_list_available_instances_uses_runtime_ssrf_policy_for_dynamic_oidc() {
    let guard = synctv_common::ssrf::SsrfGuard::builder()
        .allow_private_network_targets(true)
        .build();
    let registry = create_test_settings_registry(&guard);
    let configs: crate::service::OAuth2ProviderConfigs = r#"{"casdoor_oidc":{"type":"oidc","enable_signup":true,"config":{"client_id":"id","client_secret":"secret","redirect_url":"http://127.0.0.1:18081/oauth/callback","issuer":"http://127.0.0.1:18000"}}}"#
        .parse()
        .expect("test OAuth2 provider config should parse");
    registry
        .oauth2_providers
        .set_for_test(&configs)
        .expect("test settings seed should validate");

    let service = OAuth2Service::new_without_repository_for_tests(
        local_oauth_state_store(),
        crate::oauth2::providers::provider_registry(guard),
        synctv_common::ssrf::SsrfGuard::builder()
            .allow_private_network_targets(true)
            .build(),
        false,
        OAuth2ServiceRuntime {
            settings_registry: Some(registry),
            ..OAuth2ServiceRuntime::default()
        },
    )
    .expect("OAuth2 service should be created");

    let providers = service
        .list_available_instances()
        .await
        .expect("runtime SSRF policy should allow local Casdoor OIDC issuer");
    assert_eq!(providers.len(), 1);
    assert_eq!(providers[0].0, "casdoor_oidc");
    assert_eq!(providers[0].1, OAuth2Provider::Oidc);
}

#[tokio::test]
async fn test_register_multiple_providers() {
    let service = create_test_service();

    service
        .register_provider(
            "github".to_string(),
            OAuth2Provider::GitHub,
            Box::new(TestOAuth2Provider::new()),
        )
        .await;
    service
        .register_provider(
            "logto1".to_string(),
            OAuth2Provider::Logto,
            Box::new(TestOAuth2Provider::new()),
        )
        .await;
    service
        .register_provider(
            "google".to_string(),
            OAuth2Provider::Google,
            Box::new(TestOAuth2Provider::new()),
        )
        .await;

    let providers = service.list_available_instances().await.unwrap();
    assert_eq!(providers.len(), 3);

    let names: Vec<&str> = providers.iter().map(|(n, _, _)| n.as_str()).collect();
    assert!(names.contains(&"github"));
    assert!(names.contains(&"logto1"));
    assert!(names.contains(&"google"));
}

#[tokio::test]
async fn test_register_provider_replaces_existing() {
    let service = create_test_service();

    service
        .register_provider(
            "github".to_string(),
            OAuth2Provider::GitHub,
            Box::new(TestOAuth2Provider::new()),
        )
        .await;

    // Re-register with same name but different type
    service
        .register_provider(
            "github".to_string(),
            OAuth2Provider::Oidc,
            Box::new(TestOAuth2Provider::new()),
        )
        .await;

    let providers = service.list_available_instances().await.unwrap();
    assert_eq!(providers.len(), 1);
    assert_eq!(providers[0].1, OAuth2Provider::Oidc);
}

// Tests: Authorization URL Generation with PKCE

#[tokio::test]
async fn test_get_authorization_url_success() {
    let service = create_test_service();
    service
        .register_provider(
            "github".to_string(),
            OAuth2Provider::GitHub,
            Box::new(TestOAuth2Provider::new()),
        )
        .await;

    let (auth_url, state_token) = service.get_authorization_url("github", None).await.unwrap();

    // Auth URL should contain the mock base URL and the state parameter
    assert!(auth_url.contains("https://provider.example.com/auth"));
    assert!(auth_url.contains("state="));

    // State token should be a 32-char shared base62 token
    assert_eq!(state_token.len(), 32);

    // State should be stored and consumable
    let state = service.verify_state(&state_token).await.unwrap();
    assert_eq!(state.instance_name, "github");
    assert_eq!(state.pkce_verifier, "test_pkce_verifier_abc123");
    assert!(state.redirect_url.is_none());
    assert!(state.bind_user_id.is_none());
}

#[tokio::test]
async fn test_get_authorization_url_with_redirect() {
    let service = create_test_service();
    service
        .register_provider(
            "github".to_string(),
            OAuth2Provider::GitHub,
            Box::new(TestOAuth2Provider::new()),
        )
        .await;

    let err = service
        .get_authorization_url("github", Some("/rooms/123".to_string()))
        .await
        .expect_err("relative redirect URL must be rejected");
    assert!(matches!(err, Error::InvalidInput(_)));
}

#[tokio::test]
async fn test_get_authorization_url_rejects_invalid_redirect() {
    let service = create_test_service();
    service
        .register_provider(
            "github".to_string(),
            OAuth2Provider::GitHub,
            Box::new(TestOAuth2Provider::new()),
        )
        .await;

    // Absolute URL with no allowed domains should be rejected
    let result = service
        .get_authorization_url("github", Some("https://evil.com/steal".to_string()))
        .await;
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), Error::InvalidInput(_)));
}

#[tokio::test]
async fn test_get_authorization_url_rejects_protocol_relative_redirect() {
    let service = create_test_service();
    service
        .register_provider(
            "github".to_string(),
            OAuth2Provider::GitHub,
            Box::new(TestOAuth2Provider::new()),
        )
        .await;

    let result = service
        .get_authorization_url("github", Some("//evil.com/steal".to_string()))
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_get_authorization_url_unknown_provider() {
    let service = create_test_service();

    let result = service.get_authorization_url("nonexistent", None).await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        matches!(&err, Error::InvalidInput(msg) if msg.contains("not found")),
        "Expected provider not found error, got: {err}"
    );
}

#[tokio::test]
async fn test_get_authorization_url_with_allowed_redirect_domains() {
    let service = create_test_service_with_domains(vec!["myapp.com".to_string()]);
    service
        .register_provider(
            "github".to_string(),
            OAuth2Provider::GitHub,
            Box::new(TestOAuth2Provider::new()),
        )
        .await;

    // Allowed domain works
    let result = service
        .get_authorization_url("github", Some("https://myapp.com/callback".to_string()))
        .await;
    assert!(result.is_ok());

    // Subdomain also works
    let result = service
        .get_authorization_url("github", Some("https://auth.myapp.com/cb".to_string()))
        .await;
    assert!(result.is_ok());

    // Disallowed domain rejected
    let result = service
        .get_authorization_url("github", Some("https://evil.com/steal".to_string()))
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_get_authorization_url_accepts_loopback_native_client_redirects() {
    let service = create_test_service_with_domains(vec!["github.io".to_string()]);
    service
        .register_provider(
            "github".to_string(),
            OAuth2Provider::GitHub,
            Box::new(TestOAuth2Provider::new()),
        )
        .await;

    let (_, loopback_state_token) = service
        .get_authorization_url(
            "github",
            Some("http://127.0.0.1:34567/oauth/callback".to_string()),
        )
        .await
        .expect("native loopback redirects should not require domain allowlist");
    let loopback_state = service.verify_state(&loopback_state_token).await.unwrap();
    assert_eq!(
        loopback_state.redirect_url.as_deref(),
        Some("http://127.0.0.1:34567/oauth/callback")
    );
}

// Tests: Authorization URL with User Binding (PKCE)

#[tokio::test]
async fn test_get_authorization_url_with_user_stores_user_id() {
    let service = create_test_service();
    service
        .register_provider(
            "logto".to_string(),
            OAuth2Provider::Logto,
            Box::new(TestOAuth2Provider::new()),
        )
        .await;

    let user_id = UserId::expect_positive(93_002);
    let (auth_url, state_token) = service
        .get_authorization_url_with_user("logto", None, Some(user_id))
        .await
        .unwrap();

    assert!(auth_url.contains("https://provider.example.com/auth"));

    let state = service.verify_state(&state_token).await.unwrap();
    assert_eq!(state.instance_name, "logto");
    assert_eq!(state.bind_user_id.as_ref().unwrap().to_string(), "93002");
    assert_eq!(state.pkce_verifier, "test_pkce_verifier_abc123");
}

#[tokio::test]
async fn test_get_authorization_url_with_user_none_user_id() {
    let service = create_test_service();
    service
        .register_provider(
            "github".to_string(),
            OAuth2Provider::GitHub,
            Box::new(TestOAuth2Provider::new()),
        )
        .await;

    let (_, state_token) = service
        .get_authorization_url_with_user("github", None, None)
        .await
        .unwrap();

    let state = service.verify_state(&state_token).await.unwrap();
    assert!(state.bind_user_id.is_none());
}

#[tokio::test]
async fn test_get_authorization_url_with_user_rejects_bad_redirect() {
    let service = create_test_service();
    service
        .register_provider(
            "github".to_string(),
            OAuth2Provider::GitHub,
            Box::new(TestOAuth2Provider::new()),
        )
        .await;

    let result = service
        .get_authorization_url_with_user("github", Some("//evil.com".to_string()), None)
        .await;
    assert!(result.is_err());
}

// Tests: Code Exchange for User Info

#[tokio::test]
async fn test_exchange_code_for_user_info_success() {
    let service = create_test_service();
    service
        .register_provider(
            "github".to_string(),
            OAuth2Provider::GitHub,
            Box::new(TestOAuth2Provider::new()),
        )
        .await;

    let user_info = service
        .exchange_code_for_user_info("github", "auth_code_123", "pkce_verifier_abc")
        .await
        .unwrap();

    assert_eq!(user_info.provider_user_id, "provider_user_42");
    assert_eq!(user_info.username, "testuser");
    assert_eq!(user_info.email.as_deref(), Some("test@example.com"));
    assert_eq!(
        user_info.avatar.as_deref(),
        Some("https://avatar.example.com/42.png")
    );
    assert_eq!(user_info.provider, OAuth2Provider::GitHub);
    assert_eq!(user_info.provider_instance_name, "github");
    assert!(user_info.provider_issuer.is_none());
}

#[tokio::test]
async fn test_exchange_code_unknown_provider() {
    let service = create_test_service();

    let result = service
        .exchange_code_for_user_info("nonexistent", "code", "verifier")
        .await;
    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        Error::InvalidInput(msg) if msg.contains("not found")
    ));
}

#[tokio::test]
async fn test_exchange_code_provider_returns_error() {
    let service = create_test_service();
    let failing_provider =
        TestOAuth2Provider::new().with_exchange_error("token exchange failed: invalid_grant");

    service
        .register_provider(
            "failing".to_string(),
            OAuth2Provider::Oidc,
            Box::new(failing_provider),
        )
        .await;

    let result = service
        .exchange_code_for_user_info("failing", "bad_code", "verifier")
        .await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        matches!(&err, Error::Internal(msg) if msg.contains("invalid_grant")),
        "Expected internal error with invalid_grant, got: {err}"
    );
}

// Tests: Full Authorization Flow (URL -> State -> Exchange)

#[tokio::test]
async fn test_full_oauth2_login_flow() {
    let service = create_test_service();
    service
        .register_provider(
            "github".to_string(),
            OAuth2Provider::GitHub,
            Box::new(TestOAuth2Provider::new()),
        )
        .await;

    // Step 1: Generate authorization URL
    let err = service
        .get_authorization_url("github", Some("/dashboard".to_string()))
        .await
        .expect_err("relative redirect URL must be rejected");
    assert!(matches!(err, Error::InvalidInput(_)));
    let (auth_url, state_token) = service
        .get_authorization_url(
            "github",
            Some("http://127.0.0.1:34567/dashboard".to_string()),
        )
        .await
        .unwrap();
    assert!(auth_url.contains("state="));

    // Step 2: Verify state (simulating callback)
    let state = service.verify_state(&state_token).await.unwrap();
    assert_eq!(state.instance_name, "github");
    assert_eq!(
        state.redirect_url.as_deref(),
        Some("http://127.0.0.1:34567/dashboard")
    );

    // Step 3: Exchange code with PKCE verifier from stored state
    let user_info = service
        .exchange_code_for_user_info("github", "callback_code", &state.pkce_verifier)
        .await
        .unwrap();
    assert_eq!(user_info.username, "testuser");
    assert_eq!(user_info.provider, OAuth2Provider::GitHub);
    assert_eq!(user_info.provider_instance_name, "github");

    // Step 4: State cannot be replayed
    let replay = service.verify_state(&state_token).await;
    assert!(replay.is_err());
}

#[tokio::test]
async fn test_full_oauth2_bind_flow() {
    let service = create_test_service();
    service
        .register_provider(
            "logto".to_string(),
            OAuth2Provider::Logto,
            Box::new(TestOAuth2Provider::new()),
        )
        .await;

    let user_id = UserId::expect_positive(93_003);

    // Step 1: Generate auth URL with user binding
    let (_, state_token) = service
        .get_authorization_url_with_user("logto", None, Some(user_id))
        .await
        .unwrap();

    // Step 2: Verify state carries user ID
    let state = service.verify_state(&state_token).await.unwrap();
    assert_eq!(state.bind_user_id.as_ref().unwrap().to_string(), "93003");
    assert_eq!(state.instance_name, "logto");
}

// Tests: Service Configuration

#[tokio::test]
async fn test_state_store_is_abstracted() {
    // OAuth2Service takes Arc<dyn OAuthStateStore>, not a concrete Redis type.
    // This verifies the abstraction compiles with the in-memory implementation.
    let _service = create_test_service();
}

#[tokio::test]
async fn test_allowed_redirect_domains_are_constructor_configured() {
    let service =
        create_test_service_with_domains(vec!["example.com".to_string(), "myapp.io".to_string()]);

    service
        .register_provider(
            "github".to_string(),
            OAuth2Provider::GitHub,
            Box::new(TestOAuth2Provider::new()),
        )
        .await;

    // Allowed domain
    let result = service
        .get_authorization_url("github", Some("https://example.com/cb".to_string()))
        .await;
    assert!(result.is_ok());

    // Another allowed domain
    let result = service
        .get_authorization_url("github", Some("https://myapp.io/cb".to_string()))
        .await;
    assert!(result.is_ok());

    // Non-allowed domain
    let result = service
        .get_authorization_url("github", Some("https://other.com/cb".to_string()))
        .await;
    assert!(result.is_err());
}

// Tests: OAuth2State serialization (used for storage path)

#[test]
fn test_oauth2_state_serialization_roundtrip() {
    let state = OAuth2State {
        instance_name: "github".to_string(),
        redirect_url: Some("http://127.0.0.1:34567/dashboard".to_string()),
        created_at: chrono::Utc::now(),
        bind_user_id: Some(UserId::expect_positive(93_004)),
        pkce_verifier: "S256_challenge_verifier".to_string(),
        nonce: Some("oidc_nonce_123".to_string()),
    };

    let json = serde_json::to_string(&state).unwrap();
    let deserialized: OAuth2State = serde_json::from_str(&json).unwrap();

    assert_eq!(deserialized.instance_name, state.instance_name);
    assert_eq!(deserialized.redirect_url, state.redirect_url);
    assert_eq!(deserialized.pkce_verifier, state.pkce_verifier);
    assert_eq!(deserialized.nonce, state.nonce);
    assert_eq!(
        deserialized.bind_user_id.as_ref().unwrap().to_string(),
        "93004"
    );
}

#[test]
fn test_oauth2_state_serialization_none_fields() {
    let state = OAuth2State {
        instance_name: "oidc".to_string(),
        redirect_url: None,
        created_at: chrono::Utc::now(),
        bind_user_id: None,
        pkce_verifier: "v".to_string(),
        nonce: None,
    };

    let json = serde_json::to_string(&state).unwrap();
    let deserialized: OAuth2State = serde_json::from_str(&json).unwrap();

    assert!(deserialized.redirect_url.is_none());
    assert!(deserialized.bind_user_id.is_none());
}

// Tests: Concurrent State Operations

#[tokio::test]
async fn test_multiple_concurrent_states() {
    let service = create_test_service();

    // Store multiple states
    for i in 0..10 {
        let state = OAuth2State {
            instance_name: format!("provider_{i}"),
            redirect_url: None,
            created_at: chrono::Utc::now(),
            bind_user_id: None,
            pkce_verifier: format!("verifier_{i}"),
            nonce: None,
        };
        service
            .store_state(&format!("token_{i}"), &state)
            .await
            .unwrap();
    }

    // Each state should be independently consumable
    for i in 0..10 {
        let state = service.consume_state(&format!("token_{i}")).await.unwrap();
        assert_eq!(state.instance_name, format!("provider_{i}"));
        assert_eq!(state.pkce_verifier, format!("verifier_{i}"));
    }

    // All consumed, none should remain
    for i in 0..10 {
        let result = service.consume_state(&format!("token_{i}")).await;
        assert!(result.is_err());
    }
}

// Tests: PKCE Verifier Integrity

#[tokio::test]
async fn test_pkce_verifier_preserved_through_state_lifecycle() {
    let service = create_test_service();
    let provider = TestOAuth2Provider {
        auth_url: "https://auth.test/authorize".to_string(),
        pkce_verifier: "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk".to_string(),
        user_info: Some(crate::oauth2::OAuth2UserInfo {
            provider_user_id: "94001".to_string(),
            username: "user1".to_string(),
            email: None,
            avatar: None,
            email_verified: false,
        }),
        exchange_error: None,
    };

    service
        .register_provider(
            "test_pkce".to_string(),
            OAuth2Provider::Oidc,
            Box::new(provider),
        )
        .await;

    // Generate URL -- PKCE verifier should be stored in state
    let (_, state_token) = service
        .get_authorization_url("test_pkce", None)
        .await
        .unwrap();

    // Retrieve state and check PKCE verifier is intact
    let state = service.verify_state(&state_token).await.unwrap();
    assert_eq!(
        state.pkce_verifier,
        "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"
    );
}

#[tokio::test]
async fn test_each_auth_url_gets_unique_state_token() {
    let service = create_test_service();
    service
        .register_provider(
            "github".to_string(),
            OAuth2Provider::GitHub,
            Box::new(TestOAuth2Provider::new()),
        )
        .await;

    let (_, token1) = service.get_authorization_url("github", None).await.unwrap();
    let (_, token2) = service.get_authorization_url("github", None).await.unwrap();

    assert_ne!(
        token1, token2,
        "Each authorization request must get a unique state token"
    );
}

// Tests: OAuth2 Concurrent State Consumption (only one succeeds)

#[tokio::test]
async fn test_concurrent_state_consumption_only_first_succeeds() {
    let service = Arc::new(create_test_service());
    let state = OAuth2State {
        instance_name: "github".to_string(),
        redirect_url: None,
        created_at: chrono::Utc::now(),
        bind_user_id: None,
        pkce_verifier: "concurrent_verifier".to_string(),
        nonce: None,
    };

    service
        .store_state("concurrent_token", &state)
        .await
        .unwrap();

    // Spawn multiple concurrent consumers
    let mut handles = Vec::new();
    for _ in 0..20 {
        let svc = service.clone();
        handles.push(tokio::spawn(async move {
            svc.consume_state("concurrent_token").await
        }));
    }

    let mut success_count = 0;
    let mut failure_count = 0;
    for h in handles {
        match h.await.unwrap() {
            Ok(state) => {
                assert_eq!(state.pkce_verifier, "concurrent_verifier");
                success_count += 1;
            }
            Err(_) => {
                failure_count += 1;
            }
        }
    }

    // With the Mutex-based store, exactly one consumer must succeed.
    assert_eq!(success_count, 1, "Exactly one consumer must succeed");
    assert_eq!(failure_count, 19, "All other consumers must fail");

    // Token is fully consumed -- no further consumption should succeed
    let replay = service.consume_state("concurrent_token").await;
    assert!(replay.is_err(), "Token should be fully consumed");
}

#[tokio::test]
async fn test_concurrent_verify_state_only_first_succeeds() {
    let service = Arc::new(create_test_service());
    service
        .register_provider(
            "github".to_string(),
            OAuth2Provider::GitHub,
            Box::new(TestOAuth2Provider::new()),
        )
        .await;

    // Generate an auth URL (stores state internally)
    let (_, state_token) = service.get_authorization_url("github", None).await.unwrap();

    // Spawn concurrent verify_state attempts
    let mut handles = Vec::new();
    for _ in 0..10 {
        let svc = service.clone();
        let tok = state_token.clone();
        handles.push(tokio::spawn(async move { svc.verify_state(&tok).await }));
    }

    let mut success_count = 0;
    for h in handles {
        if h.await.unwrap().is_ok() {
            success_count += 1;
        }
    }

    // Exactly one should succeed with the Mutex-based store
    assert_eq!(success_count, 1, "Exactly one verify must succeed");

    // No further verification should succeed
    let replay = service.verify_state(&state_token).await;
    assert!(replay.is_err(), "State token should be consumed");
}

// Tests: State Isolation Between Tokens

#[tokio::test]
async fn test_consuming_one_state_does_not_affect_others() {
    let service = create_test_service();

    for i in 0..5 {
        let state = OAuth2State {
            instance_name: format!("provider_{i}"),
            redirect_url: None,
            created_at: chrono::Utc::now(),
            bind_user_id: None,
            pkce_verifier: format!("verifier_{i}"),
            nonce: None,
        };
        service
            .store_state(&format!("isolated_token_{i}"), &state)
            .await
            .unwrap();
    }

    // Consume token 2
    let consumed = service.consume_state("isolated_token_2").await.unwrap();
    assert_eq!(consumed.instance_name, "provider_2");

    // Other tokens should still be available
    for i in [0, 1, 3, 4] {
        let state = service
            .consume_state(&format!("isolated_token_{i}"))
            .await
            .unwrap();
        assert_eq!(state.instance_name, format!("provider_{i}"));
    }

    // Token 2 is consumed, should fail
    let result = service.consume_state("isolated_token_2").await;
    assert!(result.is_err());
}

// Tests: CSRF Protection - Defense in Depth

/// Test that state tokens with expired `created_at` timestamps are rejected
/// even if they somehow persist in the store (defense-in-depth).
#[tokio::test]
async fn test_state_expired_created_at_rejected() {
    let service = create_test_service();

    // Create a state with a created_at timestamp that is already expired
    // (6 minutes ago, which exceeds the 5-minute TTL)
    let expired_time = chrono::Utc::now() - chrono::Duration::seconds(360);
    let state = OAuth2State {
        instance_name: "github".to_string(),
        redirect_url: None,
        created_at: expired_time,
        bind_user_id: None,
        pkce_verifier: "expired_verifier".to_string(),
        nonce: None,
    };

    // Store the state directly (bypassing normal TTL enforcement)
    service.store_state("expired_token", &state).await.unwrap();

    // Consumption should fail due to created_at check, even though token exists
    let result = service.consume_state("expired_token").await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        matches!(&err, Error::Authentication(msg) if msg.contains("Invalid or expired")),
        "Expected authentication error for expired state, got: {err}"
    );
}

/// Test that state tokens just within the TTL are accepted
#[tokio::test]
async fn test_state_within_ttl_accepted() {
    let service = create_test_service();

    // Create a state with a created_at timestamp that is just within TTL
    // (4 minutes ago, which is less than the 5-minute TTL)
    let within_ttl_time = chrono::Utc::now() - chrono::Duration::seconds(240);
    let state = OAuth2State {
        instance_name: "github".to_string(),
        redirect_url: None,
        created_at: within_ttl_time,
        bind_user_id: None,
        pkce_verifier: "valid_verifier".to_string(),
        nonce: None,
    };

    service
        .store_state("within_ttl_token", &state)
        .await
        .unwrap();

    // Consumption should succeed
    let result = service.consume_state("within_ttl_token").await;
    assert!(result.is_ok());
    let retrieved = result.unwrap();
    assert_eq!(retrieved.pkce_verifier, "valid_verifier");
}

/// Test that state tokens at the exact TTL boundary are handled correctly
#[tokio::test]
async fn test_state_at_ttl_boundary() {
    let service = create_test_service();

    // Create a state just past the TTL boundary (TTL + 1 second ago)
    // This ensures the test is deterministic regardless of execution timing
    let past_boundary_time =
        chrono::Utc::now() - chrono::Duration::seconds(OAUTH2_STATE_TTL_SECONDS_I64 + 1);
    let state = OAuth2State {
        instance_name: "github".to_string(),
        redirect_url: None,
        created_at: past_boundary_time,
        bind_user_id: None,
        pkce_verifier: "boundary_verifier".to_string(),
        nonce: None,
    };

    service.store_state("boundary_token", &state).await.unwrap();

    // Past TTL seconds, the state should be rejected (> TTL)
    let result = service.consume_state("boundary_token").await;
    assert!(result.is_err());
}

/// Test that `verify_state` includes the `created_at` expiry check
#[tokio::test]
async fn test_verify_state_checks_created_at_expiry() {
    let service = create_test_service();
    service
        .register_provider(
            "github".to_string(),
            OAuth2Provider::GitHub,
            Box::new(TestOAuth2Provider::new()),
        )
        .await;

    // Manually create an expired state
    let expired_time = chrono::Utc::now() - chrono::Duration::seconds(360);
    let state = OAuth2State {
        instance_name: "github".to_string(),
        redirect_url: None,
        created_at: expired_time,
        bind_user_id: None,
        pkce_verifier: "expired".to_string(),
        nonce: None,
    };

    service
        .store_state("verify_expired_token", &state)
        .await
        .unwrap();

    // verify_state should also reject expired tokens
    let result = service.verify_state("verify_expired_token").await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        matches!(&err, Error::Authentication(msg) if msg.contains("Invalid or expired")),
        "Expected authentication error for expired state in verify_state, got: {err}"
    );
}

/// Test that provider mismatch is detected during code exchange
#[tokio::test]
async fn test_csrf_protection_provider_mismatch_detected() {
    let service = create_test_service();
    service
        .register_provider(
            "github".to_string(),
            OAuth2Provider::GitHub,
            Box::new(TestOAuth2Provider::new()),
        )
        .await;
    service
        .register_provider(
            "google".to_string(),
            OAuth2Provider::Google,
            Box::new(TestOAuth2Provider::new()),
        )
        .await;

    // Generate state for github
    let (_, state_token) = service.get_authorization_url("github", None).await.unwrap();

    // Verify the state contains github as provider
    let state = service.verify_state(&state_token).await.unwrap();
    assert_eq!(state.instance_name, "github");

    // API handlers compare this instance name against the callback route.
}

#[tokio::test]
async fn test_distributed_mode_without_redis_returns_error() {
    let state_store = local_oauth_state_store();

    let result = OAuth2Service::validate_cluster_state_store(true, state_store.as_ref());

    assert!(
        result.is_err(),
        "Distributed mode without shared single-use state must return an error"
    );

    let err = result.unwrap_err();
    let err_msg = err.to_string();

    assert!(
        err_msg.contains("shared single-use OAuth2 state"),
        "Error should mention shared single-use OAuth2 state; got: {err_msg}"
    );
    assert!(
        err_msg.contains("distributed runtime"),
        "Error should mention distributed runtime; got: {err_msg}"
    );
    assert!(
        err_msg.contains("replica") || err_msg.contains("replicas"),
        "Error should explain the replica visibility issue; got: {err_msg}"
    );
}

#[tokio::test]
async fn test_cluster_mode_error_message_is_actionable() {
    let state_store = local_oauth_state_store();

    let result = OAuth2Service::validate_cluster_state_store(true, state_store.as_ref());
    let err_msg = result.unwrap_err().to_string();

    assert!(
        err_msg.contains("Configure a shared state backend"),
        "Error should suggest configuring a shared state backend; got: {err_msg}"
    );
}

#[tokio::test]
async fn test_non_cluster_mode_allows_memory() {
    let state_store = local_oauth_state_store();

    let result = OAuth2Service::validate_cluster_state_store(false, state_store.as_ref());

    assert!(
        result.is_ok(),
        "Non-cluster mode should allow in-memory state store"
    );
}

#[tokio::test]
async fn test_cluster_mode_validation_at_creation_time() {
    let state_store = local_oauth_state_store();
    let service_result = OAuth2Service::validate_cluster_state_store(true, state_store.as_ref());

    assert!(
        service_result.is_err(),
        "Cluster mode validation should fail at service creation"
    );
}

#[tokio::test(start_paused = true)]
async fn test_redis_state_store_timeout_maps_to_timeout_error() {
    let timeout_future = run_oauth_state_redis_op(
        crate::resilience::timeout::REDIS_OPERATION_TIMEOUT,
        "store OAuth2 state in Redis",
        async { std::future::pending::<std::result::Result<(), redis::RedisError>>().await },
    );

    tokio::pin!(timeout_future);
    tokio::task::yield_now().await;
    tokio::time::advance(crate::resilience::timeout::REDIS_OPERATION_TIMEOUT).await;

    let err = timeout_future.await.expect_err("operation should time out");
    assert!(matches!(
        err,
        Error::Timeout(ref msg) if msg == "Redis timeout: store OAuth2 state in Redis"
    ));
}
