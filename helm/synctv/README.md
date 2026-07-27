# SyncTV Helm Chart

Helm chart for deploying SyncTV. By default it installs one SyncTV Deployment plus chart-managed PostgreSQL 18 and Redis 8 services for the default install profile. Redis is optional for single-replica application mode, but recommended for production and required when SyncTV cluster mode is enabled.

## Features

- Single-process deployment for HTTP, gRPC, RTMP, STUN, and the management endpoint
- Independent HTTP and gRPC Kubernetes Services, even though both target the same process port
- Built-in PostgreSQL and Redis by default, with no extra dependency chart required
- Optional KubeBlocks-backed or external database modes
- Built-in HPA, PDB, probes, anti-affinity, NetworkPolicy, and Ingress templates

## Prerequisites

- Kubernetes 1.23.0+
- Helm 3.8.0+
- Optional: Ingress controller
- Optional: cert-manager
- Optional: KubeBlocks operator, required only in `kubeblocks` mode

## Database Modes

Both `postgresql.mode` and `redis.mode` support these modes:

- `standard`
  The chart creates and manages PostgreSQL / Redis resources itself. This is the default mode.
- `kubeblocks`
  The chart creates KubeBlocks `Cluster` resources and directly consumes the generated connection secrets.
- `external`
  The chart connects SyncTV to an existing PostgreSQL / Redis service and does not render the matching database resources.

## Quick Start

The default installation deploys:

- SyncTV
- PostgreSQL 18
- Redis 8

These database services are internal-only and are not exposed outside the cluster. For temporary external access, prefer `kubectl port-forward`.
The chart defaults to single-replica mode with `config.cluster.enabled=false`. Scaling beyond one replica requires cluster mode; the chart fails rendering for `replicaCount > 1` or `autoscaling.maxReplicas > 1` unless `config.cluster.enabled=true`. Local HLS backends work through publisher-node gRPC proxying, while `shared_file` with a real shared filesystem (`persistence.hls.existingClaim`) or `oss` with S3-compatible object storage is recommended for production HLS traffic.
Set `safety.allowStandaloneReplicas=true` only when you intentionally want multiple independent standalone pods.

Install the released OCI chart:

```bash
helm install synctv oci://ghcr.io/synctv-org/synctv/charts/synctv \
  --version 1.0.1-rc.1 \
  --namespace synctv \
  --create-namespace
```

The default parent OCI repository is `ghcr.io/synctv-org/synctv/charts`. Helm
appends the chart name, so the install reference ends with `/synctv`.
Maintainers can override the publishing target with `HELM_OCI_REPOSITORY`.

For local development, install from the chart source:

```bash
helm install synctv ./helm/synctv \
  --namespace synctv \
  --create-namespace
```

The release workflow packages and publishes released charts as OCI artifacts.
Public installs require the GHCR chart package to be public.

The default chart does not create an Ingress. For a quick smoke test, forward
the internal Service:

```bash
kubectl -n synctv port-forward svc/synctv 8080:8080
```

When `existingSecret` is not set, the chart auto-generates the built-in Secret
on first install and preserves existing values on upgrade. That is suitable for
simple production installs as long as the release Secret is backed up and kept
stable. Override values explicitly, or use `existingSecret`, when your
production process requires externally managed or pre-provisioned secrets:

```yaml
secrets:
  database:
    password: "replace-me"
  redis:
    password: "replace-me"
  jwt:
    secret: "replace-with-a-strong-random-secret"
  cluster:
    grpcSecret: "replace-me"
  security:
    credentialEncryptionKey: "64-hex-character-key"
    opaqueServerSetupSecret: "stable-random-secret"
  bootstrap:
    rootPassword: "replace-me"
```

Do not rotate `secrets.security.credentialEncryptionKey` or
`secrets.security.opaqueServerSetupSecret` casually; changing them can break
provider credential decryption or password authentication.

## Security

Server-side outbound requests use the global SSRF policy from
`config.security.ssrf`. SSRF protection is disabled by default so self-hosted
deployments can use private media sources. Public deployments should enable
SSRF protection and prefer explicit allowlists for trusted internal media
endpoints:

```yaml
config:
  security:
    ssrf:
      enabled: true
      allowPrivateNetworkTargets: false
      allowedHosts:
        - nas.example.internal
      allowedIpRanges:
        - 10.0.8.0/24
```

Set `allowPrivateNetworkTargets=true` only for deployments where all users and
configured provider endpoints are trusted.

## Standard Mode

In standard mode, database authentication settings live under `standard.auth`, not next to `mode`:

```yaml
postgresql:
  mode: standard
  standard:
    auth:
      username: synctv
      database: synctv
    persistence:
      size: 20Gi

redis:
  mode: standard
  standard:
    auth:
      username: ""
      database: 0
    persistence:
      size: 8Gi
```

Notes:

- PostgreSQL standard mode uses `SYNCTV_DATABASE_PASSWORD` from the chart secret
- Redis standard mode uses `SYNCTV_REDIS_PASSWORD` from the chart secret
- The PostgreSQL `18.1-bookworm` image must mount `/var/lib/postgresql`, not `/var/lib/postgresql/data`
- In standard mode, PostgreSQL and Redis are only reachable through in-cluster `ClusterIP` / Pod networking

## KubeBlocks Mode

In KubeBlocks mode, the chart no longer generates or manages static database passwords. It directly references the secrets generated by KubeBlocks.

PostgreSQL example:

```yaml
postgresql:
  mode: kubeblocks
  kubeblocks:
    clusterName: synctv-pg
    replicas: 2
    serviceVersion: "18.1.0"
```

Redis example:

```yaml
redis:
  mode: kubeblocks
  kubeblocks:
    clusterName: synctv-redis
    replicas: 2
    serviceVersion: "8.4.0"
    sentinel:
      replicas: 3
```

Notes:

- PostgreSQL connects to `<cluster>-postgresql-postgresql:5432` by default
- PostgreSQL uses `<cluster>-postgresql-account-postgres` only as the bootstrap system account
- SyncTV bootstraps and then runs as `postgresql.kubeblocks.appUsername` against `postgresql.kubeblocks.database`
- When `postgresql.kubeblocks.replicas > 1` and no explicit read URL Secret is configured, the chart creates `<cluster>-postgresql-read` for secondary pods and injects `SYNCTV_DATABASE_READ_HOST` / `SYNCTV_DATABASE_READ_PORT`
- SyncTV routes only allowlisted eventually-consistent reads to the read pool; strong reads, writes, migrations, and cache rebuilds use the primary connection
- Redis connects to `<cluster>-redis-redis:6379` by default
- Redis uses `<cluster>-redis-account-default` by default
- PostgreSQL and Redis secret keys are fixed as `username` / `password`
- The SyncTV app database password is stored in the chart secret as `SYNCTV_DATABASE_PASSWORD`; when using `existingSecret`, provide that key yourself
- KubeBlocks-generated database services are also internal-only; for external debugging, prefer `kubectl port-forward`
- KubeBlocks `terminationPolicy` defaults to `Retain` for PostgreSQL and Redis. Set it to `Delete` only for disposable test environments.
- The KubeBlocks Redis Sentinel component is part of the database operator topology. It does not automatically set SyncTV `redis.deployment_mode=sentinel`; this chart injects a stable Redis service endpoint. SyncTV cluster mode must not be combined with SyncTV Sentinel mode.

## Configuration Model

The chart renders a config file mounted at `/config/synctv.yaml` and injects sensitive values plus connection details through `SYNCTV_` environment variables.

The application uses split database/Redis configuration so credentials can stay in Secrets while the chart controls service endpoints:

| Section | Description |
|---------|-------------|
| `config.server` | API bind address, CORS, proxy settings, and gRPC transport settings |
| `config.publicIds` | Optional sqids settings for public API IDs |
| `config.management` | Management endpoint settings |
| `config.database` | Pool settings; actual host/port/user/password and optional read URL come from env vars |
| `config.redis` | Redis timeouts, pipeline buffer, key prefix, and deployment mode; connection details come from env vars |
| `config.cluster` | Cluster coordination and discovery settings |
| `config.jwt` | Token durations; signing secret comes from a secret |
| `config.bootstrap` | Bootstrap root-user settings |
| `config.livestream` | RTMP/HLS/pull timeout and cache settings |
| `config.fileStorage` | Uploaded file storage backends and product-level backend routing |
| `config.cache` | Business L1/L2 cache settings |
| `config.proxySliceCache` | Startup-only media proxy Range-slice cache settings |
| `config.mediaProviders` | Local built-in provider adapter request and connect timeouts |
| `config.webauthn` | Passkey relying-party settings |
| `config.webrtc` | Built-in STUN and WebRTC settings; external ICE servers are runtime settings |
| `config.requestRateLimits` | Shared HTTP and gRPC API category rate limits |
| `config.passwordComplexity` | Password policy for account credentials |
| `config.bufferSizes` | Internal queue sizes |

The chart creates separate Services for application traffic:

- `{{ release-name }}` exposes HTTP/REST.
- `{{ release-name }}-rtmp` exposes RTMP ingest when `rtmpService.enabled=true`.
- `{{ release-name }}-stun` exposes the built-in UDP STUN listener when `stunService.enabled=true` and `config.webrtc.enableBuiltinStun=true`; it is disabled by default because a ClusterIP STUN Service is not reachable by public WebRTC clients.
- `{{ release-name }}-metrics` exposes metrics when `metrics.enabled=true`.
- `{{ release-name }}-grpc` exposes gRPC only and targets the same container port as HTTP.

Important transport defaults:

- `config.server.grpcCompressionEnabled=true` enables gzip negotiation for public gRPC traffic and cluster gRPC calls.
- `config.fileStorage.backends.<name>.compression=zstd` controls PostgreSQL `file_blob_parts` compression for database file-storage backends; `compressionMinSizeBytes` gates small payloads and `compressionMinSavingsPercent=10` stores raw bytes when compression saves less than 10%. Database file storage uses permanent segments and serves HTTP Range from those segments.
- `config.fileStorage.backends.<name>.publicBaseUrl` is required for S3 file-storage backends because clients receive readable file URLs after upload or ownership proof validation. S3 file storage uses native multipart direct uploads for resumable GB-scale objects.
- For S3 file-storage credentials, mount a Kubernetes Secret and set `accessKeyIdFile` / `secretAccessKeyFile` so the generated ConfigMap stores file paths:

```yaml
config:
  fileStorage:
    defaultBackend: s3_public
    backends:
      s3_public:
        type: s3
        endpoint: https://s3.example.com
        bucket: synctv-files
        region: auto
        basePath: files/
        publicBaseUrl: https://cdn.example.com/files
        accessKeyIdFile: /run/secrets/file-storage-s3/access_key_id
        secretAccessKeyFile: /run/secrets/file-storage-s3/secret_access_key

extraVolumes:
  - name: file-storage-s3
    secret:
      secretName: synctv-file-storage-s3
extraVolumeMounts:
  - name: file-storage-s3
    mountPath: /run/secrets/file-storage-s3
    readOnly: true
```

- To expose the built-in STUN server, set `stunService.enabled=true`, use `stunService.type=LoadBalancer` or `NodePort`, and set `config.webrtc.stunExternalAddr` to the public client-reachable address.
- `config.redis.responseTimeoutSeconds=5` bounds how long a Redis command can wait for a response.
- `config.redis.pipelineBufferSize=512` controls the Redis connection manager pipeline buffer for bursty short-command workloads.

Ingress is disabled by default so the chart can install without assuming an
Ingress controller, DNS name, or cert-manager issuer. Set `ingress.enabled=true`
with `ingress.className`, `ingress.hosts`, optional annotations, and TLS values
for HTTP access. When `ingress.grpc.enabled=true`, the chart creates a second
Ingress that routes to the gRPC Service and uses independent
`ingress.grpc.annotations`. Each path defaults to `path: "/"` and
`pathType: Prefix`; set `pathType` explicitly to `Exact` or
`ImplementationSpecific` only when your ingress controller requires it.

The default topology spread policy uses `whenUnsatisfiable: ScheduleAnyway`.
This still biases replicas across zones, but avoids leaving pods pending on
single-zone clusters or during partial zone outages. Set
`topologySpread.whenUnsatisfiable=DoNotSchedule` only when strict skew is more
important than availability.

The application Role is rendered only for Kubernetes-backed cluster features:
`config.cluster.discoveryMode=k8s_dns` grants namespace-scoped pod/endpoints
read access, and `config.cluster.leaderElectionMode=k8s_lease` grants
namespace-scoped Lease access. Redis/static defaults do not require these API
permissions.

When using `existingSecret`, provide these keys with current names:

- `SYNCTV_DATABASE_PASSWORD` for PostgreSQL standard mode, external mode, and the KubeBlocks application role
- `SYNCTV_DATABASE_READ_URL` when `config.database.useSecretReadUrl=true`
- `SYNCTV_REDIS_PASSWORD` when Redis uses standard mode; provide it in external mode only when the external Redis requires password authentication; do not provide it for KubeBlocks mode
- `SYNCTV_JWT_SECRET`
- `SYNCTV_CLUSTER_SECRET`
- `SYNCTV_SECURITY_CREDENTIAL_ENCRYPTION_KEY`
- `SYNCTV_SECURITY_OPAQUE_SERVER_SETUP_SECRET`
- `SYNCTV_BOOTSTRAP_ROOT_PASSWORD` when `config.bootstrap.createRootUser=true`
- `SYNCTV_MANAGEMENT_AUTH_TOKEN` when `config.management.transport=tcp`
- `SYNCTV_METRICS_AUTH_BEARER_TOKEN` when `metrics.enabled=true` and `metrics.auth.mode=bearer_token`
- `SYNCTV_METRICS_AUTH_BASIC_USERNAME` and `SYNCTV_METRICS_AUTH_BASIC_PASSWORD` when `metrics.enabled=true` and `metrics.auth.mode=basic`
- `SYNCTV_LIVESTREAM_HLS_STORAGE_ACCESS_KEY_ID` when `config.livestream.hlsStorage.type=oss`
- `SYNCTV_LIVESTREAM_HLS_STORAGE_SECRET_ACCESS_KEY` when `config.livestream.hlsStorage.type=oss`

HLS storage rendering fails fast for invalid combinations: `hlsStorage.type=file/shared_file` requires a non-empty `hlsStorage.path`, and `hlsStorage.type=shared_file` requires `persistence.hls.existingClaim` so `emptyDir` is not mistaken for shared storage. Cluster mode can use `memory` or local `file` through publisher-node HLS proxying, but `shared_file` or OSS is the recommended production model.

## Verify the Deployment

```bash
kubectl get pods -n synctv
kubectl get svc -n synctv
kubectl logs -n synctv -l app.kubernetes.io/name=synctv -f
```

Default probe paths match the actual service routes:

- `startupProbe`: `/health/ready`
- `livenessProbe`: `/health/live`
- `readinessProbe`: `/health/ready`

## Upgrade

```bash
helm upgrade synctv ./helm/synctv \
  --namespace synctv \
  --values my-values.yaml
```

## Uninstall

```bash
helm uninstall synctv -n synctv
```

### Security Best Practices

#### 1. Generate Secure Secrets

```bash
# JWT Secret (256-bit)
openssl rand -base64 32

# Generic secrets
openssl rand -hex 32

# Credential encryption key, exactly 64 hex characters
openssl rand -hex 32

# OPAQUE setup secret
openssl rand -base64 48
```

The built-in chart Secret uses generated values when these fields are left
empty. Use the commands above when you need to provide values through
`--set`, a private values file, or an external secret manager.

#### 2. Use External Secrets

```yaml
# Skip creating the built-in Secret (use your own)
existingSecret: "my-external-synctv-secret"
```

#### 3. Enable Network Policies

```yaml
networkPolicy:
  enabled: true
  policyTypes:
    - Ingress
    - Egress
  ingressControllerNamespaces:
    - ingress-nginx
  metricsSourceNamespaces:
    - monitoring
  allowAnyRtmpSource: false
  rtmpSourceCIDRs:
    - "203.0.113.0/24"
  allowAnyStunSource: false
  allowAnyExternalHttpEgress: false
  externalHttpCIDRs:
    - "203.0.113.0/24"
  allowAnyExternalDatabaseEgress: false
  externalPostgresqlCIDRs: []
  externalRedisCIDRs: []
```

Notes:

- `ingressControllerNamespaces` controls which namespaces may reach the SyncTV API through an ingress controller
- `metricsSourceNamespaces` controls which namespaces may scrape the metrics port
- The template matches namespaces using the standard Kubernetes namespace label `kubernetes.io/metadata.name`
- SyncTV API, gRPC, RTMP, STUN, metrics, PDB, and app NetworkPolicy selectors include `app.kubernetes.io/component=app`, so chart-managed PostgreSQL/Redis pods are not selected as application endpoints.
- When NetworkPolicy ingress isolation is enabled with chart-managed PostgreSQL or Redis, the chart also renders dependency-specific ingress policies that allow only SyncTV application pods to reach those dependency ports.
- Empty CIDR lists do not create broad allow rules. Use explicit CIDRs, or set `allowAnyRtmpSource`, `allowAnyStunSource`, `allowAnyExternalHttpEgress`, or `allowAnyExternalDatabaseEgress` when that traffic is intentionally unrestricted.
- When `postgresql.mode=external` or `redis.mode=external`, enabling NetworkPolicy requires explicit external database CIDRs or `allowAnyExternalDatabaseEgress=true`.

## Upgrading

```bash
helm upgrade synctv ./helm/synctv \
  --namespace synctv \
  --values my-values.yaml
```

## Uninstallation

```bash
helm uninstall synctv -n synctv
kubectl delete namespace synctv
```

## Monitoring

Helm defaults to `metrics.auth.mode=bearer_token`, which means:

- The chart stores a bearer token in the SyncTV secret
- `ServiceMonitor` and `VMServiceScrape` use that bearer token automatically

Example:

```yaml
metrics:
  enabled: true
  auth:
    mode: bearer_token
  serviceMonitor:
    enabled: true
    labels:
      prometheus: kube-prometheus
networkPolicy:
  metricsSourceNamespaces:
    - monitoring
```

Keep `ServiceMonitor` and `VMServiceScrape` in the SyncTV release namespace when
using static bearer/basic auth or chart-managed metrics TLS. Operator Secret
references are namespace-scoped, so cross-namespace scrape objects only work
without those Secret references, for example with `metrics.auth.mode=kubernetes`.

If you want Kubernetes-native `TokenReview` + `SubjectAccessReview` auth instead:

- SyncTV validates the scraper's service account token with Kubernetes `TokenReview`
- SyncTV authorizes `/metrics` access with `SubjectAccessReview`
- You grant scrape access by listing allowed service accounts in the chart values
- The SyncTV image must be compiled with the `k8s` feature. The chart renders RBAC and token settings, but it cannot change the binary feature set.

```yaml
metrics:
  enabled: true
  auth:
    mode: kubernetes
    kubernetes:
      allowedServiceAccounts:
        - name: prometheus-kube-prometheus-prometheus
          namespace: monitoring
```

If you want static username/password instead, switch to basic auth:

```yaml
metrics:
  enabled: true
  auth:
    mode: basic
  serviceMonitor:
    enabled: true
secrets:
  metrics:
    basicUsername: metrics
    basicPassword: change-me
```

If you want HTTPS on the metrics endpoint, enable cert-manager-managed TLS for metrics. The chart will mount the generated certificate into the container automatically. When `issuerRef.name` is empty, the chart creates a namespace-local self-signed issuer:

```yaml
metrics:
  enabled: true
  tls:
    enabled: true
    issuerRef:
      name: ""
      kind: Issuer
  serviceMonitor:
    enabled: true
```

To use an existing cert-manager issuer instead:

```yaml
metrics:
  enabled: true
  tls:
    enabled: true
    issuerRef:
      name: monitoring-ca
      kind: ClusterIssuer
  vmServiceScrape:
    enabled: true
```

You can then scrape through either Prometheus Operator (`ServiceMonitor`) or VictoriaMetrics Operator (`VMServiceScrape`).

Alerting rules are disabled by default because `PrometheusRule` is a
Prometheus Operator CRD. Enable them only on clusters where that CRD is
installed:

```yaml
metrics:
  enabled: true
alerting:
  enabled: true
```

## Architecture

```
                    API Service (ClusterIP)
                    optional Ingress
                         |
              +----------v-----------+
              |  SyncTV Deployment   |
              |  (1 replica default, |
              |   scale-out optional)|
              |                      |
              |  HTTP API:  8080     |
              |  gRPC:      8080     |
              |  RTMP:      1935     |
              |  STUN:      3478/udp |
              +----+----------+------+
                   ^          ^
                   |          |
              RTMP Service  STUN Service
                   |          |
           +-------+    +----+-----+
           |             |          |
      +----v-----+  +---v----+  (Cluster)
      |PostgreSQL|  | Redis  |  Node Discovery
      |(Internal)|  |(Internal) via Redis
      +----------+  +--------+
```

## Production Checklist

- [ ] Decide whether to use the chart-generated Secret or an externally managed `existingSecret`
- [ ] Back up generated secrets or keep externally managed secrets stable across upgrades
- [ ] Keep `secrets.security.credentialEncryptionKey` and `secrets.security.opaqueServerSetupSecret` stable
- [ ] Configure ingress and enable TLS when exposing SyncTV outside the cluster
- [ ] Set appropriate resource limits
- [ ] Choose the HLS model before enabling autoscaling: publisher-node proxy for small deployments, or shared_file/OSS for production traffic
- [ ] Enable `config.cluster.enabled=true` before using multiple replicas or autoscaling beyond one pod
- [ ] Enable autoscaling (HPA)
- [ ] Configure pod disruption budget
- [ ] Enable network policies
- [ ] Set up monitoring (Prometheus/Grafana)
- [ ] Configure backup for PostgreSQL
- [ ] Review connection limits for your scale

## Support

- Website: https://syncs.tv
- Documentation: https://docs.syncs.tv
- GitHub: https://github.com/synctv-org/synctv
- Issues: https://github.com/synctv-org/synctv/issues

## License

MIT. See the repository [LICENSE](../../LICENSE) file for details.
