# synctv-sfu

**Status: Experimental / Partially Functional**

This crate implements a WebRTC Selective Forwarding Unit (SFU) for synctv.
The core infrastructure is implemented but has **known limitations in production environments**,
particularly in multi-node cluster deployments. See **Cluster Limitations** below.

## What works

The following features are implemented and tested:

### Forwarding Plane
- **RTP packet forwarding** -- `MediaTrack` reads RTP packets from a `TrackRemote`,
  wraps them in `ForwardablePacket`, and fans them out to subscriber channels.
- **Simulcast quality layers** -- `QualityLayer` (High/Medium/Low) with
  bandwidth-based selection and per-subscriber filtering (basic implementation).
- **Bandwidth estimation** -- Exponential-smoothing estimator in `SfuPeer` that
  drives automatic quality adaptation.
- **Room management** -- `SfuRoom` handles peer add/remove, track
  publish/subscribe, and automatic P2P-to-SFU mode switching with hysteresis.
- **Multi-room orchestration** -- `SfuManager` manages concurrent rooms with
  atomic capacity enforcement, background cleanup, and statistics aggregation.
- **Network quality monitoring** -- `NetworkQualityMonitor` computes per-peer
  quality scores (0--5) and suggests adaptive actions (reduce quality, reduce
  framerate, audio-only).
- **Packet pacing** -- `PacketPacer` implements token bucket rate limiting to prevent
  bursty traffic and network congestion.
- **RTCP feedback** -- `RtcpHandler` polls WebRTC statistics to extract RTT, packet loss,
  and bandwidth metrics for adaptive quality control.

### Control Plane
- **Peer connection management** -- `PeerConnection` wraps `RTCPeerConnection` instances,
  configures ICE servers, and manages connection state transitions.
- **SDP signaling** -- `SfuSessionManager` handles offer/answer exchange and integrates
  with the WebSocket messaging layer in `synctv-api`.
- **ICE candidate handling** -- Trickle ICE support with automatic candidate relay
  through the signaling channel.
- **Subscriber output path** -- `PeerConnection::start_subscriber_output()` consumes
  packets from `SfuPeer::take_packet_receiver()` and writes to `TrackLocal` instances.
- **Session lifecycle** -- `SfuSessionManager` creates and destroys server-side
  `PeerConnection` instances based on room size thresholds.

## Architecture: Single-Node SFU

SFU mode operates as a **single-node service**. All WebRTC PeerConnections for a
room must reside on the same process. Cross-node media forwarding is not implemented.

### Why single-node only

WebRTC PeerConnections maintain stateful ICE/DTLS sessions tied to a specific
process. Forwarding RTP packets between nodes would require:
- RTP packet tunneling (gRPC streaming or Redis Pub/Sub) with sub-100ms latency
- Cross-node RTCP feedback aggregation for bandwidth estimation
- Distributed room state synchronization

These are non-trivial to implement correctly without degrading media quality.

### Enforcement

Startup configuration validation (`Config::validate()`) **blocks SFU and Hybrid
modes** when a cluster secret is configured. This prevents silent failures in
multi-node deployments. The relevant check is in `synctv-core/src/config.rs`.

### Session affinity (prepared, not required for single-node)

Infrastructure for session-affine routing exists for potential future use:
- `SessionAffinityRegistry` trait in `session_manager.rs`
- `RedisSessionAffinityRegistry` implementation in `redis_affinity.rs`
- `lookup_session_replica()` for routing queries

In single-node mode, the `NoopSessionRegistry` is used (zero overhead).

### Deployment options

| Mode | Nodes | Description |
|------|-------|-------------|
| `peer_to_peer` | 1+ | Direct P2P media, signaling relay only |
| `signaling_only` | 1+ | WebSocket signaling relay, no SFU/TURN |
| `hybrid` | **1 only** | P2P for small rooms, SFU for large rooms |
| `sfu` | **1 only** | All rooms use SFU forwarding |

## Known Issues and Limitations

### P1: Incomplete Features
- **Simulcast layer switching** -- Basic implementation exists but lacks dynamic layer
  switching based on bandwidth estimation. All subscribers currently receive the medium layer.

### P2: Production Hardening
- **STUN server configuration** -- Built-in STUN server (`enable_builtin_stun`) requires
  `stun_external_addr` to be set in NAT/K8s environments. Falls back to `advertise_host`
  but may not work correctly behind load balancers.
- **No integration tests** -- Unit tests pass but end-to-end SFU-to-client integration
  is not tested. Manual testing required before production use.

## Configuration Recommendations

```yaml
# Production cluster (multi-node) - REQUIRED
webrtc:
  mode: peer_to_peer  # or signaling_only
  enable_builtin_stun: true
  stun_external_addr: ""  # Auto-detected from advertise_host

# Single-node deployment (for testing SFU)
webrtc:
  mode: hybrid
  sfu_threshold: 5
  enable_builtin_stun: true
  stun_external_addr: "1.2.3.4:3478"  # Set to node's public IP
```
