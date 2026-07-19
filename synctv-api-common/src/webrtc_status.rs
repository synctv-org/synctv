pub fn to_proto_status(
    status: &synctv_core::service::WebRtcRuntimeStatus,
) -> synctv_proto::client::WebRtcStatus {
    synctv_proto::client::WebRtcStatus {
        mode: status.mode.as_str().to_string(),
        builtin_stun_state: status.builtin_stun_state.as_str().to_string(),
        builtin_stun_configured: status.builtin_stun_configured,
        reason: status.reason.as_str().to_string(),
        local_addr: status.local_addr.clone(),
        external_addr: status.external_addr.clone(),
        message: status.message.clone(),
    }
}
