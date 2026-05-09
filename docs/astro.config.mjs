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
          label: '开始',
          translations: { en: 'Start' },
          items: [
            { label: '项目介绍', translations: { en: 'Overview' }, link: '/' },
            { label: '快速开始', translations: { en: 'Quick Start' }, slug: 'guides/quick-start' },
            { label: '架构总览', translations: { en: 'Architecture Overview' }, slug: 'guides/architecture' },
            {
              label: '术语速查',
              translations: { en: 'Glossary' },
              slug: 'reference/glossary',
            },
            { label: '文档导览', translations: { en: 'Documentation Map' }, slug: 'guides/documentation-map' },
          ],
        },
        {
          label: '部署',
          translations: { en: 'Deployment' },
          items: [
            {
              label: '部署路径选择',
              translations: { en: 'Choose a Deployment Path' },
              slug: 'deployment/choose-path',
            },
            {
              label: 'Docker Compose 部署',
              translations: { en: 'Docker Compose Deployment' },
              slug: 'deployment/docker-compose',
            },
            {
              label: 'Helm 部署',
              translations: { en: 'Helm Deployment' },
              slug: 'deployment/helm',
            },
            {
              label: '生产部署清单',
              translations: { en: 'Production Checklist' },
              slug: 'deployment/production-checklist',
            },
          ],
          collapsed: false,
        },
        {
          label: '使用与集成',
          translations: { en: 'Use and Integrate' },
          items: [
            {
              label: '管理员操作手册',
              translations: { en: 'Administration Runbook' },
              slug: 'guides/administration',
            },
            {
              label: '认证与安全模型',
              translations: { en: 'Authentication and Security Model' },
              slug: 'guides/security-model',
            },
            {
              label: '房间、权限与用户偏好',
              translations: { en: 'Rooms, Permissions, and Preferences' },
              slug: 'guides/rooms-permissions',
            },
            {
              label: '客户端集成指南',
              translations: { en: 'Client Integration Guide' },
              slug: 'guides/client-integration',
            },
            {
              label: 'Realtime API',
              translations: { en: 'Realtime API' },
              slug: 'guides/realtime-api',
            },
          ],
          collapsed: false,
        },
        {
          label: '配置参考',
          translations: { en: 'Configuration' },
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
          label: '运维',
          translations: { en: 'Operations' },
          items: [
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
            {
              label: '数据、隐私与保留策略',
              translations: { en: 'Data, Privacy, and Retention' },
              slug: 'operations/data-retention',
            },
            {
              label: '排障入口',
              translations: { en: 'Troubleshooting' },
              slug: 'operations/troubleshooting',
            },
          ],
          collapsed: false,
        },
        {
          label: '参考',
          translations: { en: 'Reference' },
          items: [
            { label: 'CLI 参考', translations: { en: 'CLI Reference' }, slug: 'reference/cli' },
            {
              label: 'Runtime settings 参考',
              translations: { en: 'Runtime Settings Reference' },
              slug: 'reference/runtime-settings',
            },
            {
              label: '常用环境变量',
              translations: { en: 'Environment Variables' },
              slug: 'reference/environment-variables',
            },
            {
              label: 'OpenAPI 文档入口',
              translations: { en: 'OpenAPI Access' },
              slug: 'reference/openapi',
            },
            { label: 'gRPC 调试', translations: { en: 'gRPC Debugging' }, slug: 'reference/grpc' },
          ],
          collapsed: true,
        },
      ],
    }),
  ],
});
