#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MANIFEST_DIR="${SCRIPT_DIR}/../manifests"

echo "=== Configuring Vault Kubernetes auth for ESO ==="

# Enable Kubernetes auth method
kubectl exec -n vault vault-0 -- vault auth enable kubernetes 2>/dev/null || true

# Get the Kubernetes API server address accessible from within the cluster
# In Kind, the API server is accessible via the kubernetes.default.svc endpoint
kubectl exec -n vault vault-0 -- vault write auth/kubernetes/config \
  kubernetes_host="https://kubernetes.default.svc.cluster.local:443"

echo "=== Creating Vault policy for ESO (read + write for PushSecret) ==="
kubectl exec -i -n vault vault-0 -- vault policy write eso-readwrite - <<'POLICY'
path "secret/data/wash/*" {
  capabilities = ["create", "read", "update", "delete", "list"]
}
path "secret/metadata/wash/*" {
  capabilities = ["create", "read", "update", "delete", "list"]
}
POLICY

echo "=== Creating Vault role for ESO service account ==="
kubectl exec -n vault vault-0 -- vault write auth/kubernetes/role/eso-role \
  bound_service_account_names=external-secrets \
  bound_service_account_namespaces=external-secrets \
  policies=eso-readwrite \
  ttl=1h

echo "=== Applying ClusterSecretStore ==="
kubectl apply -f "${MANIFEST_DIR}/eso/cluster-secret-store.yaml"

echo "=== Waiting for ClusterSecretStore to be ready ==="
sleep 5
kubectl get clustersecretstore vault-backend

echo "=== Applying ExternalSecret ==="
kubectl apply -f "${MANIFEST_DIR}/eso/external-secret.yaml"

echo "=== Waiting for ExternalSecret to sync ==="
kubectl wait --for=condition=Ready \
  externalsecret/mcp-jwt-credentials \
  -n wasmcloud-system \
  --timeout=60s

echo "=== Applying PushSecret ==="
kubectl apply -f "${MANIFEST_DIR}/eso/push-secret.yaml"

echo "=== Checking PushSecret status ==="
sleep 10
echo "(PushSecret will show 'Errored' until the test client creates the writable secret — this is expected)"
kubectl get pushsecret -n wasmcloud-system

echo "=== Verifying K8s Secret was created by ESO ==="
echo "Secret keys:"
kubectl get secret mcp-jwt-credentials -n wasmcloud-system -o jsonpath='{.data}' | python3 -m json.tool 2>/dev/null || \
  kubectl get secret mcp-jwt-credentials -n wasmcloud-system -o yaml

echo ""
echo "JWT_ISSUER value:"
kubectl get secret mcp-jwt-credentials -n wasmcloud-system \
  -o jsonpath='{.data.JWT_ISSUER}' | base64 -d && echo

echo ""
echo "=== Vault + ESO + PushSecret configuration complete ==="
