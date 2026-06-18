# Playback Background Workers

Playback background workers use the current process's active room set as their scheduling boundary.

An active room for these workers means a room with at least one realtime connection registered in the local `ConnectionRuntime`. It is intentionally local-node state. In cluster deployments, the same room may be active on more than one node, and each node may attempt the same background work for that room.

This is a deliberate ownership boundary:

- a room with local realtime connections already has lifecycle state on the current node;
- background playback work only needs rooms this node can observe locally;
- global hot-room stats are presence/analytics data for lists, admin views, and metrics.

These workers run on every node so rooms connected to any replica receive duration probing, auto-advance, and playback resource lifecycle work.

Correctness for duplicate attempts belongs at the storage and state-transition layer:

- duration probing claims rows with database locks and `SKIP LOCKED`, so one worker owns a probe attempt;
- playback auto-advance uses the playback state transaction and optimistic version checks, so concurrent attempts converge to one committed state transition;
- workers use local realtime ownership, which keeps lifecycle work tied to rooms the node can observe.

Use shared presence or global hot-room queries for user-facing room lists and cross-node statistics. Use `ConnectionRuntime::active_room_ids()` for per-node playback lifecycle work. Use leader election for true singleton tasks such as partition management and cleanup.

This rule also applies when optimizing for fewer background scans. An active room already exists on one or more processes because clients are connected there. Each process scans its own active rooms, and the write paths provide the lock or version guard. Keeping analytics data out of lifecycle scheduling keeps live resource ownership easy to reason about.

## SQLx Query Cache

Repository queries for playback background work use SQLx checked macros. When any of these queries change, update `.sqlx` with `cargo sqlx prepare --workspace -- --all-targets`, then verify with `SQLX_OFFLINE=true cargo check --workspace --all-targets`.

Keep checked SQLx macros in repository code and treat `.sqlx` updates as part of the SQL change.

The room-scoped queries must keep the `room_id = ANY(...)` filter and the join to the current `room_playback_progress.target_hash`. The room filter preserves the local active-room scheduler boundary. The target hash binds media and dynamic playlist playback to the currently selected item.

Auto-advance must move state through the playback-state transaction and version write path. When several nodes scan the same active room, exactly one state transition should commit.

Finite sequential playlists persist a stable ended or paused state when there is no next item. The persisted end state lets later scan intervals skip the already-finished source.

## End-to-End Coverage

Playback background changes require manual end-to-end checks in addition to unit tests:

- start a built `synctv` binary so startup, config, migrations, and routes are covered;
- keep a room active through a real WebSocket connection;
- use `synctv` CLI for setup/control and `curl` for HTTP/media assertions;
- verify duration probing initializes metadata only for local active rooms;
- verify auto-advance advances once when multiple workers can see the same room;
- verify provider playback URLs by actually requesting direct/proxy/manifest/segment URLs;
- cover dynamic playlist media resolution, path return, switching, cover/thumbnail paths, and auto-advance target changes.

Provider E2E coverage should include Direct URL, Alist, Emby, Jellyfin, Bilibili anonymous playback, RTMP, live proxy, HLS, FLV, cache hit/miss, URL expiry, and cleanup behavior.

The scheduling source for these checks is `ConnectionRuntime::active_room_ids()`.
Presence hot-room queries are analytics inputs for lists and metrics. Playback
workers use local realtime ownership, and storage/state transitions provide
cross-node convergence.

Provider checks must request every mode and auxiliary URL returned in
`PlaybackResult`: upstream URLs, `proxy_*` URLs, manifests, indexed segments,
subtitles, danmaku, thumbnails, Range responses, cache hit/miss paths, expiry
handling, and live resource cleanup. Add `synctv` CLI coverage for workflows
that require manual verification, then exercise them through CLI plus `curl`.

Use cached playback results as part of the provider checklist. Cache hits and
fresh provider responses must expose the same usable mode names and resolver
actions, including MPD/HLS manifests and indexed segment routes.

RTMP and live proxy checks should prove the full lifecycle: create the publish
key or live proxy media through CLI, publish with a real upstream such as
`ffmpeg`, request stream info, fetch HLS playlists and segments, fetch FLV, stop
the viewer or publisher, and observe idle cleanup releasing the stream.

## Provider Playback Contract

Every Provider signs and finalizes playback inside `generate_playback`, then
serves every generated proxy action through its resolver.

`VersionedPlayback` is the cache payload plus proxy lookup index. The `version`
maps every generated `/stream`, `/m3u8`, `/mpd`, subtitle, danmaku, thumbnail,
FLV, and HLS segment URL back to the provider-owned playback result. Response
finalization applies the provider's signer/rewrite callback for the current
request. Policy still lives in that provider: mode names, default mode, header
exposure, direct/proxy shape, manifest metadata, and live lifecycle data.

| Provider | Coverage |
| --- | --- |
| Direct URL | upstream mode, `proxy_*` sibling, proxy default for header-bound sources, HLS segment rewriting, Range |
| Alist | direct/transcode modes, proxy siblings, thumbnail, subtitle, HLS segment, stream resolver |
| Emby/Jellyfin | upstream/transcode modes, proxy siblings, allowed token headers, stream/HLS/subtitle proxy |
| Bilibili | anonymous playback, DASH/MPD proxy default, proxied manifest segments, subtitle, danmaku, thumbnail, cache metadata |
| RTMP | publish key, stream info, HLS playlist/segment, FLV, provider-proxy URL, idle cleanup |
| live proxy | external RTMP/HTTP-FLV pull, HLS/FLV provider-proxy URL, publisher registration and unregister cleanup |
