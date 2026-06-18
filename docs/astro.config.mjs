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
      description: 'SyncTV 安装、配置、部署和运维文档',
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
          label: '总览',
          translations: { en: 'Overview' },
          items: [
            { label: '项目介绍', translations: { en: 'Overview' }, link: '/' },
            { label: '文档导览', translations: { en: 'Documentation Map' }, slug: 'overview/documentation-map' },
            { label: '常见任务流程', translations: { en: 'Common Workflows' }, slug: 'overview/common-workflows' },
            { label: '快速开始', translations: { en: 'Quick Start' }, slug: 'install/quick-start' },
            { label: '术语速查', translations: { en: 'Glossary' }, slug: 'reference/glossary' },
          ],
        },
        {
          label: '概念',
          translations: { en: 'Concepts' },
          items: [
            { label: '概念总览', translations: { en: 'Concepts Overview' }, slug: 'concepts' },
            { label: '房间', translations: { en: 'Rooms' }, slug: 'concepts/rooms' },
            { label: '媒体源', translations: { en: 'Media Sources' }, slug: 'concepts/media-providers' },
            { label: '播放模型', translations: { en: 'Playback Model' }, slug: 'concepts/playback-model' },
            { label: '权限模型', translations: { en: 'Permissions Model' }, slug: 'concepts/permissions' },
            { label: '运行边界', translations: { en: 'Runtime Boundaries' }, slug: 'concepts/runtime-boundaries' },
          ],
        },
        {
          label: '使用 SyncTV',
          translations: { en: 'Use SyncTV' },
          items: [
            { label: '使用入口', translations: { en: 'Use Overview' }, slug: 'use' },
            { label: '登录与账号安全', translations: { en: 'Sign In and Account Security' }, slug: 'use/accounts-security' },
            { label: '创建和加入房间', translations: { en: 'Create and Join Rooms' }, slug: 'use/rooms' },
            { label: '同步观看', translations: { en: 'Watch Together' }, slug: 'use/watch-together' },
            { label: '聊天', translations: { en: 'Chat' }, slug: 'use/chat' },
            {
              label: '房间、权限与用户偏好',
              translations: { en: 'Rooms, Permissions, and Preferences' },
              slug: 'use/rooms-permissions',
            },
            {
              label: '添加媒体',
              translations: { en: 'Add Media' },
              slug: 'use/media-sources',
            },
            {
              label: '播放与代理模型',
              translations: { en: 'Playback and Proxy Model' },
              slug: 'use/playback-and-proxy',
            },
            {
              label: '通知与个人设置',
              translations: { en: 'Notifications and Preferences' },
              slug: 'use/preferences-notifications',
            },
            { label: '用户排障', translations: { en: 'User Troubleshooting' }, slug: 'use/troubleshooting' },
          ],
        },
        {
          label: '管理 SyncTV',
          translations: { en: 'Administer SyncTV' },
          items: [
            {
              label: '管理入口',
              translations: { en: 'Admin Overview' },
              slug: 'admin',
            },
            { label: '用户管理', translations: { en: 'User Management' }, slug: 'admin/users' },
            {
              label: '房间与成员管理',
              translations: { en: 'Room and Member Management' },
              slug: 'admin/rooms-members',
            },
            {
              label: '审核与治理',
              translations: { en: 'Reviews and Moderation' },
              slug: 'admin/reviews-moderation',
            },
            { label: 'Provider 管理', translations: { en: 'Provider Management' }, slug: 'admin/providers' },
            { label: '运行设置', translations: { en: 'Runtime Settings' }, slug: 'admin/runtime-settings' },
            { label: '维护任务', translations: { en: 'Maintenance Tasks' }, slug: 'admin/maintenance' },
            {
              label: '认证与安全模型',
              translations: { en: 'Authentication and Security Model' },
              slug: 'admin/authentication-security',
            },
            {
              label: '数据、隐私与保留策略',
              translations: { en: 'Data, Privacy, and Retention' },
              slug: 'operations/data-retention',
            },
            {
              label: '安全加固与密钥轮换',
              translations: { en: 'Security Hardening and Rotation' },
              slug: 'operations/security-hardening-and-rotation',
            },
            {
              label: 'CLI 参考',
              translations: { en: 'CLI Reference' },
              slug: 'reference/cli',
            },
            {
              label: 'Runtime settings 参考',
              translations: { en: 'Runtime Settings Reference' },
              slug: 'reference/runtime-settings',
            },
          ],
          collapsed: false,
        },
        {
          label: '安装与升级',
          translations: { en: 'Install and Upgrade' },
          items: [
            {
              label: '部署路径选择',
              translations: { en: 'Choose a Deployment Path' },
              slug: 'install/choose-path',
            },
            {
              label: '快速开始',
              translations: { en: 'Quick Start' },
              slug: 'install/quick-start',
            },
            {
              label: 'Docker Compose 部署',
              translations: { en: 'Docker Compose Deployment' },
              slug: 'install/docker-compose',
            },
            {
              label: 'Helm 部署',
              translations: { en: 'Helm Deployment' },
              slug: 'install/helm',
            },
            {
              label: '生产部署清单',
              translations: { en: 'Production Checklist' },
              slug: 'install/production-checklist',
            },
            {
              label: '备份与恢复',
              translations: { en: 'Backup and Restore' },
              slug: 'operations/backup-restore',
            },
            {
              label: '升级与迁移',
              translations: { en: 'Upgrades and Migrations' },
              slug: 'operations/upgrades',
            },
            {
              label: '发布流程',
              translations: { en: 'Release Process' },
              slug: 'operations/release',
            },
          ],
          collapsed: false,
        },
        {
          label: '配置 SyncTV',
          translations: { en: 'Configure SyncTV' },
          items: [
            {
              label: '配置总索引',
              translations: { en: 'Configuration Index' },
              slug: 'reference/configuration-index',
            },
            {
              label: '配置文件如何工作',
              translations: { en: 'How Configuration Works' },
              slug: 'configuration/how-configuration-works',
            },
            {
              label: '完整配置示例',
              translations: { en: 'Full Configuration Example' },
              slug: 'configuration/full-example',
            },
            {
              label: '服务监听与运行时路径',
              translations: { en: 'Server Listener and Runtime Paths' },
              slug: 'configuration/server-and-runtime',
            },
            {
              label: '安全与密钥',
              translations: { en: 'Security and Secrets' },
              slug: 'configuration/security',
            },
            {
              label: '数据库与 Redis',
              translations: { en: 'Database and Redis' },
              slug: 'configuration/database-and-redis',
            },
            {
              label: '业务缓存',
              translations: { en: 'Business Cache' },
              slug: 'configuration/cache',
            },
            {
              label: 'Proxy slice cache',
              translations: { en: 'Proxy Slice Cache' },
              slug: 'configuration/proxy-slice-cache',
            },
            {
              label: 'Metrics 监控',
              translations: { en: 'Metrics Monitoring' },
              slug: 'configuration/metrics',
            },
            {
              label: '限流与连接限制',
              translations: { en: 'Rate Limits and Connection Limits' },
              slug: 'configuration/rate-limits',
            },
            {
              label: '媒体 Provider',
              translations: { en: 'Media Providers' },
              slug: 'configuration/media-providers',
            },
            {
              label: '邮件与 OAuth2',
              translations: { en: 'Email and OAuth2' },
              slug: 'configuration/email-oauth2',
            },
            {
              label: 'WebAuthn 配置',
              translations: { en: 'WebAuthn and Passkeys' },
              slug: 'configuration/webauthn',
            },
            {
              label: 'WebRTC 配置',
              translations: { en: 'WebRTC Configuration' },
              slug: 'configuration/webrtc',
            },
            {
              label: '直播配置',
              translations: { en: 'Livestream Configuration' },
              slug: 'configuration/livestream',
            },
            {
              label: '集群配置',
              translations: { en: 'Cluster Configuration' },
              slug: 'configuration/cluster',
            },
            {
              label: '公开 ID',
              translations: { en: 'Public IDs' },
              slug: 'configuration/public-ids',
            },
            {
              label: '初始化 root 用户',
              translations: { en: 'Bootstrap Root User' },
              slug: 'configuration/bootstrap',
            },
            {
              label: '内部缓冲区',
              translations: { en: 'Internal Buffers' },
              slug: 'configuration/buffer-sizes',
            },
          ],
          collapsed: false,
        },
        {
          label: '开发与集成',
          translations: { en: 'Develop with SyncTV' },
          items: [
            {
              label: '客户端集成指南',
              translations: { en: 'Client Integration Guide' },
              slug: 'develop/client-integration',
            },
            {
              label: 'SDK 与 API 示例',
              translations: { en: 'SDK and API Examples' },
              slug: 'develop/sdk-and-api-examples',
            },
            {
              label: 'Realtime API',
              translations: { en: 'Realtime API' },
              slug: 'develop/realtime-api',
            },
            {
              label: 'Realtime 资源观察',
              translations: { en: 'Realtime Resource Observation' },
              slug: 'develop/realtime-resource-observation',
            },
            {
              label: '缓存一致性开发指南',
              translations: { en: 'Cache Consistency Development Guide' },
              slug: 'develop/cache-consistency',
            },
            {
              label: '实现契约',
              translations: { en: 'Implementation Contracts' },
              slug: 'develop/implementation-contracts',
            },
            {
              label: 'OpenAPI 文档入口',
              translations: { en: 'OpenAPI Access' },
              slug: 'reference/openapi',
            },
            {
              label: 'gRPC 调试',
              translations: { en: 'gRPC Debugging' },
              slug: 'reference/grpc',
            },
            {
              label: '错误参考',
              translations: { en: 'Errors' },
              slug: 'reference/errors',
            },
            {
              label: 'API 与 protobuf 演进策略',
              translations: { en: 'API and Protobuf Evolution' },
              slug: 'reference/api-versioning',
            },
            {
              label: '文档写作规范',
              translations: { en: 'Documentation Style Guide' },
              slug: 'develop/documentation-style-guide',
            },
          ],
          collapsed: false,
        },
        {
          label: '运维',
          translations: { en: 'Operations' },
          items: [
            {
              label: '排障入口',
              translations: { en: 'Troubleshooting' },
              slug: 'operations/troubleshooting',
            },
            {
              label: '配置校验',
              translations: { en: 'Configuration Validation' },
              slug: 'operations/config-validation',
            },
            {
              label: '观测与运行手册',
              translations: { en: 'Observability Runbook' },
              slug: 'operations/observability',
            },
            {
              label: '容量规划',
              translations: { en: 'Capacity Planning' },
              slug: 'operations/capacity-planning',
            },
          ],
          collapsed: false,
        },
        {
          label: '参考',
          translations: { en: 'Reference' },
          items: [
            {
              label: '配置总索引',
              translations: { en: 'Configuration Index' },
              slug: 'reference/configuration-index',
            },
            {
              label: '常用环境变量',
              translations: { en: 'Environment Variables' },
              slug: 'reference/environment-variables',
            },
            {
              label: 'Runtime settings 参考',
              translations: { en: 'Runtime Settings Reference' },
              slug: 'reference/runtime-settings',
            },
            {
              label: 'Metrics Catalog',
              translations: { en: 'Metrics Catalog' },
              slug: 'reference/metrics-catalog',
            },
            {
              label: '能力限制与非目标',
              translations: { en: 'Limitations and Non-goals' },
              slug: 'reference/limitations',
            },
          ],
          collapsed: true,
        },
      ],
    }),
  ],
});
