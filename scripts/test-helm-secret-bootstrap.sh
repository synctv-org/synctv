#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

chart_dir="${SYNCTV_HELM_CHART_DIR:-helm/synctv}"
namespace="${SYNCTV_HELM_TEST_NAMESPACE:-synctv-secret-bootstrap-test}"
release="${SYNCTV_HELM_TEST_RELEASE:-synctv-bootstrap-test}"
secret_name="${release}-secrets"
stable_key="SYNCTV_JWT_SECRET"
managed_security_keys=(
  SYNCTV_SECURITY_CREDENTIAL_ENCRYPTION_KEY
  SYNCTV_SECURITY_TOTP_ENCRYPTION_KEY
  SYNCTV_SECURITY_EMAIL_OUTBOX_ENCRYPTION_KEY
  SYNCTV_SECURITY_OPAQUE_SERVER_SETUP_SECRET
  SYNCTV_SECURITY_PROXY_SIGNING_KEY
  SYNCTV_SECURITY_MEDIA_SWARM_SIGNING_KEY
  SYNCTV_SECURITY_PROVIDER_SESSION_ENCRYPTION_KEY
  SYNCTV_SECURITY_LOGIN_DISCOVERY_KEY
  SYNCTV_SECURITY_WEBAUTHN_ENUMERATION_KEY
  SYNCTV_FILE_UPLOAD_TOKEN_SECRET
)

cleanup() {
  helm uninstall "$release" --namespace "$namespace" >/dev/null 2>&1 || true
  kubectl delete namespace "$namespace" --wait=true --timeout=60s >/dev/null 2>&1 || true
}
trap cleanup EXIT

cleanup
kubectl create namespace "$namespace" >/dev/null

helm install "$release" "$chart_dir" \
  --namespace "$namespace" \
  --timeout 5m \
  --set postgresql.mode=external \
  --set postgresql.external.host=postgres.invalid \
  --set-string secrets.database.password=kind-test-database-password \
  --set redis.mode=external \
  --set redis.external.host=redis.invalid >/dev/null

stable_before="$(kubectl get secret "$secret_name" --namespace "$namespace" -o "jsonpath={.data.$stable_key}")"
test -n "$stable_before"
for key in "${managed_security_keys[@]}"; do
  kubectl patch secret "$secret_name" \
    --namespace "$namespace" \
    --type=json \
    -p="[{\"op\":\"remove\",\"path\":\"/data/$key\"}]" >/dev/null
done

helm upgrade "$release" "$chart_dir" \
  --namespace "$namespace" \
  --timeout 5m \
  --set postgresql.mode=external \
  --set postgresql.external.host=postgres.invalid \
  --set-string secrets.database.password=kind-test-database-password \
  --set redis.mode=external \
  --set redis.external.host=redis.invalid >/dev/null

stable_after="$(kubectl get secret "$secret_name" --namespace "$namespace" -o "jsonpath={.data.$stable_key}")"
test "$stable_before" = "$stable_after"
for key in "${managed_security_keys[@]}"; do
  reconciled="$(kubectl get secret "$secret_name" --namespace "$namespace" -o "jsonpath={.data.$key}")"
  test -n "$reconciled"
done

echo "Helm Secret bootstrap upgrade test passed."
