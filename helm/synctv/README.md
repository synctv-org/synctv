# SyncTV Helm Chart

用于部署 SyncTV 的 Helm Chart。默认安装一个 SyncTV Deployment，并自动部署项目自身依赖的 PostgreSQL 18 和 Redis 8。

## 特性

- 单进程部署 HTTP、gRPC、RTMP、STUN 与管理接口
- 默认内置 PostgreSQL 与 Redis，无需额外依赖 chart
- 可切换为 KubeBlocks 管理数据库
- 内置 HPA、PDB、探针、反亲和、NetworkPolicy 与 Ingress

## 前置要求

- Kubernetes 1.23+
- Helm 3.8+
- 可选：Ingress Controller
- 可选：cert-manager
- 可选：已安装 KubeBlocks Operator（仅在 `kubeblocks` 模式下需要）

## 数据库模式

`postgresql.mode` 和 `redis.mode` 都支持以下两种模式：

- `standard`
  Chart 自己创建 PostgreSQL / Redis 资源。这是默认模式。
- `kubeblocks`
  Chart 创建 KubeBlocks `Cluster` 资源，并直接消费 KubeBlocks 自动生成的连接 Secret。

## 快速开始

默认安装会同时部署：

- SyncTV
- PostgreSQL 18
- Redis 8

这些数据库服务仅用于集群内访问，不提供面向集群外的数据库暴露能力。需要临时从集群外访问时，只建议使用 `kubectl port-forward`。
此外，Helm chart 默认会开启 `cluster.enabled=true`，因为这个部署形态本身就是多副本、Redis 驱动的集群运行模式。

```bash
helm install synctv ./helm/synctv \
  --namespace synctv \
  --create-namespace
```

生产环境至少应覆盖这些秘密：

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
  bootstrap:
    rootPassword: "replace-me"
```

## 标准模式

标准模式下，数据库认证配置放在 `standard.auth` 内，而不是 `mode` 同级：

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

说明：

- PostgreSQL 标准模式使用 chart Secret 中的 `SYNCTV_DATABASE_PASSWORD`
- Redis 标准模式使用 chart Secret 中的 `SYNCTV_REDIS_PASSWORD`
- PostgreSQL 18 官方镜像应挂载 `/var/lib/postgresql`，不能继续挂 `/var/lib/postgresql/data`
- 标准模式下 PostgreSQL 和 Redis 都只通过集群内 `ClusterIP` / Pod 网络访问

## KubeBlocks 模式

KubeBlocks 模式下，chart 不再生成和使用数据库静态密码，而是直接引用 KubeBlocks 自动生成的 Secret。

PostgreSQL 示例：

```yaml
postgresql:
  mode: kubeblocks
  kubeblocks:
    clusterName: synctv-pg
    replicas: 2
    serviceVersion: "18.1.0"
```

Redis 示例：

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

说明：

- PostgreSQL 默认连接到 `<cluster>-postgresql-postgresql:5432`
- PostgreSQL 默认引用 `<cluster>-postgresql-account-postgres`
- Redis 默认连接到 `<cluster>-redis-redis:6379`
- Redis 默认引用 `<cluster>-redis-account-default`
- PostgreSQL 和 Redis 的 Secret key 固定为 `username` / `password`
- KubeBlocks 模式默认使用生成的数据库账号；PostgreSQL 数据库名固定为 `postgres`
- KubeBlocks 生成的数据库服务也仅用于集群内访问；从集群外调试时同样只建议用 `kubectl port-forward`

## 配置模型

Chart 会生成配置文件并挂载到 `/config/synctv.yaml`，同时通过 `SYNCTV_` 环境变量注入敏感值和连接信息。

应用当前使用分离式数据库配置模型，而不是仅依赖单个 DSN：

| Section | Description |
|---------|-------------|
| `config.server` | API 监听地址、CORS、代理设置 |
| `config.management` | 管理端点配置 |
| `config.database` | 连接池参数，实际主机/端口/用户/密码由环境变量注入 |
| `config.redis` | Redis 基础配置，连接信息由环境变量注入 |
| `config.cluster` | 集群同步与发现参数 |
| `config.jwt` | Token 时长，密钥由 Secret 注入 |
| `config.bootstrap` | 初始化 root 用户 |
| `config.email` | SMTP 基础配置，凭据可由 Secret 注入 |
| `config.livestream` | RTMP/HLS/拉流超时与缓存 |
| `config.webrtc` | STUN/TURN/WebRTC 相关配置 |

## 验证部署

```bash
kubectl get pods -n synctv
kubectl get svc -n synctv
kubectl logs -n synctv -l app.kubernetes.io/name=synctv -f
```

## 升级

```bash
helm upgrade synctv ./helm/synctv \
  --namespace synctv \
  --values my-values.yaml
```

## 卸载

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
```

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
```

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
    namespace: monitoring
    labels:
      prometheus: kube-prometheus
```

If you want Kubernetes-native `TokenReview` + `SubjectAccessReview` auth instead:

- SyncTV validates the scraper's service account token with Kubernetes `TokenReview`
- SyncTV authorizes `/metrics` access with `SubjectAccessReview`
- You grant scrape access by listing allowed service accounts in the chart values

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

## Architecture

```
                    Ingress (HTTPS)
                  synctv.example.com
                         |
              +----------v-----------+
              |  SyncTV Deployment   |
              |  (3+ replicas, HPA)  |
              |                      |
              |  HTTP API:  8080     |
              |  gRPC:      8080     |
              |  RTMP:      1935     |
              |  STUN:      3478/udp |
              +----+----------+------+
                   |          |
           +-------+    +----+-----+
           |             |          |
      +----v-----+  +---v----+  (Cluster)
      |PostgreSQL|  | Redis  |  Node Discovery
      |(Internal)|  |(Internal) via Redis
      +----------+  +--------+
```

## Production Checklist

- [ ] Change all default secrets in `secrets` section
- [ ] Enable TLS for ingress (cert-manager)
- [ ] Set appropriate resource limits
- [ ] Enable autoscaling (HPA)
- [ ] Configure pod disruption budget
- [ ] Enable network policies
- [ ] Set up monitoring (Prometheus/Grafana)
- [ ] Configure backup for PostgreSQL
- [ ] Review connection limits for your scale

## Support

- GitHub: https://github.com/synctv-org/synctv
- Issues: https://github.com/synctv-org/synctv/issues

## License

MIT License - see LICENSE file for details.
