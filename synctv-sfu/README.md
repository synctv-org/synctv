# synctv-sfu

**Status: Experimental / Infrastructure Only**

This crate implements the forwarding plane of a WebRTC Selective Forwarding Unit (SFU)
for synctv. It is **not production-ready** and cannot be used end-to-end in its current
state. The crate exists as foundational infrastructure that can be completed when WebRTC
support for large rooms becomes a priority.

## What works

The internal forwarding and quality-management layers are implemented and tested:

- **RTP packet forwarding** -- `MediaTrack` reads RTP packets from a `TrackRemote`,
  wraps them in `ForwardablePacket`, and fans them out to subscriber channels.
- **Simulcast quality layers** -- `QualityLayer` (High/Medium/Low) with
  bandwidth-based selection and per-subscriber filtering.
- **Bandwidth estimation** -- Exponential-smoothing estimator in `SfuPeer` that
  drives automatic quality adaptation.
- **Room management** -- `SfuRoom` handles peer add/remove, track
  publish/subscribe, and automatic P2P-to-SFU mode switching with hysteresis.
- **Multi-room orchestration** -- `SfuManager` manages concurrent rooms with
  atomic capacity enforcement, background cleanup, and statistics aggregation.
- **Network quality monitoring** -- `NetworkQualityMonitor` computes per-peer
  quality scores (0--5) and suggests adaptive actions (reduce quality, reduce
  framerate, audio-only).

## What is missing

The control plane and output path are not implemented, which means the crate
cannot handle a real WebRTC session:

- **Peer connection management** -- There is no code that creates
  `RTCPeerConnection` instances, configures ICE servers, or manages connection
  state transitions. `SfuPeer` models a logical peer but does not own a
  connection.
- **SDP signaling** -- No offer/answer exchange. The crate has no HTTP or
  WebSocket signaling endpoint and no SDP manipulation logic.
- **ICE candidate handling** -- No trickle ICE support, no candidate gathering
  or relay configuration.
- **Subscriber output path** -- `SfuPeer::take_packet_receiver()` returns an
  `mpsc::Receiver<ForwardablePacket>`, but nothing consumes it to write packets
  to an outbound WebRTC track. The last hop from the SFU to the subscriber
  browser is unimplemented.
- **Integration with synctv-api** -- The API layer depends on this crate but
  does not expose any SFU-related endpoints or signaling handlers.

## Usage status

The crate compiles and its unit tests pass (`cargo test -p synctv-sfu`), but it
cannot serve real WebRTC traffic. It is included in the workspace for forward
compatibility and to preserve the existing design work.

## Future work

To make the SFU operational, the following would need to be implemented:

1. A signaling endpoint (WebSocket or HTTP) for SDP offer/answer and ICE
   candidates.
2. `RTCPeerConnection` lifecycle management per peer (create, configure, close).
3. An output task per subscriber that reads from
   `SfuPeer::take_packet_receiver()` and writes to `TrackLocal` instances on the
   subscriber's peer connection.
4. Integration hooks in synctv-api to route signaling traffic and manage SFU
   room lifecycle alongside existing room logic.
