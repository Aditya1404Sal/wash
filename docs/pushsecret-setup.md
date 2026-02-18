# PushSecret: K8s-Native Secret Management Without Vault Lock-In

## The Idea

Instead of writing secrets directly to Vault (or any specific provider), you write them as K8s Secrets and let ESO push them to whatever backend you've configured. Your workflow stays K8s-native — `kubectl`, Helm, CI/CD pipelines, ArgoCD — and the external provider becomes a pluggable storage backend. Switch from Vault to AWS Secrets Manager by changing a `SecretStore` reference. Your application code, your manifests, and your operator never know the difference.

```
                  K8s Secret (source of truth for writes)
                          │
              ┌───────────┼───────────┐
              │           │           │
         PushSecret  PushSecret  PushSecret
              │           │           │
              ▼           ▼           ▼
           Vault    AWS Secrets   Azure KV
                    Manager
```

The key insight: **K8s becomes the control plane for secrets, not Vault.** Vault is just one possible storage backend. You can push the same secret to multiple providers simultaneously for redundancy, or swap providers entirely by changing a single `SecretStore` reference.

---

## How PushSecret Works

PushSecret is the inverse of ExternalSecret:

| Direction | CRD | What it does |
|-----------|-----|--------------|
| Pull (inbound) | `ExternalSecret` | Reads from Vault/AWS/etc → creates K8s Secret |
| Push (outbound) | `PushSecret` | Reads K8s Secret → writes to Vault/AWS/etc |

You can use both together. A common pattern: PushSecret writes to Vault, ExternalSecret reads from Vault into other namespaces/clusters. Vault acts as a distribution hub, not a management UI.

### PushSecret Spec

```yaml
apiVersion: external-secrets.io/v1alpha1
kind: PushSecret
metadata:
  name: push-mcp-credentials
  namespace: wasmcloud-system
spec:
  # How often to sync K8s → provider
  refreshInterval: 1h

  # Replace: overwrite provider secret on every sync
  # IfNotExists: only write if provider secret doesn't exist (provider becomes source of truth)
  updatePolicy: Replace

  # Delete: remove from provider when PushSecret CR is deleted
  # None: leave provider secret in place (default)
  deletionPolicy: Delete

  # Which SecretStore(s) to push to — this is where provider-agnosticism lives
  secretStoreRefs:
    - name: vault-backend
      kind: ClusterSecretStore

  # Which K8s Secret to read from
  selector:
    secret:
      name: mcp-jwt-credentials

  # Key mapping: which keys to push and where they land in the provider
  data:
    - match:
        secretKey: JWT_PUBLIC_KEY        # key in the K8s Secret
        remoteRef:
          remoteKey: wash/mcp-jwt-credentials   # path in Vault
          property: JWT_PUBLIC_KEY               # field within that path
    - match:
        secretKey: JWT_ISSUER
        remoteRef:
          remoteKey: wash/mcp-jwt-credentials
          property: JWT_ISSUER
    - match:
        secretKey: JWT_AUDIENCE
        remoteRef:
          remoteKey: wash/mcp-jwt-credentials
          property: JWT_AUDIENCE
```

### Push All Keys (No Mapping)

If you want to push every key in the K8s Secret to a single remote path without listing them individually:

```yaml
spec:
  selector:
    secret:
      name: mcp-jwt-credentials
  data:
    - match:
        remoteRef:
          remoteKey: wash/mcp-jwt-credentials
          # no secretKey or property — pushes all keys as-is
```

---

## Provider-Agnostic Architecture

The magic is in `secretStoreRefs`. Your PushSecret doesn't reference Vault directly — it references a `SecretStore` (or `ClusterSecretStore`), which is a separate resource that encapsulates provider-specific connection details. To switch providers, you change the SecretStore. Everything else stays the same.

### Vault SecretStore

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

### AWS Secrets Manager SecretStore

```yaml
apiVersion: external-secrets.io/v1
kind: ClusterSecretStore
metadata:
  name: aws-backend
spec:
  provider:
    aws:
      service: SecretsManager
      region: us-east-1
      auth:
        jwt:
          serviceAccountRef:
            name: "external-secrets"
            namespace: "external-secrets"
```

### Azure Key Vault SecretStore

```yaml
apiVersion: external-secrets.io/v1
kind: ClusterSecretStore
metadata:
  name: azure-backend
spec:
  provider:
    azurekv:
      tenantId: "your-tenant-id"
      vaultUrl: "https://your-vault.vault.azure.net"
      authType: ManagedIdentity
```

### GCP Secret Manager SecretStore

```yaml
apiVersion: external-secrets.io/v1
kind: ClusterSecretStore
metadata:
  name: gcp-backend
spec:
  provider:
    gcpsm:
      projectID: "your-gcp-project"
      auth:
        workloadIdentity:
          clusterLocation: us-central1
          clusterName: your-cluster
          serviceAccountRef:
            name: "external-secrets"
            namespace: "external-secrets"
```

### Switching Providers

To migrate from Vault to AWS Secrets Manager:

```diff
  spec:
    secretStoreRefs:
-     - name: vault-backend
+     - name: aws-backend
        kind: ClusterSecretStore
```

That's it. The K8s Secret, the PushSecret data mapping, your workload manifests, your operator, your Wasm components — none of them change.

---

## Multi-Provider Push

You can push to multiple providers simultaneously. This is useful for disaster recovery, multi-cloud redundancy, or gradual migrations.

```yaml
apiVersion: external-secrets.io/v1alpha1
kind: PushSecret
metadata:
  name: push-mcp-credentials-multi
  namespace: wasmcloud-system
spec:
  refreshInterval: 1h
  updatePolicy: Replace
  deletionPolicy: Delete
  secretStoreRefs:
    - name: vault-backend
      kind: ClusterSecretStore
    - name: aws-backend
      kind: ClusterSecretStore
    - name: azure-backend
      kind: ClusterSecretStore
  selector:
    secret:
      name: mcp-jwt-credentials
  data:
    - match:
        remoteRef:
          remoteKey: wash/mcp-jwt-credentials
```

Each provider receives an independent copy. If one provider is down, the others still get updated.

---

## Automatic Rotation with Generators

ESO has a `Generator` CRD that produces secret values. Combined with PushSecret, you get automatic rotation without any external tooling.

```yaml
# Generator: produces a random password
apiVersion: generators.external-secrets.io/v1alpha1
kind: Password
metadata:
  name: db-password-gen
  namespace: wasmcloud-system
spec:
  length: 64
  digits: 10
  symbols: 5
  symbolCharacters: "-_!@#"
  noUpper: false
  allowRepeat: true

---
# PushSecret: pushes the generated password to Vault every 6 hours
apiVersion: external-secrets.io/v1alpha1
kind: PushSecret
metadata:
  name: rotate-db-password
  namespace: wasmcloud-system
spec:
  refreshInterval: 6h          # rotates every 6 hours
  updatePolicy: Replace         # overwrites the old password
  deletionPolicy: Delete
  secretStoreRefs:
    - name: vault-backend
      kind: ClusterSecretStore
  selector:
    generatorRef:
      apiVersion: generators.external-secrets.io/v1alpha1
      kind: Password
      name: db-password-gen
  data:
    - match:
        secretKey: password
        remoteRef:
          remoteKey: wash/database
          property: password
```

Every `refreshInterval`, ESO:
1. Consults the Generator to produce a new password
2. Pushes it to Vault (or whatever provider is configured)
3. The ExternalSecret on the other side picks up the new value on its own `refreshInterval`
4. The operator re-materializes and sends `WorkloadUpdateConfig` to the runtime

Change `updatePolicy: IfNotExists` to generate once and never rotate.

---

## Programmatic Secret Updates from Rust

If you generate secrets in your own code (e.g., JWT signing keys from a Rust service), you don't need Vault CLI or any provider SDK. Just update the K8s Secret via the K8s API — PushSecret watches it and syncs to the provider automatically.

### The Flow

```
Rust service (generates JWT keys)
    │
    │  K8s API: PATCH Secret "mcp-jwt-credentials"
    ▼
K8s Secret updated
    │
    │  ESO detects change on next refreshInterval
    ▼
PushSecret syncs to Vault/AWS/Azure
    │
    │  ExternalSecret picks it up (other namespaces/clusters)
    ▼
Operator re-materializes → WorkloadUpdateConfig RPC → component sees new key
```

### Rust Example Using the `kube` Crate

```rust
use k8s_openapi::api::core::v1::Secret;
use kube::{Api, Client};
use base64::Engine;

async fn rotate_jwt_keys(namespace: &str) -> anyhow::Result<()> {
    // Generate new keys (your logic)
    let (public_key, _private_key) = generate_jwt_keypair()?;

    let client = Client::try_default().await?;
    let secrets: Api<Secret> = Api::namespaced(client, namespace);

    // K8s Secrets store base64-encoded bytes in `data`
    let b64 = base64::engine::general_purpose::STANDARD;
    let patch = serde_json::json!({
        "data": {
            "JWT_PUBLIC_KEY": b64.encode(&public_key),
            "JWT_ISSUER": b64.encode("Joken"),
            "JWT_AUDIENCE": b64.encode("mcp-server"),
        }
    });

    secrets.patch(
        "mcp-jwt-credentials",
        &kube::api::PatchParams::apply("jwt-rotator"),
        &kube::api::Patch::Merge(&patch),
    ).await?;

    Ok(())
}
```

The only dependency is the `kube` crate. No Vault SDK, no Vault token, no provider-specific code.

### RBAC for the Rotation Service

The ServiceAccount running your Rust service needs permission to read and update Secrets in the target namespace:

```yaml
apiVersion: rbac.authorization.k8s.io/v1
kind: Role
metadata:
  name: secret-writer
  namespace: wasmcloud-system
rules:
  - apiGroups: [""]
    resources: ["secrets"]
    verbs: ["get", "patch", "update"]
---
apiVersion: rbac.authorization.k8s.io/v1
kind: RoleBinding
metadata:
  name: jwt-rotator-binding
  namespace: wasmcloud-system
subjects:
  - kind: ServiceAccount
    name: jwt-rotator       # your service's ServiceAccount
roleRef:
  kind: Role
  name: secret-writer
  apiGroup: rbac.authorization.k8s.io
```

### From a Wasm Component

If your rotation logic runs as a Wasm component (not a native K8s pod), the component doesn't have direct K8s API access. Options:

1. **K8s CronJob** — schedule a native container that runs the rotation and patches the Secret
2. **Sidecar / init container** — a helper container with K8s API access that the component triggers via HTTP or NATS
3. **Operator-side rotation** — add rotation logic to the operator itself, which already has K8s client access

### Propagation Timing

PushSecret uses polling, not webhooks. The maximum delay from K8s Secret update to provider update is the PushSecret's `refreshInterval`. With `refreshInterval: 30s`, Vault gets the new value within 30 seconds. The full end-to-end delay (Secret patch → component sees new value) is:

```
K8s Secret patch
    + PushSecret refreshInterval (e.g., 30s)     → secret lands in Vault
    + ExternalSecret refreshInterval (e.g., 30s)  → other clusters pick it up
    + Operator reconcile interval (e.g., 30s)      → WorkloadUpdateConfig sent
    = worst case ~90s for cross-cluster, ~60s for same-cluster
```

For same-cluster same-namespace, the PushSecret step is unnecessary — the operator reads the K8s Secret directly. So the delay is just the operator's reconcile interval.

---

## Full Bidirectional Flow

Here's how PushSecret and ExternalSecret work together for a complete K8s-native secrets pipeline:

```
┌──────────────────────────────────────────────────────────────────────────┐
│  WRITE PATH (K8s → Provider)                                             │
│                                                                          │
│  kubectl create secret ─► K8s Secret ─► PushSecret ─► Vault/AWS/Azure   │
│  CI/CD pipeline              │                              │            │
│  Helm chart                  │                              │            │
│  ArgoCD                      │                              │            │
│  Generator (rotation)        │                              │            │
└──────────────────────────────┼──────────────────────────────┼────────────┘
                               │                              │
                               │        provider stores it    │
                               │                              │
┌──────────────────────────────┼──────────────────────────────┼────────────┐
│  READ PATH (Provider → K8s → Runtime)                       │            │
│                                                              │            │
│  Vault/AWS/Azure ──────── ExternalSecret ──► K8s Secret     │            │
│       │                  (30s poll)          (same or        │            │
│       │                                      different       │            │
│       │                                      namespace)      │            │
│       │                                         │            │            │
│       │                  Operator reads K8s Secret            │            │
│       │                  via secretFrom                       │            │
│       │                         │                             │            │
│       │                  MaterializeConfigLayer()             │            │
│       │                         │                             │            │
│       │                  WorkloadUpdateConfig RPC             │            │
│       │                         │                             │            │
│       │                  wasi:config/store.get("key")         │            │
│       │                  → returns latest value               │            │
└───────┼──────────────────────────────────────────────────────┼────────────┘
        │                                                      │
        └──────────────── same provider ───────────────────────┘
```

### Why This Works Without Lock-In

1. **Your workload manifests** reference K8s Secrets by name (`secretFrom: [{name: mcp-jwt-credentials}]`). They don't know or care whether the Secret came from Vault, AWS, or was created manually.

2. **Your CI/CD pipelines** create K8s Secrets using `kubectl` or Helm. They don't talk to Vault directly.

3. **PushSecret** handles the K8s → provider direction. Change the `SecretStore` to change the provider.

4. **ExternalSecret** handles the provider → K8s direction (for other namespaces/clusters that need the same secrets). Also just references a `SecretStore`.

5. **The operator** reads K8s Secrets. It has no Vault SDK, no AWS SDK, no Azure SDK. It uses the standard K8s client.

6. **The runtime** receives config over NATS RPC. It has no idea where the config came from.

The only place provider-specific configuration exists is in the `SecretStore` CRD. Everything else is K8s-native.

---

## Concrete Setup for wasmCloud

Here's what the complete setup looks like for our MCP workload:

### 1. Create the source K8s Secret

```bash
kubectl create secret generic mcp-jwt-credentials \
  -n wasmcloud-system \
  --from-literal=JWT_PUBLIC_KEY="-----BEGIN PUBLIC KEY-----
MIIBIjANBgkqhkiG9..." \
  --from-literal=JWT_ISSUER="Joken" \
  --from-literal=JWT_AUDIENCE="mcp-server"
```

Or via a manifest committed to Git (with sealed-secrets or SOPS for encryption at rest):

```yaml
apiVersion: v1
kind: Secret
metadata:
  name: mcp-jwt-credentials
  namespace: wasmcloud-system
type: Opaque
stringData:
  JWT_PUBLIC_KEY: "-----BEGIN PUBLIC KEY-----\nMIIBIjAN..."
  JWT_ISSUER: "Joken"
  JWT_AUDIENCE: "mcp-server"
```

### 2. Push to Vault (or any provider)

```yaml
apiVersion: external-secrets.io/v1alpha1
kind: PushSecret
metadata:
  name: push-mcp-credentials
  namespace: wasmcloud-system
spec:
  refreshInterval: 1h
  updatePolicy: Replace
  deletionPolicy: Delete
  secretStoreRefs:
    - name: vault-backend
      kind: ClusterSecretStore
  selector:
    secret:
      name: mcp-jwt-credentials
  data:
    - match:
        remoteRef:
          remoteKey: wash/mcp-jwt-credentials
```

### 3. (Optional) Pull into other namespaces/clusters via ExternalSecret

If other namespaces or clusters need the same secret:

```yaml
apiVersion: external-secrets.io/v1
kind: ExternalSecret
metadata:
  name: mcp-jwt-credentials
  namespace: other-namespace
spec:
  refreshInterval: "30s"
  secretStoreRef:
    name: vault-backend
    kind: ClusterSecretStore
  target:
    name: mcp-jwt-credentials
    creationPolicy: Owner
  data:
    - secretKey: JWT_PUBLIC_KEY
      remoteRef:
        key: wash/mcp-jwt-credentials
        property: JWT_PUBLIC_KEY
    - secretKey: JWT_ISSUER
      remoteRef:
        key: wash/mcp-jwt-credentials
        property: JWT_ISSUER
    - secretKey: JWT_AUDIENCE
      remoteRef:
        key: wash/mcp-jwt-credentials
        property: JWT_AUDIENCE
```

### 4. Workload references K8s Secret (unchanged)

```yaml
apiVersion: runtime.wasmcloud.dev/v1alpha1
kind: Workload
metadata:
  name: mcp-server
  namespace: wasmcloud-system
spec:
  components:
    - name: mcp-handler
      image: ghcr.io/example/mcp-handler:latest
      localResources:
        environment:
          secretFrom:
            - name: mcp-jwt-credentials
```

Nothing in this manifest is Vault-specific. If you switch from Vault to AWS Secrets Manager tomorrow, this manifest doesn't change.

---

## Updating a Secret

When you need to rotate or update a secret, the workflow is entirely K8s-native:

```bash
# Update the K8s Secret
kubectl create secret generic mcp-jwt-credentials \
  -n wasmcloud-system \
  --from-literal=JWT_PUBLIC_KEY="-----BEGIN PUBLIC KEY-----
NEW_KEY_HERE..." \
  --from-literal=JWT_ISSUER="Joken" \
  --from-literal=JWT_AUDIENCE="mcp-server" \
  --dry-run=client -o yaml | kubectl apply -f -
```

What happens next:
1. PushSecret detects the K8s Secret changed (next `refreshInterval` or immediately if using a watch)
2. PushSecret writes the new value to Vault (or whichever provider is configured)
3. If other clusters use ExternalSecret to pull from the same provider, they get the update too
4. The operator detects the K8s Secret change, re-materializes config, sends `WorkloadUpdateConfig` RPC
5. Wasm components see the new value on their next `wasi:config/store.get()` call

You never had to run `vault kv put`. You never had to install the Vault CLI. You used `kubectl`.

---

## Policies and Lifecycle

### updatePolicy

| Policy | Behavior | Use case |
|--------|----------|----------|
| `Replace` | Overwrites provider secret every `refreshInterval` | Active management — K8s is source of truth |
| `IfNotExists` | Only writes if the provider secret doesn't exist | Bootstrap / seed — provider becomes source of truth after first write |

### deletionPolicy

| Policy | Behavior | Use case |
|--------|----------|----------|
| `None` (default) | Provider secret survives PushSecret deletion | Audit trail, DR — provider retains history |
| `Delete` | Provider secret is deleted when PushSecret is deleted | Clean lifecycle — no orphaned secrets |

### refreshInterval

How often ESO reconciles the PushSecret. At each tick, ESO reads the K8s Secret and compares it with the provider. If they differ, ESO pushes the update.

- `1h` — good default for stable secrets
- `30s` — for secrets that change frequently or need fast propagation
- `6h` — for generated/rotated passwords (combined with Generator)

---

## Supported Providers (PushSecret)

Not all ESO providers support PushSecret. The ones that do:

| Provider | PushSecret Support |
|----------|-------------------|
| HashiCorp Vault | Yes |
| AWS Secrets Manager | Yes |
| AWS Parameter Store | Yes |
| Azure Key Vault | Yes |
| GCP Secret Manager | Yes |
| Kubernetes (cross-namespace/cluster) | Yes |
| Akeyless | Yes |
| Pulumi ESC | Yes |
| Keeper | Yes |
| Doppler | Check provider docs |

If a provider doesn't support PushSecret, you can still use the K8s → K8s provider to replicate secrets across namespaces and clusters, keeping K8s as the write interface.

---

## Comparison with Direct Vault Integration

| Aspect | PushSecret (K8s-native) | Direct Vault SDK |
|--------|------------------------|------------------|
| Write interface | `kubectl`, Helm, ArgoCD | `vault kv put`, Vault API |
| Provider lock-in | None — swap SecretStore | Full lock-in to Vault API |
| Multi-provider | Built-in (list multiple SecretStores) | Custom code per provider |
| Rotation | Generator + PushSecret | Vault dynamic secrets (Vault-only) |
| CI/CD integration | Standard K8s tooling | Vault CLI/SDK in pipeline |
| Audit trail | K8s audit log + provider audit log | Provider audit log only |
| Operator complexity | Zero Vault SDK code | Must import Vault client |
| Runtime dependency | ESO running in cluster | Vault reachable from app |
| GitOps compatible | Yes (Secret manifests in Git with SOPS/Sealed Secrets) | Partial (Vault policies in Git, but secrets aren't) |

---

## References

- [PushSecret API Reference](https://external-secrets.io/latest/api/pushsecret/)
- [PushSecret Guide](https://external-secrets.io/latest/guides/pushsecrets/)
- [ESO Provider Documentation](https://external-secrets.io/latest/introduction/overview/)
- [Vault Provider for ESO](https://external-secrets.io/latest/provider/hashicorp-vault/)
- [Reversing the Workflow with PushSecret](https://eminalemdar.medium.com/reversing-the-workflow-with-external-secrets-operators-push-secret-feature-f2a64f3db748)
