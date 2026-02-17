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

## Known Issues and Limitations

### P0: Cluster Limitations
**SFU mode does not work correctly in multi-replica cluster deployments.**

In a clustered environment:
- WebSocket connections may be distributed across different nodes
- SFU rooms are created on individual nodes, not replicated via Redis
- P2P peer migrations to SFU fail when peers are on different nodes
- Media streams cannot be forwarded across cluster nodes

**Workarounds:**
- Use `webrtc.mode: peer_to_peer` or `webrtc.mode: signaling_only` in production clusters
- Deploy a single replica for SFU/Hybrid modes (not recommended for high availability)
- Use sticky sessions to route all room members to the same node (partial solution)

**Future Work:**
- Implement cross-node media forwarding via Redis Pub/Sub or gRPC streaming
- Add cluster-aware peer migration (migrate entire rooms, not individual peers)
- Document deployment topologies (single-node SFU vs. multi-node P2P)

### P1: Incomplete Features
- **Simulcast layer switching** -- Basic implementation exists but lacks dynamic layer
  switching based on bandwidth estimation. All subscribers currently receive the high layer.
- **ICE candidate filtering** -- No SRFLX/RELAY preference logic implemented yet.
- **Track ID conflict detection** -- No duplicate track ID validation when peers publish.
- **Session timeout mechanism** -- SFU sessions do not expire automatically on idle or
  connection failure. Manual cleanup required.

### P2: Production Hardening
- **STUN server configuration** -- Built-in STUN server (`enable_builtin_stun`) requires
  `stun_external_addr` to be set in NAT/K8s environments. Falls back to `advertise_host`
  but may not work correctly behind load balancers.
- **No integration tests** -- Unit tests pass but end-to-end SFU→client integration
  is not tested. Manual testing required before production use.

## Usage Status

**Single-node deployments:** Basic SFU functionality works for testing.
**Multi-node clusters:** DO NOT USE `sfu` or `hybrid` modes. Use `peer_to_peer` or `signaling_only`.

To prevent misconfiguration, startup validation blocks `sfu`/`hybrid` modes when:
- Cluster secret is configured (indicates multi-node deployment)
- This prevents silent failures in production clusters

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
