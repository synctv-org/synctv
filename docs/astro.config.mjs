import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';
import starlightCodeblockFullscreen from 'starlight-codeblock-fullscreen';
import starlightLinksValidator from 'starlight-links-validator';
import { docsBase, docsSite, githubEditUrl, githubUrl } from './src/lib/project';

export default defineConfig({
  site: docsSite,
  base: docsBase,
  integrations: [
    starlight({
      title: {
        'zh-CN': 'SyncTV 文档',
        en: 'SyncTV Docs',
      },
      description: 'SyncTV Server 安装、配置、管理和运维文档',
      titleDelimiter: '·',
      logo: {
        src: './public/logo-notext.svg',
        alt: 'SyncTV',
      },
      defaultLocale: 'root',
      locales: {
        root: {
          label: '简体中文',
          lang: 'zh-CN',
        },
        en: {
          label: 'English',
          lang: 'en',
        },
      },
      favicon: '/favicon.svg',
      head: [
        {
          tag: 'meta',
          attrs: {
            property: 'og:type',
            content: 'website',
          },
        },
        {
          tag: 'meta',
          attrs: {
            name: 'twitter:card',
            content: 'summary_large_image',
          },
        },
        {
          tag: 'meta',
          attrs: {
            property: 'og:image',
            content: '/og.svg',
          },
        },
        {
          tag: 'meta',
          attrs: {
            name: 'twitter:image',
            content: '/og.svg',
          },
        },
        {
          tag: 'meta',
          attrs: {
            name: 'theme-color',
            content: '#8788fe',
          },
        },
      ],
      customCss: ['./src/styles/custom.css'],
      editLink: {
        baseUrl: githubEditUrl,
      },
      lastUpdated: true,
      tableOfContents: {
        minHeadingLevel: 2,
        maxHeadingLevel: 3,
      },
      pagefind: {
        ranking: {
          pageLength: 0.05,
          termFrequency: 0.2,
          termSaturation: 1.4,
          termSimilarity: 6,
        },
      },
      expressiveCode: {
        emitExternalStylesheet: true,
        removeUnusedThemes: true,
        styleOverrides: {
          borderRadius: '0.85rem',
        },
      },
      plugins: [
        starlightLinksValidator({
          errorOnRelativeLinks: false,
          failOnError: true,
        }),
        starlightCodeblockFullscreen({
          addToUntitledBlocks: true,
          enableEscapeKey: true,
          exitOnBrowserBack: true,
        }),
      ],
      social: [
        {
          icon: 'github',
          label: 'GitHub',
          href: githubUrl,
        },
      ],
      sidebar: [
        {
          label: '安装与升级',
          translations: { en: 'Install and Upgrade' },
          items: [
            { label: '选择部署方式', translations: { en: 'Choose a Deployment' }, slug: 'install/choose-path' },
            { label: 'Docker Compose 快速安装', translations: { en: 'Docker Compose Quick Start' }, slug: 'install/quick-start' },
            { label: 'Docker Compose 参考', translations: { en: 'Docker Compose Reference' }, slug: 'install/docker-compose' },
            { label: 'Helm / Kubernetes', translations: { en: 'Helm / Kubernetes' }, slug: 'install/helm' },
            { label: '生产部署清单', translations: { en: 'Production Checklist' }, slug: 'install/production-checklist' },
            { label: '升级与迁移', translations: { en: 'Upgrades and Migrations' }, slug: 'operations/upgrades' },
            { label: '备份与恢复', translations: { en: 'Backup and Restore' }, slug: 'operations/backup-restore' },
          ],
          collapsed: true,
        },
        {
          label: '配置',
          translations: { en: 'Configuration' },
          items: [
            { label: '配置加载与优先级', translations: { en: 'Loading and Precedence' }, slug: 'configuration/how-configuration-works' },
            { label: '配置示例', translations: { en: 'Configuration Examples' }, slug: 'configuration/full-example' },
            { label: '安全与密钥', translations: { en: 'Security and Secrets' }, slug: 'configuration/security' },
            { label: '数据库与 Redis', translations: { en: 'Database and Redis' }, slug: 'configuration/database-and-redis' },
            { label: '服务监听与运行路径', translations: { en: 'Listener and Runtime Paths' }, slug: 'configuration/server-and-runtime' },
            { label: '初始化 root 用户', translations: { en: 'Bootstrap Root User' }, slug: 'configuration/bootstrap' },
            {
              label: '认证与外部服务',
              translations: { en: 'Authentication and External Services' },
              items: [
                { label: 'WebAuthn / Passkeys', slug: 'configuration/webauthn' },
                { label: '邮件与 OAuth2', translations: { en: 'Email and OAuth2' }, slug: 'configuration/email-oauth2' },
                { label: '媒体 Provider', translations: { en: 'Media Providers' }, slug: 'configuration/media-providers' },
              ],
            },
            {
              label: '媒体与实时通信',
              translations: { en: 'Media and Realtime' },
              items: [
                { label: 'WebRTC', slug: 'configuration/webrtc' },
                { label: '直播', translations: { en: 'Livestreaming' }, slug: 'configuration/livestream' },
                { label: '公开 ID', translations: { en: 'Public IDs' }, slug: 'configuration/public-ids' },
              ],
            },
            {
              label: '扩展与性能',
              translations: { en: 'Scale and Performance' },
              items: [
                { label: '集群', translations: { en: 'Cluster' }, slug: 'configuration/cluster' },
                { label: 'Metrics', slug: 'configuration/metrics' },
                { label: '业务缓存', translations: { en: 'Business Cache' }, slug: 'configuration/cache' },
                { label: 'Proxy slice cache', slug: 'configuration/proxy-slice-cache' },
                { label: '限流与连接限制', translations: { en: 'Rate and Connection Limits' }, slug: 'configuration/rate-limits' },
                { label: '内部缓冲区', translations: { en: 'Internal Buffers' }, slug: 'configuration/buffer-sizes' },
              ],
            },
          ],
          collapsed: true,
        },
        {
          label: '管理',
          translations: { en: 'Administration' },
          items: [
            { label: '管理入口', translations: { en: 'Administration Overview' }, slug: 'admin' },
            { label: '用户', translations: { en: 'Users' }, slug: 'admin/users' },
            { label: '房间与成员', translations: { en: 'Rooms and Members' }, slug: 'admin/rooms-members' },
            { label: '角色、权限与用户偏好', translations: { en: 'Roles, Permissions, and Preferences' }, slug: 'admin/permissions' },
            { label: '审核与治理', translations: { en: 'Reviews and Moderation' }, slug: 'admin/reviews-moderation' },
            { label: 'Provider', slug: 'admin/providers' },
            { label: '运行设置', translations: { en: 'Runtime Settings' }, slug: 'admin/runtime-settings' },
            { label: '认证与安全', translations: { en: 'Authentication and Security' }, slug: 'admin/authentication-security' },
            { label: '数据与保留策略', translations: { en: 'Data and Retention' }, slug: 'operations/data-retention' },
            { label: '维护任务', translations: { en: 'Maintenance Tasks' }, slug: 'admin/maintenance' },
          ],
          collapsed: true,
        },
        {
          label: '运维',
          translations: { en: 'Operations' },
          items: [
            { label: '排障', translations: { en: 'Troubleshooting' }, slug: 'operations/troubleshooting' },
            { label: '观测与运行手册', translations: { en: 'Observability Runbook' }, slug: 'operations/observability' },
            { label: '配置校验', translations: { en: 'Configuration Validation' }, slug: 'operations/config-validation' },
            { label: '容量规划', translations: { en: 'Capacity Planning' }, slug: 'operations/capacity-planning' },
            { label: '安全加固与密钥轮换', translations: { en: 'Hardening and Secret Rotation' }, slug: 'operations/security-hardening-and-rotation' },
            { label: '部署与运行边界', translations: { en: 'Deployment and Runtime Boundaries' }, slug: 'operations/deployment-boundaries' },
          ],
          collapsed: true,
        },
        {
          label: '开发与集成',
          translations: { en: 'Development and Integration' },
          items: [
            { label: '本地开发', translations: { en: 'Local Development' }, slug: 'develop/local-development' },
            { label: '系统架构', translations: { en: 'Architecture' }, slug: 'overview/architecture' },
            { label: '客户端集成', translations: { en: 'Client Integration' }, slug: 'develop/client-integration' },
            { label: 'SDK 与 API 示例', translations: { en: 'SDK and API Examples' }, slug: 'develop/sdk-and-api-examples' },
            { label: 'Provider 开发', translations: { en: 'Provider Development' }, slug: 'develop/provider-development' },
            { label: '播放与代理协议', translations: { en: 'Playback and Proxy Protocol' }, slug: 'develop/playback-and-proxy' },
            { label: '播放后台任务', translations: { en: 'Playback Background Workers' }, slug: 'develop/playback-background-workers' },
            { label: 'Realtime API', slug: 'develop/realtime-api' },
            { label: 'Realtime 资源观察', translations: { en: 'Realtime Resource Observation' }, slug: 'develop/realtime-resource-observation' },
            { label: '缓存一致性', translations: { en: 'Cache Consistency' }, slug: 'develop/cache-consistency' },
            { label: '实现契约', translations: { en: 'Implementation Contracts' }, slug: 'develop/implementation-contracts' },
            { label: '项目发布流程', translations: { en: 'Project Release Process' }, slug: 'operations/release' },
          ],
          collapsed: true,
        },
        {
          label: '参考',
          translations: { en: 'Reference' },
          items: [
            { label: '配置字段', translations: { en: 'Configuration Fields' }, slug: 'reference/configuration-index' },
            { label: '环境变量', translations: { en: 'Environment Variables' }, slug: 'reference/environment-variables' },
            { label: 'Runtime settings', slug: 'reference/runtime-settings' },
            { label: 'CLI', slug: 'reference/cli' },
            { label: 'OpenAPI', slug: 'reference/openapi' },
            { label: 'gRPC', slug: 'reference/grpc' },
            { label: '错误码', translations: { en: 'Errors' }, slug: 'reference/errors' },
            { label: 'API 与 protobuf 演进', translations: { en: 'API and Protobuf Evolution' }, slug: 'reference/api-versioning' },
            { label: 'Metrics Catalog', slug: 'reference/metrics-catalog' },
            { label: '已知限制', translations: { en: 'Known Limitations' }, slug: 'reference/limitations' },
            { label: '术语', translations: { en: 'Glossary' }, slug: 'reference/glossary' },
          ],
          collapsed: true,
        },
      ],
    }),
  ],
});
