use axum::http::HeaderMap;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use subtle::ConstantTimeEq;
use synctv_core::config::{MetricsAuthMode, MetricsConfig};

#[cfg(feature = "k8s")]
use sha2::{Digest, Sha256};
#[cfg(feature = "k8s")]
use std::sync::Arc;
#[cfg(feature = "k8s")]
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetricsAccessError {
    Unauthorized,
    Forbidden,
    Internal,
}

#[derive(Clone, Default)]
pub struct MetricsAccessController {
    #[cfg(feature = "k8s")]
    kubernetes: Arc<tokio::sync::OnceCell<Arc<KubernetesMetricsAuthorizer>>>,
}

impl MetricsAccessController {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn authorize(
        &self,
        metrics: &MetricsConfig,
        headers: &HeaderMap,
        path: &str,
        method: &str,
    ) -> Result<(), MetricsAccessError> {
        match metrics.auth.mode {
            MetricsAuthMode::BearerToken => self.authorize_bearer(metrics, headers),
            MetricsAuthMode::Basic => self.authorize_basic(metrics, headers),
            MetricsAuthMode::Kubernetes => {
                self.authorize_kubernetes(metrics, headers, path, method)
                    .await
            }
        }
    }

    fn authorize_bearer(
        &self,
        metrics: &MetricsConfig,
        headers: &HeaderMap,
    ) -> Result<(), MetricsAccessError> {
        let Some(header_value) = headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
        else {
            return Err(MetricsAccessError::Unauthorized);
        };

        let provided = synctv_core::service::auth::JwtValidator::extract_bearer_token(header_value)
            .map_err(|_| MetricsAccessError::Unauthorized)?;

        if constant_time_eq(&provided, &metrics.auth.bearer_token) {
            Ok(())
        } else {
            Err(MetricsAccessError::Unauthorized)
        }
    }

    fn authorize_basic(
        &self,
        metrics: &MetricsConfig,
        headers: &HeaderMap,
    ) -> Result<(), MetricsAccessError> {
        let credentials = extract_basic_credentials(headers)?;
        let Some((username, password)) = credentials.split_once(':') else {
            return Err(MetricsAccessError::Unauthorized);
        };

        if constant_time_eq(username, &metrics.auth.basic_username)
            && constant_time_eq(password, &metrics.auth.basic_password)
        {
            Ok(())
        } else {
            Err(MetricsAccessError::Unauthorized)
        }
    }

    async fn authorize_kubernetes(
        &self,
        metrics: &MetricsConfig,
        headers: &HeaderMap,
        path: &str,
        method: &str,
    ) -> Result<(), MetricsAccessError> {
        #[cfg(feature = "k8s")]
        {
            let token = extract_bearer_token(headers)?;
            let authorizer = self
                .kubernetes
                .get_or_try_init(|| async {
                    KubernetesMetricsAuthorizer::new(metrics)
                        .await
                        .map(Arc::new)
                        .map_err(|error| {
                            tracing::error!(
                                error = %error,
                                "failed to initialize Kubernetes metrics authorizer"
                            );
                            error
                        })
                })
                .await
                .map_err(|_| MetricsAccessError::Internal)?;
            return authorizer
                .authorize(&token, path, method, metrics)
                .await
                .map_err(|error| {
                    tracing::warn!(error = ?error, path, method, "metrics request denied");
                    error
                });
        }

        #[cfg(not(feature = "k8s"))]
        {
            let _ = (metrics, headers, path, method);
            Err(MetricsAccessError::Internal)
        }
    }
}

fn constant_time_eq(left: &str, right: &str) -> bool {
    left.as_bytes().ct_eq(right.as_bytes()).into()
}

#[cfg(feature = "k8s")]
fn extract_bearer_token(headers: &HeaderMap) -> Result<String, MetricsAccessError> {
    let Some(header_value) = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
    else {
        return Err(MetricsAccessError::Unauthorized);
    };

    synctv_core::service::auth::JwtValidator::extract_bearer_token(header_value)
        .map_err(|_| MetricsAccessError::Unauthorized)
}

fn extract_basic_credentials(headers: &HeaderMap) -> Result<String, MetricsAccessError> {
    let Some(header_value) = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
    else {
        return Err(MetricsAccessError::Unauthorized);
    };

    let Some(encoded) = header_value
        .split_once(' ')
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("basic"))
        .map(|(_, encoded)| encoded.trim())
    else {
        return Err(MetricsAccessError::Unauthorized);
    };

    let decoded = BASE64_STANDARD
        .decode(encoded)
        .map_err(|_| MetricsAccessError::Unauthorized)?;
    String::from_utf8(decoded).map_err(|_| MetricsAccessError::Unauthorized)
}

#[cfg(feature = "k8s")]
#[derive(Clone)]
struct CachedAuthUser {
    user_info: k8s_openapi::api::authentication::v1::UserInfo,
    expires_at: Instant,
}

#[cfg(feature = "k8s")]
#[derive(Clone)]
struct CachedAuthorizationDecision {
    allowed: bool,
    expires_at: Instant,
}

#[cfg(feature = "k8s")]
struct KubernetesMetricsAuthorizer {
    client: kube::Client,
    authentication_cache: dashmap::DashMap<String, CachedAuthUser>,
    authorization_cache: dashmap::DashMap<String, CachedAuthorizationDecision>,
}

#[cfg(feature = "k8s")]
impl KubernetesMetricsAuthorizer {
    async fn new(_metrics: &MetricsConfig) -> Result<Self, kube::Error> {
        let client = kube::Client::try_default().await?;
        Ok(Self {
            client,
            authentication_cache: dashmap::DashMap::new(),
            authorization_cache: dashmap::DashMap::new(),
        })
    }

    async fn authorize(
        &self,
        token: &str,
        path: &str,
        method: &str,
        metrics: &MetricsConfig,
    ) -> Result<(), MetricsAccessError> {
        let user_info = self
            .authenticate(token, metrics)
            .await
            .map_err(map_kubernetes_metrics_error)?;

        let allowed = self
            .authorize_non_resource(token, &user_info, path, method, metrics)
            .await
            .map_err(map_kubernetes_metrics_error)?;

        if allowed {
            Ok(())
        } else {
            Err(MetricsAccessError::Forbidden)
        }
    }

    async fn authenticate(
        &self,
        token: &str,
        metrics: &MetricsConfig,
    ) -> Result<k8s_openapi::api::authentication::v1::UserInfo, kube::Error> {
        let cache_key = hash_token(token);
        let now = Instant::now();
        if let Some(entry) = self.authentication_cache.get(&cache_key) {
            if entry.expires_at > now {
                return Ok(entry.user_info.clone());
            }
        }

        use k8s_openapi::api::authentication::v1::{TokenReview, TokenReviewSpec};
        let api: kube::Api<TokenReview> = kube::Api::all(self.client.clone());
        let review = api
            .create(
                &kube::api::PostParams::default(),
                &TokenReview {
                    metadata: Default::default(),
                    spec: TokenReviewSpec {
                        audiences: (!metrics.auth.kubernetes.audience.trim().is_empty())
                            .then(|| vec![metrics.auth.kubernetes.audience.clone()]),
                        token: Some(token.to_string()),
                    },
                    status: None,
                },
            )
            .await?;

        let authenticated = review
            .status
            .as_ref()
            .and_then(|status| status.authenticated)
            .unwrap_or(false);
        if !authenticated {
            return Err(kube::Error::Api(kube::error::ErrorResponse {
                status: "Failure".to_string(),
                message: "token review rejected request".to_string(),
                reason: "Unauthorized".to_string(),
                code: 401,
            }));
        }

        let Some(user_info) = review.status.and_then(|status| status.user) else {
            return Err(kube::Error::Api(kube::error::ErrorResponse {
                status: "Failure".to_string(),
                message: "token review did not return user info".to_string(),
                reason: "Unauthorized".to_string(),
                code: 401,
            }));
        };

        self.authentication_cache.insert(
            cache_key,
            CachedAuthUser {
                user_info: user_info.clone(),
                expires_at: now
                    + Duration::from_secs(metrics.auth.kubernetes.authentication_cache_ttl_seconds),
            },
        );

        Ok(user_info)
    }

    async fn authorize_non_resource(
        &self,
        token: &str,
        user_info: &k8s_openapi::api::authentication::v1::UserInfo,
        path: &str,
        method: &str,
        metrics: &MetricsConfig,
    ) -> Result<bool, kube::Error> {
        let cache_key = format!("{}:{path}:{method}", hash_token(token));
        let now = Instant::now();
        if let Some(entry) = self.authorization_cache.get(&cache_key) {
            if entry.expires_at > now {
                return Ok(entry.allowed);
            }
        }

        use k8s_openapi::api::authorization::v1::{
            NonResourceAttributes, SubjectAccessReview, SubjectAccessReviewSpec,
        };

        let api: kube::Api<SubjectAccessReview> = kube::Api::all(self.client.clone());
        let review = api
            .create(
                &kube::api::PostParams::default(),
                &SubjectAccessReview {
                    metadata: Default::default(),
                    spec: SubjectAccessReviewSpec {
                        extra: user_info.extra.clone(),
                        groups: user_info.groups.clone(),
                        non_resource_attributes: Some(NonResourceAttributes {
                            path: Some(path.to_string()),
                            verb: Some(method.to_ascii_lowercase()),
                        }),
                        resource_attributes: None,
                        uid: user_info.uid.clone(),
                        user: user_info.username.clone(),
                    },
                    status: None,
                },
            )
            .await?;

        let allowed = review
            .status
            .as_ref()
            .map(|status| status.allowed)
            .unwrap_or(false);

        self.authorization_cache.insert(
            cache_key,
            CachedAuthorizationDecision {
                allowed,
                expires_at: now
                    + Duration::from_secs(metrics.auth.kubernetes.authorization_cache_ttl_seconds),
            },
        );

        Ok(allowed)
    }
}

#[cfg(feature = "k8s")]
fn map_kubernetes_metrics_error(error: kube::Error) -> MetricsAccessError {
    if let kube::Error::Api(response) = &error {
        if response.code == 401 {
            return MetricsAccessError::Unauthorized;
        }
        if response.code == 403 {
            return MetricsAccessError::Forbidden;
        }
    }
    tracing::error!(error = %error, "kubernetes metrics auth request failed");
    MetricsAccessError::Internal
}

#[cfg(feature = "k8s")]
fn hash_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::header::AUTHORIZATION;
    use axum::http::HeaderValue;

    fn bearer_metrics_config(token: &str) -> MetricsConfig {
        let mut config = MetricsConfig::default();
        config.enabled = true;
        config.auth.mode = MetricsAuthMode::BearerToken;
        config.auth.bearer_token = token.to_string();
        config
    }

    fn basic_metrics_config(username: &str, password: &str) -> MetricsConfig {
        let mut config = MetricsConfig::default();
        config.enabled = true;
        config.auth.mode = MetricsAuthMode::Basic;
        config.auth.basic_username = username.to_string();
        config.auth.basic_password = password.to_string();
        config
    }

    #[tokio::test]
    async fn metrics_access_controller_accepts_matching_bearer_token() {
        let controller = MetricsAccessController::new();
        let config = bearer_metrics_config("secret");
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer secret"));

        let result = controller
            .authorize(&config, &headers, "/metrics", "GET")
            .await;

        assert_eq!(result, Ok(()));
    }

    #[tokio::test]
    async fn metrics_access_controller_rejects_wrong_basic_password() {
        let controller = MetricsAccessController::new();
        let config = basic_metrics_config("metrics", "secret");
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_static("Basic bWV0cmljczp3cm9uZw=="),
        );

        let result = controller
            .authorize(&config, &headers, "/metrics", "GET")
            .await;

        assert_eq!(result, Err(MetricsAccessError::Unauthorized));
    }
}
