{{/*
Expand the name of the chart.
*/}}
{{- define "synctv.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Create a default fully qualified app name.
*/}}
{{- define "synctv.fullname" -}}
{{- if .Values.fullnameOverride }}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- $name := default .Chart.Name .Values.nameOverride }}
{{- if contains $name .Release.Name }}
{{- .Release.Name | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- printf "%s-%s" .Release.Name $name | trunc 63 | trimSuffix "-" }}
{{- end }}
{{- end }}
{{- end }}

{{/*
Create chart name and version as used by the chart label.
*/}}
{{- define "synctv.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Append a DNS-label suffix while preserving the suffix under the 63-character
Kubernetes Service-name limit.
*/}}
{{- define "synctv.nameWithSuffix" -}}
{{- $base := index . 0 -}}
{{- $suffix := index . 1 -}}
{{- $maxBaseLen := int (sub 62 (len $suffix)) -}}
{{- printf "%s-%s" ($base | trunc $maxBaseLen | trimSuffix "-") $suffix | trunc 63 | trimSuffix "-" -}}
{{- end }}

{{/*
Common labels
*/}}
{{- define "synctv.labels" -}}
helm.sh/chart: {{ include "synctv.chart" . }}
{{ include "synctv.selectorLabels" . }}
{{- if .Chart.AppVersion }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
{{- end }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end }}

{{/*
Selector labels
*/}}
{{- define "synctv.selectorLabels" -}}
app.kubernetes.io/name: {{ include "synctv.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{/*
Selector labels for SyncTV application pods only.
*/}}
{{- define "synctv.appSelectorLabels" -}}
{{ include "synctv.selectorLabels" . }}
app.kubernetes.io/component: app
{{- end }}

{{/*
Create the name of the service account to use
*/}}
{{- define "synctv.serviceAccountName" -}}
{{- if .Values.serviceAccount.create }}
{{- default (include "synctv.fullname" .) .Values.serviceAccount.name }}
{{- else }}
{{- default "default" .Values.serviceAccount.name }}
{{- end }}
{{- end }}

{{/*
Return the synctv image name
*/}}
{{- define "synctv.image" -}}
{{- $registry := .Values.image.registry | default "docker.io" -}}
{{- $repository := .Values.image.repository -}}
{{- $tag := .Values.image.tag | default .Chart.AppVersion -}}
{{- include "synctv.imageReference" (list $registry $repository $tag) -}}
{{- end }}

{{/*
Return an image reference, preserving repositories that already include a registry host.
*/}}
{{- define "synctv.imageReference" -}}
{{- $registry := index . 0 -}}
{{- $repository := index . 1 -}}
{{- $tag := index . 2 -}}
{{- $firstComponent := first (splitList "/" $repository) -}}
{{- $hasRegistry := or (contains "." $firstComponent) (contains ":" $firstComponent) (eq $firstComponent "localhost") -}}
{{- if $hasRegistry -}}
{{- printf "%s:%s" $repository $tag -}}
{{- else -}}
{{- printf "%s/%s:%s" $registry $repository $tag -}}
{{- end -}}
{{- end }}

{{/*
Return a chart-managed backing service image name.
*/}}
{{- define "synctv.managedImage" -}}
{{- $root := index . 0 -}}
{{- $image := index . 1 -}}
{{- $defaultRepository := index . 2 -}}
{{- $defaultTag := index . 3 -}}
{{- $registry := "docker.io" -}}
{{- $repository := $image.repository | default $defaultRepository -}}
{{- $tag := $image.tag | default $defaultTag -}}
{{- include "synctv.imageReference" (list $registry $repository $tag) -}}
{{- end }}

{{/*
Return the secret name
*/}}
{{- define "synctv.secretName" -}}
{{- if .Values.existingSecret }}
{{- .Values.existingSecret }}
{{- else }}
{{- printf "%s-secrets" (include "synctv.fullname" .) }}
{{- end }}
{{- end }}

{{/*
Return the ConfigMap name
*/}}
{{- define "synctv.configMapName" -}}
{{- if .Values.existingConfigMap }}
{{- .Values.existingConfigMap }}
{{- else }}
{{- printf "%s-config" (include "synctv.fullname" .) }}
{{- end }}
{{- end }}

{{/*
Return the gRPC service name
*/}}
{{- define "synctv.grpcServiceName" -}}
{{- include "synctv.nameWithSuffix" (list (include "synctv.fullname" .) "grpc") }}
{{- end }}

{{/*
Return the RTMP service name
*/}}
{{- define "synctv.rtmpServiceName" -}}
{{- include "synctv.nameWithSuffix" (list (include "synctv.fullname" .) "rtmp") }}
{{- end }}

{{/*
Return the STUN service name
*/}}
{{- define "synctv.stunServiceName" -}}
{{- include "synctv.nameWithSuffix" (list (include "synctv.fullname" .) "stun") }}
{{- end }}

{{/*
Return the headless service name
*/}}
{{- define "synctv.headlessServiceName" -}}
{{- include "synctv.nameWithSuffix" (list (include "synctv.fullname" .) "headless") }}
{{- end }}

{{/*
Return the metrics service name
*/}}
{{- define "synctv.metricsServiceName" -}}
{{- include "synctv.nameWithSuffix" (list (include "synctv.fullname" .) "metrics") }}
{{- end }}

{{/*
Return the metrics TLS secret name
*/}}
{{- define "synctv.metricsTlsSecretName" -}}
{{- printf "%s-metrics-tls" (include "synctv.fullname" .) }}
{{- end }}

{{/*
Return the metrics TLS self-signed issuer name
*/}}
{{- define "synctv.metricsTlsIssuerName" -}}
{{- printf "%s-metrics-selfsigned" (include "synctv.fullname" .) }}
{{- end }}

{{/*
Return the metrics TLS server name used by scrape clients
*/}}
{{- define "synctv.metricsTlsServerName" -}}
{{- printf "%s.%s.svc.cluster.local" (include "synctv.metricsServiceName" .) .Release.Namespace }}
{{- end }}

{{/*
PostgreSQL deployment mode
*/}}
{{- define "synctv.postgresql.mode" -}}
{{- $mode := .Values.postgresql.mode | default "standard" -}}
{{- if not (has $mode (list "standard" "kubeblocks" "external")) -}}
{{- fail "postgresql.mode must be one of: standard, kubeblocks, external" -}}
{{- end -}}
{{- $mode -}}
{{- end }}

{{/*
Redis deployment mode
*/}}
{{- define "synctv.redis.mode" -}}
{{- $mode := .Values.redis.mode | default "standard" -}}
{{- if not (has $mode (list "standard" "kubeblocks" "external")) -}}
{{- fail "redis.mode must be one of: standard, kubeblocks, external" -}}
{{- end -}}
{{- $mode -}}
{{- end }}

{{/*
Managed PostgreSQL service/statefulset name
*/}}
{{- define "synctv.postgresql.fullname" -}}
{{- include "synctv.nameWithSuffix" (list (include "synctv.fullname" .) "postgresql") -}}
{{- end }}

{{/*
Managed Redis service/statefulset name
*/}}
{{- define "synctv.redis.fullname" -}}
{{- include "synctv.nameWithSuffix" (list (include "synctv.fullname" .) "redis") -}}
{{- end }}

{{/*
KubeBlocks PostgreSQL cluster name
*/}}
{{- define "synctv.postgresql.kubeblocks.clusterName" -}}
{{- .Values.postgresql.kubeblocks.clusterName | default (printf "%s-pg" (include "synctv.fullname" .)) -}}
{{- end }}

{{/*
KubeBlocks PostgreSQL secondary read service name managed by this chart
*/}}
{{- define "synctv.postgresql.kubeblocks.readServiceName" -}}
{{- include "synctv.nameWithSuffix" (list (include "synctv.postgresql.kubeblocks.clusterName" .) "postgresql-read") -}}
{{- end }}

{{/*
Whether the chart should expose KubeBlocks secondaries as a read pool endpoint.
*/}}
{{- define "synctv.postgresql.hasAutoReadReplica" -}}
{{- $replicas := int (.Values.postgresql.kubeblocks.replicas | default 2) -}}
{{- $hasSecretReadUrl := or (and .Values.existingSecret .Values.config.database.useSecretReadUrl) (and (not .Values.existingSecret) .Values.secrets.database.readUrl) -}}
{{- if and (eq (include "synctv.postgresql.mode" .) "kubeblocks") (gt $replicas 1) (not $hasSecretReadUrl) -}}
true
{{- end -}}
{{- end }}

{{/*
Whether the application container should read SYNCTV_DATABASE_READ_URL from a Secret.
Chart-managed Secrets can only be referenced when secrets.database.readUrl renders the key.
External Secrets are trusted to provide the key when useSecretReadUrl=true.
*/}}
{{- define "synctv.database.useSecretReadUrlEnv" -}}
{{- if .Values.existingSecret -}}
  {{- if .Values.config.database.useSecretReadUrl -}}
true
  {{- end -}}
{{- else if .Values.secrets.database.readUrl -}}
true
{{- else if .Values.config.database.useSecretReadUrl -}}
{{- fail "config.database.useSecretReadUrl=true requires existingSecret with SYNCTV_DATABASE_READ_URL or secrets.database.readUrl for the chart-managed Secret" -}}
{{- end -}}
{{- end }}

{{/*
PostgreSQL read connection host for SyncTV.
*/}}
{{- define "synctv.postgresql.readHost" -}}
{{- if include "synctv.postgresql.hasAutoReadReplica" . -}}
{{- include "synctv.postgresql.kubeblocks.readServiceName" . -}}
{{- end -}}
{{- end }}

{{/*
KubeBlocks Redis cluster name
*/}}
{{- define "synctv.redis.kubeblocks.clusterName" -}}
{{- .Values.redis.kubeblocks.clusterName | default (printf "%s-redis" (include "synctv.fullname" .)) -}}
{{- end }}

{{/*
PostgreSQL connection host for SyncTV
*/}}
{{- define "synctv.postgresql.host" -}}
{{- if eq (include "synctv.postgresql.mode" .) "kubeblocks" -}}
{{- printf "%s-postgresql-postgresql" (include "synctv.postgresql.kubeblocks.clusterName" .) -}}
{{- else if eq (include "synctv.postgresql.mode" .) "external" -}}
{{- required "postgresql.external.host is required when postgresql.mode=external" .Values.postgresql.external.host -}}
{{- else -}}
{{ include "synctv.postgresql.fullname" . }}
{{- end -}}
{{- end }}

{{/*
PostgreSQL connection port for SyncTV
*/}}
{{- define "synctv.postgresql.port" -}}
{{- if eq (include "synctv.postgresql.mode" .) "kubeblocks" -}}
5432
{{- else if eq (include "synctv.postgresql.mode" .) "external" -}}
{{- .Values.postgresql.external.port | default 5432 -}}
{{- else -}}
{{- .Values.postgresql.standard.service.port | default 5432 -}}
{{- end -}}
{{- end }}

{{/*
Redis connection host for SyncTV
*/}}
{{- define "synctv.redis.host" -}}
{{- if eq (include "synctv.redis.mode" .) "kubeblocks" -}}
{{- printf "%s-redis-redis" (include "synctv.redis.kubeblocks.clusterName" .) -}}
{{- else if eq (include "synctv.redis.mode" .) "external" -}}
{{- required "redis.external.host is required when redis.mode=external" .Values.redis.external.host -}}
{{- else -}}
{{ include "synctv.redis.fullname" . }}
{{- end -}}
{{- end }}

{{/*
Redis connection port for SyncTV
*/}}
{{- define "synctv.redis.port" -}}
{{- if eq (include "synctv.redis.mode" .) "kubeblocks" -}}
6379
{{- else if eq (include "synctv.redis.mode" .) "external" -}}
{{- .Values.redis.external.port | default 6379 -}}
{{- else -}}
{{- .Values.redis.standard.service.port | default 6379 -}}
{{- end -}}
{{- end }}

{{/*
PostgreSQL app username for SyncTV
*/}}
{{- define "synctv.postgresql.appUsername" -}}
{{- if eq (include "synctv.postgresql.mode" .) "kubeblocks" -}}
{{- .Values.postgresql.kubeblocks.appUsername | default "synctv" -}}
{{- else if eq (include "synctv.postgresql.mode" .) "external" -}}
{{- .Values.postgresql.external.username | default "synctv" -}}
{{- else -}}
{{- .Values.postgresql.standard.auth.username | default "synctv" -}}
{{- end -}}
{{- end }}

{{/*
PostgreSQL app database name for SyncTV
*/}}
{{- define "synctv.postgresql.database" -}}
{{- if eq (include "synctv.postgresql.mode" .) "kubeblocks" -}}
{{- .Values.postgresql.kubeblocks.database | default "synctv" -}}
{{- else if eq (include "synctv.postgresql.mode" .) "external" -}}
{{- .Values.postgresql.external.database | default "synctv" -}}
{{- else -}}
{{- .Values.postgresql.standard.auth.database | default "synctv" -}}
{{- end -}}
{{- end }}

{{/*
Redis username for SyncTV when statically configured
*/}}
{{- define "synctv.redis.username" -}}
{{- if eq (include "synctv.redis.mode" .) "external" -}}
{{- .Values.redis.external.username | default "" -}}
{{- else -}}
{{- .Values.redis.standard.auth.username | default "" -}}
{{- end -}}
{{- end }}

{{/*
Redis logical database index for SyncTV
*/}}
{{- define "synctv.redis.database" -}}
{{- if eq (include "synctv.redis.mode" .) "external" -}}
{{- .Values.redis.external.database | default 0 -}}
{{- else -}}
{{- .Values.redis.standard.auth.database | default 0 -}}
{{- end -}}
{{- end }}

{{/*
Secret containing KubeBlocks PostgreSQL superuser credentials
*/}}
{{- define "synctv.postgresql.kubeblocks.superuserSecretName" -}}
{{- printf "%s-postgresql-account-postgres" (include "synctv.postgresql.kubeblocks.clusterName" .) -}}
{{- end }}

{{/*
KubeBlocks Redis generated credential secret name
*/}}
{{- define "synctv.redis.kubeblocks.secretName" -}}
{{- printf "%s-redis-account-default" (include "synctv.redis.kubeblocks.clusterName" .) -}}
{{- end }}
