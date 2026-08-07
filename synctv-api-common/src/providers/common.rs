use std::sync::Arc;
use synctv_core::models::{
    normalize_provider_instance_name, resolve_provider_instance_binding,
    validate_provider_instance_name, AuditDetails, CredentialProviderInstanceName,
    NewProviderInstance, ProviderInstance,
};
use synctv_core::models::{SortDirection as CoreSortDirection, UserId};
use synctv_core::provider::ExecutionControl;
use synctv_core::provider::ProviderError;
use synctv_core::service::{
    AuditEventParams, AuditService, ProvidersManager, RemoteProviderManager, UserService,
};
use synctv_proto::providers::common::ProviderInstanceQuery;

use crate::impls::admin::{AdminAuthValidator, RequestContext, ValidatedAdmin};
use crate::impls::source_provider::{
    core_source_provider_vec_to_proto, proto_source_provider_filter,
    proto_source_provider_required, proto_source_provider_vec,
};
use crate::impls::{ApiError, EndpointRateLimitCategory, RequestExecutor, RequestMetadata};

#[must_use]
pub fn provider_instance_name_for_response(value: Option<String>) -> String {
    value.unwrap_or_default()
}

fn i64_to_i32(value: i64, field: &'static str) -> Result<i32, ApiError> {
    i32::try_from(value).map_err(|_| ApiError::Internal(format!("{field} exceeds i32::MAX")))
}

pub fn provider_instance_name_from_value(value: &str) -> Result<Option<&str>, ApiError> {
    let Some(instance_name) = normalize_provider_instance_name(Some(value)) else {
        return Ok(None);
    };
    validate_provider_instance_name(instance_name).map_err(ApiError::InvalidInput)?;
    Ok(Some(instance_name))
}

pub fn provider_instance_name_for_provider(
    value: Option<&str>,
) -> Result<Option<&str>, ProviderError> {
    let Some(instance_name) = normalize_provider_instance_name(value) else {
        return Ok(None);
    };
    validate_provider_instance_name(instance_name).map_err(ProviderError::InvalidConfig)?;
    Ok(Some(instance_name))
}

pub fn provider_instance_name_from_query(
    query: &ProviderInstanceQuery,
) -> Result<Option<&str>, ApiError> {
    provider_instance_name_from_value(&query.instance_name)
}

pub fn resolve_bound_instance_name(
    requested_instance_name: Option<&str>,
    credential_instance_name: Option<&str>,
) -> Result<Option<String>, ProviderError> {
    resolve_provider_instance_binding(
        requested_instance_name,
        CredentialProviderInstanceName::CredentialBacked(credential_instance_name),
    )
    .map_err(|error| ProviderError::InvalidConfig(error.to_string()))
}

#[derive(Clone)]
pub struct ProviderCommonApiImpl {
    provider_instance_manager: Arc<RemoteProviderManager>,
    providers_manager: Arc<ProvidersManager>,
    user_service: Arc<UserService>,
    audit_service: Arc<AuditService>,
    request_executor: Arc<RequestExecutor>,
}

#[derive(Clone)]
pub struct ProviderCommonApiRuntime {
    pub providers_manager: Arc<ProvidersManager>,
    pub request_executor: Arc<RequestExecutor>,
}

impl ProviderCommonApiImpl {
    #[must_use]
    pub fn new_with_runtime(
        provider_instance_manager: Arc<RemoteProviderManager>,
        user_service: Arc<UserService>,
        audit_service: Arc<AuditService>,
        runtime: ProviderCommonApiRuntime,
    ) -> Self {
        Self {
            provider_instance_manager,
            providers_manager: runtime.providers_manager,
            user_service,
            audit_service,
            request_executor: runtime.request_executor,
        }
    }

    fn request_executor(&self) -> &Arc<RequestExecutor> {
        &self.request_executor
    }

    pub fn execute_admin_endpoint<'a, T, F, Fut>(
        &'a self,
        metadata: &'a RequestMetadata,
        operation: F,
    ) -> futures::future::BoxFuture<'a, Result<T, ApiError>>
    where
        T: Send + 'a,
        F: FnOnce(ValidatedAdmin) -> Fut + Send + 'a,
        Fut: std::future::Future<Output = Result<T, ApiError>> + Send + 'a,
    {
        self.execute_admin_endpoint_with_control(metadata, move |_, validated| operation(validated))
    }

    pub fn execute_user_endpoint<'a, T, E, F, Fut>(
        &'a self,
        metadata: &'a RequestMetadata,
        category: EndpointRateLimitCategory,
        operation: F,
    ) -> futures::future::BoxFuture<'a, Result<T, ApiError>>
    where
        T: Send + 'a,
        E: Into<ApiError> + Send + 'a,
        F: FnOnce(synctv_core::service::AuthenticatedToken) -> Fut + Send + 'a,
        Fut: std::future::Future<Output = Result<T, E>> + Send + 'a,
    {
        self.request_executor()
            .execute_user(metadata, category, move |authenticated| async move {
                operation(authenticated).await.map_err(Into::into)
            })
    }

    pub fn execute_admin_endpoint_with_control<'a, T, F, Fut>(
        &'a self,
        metadata: &'a RequestMetadata,
        operation: F,
    ) -> futures::future::BoxFuture<'a, Result<T, ApiError>>
    where
        T: Send + 'a,
        F: FnOnce(ExecutionControl, ValidatedAdmin) -> Fut + Send + 'a,
        Fut: std::future::Future<Output = Result<T, ApiError>> + Send + 'a,
    {
        let user_service = Arc::clone(&self.user_service);
        self.request_executor().execute_user_with_control(
            metadata,
            EndpointRateLimitCategory::Admin,
            move |request_control, authenticated| async move {
                let validated = AdminAuthValidator::new(user_service.as_ref())
                    .validate(
                        authenticated.user_id,
                        authenticated.claims.pv,
                        authenticated.claims.iat,
                    )
                    .await?;
                if !validated.role.is_admin_or_above() {
                    return Err(ApiError::Authorization("Admin role required".to_string()));
                }
                operation(request_control, validated).await
            },
        )
    }

    pub fn execute_root_endpoint<'a, T, F, Fut>(
        &'a self,
        metadata: &'a RequestMetadata,
        operation: F,
    ) -> futures::future::BoxFuture<'a, Result<T, ApiError>>
    where
        T: Send + 'a,
        F: FnOnce(ValidatedAdmin) -> Fut + Send + 'a,
        Fut: std::future::Future<Output = Result<T, ApiError>> + Send + 'a,
    {
        self.execute_root_endpoint_with_control(metadata, move |_, validated| operation(validated))
    }

    pub fn execute_root_endpoint_with_control<'a, T, F, Fut>(
        &'a self,
        metadata: &'a RequestMetadata,
        operation: F,
    ) -> futures::future::BoxFuture<'a, Result<T, ApiError>>
    where
        T: Send + 'a,
        F: FnOnce(ExecutionControl, ValidatedAdmin) -> Fut + Send + 'a,
        Fut: std::future::Future<Output = Result<T, ApiError>> + Send + 'a,
    {
        self.execute_admin_endpoint_with_control(
            metadata,
            move |request_control, validated| async move {
                if !matches!(validated.role, synctv_core::models::UserRole::Root) {
                    return Err(ApiError::Authorization("Root role required".to_string()));
                }
                operation(request_control, validated).await
            },
        )
    }

    async fn log_admin_action(
        &self,
        admin_user_id: &UserId,
        action: synctv_core::models::AuditAction,
        target_type: synctv_core::models::AuditTargetType,
        target_id: Option<String>,
        details: synctv_core::models::AuditDetails,
        ctx: &RequestContext,
    ) {
        let admin_username = match self.user_service.get_user(admin_user_id).await {
            Ok(user) => user.username,
            Err(error) => {
                tracing::error!(
                    error = %error,
                    admin_user_id = %admin_user_id,
                    action = %action,
                    "AUDIT LOG SKIPPED: failed to resolve provider admin actor username"
                );
                return;
            }
        };

        if let Err(error) = self
            .audit_service
            .log(AuditEventParams {
                actor_id: admin_user_id.to_string(),
                actor_username: admin_username.clone(),
                action,
                target_type,
                target_id,
                details,
                ip_address: ctx.ip_address.clone(),
                user_agent: ctx.user_agent.clone(),
            })
            .await
        {
            tracing::error!(
                error = %error,
                admin_user_id = %admin_user_id,
                admin_username = %admin_username,
                "AUDIT LOG FAILURE: failed to record provider common admin action"
            );
        }
    }

    pub async fn list_available_provider_instances(
        &self,
        req: synctv_proto::providers::common::ListAvailableProviderInstancesRequest,
    ) -> Result<synctv_proto::providers::common::ProviderInstancesResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;

        let provider_type = proto_source_provider_filter(req.provider_type)?;
        let mut instances = if let Some(provider_type) = provider_type {
            self.provider_instance_manager
                .find_instances_by_provider(provider_type.as_str())
                .await
                .map_err(ApiError::from)?
                .into_iter()
                .map(|instance| instance.name)
                .collect()
        } else {
            self.provider_instance_manager
                .list()
                .await
                .map_err(ApiError::from)?
        };
        instances.sort();

        Ok(synctv_proto::providers::common::ProviderInstancesResponse { instances })
    }

    pub async fn list_provider_backends(
        &self,
        req: synctv_proto::providers::common::ListProviderBackendsRequest,
    ) -> Result<synctv_proto::providers::common::ProviderBackendsResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;

        let mut backends = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let provider_type = proto_source_provider_required(req.provider_type)?;
        let provider_name = provider_type.as_str();

        if self
            .providers_manager
            .get_by_type(provider_name)
            .await
            .is_some()
        {
            backends.push(provider_name.to_string());
            seen.insert(provider_name.to_string());
        }

        let mut remote_backends = self
            .provider_instance_manager
            .find_instances_by_provider(provider_name)
            .await
            .map_err(ApiError::from)?
            .into_iter()
            .map(|instance| instance.name)
            .collect::<Vec<_>>();
        remote_backends.sort();

        for backend in remote_backends {
            if seen.insert(backend.clone()) {
                backends.push(backend);
            }
        }

        Ok(synctv_proto::providers::common::ProviderBackendsResponse { backends })
    }

    pub async fn list_provider_instances(
        &self,
        req: synctv_proto::providers::common::ListProviderInstancesRequest,
    ) -> Result<synctv_proto::providers::common::ListProviderInstancesResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;

        let query = synctv_core::models::ProviderInstanceListQuery {
            pagination: synctv_core::models::PageParams::new(
                defaultable_page_i32_to_u32(req.page),
                defaultable_page_size_i32_to_u32(req.page_size, 100),
            ),
            provider_type: proto_source_provider_filter(req.provider_type)?,
            search: normalize_non_empty_filter(&req.search),
            enabled: req.enabled,
            tls: req.tls,
            sort_by: provider_instance_sort_by_from_proto(req.sort_by)?,
            sort_direction: provider_instance_sort_direction_from_proto(req.sort_direction)?,
        };

        let (instances, total) = self
            .provider_instance_manager
            .list_instances_with_total(&query)
            .await
            .map_err(ApiError::from)?;
        let provider_instance_manager = Arc::clone(&self.provider_instance_manager);
        let health = provider_instance_manager
            .health_check_instances_owned(instances.clone())
            .await;

        Ok(
            synctv_proto::providers::common::ListProviderInstancesResponse {
                instances: instances
                    .into_iter()
                    .map(|instance| {
                        let status = provider_instance_status(
                            &instance,
                            health.get(&instance.name).copied(),
                        );
                        provider_instance_to_proto(instance, status)
                    })
                    .collect(),
                total: i64_to_i32(total, "provider instance count")?,
            },
        )
    }

    pub async fn add_provider_instance(
        &self,
        req: synctv_proto::providers::common::AddProviderInstanceRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
        control: Option<&ExecutionControl>,
    ) -> Result<synctv_proto::providers::common::AddProviderInstanceResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let instance = ProviderInstance::new_remote(NewProviderInstance {
            name: req.name,
            endpoint: req.endpoint,
            comment: Some(req.comment),
            jwt_secret: req.jwt_secret,
            custom_ca: req.custom_ca,
            timeout_seconds: req.timeout_seconds,
            tls: req.tls,
            insecure_tls: req.insecure_tls,
            providers: proto_source_provider_vec(req.providers)?,
        });

        self.provider_instance_manager
            .add_with_control(instance.clone(), control)
            .await
            .map_err(ApiError::from)?;

        self.log_admin_action(
            admin_user_id,
            synctv_core::models::AuditAction::ProviderInstanceCreated,
            synctv_core::models::AuditTargetType::ProviderInstance,
            Some(instance.name.clone()),
            AuditDetails {
                instance_name: Some(instance.name.clone()),
                endpoint: Some(mask_url_credentials(&instance.endpoint)),
                ..Default::default()
            },
            ctx,
        )
        .await;

        Ok(
            synctv_proto::providers::common::AddProviderInstanceResponse {
                instance: Some(provider_instance_to_proto(
                    instance,
                    synctv_proto::providers::common::ProviderInstanceStatus::Connected.into(),
                )),
            },
        )
    }

    pub async fn update_provider_instance(
        &self,
        req: synctv_proto::providers::common::UpdateProviderInstanceRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
        control: Option<&ExecutionControl>,
    ) -> Result<synctv_proto::providers::common::UpdateProviderInstanceResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let clear_comment = req.clear_comment.unwrap_or(false);
        let clear_jwt_secret = req.clear_jwt_secret.unwrap_or(false);
        let clear_custom_ca = req.clear_custom_ca.unwrap_or(false);
        if req.endpoint.is_none()
            && req.comment.is_none()
            && req.timeout_seconds.is_none()
            && req.tls.is_none()
            && req.insecure_tls.is_none()
            && req.providers.is_empty()
            && req.jwt_secret.is_none()
            && req.custom_ca.is_none()
            && !(clear_comment || clear_jwt_secret || clear_custom_ca)
        {
            return Err(ApiError::InvalidInput(
                "provider update requires at least one changed field".to_string(),
            ));
        }
        validate_provider_instance_clear_flags(
            &req,
            clear_comment,
            clear_jwt_secret,
            clear_custom_ca,
        )?;

        let mut instance = self
            .provider_instance_manager
            .get_instance(&req.name)
            .await
            .map_err(ApiError::from)?
            .ok_or_else(|| {
                ApiError::NotFound(format!("Provider instance '{}' not found", req.name))
            })?;

        if let Some(endpoint) = req.endpoint {
            instance.endpoint = endpoint;
        }
        if clear_comment {
            instance.comment = None;
        } else if let Some(comment) = req.comment.as_deref() {
            instance.comment = trim_to_optional(comment);
        }
        if let Some(timeout_seconds) = req.timeout_seconds {
            instance.timeout = ProviderInstance::timeout_string_from_seconds(timeout_seconds);
        }
        if !req.providers.is_empty() {
            instance.providers = proto_source_provider_vec(req.providers)?;
        }
        if let Some(tls) = req.tls {
            instance.tls = tls;
        }
        if let Some(insecure_tls) = req.insecure_tls {
            instance.insecure_tls = insecure_tls;
        }
        if clear_jwt_secret {
            instance.jwt_secret = None;
        } else if let Some(jwt_secret) = req.jwt_secret.as_deref() {
            instance.jwt_secret = trim_to_optional(jwt_secret);
        }
        if clear_custom_ca {
            instance.custom_ca = None;
        } else if let Some(custom_ca) = req.custom_ca.as_deref() {
            instance.custom_ca = trim_to_optional(custom_ca);
        }

        instance.updated_at = synctv_core::SystemClock.now();

        self.provider_instance_manager
            .update_with_control(instance.clone(), control)
            .await
            .map_err(ApiError::from)?;

        self.log_admin_action(
            admin_user_id,
            synctv_core::models::AuditAction::ProviderInstanceUpdated,
            synctv_core::models::AuditTargetType::ProviderInstance,
            Some(instance.name.clone()),
            AuditDetails {
                instance_name: Some(instance.name.clone()),
                ..Default::default()
            },
            ctx,
        )
        .await;

        Ok(
            synctv_proto::providers::common::UpdateProviderInstanceResponse {
                instance: Some(provider_instance_to_proto(
                    instance,
                    synctv_proto::providers::common::ProviderInstanceStatus::Connected.into(),
                )),
            },
        )
    }

    pub async fn delete_provider_instance(
        &self,
        req: synctv_proto::providers::common::DeleteProviderInstanceRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<synctv_proto::providers::common::DeleteProviderInstanceResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        self.provider_instance_manager
            .delete(&req.name)
            .await
            .map_err(ApiError::from)?;

        self.log_admin_action(
            admin_user_id,
            synctv_core::models::AuditAction::ProviderInstanceDeleted,
            synctv_core::models::AuditTargetType::ProviderInstance,
            Some(req.name.clone()),
            AuditDetails {
                instance_name: Some(req.name),
                ..Default::default()
            },
            ctx,
        )
        .await;

        Ok(synctv_proto::providers::common::DeleteProviderInstanceResponse { success: true })
    }

    pub async fn reconnect_provider_instance(
        &self,
        req: synctv_proto::providers::common::ReconnectProviderInstanceRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
        control: Option<&ExecutionControl>,
    ) -> Result<synctv_proto::providers::common::ReconnectProviderInstanceResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        self.provider_instance_manager
            .reconnect_with_control(&req.name, control)
            .await
            .map_err(ApiError::from)?;

        let instance = self
            .provider_instance_manager
            .get_instance(&req.name)
            .await
            .map_err(ApiError::from)?
            .ok_or_else(|| {
                ApiError::NotFound(format!("Provider instance '{}' not found", req.name))
            })?;

        self.log_admin_action(
            admin_user_id,
            synctv_core::models::AuditAction::ProviderInstanceReconnected,
            synctv_core::models::AuditTargetType::ProviderInstance,
            Some(instance.name.clone()),
            AuditDetails {
                instance_name: Some(instance.name.clone()),
                endpoint: Some(mask_url_credentials(&instance.endpoint)),
                ..Default::default()
            },
            ctx,
        )
        .await;

        Ok(
            synctv_proto::providers::common::ReconnectProviderInstanceResponse {
                instance: Some(provider_instance_to_proto(
                    instance,
                    synctv_proto::providers::common::ProviderInstanceStatus::Connected.into(),
                )),
            },
        )
    }

    pub async fn enable_provider_instance(
        &self,
        req: synctv_proto::providers::common::EnableProviderInstanceRequest,
        control: Option<&ExecutionControl>,
    ) -> Result<synctv_proto::providers::common::EnableProviderInstanceResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        self.provider_instance_manager
            .enable_with_control(&req.name, control)
            .await
            .map_err(ApiError::from)?;

        let instance = self
            .provider_instance_manager
            .get_instance(&req.name)
            .await
            .map_err(ApiError::from)?
            .ok_or_else(|| {
                ApiError::NotFound(format!("Provider instance '{}' not found", req.name))
            })?;

        Ok(
            synctv_proto::providers::common::EnableProviderInstanceResponse {
                instance: Some(provider_instance_to_proto(
                    instance,
                    synctv_proto::providers::common::ProviderInstanceStatus::Connected.into(),
                )),
            },
        )
    }

    pub async fn disable_provider_instance(
        &self,
        req: synctv_proto::providers::common::DisableProviderInstanceRequest,
        _control: Option<&ExecutionControl>,
    ) -> Result<synctv_proto::providers::common::DisableProviderInstanceResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        self.provider_instance_manager
            .disable(&req.name)
            .await
            .map_err(ApiError::from)?;

        let instance = self
            .provider_instance_manager
            .get_instance(&req.name)
            .await
            .map_err(ApiError::from)?
            .ok_or_else(|| {
                ApiError::NotFound(format!("Provider instance '{}' not found", req.name))
            })?;

        Ok(
            synctv_proto::providers::common::DisableProviderInstanceResponse {
                instance: Some(provider_instance_to_proto(
                    instance,
                    synctv_proto::providers::common::ProviderInstanceStatus::Disconnected.into(),
                )),
            },
        )
    }
}

fn defaultable_page_i32_to_u32(value: i32) -> Option<u32> {
    (value > 0).then_some(value.cast_unsigned())
}

fn defaultable_page_size_i32_to_u32(value: i32, max: i32) -> Option<u32> {
    (value > 0).then_some(value.clamp(1, max).cast_unsigned())
}

fn provider_instance_sort_by_from_proto(
    sort_by: i32,
) -> Result<synctv_core::models::ProviderInstanceListSortBy, ApiError> {
    use synctv_core::models::ProviderInstanceListSortBy as CoreSortBy;
    use synctv_proto::providers::common::ProviderInstanceListSortBy as ProtoSortBy;

    match ProtoSortBy::try_from(sort_by) {
        Ok(ProtoSortBy::Name) => Ok(CoreSortBy::Name),
        Ok(ProtoSortBy::Endpoint) => Ok(CoreSortBy::Endpoint),
        Ok(ProtoSortBy::UpdatedAt) => Ok(CoreSortBy::UpdatedAt),
        Ok(ProtoSortBy::CreatedAt | ProtoSortBy::Unspecified) => Ok(CoreSortBy::CreatedAt),
        Err(_) => Err(ApiError::InvalidInput(format!(
            "Unknown provider instance sort field: {sort_by}"
        ))),
    }
}

fn provider_instance_sort_direction_from_proto(
    sort_direction: i32,
) -> Result<CoreSortDirection, ApiError> {
    use synctv_proto::providers::common::SortDirection as ProtoSortDirection;

    match ProtoSortDirection::try_from(sort_direction) {
        Ok(ProtoSortDirection::Asc) => Ok(CoreSortDirection::Asc),
        Ok(ProtoSortDirection::Desc | ProtoSortDirection::Unspecified) => {
            Ok(CoreSortDirection::Desc)
        }
        Err(_) => Err(ApiError::InvalidInput(format!(
            "Unknown provider instance sort direction: {sort_direction}"
        ))),
    }
}

fn normalize_non_empty_filter(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then_some(trimmed.to_string())
}

fn provider_instance_to_proto(
    instance: synctv_core::models::ProviderInstance,
    status: i32,
) -> synctv_proto::providers::common::ProviderInstance {
    let timeout_seconds = instance.timeout_seconds();

    synctv_proto::providers::common::ProviderInstance {
        name: instance.name,
        endpoint: instance.endpoint,
        comment: instance.comment.unwrap_or_default(),
        timeout_seconds,
        tls: instance.tls,
        insecure_tls: instance.insecure_tls,
        providers: core_source_provider_vec_to_proto(&instance.providers),
        enabled: instance.enabled,
        status,
        created_at: instance.created_at.timestamp(),
        updated_at: instance.updated_at.timestamp(),
    }
}

fn provider_instance_status(
    instance: &synctv_core::models::ProviderInstance,
    healthy: Option<bool>,
) -> i32 {
    use synctv_proto::providers::common::ProviderInstanceStatus;

    if !instance.enabled {
        return ProviderInstanceStatus::Disconnected.into();
    }

    match healthy {
        Some(true) => ProviderInstanceStatus::Connected.into(),
        Some(false) => ProviderInstanceStatus::Error.into(),
        None => ProviderInstanceStatus::Unspecified.into(),
    }
}

fn trim_to_optional(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn validate_provider_instance_clear_flags(
    req: &synctv_proto::providers::common::UpdateProviderInstanceRequest,
    clear_comment: bool,
    clear_jwt_secret: bool,
    clear_custom_ca: bool,
) -> Result<(), ApiError> {
    if req.comment.is_some() && clear_comment {
        return Err(ApiError::InvalidInput(
            "comment and clear_comment cannot both be set".to_string(),
        ));
    }
    if req.jwt_secret.is_some() && clear_jwt_secret {
        return Err(ApiError::InvalidInput(
            "jwt_secret and clear_jwt_secret cannot both be set".to_string(),
        ));
    }
    if req.custom_ca.is_some() && clear_custom_ca {
        return Err(ApiError::InvalidInput(
            "custom_ca and clear_custom_ca cannot both be set".to_string(),
        ));
    }
    Ok(())
}

fn mask_url_credentials(endpoint: &str) -> String {
    match url::Url::parse(endpoint) {
        Ok(mut parsed) => {
            let has_credentials = !parsed.username().is_empty() || parsed.password().is_some();
            if has_credentials
                && (parsed.set_username("").is_err() || parsed.set_password(None).is_err())
            {
                tracing::warn!(
                    endpoint,
                    "failed to mask provider endpoint credentials for diagnostics"
                );
                return endpoint.to_string();
            }
            parsed.to_string()
        }
        Err(_) => endpoint.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use std::sync::Arc;
    use synctv_core::cache::{KeyBuilder, UsernameCache};
    use synctv_core::models::ProviderInstance;
    use synctv_core::repository::ProviderInstanceRepository;
    use synctv_core::service::{
        BruteForceProtection, InMemoryTokenBlacklistStore, JwtService, ProvidersManager,
    };
    use synctv_core_testing::create_test_pool;

    type TestResult<T = ()> = anyhow::Result<T>;

    fn test_error(message: impl Into<String>) -> anyhow::Error {
        anyhow::anyhow!(message.into())
    }

    fn api_ok<T>(result: Result<T, ApiError>) -> TestResult<T> {
        result.map_err(|error| test_error(format!("{error:?}")))
    }

    fn core_ok<T>(result: synctv_core::Result<T>) -> TestResult<T> {
        result.map_err(|error| test_error(error.to_string()))
    }

    #[test]
    fn provider_instance_name_helpers_use_core_normalization() -> TestResult {
        let query = ProviderInstanceQuery {
            instance_name: "  alist-main  ".to_string(),
        };
        assert_eq!(
            api_ok(provider_instance_name_from_query(&query))?,
            Some("alist-main")
        );

        let query = ProviderInstanceQuery {
            instance_name: "bad instance!".to_string(),
        };
        assert!(matches!(
            provider_instance_name_from_query(&query),
            Err(ApiError::InvalidInput(message))
                if message.contains("provider instance name")
        ));
        Ok(())
    }

    fn test_user_service(pool: &sqlx::PgPool) -> TestResult<UserService> {
        Ok(UserService::new_for_tests(
            pool,
            core_ok(JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!"))?,
            UsernameCache::local_only("test:username:".to_string(), 100, 60),
            Arc::new(InMemoryTokenBlacklistStore::new(1000, 3600, 86400)),
            KeyBuilder::new("test"),
            BruteForceProtection::in_memory("test".to_string()),
        ))
    }

    fn make_provider_common_api(
        pool: sqlx::PgPool,
        provider_instance_manager: Arc<RemoteProviderManager>,
        providers_manager: Arc<ProvidersManager>,
    ) -> TestResult<ProviderCommonApiImpl> {
        let user_service = Arc::new(test_user_service(&pool)?);
        let (audit_service, _flush_handle) = AuditService::new(pool);
        Ok(ProviderCommonApiImpl::new_with_runtime(
            provider_instance_manager,
            user_service,
            Arc::new(audit_service),
            ProviderCommonApiRuntime {
                providers_manager,
                request_executor: Arc::new(crate::test_support::local_request_executor()),
            },
        ))
    }

    #[test]
    fn defaultable_pagination_preserves_page_params_defaults() {
        let pagination = synctv_core::models::PageParams::new(
            defaultable_page_i32_to_u32(0),
            defaultable_page_size_i32_to_u32(0, 100),
        );

        assert_eq!(pagination.page, 1);
        assert_eq!(pagination.page_size, 20);
    }

    #[test]
    fn provider_instance_sort_mapping_rejects_unknown_enum_values() {
        assert!(provider_instance_sort_by_from_proto(99_999).is_err());
        assert!(provider_instance_sort_direction_from_proto(99_999).is_err());
    }

    #[test]
    fn provider_instance_sort_mapping_defaults_only_unspecified_values() -> TestResult {
        assert_eq!(
            api_ok(provider_instance_sort_by_from_proto(
                synctv_proto::providers::common::ProviderInstanceListSortBy::Unspecified as i32,
            ))?,
            synctv_core::models::ProviderInstanceListSortBy::CreatedAt
        );
        assert_eq!(
            api_ok(provider_instance_sort_direction_from_proto(
                synctv_proto::providers::common::SortDirection::Unspecified as i32,
            ))?,
            CoreSortDirection::Desc
        );
        Ok(())
    }

    #[tokio::test]
    #[ignore = "Requires Docker-backed PostgreSQL"]
    async fn list_provider_instances_uses_default_page_size_when_request_omits_it() -> TestResult {
        let (_postgres, pool) = create_test_pool().await;
        let repo = Arc::new(ProviderInstanceRepository::new(pool.clone()));
        let provider_instance_manager = Arc::new(RemoteProviderManager::new(repo.clone()));
        let providers_manager = Arc::new(core_ok(ProvidersManager::new(
            provider_instance_manager.clone(),
        ))?);
        let api = make_provider_common_api(pool, provider_instance_manager, providers_manager)?;
        let now = synctv_core::SystemClock.now();

        for index in 0..25 {
            repo.create(&ProviderInstance {
                name: format!("direct-url-{index:02}"),
                endpoint: format!("http://provider{index}.example.com:50051"),
                comment: Some(format!("provider #{index}")),
                jwt_secret: None,
                custom_ca: None,
                timeout: "10s".to_string(),
                tls: false,
                insecure_tls: false,
                providers: vec![synctv_core::models::SourceProvider::DirectUrl],
                enabled: true,
                created_at: now + Duration::seconds(i64::from(index)),
                updated_at: now + Duration::seconds(i64::from(index)),
            })
            .await
            .map_err(|error| test_error(error.to_string()))?;
        }

        let response = api
            .list_provider_instances(
                synctv_proto::providers::common::ListProviderInstancesRequest {
                    page: 0,
                    page_size: 0,
                    provider_type: synctv_proto::source_config::SourceProvider::DirectUrl as i32,
                    search: String::new(),
                    enabled: None,
                    tls: None,
                    sort_by: synctv_proto::providers::common::ProviderInstanceListSortBy::CreatedAt
                        as i32,
                    sort_direction: synctv_proto::providers::common::SortDirection::Asc as i32,
                },
            )
            .await
            .map_err(|error| test_error(format!("{error:?}")))?;

        assert_eq!(
            response.instances.len(),
            20,
            "page_size=0 should preserve the shared default page size"
        );
        assert_eq!(response.instances[0].name, "direct-url-00");
        assert_eq!(response.instances[19].name, "direct-url-19");
        Ok(())
    }
}
