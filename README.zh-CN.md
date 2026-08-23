<!-- markdownlint-disable MD013 MD033 MD041 -->

<p align="center">
  <img src="./docs/public/logo.svg" alt="SyncTV" width="180">
</p>

# SyncTV

[English](./README.md) · [官方网站](https://syncs.tv)

![Rust](https://img.shields.io/badge/Rust-2021-b7410e?logo=rust&logoColor=white)
![Tokio](https://img.shields.io/badge/runtime-Tokio-2f80ed)
![Axum](https://img.shields.io/badge/HTTP-Axum-00a8a8)
![gRPC](https://img.shields.io/badge/API-gRPC-244c5a?logo=grpc&logoColor=white)
![PostgreSQL](https://img.shields.io/badge/database-PostgreSQL-4169e1?logo=postgresql&logoColor=white)
![Redis](https://img.shields.io/badge/cache%20%26%20coordination-Redis-dc382d?logo=redis&logoColor=white)
![Docker](https://img.shields.io/badge/deploy-Docker-2496ed?logo=docker&logoColor=white)
![Helm](https://img.shields.io/badge/deploy-Helm-0f1689?logo=helm&logoColor=white)
![Kubernetes](https://img.shields.io/badge/orchestration-Kubernetes-326ce5?logo=kubernetes&logoColor=white)
![OpenAPI](https://img.shields.io/badge/docs-OpenAPI-6ba539?logo=openapiinitiative&logoColor=white)
![Astro Starlight](https://img.shields.io/badge/docs-Astro%20Starlight-bc52ee?logo=astro&logoColor=white)
![License](https://img.shields.io/badge/license-MIT-green)

SyncTV 是使用 Rust 实现的实时同步观影平台，支持媒体 Provider 集成、直播、HTTP/gRPC API，以及面向 Kubernetes 的横向扩展部署。

<p align="center">
  <img src="./docs/public/screenshots/room-macos.png" alt="SyncTV 房间同步播放" width="860">
</p>

## 核心能力

- 房间内同步播放，实时同步播放状态。
- 媒体 Provider 支持 Bilibili、Twitch、YouTube、抖音、TikTok、虎牙、斗鱼、AcFun、CCTV、Alist、Cloudreve、Emby/Jellyfin、FNOS、QNAP、Synology、Nextcloud、Seafile、TrueNAS、直链和直播来源。
- 支持 RTMP 推/拉流、HLS 和 HTTP-FLV 直播。
- 提供 HTTP REST、公开 gRPC、WebSocket、management gRPC、metrics、RTMP 和 STUN 等运行时入口。
- PostgreSQL 持久化业务数据，可选 Redis 作为共享状态、缓存、限流和集群协调层。
- 提供 Docker Compose 和 Helm 部署模板。
- 内置 management CLI，可选 OpenAPI/Swagger UI。
- 使用 Astro Starlight 构建中英文文档站。

## 讨论与贡献者

加入 [SyncTV Telegram 讨论组](https://t.me/synctv)，与用户和贡献者交流部署、运维、媒体 Provider、客户端开发和产品路线。

![SyncTV 贡献者](https://contrib.nn.ci/api?repo=synctv-org/synctv&repo=synctv-org/synctv-app)

## 文档

完整文档见 [docs.syncs.tv](https://docs.syncs.tv)。

从 [SyncTV App Releases](https://github.com/synctv-org/synctv-app/releases/latest) 下载原生客户端，或通过支持的应用商店安装。

## License

MIT。见 [LICENSE](./LICENSE)。
