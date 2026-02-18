# Local Vault + External Secrets Operator Setup

Local Kubernetes dev environment for testing secrets management with HashiCorp Vault and External Secrets Operator (ESO), including an E2E test for dynamic config hot-updates.

## Prerequisites

- [Kind](https://kind.sigs.k8s.io/) installed
- [kubectl](https://kubernetes.io/docs/tasks/tools/) installed
- [Rust toolchain](https://www.rust-lang.org/tools/install) with `wasm32-wasip2` target
- Docker running
- Helm is auto-installed by the scripts if not present

## Quick Start

### 1. Infrastructure setup (scripts 01-04)

```bash
./scripts/01-setup-kind.sh          # Kind cluster + Helm chart (NATS, wash hosts)
./scripts/02-install-vault.sh       # Vault in dev mode + seed test secrets
./scripts/03-install-eso.sh         # External Secrets Operator
./scripts/04-configure-vault-eso.sh # Wire Vault auth + ESO SecretStore + ExternalSecret + PushSecret
```

### 2. Start local services (in separate terminals)

Terminal 1 — **Operator** (uses your local code):
```bash
cd runtime-operator && make devlog
```

Terminal 2 — **Wash host**: (Explicitly use local wash due to hostapi changes)
```bash
target/release/wash host --http-addr '127.0.0.1:8000' --host-group public-ingress
```

### 3. Verify hosts are registered

```bash
kubectl get host
```

You should see your local host in the `public-ingress` hostgroup with `Ready=True`.

### 4. Run the E2E test

```bash
./scripts/05-run-test-scenario.sh
```

# Viewing secrets inside Vault 

run 
```bash
kubectl port-forward -n vault vault-0 8200:8200
```
Because the vault pod runs inside the kind cluster

Vault UI: http://localhost:8200 (token: `root`)

## Architecture

```
                  PushSecret (30s)
                       |
                       v
Vault KV (secret/wash/mcp-jwt-credentials-rotated)
  |  JWT_PUBLIC_KEY, JWT_ISSUER, JWT_AUDIENCE
  v
ESO ExternalSecret (refreshInterval: 30s)
  |  polls via ClusterSecretStore (K8s auth)
  v
K8s Secret "mcp-jwt-credentials" (namespace: wasmcloud-system)
  |  created/updated by ESO
  v
Operator reconcileConfigSync (30s reconcile interval)
  |  detects config hash change
  v
NATS RPC UpdateConfig -> wash-runtime host
  |  hot-updates wasi:config/store
  v
Component reads updated config — no restart needed
```

### E2E Test Flow

The test client (`hack/vault/test-client/`) validates the full round-trip:

1. Reads baseline config from the running `config-echo` component
2. Creates/patches a **writable** K8s Secret (`mcp-jwt-credentials-writable`)
3. PushSecret pushes the writable secret to Vault (~30s)
4. ExternalSecret syncs from Vault to the ESO-owned `mcp-jwt-credentials` secret (~30s)
5. Operator detects the config hash change and sends `UpdateConfig` RPC (~30s)
6. Component returns the updated value — hot update confirmed

**Why two secrets?** ESO's PushSecret refuses to push a secret it already manages via ExternalSecret. We use a separate writable secret for the push path and let ExternalSecret pull from the same Vault key.

## Config vs Secrets

| Type | K8s Resource | CRD Field | Vault-backed? | Use for |
|------|-------------|-----------|---------------|---------|
| Config | ConfigMap | `configFrom` | No | Feature flags, URLs, non-sensitive settings |
| Secret | Secret | `secretFrom` | Yes (via ESO) | API keys, JWT keys, passwords |
| Inline | - | `config` | No | Simple overrides |

Merge precedence (last wins): inline < configFrom < secretFrom

## Teardown

```bash
./scripts/teardown.sh  # Deletes the Kind cluster
```
