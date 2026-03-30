# SyncTV Helm Chart

A production-ready Helm chart for deploying SyncTV - a distributed video synchronization platform with real-time streaming capabilities (RTMP/HLS/WebRTC).

## Features

- **Single Binary**: HTTP API, gRPC, RTMP, STUN, and SFU all run in one process
- **Multi-Replica**: Native cluster support with automatic node discovery and load balancing via Redis
- **High Availability**: Pod Disruption Budgets, anti-affinity rules, startup/liveness/readiness probes
- **Security**: Network policies, pod security contexts, secrets management, read-only root filesystem
- **Observability**: Prometheus ServiceMonitor, structured JSON logging
- **Production-Ready**: Resource limits, HPA autoscaling, ingress with TLS

## Prerequisites

- Kubernetes 1.23+
- Helm 3.8+
- Ingress controller (nginx recommended)
- cert-manager (optional, for automatic TLS certificates)
- PostgreSQL 14+ (external or via Bitnami chart)
- Redis 7+ (external or via Bitnami chart)

## Installation

### 1. Install External Dependencies

#### PostgreSQL

```bash
helm repo add bitnami https://charts.bitnami.com/bitnami

helm install postgresql bitnami/postgresql \
  --namespace synctv \
  --create-namespace \
  --set auth.username=synctv \
  --set auth.password=your-secure-password \
  --set auth.database=synctv \
  --set primary.persistence.size=20Gi
```

#### Redis

```bash
helm install redis bitnami/redis \
  --namespace synctv \
  --set auth.password=your-redis-password \
  --set master.persistence.size=8Gi
```

### 2. Create Custom Values File

Create a `my-values.yaml` file:

```yaml
# Database connection (host/port/name used to compose the URL)
config:
  database:
    host: "postgresql.synctv.svc.cluster.local"
    port: 5432
    name: "synctv"
  redis:
    host: "redis-master.synctv.svc.cluster.local"
    port: 6379
  server:
    corsAllowedOrigins:
      - "https://synctv.yourdomain.com"

ingress:
  hosts:
    - host: synctv.yourdomain.com
      paths:
        - path: /
          pathType: Prefix
  tls:
    - secretName: synctv-tls
      hosts:
        - synctv.yourdomain.com

# Secrets (IMPORTANT: Change all values in production!)
secrets:
  database:
    username: "synctv"
    password: "your-secure-password"
  redis:
    password: "your-redis-password"
  jwt:
    secret: "your-256-bit-random-jwt-secret"
  cluster:
    grpcSecret: "your-cluster-secret"
  bootstrap:
    rootPassword: "your-root-password"
```

### 3. Install SyncTV

```bash
helm install synctv ./helm/synctv \
  --namespace synctv \
  --create-namespace \
  --values my-values.yaml
```

### 4. Verify Installation

```bash
# Check pods
kubectl get pods -n synctv

# Check services
kubectl get svc -n synctv

# Check ingress
kubectl get ingress -n synctv

# View logs
kubectl logs -n synctv -l app.kubernetes.io/name=synctv -f
```

## Configuration

### Application Config

All configuration fields match the Rust `Config` struct in `synctv-core/src/config.rs`. The config file is generated from the ConfigMap and mounted at `/config/config.yaml`. Secrets are injected as environment variables with the `SYNCTV_` prefix (e.g., `SYNCTV_DATABASE_URL`, `SYNCTV_JWT_SECRET`).

Key sections:

| Section | Description |
|---------|-------------|
| `config.server` | Unified API port, CORS, trusted proxies |
| `config.database` | Host, port, name (URL composed with secrets), pool settings |
| `config.redis` | Host, port (URL composed with secrets), pool settings |
| `config.jwt` | Token durations (secret via env var) |
| `config.livestream` | RTMP port, max streams, GOP cache, timeouts |
| `config.cluster` | Channel capacities for inter-node communication |
| `config.webrtc` | Mode (signaling_only/peer_to_peer/hybrid/sfu), STUN, SFU settings |
| `config.connectionLimits` | Per-user, per-room, total connection limits |
| `config.bootstrap` | Root user creation on first startup |
| `config.email` | SMTP settings (credentials via secrets) |
| `config.oauth2` | Redirect scheme (http/https) |

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

```yaml
metrics:
  enabled: true
  serviceMonitor:
    enabled: true
    namespace: monitoring
    interval: 30s
    labels:
      prometheus: kube-prometheus
```

```bash
# Port-forward to access metrics
kubectl port-forward -n synctv svc/synctv 8080:8080
curl http://localhost:8080/metrics
```

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
      |(External)|  |(Extern)|  via Redis
      +----------+  +--------+
```

## Production Checklist

- [ ] Change all default secrets in `secrets` section
- [ ] Configure external PostgreSQL and Redis
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
