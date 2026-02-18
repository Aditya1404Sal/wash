#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MANIFEST_DIR="${SCRIPT_DIR}/../manifests"

echo "=== Installing HashiCorp Vault ==="

# Install Helm if not present
if ! command -v helm &>/dev/null; then
  echo "Helm not found, installing..."
  curl -fsSL https://raw.githubusercontent.com/helm/helm/main/scripts/get-helm-3 | bash
fi

helm repo add hashicorp https://helm.releases.hashicorp.com
helm repo update hashicorp

kubectl create namespace vault --dry-run=client -o yaml | kubectl apply -f -

helm upgrade --install vault hashicorp/vault \
  --namespace vault \
  --values "${MANIFEST_DIR}/vault/vault-values.yaml" \
  --wait --timeout 300s

echo "=== Waiting for Vault pod to be ready ==="
until kubectl get pod vault-0 -n vault &>/dev/null; do sleep 2; done
kubectl wait --for=condition=Ready pod/vault-0 -n vault --timeout=120s

echo "=== Enabling KV v2 secrets engine ==="
# This may already be enabled in dev mode, ignore errors
kubectl exec -n vault vault-0 -- vault secrets enable -path=secret kv-v2 2>/dev/null || true

echo "=== Seeding test secret (mcp-jwt-credentials-rotated) ==="
kubectl exec -n vault vault-0 -- vault kv put secret/wash/mcp-jwt-credentials-rotated \
  JWT_PUBLIC_KEY="-----BEGIN PUBLIC KEY-----
MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAuEASoTMPw0IuxZjbNqDM
IR+kCPRCQES2R8j75JooAIFieAehDWoRH37BIk/xVtSzeDfdWwySbq7BIwa/K3JW
wMB8DLdLA1YGjxaTIeX+vv0gwp9l8+ZKWEt8aW31GRuLHHLbhEuvs+EheRw20cT8
Cxc1MlLm91hrXlXNNpxBO/uO/7oYUndtykDRmHw2CDuyxpKu2gQwYo81jG5MkMw/
5adXkUGuqX/X7tN4IyHm/uZIs1R9Zeb8+moMsPlz0ax91Z+5H0FkW1es1ZcwmqBG
gQnMGOSzpvXWgPxzusEP7y+w23uzZHNCRVLyOwGGirmUVJcmK1+/+VT60h/XqEgQ
mQIDAQAB
-----END PUBLIC KEY-----" \
  JWT_ISSUER="Joken" \
  JWT_AUDIENCE="Joken"

echo "=== Setting ESO managed-by metadata (required for PushSecret ownership) ==="
kubectl exec -n vault vault-0 -- vault kv metadata put \
  -custom-metadata=managed-by=external-secrets \
  secret/wash/mcp-jwt-credentials-rotated

echo "=== Verifying secret was written ==="
kubectl exec -n vault vault-0 -- vault kv get secret/wash/mcp-jwt-credentials-rotated

echo ""
echo "Vault root token: root"
echo "Vault UI: http://localhost:8200"
echo "=== Vault installation complete ==="
