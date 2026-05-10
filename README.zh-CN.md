<p align="center">
  <img src="./docs/public/logo.svg" alt="SyncTV" width="180">
</p>

# SyncTV

[English](./README.md)

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

## 核心能力

- 房间内同步播放，实时同步播放状态。
- 媒体 Provider 支持 Bilibili、Alist、Emby 兼容服务（Emby/Jellyfin）和直链。
- 支持 RTMP 推/拉流、HLS 和 HTTP-FLV 直播。
- 提供 HTTP REST、公开 gRPC、WebSocket、management gRPC、metrics、RTMP 和 STUN 等运行时入口。
- PostgreSQL 持久化业务数据，可选 Redis 作为共享状态、缓存、限流和集群协调层。
- 提供 Docker Compose 和 Helm 部署模板。
- 内置 management CLI，可选 OpenAPI/Swagger UI。
- 使用 Astro Starlight 构建中英文文档站。

## 快速开始

开发环境需要完整源码仓库：

```bash
docker compose -f docker-compose.dev.yml up -d
```

生产 Compose 可以在仓库根目录执行，也可以在只包含 `docker-compose.yml`、`.env.postgres.example`、`.env.synctv.example` 和 `scripts/init-compose-env.sh` 的部署目录执行。它需要显式配置 secret：

```bash
./scripts/init-compose-env.sh

# 启动前编辑 .env.synctv 中的 SYNCTV_BOOTSTRAP_ROOT_PASSWORD。
docker compose config
docker compose up -d
```

校验配置：

```bash
cargo run -p synctv --bin synctv -- config validate
```

可选 migration 预检。服务启动阶段也会自动执行 embedded SQLx migrations：

```bash
cargo run -p synctv --bin synctv -- db migrate
```

本地启动：

```bash
cargo run -p synctv --bin synctv -- serve
```

## 文档

主文档站位于 [`docs/`](./docs)，包含完整配置参考、部署文档、运维手册、CLI 参考、开发文档和 OpenAPI 入口说明。

```bash
cd docs
npm install
npm run dev
```

构建静态文档站：

```bash
cd docs
npm run build
```

如果生成站点部署在子路径下，构建时设置 `SYNCTV_DOCS_BASE`。`SYNCTV_DOCS_SITE` 用于 canonical URL 和 sitemap 的公开域名。

```bash
cd docs
SYNCTV_DOCS_SITE=https://example.com SYNCTV_DOCS_BASE=/synctv npm run build
```

重要入口：

- [快速开始](./docs/src/content/docs/install/quick-start.mdx)
- [文档导览](./docs/src/content/docs/overview/documentation-map.mdx)
- [架构总览](./docs/src/content/docs/overview/architecture.mdx)
- [认证与安全模型](./docs/src/content/docs/admin/authentication-security.mdx)
- [管理员操作手册](./docs/src/content/docs/admin/index.mdx)
- [房间、权限与用户偏好](./docs/src/content/docs/use/rooms-permissions.mdx)
- [客户端集成指南](./docs/src/content/docs/develop/client-integration.mdx)
- [配置文件如何工作](./docs/src/content/docs/configuration/how-configuration-works.mdx)
- [完整配置示例](./docs/src/content/docs/configuration/full-example.mdx)
- [配置总索引](./docs/src/content/docs/reference/configuration-index.mdx)
- [环境变量](./docs/src/content/docs/reference/environment-variables.mdx)
- [Runtime settings 参考](./docs/src/content/docs/reference/runtime-settings.mdx)
- [Docker Compose 部署](./docs/src/content/docs/install/docker-compose.mdx)
- [Helm 部署](./docs/src/content/docs/install/helm.mdx)
- [生产部署清单](./docs/src/content/docs/install/production-checklist.mdx)
- [备份与恢复](./docs/src/content/docs/operations/backup-restore.mdx)
- [升级与迁移](./docs/src/content/docs/operations/upgrades.mdx)
- [数据、隐私与保留策略](./docs/src/content/docs/operations/data-retention.mdx)
- [观测与运行手册](./docs/src/content/docs/operations/observability.mdx)
- [排障入口](./docs/src/content/docs/operations/troubleshooting.mdx)
- [CLI 参考](./docs/src/content/docs/reference/cli.mdx)
- [OpenAPI 文档入口](./docs/src/content/docs/reference/openapi.mdx)
- [gRPC 调试](./docs/src/content/docs/reference/grpc.mdx)
- [开发指南](./docs/src/content/docs/develop/local-development.mdx)

仓库流程文档：

- [安全策略](./SECURITY.md)
- [贡献指南](./CONTRIBUTING.md)

## 工作区结构

- `synctv`：应用二进制和 CLI。
- `synctv-core`：核心业务逻辑、配置、服务和 repository。
- `synctv-api`：HTTP/gRPC API 层。
- `synctv-livestream`：RTMP/HLS/HTTP-FLV 直播能力。
- `synctv-cluster`：集群协调。
- `synctv-proxy`：媒体代理和 slice cache。
- `synctv-proto`：protobuf 定义。
- `synctv-media-providers`：Provider 集成支持。
- `synctv-management`：management 客户端/控制面支持。
- `synctv-common`：共享工具。
- `synctv-xiu`：整合后的直播组件。
- `helm/synctv`：Kubernetes Helm chart。
- `docs`：Astro Starlight 文档站。

## License

MIT。见 [LICENSE](./LICENSE)。
