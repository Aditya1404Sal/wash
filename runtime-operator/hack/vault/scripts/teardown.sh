#!/usr/bin/env bash
set -euo pipefail

CLUSTER_NAME="wasmcloud"

echo "=== Tearing down Kind cluster '${CLUSTER_NAME}' ==="

if kind get clusters 2>/dev/null | grep -q "^${CLUSTER_NAME}$"; then
  kind delete cluster --name "${CLUSTER_NAME}"
  echo "Cluster '${CLUSTER_NAME}' deleted."
else
  echo "Cluster '${CLUSTER_NAME}' not found, nothing to do."
fi
