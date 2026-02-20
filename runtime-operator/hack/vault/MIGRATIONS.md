# Secret Store Migration Guide

This document describes how to swap the external secret store backend
(e.g. HashiCorp Vault → Azure Key Vault, AWS Secrets Manager, or GCP Secret Manager)
without changing any operator or wash runtime code.

## Architecture

```
┌─────────────────────┐
│  External Store     │  ← Only this layer changes
│  (Vault / Azure /   │
│   AWS / GCP)        │
└────────┬────────────┘
         │
   ESO pulls/pushes
         │
┌────────▼────────────┐
│  K8s Secret         │  ← Operator watches this (unchanged)
│  mcp-jwt-credentials│
└────────┬────────────┘
         │
   Operator detects hash change
         │
┌────────▼────────────┐
│  UpdateConfig RPC   │  ← wash runtime (unchanged)
│  → wasi:config/store│
└─────────────────────┘
```

The operator and wash runtime never talk to the external store directly.
They only interact with K8s Secrets and ConfigMaps. All migration work
is confined to the ESO configuration layer.

## What Changes

| File | Purpose | Must modify? |
|------|---------|--------------|
| `manifests/eso/cluster-secret-store.yaml` | ESO provider config | **Yes** — swap provider block |
| `manifests/eso/external-secret.yaml` | Pulls secrets into K8s | **Yes** — update `remoteRef.key` format |
| `manifests/eso/push-secret.yaml` | Pushes K8s secrets to store | **Yes** — update `remoteKey` format |
| `scripts/02-install-vault.sh` | Installs + seeds Vault | **Yes** — replace with new store setup |
| `scripts/04-configure-vault-eso.sh` | Vault auth + ESO config | **Yes** — replace auth method |
| `manifests/eso/eso-values.yaml` | ESO Helm values | No (ESO itself stays the same) |
| `manifests/test/workload-with-secrets.yaml` | Workload manifest | No |
| `test-client/src/main.rs` | E2E test | No |
| Operator code (Go) | Watches K8s Secrets | No |
| wash runtime (Rust) | wasi:config/store plugin | No |

## What Does NOT Change

- **Workload manifests** — they reference K8s Secrets by name, not the backing store
- **Operator** — `reconcileConfigSync` reads K8s Secrets via `ResolveSecretFrom`, hashes them, sends RPC
- **wash runtime** — `WasiConfig` plugin receives config via RPC, doesn't know the source
- **Test client** — patches K8s Secrets directly, polls the component
- **ConfigMap flow** — ConfigMaps don't use ESO at all, completely unaffected

## Migration Steps

### 1. ClusterSecretStore

Replace the provider block in `manifests/eso/cluster-secret-store.yaml`:

**Current (Vault):**
```yaml
apiVersion: external-secrets.io/v1
kind: ClusterSecretStore
metadata:
  name: vault-backend
spec:
  provider:
    vault:
      server: "http://vault.vault.svc.cluster.local:8200"
      path: "secret"
      version: "v2"
      auth:
        kubernetes:
          mountPath: "kubernetes"
          role: "eso-role"
          serviceAccountRef:
            name: "external-secrets"
            namespace: "external-secrets"
```

**Azure Key Vault (Managed Identity):**
```yaml
apiVersion: external-secrets.io/v1beta1
kind: ClusterSecretStore
metadata:
  name: vault-backend          # keep the name so ExternalSecret/PushSecret refs don't change
spec:
  provider:
    azurekv:
      authType: ManagedIdentity # or WorkloadIdentity, ServicePrincipal
      vaultUrl: "https://<your-keyvault>.vault.azure.net"
      tenantId: "<azure-tenant-id>"
```

**AWS Secrets Manager:**
```yaml
apiVersion: external-secrets.io/v1
kind: ClusterSecretStore
metadata:
  name: vault-backend
spec:
  provider:
    aws:
      service: SecretsManager
      region: us-west-2
      auth:
        jwt:
          serviceAccountRef:
            name: "external-secrets"
            namespace: "external-secrets"
```

**GCP Secret Manager:**
```yaml
apiVersion: external-secrets.io/v1
kind: ClusterSecretStore
metadata:
  name: vault-backend
spec:
  provider:
    gcpsm:
      projectID: <your-project-id>
      auth:
        workloadIdentity:
          clusterLocation: us-central1
          clusterName: <your-cluster>
          serviceAccountRef:
            name: "external-secrets"
            namespace: "external-secrets"
```

> **Tip:** Keep `metadata.name: vault-backend` (or rename it to something
> generic like `secret-backend`) so that ExternalSecret and PushSecret
> `secretStoreRef.name` values stay consistent.

### 2. ExternalSecret

Update `remoteRef.key` to match the new store's key format:

**Current (Vault KV v2 — path-based):**
```yaml
data:
  - secretKey: JWT_ISSUER
    remoteRef:
      key: wash/mcp-jwt-credentials-rotated
      property: JWT_ISSUER
```

**Azure Key Vault (flat key names, no `/`):**
```yaml
data:
  - secretKey: JWT_ISSUER
    remoteRef:
      key: wash-mcp-jwt-credentials-rotated
      property: JWT_ISSUER
```

**AWS Secrets Manager (JSON secret with properties):**
```yaml
data:
  - secretKey: JWT_ISSUER
    remoteRef:
      key: wash/mcp-jwt-credentials-rotated   # secret name in AWS
      property: JWT_ISSUER                      # JSON key within the secret
```

**GCP Secret Manager (one secret per key, or JSON):**
```yaml
data:
  - secretKey: JWT_ISSUER
    remoteRef:
      key: wash-mcp-jwt-credentials-rotated
      property: JWT_ISSUER
```

### 3. PushSecret

Same key format change as ExternalSecret:

**Current (Vault):**
```yaml
data:
  - match:
      remoteRef:
        remoteKey: wash/mcp-jwt-credentials-rotated
```

**Azure:**
```yaml
data:
  - match:
      remoteRef:
        remoteKey: wash-mcp-jwt-credentials-rotated
```

> **Note:** Not all providers support PushSecret. Check the
> [ESO PushSecret docs](https://external-secrets.io/latest/guides/pushsecrets/)
> for provider compatibility. If your provider doesn't support PushSecret,
> you'll need to write secrets to the external store directly (CLI, API, or CI/CD)
> and rely on ExternalSecret to pull them into K8s.

### 4. Script 02 — Store Installation

Replace `scripts/02-install-vault.sh` with your store's setup.

**For Azure Key Vault:** No in-cluster installation needed — the vault
exists in Azure. Script becomes seed-only:

```bash
#!/usr/bin/env bash
set -euo pipefail

echo "=== Seeding Azure Key Vault ==="
az keyvault secret set \
  --vault-name <your-keyvault> \
  --name "wash-mcp-jwt-credentials-rotated" \
  --value '{"JWT_PUBLIC_KEY":"...","JWT_ISSUER":"Joken","JWT_AUDIENCE":"Joken"}'

echo "=== Azure Key Vault seeded ==="
```

**For AWS Secrets Manager:**
```bash
aws secretsmanager create-secret \
  --name wash/mcp-jwt-credentials-rotated \
  --secret-string '{"JWT_PUBLIC_KEY":"...","JWT_ISSUER":"Joken","JWT_AUDIENCE":"Joken"}'
```

### 5. Script 04 — Auth Configuration

Replace `scripts/04-configure-vault-eso.sh`.

**For Azure (Workload Identity):**
```bash
#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MANIFEST_DIR="${SCRIPT_DIR}/../manifests"

echo "=== Applying ClusterSecretStore ==="
kubectl apply -f "${MANIFEST_DIR}/eso/cluster-secret-store.yaml"

echo "=== Applying ExternalSecret ==="
kubectl apply -f "${MANIFEST_DIR}/eso/external-secret.yaml"

kubectl wait --for=condition=Ready \
  externalsecret/mcp-jwt-credentials \
  -n wasmcloud-system \
  --timeout=60s

echo "=== Applying PushSecret ==="
kubectl apply -f "${MANIFEST_DIR}/eso/push-secret.yaml"

echo "=== Done ==="
```

No Vault policy or Kubernetes auth config needed — Azure uses
managed identity or workload identity directly.

## Verification

After migration, the same E2E test works unchanged:

```bash
./scripts/05-run-test-scenario.sh
```

Expected output:
```
Test 1: Secret Hot-Update (via ESO)     — PASS (~20-35s)
Test 2: ConfigMap Hot-Update (direct)   — PASS (~30s)
```

The timing for Test 1 depends on `refreshInterval` in the
ExternalSecret and PushSecret manifests (currently 10s each).

## Two-Secret Pattern

The test setup uses a two-secret pattern to avoid ESO ownership conflicts:

```
mcp-jwt-credentials          ← ESO-owned (ExternalSecret creates this, workload reads it)
mcp-jwt-credentials-writable ← Test-client-owned (PushSecret watches this, pushes to store)
```

This pattern is the same regardless of the backing store. The only
difference is the `remoteKey` format in the PushSecret and ExternalSecret.

## Quick Reference: Key Format by Provider

| Provider | Key format | Supports PushSecret |
|----------|-----------|-------------------|
| Vault KV v2 | `wash/mcp-jwt-credentials-rotated` | Yes |
| Azure Key Vault | `wash-mcp-jwt-credentials-rotated` | Yes |
| AWS Secrets Manager | `wash/mcp-jwt-credentials-rotated` | Yes |
| GCP Secret Manager | `wash-mcp-jwt-credentials-rotated` | Yes |
