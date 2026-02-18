#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../../../.." && pwd)"      # wash/ repo root

echo "=== Setting up Kind cluster via make kind-setup ==="

if kind get clusters 2>/dev/null | grep -q "^wasmcloud$"; then
  echo "Cluster 'wasmcloud' already exists, skipping creation."
else
  (cd "${REPO_ROOT}" && make kind-setup)
  echo "Cluster 'wasmcloud' created."
fi

kubectl config use-context "kind-wasmcloud"

echo "=== Creating wasmcloud-system namespace ==="
kubectl create namespace wasmcloud-system --dry-run=client -o yaml | kubectl apply -f -

echo "=== Updating Helm chart dependencies ==="
helm dependency update "${REPO_ROOT}/charts/runtime-operator"

echo "=== Installing Helm chart (NATS + wash hosts) ==="
(cd "${REPO_ROOT}" && make helm-install)

echo "=== Waiting for NATS to be ready ==="
echo "(Waiting for pod to be scheduled...)"
until kubectl get pod -l wasmcloud.com/name=nats -n wasmcloud-system 2>/dev/null | grep -q "nats"; do sleep 2; done
kubectl wait --for=condition=Ready pod -l wasmcloud.com/name=nats -n wasmcloud-system --timeout=180s

echo "=== Scaling down in-cluster operator (we run locally via make devlog) ==="
kubectl scale deployment -n wasmcloud-system runtime-operator --replicas=0

echo "=== Port-forwarding NATS to localhost:4222 ==="
echo "(Running in background — kill with: pkill -f 'port-forward.*nats')"
nohup kubectl port-forward -n wasmcloud-system svc/nats 4222:4222 > /tmp/nats-portforward.log 2>&1 &
sleep 2

if nc -z localhost 4222 2>/dev/null; then
  echo "NATS reachable on localhost:4222"
else
  echo "WARNING: NATS port-forward may have failed. Check /tmp/nats-portforward.log"
fi

echo ""
echo "=== Kind cluster ready ==="
echo ""
echo "Next steps (run in separate terminals):"
echo "  1. Start the operator:  cd runtime-operator && make devlog"
echo "  2. Start a wash host:   wash host --http-addr 127.0.0.1:8000 --host-group public-ingress"
echo "  3. Verify:              kubectl get host"
echo ""
kubectl cluster-info
