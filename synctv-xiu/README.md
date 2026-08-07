# synctv-xiu

Consolidated streaming library for SyncTV, providing RTMP, RTSP, HLS, and HTTP-FLV protocol support with FLV/MPEG-TS container formats.

This crate is derived from [xiu](https://github.com/harlanc/xiu) by HarlanC, restructured from 9 separate crates into a single unified crate with the following modules:

- **bytesio** - Async byte I/O utilities built on tokio
- **h264** - H.264 (AVC) bitstream parser (SPS/PPS)
- **flv** - FLV container format (muxer, demuxer, AMF0)
- **mpegts** - MPEG-TS container format (PAT/PMT/PES)
- **streamhub** - Central event bus for stream distribution
- **storage** - Pluggable HLS segment storage (file, memory, S3-compatible object storage)
- **rtmp** - RTMP protocol (handshake, chunking, sessions)
- **rtsp** - RTSP client ingest, RTP over TCP/UDP, Basic/Digest authentication,
  multi-track selection, and H.264/H.265/AAC conversion to StreamHub frames
- **hls** - HLS protocol (RTMP-to-HLS remuxer, segment management, HTTP server)
- **httpflv** - HTTP-FLV streaming

## S3 storage feature flags

The S3 backend is enabled with `s3` and requires one Rustls crypto provider:
use `tls-aws-lc` or `tls-ring`. Certificate-root features such as
`tls-webpki-roots` and `tls-native-roots` remain application choices. The
provider feature is selected by the application so a workspace can choose its
required cryptographic backend.

## RTSP ingest

External `rtsp://` sources enter the same StreamHub pipeline as RTMP and
HTTP-FLV sources. Each SyncTV media item owns an independent pull session, so a
node can run multiple RTSP sources concurrently within its configured external
stream capacity. The default selects the first compatible video and audio
tracks and transports RTP over interleaved TCP. The public RTSP configuration
also supports exact SDP track indices, disabled audio/video tracks, and UDP.

Supported output mappings:

| RTSP input | StreamHub / HTTP-FLV | HLS |
| --- | --- | --- |
| H.264 | AVC FLV tags | MPEG-TS H.264 |
| H.265 | HEVC FLV codec ID 12 | MPEG-TS H.265 |
| AAC | AAC FLV tags | MPEG-TS AAC |

RTSP sources are published through the same live stream path as RTMP. HLS exposes a short sliding segment window,
and HTTP-FLV follows the live edge. The stream pipeline keeps live segments for
active playback and evicts them as the window advances.

## License

MIT - see the [original project](https://github.com/harlanc/xiu) for details.
