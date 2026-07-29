#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

chart_dir="${SYNCTV_HELM_CHART_DIR:-helm/synctv}"
namespace="${SYNCTV_HELM_NAMESPACE:-synctv}"
tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

fail() {
  echo "Helm validation failed: $*" >&2
  exit 1
}

assert_template_fails() {
  local case_name="$1"
  local message="$2"
  shift 2
  if helm template synctv "$chart_dir" \
    --namespace "$namespace" \
    "$@" \
    >"$tmp_dir/$case_name.yaml" 2>"$tmp_dir/$case_name.err"; then
    fail "$message"
  fi
}

assert_max_service_name_len() {
  local file="$1"
  local max_len="${2:-63}"
  ruby -ryaml -e '
    file = ARGV.fetch(0)
    max = Integer(ARGV.fetch(1))
    docs = YAML.load_stream(File.read(file)).compact
    bad = docs
      .select { |doc| doc["kind"] == "Service" }
      .map { |doc| doc.dig("metadata", "name").to_s }
      .select { |name| name.length > max }
    abort("rendered Service name(s) exceed #{max} characters in #{file}: #{bad.join(", ")}") unless bad.empty?
  ' "$file" "$max_len"
}

assert_pdb_field() {
  local file="$1"
  local field="$2"
  local expected="$3"
  ruby -ryaml -e '
    file, field, expected = ARGV
    docs = YAML.load_stream(File.read(file)).compact
    pdb = docs.find { |doc| doc["kind"] == "PodDisruptionBudget" }
    abort("PodDisruptionBudget was not rendered in #{file}") unless pdb
    actual = pdb.dig("spec", field)
    abort("PodDisruptionBudget #{field} expected #{expected.inspect}, got #{actual.inspect}") unless actual.to_s == expected
  ' "$file" "$field" "$expected"
}

assert_pdb_field_absent() {
  local file="$1"
  local field="$2"
  ruby -ryaml -e '
    file, field = ARGV
    docs = YAML.load_stream(File.read(file)).compact
    pdb = docs.find { |doc| doc["kind"] == "PodDisruptionBudget" }
    abort("PodDisruptionBudget was not rendered in #{file}") unless pdb
    abort("PodDisruptionBudget #{field} was rendered in #{file}") if pdb.fetch("spec", {}).key?(field)
  ' "$file" "$field"
}

assert_service() {
  local file="$1"
  local name="$2"
  local type="$3"
  ruby -ryaml -e '
    file, name, type = ARGV
    docs = YAML.load_stream(File.read(file)).compact
    service = docs.find { |doc| doc["kind"] == "Service" && doc.dig("metadata", "name") == name }
    abort("Service #{name.inspect} was not rendered in #{file}") unless service
    actual = service.dig("spec", "type")
    abort("Service #{name.inspect} type expected #{type.inspect}, got #{actual.inspect}") unless actual == type
  ' "$file" "$name" "$type"
}

assert_no_resource_named() {
  local file="$1"
  local name="$2"
  ruby -ryaml -e '
    file, name = ARGV
    docs = YAML.load_stream(File.read(file)).compact
    found = docs.any? { |doc| doc.dig("metadata", "name") == name }
    abort("Resource #{name.inspect} was rendered in #{file}") if found
  ' "$file" "$name"
}

assert_no_certificate_common_name() {
  local file="$1"
  ruby -ryaml -e '
    file = ARGV.fetch(0)
    docs = YAML.load_stream(File.read(file)).compact
    certificates = docs.select { |doc| doc["kind"] == "Certificate" }
    with_common_name = certificates.select { |doc| doc.fetch("spec", {}).key?("commonName") }
    abort("Certificate commonName was rendered in #{file}") unless with_common_name.empty?
  ' "$file"
}

assert_env_secret_key_ref() {
  local file="$1"
  local env_name="$2"
  local secret_key="$3"
  ruby -ryaml -e '
    file, env_name, secret_key = ARGV
    docs = YAML.load_stream(File.read(file)).compact
    containers = docs.flat_map do |doc|
      next [] unless ["Deployment", "StatefulSet"].include?(doc["kind"])
      doc.dig("spec", "template", "spec", "containers") || []
    end
    env = containers.flat_map { |container| container["env"] || [] }
    entry = env.find { |item| item["name"] == env_name }
    abort("env #{env_name.inspect} was not rendered in #{file}") unless entry
    actual = entry.dig("valueFrom", "secretKeyRef", "key")
    abort("env #{env_name.inspect} secret key expected #{secret_key.inspect}, got #{actual.inspect}") unless actual == secret_key
  ' "$file" "$env_name" "$secret_key"
}

assert_secret_string_data_key_absent() {
  local file="$1"
  local key="$2"
  ruby -ryaml -e '
    file, key = ARGV
    docs = YAML.load_stream(File.read(file)).compact
    found = docs.any? { |doc| doc["kind"] == "Secret" && doc.fetch("stringData", {}).key?(key) }
    abort("Secret stringData key #{key.inspect} was unexpectedly rendered in #{file}") if found
  ' "$file" "$key"
}

assert_env_secret_name() {
  local file="$1"
  local env_name="$2"
  local secret_name="$3"
  ruby -ryaml -e '
    file, env_name, secret_name = ARGV
    docs = YAML.load_stream(File.read(file)).compact
    containers = docs.flat_map do |doc|
      next [] unless ["Deployment", "StatefulSet"].include?(doc["kind"])
      doc.dig("spec", "template", "spec", "containers") || []
    end
    entry = containers.flat_map { |container| container["env"] || [] }
      .find { |item| item["name"] == env_name }
    abort("env #{env_name.inspect} was not rendered in #{file}") unless entry
    actual = entry.dig("valueFrom", "secretKeyRef", "name")
    abort("env #{env_name.inspect} Secret expected #{secret_name.inspect}, got #{actual.inspect}") unless actual == secret_name
  ' "$file" "$env_name" "$secret_name"
}

assert_bootstrap_env_value() {
  local file="$1"
  local env_name="$2"
  local expected="$3"
  ruby -ryaml -e '
    file, env_name, expected = ARGV
    docs = YAML.load_stream(File.read(file)).compact
    job = docs.find do |doc|
      doc["kind"] == "Job" && doc.dig("metadata", "labels", "app.kubernetes.io/component") == "secret-bootstrap"
    end
    abort("secret bootstrap Job was not rendered in #{file}") unless job
    containers = job.dig("spec", "template", "spec", "containers") || []
    container = containers.find { |item| item["name"] == "secret-bootstrap" }
    abort("secret bootstrap container was not rendered in #{file}") unless container
    entry = (container["env"] || []).find { |item| item["name"] == env_name }
    abort("bootstrap env #{env_name.inspect} was not rendered in #{file}") unless entry
    actual = entry["value"].to_s
    abort("bootstrap env #{env_name.inspect} expected #{expected.inspect}, got #{actual.inspect}") unless actual == expected
  ' "$file" "$env_name" "$expected"
}

assert_bootstrap_reconciles_key() {
  local file="$1"
  local key="$2"
  ruby -ryaml -e '
    file, key = ARGV
    docs = YAML.load_stream(File.read(file)).compact
    job = docs.find do |doc|
      doc["kind"] == "Job" && doc.dig("metadata", "labels", "app.kubernetes.io/component") == "secret-bootstrap"
    end
    abort("secret bootstrap Job was not rendered in #{file}") unless job
    containers = job.dig("spec", "template", "spec", "containers") || []
    script = containers.find { |item| item["name"] == "secret-bootstrap" }&.dig("args", 0).to_s
    abort("secret bootstrap does not reconcile #{key.inspect}") unless script.include?("ensure_key #{key} ")
  ' "$file" "$key"
}

assert_deployment_annotation() {
  local file="$1"
  local annotation="$2"
  local expected="$3"
  ruby -ryaml -e '
    file, annotation, expected = ARGV
    docs = YAML.load_stream(File.read(file)).compact
    deployment = docs.find { |doc| doc["kind"] == "Deployment" && doc.dig("metadata", "name") == "synctv" }
    abort("synctv Deployment was not rendered in #{file}") unless deployment
    actual = deployment.dig("spec", "template", "metadata", "annotations", annotation)
    abort("Deployment annotation #{annotation.inspect} expected #{expected.inspect}, got #{actual.inspect}") unless actual == expected
  ' "$file" "$annotation" "$expected"
}

assert_app_env_contract() {
  local file="$1"
  ruby -ryaml -e '
    file, source_root = ARGV
    docs = YAML.load_stream(File.read(file)).compact
    deployment = docs.find { |doc| doc["kind"] == "Deployment" && doc.dig("metadata", "name") == "synctv" }
    abort("synctv Deployment was not rendered in #{file}") unless deployment
    pod_spec = deployment.dig("spec", "template", "spec") || {}
    abort("synctv Deployment must set enableServiceLinks=false") unless pod_spec["enableServiceLinks"] == false
    container = Array(pod_spec["containers"]).find { |item| item["name"] == "synctv" }
    abort("synctv container was not rendered in #{file}") unless container
    rendered = Array(container["env"]).filter_map { |item| item["name"] if item["name"].to_s.start_with?("SYNCTV_") }
    supported = Dir.glob(File.join(source_root, "**", "*.rs")).flat_map do |path|
      File.read(path).scan(/"(SYNCTV_[A-Z0-9_]+)"/).flatten
    end.uniq
    unknown = rendered - supported
    abort("unsupported SYNCTV_ environment variable(s) rendered in #{file}: #{unknown.join(", ")}") unless unknown.empty?
  ' "$file" synctv/src
}

assert_security_rendering() {
  local file="$1"
  ruby -ryaml -e '
    file = ARGV.fetch(0)
    docs = YAML.load_stream(File.read(file)).compact

    containers = docs.flat_map do |doc|
      next [] unless ["Deployment", "StatefulSet"].include?(doc["kind"])
      doc.dig("spec", "template", "spec", "containers") || []
    end
    images = containers.map { |container| container["image"].to_s }

    abort("SyncTV image registry override was not applied") unless images.any? { |image| image.start_with?("registry.example.com/") && image.include?("/synctv:") }
    config = docs.find { |doc| doc["kind"] == "ConfigMap" && doc.dig("metadata", "name") == "synctv-config" }
    abort("synctv-config ConfigMap was not rendered") unless config
    synctv_yaml = config.dig("data", "synctv.yaml")
    abort("synctv.yaml ConfigMap entry was not rendered") unless synctv_yaml
    app_config = YAML.safe_load(synctv_yaml)
    ssrf = app_config.dig("security", "ssrf") || {}
    abort("SSRF private-network override was not applied") unless ssrf["allow_private_network_targets"] == true
    abort("SSRF allowed host was not applied") unless Array(ssrf["allowed_hosts"]).include?("nas.example.internal")
    abort("SSRF allowed IP range was not applied") unless Array(ssrf["allowed_ip_ranges"]).include?("10.0.8.0/24")
  ' "$file"
}

assert_native_webauthn_rendering() {
  local file="$1"
  ruby -ryaml -e '
    file = ARGV.fetch(0)
    docs = YAML.load_stream(File.read(file)).compact
    config = docs.find { |doc| doc["kind"] == "ConfigMap" && doc.dig("metadata", "name") == "synctv-config" }
    abort("synctv-config ConfigMap was not rendered in #{file}") unless config
    synctv_yaml = config.dig("data", "synctv.yaml")
    abort("synctv.yaml ConfigMap entry was not rendered in #{file}") unless synctv_yaml
    webauthn = YAML.safe_load(synctv_yaml).dig("webauthn") || {}

    expected_apple_ids = ["ABCDE12345.org.synctv.app"]
    abort("Apple app IDs were not rendered") unless webauthn["apple_app_ids"] == expected_apple_ids

    expected_android_apps = [{
      "package_name" => "org.synctv.app",
      "sha256_cert_fingerprints" => ["AA:BB:CC:DD"]
    }]
    abort("Android app identities were not rendered") unless webauthn["android_apps"] == expected_android_apps
  ' "$file"
}

assert_file_storage_s3_file_credentials_rendering() {
  local file="$1"
  ruby -ryaml -e '
    file = ARGV.fetch(0)
    docs = YAML.load_stream(File.read(file)).compact

    config = docs.find { |doc| doc["kind"] == "ConfigMap" && doc.dig("metadata", "name") == "synctv-config" }
    abort("synctv-config ConfigMap was not rendered in #{file}") unless config
    synctv_yaml = config.dig("data", "synctv.yaml")
    abort("synctv.yaml ConfigMap entry was not rendered in #{file}") unless synctv_yaml
    app_config = YAML.safe_load(synctv_yaml)
    s3 = app_config.dig("file_storage", "backends", "s3_public") || {}
    abort("S3 access_key_id_file was not rendered") unless s3["access_key_id_file"] == "/run/secrets/file-storage-s3/access_key_id"
    abort("S3 secret_access_key_file was not rendered") unless s3["secret_access_key_file"] == "/run/secrets/file-storage-s3/secret_access_key"
    abort("S3 inline access_key_id was rendered with file credentials") if s3.key?("access_key_id")
    abort("S3 inline secret_access_key was rendered with file credentials") if s3.key?("secret_access_key")

    deployment = docs.find { |doc| doc["kind"] == "Deployment" && doc.dig("metadata", "name") == "synctv" }
    abort("synctv Deployment was not rendered in #{file}") unless deployment
    volumes = deployment.dig("spec", "template", "spec", "volumes") || []
    volume = volumes.find { |item| item["name"] == "file-storage-s3" }
    abort("file-storage-s3 volume was not rendered") unless volume&.dig("secret", "secretName") == "synctv-file-storage-s3"

    containers = deployment.dig("spec", "template", "spec", "containers") || []
    synctv = containers.find { |item| item["name"] == "synctv" }
    abort("synctv container was not rendered") unless synctv
    mounts = synctv["volumeMounts"] || []
    mount = mounts.find { |item| item["name"] == "file-storage-s3" }
    abort("file-storage-s3 volumeMount was not rendered") unless mount
    abort("file-storage-s3 mountPath was #{mount["mountPath"].inspect}") unless mount["mountPath"] == "/run/secrets/file-storage-s3"
    abort("file-storage-s3 volumeMount should be readOnly") unless mount["readOnly"] == true
  ' "$file"
}

write_rendered_synctv_config() {
  local rendered_manifest="$1"
  local rendered_config="$2"
  local secret_dir="${3:-}"
  ruby -ryaml -e '
    file, output, secret_dir = ARGV
    docs = YAML.load_stream(File.read(file)).compact
    config = docs.find { |doc| doc["kind"] == "ConfigMap" && doc.dig("metadata", "name") == "synctv-config" }
    abort("synctv-config ConfigMap was not rendered in #{file}") unless config
    synctv_yaml = config.dig("data", "synctv.yaml")
    abort("synctv.yaml ConfigMap entry was not rendered in #{file}") unless synctv_yaml
    synctv_yaml = synctv_yaml.gsub("/run/secrets/file-storage-s3", secret_dir) unless secret_dir.empty?
    File.write(output, synctv_yaml)
  ' "$rendered_manifest" "$rendered_config" "$secret_dir"
}

run_rendered_synctv_config_validation() {
  local rendered_config="$1"
  local -a validation_env=(
    "PATH=${PATH:-/usr/bin:/bin}"
    "HOME=${HOME:-$tmp_dir}"
    "TMPDIR=${TMPDIR:-/tmp}"
    "SYNCTV_JWT_SECRET=helm-validation-jwt-secret-12345678901234567890"
    "SYNCTV_SECURITY_CREDENTIAL_ENCRYPTION_KEY=5656565656565656565656565656565656565656565656565656565656565656"
    "SYNCTV_SECURITY_TOTP_ENCRYPTION_KEY=5757575757575757575757575757575757575757575757575757575757575757"
    "SYNCTV_SECURITY_EMAIL_OUTBOX_ENCRYPTION_KEY=5858585858585858585858585858585858585858585858585858585858585858"
    "SYNCTV_SECURITY_OPAQUE_SERVER_SETUP_SECRET=helm-validation-opaque-secret-123456789012345"
    "SYNCTV_SECURITY_PROXY_SIGNING_KEY=helm-validation-proxy-signing-key-123456789012345"
    "SYNCTV_SECURITY_MEDIA_SWARM_SIGNING_KEY=helm-validation-media-swarm-signing-key-123456789012345"
    "SYNCTV_SECURITY_PROVIDER_SESSION_ENCRYPTION_KEY=helm-validation-provider-session-key-123456789012345"
    "SYNCTV_SECURITY_LOGIN_DISCOVERY_KEY=helm-validation-login-discovery-key-123456789012345"
    "SYNCTV_SECURITY_WEBAUTHN_ENUMERATION_KEY=helm-validation-webauthn-enumeration-key-123456789012345"
    "SYNCTV_FILE_UPLOAD_TOKEN_SECRET=helm-validation-file-upload-key-123456789012345"
    "SYNCTV_CLUSTER_SECRET=helm-validation-cluster-secret-12345678901234567890"
    "SYNCTV_SERVER_ADVERTISE_HOST=10.0.0.10"
    "SYNCTV_REDIS_HOST=synctv-redis"
    "SYNCTV_REDIS_PORT=6379"
    "SYNCTV_REDIS_DATABASE=0"
  )
  [ -z "${USER:-}" ] || validation_env+=("USER=$USER")
  [ -z "${CARGO_HOME:-}" ] || validation_env+=("CARGO_HOME=$CARGO_HOME")
  [ -z "${RUSTUP_HOME:-}" ] || validation_env+=("RUSTUP_HOME=$RUSTUP_HOME")
  [ -z "${CARGO_TARGET_DIR:-}" ] || validation_env+=("CARGO_TARGET_DIR=$CARGO_TARGET_DIR")
  env -i "${validation_env[@]}" \
    cargo run -q -p synctv --bin synctv -- --no-dotenv --config "$rendered_config" config validate --strict
}

validate_rendered_synctv_config() {
  local rendered_manifest="$1"
  local rendered_config
  rendered_config="$tmp_dir/$(basename "$rendered_manifest" .yaml).synctv.yaml"
  write_rendered_synctv_config "$rendered_manifest" "$rendered_config"
  run_rendered_synctv_config_validation "$rendered_config"
}

validate_rendered_synctv_config_with_file_storage_s3_secret_files() {
  local rendered_manifest="$1"
  local secret_dir="$tmp_dir/file-storage-s3"
  mkdir -p "$secret_dir"
  printf '%s\n' "file-storage-access-key" >"$secret_dir/access_key_id"
  printf '%s\n' "file-storage-secret-key" >"$secret_dir/secret_access_key"

  local rendered_config
  rendered_config="$tmp_dir/$(basename "$rendered_manifest" .yaml).synctv.yaml"
  write_rendered_synctv_config "$rendered_manifest" "$rendered_config" "$secret_dir"
  run_rendered_synctv_config_validation "$rendered_config"
}

chart_version="$(ruby -ryaml -e 'puts YAML.load_file(ARGV.fetch(0)).fetch("version")' "$chart_dir/Chart.yaml")"
app_version="$(ruby -ryaml -e 'puts YAML.load_file(ARGV.fetch(0)).fetch("appVersion")' "$chart_dir/Chart.yaml")"
cargo_version="$(cargo metadata --format-version 1 --no-deps | node -e 'const fs = require("fs"); const meta = JSON.parse(fs.readFileSync(0, "utf8")); process.stdout.write(meta.workspace_default_members.length ? meta.packages.find((pkg) => pkg.id === meta.workspace_default_members[0]).version : meta.packages[0].version);')"
compose_image_tag="$(ruby -ryaml -e '
  compose = YAML.load_file(ARGV.fetch(0))
  image = compose.fetch("services").fetch("synctv").fetch("image")
  match = image.match(/\$\{SYNCTV_IMAGE_TAG:-([^}]+)\}/)
  abort("docker-compose.yml synctv image must use SYNCTV_IMAGE_TAG fallback") unless match
  puts match[1]
' docker-compose.yml)"
docs_default_app_version="$(node --input-type=module -e 'const project = await import("./docs/src/lib/project.ts"); process.stdout.write(project.dockerImageTag);')"

[ -n "$chart_version" ] || fail "$chart_dir/Chart.yaml must define version"
[ -n "$app_version" ] || fail "$chart_dir/Chart.yaml must define appVersion"
[ -n "$cargo_version" ] || fail "Cargo.toml must define workspace.package.version"
[ -n "$compose_image_tag" ] || fail "docker-compose.yml must define SYNCTV_IMAGE_TAG fallback"
[ -n "$docs_default_app_version" ] || fail "docs/src/lib/project.ts must define defaultAppVersion"

[ "$chart_version" = "$cargo_version" ] ||
  fail "chart version ($chart_version) must match Cargo workspace version ($cargo_version)"
[ "$app_version" = "$cargo_version" ] ||
  fail "chart appVersion ($app_version) must match Cargo workspace version ($cargo_version)"
[ "$compose_image_tag" = "$cargo_version" ] ||
  fail "Compose image fallback tag ($compose_image_tag) must match Cargo workspace version ($cargo_version)"
[ "$docs_default_app_version" = "$cargo_version" ] ||
  fail "docs default app version ($docs_default_app_version) must match Cargo workspace version ($cargo_version)"

helm lint "$chart_dir"

helm template synctv "$chart_dir" \
  --namespace "$namespace" \
  >"$tmp_dir/default.yaml"

helm template synctv "$chart_dir" \
  --namespace "$namespace" \
  >"$tmp_dir/default-repeat.yaml"
cmp -s "$tmp_dir/default.yaml" "$tmp_dir/default-repeat.yaml" ||
  fail "default Helm rendering must be deterministic"

helm template synctv "$chart_dir" \
  --namespace "$namespace" \
  --set existingSecret=synctv-managed-secrets \
  --set secretRolloutChecksum=rotation-2 \
  >"$tmp_dir/existing-secret.yaml"

helm template synctv "$chart_dir" \
  --namespace "$namespace" \
  --set secrets.security.emailOutboxEncryptionKey=5959595959595959595959595959595959595959595959595959595959595959 \
  >"$tmp_dir/explicit-email-outbox-key.yaml"

assert_template_fails invalid-email-outbox-key \
  "invalid secrets.security.emailOutboxEncryptionKey must fail validation" \
  --set-string secrets.security.emailOutboxEncryptionKey=invalid
assert_template_fails invalid-credential-key \
  "invalid secrets.security.credentialEncryptionKey must fail validation" \
  --set-string secrets.security.credentialEncryptionKey=invalid
assert_template_fails short-opaque-secret \
  "short secrets.security.opaqueServerSetupSecret must fail validation" \
  --set-string secrets.security.opaqueServerSetupSecret=short
assert_template_fails placeholder-proxy-key \
  "placeholder secrets.security.proxySigningKey must fail validation" \
  --set-string secrets.security.proxySigningKey=CHANGE_ME_proxy_signing_key_1234567890
assert_template_fails duplicate-security-secret \
  "duplicate security-domain secret values must fail validation" \
  --set-string secrets.jwt.secret=duplicate-security-secret-value-1234567890 \
  --set-string secrets.security.proxySigningKey=duplicate-security-secret-value-1234567890
assert_template_fails duplicate-hex-security-secret \
  "hexadecimal security-domain secret values must be compared case-insensitively" \
  --set-string secrets.security.credentialEncryptionKey=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA \
  --set-string secrets.security.totpEncryptionKey=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa

helm template synctv "$chart_dir" \
  --namespace "$namespace" \
  --set existingSecret=synctv-managed-secrets \
  --set-string secrets.security.credentialEncryptionKey=ignored-external-value \
  >"$tmp_dir/existing-secret-ignores-values.yaml"

helm template synctv "$chart_dir" \
  --namespace "$namespace" \
  --set ingress.grpc.enabled=true \
  >"$tmp_dir/grpc.yaml"

helm template synctv "$chart_dir" \
  --namespace "$namespace" \
  --set postgresql.mode=kubeblocks \
  --set redis.mode=kubeblocks \
  >"$tmp_dir/kubeblocks.yaml"

helm template synctv "$chart_dir" \
  --namespace "$namespace" \
  --set postgresql.mode=kubeblocks \
  --set postgresql.kubeblocks.bootstrapAppDatabase=false \
  >"$tmp_dir/kubeblocks-no-bootstrap.yaml"

helm template aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa "$chart_dir" \
  --namespace "$namespace" \
  --set metrics.enabled=true \
  --set metrics.tls.enabled=true \
  --set metrics.serviceMonitor.enabled=true \
  --set metrics.vmServiceScrape.enabled=true \
  --set ingress.grpc.enabled=true \
  --set headlessService.enabled=true \
  >"$tmp_dir/long-release.yaml"

helm template synctv "$chart_dir" \
  --namespace "$namespace" \
  --set image.registry=registry.example.com \
  --set config.security.ssrf.allowPrivateNetworkTargets=true \
  --set config.security.ssrf.allowedHosts[0]=nas.example.internal \
  --set config.security.ssrf.allowedIpRanges[0]=10.0.8.0/24 \
  >"$tmp_dir/security.yaml"

helm template synctv "$chart_dir" \
  --namespace "$namespace" \
  --set-string config.webauthn.appleAppIds[0]=ABCDE12345.org.synctv.app \
  --set-string config.webauthn.androidApps[0].packageName=org.synctv.app \
  --set-string config.webauthn.androidApps[0].sha256CertFingerprints[0]=AA:BB:CC:DD \
  >"$tmp_dir/native-webauthn.yaml"

helm template synctv "$chart_dir" \
  --namespace "$namespace" \
  --set secrets.database.readUrl=postgresql://reader:secret@postgres-read:5432/synctv \
  --set config.database.useSecretReadUrl=true \
  >"$tmp_dir/secret-read-url.yaml"

helm template synctv "$chart_dir" \
  --namespace "$namespace" \
  --set config.fileStorage.defaultBackend=s3_public \
  --set config.fileStorage.backends.s3_public.type=s3 \
  --set config.fileStorage.backends.s3_public.endpoint=https://s3.example.com \
  --set config.fileStorage.backends.s3_public.bucket=synctv-files \
  --set config.fileStorage.backends.s3_public.region=auto \
  --set config.fileStorage.backends.s3_public.basePath=files/ \
  --set config.fileStorage.backends.s3_public.publicBaseUrl=https://cdn.example.com/files \
  --set config.fileStorage.backends.s3_public.accessKeyIdFile=/run/secrets/file-storage-s3/access_key_id \
  --set config.fileStorage.backends.s3_public.secretAccessKeyFile=/run/secrets/file-storage-s3/secret_access_key \
  --set extraVolumes[0].name=file-storage-s3 \
  --set extraVolumes[0].secret.secretName=synctv-file-storage-s3 \
  --set extraVolumeMounts[0].name=file-storage-s3 \
  --set extraVolumeMounts[0].mountPath=/run/secrets/file-storage-s3 \
  --set extraVolumeMounts[0].readOnly=true \
  >"$tmp_dir/file-storage-s3-files.yaml"

if helm template synctv "$chart_dir" \
  --namespace "$namespace" \
  --set replicaCount=2 \
  >"$tmp_dir/standalone-replicas.yaml" 2>"$tmp_dir/standalone-replicas.err"; then
  fail "replicaCount=2 without cluster mode must fail validation"
fi

if helm template synctv "$chart_dir" \
  --namespace "$namespace" \
  --set autoscaling.enabled=true \
  >"$tmp_dir/standalone-hpa.yaml" 2>"$tmp_dir/standalone-hpa.err"; then
  fail "autoscaling beyond one pod without cluster mode must fail validation"
fi

if helm template synctv "$chart_dir" \
  --namespace "$namespace" \
  --set config.database.useSecretReadUrl=true \
  >"$tmp_dir/missing-secret-read-url.yaml" 2>"$tmp_dir/missing-secret-read-url.err"; then
  fail "config.database.useSecretReadUrl=true without existingSecret or secrets.database.readUrl must fail validation"
fi

if helm template synctv "$chart_dir" \
  --namespace "$namespace" \
  --set secretBootstrap.enabled=false \
  >"$tmp_dir/missing-secret-owner.yaml" 2>"$tmp_dir/missing-secret-owner.err"; then
  fail "secretBootstrap.enabled=false without existingSecret must fail validation"
fi

helm template synctv "$chart_dir" \
  --namespace "$namespace" \
  --set replicaCount=2 \
  --set config.cluster.enabled=true \
  >"$tmp_dir/cluster-replicas.yaml"

helm template synctv "$chart_dir" \
  --namespace "$namespace" \
  --set replicaCount=2 \
  --set safety.allowStandaloneReplicas=true \
  >"$tmp_dir/acknowledged-standalone-replicas.yaml"

helm template synctv "$chart_dir" \
  --namespace "$namespace" \
  --set podDisruptionBudget.enabled=true \
  >"$tmp_dir/pdb-default.yaml"
assert_pdb_field "$tmp_dir/pdb-default.yaml" maxUnavailable 1

helm template synctv "$chart_dir" \
  --namespace "$namespace" \
  --set podDisruptionBudget.enabled=true \
  --set podDisruptionBudget.minAvailable=2 \
  >"$tmp_dir/pdb-legacy-min-available.yaml"
assert_pdb_field "$tmp_dir/pdb-legacy-min-available.yaml" minAvailable 2
assert_pdb_field_absent "$tmp_dir/pdb-legacy-min-available.yaml" maxUnavailable

if helm template synctv "$chart_dir" \
  --namespace "$namespace" \
  --set stunService.enabled=true \
  --set config.webrtc.stunExternalAddr=203.0.113.10:3478 \
  >"$tmp_dir/clusterip-stun.yaml" 2>"$tmp_dir/clusterip-stun.err"; then
  fail "ClusterIP STUN service with external STUN address must fail validation"
fi

helm template synctv "$chart_dir" \
  --namespace "$namespace" \
  --set stunService.enabled=true \
  --set stunService.type=LoadBalancer \
  --set config.webrtc.stunExternalAddr=203.0.113.10:3478 \
  >"$tmp_dir/loadbalancer-stun.yaml"
assert_service "$tmp_dir/loadbalancer-stun.yaml" synctv-stun LoadBalancer

assert_security_rendering "$tmp_dir/security.yaml"
assert_native_webauthn_rendering "$tmp_dir/native-webauthn.yaml"
for key in \
  SYNCTV_SECURITY_CREDENTIAL_ENCRYPTION_KEY \
  SYNCTV_SECURITY_TOTP_ENCRYPTION_KEY \
  SYNCTV_SECURITY_EMAIL_OUTBOX_ENCRYPTION_KEY \
  SYNCTV_SECURITY_OPAQUE_SERVER_SETUP_SECRET \
  SYNCTV_SECURITY_PROXY_SIGNING_KEY \
  SYNCTV_SECURITY_MEDIA_SWARM_SIGNING_KEY \
  SYNCTV_SECURITY_PROVIDER_SESSION_ENCRYPTION_KEY \
  SYNCTV_SECURITY_LOGIN_DISCOVERY_KEY \
  SYNCTV_SECURITY_WEBAUTHN_ENUMERATION_KEY \
  SYNCTV_FILE_UPLOAD_TOKEN_SECRET; do
  assert_env_secret_key_ref "$tmp_dir/default.yaml" "$key" "$key"
  assert_bootstrap_reconciles_key "$tmp_dir/default.yaml" "$key"
  assert_env_secret_key_ref "$tmp_dir/existing-secret.yaml" "$key" "$key"
  assert_env_secret_name "$tmp_dir/existing-secret.yaml" "$key" synctv-managed-secrets
  assert_secret_string_data_key_absent "$tmp_dir/existing-secret.yaml" "$key"
done
assert_no_resource_named "$tmp_dir/existing-secret.yaml" synctv-secret-bootstrap
assert_deployment_annotation "$tmp_dir/existing-secret.yaml" synctv.io/secret-rollout-checksum rotation-2
assert_bootstrap_env_value "$tmp_dir/explicit-email-outbox-key.yaml" EMAIL_OUTBOX_KEY 5959595959595959595959595959595959595959595959595959595959595959
assert_file_storage_s3_file_credentials_rendering "$tmp_dir/file-storage-s3-files.yaml"
validate_rendered_synctv_config "$tmp_dir/default.yaml"
assert_app_env_contract "$tmp_dir/default.yaml"
validate_rendered_synctv_config "$tmp_dir/security.yaml"
validate_rendered_synctv_config "$tmp_dir/cluster-replicas.yaml"
validate_rendered_synctv_config_with_file_storage_s3_secret_files "$tmp_dir/file-storage-s3-files.yaml"
assert_env_secret_key_ref "$tmp_dir/secret-read-url.yaml" SYNCTV_DATABASE_READ_URL SYNCTV_DATABASE_READ_URL
assert_bootstrap_env_value "$tmp_dir/secret-read-url.yaml" DATABASE_READ_URL postgresql://reader:secret@postgres-read:5432/synctv
assert_env_secret_key_ref "$tmp_dir/kubeblocks.yaml" SYNCTV_DATABASE_PASSWORD SYNCTV_DATABASE_PASSWORD
assert_no_resource_named "$tmp_dir/kubeblocks-no-bootstrap.yaml" bootstrap-postgresql-app-db
assert_max_service_name_len "$tmp_dir/long-release.yaml" 63
assert_no_certificate_common_name "$tmp_dir/long-release.yaml"

echo "Helm chart validation passed."
