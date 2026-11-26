//! The production modules in the Verus verification unit.
#![allow(dead_code)]
#![feature(proc_macro_hygiene)]

#[cfg(not(verus_only))]
compile_error!("Run make verify with the Verus compiler");

#[path = "../synctv-core/src/models/permission.rs"]
pub mod permission;

#[path = "../synctv-quic/src/frame_budget/header.rs"]
mod header;

#[path = "../synctv-core/src/cache/consistency/decision.rs"]
mod decision;

#[path = "../synctv-proxy/src/slice_cache/bounds.rs"]
mod bounds;

#[path = "../synctv-proxy/src/cors_policy.rs"]
mod cors_policy;

#[path = "../synctv-proxy/src/redirect_header_policy.rs"]
mod redirect_header_policy;

#[path = "../synctv-core/src/service/auth/brute_force/lockout.rs"]
mod lockout;

#[path = "../synctv-common/src/ssrf/destination.rs"]
mod destination;

#[path = "../synctv-core/src/service/auth/jwt/expiration.rs"]
mod expiration;

#[path = "../synctv-core/src/models/room/play_mode.rs"]
mod play_mode;

#[path = "../synctv-core/src/service/room/member_role_policy/overrides.rs"]
mod overrides;

#[path = "../synctv-realtime/src/sync/connection_manager/reservation.rs"]
mod reservation;

#[path = "../synctv-xiu/src/rtmp/chunk/unpacketizer/budget.rs"]
mod budget;

#[path = "../synctv-cluster/src/leader/timing.rs"]
mod timing;

#[path = "../synctv-core/src/service/publish_key/lifecycle.rs"]
mod lifecycle;

#[path = "../synctv-core/src/models/user/access.rs"]
mod account_access;

#[path = "../synctv-core/src/service/room/member_admission/admission.rs"]
mod room_admission;

#[path = "../synctv-core/src/service/auth/validator/bearer.rs"]
mod bearer;

#[path = "../synctv-core/src/service/user/factors.rs"]
mod factors;

#[path = "../synctv-media-providers/src/circuit_breaker/state.rs"]
mod breaker_state;

#[path = "../synctv-adapter/src/public_id/typed.rs"]
mod typed_id;

#[path = "../synctv-core/src/service/rate_limit/window.rs"]
mod rate_window;

#[path = "../synctv-livestream/src/util/playlist_field.rs"]
mod playlist_field;

#[path = "../synctv-api-common/src/impls/messaging/event_policy/scope.rs"]
mod disconnect_scope;

#[path = "../synctv-api-http/src/http/websocket/origin.rs"]
mod origin;

#[path = "../synctv-core/src/service/room/resource_access/availability.rs"]
mod room_availability;

#[path = "../synctv-management/src/lifecycle/transition.rs"]
mod shutdown;

#[path = "../synctv/src/rtmp_auth/access.rs"]
mod publish_access;

#[path = "../synctv-common/src/validation/encoded_dot.rs"]
mod encoded_dot;

#[path = "../synctv-core/src/service/ws_ticket/memory_store/claim.rs"]
mod ticket_claim;

#[path = "../synctv-api-grpc/src/grpc/registration.rs"]
mod grpc_registration;

#[path = "../synctv-proxy/src/slice_cache/backend/file_format/layout.rs"]
mod cache_file_layout;

#[path = "../synctv-core/src/repository/outbox_retry.rs"]
mod outbox_retry;

#[path = "../synctv-core/src/service/file_storage/part_count.rs"]
mod upload_part_count;

#[path = "../synctv-quic/src/bridge/body_budget.rs"]
mod body_budget;

#[path = "../synctv-core/src/service/user/totp/window.rs"]
mod totp_window;

#[path = "../synctv-core/src/service/file_storage/read_bounds.rs"]
mod read_bounds;
