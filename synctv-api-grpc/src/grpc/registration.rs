#[cfg(verus_only)]
use vstd::prelude::*;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(verus_only, verus_verify)]
pub(super) struct GrpcHealthRegistrationState {
    pub(super) auth_registered: bool,
    pub(super) user_registered: bool,
    pub(super) room_registered: bool,
    pub(super) public_registered: bool,
    pub(super) admin_registered: bool,
    pub(super) email_registered: bool,
    pub(super) notification_registered: bool,
    pub(super) oauth2_registered: bool,
    pub(super) provider_services_registered: bool,
    pub(super) cluster_service_registered: bool,
    pub(super) server_state_registered: bool,
    pub(super) realtime_presence_registered: bool,
    pub(super) proxy_slice_cache_registered: bool,
    pub(super) livestream_relay_registered: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(verus_only, verus_verify)]
pub(super) struct GrpcServiceRegistrationPlan {
    pub(super) reflection_enabled: bool,
    pub(super) health_state: GrpcHealthRegistrationState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(verus_only, verus_verify)]
pub(super) struct GrpcOptionalRegistrations {
    pub(super) email_registered: bool,
    pub(super) notification_registered: bool,
    pub(super) oauth2_registered: bool,
    pub(super) provider_services_registered: bool,
    pub(super) cluster_service_registered: bool,
    pub(super) server_state_registered: bool,
    pub(super) realtime_presence_registered: bool,
    pub(super) proxy_slice_cache_registered: bool,
    pub(super) livestream_relay_registered: bool,
}

#[cfg_attr(verus_only, verus_spec(out => ensures
    out.reflection_enabled == (!expose_cluster_services && reflection),
    out.health_state.auth_registered == !expose_cluster_services,
    out.health_state.user_registered == !expose_cluster_services,
    out.health_state.room_registered == !expose_cluster_services,
    out.health_state.public_registered == !expose_cluster_services,
    out.health_state.admin_registered == !expose_cluster_services,
    out.health_state.email_registered == (!expose_cluster_services && optional.email_registered),
    out.health_state.notification_registered == (!expose_cluster_services && optional.notification_registered),
    out.health_state.oauth2_registered == (!expose_cluster_services && optional.oauth2_registered),
    out.health_state.provider_services_registered == (!expose_cluster_services && optional.provider_services_registered),
    out.health_state.cluster_service_registered == (expose_cluster_services && optional.cluster_service_registered),
    out.health_state.server_state_registered == (expose_cluster_services && optional.server_state_registered),
    out.health_state.realtime_presence_registered == (expose_cluster_services && optional.realtime_presence_registered),
    out.health_state.proxy_slice_cache_registered == (expose_cluster_services && optional.proxy_slice_cache_registered),
    out.health_state.livestream_relay_registered == (expose_cluster_services && optional.livestream_relay_registered),
))]
pub(super) const fn grpc_service_registration_plan(
    reflection: bool,
    expose_cluster_services: bool,
    optional: GrpcOptionalRegistrations,
) -> GrpcServiceRegistrationPlan {
    let expose_public_services = !expose_cluster_services;
    GrpcServiceRegistrationPlan {
        reflection_enabled: expose_public_services && reflection,
        health_state: GrpcHealthRegistrationState {
            auth_registered: expose_public_services,
            user_registered: expose_public_services,
            room_registered: expose_public_services,
            public_registered: expose_public_services,
            admin_registered: expose_public_services,
            email_registered: expose_public_services && optional.email_registered,
            notification_registered: expose_public_services && optional.notification_registered,
            oauth2_registered: expose_public_services && optional.oauth2_registered,
            provider_services_registered: expose_public_services
                && optional.provider_services_registered,
            cluster_service_registered: expose_cluster_services
                && optional.cluster_service_registered,
            server_state_registered: expose_cluster_services && optional.server_state_registered,
            realtime_presence_registered: expose_cluster_services
                && optional.realtime_presence_registered,
            proxy_slice_cache_registered: expose_cluster_services
                && optional.proxy_slice_cache_registered,
            livestream_relay_registered: expose_cluster_services
                && optional.livestream_relay_registered,
        },
    }
}

#[cfg_attr(verus_only, verus_verify)]
pub(super) enum ClusterSetupError {
    Secret,
    Registry,
}

#[cfg_attr(verus_only, verus_spec(out => ensures
    out.is_ok() == (!enabled || (secret && registry)),
    (out matches Err(ClusterSetupError::Secret)) == (enabled && !secret),
    (out matches Err(ClusterSetupError::Registry)) == (enabled && secret && !registry),
))]
pub(super) const fn check_cluster(
    enabled: bool,
    secret: bool,
    registry: bool,
) -> Result<(), ClusterSetupError> {
    if enabled && !secret {
        Err(ClusterSetupError::Secret)
    } else if enabled && !registry {
        Err(ClusterSetupError::Registry)
    } else {
        Ok(())
    }
}
