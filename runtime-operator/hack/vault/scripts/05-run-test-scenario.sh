#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MANIFEST_DIR="${SCRIPT_DIR}/../manifests"
TEST_CLIENT_DIR="${SCRIPT_DIR}/../test-client"
NAMESPACE="wasmcloud-system"

echo "============================================"
echo "  E2E Dynamic Config Update Test"
echo "  (Full PushSecret Flow)"
echo "============================================"

echo ""
echo "=== Step 1: Pre-flight checks ==="

# Check NATS is reachable
if nc -z localhost 4222 2>/dev/null; then
  echo "  NATS: reachable on localhost:4222"
else
  echo "  ERROR: NATS not reachable on localhost:4222."
  echo "  Run: kubectl port-forward -n wasmcloud-system svc/nats 4222:4222 &"
  exit 1
fi

# Check operator is running
if pgrep -f "runtime-operator" > /dev/null; then
  echo "  Operator: running"
else
  echo "  ERROR: Operator not running. Start it with: cd runtime-operator && make devlog"
  exit 1
fi

# Check wash host is running
if pgrep -f "wash host" > /dev/null; then
  echo "  Wash host: running"
else
  echo "  ERROR: Wash host not running. Start it with:"
  echo "    wash host --http-addr 127.0.0.1:8000 --host-group public-ingress"
  exit 1
fi

# Check hosts are registered
HOST_COUNT=$(kubectl get host --no-headers 2>/dev/null | wc -l)
if [ "${HOST_COUNT}" -gt 0 ]; then
  echo "  Hosts registered: ${HOST_COUNT}"
else
  echo "  WARNING: No hosts registered yet. Waiting 15s..."
  sleep 15
  HOST_COUNT=$(kubectl get host --no-headers 2>/dev/null | wc -l)
  echo "  Hosts registered: ${HOST_COUNT}"
fi

echo ""
echo "=== Step 2: Apply ConfigMap ==="
kubectl apply -f "${MANIFEST_DIR}/test/configmap-plain.yaml"

echo ""
echo "=== Step 3: Verify ESO-synced secret exists ==="
kubectl get secret mcp-jwt-credentials -n "${NAMESPACE}" || {
  echo "ERROR: Secret not found. Run 04-configure-vault-eso.sh first."
  exit 1
}

echo ""
echo "=== Step 4: Deploy config-echo workload ==="
kubectl apply -f "${MANIFEST_DIR}/test/workload-with-secrets.yaml"

echo ""
echo "=== Step 5: Wait for WorkloadDeployment to be ready ==="
echo "(This requires the operator and a wash host to be running)"
kubectl wait --for=condition=Ready \
  workloaddeployment/config-echo-workload \
  -n "${NAMESPACE}" \
  --timeout=120s || {
  echo "WARNING: WorkloadDeployment not ready yet."
  echo "Make sure the operator and a wash host are running."
  echo "Continuing anyway — the test client will wait for the component."
}

echo ""
echo "=== Step 6: Run Rust test client ==="
echo "The test client will:"
echo "  1. Read baseline config from the config-echo component"
echo "  2. Create/patch the writable K8s Secret"
echo "  3. Wait for: writable Secret → PushSecret → Vault → ExternalSecret → K8s Secret → Operator → RPC"
echo "  4. Poll the component until it sees the updated config"
echo ""

cd "${TEST_CLIENT_DIR}"
cargo run 2>&1
TEST_EXIT=$?

echo ""
if [ ${TEST_EXIT} -eq 0 ]; then
  echo "============================================"
  echo "  E2E Dynamic Config Update: PASSED"
  echo "============================================"
else
  echo "============================================"
  echo "  E2E Dynamic Config Update: FAILED"
  echo "============================================"
  exit 1
fi
