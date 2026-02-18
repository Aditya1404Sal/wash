#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MANIFEST_DIR="${SCRIPT_DIR}/../manifests"

echo "=== Installing External Secrets Operator ==="

helm repo add external-secrets https://charts.external-secrets.io
helm repo update external-secrets

kubectl create namespace external-secrets --dry-run=client -o yaml | kubectl apply -f -

helm upgrade --install external-secrets external-secrets/external-secrets \
  --namespace external-secrets \
  --values "${MANIFEST_DIR}/eso/eso-values.yaml" \
  --wait --timeout 120s

echo "=== Waiting for ESO pods to be ready ==="
kubectl wait --for=condition=Ready pods --all -n external-secrets --timeout=120s

echo "=== ESO installation complete ==="
kubectl get pods -n external-secrets
