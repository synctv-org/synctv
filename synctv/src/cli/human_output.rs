use serde::Serialize;
use serde_json::Value;
use synctv_common::time as app_time;

#[cfg(test)]
use anyhow::Result;

use super::args::{
    CLI_ADMIN_NAMED_PERMISSIONS, CLI_MEMBER_NAMED_PERMISSIONS, CLI_NAMED_PERMISSIONS,
};
use super::output_dto::{
    GetPlaybackCliOutput, KickStreamCliOutput, PlaybackStartCliOutput, PlaybackStopCliOutput,
    UserMutationCliOutput,
};

pub(in crate::cli) trait ToHuman {
    type Human: Serialize;

    fn to_human(&self) -> Self::Human;
}

impl ToHuman for String {
    type Human = Self;

    fn to_human(&self) -> Self::Human {
        self.clone()
    }
}

impl ToHuman for bool {
    type Human = Self;

    fn to_human(&self) -> Self::Human {
        *self
    }
}

impl ToHuman for i32 {
    type Human = Self;

    fn to_human(&self) -> Self::Human {
        *self
    }
}

impl ToHuman for i64 {
    type Human = Self;

    fn to_human(&self) -> Self::Human {
        *self
    }
}

impl ToHuman for u32 {
    type Human = Self;

    fn to_human(&self) -> Self::Human {
        *self
    }
}

impl ToHuman for u64 {
    type Human = Self;

    fn to_human(&self) -> Self::Human {
        *self
    }
}

impl ToHuman for f64 {
    type Human = Self;

    fn to_human(&self) -> Self::Human {
        *self
    }
}

impl<T> ToHuman for Option<T>
where
    T: ToHuman,
{
    type Human = Option<T::Human>;

    fn to_human(&self) -> Self::Human {
        self.as_ref().map(ToHuman::to_human)
    }
}

impl<T> ToHuman for Vec<T>
where
    T: ToHuman,
{
    type Human = Vec<T::Human>;

    fn to_human(&self) -> Self::Human {
        self.iter().map(ToHuman::to_human).collect()
    }
}

impl<K, V> ToHuman for std::collections::HashMap<K, V>
where
    K: Clone + Eq + std::hash::Hash + Serialize,
    V: ToHuman,
{
    type Human = std::collections::HashMap<K, V::Human>;

    fn to_human(&self) -> Self::Human {
        self.iter()
            .map(|(key, value)| (key.clone(), value.to_human()))
            .collect()
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanAdminUser {
    id: String,
    username: String,
    email: String,
    role: String,
    status: String,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanRoom {
    id: String,
    name: String,
    created_by: String,
    status: String,
    settings: Value,
    created_at: String,
    member_count: i32,
    description: String,
    updated_at: String,
    is_banned: bool,
    availability: String,
    version: i64,
    favorited: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanManagedRoom {
    id: String,
    name: String,
    creator_id: String,
    creator_username: String,
    creator_status: String,
    status: String,
    settings: Value,
    member_count: i32,
    created_at: String,
    updated_at: String,
    description: String,
    is_banned: bool,
    version: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanRoomCategory {
    id: String,
    key: String,
    name: String,
    description: String,
    sort_order: i32,
    is_enabled: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanRoomLabel {
    id: String,
    key: String,
    name: String,
    description: String,
    color: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    category_id: Option<String>,
    sort_order: i32,
    is_enabled: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanRoomMember {
    room_id: String,
    user_id: String,
    username: String,
    remark_name: String,
    display_tag: String,
    role: String,
    permissions: u64,
    permission_names: Vec<String>,
    added_permissions: u64,
    added_permission_names: Vec<String>,
    removed_permissions: u64,
    removed_permission_names: Vec<String>,
    admin_added_permissions: u64,
    admin_added_permission_names: Vec<String>,
    admin_removed_permissions: u64,
    admin_removed_permission_names: Vec<String>,
    joined_at: String,
    is_online: bool,
    connection_count: i32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanChatMessage {
    id: String,
    room_id: String,
    user_id: String,
    username: Option<String>,
    content: String,
    timestamp: String,
    display_position: String,
    display_color: String,
    client_message_id: String,
    status: String,
    version: i64,
    edited_at: String,
    deleted_at: String,
    reply_to_message_id: String,
    attachments: Vec<synctv_proto::client::ChatAttachment>,
    deleted_by_user_id: String,
    delete_reason: String,
    playback_media_id: String,
    playback_playlist_id: String,
    playback_target: Value,
    playback_target_hash: String,
    playback_position_seconds: Option<f64>,
    reactions: Vec<synctv_proto::client::ChatReactionSummary>,
    reaction_count: i32,
    metadata: Value,
    mentions: Vec<synctv_proto::client::ChatMention>,
    pin: Option<HumanChatMessagePin>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanChatMessagePin {
    pinned_by_user_id: String,
    pinned_by_username: String,
    note: String,
    pinned_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanProviderInstance {
    name: String,
    endpoint: String,
    comment: String,
    timeout_seconds: u32,
    tls: bool,
    insecure_tls: bool,
    providers: Vec<String>,
    enabled: bool,
    status: String,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanPlaylist {
    id: String,
    room_id: String,
    name: String,
    parent_id: String,
    position: f64,
    is_dynamic: bool,
    source_provider: String,
    provider_instance_name: String,
    item_count: i32,
    created_at: String,
    updated_at: String,
    availability: String,
    version: i64,
    source_config: Value,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanMedia {
    id: String,
    room_id: String,
    source_provider: String,
    name: String,
    metadata: Value,
    position: f64,
    added_at: String,
    creator_id: String,
    provider_instance_name: String,
    source_config: Value,
    availability: String,
    version: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanPlaybackState {
    room_id: String,
    playing_media_id: String,
    position: f64,
    speed: f64,
    is_playing: bool,
    updated_at: String,
    version: i64,
    playing_playlist_id: String,
    target_hash: String,
    target: Value,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanReviewRequest {
    id: String,
    status: String,
    requested_at: String,
    reviewed_at: String,
    reviewed_by: Option<String>,
    rejection_reason: Option<String>,
    username: String,
    email: String,
    signup_method: i32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanRoomCreationReview {
    id: String,
    status: String,
    requested_at: String,
    reviewed_at: String,
    reviewed_by: Option<String>,
    rejection_reason: Option<String>,
    requested_by: String,
    requested_by_username: String,
    name: String,
    description: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanRoomJoinReview {
    id: String,
    status: String,
    requested_at: String,
    reviewed_at: String,
    reviewed_by: Option<String>,
    rejection_reason: Option<String>,
    room_id: String,
    room_name: String,
    user_id: String,
    username: String,
    requested_role: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanBanRecord {
    id: String,
    target_type: String,
    user_id: String,
    username: String,
    room_id: String,
    room_name: String,
    banned_by: String,
    banned_by_username: String,
    reason: String,
    starts_at: String,
    ends_at: String,
    revoked_at: String,
    revoked_by: String,
    is_active: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanCreatePublishKeyResponse {
    publish_key: String,
    rtmp_url: String,
    stream_key: String,
    expires_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanStreamPublisherInfo {
    user_id: String,
    started_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanGetStreamInfoResponse {
    active: bool,
    publisher: Option<HumanStreamPublisherInfo>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanStreamEntry {
    media_id: String,
    active: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanListRoomStreamsResponse {
    streams: Vec<HumanStreamEntry>,
    total: i32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanSliceCacheConfigInfo {
    engine_enabled: bool,
    backend: String,
    file_cache_dir: String,
    slice_size: u64,
    max_cache_size: u64,
    segment_ttl_secs: u64,
    stale_max_age_secs: u64,
    stale_while_revalidate: bool,
    eviction_interval_secs: u64,
    watermark_ratio: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanSliceCacheStatsResponse {
    config: Option<HumanSliceCacheConfigInfo>,
    current_size_bytes: u64,
    entry_count: u64,
    metadata_entries: u64,
    updating_entries: u64,
    lock_count: u64,
    usage_ratio: f64,
    node_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanSliceCacheNodeFailure {
    node_id: String,
    error: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanGetSliceCacheStatsResponse {
    nodes: Vec<HumanSliceCacheStatsResponse>,
    failures: Vec<HumanSliceCacheNodeFailure>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanPurgeSliceCacheNodeResult {
    node_id: String,
    success: bool,
    removed_entries: u64,
    freed_bytes: u64,
    stats: Option<HumanSliceCacheStatsResponse>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanPurgeSliceCacheResponse {
    success: bool,
    removed_entries: u64,
    freed_bytes: u64,
    stats: Option<HumanSliceCacheStatsResponse>,
    nodes: Vec<HumanPurgeSliceCacheNodeResult>,
    failures: Vec<HumanSliceCacheNodeFailure>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanEvictExpiredSliceCacheNodeResult {
    node_id: String,
    success: bool,
    removed_expired_entries: u64,
    stats: Option<HumanSliceCacheStatsResponse>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanEvictExpiredSliceCacheResponse {
    success: bool,
    removed_expired_entries: u64,
    stats: Option<HumanSliceCacheStatsResponse>,
    nodes: Vec<HumanEvictExpiredSliceCacheNodeResult>,
    failures: Vec<HumanSliceCacheNodeFailure>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanServerStateSummary {
    status: String,
    healthy_nodes: i64,
    degraded_nodes: i64,
    unhealthy_nodes: i64,
    failed_nodes: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanServerStateDatabasePool {
    size: u32,
    idle_connections: u32,
    active_connections: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanServerStateDatabase {
    status: String,
    host: String,
    port: u32,
    database: String,
    max_connections: u32,
    min_connections: u32,
    connect_timeout: String,
    idle_timeout: String,
    max_lifetime: String,
    primary_pool: Option<HumanServerStateDatabasePool>,
    read_pool_enabled: bool,
    read_host: String,
    read_port: u32,
    read_pool: Option<HumanServerStateDatabasePool>,
    message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanServerStateRedis {
    status: String,
    configured: bool,
    deployment_mode: String,
    database: i64,
    key_prefix: String,
    connect_timeout: String,
    response_timeout: String,
    pipeline_buffer_size: u64,
    sentinel_master_name: String,
    sentinel_node_count: u32,
    ping_latency: Option<String>,
    message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanServerStateClusterNode {
    node_id: String,
    api_address: String,
    last_heartbeat: String,
    epoch: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanServerStateCluster {
    status: String,
    enabled: bool,
    discovery_mode: String,
    distributed_realtime_enabled: bool,
    node_id_empty: bool,
    routable_node_count: u32,
    nodes: Vec<HumanServerStateClusterNode>,
    message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanServerStateWsTicket {
    status: String,
    cross_node_capable: bool,
    message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanServerStateEmail {
    status: String,
    configured: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanServerStateLivestream {
    status: String,
    configured: bool,
    active_publisher_count: u64,
    active_room_count: u64,
    rtmp_port: u32,
    public_rtmp_host: String,
    gop_cache_size: u32,
    gop_cache_max_memory: String,
    stream_timeout: String,
    hls_storage_backend: String,
    hls_storage_path: String,
    hls_memory_max: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanServerStateMemory {
    status: String,
    used: Option<String>,
    total: Option<String>,
    available: Option<String>,
    usage: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanServerStateRealtime {
    distributed_enabled: bool,
    connection_count: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanServerStateWebRtc {
    status: String,
    mode: String,
    builtin_stun_configured: bool,
    builtin_stun_state: String,
    reason: String,
    local_addr: String,
    external_addr: String,
    message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanServerStateSliceCache {
    status: String,
    engine_enabled: bool,
    backend: String,
    file_cache_dir: String,
    slice_size: String,
    max_cache_size: String,
    segment_ttl: String,
    stale_max_age: String,
    stale_while_revalidate: bool,
    eviction_interval: String,
    watermark: String,
    current_size: String,
    entry_count: u64,
    metadata_entries: u64,
    updating_entries: u64,
    lock_count: u64,
    usage: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanServerStateCpu {
    status: String,
    available_parallelism: u32,
    current_load_1m: Option<String>,
    load_ratio_1m: Option<String>,
    load_average_1m: Option<String>,
    load_average_5m: Option<String>,
    load_average_15m: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanServerStateNode {
    node_id: String,
    status: String,
    updated_at: String,
    version: String,
    api_address: String,
    realtime: Option<HumanServerStateRealtime>,
    database: Option<HumanServerStateDatabase>,
    redis: Option<HumanServerStateRedis>,
    cluster: Option<HumanServerStateCluster>,
    ws_ticket: Option<HumanServerStateWsTicket>,
    email: Option<HumanServerStateEmail>,
    livestream: Option<HumanServerStateLivestream>,
    memory: Option<HumanServerStateMemory>,
    webrtc: Option<HumanServerStateWebRtc>,
    cpu: Option<HumanServerStateCpu>,
    slice_cache: Option<HumanServerStateSliceCache>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanServerStateNodeFailure {
    node_id: String,
    error: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanGetServerStateResponse {
    scope: String,
    summary: Option<HumanServerStateSummary>,
    nodes: Vec<HumanServerStateNode>,
    failures: Vec<HumanServerStateNodeFailure>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanUserAuthFactors {
    password: bool,
    webauthn: bool,
    email: bool,
    eligible_count: i32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanUserPreferences {
    two_factor_enabled: bool,
    notifications: Option<HumanUserNotificationPreferences>,
    settings: Value,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanUserNotificationPreferences {
    room_invitation_in_app: bool,
    room_event_in_app: bool,
    system_announcement_in_app: bool,
    room_invitation_email: bool,
    room_event_email: bool,
    system_announcement_email: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanUserPreferencesResponse<T> {
    user: Option<T>,
    preferences: Option<HumanUserPreferences>,
    auth_factors: Option<HumanUserAuthFactors>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanUserMutationCliOutput {
    success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    user: Option<HumanAdminUser>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanUsersResponse<T> {
    users: Vec<T>,
    total: i32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanAdminsResponse<T> {
    admins: Vec<T>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanRoomsResponse<T> {
    rooms: Vec<T>,
    total: i32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanRoomResponse<T> {
    room: Option<T>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanRoomMembersResponse<T> {
    members: Vec<T>,
    total: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanProviderInstancesResponse<T> {
    instances: Vec<T>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanProviderNamesResponse {
    instances: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanProviderBackendsResponse {
    backends: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanProviderInstanceResponse<T> {
    instance: Option<T>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanPlaylistsResponse<T> {
    playlists: Vec<T>,
    total: i32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanMediaBatchResponse<T> {
    moved_count: i32,
    media: Vec<T>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanPlaylistItemsResponse<P, M> {
    playlists: Vec<P>,
    media: Vec<M>,
    total: Option<u64>,
    folder_count: u64,
    file_count: u64,
    dynamic_items: Vec<synctv_proto::client::PlaylistItem>,
    current_path: Vec<synctv_proto::client::PlaylistBrowsePathNode>,
    version: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanGetPlaybackResponse<T> {
    playback_state: Option<T>,
    playback: Option<synctv_proto::client::Playback>,
    #[serde(skip_serializing_if = "Option::is_none")]
    default_mode: Option<String>,
    pull_urls: Vec<super::output_dto::PlaybackPullUrlCliOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    default_pull_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    default_absolute_pull_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hls_pull_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hls_absolute_pull_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    flv_pull_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    flv_absolute_pull_url: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanGetRoomWithPlaybackResponse<R, P> {
    room: Option<R>,
    playback_state: Option<P>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanJoinRoomResponse<R, P, M> {
    room: Option<R>,
    playback_state: Option<P>,
    members: Vec<M>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanGetPlaylistResponse<T> {
    playlist: Option<T>,
    child_folder_count: i32,
    media_count: i32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanReviewRequestsResponse<T> {
    reviews: Vec<T>,
    total: i32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanApproveReviewRequestResponse<R, T> {
    review: Option<R>,
    result: Option<T>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanApproveRoomJoinReviewResponse<R, M> {
    review: Option<R>,
    member: Option<M>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanApproveUserRegistrationReviewResponse<R, U> {
    review: Option<R>,
    user: Option<U>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanBanRecordsResponse<T> {
    bans: Vec<T>,
    total: i32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanRoomCategoriesResponse<T> {
    categories: Vec<T>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanRoomLabelsResponse<T> {
    labels: Vec<T>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanChatMessagesResponse<T> {
    messages: Vec<T>,
    next_cursor: String,
    event_cursor: Option<synctv_proto::client::EventCursor>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct HumanDeleteResponse {
    success: bool,
}

fn proto_json_value<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or_else(|_| Value::String("<invalid json>".into()))
}

fn source_config_json<T: serde::Serialize>(config: Option<&T>) -> Value {
    config
        .and_then(|config| serde_json::to_value(config).ok())
        .unwrap_or(Value::Null)
}

fn humanize_timestamp(raw: i64) -> String {
    if raw <= 0 {
        return "unset".to_string();
    }

    app_time::format_timestamp_secs_display(raw).unwrap_or_else(|| raw.to_string())
}

fn humanize_bytes(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut divisor = 1u128;
    let mut unit_index = 0usize;
    let bytes_u128 = u128::from(bytes);
    while unit_index + 1 < UNITS.len() {
        let next_divisor = divisor.saturating_mul(1024);
        if bytes_u128 < next_divisor {
            break;
        }
        divisor = next_divisor;
        unit_index += 1;
    }
    let unit = UNITS[unit_index];
    if unit_index == 0 {
        format!("{bytes} {unit}")
    } else {
        let scaled = (bytes_u128.saturating_mul(100) + divisor / 2) / divisor;
        format!("{}.{:02} {unit}", scaled / 100, scaled % 100)
    }
}

fn humanize_mebibytes(mib: u64) -> String {
    humanize_bytes(mib.saturating_mul(1024).saturating_mul(1024))
}

fn humanize_seconds(seconds: u64) -> String {
    humantime::format_duration(std::time::Duration::from_secs(seconds)).to_string()
}

fn humanize_percent_value(percent: f64) -> String {
    format!("{percent:.2}%")
}

fn humanize_ratio_percent(ratio: f64) -> String {
    humanize_percent_value(ratio * 100.0)
}

fn humanize_optional_percent(value: Option<f64>) -> Option<String> {
    value.map(humanize_percent_value)
}

fn humanize_optional_ratio(value: Option<f64>) -> Option<String> {
    value.map(humanize_ratio_percent)
}

fn humanize_optional_bytes(value: Option<u64>) -> Option<String> {
    value.map(humanize_bytes)
}

fn humanize_optional_load(value: Option<f64>) -> Option<String> {
    value.map(|load| format!("{load:.2}"))
}

fn humanize_optional_millis(value: Option<f64>) -> Option<String> {
    value.map(|millis| format!("{millis:.2} ms"))
}

impl ToHuman for synctv_proto::admin::AdminUser {
    type Human = HumanAdminUser;

    fn to_human(&self) -> Self::Human {
        HumanAdminUser {
            id: self.id.clone(),
            username: self.username.clone(),
            email: self.email.clone(),
            role: humanize_user_role(i64::from(self.role)).unwrap_or_else(|| self.role.to_string()),
            status: humanize_user_status(i64::from(self.status))
                .unwrap_or_else(|| self.status.to_string()),
            created_at: humanize_timestamp(self.created_at),
            updated_at: humanize_timestamp(self.updated_at),
        }
    }
}

impl ToHuman for synctv_proto::client::Room {
    type Human = HumanRoom;

    fn to_human(&self) -> Self::Human {
        HumanRoom {
            id: self.id.clone(),
            name: self.name.clone(),
            created_by: self.created_by.clone(),
            status: humanize_room_status(i64::from(self.status))
                .unwrap_or_else(|| self.status.to_string()),
            settings: proto_json_value(&self.settings),
            created_at: humanize_timestamp(self.created_at),
            member_count: self.member_count,
            description: self.description.clone(),
            updated_at: humanize_timestamp(self.updated_at),
            is_banned: self.is_banned,
            availability: humanize_resource_availability(i64::from(self.availability))
                .unwrap_or_else(|| self.availability.to_string()),
            version: self.version,
            favorited: self.favorited,
        }
    }
}

impl ToHuman for synctv_proto::admin::Room {
    type Human = HumanManagedRoom;

    fn to_human(&self) -> Self::Human {
        HumanManagedRoom {
            id: self.id.clone(),
            name: self.name.clone(),
            creator_id: self.creator_id.clone(),
            creator_username: self.creator_username.clone(),
            creator_status: humanize_user_status(i64::from(self.creator_status))
                .unwrap_or_else(|| self.creator_status.to_string()),
            status: humanize_room_status(i64::from(self.status))
                .unwrap_or_else(|| self.status.to_string()),
            settings: proto_json_value(&self.settings),
            member_count: self.member_count,
            created_at: humanize_timestamp(self.created_at),
            updated_at: humanize_timestamp(self.updated_at),
            description: self.description.clone(),
            is_banned: self.is_banned,
            version: self.version,
        }
    }
}

impl ToHuman for synctv_proto::client::RoomCategory {
    type Human = HumanRoomCategory;

    fn to_human(&self) -> Self::Human {
        HumanRoomCategory {
            id: self.id.clone(),
            key: self.key.clone(),
            name: self.name.clone(),
            description: self.description.clone(),
            sort_order: self.sort_order,
            is_enabled: self.is_enabled,
        }
    }
}

impl ToHuman for synctv_proto::client::RoomLabel {
    type Human = HumanRoomLabel;

    fn to_human(&self) -> Self::Human {
        HumanRoomLabel {
            id: self.id.clone(),
            key: self.key.clone(),
            name: self.name.clone(),
            description: self.description.clone(),
            color: self.color.clone(),
            category_id: (!self.category_id.is_empty()).then(|| self.category_id.clone()),
            sort_order: self.sort_order,
            is_enabled: self.is_enabled,
        }
    }
}

impl ToHuman for synctv_proto::admin::UserRegistrationReview {
    type Human = HumanReviewRequest;

    fn to_human(&self) -> Self::Human {
        HumanReviewRequest {
            id: self.id.clone(),
            status: synctv_proto::common::ReviewStatus::try_from(self.status)
                .map_or_else(|_| self.status.to_string(), |value| format!("{value:?}")),
            requested_at: humanize_timestamp(self.requested_at),
            reviewed_at: humanize_timestamp(self.reviewed_at),
            reviewed_by: self.reviewed_by.clone(),
            rejection_reason: self.rejection_reason.clone(),
            username: self.username.clone(),
            email: self.email.clone(),
            signup_method: self.signup_method,
        }
    }
}

impl ToHuman for synctv_proto::admin::RoomCreationReview {
    type Human = HumanRoomCreationReview;

    fn to_human(&self) -> Self::Human {
        HumanRoomCreationReview {
            id: self.id.clone(),
            status: synctv_proto::common::ReviewStatus::try_from(self.status)
                .map_or_else(|_| self.status.to_string(), |value| format!("{value:?}")),
            requested_at: humanize_timestamp(self.requested_at),
            reviewed_at: humanize_timestamp(self.reviewed_at),
            reviewed_by: self.reviewed_by.clone(),
            rejection_reason: self.rejection_reason.clone(),
            requested_by: self.requested_by.clone(),
            requested_by_username: self.requested_by_username.clone(),
            name: self.name.clone(),
            description: self.description.clone(),
        }
    }
}

impl ToHuman for synctv_proto::admin::RoomJoinReview {
    type Human = HumanRoomJoinReview;

    fn to_human(&self) -> Self::Human {
        HumanRoomJoinReview {
            id: self.id.clone(),
            status: synctv_proto::common::ReviewStatus::try_from(self.status)
                .map_or_else(|_| self.status.to_string(), |value| format!("{value:?}")),
            requested_at: humanize_timestamp(self.requested_at),
            reviewed_at: humanize_timestamp(self.reviewed_at),
            reviewed_by: self.reviewed_by.clone(),
            rejection_reason: self.rejection_reason.clone(),
            room_id: self.room_id.clone(),
            room_name: self.room_name.clone(),
            user_id: self.user_id.clone(),
            username: self.username.clone(),
            requested_role: humanize_room_member_role(i64::from(self.requested_role))
                .unwrap_or_else(|| self.requested_role.to_string()),
        }
    }
}

impl ToHuman for synctv_proto::client::RoomJoinReview {
    type Human = HumanRoomJoinReview;

    fn to_human(&self) -> Self::Human {
        HumanRoomJoinReview {
            id: self.id.clone(),
            status: synctv_proto::common::ReviewStatus::try_from(self.status)
                .map_or_else(|_| self.status.to_string(), |value| format!("{value:?}")),
            requested_at: humanize_timestamp(self.requested_at),
            reviewed_at: humanize_timestamp(self.reviewed_at),
            reviewed_by: self.reviewed_by.clone(),
            rejection_reason: self.rejection_reason.clone(),
            room_id: self.room_id.clone(),
            room_name: String::new(),
            user_id: self.user_id.clone(),
            username: self.username.clone(),
            requested_role: humanize_room_member_role(i64::from(self.requested_role))
                .unwrap_or_else(|| self.requested_role.to_string()),
        }
    }
}

impl ToHuman for synctv_proto::admin::BanRecord {
    type Human = HumanBanRecord;

    fn to_human(&self) -> Self::Human {
        HumanBanRecord {
            id: self.id.clone(),
            target_type: synctv_proto::admin::BanTargetType::try_from(self.target_type)
                .map_or_else(
                    |_| self.target_type.to_string(),
                    |value| format!("{value:?}"),
                ),
            user_id: self.user_id.clone(),
            username: self.username.clone(),
            room_id: self.room_id.clone(),
            room_name: self.room_name.clone(),
            banned_by: self.banned_by.clone(),
            banned_by_username: self.banned_by_username.clone(),
            reason: self.reason.clone(),
            starts_at: humanize_timestamp(self.starts_at),
            ends_at: humanize_timestamp(self.ends_at),
            revoked_at: humanize_timestamp(self.revoked_at),
            revoked_by: self.revoked_by.clone(),
            is_active: self.is_active,
        }
    }
}

impl ToHuman for synctv_proto::admin::ListUserRegistrationReviewsResponse {
    type Human = HumanReviewRequestsResponse<HumanReviewRequest>;

    fn to_human(&self) -> Self::Human {
        HumanReviewRequestsResponse {
            reviews: self.reviews.iter().map(ToHuman::to_human).collect(),
            total: self.total,
        }
    }
}

impl ToHuman for synctv_proto::admin::ApproveUserRegistrationReviewResponse {
    type Human = HumanApproveUserRegistrationReviewResponse<HumanReviewRequest, HumanAdminUser>;

    fn to_human(&self) -> Self::Human {
        HumanApproveUserRegistrationReviewResponse {
            review: self.review.as_ref().map(ToHuman::to_human),
            user: self.user.as_ref().map(ToHuman::to_human),
        }
    }
}

impl ToHuman for synctv_proto::admin::ListRoomCreationReviewsResponse {
    type Human = HumanReviewRequestsResponse<HumanRoomCreationReview>;

    fn to_human(&self) -> Self::Human {
        HumanReviewRequestsResponse {
            reviews: self.reviews.iter().map(ToHuman::to_human).collect(),
            total: self.total,
        }
    }
}

impl ToHuman for synctv_proto::admin::ApproveRoomCreationReviewResponse {
    type Human = HumanApproveReviewRequestResponse<HumanRoomCreationReview, HumanManagedRoom>;

    fn to_human(&self) -> Self::Human {
        HumanApproveReviewRequestResponse {
            review: self.review.as_ref().map(ToHuman::to_human),
            result: self.room.as_ref().map(ToHuman::to_human),
        }
    }
}

impl ToHuman for synctv_proto::admin::ListRoomJoinReviewsResponse {
    type Human = HumanReviewRequestsResponse<HumanRoomJoinReview>;

    fn to_human(&self) -> Self::Human {
        HumanReviewRequestsResponse {
            reviews: self.reviews.iter().map(ToHuman::to_human).collect(),
            total: self.total,
        }
    }
}

impl ToHuman for synctv_proto::admin::ApproveRoomJoinReviewResponse {
    type Human = HumanApproveRoomJoinReviewResponse<HumanRoomJoinReview, HumanRoomMember>;

    fn to_human(&self) -> Self::Human {
        HumanApproveRoomJoinReviewResponse {
            review: self.review.as_ref().map(ToHuman::to_human),
            member: self.member.as_ref().map(ToHuman::to_human),
        }
    }
}

impl ToHuman for synctv_proto::client::ListRoomJoinReviewsResponse {
    type Human = HumanReviewRequestsResponse<HumanRoomJoinReview>;

    fn to_human(&self) -> Self::Human {
        HumanReviewRequestsResponse {
            reviews: self.reviews.iter().map(ToHuman::to_human).collect(),
            total: self.total,
        }
    }
}

impl ToHuman for synctv_proto::client::ApproveRoomJoinReviewResponse {
    type Human = HumanApproveRoomJoinReviewResponse<HumanRoomJoinReview, HumanRoomMember>;

    fn to_human(&self) -> Self::Human {
        HumanApproveRoomJoinReviewResponse {
            review: self.review.as_ref().map(ToHuman::to_human),
            member: self.member.as_ref().map(ToHuman::to_human),
        }
    }
}

impl ToHuman for synctv_proto::admin::ListBanRecordsResponse {
    type Human = HumanBanRecordsResponse<HumanBanRecord>;

    fn to_human(&self) -> Self::Human {
        HumanBanRecordsResponse {
            bans: self.bans.iter().map(ToHuman::to_human).collect(),
            total: self.total,
        }
    }
}

impl ToHuman for synctv_proto::common::RoomMember {
    type Human = HumanRoomMember;

    fn to_human(&self) -> Self::Human {
        HumanRoomMember {
            room_id: self.room_id.clone(),
            user_id: self.user_id.clone(),
            username: self.username.clone(),
            remark_name: self.remark_name.clone(),
            display_tag: self.display_tag.clone(),
            role: humanize_room_member_role(i64::from(self.role))
                .unwrap_or_else(|| self.role.to_string()),
            permissions: self.permissions,
            permission_names: humanize_permission_bits(self.permissions),
            added_permissions: self.added_permissions,
            added_permission_names: humanize_named_permission_bits(
                self.added_permissions,
                CLI_MEMBER_NAMED_PERMISSIONS,
            ),
            removed_permissions: self.removed_permissions,
            removed_permission_names: humanize_named_permission_bits(
                self.removed_permissions,
                CLI_MEMBER_NAMED_PERMISSIONS,
            ),
            admin_added_permissions: self.admin_added_permissions,
            admin_added_permission_names: humanize_named_permission_bits(
                self.admin_added_permissions,
                CLI_ADMIN_NAMED_PERMISSIONS,
            ),
            admin_removed_permissions: self.admin_removed_permissions,
            admin_removed_permission_names: humanize_named_permission_bits(
                self.admin_removed_permissions,
                CLI_ADMIN_NAMED_PERMISSIONS,
            ),
            joined_at: humanize_timestamp(self.joined_at),
            is_online: self.is_online,
            connection_count: self.connection_count,
        }
    }
}

impl ToHuman for synctv_proto::client::ChatMessagePin {
    type Human = HumanChatMessagePin;

    fn to_human(&self) -> Self::Human {
        HumanChatMessagePin {
            pinned_by_user_id: self.pinned_by_user_id.clone(),
            pinned_by_username: self.pinned_by_username.clone(),
            note: self.note.clone(),
            pinned_at: humanize_timestamp(self.pinned_at),
        }
    }
}

impl ToHuman for synctv_proto::client::ChatMessageReceive {
    type Human = HumanChatMessage;

    fn to_human(&self) -> Self::Human {
        HumanChatMessage {
            id: self.id.clone(),
            room_id: self.room_id.clone(),
            user_id: self.user_id.clone(),
            username: self.username.clone(),
            content: self.content.clone(),
            timestamp: humanize_timestamp(self.timestamp),
            display_position: self.display_position.clone(),
            display_color: self.display_color.clone(),
            client_message_id: self.client_message_id.clone(),
            status: humanize_chat_message_status(i64::from(self.status))
                .unwrap_or_else(|| self.status.to_string()),
            version: self.version,
            edited_at: humanize_timestamp(self.edited_at),
            deleted_at: humanize_timestamp(self.deleted_at),
            reply_to_message_id: self.reply_to_message_id.clone(),
            attachments: self.attachments.clone(),
            deleted_by_user_id: self.deleted_by_user_id.clone(),
            delete_reason: self.delete_reason.clone(),
            playback_media_id: self.playback_media_id.clone(),
            playback_playlist_id: self.playback_playlist_id.clone(),
            playback_target: proto_json_value(&self.playback_target),
            playback_target_hash: self.playback_target_hash.clone(),
            playback_position_seconds: self.playback_position_seconds,
            reactions: self.reactions.clone(),
            reaction_count: self.reaction_count,
            metadata: proto_json_value(&self.metadata),
            mentions: self.mentions.clone(),
            pin: self.pin.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::providers::common::ProviderInstance {
    type Human = HumanProviderInstance;

    fn to_human(&self) -> Self::Human {
        HumanProviderInstance {
            name: self.name.clone(),
            endpoint: self.endpoint.clone(),
            comment: self.comment.clone(),
            timeout_seconds: self.timeout_seconds,
            tls: self.tls,
            insecure_tls: self.insecure_tls,
            providers: humanize_source_providers(&self.providers),
            enabled: self.enabled,
            status: humanize_provider_instance_status(i64::from(self.status))
                .unwrap_or_else(|| self.status.to_string()),
            created_at: humanize_timestamp(self.created_at),
            updated_at: humanize_timestamp(self.updated_at),
        }
    }
}

impl ToHuman for synctv_proto::providers::rtmp::StreamPublisherInfo {
    type Human = HumanStreamPublisherInfo;

    fn to_human(&self) -> Self::Human {
        HumanStreamPublisherInfo {
            user_id: self.user_id.clone(),
            started_at: humanize_timestamp(self.started_at),
        }
    }
}

impl ToHuman for synctv_proto::client::StreamEntry {
    type Human = HumanStreamEntry;

    fn to_human(&self) -> Self::Human {
        HumanStreamEntry {
            media_id: self.media_id.clone(),
            active: self.active,
        }
    }
}

impl ToHuman for synctv_proto::client::Playlist {
    type Human = HumanPlaylist;

    fn to_human(&self) -> Self::Human {
        HumanPlaylist {
            id: self.id.clone(),
            room_id: self.room_id.clone(),
            name: self.name.clone(),
            parent_id: self.parent_id.clone(),
            position: self.position,
            is_dynamic: self.is_dynamic,
            source_provider: humanize_source_provider(self.source_provider),
            provider_instance_name: self.provider_instance_name.clone(),
            item_count: self.item_count,
            created_at: humanize_timestamp(self.created_at),
            updated_at: humanize_timestamp(self.updated_at),
            availability: humanize_resource_availability(i64::from(self.availability))
                .unwrap_or_else(|| self.availability.to_string()),
            version: self.version,
            source_config: source_config_json(self.source_config.as_ref()),
        }
    }
}

impl ToHuman for synctv_proto::client::Media {
    type Human = HumanMedia;

    fn to_human(&self) -> Self::Human {
        HumanMedia {
            id: self.id.clone(),
            room_id: self.room_id.clone(),
            source_provider: humanize_source_provider(self.source_provider),
            name: self.name.clone(),
            metadata: proto_json_value(&self.metadata),
            position: self.position,
            added_at: humanize_timestamp(self.added_at),
            creator_id: self.creator_id.clone(),
            provider_instance_name: self.provider_instance_name.clone(),
            source_config: source_config_json(self.source_config.as_ref()),
            availability: humanize_resource_availability(i64::from(self.availability))
                .unwrap_or_else(|| self.availability.to_string()),
            version: self.version,
        }
    }
}

impl ToHuman for synctv_proto::client::PlaybackState {
    type Human = HumanPlaybackState;

    fn to_human(&self) -> Self::Human {
        HumanPlaybackState {
            room_id: self.room_id.clone(),
            playing_media_id: self.playing_media_id.clone(),
            position: self.position,
            speed: self.speed,
            is_playing: self.is_playing,
            updated_at: humanize_timestamp(self.updated_at),
            version: self.version,
            playing_playlist_id: self.playing_playlist_id.clone(),
            target_hash: self.target_hash.clone(),
            target: proto_json_value(&self.target),
        }
    }
}

impl ToHuman for synctv_proto::admin::RuntimeSettings {
    type Human = Value;

    fn to_human(&self) -> Self::Human {
        proto_json_value(self)
    }
}

macro_rules! impl_identity_to_human {
    ($($ty:path),+ $(,)?) => {
        $(
            impl ToHuman for $ty {
                type Human = Self;

                fn to_human(&self) -> Self::Human {
                    self.clone()
                }
            }
        )+
    };
}

impl ToHuman for synctv_proto::client::UserAuthFactors {
    type Human = HumanUserAuthFactors;

    fn to_human(&self) -> Self::Human {
        HumanUserAuthFactors {
            password: self.password,
            webauthn: self.webauthn,
            email: self.email,
            eligible_count: self.eligible_count,
        }
    }
}

impl ToHuman for synctv_proto::client::UserPreferences {
    type Human = HumanUserPreferences;

    fn to_human(&self) -> Self::Human {
        HumanUserPreferences {
            two_factor_enabled: self.two_factor_enabled,
            notifications: self.notifications.map(|notifications| {
                HumanUserNotificationPreferences {
                    room_invitation_in_app: notifications.room_invitation_in_app,
                    room_event_in_app: notifications.room_event_in_app,
                    system_announcement_in_app: notifications.system_announcement_in_app,
                    room_invitation_email: notifications.room_invitation_email,
                    room_event_email: notifications.room_event_email,
                    system_announcement_email: notifications.system_announcement_email,
                }
            }),
            settings: proto_json_value(&self.settings),
        }
    }
}

impl ToHuman for synctv_proto::admin::GetUserPreferencesResponse {
    type Human = HumanUserPreferencesResponse<HumanAdminUser>;

    fn to_human(&self) -> Self::Human {
        HumanUserPreferencesResponse {
            user: self.user.to_human(),
            preferences: self.preferences.to_human(),
            auth_factors: self.auth_factors.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::admin::UpdateUserPreferencesResponse {
    type Human = HumanUserPreferencesResponse<HumanAdminUser>;

    fn to_human(&self) -> Self::Human {
        HumanUserPreferencesResponse {
            user: self.user.to_human(),
            preferences: self.preferences.to_human(),
            auth_factors: self.auth_factors.to_human(),
        }
    }
}

impl ToHuman for UserMutationCliOutput {
    type Human = HumanUserMutationCliOutput;

    fn to_human(&self) -> Self::Human {
        HumanUserMutationCliOutput {
            success: self.success,
            user: self.user.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::admin::ListUsersResponse {
    type Human = HumanUsersResponse<HumanAdminUser>;

    fn to_human(&self) -> Self::Human {
        HumanUsersResponse {
            users: self.users.to_human(),
            total: self.total,
        }
    }
}

impl ToHuman for synctv_proto::admin::GetUserRoomsResponse {
    type Human = HumanRoomsResponse<HumanManagedRoom>;

    fn to_human(&self) -> Self::Human {
        HumanRoomsResponse {
            rooms: self.rooms.to_human(),
            total: self.total,
        }
    }
}

impl ToHuman for synctv_proto::admin::ListRoomsResponse {
    type Human = HumanRoomsResponse<HumanManagedRoom>;

    fn to_human(&self) -> Self::Human {
        HumanRoomsResponse {
            rooms: self.rooms.to_human(),
            total: self.total,
        }
    }
}

impl ToHuman for synctv_proto::admin::ListRoomCategoriesResponse {
    type Human = HumanRoomCategoriesResponse<HumanRoomCategory>;

    fn to_human(&self) -> Self::Human {
        HumanRoomCategoriesResponse {
            categories: self.categories.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::admin::DeleteRoomCategoryResponse {
    type Human = HumanDeleteResponse;

    fn to_human(&self) -> Self::Human {
        HumanDeleteResponse {
            success: self.success,
        }
    }
}

impl ToHuman for synctv_proto::admin::ListRoomLabelsResponse {
    type Human = HumanRoomLabelsResponse<HumanRoomLabel>;

    fn to_human(&self) -> Self::Human {
        HumanRoomLabelsResponse {
            labels: self.labels.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::admin::DeleteRoomLabelResponse {
    type Human = HumanDeleteResponse;

    fn to_human(&self) -> Self::Human {
        HumanDeleteResponse {
            success: self.success,
        }
    }
}

impl ToHuman for synctv_proto::admin::GetRoomMembersResponse {
    type Human = HumanRoomMembersResponse<HumanRoomMember>;

    fn to_human(&self) -> Self::Human {
        HumanRoomMembersResponse {
            members: self.members.to_human(),
            total: self.total,
            version: None,
        }
    }
}

impl ToHuman for synctv_proto::providers::common::ListProviderInstancesResponse {
    type Human = HumanProviderInstancesResponse<HumanProviderInstance>;

    fn to_human(&self) -> Self::Human {
        HumanProviderInstancesResponse {
            instances: self.instances.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::providers::common::ProviderInstancesResponse {
    type Human = HumanProviderNamesResponse;

    fn to_human(&self) -> Self::Human {
        HumanProviderNamesResponse {
            instances: self.instances.clone(),
        }
    }
}

impl ToHuman for synctv_proto::providers::common::ProviderBackendsResponse {
    type Human = HumanProviderBackendsResponse;

    fn to_human(&self) -> Self::Human {
        HumanProviderBackendsResponse {
            backends: self.backends.clone(),
        }
    }
}

impl ToHuman for synctv_proto::providers::common::AddProviderInstanceResponse {
    type Human = HumanProviderInstanceResponse<HumanProviderInstance>;

    fn to_human(&self) -> Self::Human {
        HumanProviderInstanceResponse {
            instance: self.instance.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::providers::common::UpdateProviderInstanceResponse {
    type Human = HumanProviderInstanceResponse<HumanProviderInstance>;

    fn to_human(&self) -> Self::Human {
        HumanProviderInstanceResponse {
            instance: self.instance.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::providers::common::ReconnectProviderInstanceResponse {
    type Human = HumanProviderInstanceResponse<HumanProviderInstance>;

    fn to_human(&self) -> Self::Human {
        HumanProviderInstanceResponse {
            instance: self.instance.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::providers::common::EnableProviderInstanceResponse {
    type Human = HumanProviderInstanceResponse<HumanProviderInstance>;

    fn to_human(&self) -> Self::Human {
        HumanProviderInstanceResponse {
            instance: self.instance.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::providers::common::DisableProviderInstanceResponse {
    type Human = HumanProviderInstanceResponse<HumanProviderInstance>;

    fn to_human(&self) -> Self::Human {
        HumanProviderInstanceResponse {
            instance: self.instance.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::admin::ListAdminsResponse {
    type Human = HumanAdminsResponse<HumanAdminUser>;

    fn to_human(&self) -> Self::Human {
        HumanAdminsResponse {
            admins: self.admins.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::client::GetRoomResponse {
    type Human = HumanGetRoomWithPlaybackResponse<HumanRoom, HumanPlaybackState>;

    fn to_human(&self) -> Self::Human {
        HumanGetRoomWithPlaybackResponse {
            room: self.room.to_human(),
            playback_state: self.playback_state.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::client::JoinRoomResponse {
    type Human = HumanJoinRoomResponse<HumanRoom, HumanPlaybackState, HumanRoomMember>;

    fn to_human(&self) -> Self::Human {
        HumanJoinRoomResponse {
            room: self.room.to_human(),
            playback_state: self.playback_state.to_human(),
            members: self.members.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::client::ListRoomsResponse {
    type Human = HumanRoomsResponse<HumanRoom>;

    fn to_human(&self) -> Self::Human {
        HumanRoomsResponse {
            rooms: self.rooms.to_human(),
            total: self.total,
        }
    }
}

impl ToHuman for synctv_proto::client::FavoriteRoomResponse {
    type Human = HumanRoomResponse<HumanRoom>;

    fn to_human(&self) -> Self::Human {
        HumanRoomResponse {
            room: self.room.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::client::UnfavoriteRoomResponse {
    type Human = HumanRoomResponse<HumanRoom>;

    fn to_human(&self) -> Self::Human {
        HumanRoomResponse {
            room: self.room.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::client::ListFavoriteRoomsResponse {
    type Human = HumanRoomsResponse<HumanRoom>;

    fn to_human(&self) -> Self::Human {
        HumanRoomsResponse {
            rooms: self.rooms.to_human(),
            total: self.total,
        }
    }
}

impl ToHuman for synctv_proto::client::GetRoomMembersResponse {
    type Human = HumanRoomMembersResponse<HumanRoomMember>;

    fn to_human(&self) -> Self::Human {
        HumanRoomMembersResponse {
            members: self.members.to_human(),
            total: self.total,
            version: Some(self.version.clone()),
        }
    }
}

impl ToHuman for synctv_proto::client::SearchChatMessagesResponse {
    type Human = HumanChatMessagesResponse<HumanChatMessage>;

    fn to_human(&self) -> Self::Human {
        HumanChatMessagesResponse {
            messages: self.messages.to_human(),
            next_cursor: self.next_cursor.clone(),
            event_cursor: self.event_cursor.clone(),
        }
    }
}

impl ToHuman for synctv_proto::client::GetPlaylistResponse {
    type Human = HumanGetPlaylistResponse<HumanPlaylist>;

    fn to_human(&self) -> Self::Human {
        HumanGetPlaylistResponse {
            playlist: self.playlist.to_human(),
            child_folder_count: self.child_folder_count,
            media_count: self.media_count,
        }
    }
}

impl ToHuman for synctv_proto::client::ListPlaylistsResponse {
    type Human = HumanPlaylistsResponse<HumanPlaylist>;

    fn to_human(&self) -> Self::Human {
        HumanPlaylistsResponse {
            playlists: self.playlists.to_human(),
            total: self.total,
        }
    }
}

impl ToHuman for synctv_proto::client::MoveMediaResponse {
    type Human = HumanMediaBatchResponse<HumanMedia>;

    fn to_human(&self) -> Self::Human {
        HumanMediaBatchResponse {
            moved_count: self.moved_count,
            media: self.media.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::client::ListPlaylistItemsResponse {
    type Human = HumanPlaylistItemsResponse<HumanPlaylist, HumanMedia>;

    fn to_human(&self) -> Self::Human {
        HumanPlaylistItemsResponse {
            playlists: self.playlists.to_human(),
            media: self.media.to_human(),
            total: self.total,
            folder_count: self.folder_count,
            file_count: self.file_count,
            dynamic_items: self.dynamic_items.clone(),
            current_path: self.current_path.clone(),
            version: self.version.clone(),
        }
    }
}

impl ToHuman for GetPlaybackCliOutput {
    type Human = HumanGetPlaybackResponse<HumanPlaybackState>;

    fn to_human(&self) -> Self::Human {
        HumanGetPlaybackResponse {
            playback_state: self.playback_state.to_human(),
            playback: self.playback.clone(),
            default_mode: self.default_mode.clone(),
            pull_urls: self.pull_urls.clone(),
            default_pull_url: self.default_pull_url.clone(),
            default_absolute_pull_url: self.default_absolute_pull_url.clone(),
            hls_pull_url: self.hls_pull_url.clone(),
            hls_absolute_pull_url: self.hls_absolute_pull_url.clone(),
            flv_pull_url: self.flv_pull_url.clone(),
            flv_absolute_pull_url: self.flv_absolute_pull_url.clone(),
        }
    }
}

impl ToHuman for synctv_proto::providers::rtmp::CreatePublishKeyResponse {
    type Human = HumanCreatePublishKeyResponse;

    fn to_human(&self) -> Self::Human {
        HumanCreatePublishKeyResponse {
            publish_key: self.publish_key.clone(),
            rtmp_url: self.rtmp_url.clone(),
            stream_key: self.stream_key.clone(),
            expires_at: humanize_timestamp(self.expires_at),
        }
    }
}

impl ToHuman for synctv_proto::providers::rtmp::GetStreamInfoResponse {
    type Human = HumanGetStreamInfoResponse;

    fn to_human(&self) -> Self::Human {
        HumanGetStreamInfoResponse {
            active: self.active,
            publisher: self.publisher.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::client::ListRoomStreamsResponse {
    type Human = HumanListRoomStreamsResponse;

    fn to_human(&self) -> Self::Human {
        HumanListRoomStreamsResponse {
            streams: self.streams.to_human(),
            total: self.total,
        }
    }
}

impl ToHuman for synctv_proto::admin::SliceCacheConfigInfo {
    type Human = HumanSliceCacheConfigInfo;

    fn to_human(&self) -> Self::Human {
        HumanSliceCacheConfigInfo {
            engine_enabled: self.engine_enabled,
            backend: self.backend.clone(),
            file_cache_dir: self.file_cache_dir.clone(),
            slice_size: self.slice_size,
            max_cache_size: self.max_cache_size,
            segment_ttl_secs: self.segment_ttl_secs,
            stale_max_age_secs: self.stale_max_age_secs,
            stale_while_revalidate: self.stale_while_revalidate,
            eviction_interval_secs: self.eviction_interval_secs,
            watermark_ratio: self.watermark_ratio,
        }
    }
}

impl ToHuman for synctv_proto::admin::SliceCacheStatsNode {
    type Human = HumanSliceCacheStatsResponse;

    fn to_human(&self) -> Self::Human {
        HumanSliceCacheStatsResponse {
            config: self.config.to_human(),
            current_size_bytes: self.current_size_bytes,
            entry_count: self.entry_count,
            metadata_entries: self.metadata_entries,
            updating_entries: self.updating_entries,
            lock_count: self.lock_count,
            usage_ratio: self.usage_ratio,
            node_id: self.node_id.clone(),
        }
    }
}

impl ToHuman for synctv_proto::admin::SliceCacheNodeFailure {
    type Human = HumanSliceCacheNodeFailure;

    fn to_human(&self) -> Self::Human {
        HumanSliceCacheNodeFailure {
            node_id: self.node_id.clone(),
            error: self.error.clone(),
        }
    }
}

impl ToHuman for synctv_proto::admin::GetSliceCacheStatsResponse {
    type Human = HumanGetSliceCacheStatsResponse;

    fn to_human(&self) -> Self::Human {
        HumanGetSliceCacheStatsResponse {
            nodes: self.nodes.to_human(),
            failures: self.failures.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::admin::PurgeSliceCacheNodeResult {
    type Human = HumanPurgeSliceCacheNodeResult;

    fn to_human(&self) -> Self::Human {
        HumanPurgeSliceCacheNodeResult {
            node_id: self.node_id.clone(),
            success: self.success,
            removed_entries: self.removed_entries,
            freed_bytes: self.freed_bytes,
            stats: self.stats.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::admin::PurgeSliceCacheResponse {
    type Human = HumanPurgeSliceCacheResponse;

    fn to_human(&self) -> Self::Human {
        HumanPurgeSliceCacheResponse {
            success: self.success,
            removed_entries: self.removed_entries,
            freed_bytes: self.freed_bytes,
            stats: self.stats.to_human(),
            nodes: self.nodes.to_human(),
            failures: self.failures.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::admin::EvictExpiredSliceCacheNodeResult {
    type Human = HumanEvictExpiredSliceCacheNodeResult;

    fn to_human(&self) -> Self::Human {
        HumanEvictExpiredSliceCacheNodeResult {
            node_id: self.node_id.clone(),
            success: self.success,
            removed_expired_entries: self.removed_expired_entries,
            stats: self.stats.to_human(),
        }
    }
}

impl ToHuman for synctv_proto::admin::EvictExpiredSliceCacheResponse {
    type Human = HumanEvictExpiredSliceCacheResponse;

    fn to_human(&self) -> Self::Human {
        HumanEvictExpiredSliceCacheResponse {
            success: self.success,
            removed_expired_entries: self.removed_expired_entries,
            stats: self.stats.to_human(),
            nodes: self.nodes.to_human(),
            failures: self.failures.to_human(),
        }
    }
}

impl ToHuman for synctv_management::proto::ServerStateSummary {
    type Human = HumanServerStateSummary;

    fn to_human(&self) -> Self::Human {
        HumanServerStateSummary {
            status: humanize_server_state_node_status(self.status),
            healthy_nodes: self.healthy_nodes,
            degraded_nodes: self.degraded_nodes,
            unhealthy_nodes: self.unhealthy_nodes,
            failed_nodes: self.failed_nodes,
        }
    }
}

impl ToHuman for synctv_management::proto::ServerStateDatabasePool {
    type Human = HumanServerStateDatabasePool;

    fn to_human(&self) -> Self::Human {
        HumanServerStateDatabasePool {
            size: self.size,
            idle_connections: self.idle_connections,
            active_connections: self.active_connections,
        }
    }
}

impl ToHuman for synctv_management::proto::ServerStateDatabase {
    type Human = HumanServerStateDatabase;

    fn to_human(&self) -> Self::Human {
        HumanServerStateDatabase {
            status: humanize_server_state_database_status(self.status),
            host: self.host.clone(),
            port: self.port,
            database: self.database.clone(),
            max_connections: self.max_connections,
            min_connections: self.min_connections,
            connect_timeout: humanize_seconds(self.connect_timeout_seconds),
            idle_timeout: humanize_seconds(self.idle_timeout_seconds),
            max_lifetime: humanize_seconds(self.max_lifetime_seconds),
            primary_pool: self.primary_pool.to_human(),
            read_pool_enabled: self.read_pool_enabled,
            read_host: self.read_host.clone(),
            read_port: self.read_port,
            read_pool: self.read_pool.to_human(),
            message: self.message.clone(),
        }
    }
}

impl ToHuman for synctv_management::proto::ServerStateRedis {
    type Human = HumanServerStateRedis;

    fn to_human(&self) -> Self::Human {
        HumanServerStateRedis {
            status: humanize_server_state_redis_status(self.status),
            configured: self.configured,
            deployment_mode: self.deployment_mode.clone(),
            database: self.database,
            key_prefix: self.key_prefix.clone(),
            connect_timeout: humanize_seconds(self.connect_timeout_seconds),
            response_timeout: humanize_seconds(self.response_timeout_seconds),
            pipeline_buffer_size: self.pipeline_buffer_size,
            sentinel_master_name: self.sentinel_master_name.clone(),
            sentinel_node_count: self.sentinel_node_count,
            ping_latency: humanize_optional_millis(self.ping_latency_ms),
            message: self.message.clone(),
        }
    }
}

impl ToHuman for synctv_management::proto::ServerStateClusterNode {
    type Human = HumanServerStateClusterNode;

    fn to_human(&self) -> Self::Human {
        HumanServerStateClusterNode {
            node_id: self.node_id.clone(),
            api_address: self.api_address.clone(),
            last_heartbeat: humanize_timestamp(self.last_heartbeat),
            epoch: self.epoch,
        }
    }
}

impl ToHuman for synctv_management::proto::ServerStateCluster {
    type Human = HumanServerStateCluster;

    fn to_human(&self) -> Self::Human {
        HumanServerStateCluster {
            status: humanize_server_state_cluster_status(self.status),
            enabled: self.enabled,
            discovery_mode: self.discovery_mode.clone(),
            distributed_realtime_enabled: self.distributed_realtime_enabled,
            node_id_empty: self.node_id_empty,
            routable_node_count: self.routable_node_count,
            nodes: self.nodes.to_human(),
            message: self.message.clone(),
        }
    }
}

impl ToHuman for synctv_management::proto::ServerStateWsTicket {
    type Human = HumanServerStateWsTicket;

    fn to_human(&self) -> Self::Human {
        HumanServerStateWsTicket {
            status: humanize_server_state_ws_ticket_status(self.status),
            cross_node_capable: self.cross_node_capable,
            message: self.message.clone(),
        }
    }
}

impl ToHuman for synctv_management::proto::ServerStateEmail {
    type Human = HumanServerStateEmail;

    fn to_human(&self) -> Self::Human {
        HumanServerStateEmail {
            status: humanize_server_state_email_status(self.status),
            configured: self.configured,
        }
    }
}

impl ToHuman for synctv_management::proto::ServerStateLivestream {
    type Human = HumanServerStateLivestream;

    fn to_human(&self) -> Self::Human {
        HumanServerStateLivestream {
            status: humanize_server_state_livestream_status(self.status),
            configured: self.configured,
            active_publisher_count: self.active_publisher_count,
            active_room_count: self.active_room_count,
            rtmp_port: self.rtmp_port,
            public_rtmp_host: self.public_rtmp_host.clone(),
            gop_cache_size: self.gop_cache_size,
            gop_cache_max_memory: humanize_mebibytes(self.gop_cache_max_memory_mb),
            stream_timeout: humanize_seconds(self.stream_timeout_seconds),
            hls_storage_backend: self.hls_storage_backend.clone(),
            hls_storage_path: self.hls_storage_path.clone(),
            hls_memory_max: humanize_mebibytes(self.hls_memory_max_mb),
        }
    }
}

impl ToHuman for synctv_management::proto::ServerStateMemory {
    type Human = HumanServerStateMemory;

    fn to_human(&self) -> Self::Human {
        HumanServerStateMemory {
            status: humanize_server_state_memory_status(self.status),
            used: humanize_optional_bytes(self.used_bytes),
            total: humanize_optional_bytes(self.total_bytes),
            available: humanize_optional_bytes(self.available_bytes),
            usage: humanize_optional_percent(self.usage_percent),
        }
    }
}

impl ToHuman for synctv_management::proto::ServerStateRealtime {
    type Human = HumanServerStateRealtime;

    fn to_human(&self) -> Self::Human {
        HumanServerStateRealtime {
            distributed_enabled: self.distributed_enabled,
            connection_count: self.connection_count,
        }
    }
}

impl ToHuman for synctv_management::proto::ServerStateWebRtc {
    type Human = HumanServerStateWebRtc;

    fn to_human(&self) -> Self::Human {
        HumanServerStateWebRtc {
            status: humanize_server_state_webrtc_status(self.status),
            mode: self.mode.clone(),
            builtin_stun_configured: self.builtin_stun_configured,
            builtin_stun_state: self.builtin_stun_state.clone(),
            reason: self.reason.clone(),
            local_addr: self.local_addr.clone(),
            external_addr: self.external_addr.clone(),
            message: self.message.clone(),
        }
    }
}

impl ToHuman for synctv_management::proto::ServerStateSliceCache {
    type Human = HumanServerStateSliceCache;

    fn to_human(&self) -> Self::Human {
        HumanServerStateSliceCache {
            status: humanize_server_state_slice_cache_status(self.status),
            engine_enabled: self.engine_enabled,
            backend: self.backend.clone(),
            file_cache_dir: self.file_cache_dir.clone(),
            slice_size: humanize_bytes(self.slice_size),
            max_cache_size: humanize_bytes(self.max_cache_size),
            segment_ttl: humanize_seconds(self.segment_ttl_secs),
            stale_max_age: humanize_seconds(self.stale_max_age_secs),
            stale_while_revalidate: self.stale_while_revalidate,
            eviction_interval: humanize_seconds(self.eviction_interval_secs),
            watermark: humanize_ratio_percent(self.watermark_ratio),
            current_size: humanize_bytes(self.current_size_bytes),
            entry_count: self.entry_count,
            metadata_entries: self.metadata_entries,
            updating_entries: self.updating_entries,
            lock_count: self.lock_count,
            usage: humanize_ratio_percent(self.usage_ratio),
        }
    }
}

impl ToHuman for synctv_management::proto::ServerStateCpu {
    type Human = HumanServerStateCpu;

    fn to_human(&self) -> Self::Human {
        HumanServerStateCpu {
            status: humanize_server_state_cpu_status(self.status),
            available_parallelism: self.available_parallelism,
            current_load_1m: humanize_optional_load(self.current_load_1m),
            load_ratio_1m: humanize_optional_ratio(self.load_ratio_1m),
            load_average_1m: humanize_optional_load(self.load_average_1m),
            load_average_5m: humanize_optional_load(self.load_average_5m),
            load_average_15m: humanize_optional_load(self.load_average_15m),
        }
    }
}

impl ToHuman for synctv_management::proto::ServerStateNode {
    type Human = HumanServerStateNode;

    fn to_human(&self) -> Self::Human {
        HumanServerStateNode {
            node_id: self.node_id.clone(),
            status: humanize_server_state_node_status(self.status),
            updated_at: humanize_timestamp(self.updated_at),
            version: self.version.clone(),
            api_address: self.api_address.clone(),
            realtime: self.realtime.to_human(),
            database: self.database.to_human(),
            redis: self.redis.to_human(),
            cluster: self.cluster.to_human(),
            ws_ticket: self.ws_ticket.to_human(),
            email: self.email.to_human(),
            livestream: self.livestream.to_human(),
            memory: self.memory.to_human(),
            webrtc: self.webrtc.to_human(),
            cpu: self.cpu.to_human(),
            slice_cache: self.slice_cache.to_human(),
        }
    }
}

impl ToHuman for synctv_management::proto::ServerStateNodeFailure {
    type Human = HumanServerStateNodeFailure;

    fn to_human(&self) -> Self::Human {
        HumanServerStateNodeFailure {
            node_id: self.node_id.clone(),
            error: self.error.clone(),
        }
    }
}

impl ToHuman for synctv_management::proto::GetServerStateResponse {
    type Human = HumanGetServerStateResponse;

    fn to_human(&self) -> Self::Human {
        HumanGetServerStateResponse {
            scope: self.scope.clone(),
            summary: self.summary.to_human(),
            nodes: self.nodes.to_human(),
            failures: self.failures.to_human(),
        }
    }
}

impl_identity_to_human!(
    synctv_proto::admin::DeleteUserResponse,
    synctv_proto::admin::SetUserPasswordResponse,
    synctv_proto::admin::GetRoomSettingsResponse,
    synctv_proto::admin::UpdateRoomPasswordResponse,
    synctv_proto::admin::DeleteRoomResponse,
    synctv_proto::admin::RemoveAdminResponse,
    synctv_proto::admin::GetServiceStateResponse,
    synctv_proto::admin::ListActiveStreamsResponse,
    synctv_proto::admin::KickStreamResponse,
    synctv_proto::client::KickRoomStreamResponse,
    synctv_proto::admin::BatchBanUsersResponse,
    synctv_proto::admin::BatchDeleteUsersResponse,
    synctv_proto::admin::BatchBanRoomsResponse,
    synctv_proto::admin::BatchDeleteRoomsResponse,
    synctv_proto::providers::common::DeleteProviderInstanceResponse,
    synctv_proto::admin::SendTestEmailResponse,
    synctv_proto::client::LeaveRoomResponse,
    synctv_proto::client::DeleteRoomResponse,
    synctv_proto::client::GetRoomSettingsResponse,
    synctv_proto::client::RoomSettings,
    synctv_proto::client::SetRoomPasswordResponse,
    synctv_proto::client::KickMemberResponse,
    synctv_proto::client::DeletePlaylistResponse,
    synctv_proto::client::DeleteMediaResponse,
    synctv_proto::client::DeleteEntriesResponse,
    synctv_proto::client::ClearPlaylistResponse,
    synctv_proto::providers::alist::LoginResponse,
    synctv_proto::providers::alist::ListResponse,
    synctv_proto::providers::alist::SearchResponse,
    synctv_proto::providers::alist::GetMeResponse,
    synctv_proto::providers::alist::LogoutResponse,
    synctv_proto::providers::alist::GetBindsResponse,
    synctv_proto::providers::emby::LoginResponse,
    synctv_proto::providers::emby::ListResponse,
    synctv_proto::providers::emby::GetMeResponse,
    synctv_proto::providers::emby::LogoutResponse,
    synctv_proto::providers::emby::GetBindsResponse,
    synctv_proto::providers::douyin::BindResponse,
    synctv_proto::providers::douyin::GetBindsResponse,
    synctv_proto::providers::douyin::UnbindResponse,
    synctv_proto::providers::douyin::ResolveResponse,
    synctv_proto::providers::douyin::ListUserPostsResponse,
    synctv_proto::providers::tiktok::BindResponse,
    synctv_proto::providers::tiktok::GetBindsResponse,
    synctv_proto::providers::tiktok::UnbindResponse,
    synctv_proto::providers::tiktok::ResolveResponse,
    synctv_proto::providers::tiktok::GetUserResponse,
    synctv_proto::providers::tiktok::ListUserPostsResponse,
    synctv_proto::providers::twitch::BindResponse,
    synctv_proto::providers::twitch::GetBindsResponse,
    synctv_proto::providers::twitch::UnbindResponse,
    synctv_proto::providers::twitch::ResolveResponse,
    synctv_proto::providers::twitch::ListChannelItemsResponse,
    synctv_proto::providers::bilibili::ParseResponse,
    synctv_proto::providers::bilibili::QrCodeResponse,
    synctv_proto::providers::bilibili::QrStatusResponse,
    synctv_proto::providers::bilibili::StartSmsLoginResponse,
    synctv_proto::providers::bilibili::SendSmsResponse,
    synctv_proto::providers::bilibili::LoginSmsResponse,
    synctv_proto::providers::bilibili::UserInfoResponse,
    synctv_proto::providers::bilibili::LogoutResponse,
    synctv_proto::providers::bilibili::GetBindsResponse
);

impl_identity_to_human!(
    PlaybackStartCliOutput,
    PlaybackStopCliOutput,
    KickStreamCliOutput
);

#[cfg(test)]
pub(super) fn render_human_output<T>(value: &T) -> Result<Value>
where
    T: ?Sized + ToHuman,
{
    Ok(serde_json::to_value(value.to_human())?)
}

fn i64_to_i32(raw: i64) -> Option<i32> {
    i32::try_from(raw).ok()
}

fn humanize_server_state_node_status(raw: i32) -> String {
    use synctv_management::proto::ServerStateNodeStatus;

    match ServerStateNodeStatus::try_from(raw).unwrap_or(ServerStateNodeStatus::Unhealthy) {
        ServerStateNodeStatus::Unspecified => "unspecified",
        ServerStateNodeStatus::Healthy => "healthy",
        ServerStateNodeStatus::Degraded => "degraded",
        ServerStateNodeStatus::Unhealthy => "unhealthy",
    }
    .to_string()
}

fn humanize_server_state_database_status(raw: i32) -> String {
    use synctv_management::proto::ServerStateDatabaseStatus;

    match ServerStateDatabaseStatus::try_from(raw).unwrap_or(ServerStateDatabaseStatus::Unhealthy) {
        ServerStateDatabaseStatus::Unspecified => "unspecified",
        ServerStateDatabaseStatus::Healthy => "healthy",
        ServerStateDatabaseStatus::Unhealthy => "unhealthy",
    }
    .to_string()
}

fn humanize_server_state_redis_status(raw: i32) -> String {
    use synctv_management::proto::ServerStateRedisStatus;

    match ServerStateRedisStatus::try_from(raw).unwrap_or(ServerStateRedisStatus::Unhealthy) {
        ServerStateRedisStatus::Unspecified => "unspecified",
        ServerStateRedisStatus::Healthy => "healthy",
        ServerStateRedisStatus::NotConfigured => "not_configured",
        ServerStateRedisStatus::Unhealthy => "unhealthy",
    }
    .to_string()
}

fn humanize_server_state_cluster_status(raw: i32) -> String {
    use synctv_management::proto::ServerStateClusterStatus;

    match ServerStateClusterStatus::try_from(raw).unwrap_or(ServerStateClusterStatus::Unhealthy) {
        ServerStateClusterStatus::Unspecified => "unspecified",
        ServerStateClusterStatus::Healthy => "healthy",
        ServerStateClusterStatus::Unhealthy => "unhealthy",
        ServerStateClusterStatus::Disabled => "disabled",
    }
    .to_string()
}

fn humanize_server_state_ws_ticket_status(raw: i32) -> String {
    use synctv_management::proto::ServerStateWsTicketStatus;

    match ServerStateWsTicketStatus::try_from(raw).unwrap_or(ServerStateWsTicketStatus::Unhealthy) {
        ServerStateWsTicketStatus::Unspecified => "unspecified",
        ServerStateWsTicketStatus::Healthy => "healthy",
        ServerStateWsTicketStatus::Unhealthy => "unhealthy",
    }
    .to_string()
}

fn humanize_server_state_email_status(raw: i32) -> String {
    use synctv_management::proto::ServerStateEmailStatus;

    match ServerStateEmailStatus::try_from(raw).unwrap_or(ServerStateEmailStatus::NotConfigured) {
        ServerStateEmailStatus::Unspecified => "unspecified",
        ServerStateEmailStatus::Configured => "configured",
        ServerStateEmailStatus::NotConfigured => "not_configured",
    }
    .to_string()
}

fn humanize_server_state_livestream_status(raw: i32) -> String {
    use synctv_management::proto::ServerStateLivestreamStatus;

    match ServerStateLivestreamStatus::try_from(raw)
        .unwrap_or(ServerStateLivestreamStatus::NotConfigured)
    {
        ServerStateLivestreamStatus::Unspecified => "unspecified",
        ServerStateLivestreamStatus::Configured => "configured",
        ServerStateLivestreamStatus::NotConfigured => "not_configured",
    }
    .to_string()
}

fn humanize_server_state_memory_status(raw: i32) -> String {
    use synctv_management::proto::ServerStateMemoryStatus;

    match ServerStateMemoryStatus::try_from(raw).unwrap_or(ServerStateMemoryStatus::Unknown) {
        ServerStateMemoryStatus::Unspecified => "unspecified",
        ServerStateMemoryStatus::Healthy => "healthy",
        ServerStateMemoryStatus::Unhealthy => "unhealthy",
        ServerStateMemoryStatus::Unknown => "unknown",
    }
    .to_string()
}

fn humanize_server_state_webrtc_status(raw: i32) -> String {
    use synctv_management::proto::ServerStateWebRtcStatus;

    match ServerStateWebRtcStatus::try_from(raw).unwrap_or(ServerStateWebRtcStatus::Degraded) {
        ServerStateWebRtcStatus::Unspecified => "unspecified",
        ServerStateWebRtcStatus::Healthy => "healthy",
        ServerStateWebRtcStatus::Degraded => "degraded",
        ServerStateWebRtcStatus::Disabled => "disabled",
    }
    .to_string()
}

fn humanize_server_state_cpu_status(raw: i32) -> String {
    use synctv_management::proto::ServerStateCpuStatus;

    match ServerStateCpuStatus::try_from(raw).unwrap_or(ServerStateCpuStatus::Unknown) {
        ServerStateCpuStatus::Unspecified => "unspecified",
        ServerStateCpuStatus::Healthy => "healthy",
        ServerStateCpuStatus::Degraded => "degraded",
        ServerStateCpuStatus::Unhealthy => "unhealthy",
        ServerStateCpuStatus::Unknown => "unknown",
    }
    .to_string()
}

fn humanize_server_state_slice_cache_status(raw: i32) -> String {
    use synctv_management::proto::ServerStateSliceCacheStatus;

    match ServerStateSliceCacheStatus::try_from(raw)
        .unwrap_or(ServerStateSliceCacheStatus::Disabled)
    {
        ServerStateSliceCacheStatus::Unspecified => "unspecified",
        ServerStateSliceCacheStatus::Healthy => "healthy",
        ServerStateSliceCacheStatus::Disabled => "disabled",
    }
    .to_string()
}

fn humanize_user_role(raw: i64) -> Option<String> {
    use synctv_proto::common::UserRole;

    Some(
        match UserRole::try_from(i64_to_i32(raw)?).ok()? {
            UserRole::Unspecified => "unspecified",
            UserRole::User => "user",
            UserRole::Admin => "admin",
            UserRole::Root => "root",
        }
        .to_string(),
    )
}

fn humanize_user_status(raw: i64) -> Option<String> {
    use synctv_proto::common::UserStatus;

    Some(
        match UserStatus::try_from(i64_to_i32(raw)?).ok()? {
            UserStatus::Unspecified => "unspecified",
            UserStatus::Active => "active",
            UserStatus::Banned => "banned",
        }
        .to_string(),
    )
}

fn humanize_room_status(raw: i64) -> Option<String> {
    use synctv_proto::common::RoomStatus;

    Some(
        match RoomStatus::try_from(i64_to_i32(raw)?).ok()? {
            RoomStatus::Unspecified => "unspecified",
            RoomStatus::Active => "active",
            RoomStatus::Closed => "closed",
        }
        .to_string(),
    )
}

fn humanize_chat_message_status(raw: i64) -> Option<String> {
    use synctv_proto::client::ChatMessageStatus;

    Some(
        match ChatMessageStatus::try_from(i64_to_i32(raw)?).ok()? {
            ChatMessageStatus::Unspecified => "unspecified",
            ChatMessageStatus::Active => "active",
            ChatMessageStatus::Edited => "edited",
            ChatMessageStatus::Deleted => "deleted",
        }
        .to_string(),
    )
}

fn humanize_resource_availability(raw: i64) -> Option<String> {
    use synctv_proto::client::ResourceAvailability;

    Some(
        match ResourceAvailability::try_from(i64_to_i32(raw)?).ok()? {
            ResourceAvailability::Unspecified => "unspecified",
            ResourceAvailability::Available => "available",
            ResourceAvailability::CreatorInactive => "creatorInactive",
        }
        .to_string(),
    )
}

fn humanize_source_provider(raw: i32) -> String {
    match synctv_proto::source_config::SourceProvider::try_from(raw) {
        Ok(synctv_proto::source_config::SourceProvider::Unspecified) => String::new(),
        Ok(synctv_proto::source_config::SourceProvider::DirectUrl) => "directUrl".to_string(),
        Ok(synctv_proto::source_config::SourceProvider::Bilibili) => "bilibili".to_string(),
        Ok(synctv_proto::source_config::SourceProvider::Alist) => "alist".to_string(),
        Ok(synctv_proto::source_config::SourceProvider::Emby) => "emby".to_string(),
        Ok(synctv_proto::source_config::SourceProvider::Rtmp) => "rtmp".to_string(),
        Ok(synctv_proto::source_config::SourceProvider::LiveProxy) => "liveProxy".to_string(),
        Ok(synctv_proto::source_config::SourceProvider::Cloudreve) => "cloudreve".to_string(),
        Ok(synctv_proto::source_config::SourceProvider::Twitch) => "twitch".to_string(),
        Ok(synctv_proto::source_config::SourceProvider::Huya) => "huya".to_string(),
        Ok(synctv_proto::source_config::SourceProvider::Douyu) => "douyu".to_string(),
        Ok(synctv_proto::source_config::SourceProvider::Douyin) => "douyin".to_string(),
        Ok(synctv_proto::source_config::SourceProvider::Tiktok) => "tiktok".to_string(),
        Ok(synctv_proto::source_config::SourceProvider::Acfun) => "acfun".to_string(),
        Ok(synctv_proto::source_config::SourceProvider::Cctv) => "cctv".to_string(),
        Ok(synctv_proto::source_config::SourceProvider::Fnos) => "fnos".to_string(),
        Ok(synctv_proto::source_config::SourceProvider::Qnap) => "qnap".to_string(),
        Ok(synctv_proto::source_config::SourceProvider::Synology) => "synology".to_string(),
        Ok(synctv_proto::source_config::SourceProvider::Nextcloud) => "nextcloud".to_string(),
        Ok(synctv_proto::source_config::SourceProvider::Seafile) => "seafile".to_string(),
        Ok(synctv_proto::source_config::SourceProvider::Truenas) => "truenas".to_string(),
        Ok(synctv_proto::source_config::SourceProvider::Youtube) => "youtube".to_string(),
        Err(_) => raw.to_string(),
    }
}

fn humanize_source_providers(values: &[i32]) -> Vec<String> {
    values
        .iter()
        .copied()
        .map(humanize_source_provider)
        .collect()
}

fn humanize_room_member_role(raw: i64) -> Option<String> {
    use synctv_proto::common::RoomMemberRole;

    Some(
        match RoomMemberRole::try_from(i64_to_i32(raw)?).ok()? {
            RoomMemberRole::Unspecified => "unspecified",
            RoomMemberRole::Guest => "guest",
            RoomMemberRole::Member => "member",
            RoomMemberRole::Admin => "admin",
            RoomMemberRole::Creator => "creator",
        }
        .to_string(),
    )
}

fn humanize_permission_bits(bits: u64) -> Vec<String> {
    humanize_named_permission_bits(bits, CLI_NAMED_PERMISSIONS)
}

fn humanize_named_permission_bits(bits: u64, named_permissions: &[(&str, u64)]) -> Vec<String> {
    named_permissions
        .iter()
        .copied()
        .map(|(name, permission)| (permission, name))
        .filter(|&(permission, _)| bits & permission != 0)
        .map(|(_, name)| name.to_string())
        .collect()
}

fn humanize_provider_instance_status(raw: i64) -> Option<String> {
    use synctv_proto::providers::common::ProviderInstanceStatus;

    Some(
        match ProviderInstanceStatus::try_from(i64_to_i32(raw)?).ok()? {
            ProviderInstanceStatus::Unspecified => "unspecified",
            ProviderInstanceStatus::Connected => "connected",
            ProviderInstanceStatus::Disconnected => "disconnected",
            ProviderInstanceStatus::Error => "error",
        }
        .to_string(),
    )
}
