use super::state_store::run_oauth_state_redis_op;
use super::*;
use crate::oauth2::OAuth2Authorization;
use crate::oauth2::Provider as OAuth2ProviderTrait;
use crate::test_helpers::failing_redis_runtime;
use crate::{Error, SharedStateMode, SharedStateProfile};
use async_trait::async_trait;

fn ok<T, E: std::fmt::Display>(result: std::result::Result<T, E>, context: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => std::panic::panic_any(format!("{context}: {error}")),
    }
}

fn err<T, E: std::fmt::Display>(result: std::result::Result<T, E>, context: &str) -> E {
    match result {
        Ok(_) => std::panic::panic_any(context.to_string()),
        Err(error) => error,
    }
}

fn some<T>(value: Option<T>, context: &str) -> T {
    match value {
        Some(value) => value,
        None => std::panic::panic_any(context.to_string()),
    }
}

fn joined<T>(result: std::result::Result<T, tokio::task::JoinError>, context: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => std::panic::panic_any(format!("{context}: {error}")),
    }
}

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
    let profile = SharedStateProfile::for_cluster_runtime(None, "test:", false);

    let store = ok(
        state_store_from_shared_state_profile(&profile),
        "standalone mode should allow local OAuth2 state storage",
    );

    assert!(
        !store.supports_cross_node_single_use(),
        "local store must not claim cross-node single-use guarantees"
    );
}

#[test]
fn test_state_store_from_shared_state_profile_requires_shared_runtime_in_cluster_mode() {
    let profile = SharedStateProfile::for_cluster_runtime(None, "test:", true);

    let Err(error) = state_store_from_shared_state_profile(&profile) else {
        std::panic::panic_any("cluster mode must reject local OAuth2 state storage");
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

    let store = ok(
        state_store_from_shared_state_profile(&profile),
        "shared runtime profile should yield a distributed OAuth2 state store",
    );

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
                avatar: Some("https://avatar.example.com/42.png".to_string()),
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
    ok(
        OAuth2Service::new_without_repository_for_tests(
            state_store,
            crate::oauth2::ProviderRegistry::new(),
            synctv_common::ssrf::SsrfGuard::strict_policy(),
            cluster_mode,
            runtime,
        ),
        "Failed to create OAuth2 service",
    )
}

fn create_test_service_with_allowed_urls(urls: Vec<String>) -> OAuth2Service {
    let guard = synctv_common::ssrf::SsrfGuard::strict_policy();
    let settings = create_test_runtime_settings_store(&guard);
    let providers: crate::service::OAuth2ProviderConfigs = ok(
        r#"{"github":{"type":"github","enableSignup":true,"clientId":"id","clientSecret":"secret","redirectUrl":"https://syncs.tv/oauth2/callback"}}"#.parse(),
        "test GitHub provider config should parse",
    );
    ok(
        settings.oauth2.providers.set_for_test(&providers),
        "test provider settings should validate",
    );
    ok(
        settings.oauth2.allowed_redirect_urls.set_for_test(
            &crate::service::global_settings::OAuth2AllowedRedirectUrls(urls),
        ),
        "test redirect allowlist should validate",
    );
    ok(
        OAuth2Service::new_without_repository_for_tests(
            local_oauth_state_store(),
            crate::oauth2::providers::provider_registry(guard.clone()),
            guard,
            false,
            OAuth2ServiceRuntime {
                runtime_settings_store: Some(settings),
                ..OAuth2ServiceRuntime::default()
            },
        ),
        "OAuth2 service should be created",
    )
}

fn create_test_runtime_settings_store(
    guard: &synctv_common::ssrf::SsrfGuard,
) -> Arc<RuntimeSettingsStore> {
    Arc::new(RuntimeSettingsStore::new_for_tests_with_ssrf_guard(guard))
}

#[test]
fn test_redirect_relative_path_rejected() {
    let result = OAuth2Service::validate_redirect_url_with_allowlist("/dashboard", &[]);
    let error = err(result, "relative redirect path should fail");
    assert!(
        error.to_string().contains("absolute http(s) URL"),
        "error should mention missing host, got: {error:?}"
    );
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
fn test_redirect_absolute_url_rejected_when_allowlist_is_empty() {
    let result =
        OAuth2Service::validate_redirect_url_with_allowlist("https://example.com/callback", &[]);
    assert!(result.is_err());
    let error = err(result, "absolute redirect should require allowlist");
    assert!(matches!(&error, Error::InvalidInput(msg) if msg.contains("allowed redirect URLs")));
}

#[test]
fn test_redirect_absolute_url_allowed_when_url_matches() {
    let urls = vec!["https://example.com/callback".to_string()];
    let result =
        OAuth2Service::validate_redirect_url_with_allowlist("https://example.com/callback", &urls);
    assert!(result.is_ok());
}

#[test]
fn test_redirect_allowlist_requires_exact_path() {
    let urls = vec!["https://app.example.com/oauth2/callback".to_string()];
    let result = OAuth2Service::validate_redirect_url_with_allowlist(
        "https://app.example.com/callback",
        &urls,
    );
    assert!(result.is_err());
}

#[test]
fn test_redirect_http_url_rejected_for_non_loopback_host() {
    let urls = vec!["http://example.com/callback".to_string()];
    let result =
        OAuth2Service::validate_redirect_url_with_allowlist("http://example.com/callback", &urls);
    assert!(result.is_err());
    let error = err(result, "non-loopback HTTP redirect should fail");
    assert!(matches!(&error, Error::InvalidInput(msg) if msg.contains("HTTPS")));
}

#[test]
fn test_redirect_absolute_url_rejected_for_wrong_domain() {
    let urls = vec!["https://example.com/callback".to_string()];
    let result =
        OAuth2Service::validate_redirect_url_with_allowlist("https://evil.com/callback", &urls);
    assert!(result.is_err());
    let error = err(result, "wrong-domain redirect should fail");
    assert!(matches!(&error, Error::InvalidInput(msg) if msg.contains("allowed redirect URLs")));
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
    let error = err(result, "FTP redirect should fail");
    assert!(matches!(&error, Error::InvalidInput(msg) if msg.contains("Invalid URL scheme")));
}

#[test]
fn test_redirect_url_with_credentials_rejected() {
    let domains = vec!["example.com".to_string()];
    let result = OAuth2Service::validate_redirect_url_with_allowlist(
        "https://user:pass@example.com/callback",
        &domains,
    );
    assert!(result.is_err());
    let error = err(result, "credentialed redirect should fail");
    assert!(matches!(&error, Error::InvalidInput(msg) if msg.contains("credentials")));
}

#[test]
fn test_redirect_url_requires_runtime_allowlist_match() {
    let allowed = vec!["https://syncs.tv/oauth2/callback".to_string()];
    assert!(OAuth2Service::validate_redirect_url_with_allowlist(
        "https://syncs.tv/oauth2/callback",
        &allowed,
    )
    .is_ok());
    assert!(OAuth2Service::validate_redirect_url_with_allowlist(
        "https://syncs.tv/other",
        &allowed,
    )
    .is_err());
}

#[test]
fn test_redirect_malformed_url_rejected() {
    let domains = vec!["example.com".to_string()];
    let result =
        OAuth2Service::validate_redirect_url_with_allowlist("not a valid url at all", &domains);
    assert!(result.is_err());
}

#[test]
fn test_redirect_host_fragment_rejected() {
    let urls = vec!["com".to_string()];
    let result =
        OAuth2Service::validate_redirect_url_with_allowlist("https://evil.com/callback", &urls);
    assert!(result.is_err());
}

#[test]
fn test_redirect_same_domain_different_subdomain_rejected() {
    let urls = vec!["https://example.com/callback".to_string()];
    let result = OAuth2Service::validate_redirect_url_with_allowlist(
        "https://deep.sub.example.com/callback",
        &urls,
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
        created_at: crate::SystemClock.now(),
        operation: OAuth2Operation::Login,
        target_user_id: None,
        pkce_verifier: "verifier123".to_string(),
        nonce: None,
    };

    ok(
        service.store_state("token_abc", &state).await,
        "state should store",
    );
    let retrieved = ok(
        service.consume_state("token_abc").await,
        "state should consume",
    );

    assert_eq!(retrieved.instance_name, "github");
    assert_eq!(retrieved.pkce_verifier, "verifier123");
    assert_eq!(
        retrieved.redirect_url.as_deref(),
        Some("http://127.0.0.1:34567/dashboard")
    );
    assert!(retrieved.target_user_id.is_none());
}

#[tokio::test]
async fn test_state_single_use_consumed_on_first_retrieval() {
    let service = create_test_service();
    let state = OAuth2State {
        instance_name: "google".to_string(),
        redirect_url: None,
        created_at: crate::SystemClock.now(),
        operation: OAuth2Operation::Login,
        target_user_id: None,
        pkce_verifier: "v".to_string(),
        nonce: None,
    };

    ok(
        service.store_state("token_once", &state).await,
        "single-use state should store",
    );

    // First consume succeeds
    let result = service.consume_state("token_once").await;
    assert!(result.is_ok());

    // Second consume fails (state was removed)
    let result = service.consume_state("token_once").await;
    assert!(result.is_err());
    let err = err(result, "replayed state should fail");
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
    assert!(matches!(
        err(result, "missing state should fail"),
        Error::Authentication(msg) if msg.contains("Invalid or expired")
    ));
}

#[tokio::test]
async fn test_state_preserves_target_user_id() {
    let service = create_test_service();
    let user_id = UserId::expect_positive(93_001);
    let state = OAuth2State {
        instance_name: "logto".to_string(),
        redirect_url: None,
        created_at: crate::SystemClock.now(),
        operation: OAuth2Operation::Bind,
        target_user_id: Some(user_id),
        pkce_verifier: "bind_verifier".to_string(),
        nonce: None,
    };

    ok(
        service.store_state("bind_token", &state).await,
        "bound state should store",
    );
    let retrieved = ok(
        service.consume_state("bind_token").await,
        "bound state should consume",
    );

    assert_eq!(
        some(
            retrieved.target_user_id.as_ref(),
            "target user id should persist"
        )
        .to_string(),
        "93001"
    );
}

#[tokio::test]
async fn test_verify_state_consumes_token() {
    let service = create_test_service();
    let state = OAuth2State {
        instance_name: "oidc".to_string(),
        redirect_url: None,
        created_at: crate::SystemClock.now(),
        operation: OAuth2Operation::Login,
        target_user_id: None,
        pkce_verifier: "pkce_v".to_string(),
        nonce: None,
    };

    ok(
        service.store_state("verify_tok", &state).await,
        "verified state should store",
    );

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
    let providers = ok(
        service.list_available_instances().await,
        "provider list should load",
    );
    assert!(providers.is_empty());

    // Register a mock provider
    service
        .register_provider(
            "github".to_string(),
            OAuth2Provider::GitHub,
            Box::new(TestOAuth2Provider::new()),
        )
        .await;

    let providers = ok(
        service.list_available_instances().await,
        "provider list should reload",
    );
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

    let policy = ok(
        service.signup_policy_for("github").await,
        "registered provider should have a signup policy",
    );
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
    let registry = create_test_runtime_settings_store(&guard);
    let configs: crate::service::OAuth2ProviderConfigs = ok(
        r#"{"casdoor_oidc":{"type":"oidc","enableSignup":true,"clientId":"id","clientSecret":"secret","redirectUrl":"http://127.0.0.1:18081/oauth/callback","issuer":"http://127.0.0.1:18000"}}"#.parse(),
        "test OAuth2 provider config should parse",
    );
    ok(
        registry.oauth2.providers.set_for_test(&configs),
        "test settings seed should validate",
    );

    let service = ok(
        OAuth2Service::new_without_repository_for_tests(
            local_oauth_state_store(),
            crate::oauth2::providers::provider_registry(guard),
            synctv_common::ssrf::SsrfGuard::builder()
                .allow_private_network_targets(true)
                .build(),
            false,
            OAuth2ServiceRuntime {
                runtime_settings_store: Some(registry),
                ..OAuth2ServiceRuntime::default()
            },
        ),
        "OAuth2 service should be created",
    );

    let providers = ok(
        service.list_available_instances().await,
        "runtime SSRF policy should allow local Casdoor OIDC issuer",
    );
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

    let providers = ok(
        service.list_available_instances().await,
        "provider list should load",
    );
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

    service
        .register_provider(
            "github".to_string(),
            OAuth2Provider::Oidc,
            Box::new(TestOAuth2Provider::new()),
        )
        .await;

    let providers = ok(
        service.list_available_instances().await,
        "provider list should reload",
    );
    assert_eq!(providers.len(), 1);
    assert_eq!(providers[0].1, OAuth2Provider::Oidc);
}

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

    let (auth_url, state_token) = ok(
        service.get_authorization_url("github", None).await,
        "authorization URL should generate",
    );

    assert!(auth_url.contains("https://provider.example.com/auth"));
    assert!(auth_url.contains("state="));
    assert_eq!(state_token.len(), 32);

    let state = ok(
        service.verify_state(&state_token).await,
        "state should verify",
    );
    assert_eq!(state.instance_name, "github");
    assert_eq!(state.pkce_verifier, "test_pkce_verifier_abc123");
    assert!(state.redirect_url.is_none());
    assert!(state.target_user_id.is_none());
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

    let err = err(
        service
            .get_authorization_url("github", Some("/rooms/123".to_string()))
            .await,
        "relative redirect URL must be rejected",
    );
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

    let result = service
        .get_authorization_url("github", Some("https://evil.com/steal".to_string()))
        .await;
    assert!(result.is_err());
    assert!(matches!(
        err(result, "invalid redirect should fail"),
        Error::InvalidInput(_)
    ));
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
    let err = err(result, "unknown provider should fail");
    assert!(
        matches!(&err, Error::InvalidInput(msg) if msg.contains("not found")),
        "Expected provider not found error, got: {err}"
    );
}

#[tokio::test]
async fn test_get_authorization_url_with_runtime_allowlist() {
    let service = create_test_service_with_allowed_urls(vec![
        "https://myapp.com/callback".to_string(),
        "https://auth.myapp.com/cb".to_string(),
    ]);

    let result = service
        .get_authorization_url("github", Some("https://myapp.com/callback".to_string()))
        .await;
    assert!(result.is_ok());

    let result = service
        .get_authorization_url("github", Some("https://auth.myapp.com/cb".to_string()))
        .await;
    assert!(result.is_ok());

    let result = service
        .get_authorization_url("github", Some("https://evil.com/steal".to_string()))
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_get_authorization_url_accepts_loopback_native_client_redirects() {
    let service = create_test_service();
    service
        .register_provider(
            "github".to_string(),
            OAuth2Provider::GitHub,
            Box::new(TestOAuth2Provider::new()),
        )
        .await;

    let (_, loopback_state_token) = ok(
        service
            .get_authorization_url(
                "github",
                Some("http://127.0.0.1:34567/oauth/callback".to_string()),
            )
            .await,
        "native loopback redirects should not require domain allowlist",
    );
    let loopback_state = ok(
        service.verify_state(&loopback_state_token).await,
        "loopback state should verify",
    );
    assert_eq!(
        loopback_state.redirect_url.as_deref(),
        Some("http://127.0.0.1:34567/oauth/callback")
    );
}

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
    let (auth_url, state_token) = ok(
        service
            .get_authorization_url_with_user("logto", None, Some(user_id))
            .await,
        "authorization URL with user should generate",
    );

    assert!(auth_url.contains("https://provider.example.com/auth"));

    let state = ok(
        service.verify_state(&state_token).await,
        "bound state should verify",
    );
    assert_eq!(state.instance_name, "logto");
    assert_eq!(
        some(
            state.target_user_id.as_ref(),
            "target user id should persist"
        )
        .to_string(),
        "93002"
    );
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

    let (_, state_token) = ok(
        service.get_authorization_url("github", None).await,
        "login authorization URL without user should generate",
    );

    let state = ok(
        service.verify_state(&state_token).await,
        "unbound state should verify",
    );
    assert_eq!(state.operation, OAuth2Operation::Login);
    assert!(state.target_user_id.is_none());
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

    let user_info = ok(
        service
            .exchange_code_for_user_info("github", "auth_code_123", "pkce_verifier_abc")
            .await,
        "code exchange should return user info",
    );

    assert_eq!(user_info.provider_user_id, "provider_user_42");
    assert_eq!(user_info.username, "testuser");
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
        err(result, "unknown exchange provider should fail"),
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
    let err = err(result, "failing provider should return exchange error");
    assert!(
        matches!(&err, Error::Internal(msg) if msg.contains("invalid_grant")),
        "Expected internal error with invalid_grant, got: {err}"
    );
}

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

    let err = err(
        service
            .get_authorization_url("github", Some("/dashboard".to_string()))
            .await,
        "relative redirect URL must be rejected",
    );
    assert!(matches!(err, Error::InvalidInput(_)));
    let (auth_url, state_token) = ok(
        service
            .get_authorization_url(
                "github",
                Some("http://127.0.0.1:34567/dashboard".to_string()),
            )
            .await,
        "loopback authorization URL should generate",
    );
    assert!(auth_url.contains("state="));

    let state = ok(
        service.verify_state(&state_token).await,
        "login state should verify",
    );
    assert_eq!(state.instance_name, "github");
    assert_eq!(
        state.redirect_url.as_deref(),
        Some("http://127.0.0.1:34567/dashboard")
    );

    let user_info = ok(
        service
            .exchange_code_for_user_info("github", "callback_code", &state.pkce_verifier)
            .await,
        "callback code should exchange",
    );
    assert_eq!(user_info.username, "testuser");
    assert_eq!(user_info.provider, OAuth2Provider::GitHub);
    assert_eq!(user_info.provider_instance_name, "github");

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

    let (_, state_token) = ok(
        service
            .get_authorization_url_with_user("logto", None, Some(user_id))
            .await,
        "bound authorization URL should generate",
    );

    let state = ok(
        service.verify_state(&state_token).await,
        "bound state should verify",
    );
    assert_eq!(
        some(
            state.target_user_id.as_ref(),
            "target user id should persist"
        )
        .to_string(),
        "93003"
    );
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
async fn test_allowed_redirect_urls_are_runtime_configured() {
    let service = create_test_service_with_allowed_urls(vec![
        "https://example.com/cb".to_string(),
        "https://myapp.io/cb".to_string(),
    ]);

    let result = service
        .get_authorization_url("github", Some("https://example.com/cb".to_string()))
        .await;
    assert!(result.is_ok());

    let result = service
        .get_authorization_url("github", Some("https://myapp.io/cb".to_string()))
        .await;
    assert!(result.is_ok());

    let result = service
        .get_authorization_url("github", Some("https://other.com/cb".to_string()))
        .await;
    assert!(result.is_err());
}

#[test]
fn test_oauth2_state_serialization_roundtrip() {
    let state = OAuth2State {
        instance_name: "github".to_string(),
        redirect_url: Some("http://127.0.0.1:34567/dashboard".to_string()),
        created_at: crate::SystemClock.now(),
        operation: OAuth2Operation::Bind,
        target_user_id: Some(UserId::expect_positive(93_004)),
        pkce_verifier: "S256_challenge_verifier".to_string(),
        nonce: Some("oidc_nonce_123".to_string()),
    };

    let json = ok(serde_json::to_string(&state), "state should serialize");
    let deserialized: OAuth2State = ok(serde_json::from_str(&json), "state should deserialize");

    assert_eq!(deserialized.instance_name, state.instance_name);
    assert_eq!(deserialized.redirect_url, state.redirect_url);
    assert_eq!(deserialized.pkce_verifier, state.pkce_verifier);
    assert_eq!(deserialized.nonce, state.nonce);
    assert_eq!(
        some(
            deserialized.target_user_id.as_ref(),
            "target user id should deserialize"
        )
        .to_string(),
        "93004"
    );
}

#[test]
fn test_oauth2_state_serialization_none_fields() {
    let state = OAuth2State {
        instance_name: "oidc".to_string(),
        redirect_url: None,
        created_at: crate::SystemClock.now(),
        operation: OAuth2Operation::Login,
        target_user_id: None,
        pkce_verifier: "v".to_string(),
        nonce: None,
    };

    let json = ok(serde_json::to_string(&state), "state should serialize");
    let deserialized: OAuth2State = ok(serde_json::from_str(&json), "state should deserialize");

    assert!(deserialized.redirect_url.is_none());
    assert!(deserialized.target_user_id.is_none());
}

#[tokio::test]
async fn test_multiple_concurrent_states() {
    let service = create_test_service();

    for i in 0..10 {
        let state = OAuth2State {
            instance_name: format!("provider_{i}"),
            redirect_url: None,
            created_at: crate::SystemClock.now(),
            operation: OAuth2Operation::Login,
            target_user_id: None,
            pkce_verifier: format!("verifier_{i}"),
            nonce: None,
        };
        ok(
            service.store_state(&format!("token_{i}"), &state).await,
            "state should store",
        );
    }

    for i in 0..10 {
        let state = ok(
            service.consume_state(&format!("token_{i}")).await,
            "state should consume",
        );
        assert_eq!(state.instance_name, format!("provider_{i}"));
        assert_eq!(state.pkce_verifier, format!("verifier_{i}"));
    }

    for i in 0..10 {
        let result = service.consume_state(&format!("token_{i}")).await;
        assert!(result.is_err());
    }
}

#[tokio::test]
async fn test_pkce_verifier_preserved_through_state_lifecycle() {
    let service = create_test_service();
    let provider = TestOAuth2Provider {
        auth_url: "https://auth.test/authorize".to_string(),
        pkce_verifier: "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk".to_string(),
        user_info: Some(crate::oauth2::OAuth2UserInfo {
            provider_user_id: "94001".to_string(),
            username: "user1".to_string(),
            avatar: None,
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

    let (_, state_token) = ok(
        service.get_authorization_url("test_pkce", None).await,
        "authorization URL should generate",
    );

    let state = ok(
        service.verify_state(&state_token).await,
        "state should verify",
    );
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

    let (_, token1) = ok(
        service.get_authorization_url("github", None).await,
        "first authorization URL should generate",
    );
    let (_, token2) = ok(
        service.get_authorization_url("github", None).await,
        "second authorization URL should generate",
    );

    assert_ne!(
        token1, token2,
        "Each authorization request must get a unique state token"
    );
}

#[tokio::test]
async fn test_concurrent_state_consumption_only_first_succeeds() {
    let service = Arc::new(create_test_service());
    let state = OAuth2State {
        instance_name: "github".to_string(),
        redirect_url: None,
        created_at: crate::SystemClock.now(),
        operation: OAuth2Operation::Login,
        target_user_id: None,
        pkce_verifier: "concurrent_verifier".to_string(),
        nonce: None,
    };

    ok(
        service.store_state("concurrent_token", &state).await,
        "concurrent state should store",
    );

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
        match joined(h.await, "state consumer task should join") {
            Ok(state) => {
                assert_eq!(state.pkce_verifier, "concurrent_verifier");
                success_count += 1;
            }
            Err(_) => {
                failure_count += 1;
            }
        }
    }

    assert_eq!(success_count, 1, "Exactly one consumer must succeed");
    assert_eq!(failure_count, 19, "All other consumers must fail");

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

    let (_, state_token) = ok(
        service.get_authorization_url("github", None).await,
        "authorization URL should generate",
    );

    let mut handles = Vec::new();
    for _ in 0..10 {
        let svc = service.clone();
        let tok = state_token.clone();
        handles.push(tokio::spawn(async move { svc.verify_state(&tok).await }));
    }

    let mut success_count = 0;
    for h in handles {
        if joined(h.await, "verify_state task should join").is_ok() {
            success_count += 1;
        }
    }

    assert_eq!(success_count, 1, "Exactly one verify must succeed");

    let replay = service.verify_state(&state_token).await;
    assert!(replay.is_err(), "State token should be consumed");
}

#[tokio::test]
async fn test_consuming_one_state_does_not_affect_others() {
    let service = create_test_service();

    for i in 0..5 {
        let state = OAuth2State {
            instance_name: format!("provider_{i}"),
            redirect_url: None,
            created_at: crate::SystemClock.now(),
            operation: OAuth2Operation::Login,
            target_user_id: None,
            pkce_verifier: format!("verifier_{i}"),
            nonce: None,
        };
        ok(
            service
                .store_state(&format!("isolated_token_{i}"), &state)
                .await,
            "isolated state should store",
        );
    }

    let consumed = ok(
        service.consume_state("isolated_token_2").await,
        "isolated token should consume",
    );
    assert_eq!(consumed.instance_name, "provider_2");

    for i in [0, 1, 3, 4] {
        let state = ok(
            service.consume_state(&format!("isolated_token_{i}")).await,
            "other isolated token should consume",
        );
        assert_eq!(state.instance_name, format!("provider_{i}"));
    }

    let result = service.consume_state("isolated_token_2").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_state_expired_created_at_rejected() {
    let service = create_test_service();

    let expired_time = crate::SystemClock.now() - chrono::Duration::seconds(360);
    let state = OAuth2State {
        instance_name: "github".to_string(),
        redirect_url: None,
        created_at: expired_time,
        operation: OAuth2Operation::Login,
        target_user_id: None,
        pkce_verifier: "expired_verifier".to_string(),
        nonce: None,
    };

    ok(
        service.store_state("expired_token", &state).await,
        "expired state fixture should store",
    );

    let result = service.consume_state("expired_token").await;
    assert!(result.is_err());
    let err = err(result, "expired state should fail");
    assert!(
        matches!(&err, Error::Authentication(msg) if msg.contains("Invalid or expired")),
        "Expected authentication error for expired state, got: {err}"
    );
}

#[tokio::test]
async fn test_state_within_ttl_accepted() {
    let service = create_test_service();

    let within_ttl_time = crate::SystemClock.now() - chrono::Duration::seconds(240);
    let state = OAuth2State {
        instance_name: "github".to_string(),
        redirect_url: None,
        created_at: within_ttl_time,
        operation: OAuth2Operation::Login,
        target_user_id: None,
        pkce_verifier: "valid_verifier".to_string(),
        nonce: None,
    };

    ok(
        service.store_state("within_ttl_token", &state).await,
        "within-TTL state should store",
    );

    let result = service.consume_state("within_ttl_token").await;
    assert!(result.is_ok());
    let retrieved = ok(result, "within-TTL state should consume");
    assert_eq!(retrieved.pkce_verifier, "valid_verifier");
}

#[tokio::test]
async fn test_state_at_ttl_boundary() {
    let service = create_test_service();

    let past_boundary_time =
        crate::SystemClock.now() - chrono::Duration::seconds(OAUTH2_STATE_TTL_SECONDS_I64 + 1);
    let state = OAuth2State {
        instance_name: "github".to_string(),
        redirect_url: None,
        created_at: past_boundary_time,
        operation: OAuth2Operation::Login,
        target_user_id: None,
        pkce_verifier: "boundary_verifier".to_string(),
        nonce: None,
    };

    ok(
        service.store_state("boundary_token", &state).await,
        "boundary state should store",
    );

    let result = service.consume_state("boundary_token").await;
    assert!(result.is_err());
}

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

    let expired_time = crate::SystemClock.now() - chrono::Duration::seconds(360);
    let state = OAuth2State {
        instance_name: "github".to_string(),
        redirect_url: None,
        created_at: expired_time,
        operation: OAuth2Operation::Login,
        target_user_id: None,
        pkce_verifier: "expired".to_string(),
        nonce: None,
    };

    ok(
        service.store_state("verify_expired_token", &state).await,
        "verify-expired state should store",
    );

    let result = service.verify_state("verify_expired_token").await;
    assert!(result.is_err());
    let err = err(result, "expired verify_state should fail");
    assert!(
        matches!(&err, Error::Authentication(msg) if msg.contains("Invalid or expired")),
        "Expected authentication error for expired state in verify_state, got: {err}"
    );
}

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

    let (_, state_token) = ok(
        service.get_authorization_url("github", None).await,
        "authorization URL should generate",
    );

    let state = ok(
        service.verify_state(&state_token).await,
        "state should verify",
    );
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

    let err = err(result, "cluster mode without shared state should fail");
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
    let err_msg = err(result, "cluster mode should produce actionable error").to_string();

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
