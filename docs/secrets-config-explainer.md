# Secrets & Configuration: The Complete Picture

This document explains, end-to-end, how secrets and configuration flow from an external vault into a running Wasm component — and how updates propagate without restarting workloads.

---

## Table of Contents

1. [The Problem](#the-problem)
2. [Should You Use `wasi:config` for Secrets?](#should-you-use-wasiconfig-for-secrets)
3. [The Three Config Interfaces](#the-three-config-interfaces)
4. [How Secrets Get Into Vault](#how-secrets-get-into-vault)
5. [How ESO Syncs Vault to K8s](#how-eso-syncs-vault-to-k8s)
6. [How the Operator Reads K8s Secrets](#how-the-operator-reads-k8s-secrets)
7. [How the Runtime Delivers Config to Components](#how-the-runtime-delivers-config-to-components)
8. [How Config Updates Without Restart](#how-config-updates-without-restart)
9. [The Full Data Flow](#the-full-data-flow)
10. [What Happens When a Secret Rotates in Vault](#what-happens-when-a-secret-rotates-in-vault)
11. [Secrets vs Config: Where to Put What](#secrets-vs-config-where-to-put-what)
12. [RBAC: What the Operator Needs](#rbac-what-the-operator-needs)
13. [Current Limitations and Future Work](#current-limitations-and-future-work)

---

## The Problem

Wasm components running on wasmCloud need access to configuration and secrets — JWT keys, API tokens, feature flags, database connection strings. Today there's no integrated vault solution, no automatic secret rotation, and updating any config requires a full workload restart (stop + start). We need:

1. A secure external secret store (HashiCorp Vault)
2. Automatic synchronization into K8s (External Secrets Operator)
3. A pipeline that delivers secrets to Wasm components at runtime
4. Hot config updates without workload restarts

---

## Should You Use `wasi:config` for Secrets?

**Yes.** `wasi:config/store` is the recommended interface for secrets. Here's why:

| Factor | `wasi:config/store` | `wasi:cli/environment` (env vars) |
|--------|---------------------|-----------------------------------|
| Hot-updatable? | **Yes** — backed by `Arc<RwLock<HashMap>>`, shared across all invocations | **No** — baked into `WasiCtx` at store creation, frozen for lifetime of invocation |
| Read interface | `store.get("JWT_PUBLIC_KEY")` | `environment.get-environment()` |
| Mutability | Read-only from component's perspective, writable by host | Immutable after creation |
| Rotation support | Values updated in-place, visible on next `get()` | Requires workload restart |
| Isolation | Per-component scope (keyed by component ID) | Shared across all components in workload |

The key insight is in the runtime implementation. When the `WasiConfig` plugin is instantiated, it stores configuration in:

```rust
// washlet/plugins/wasi_config.rs:35
config: Arc<RwLock<HashMap<String, HashMap<String, String>>>>
```

This `Arc<RwLock<...>>` is **shared by reference** across all component invocations. When a component calls `wasi:config/store.get("key")`, the plugin acquires a read lock on this shared hashmap and returns the current value:

```rust
// washlet/plugins/wasi_config.rs:39-48
async fn get(&mut self, key: String) -> anyhow::Result<Result<Option<String>, ConfigError>> {
    let Some(plugin) = self.get_plugin::<WasiConfig>(PLUGIN_WASI_CONFIG_ID) else {
        return Ok(Ok(None));
    };
    let config_guard = plugin.config.read().await;
    config_guard
        .get(&self.component_id.to_string())
        .and_then(|map| map.get(&key).cloned())
        .map_or(Ok(Ok(None)), |v| Ok(Ok(Some(v))))
}
```

Because it acquires the lock **on every call**, if the underlying hashmap is updated between calls, the component sees the new value immediately. There is no caching, no stale reads.

Compare this with `wasi:cli/environment`, where env vars are set once during `WasiCtxBuilder::envs()`:

```rust
// engine/workload.rs:943-955
let mut wasi_ctx_builder = WasiCtxBuilder::new();
wasi_ctx_builder
    .envs(
        metadata.local_resources.environment
            .iter()
            .map(|kv| (kv.0.as_str(), kv.1.as_str()))
            .collect::<Vec<_>>()
            .as_slice(),
    )
    .inherit_stdout()
    .inherit_stderr();
```

Once those env vars are copied into the `WasiCtx`, they're gone. The original `HashMap` could change and the component would never see it. You'd have to stop the workload, create a new store, and start again.

**Bottom line:** Use `wasi:config/store.get("key")` in your components for anything that might need rotation — secrets, API keys, JWT credentials, feature flags.

---

## The Three Config Interfaces

Wasm components have access to three distinct configuration interfaces. Each serves a different purpose:

### 1. `wasi:config/store` — Operator-Managed Config (Recommended for Secrets)

- **Who writes it?** The K8s operator, via the runtime host
- **Who reads it?** The Wasm component, via `store.get(key)` and `store.get-all()`
- **Backed by?** `Arc<RwLock<HashMap>>` in the `WasiConfig` plugin
- **Hot-updatable?** Yes — the `WorkloadUpdateConfig` RPC writes directly to the shared hashmap
- **Scope:** Per-component (each component has its own isolated config namespace)
- **Use for:** JWT keys, API tokens, feature flags, database URIs, any deployment-time config that the operator controls

### 2. `wasi:cli/environment` — Static Environment Variables

- **Who writes it?** The K8s operator (same source as `wasi:config`)
- **Who reads it?** The Wasm component, via `environment.get-environment()`
- **Backed by?** A plain `HashMap` copied into `WasiCtx` at creation time
- **Hot-updatable?** No — frozen at invocation start
- **Scope:** Shared across all components in the workload
- **Use for:** Truly static config that never changes (binary feature toggles, build version info). Avoid for secrets.

### 3. `wasi:keyvalue` — Component-Writable Key-Value Store

- **Who writes it?** The Wasm component itself (read AND write)
- **Backed by?** NATS JetStream Key-Value store (in the washlet path)
- **Hot-updatable?** N/A — components manage their own data at runtime
- **Scope:** Per-component or shared (depending on bucket configuration)
- **Use for:** Runtime mutable state — session data, caches, builder configurations (Betty configs). This is the Redis replacement.

---

## How Secrets Get Into Vault

HashiCorp Vault is the source of truth for all sensitive data. In the local dev environment, Vault runs in dev mode inside the Kind cluster, with a KV v2 secrets engine enabled at the `secret/` path.

Secrets are written to Vault using the Vault CLI or API:

```bash
# Write a secret to Vault
vault kv put secret/wash/mcp-jwt-credentials \
    JWT_PUBLIC_KEY="-----BEGIN PUBLIC KEY-----\nMIIBIjAN..." \
    JWT_ISSUER="Joken" \
    JWT_AUDIENCE="mcp-server"
```

This creates (or updates) a secret at the path `secret/data/wash/mcp-jwt-credentials` in Vault's KV v2 engine. Vault versions every write — each update creates a new version that can be audited, rolled back, or compared.

In production, secrets would be written by:
- CI/CD pipelines during deployment
- Human operators via Vault UI/CLI
- Automated rotation systems (Vault's own dynamic secrets, or external rotation scripts)
- The `PushSecret` CRD (ESO can also push from K8s → Vault, enabling reverse flows)

The key point: **Vault is the single source of truth.** You never write secrets directly to K8s — they flow through Vault first, and ESO handles the synchronization.

---

## How ESO Syncs Vault to K8s

The External Secrets Operator (ESO) bridges Vault and Kubernetes. It consists of three pieces:

### 1. ClusterSecretStore — The Connection to Vault

```yaml
# manifests/eso/cluster-secret-store.yaml
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

This is cluster-scoped (no namespace). It tells ESO: "Here's how to talk to Vault — use the Kubernetes auth method, authenticating as the `external-secrets` service account with the `eso-role` role." The Kubernetes auth method means ESO presents its K8s service account token to Vault, and Vault validates it against the K8s API server's token review endpoint. No static tokens stored anywhere.

### 2. ExternalSecret — What to Sync

```yaml
# manifests/eso/external-secret.yaml
apiVersion: external-secrets.io/v1
kind: ExternalSecret
metadata:
  name: mcp-jwt-credentials
  namespace: wasmcloud-system
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

This is namespace-scoped (lives in `wasmcloud-system`). It says: "Every 30 seconds, poll Vault at `secret/data/wash/mcp-jwt-credentials` and sync `JWT_PUBLIC_KEY`, `JWT_ISSUER`, and `JWT_AUDIENCE` into a K8s Secret named `mcp-jwt-credentials`."

Key details:
- **`refreshInterval: 30s`** — ESO polls Vault every 30 seconds. If the secret in Vault changes, the K8s Secret will be updated within 30 seconds.
- **`creationPolicy: Owner`** — ESO owns the K8s Secret. If the ExternalSecret is deleted, the K8s Secret is garbage collected.
- **`remoteRef.key`** — The path in Vault (without the `secret/data/` prefix — ESO adds that based on the store's `path: "secret"` and `version: "v2"`).
- **`remoteRef.property`** — The specific field within the Vault secret to extract.

### 3. The K8s Secret — What Gets Created

ESO creates and manages a standard K8s Secret:

```yaml
apiVersion: v1
kind: Secret
metadata:
  name: mcp-jwt-credentials
  namespace: wasmcloud-system
  # ESO adds annotations tracking sync status
  annotations:
    reconcile.external-secrets.io/data-hash: "..."
type: Opaque
data:
  JWT_PUBLIC_KEY: <base64-encoded>
  JWT_ISSUER: <base64-encoded>
  JWT_AUDIENCE: <base64-encoded>
```

This is a normal K8s Secret. The operator doesn't know or care that it was created by ESO — it just reads it like any other Secret. This decoupling is important: the operator's `ResolveSecretFrom()` works the same whether the Secret was created by ESO, by `kubectl create secret`, or by any other means.

### How ESO Detects Changes

ESO's reconciliation loop:

1. Every `refreshInterval` (30s), ESO reads the ExternalSecret CR
2. It authenticates to Vault using the ClusterSecretStore's auth config
3. It fetches the secret data from Vault at the specified path
4. It compares the fetched data hash against the `reconcile.external-secrets.io/data-hash` annotation on the existing K8s Secret
5. If the hashes differ, ESO updates the K8s Secret with the new values
6. It updates the ExternalSecret status to `SecretSynced=True` with a timestamp

You can verify the sync status:
```bash
kubectl get externalsecret -n wasmcloud-system
# NAME                  STORE           REFRESH   STATUS         READY
# mcp-jwt-credentials   vault-backend   30s       SecretSynced   True
```

---

## How the Operator Reads K8s Secrets

The wasmCloud runtime operator uses the `ConfigLayer` CRD to define how configuration is assembled for each component. A `ConfigLayer` has three sources:

```go
// api/runtime/v1alpha1/workload_types.go:52-66
type ConfigLayer struct {
    ConfigFrom []corev1.LocalObjectReference `json:"configFrom,omitempty"`
    SecretFrom []corev1.LocalObjectReference `json:"secretFrom,omitempty"`
    Config     map[string]string             `json:"config,omitempty"`
}
```

A workload manifest references secrets like this:

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
          config:                         # inline key-value pairs
            LOG_LEVEL: "debug"
          configFrom:                     # K8s ConfigMaps
            - name: mcp-common-config
          secretFrom:                     # K8s Secrets
            - name: mcp-jwt-credentials
```

When the operator reconciles this workload, the `MaterializeConfigLayer()` function in `utils.go` resolves all three sources and merges them:

```go
// utils.go:145-168
func MaterializeConfigLayer(ctx context.Context,
    kubeClient client.Client, namespace string, configLayer *ConfigLayer,
) (map[string]string, error) {
    ret := make(map[string]string)

    // 1. Start with inline config
    ret = MergeMaps(ret, configLayer.Config)

    // 2. Layer on ConfigMap values
    configs, err := ResolveConfigFrom(ctx, kubeClient, namespace, configLayer.ConfigFrom)
    ret = MergeMaps(ret, configs)

    // 3. Layer on Secret values (highest precedence)
    secrets, err := ResolveSecretFrom(ctx, kubeClient, namespace, configLayer.SecretFrom)
    ret = MergeMaps(ret, secrets)

    return ret, nil
}
```

**Merge precedence: `inline` < `configFrom` < `secretFrom`.**

If the same key exists in both a ConfigMap and a Secret, the Secret wins. This means you can have a "default" value in a ConfigMap that gets overridden by a Secret when it exists.

`ResolveSecretFrom()` reads K8s Secrets using the standard K8s client:

```go
// utils.go:69-81
func ResolveSecretFrom(ctx context.Context, kubeClient client.Client,
    namespace string, secretFrom []corev1.LocalObjectReference,
) (map[string]string, error) {
    secrets := make(map[string]string)
    for _, localRef := range secretFrom {
        var secret corev1.Secret
        if err := kubeClient.Get(ctx, client.ObjectKey{
            Namespace: namespace, Name: localRef.Name,
        }, &secret); err != nil {
            return nil, err
        }
        for key, value := range secret.Data {
            secrets[key] = string(value)  // base64-decoded by K8s client
        }
    }
    return secrets, nil
}
```

The operator's RBAC role must include permissions to read Secrets and ConfigMaps:

```yaml
# config/rbac/role.yaml (auto-generated from kubebuilder markers)
- apiGroups: [""]
  resources: ["configmaps"]
  verbs: ["get", "list", "watch"]
- apiGroups: [""]
  resources: ["secrets"]
  verbs: ["get", "list", "watch"]
```

---

## How the Runtime Delivers Config to Components

After the operator materializes the config, it sends it to the wash-runtime host over NATS. The config is included in the `WorkloadStartRequest`:

```
Operator ──NATS──► runtime.host.{hostID}.workload.start
                   {
                     workload: {
                       components: [{
                         local_resources: {
                           environment: {
                             "JWT_PUBLIC_KEY": "...",
                             "JWT_ISSUER": "Joken",
                             "JWT_AUDIENCE": "mcp-server",
                             "LOG_LEVEL": "debug"
                           }
                         }
                       }]
                     }
                   }
```

The runtime host receives this and does two things with the `environment` map:

### 1. Copies to `wasi:cli/environment` (static)

During workload store creation, the environment is baked into the `WasiCtx`:

```rust
// engine/workload.rs:943-955
wasi_ctx_builder.envs(
    metadata.local_resources.environment
        .iter()
        .map(|kv| (kv.0.as_str(), kv.1.as_str()))
        .collect::<Vec<_>>()
        .as_slice(),
);
```

This creates a snapshot. The `WasiCtx` owns its own copy. Changes to the original `HashMap` don't affect it.

### 2. Copies to `wasi:config/store` (dynamic)

During component binding, the `WasiConfig` plugin stores the same environment in its shared `Arc<RwLock<HashMap>>`:

```rust
// washlet/plugins/wasi_config.rs:98-103
self.config.write().await.insert(
    component_handle.id().to_string(),
    component_handle.local_resources().environment.clone(),
);
```

This is the key difference. The `Arc<RwLock<...>>` is shared by reference — every component invocation holds an `Arc` clone pointing to the same hashmap. When the WasiConfig plugin later receives an `update_config` call, it writes to this exact same hashmap, and all future `get()` calls see the new values.

---

## How Config Updates Without Restart

This is the critical innovation. The `WorkloadUpdateConfig` RPC allows the operator to push new configuration to a running workload without stopping or restarting it. Here's the complete chain:

### Step 1: Operator Sends the RPC

```go
// host_client.go
client := NewWashHostClient(bus, hostID)
resp, err := client.UpdateConfig(ctx, &runtimev2.WorkloadUpdateConfigRequest{
    WorkloadId:  workloadID,
    Environment: newMergedConfig,  // re-materialized from ConfigMaps + Secrets
})
```

The RPC travels over NATS on subject `runtime.host.{hostID}.workload.update_config`.

### Step 2: Runtime Host Receives the RPC

The NATS command dispatcher routes the message:

```rust
// washlet/mod.rs
"workload.update_config" => {
    let req: WorkloadUpdateConfigRequest = from_api(payload)?;
    let res = workload_update_config(host, req).await?;
    to_api(&res)
}
```

### Step 3: Host Iterates Components and Updates Plugins

```rust
// host/mod.rs:634-672
async fn workload_update_config(&self, request: WorkloadUpdateConfigRequest)
    -> anyhow::Result<WorkloadUpdateConfigResponse>
{
    let workloads = self.workloads.read().await;
    let Some(HostWorkload::Running(workload)) = workloads.get(&request.workload_id) else {
        bail!("workload not running or not found: {}", request.workload_id);
    };

    let components_lock = workload.components();
    let components = components_lock.read().await;
    for (component_id, component) in components.iter() {
        if let Some(plugins) = component.metadata().plugins() {
            for plugin in plugins.values() {
                plugin.update_config(component_id, request.environment.clone()).await?;
            }
        }
    }
    // ...
}
```

### Step 4: WasiConfig Plugin Writes to the Shared HashMap

```rust
// washlet/plugins/wasi_config.rs:108-119
async fn update_config(&self, component_id: &str, config: HashMap<String, String>)
    -> anyhow::Result<()>
{
    tracing::debug!(component_id, "hot-updating wasi:config/store");
    self.config.write().await
        .insert(component_id.to_string(), config);
    Ok(())
}
```

### Step 5: Component Sees New Values

The next time the component calls `wasi:config/store.get("JWT_PUBLIC_KEY")`, the plugin reads from the same `Arc<RwLock<HashMap>>` that was just updated. Because `get()` acquires a fresh read lock on every call, it always sees the latest data. There is no caching layer, no stale window. The update is visible immediately after the write lock is released.

```
Component call: store.get("JWT_PUBLIC_KEY")
    │
    ├─ plugin.config.read().await   ← acquires read lock on shared Arc<RwLock<HashMap>>
    ├─ config_guard.get(component_id)
    ├─ map.get("JWT_PUBLIC_KEY")    ← returns the NEW value
    └─ drops read lock
```

### What About In-Flight Requests?

If a component is in the middle of handling a request when the config updates:
- Any `get()` call **before** the update sees the old value
- Any `get()` call **after** the update sees the new value
- There is no transactional boundary — if a request calls `get("A")` then `get("B")`, it's possible for `A` to return the old value and `B` to return the new value if the update happened between the two calls

For most use cases (rotating JWT keys, updating API tokens), this is perfectly fine — the new key/token simply starts being used on the next call. If you need atomic multi-key updates, the component should call `get-all()` which returns a consistent snapshot under a single read lock.

---

## The Full Data Flow

Here's the complete journey of a secret from Vault to a Wasm component, including the hot update path:

```
┌─────────────────────────────────────────────────────────────────────┐
│  1. SECRET CREATION                                                 │
│                                                                     │
│  vault kv put secret/wash/mcp-jwt-credentials \                     │
│      JWT_PUBLIC_KEY="..." JWT_ISSUER="Joken" JWT_AUDIENCE="mcp"     │
│                                                                     │
│  Secret stored in Vault KV v2 (encrypted, versioned, audited)       │
└─────────────────────────────┬───────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────────┐
│  2. ESO SYNCHRONIZATION (every 30 seconds)                          │
│                                                                     │
│  ExternalSecret "mcp-jwt-credentials" in wasmcloud-system:          │
│    → Authenticates to Vault via K8s ServiceAccount token            │
│    → Reads secret/data/wash/mcp-jwt-credentials                     │
│    → Compares hash with existing K8s Secret                         │
│    → If changed: updates K8s Secret "mcp-jwt-credentials"           │
│    → Sets status: SecretSynced=True                                 │
└─────────────────────────────┬───────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────────┐
│  3. K8s SECRET (in wasmcloud-system namespace)                      │
│                                                                     │
│  apiVersion: v1                                                     │
│  kind: Secret                                                       │
│  metadata:                                                          │
│    name: mcp-jwt-credentials                                        │
│  data:                                                              │
│    JWT_PUBLIC_KEY: <base64>                                          │
│    JWT_ISSUER: <base64>                                              │
│    JWT_AUDIENCE: <base64>                                            │
└─────────────────────────────┬───────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────────┐
│  4. OPERATOR RECONCILIATION                                         │
│                                                                     │
│  WorkloadReconciler detects workload needs config:                   │
│    → MaterializeConfigLayer()                                       │
│        → MergeMaps(inline, configFrom, secretFrom)                  │
│        → ResolveSecretFrom(): reads K8s Secret, base64-decodes      │
│    → Result: { "JWT_PUBLIC_KEY": "...", "JWT_ISSUER": "Joken", ... }│
│                                                                     │
│  INITIAL DEPLOY: sends WorkloadStart RPC with config in environment │
│  CONFIG UPDATE:  sends WorkloadUpdateConfig RPC with new config     │
└─────────────────────────────┬───────────────────────────────────────┘
                              │ NATS RPC
                              │ Subject: runtime.host.{hostID}.workload.{start|update_config}
                              ▼
┌─────────────────────────────────────────────────────────────────────┐
│  5. WASH-RUNTIME HOST                                               │
│                                                                     │
│  INITIAL DEPLOY (WorkloadStart):                                    │
│    → Creates workload stores                                        │
│    → Copies environment to WasiCtx (static env vars)                │
│    → Copies environment to WasiConfig plugin (dynamic config)       │
│                                                                     │
│  CONFIG UPDATE (WorkloadUpdateConfig):                              │
│    → Looks up running workload by ID                                │
│    → Iterates all components                                        │
│    → Calls plugin.update_config(component_id, new_environment)      │
│    → WasiConfig plugin: writes to Arc<RwLock<HashMap>>              │
└─────────────────────────────┬───────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────────┐
│  6. WASM COMPONENT                                                  │
│                                                                     │
│  // In your component code:                                         │
│  let key = wasi::config::store::get("JWT_PUBLIC_KEY")?;             │
│  // Returns the LATEST value — updated in-place, no restart needed  │
│                                                                     │
│  // Also available (but NOT hot-updatable):                         │
│  let env = wasi::cli::environment::get_environment();               │
│  // Returns the ORIGINAL value from workload start time             │
└─────────────────────────────────────────────────────────────────────┘
```

---

## What Happens When a Secret Rotates in Vault

Let's walk through a concrete rotation scenario. Your JWT signing key is being rotated:

### Timeline

```
T+0s    Admin runs: vault kv put secret/wash/mcp-jwt-credentials JWT_PUBLIC_KEY="new-key-..."
        → Vault stores new version (v2) of the secret

T+0-30s ESO's next poll cycle hasn't fired yet.
        → K8s Secret still has the OLD value.
        → Component still reads the OLD key via wasi:config/store.

T+30s   ESO's refreshInterval (30s) fires.
        → ESO reads Vault, detects hash mismatch.
        → ESO updates K8s Secret "mcp-jwt-credentials" with new value.

T+30s+  Operator's reconcile loop detects the Secret change (future: via watch).
        → Re-runs MaterializeConfigLayer().
        → Calls WorkloadUpdateConfig RPC with new merged environment.
        → WasiConfig plugin updates its Arc<RwLock<HashMap>>.

T+30s+  Next component invocation calls store.get("JWT_PUBLIC_KEY").
        → Returns "new-key-..." — the rotated value.
        → NO RESTART. NO DOWNTIME. Component didn't even know it happened.
```

### Maximum Propagation Delay

- Vault → K8s Secret: up to `refreshInterval` (30 seconds default)
- K8s Secret → Component: depends on operator reconcile interval (30 seconds default)
- **Worst case: ~60 seconds** from Vault write to component seeing the new value
- **Best case: ~0 seconds** if the operator reconciles immediately after ESO syncs

You can tune `refreshInterval` lower (e.g., `10s`) for faster propagation at the cost of more Vault API calls.

### What DOESN'T Need a Restart

- Rotating a JWT key
- Updating an API token
- Changing a database connection string
- Toggling a feature flag
- Updating any value accessible via `wasi:config/store`

### What DOES Need a Restart

- Adding a brand new environment variable (if the component reads it via `wasi:cli/environment`)
- Changing the component binary itself
- Modifying volume mounts
- Changing pool size or max invocations

---

## Secrets vs Config: Where to Put What

| Data | K8s Resource | Vault? | Why |
|------|-------------|--------|-----|
| JWT public keys | Secret (via ESO) | Yes | Cryptographic material, must be rotatable |
| API keys/tokens | Secret (via ESO) | Yes | Sensitive credentials |
| Database URLs with passwords | Secret (via ESO) | Yes | Contains credentials |
| Feature flags | ConfigMap | No | Non-sensitive, low-risk |
| Log level | ConfigMap | No | Non-sensitive |
| Service endpoints (no auth) | ConfigMap | No | Non-sensitive URLs |
| TLS certificates | Secret (via ESO) | Yes | Cryptographic material |
| OAuth client secrets | Secret (via ESO) | Yes | Sensitive credentials |
| Runtime-mutable app data | N/A (wasi:keyvalue) | No | Component manages own data |

### The Rule of Thumb

- **Could it cause a security incident if leaked?** → Vault + ESO + K8s Secret + `secretFrom`
- **Is it just configuration?** → K8s ConfigMap + `configFrom`
- **Does the component itself need to read AND write it at runtime?** → `wasi:keyvalue` (NATS JetStream KV)

### Workload Manifest Example

```yaml
spec:
  components:
    - name: my-component
      image: ghcr.io/example/my-component:latest
      localResources:
        environment:
          # Static inline config (lowest precedence)
          config:
            LOG_LEVEL: "info"
            SERVICE_NAME: "my-component"

          # Non-sensitive config from ConfigMaps
          configFrom:
            - name: shared-feature-flags
            - name: service-endpoints

          # Sensitive secrets from K8s Secrets (highest precedence)
          # These Secrets are managed by ESO, backed by Vault
          secretFrom:
            - name: mcp-jwt-credentials
            - name: database-credentials
```

Precedence when keys overlap: `config` < `configFrom` < `secretFrom`.

---

## RBAC: What the Operator Needs

The operator must have RBAC permissions to read Secrets and ConfigMaps in the workload's namespace. Without these, `ResolveSecretFrom()` and `ResolveConfigFrom()` fail with 403 errors.

The required kubebuilder markers on `SetupWithManager()`:

```go
// +kubebuilder:rbac:groups="",resources=configmaps,verbs=get;list;watch
// +kubebuilder:rbac:groups="",resources=secrets,verbs=get;list;watch
```

These generate the following in `config/rbac/role.yaml`:

```yaml
- apiGroups: [""]
  resources: ["configmaps"]
  verbs: ["get", "list", "watch"]
- apiGroups: [""]
  resources: ["secrets"]
  verbs: ["get", "list", "watch"]
```

Note: the operator only needs `get`, `list`, `watch` — it never creates or modifies Secrets. ESO handles Secret lifecycle.

---

## Current Limitations and Future Work

### What's Implemented Today

1. **Vault + ESO integration** — Local dev environment with Kind, Vault (dev mode), ESO, ClusterSecretStore, and ExternalSecret. Secrets sync from Vault to K8s every 30s.

2. **`secretFrom` / `configFrom` in workload specs** — The operator reads K8s Secrets and ConfigMaps, merges them with inline config, and sends the merged environment to the runtime host.

3. **`WorkloadUpdateConfig` RPC** — The runtime host accepts this RPC and updates the `WasiConfig` plugin's shared `Arc<RwLock<HashMap>>` in-place. Components using `wasi:config/store.get()` see new values immediately.

4. **RBAC permissions** — The operator has the necessary permissions to read Secrets and ConfigMaps.

### What's NOT Implemented Yet

1. **Operator Secret/ConfigMap watch** — Today, the operator only calls `MaterializeConfigLayer()` during `reconcilePlacement()` (initial deployment). It does NOT watch for Secret/ConfigMap changes after the workload is placed. To trigger a config update, you'd need to either:
   - Manually trigger a reconcile (e.g., by annotating the workload)
   - Wait for the periodic reconcile interval (30s)
   - Implement a proper watch on referenced Secrets/ConfigMaps

2. **Operator-side `WorkloadUpdateConfig` call** — The `UpdateConfig` method exists on `WashHostClient` (the Go RPC client), but the operator's reconcile loop doesn't call it yet. The `reconcileConfig` condition early-returns if `workload.Status.HostID != ""` (already placed). A new reconcile condition is needed that:
   - Detects when referenced Secret/ConfigMap resourceVersions change
   - Re-materializes the config via `MaterializeConfigLayer()`
   - Calls `client.UpdateConfig()` with the new merged config

3. **Config version tracking** — No mechanism to compare current vs desired config state. The operator should store a hash of the last-sent config in the workload status, and only send `WorkloadUpdateConfig` if the hash has changed.

4. **Graceful degradation** — If the `WorkloadUpdateConfig` RPC fails (host unreachable, workload not running), there's no retry logic or fallback to restart.

### Recommended Next Steps

1. **Add a `reconcileConfigSync` condition** to the operator that runs after placement, watches for config changes, and calls `WorkloadUpdateConfig`.

2. **Add watches** on Secrets and ConfigMaps referenced by workloads, so the operator is notified immediately when ESO updates a Secret (rather than waiting for the periodic reconcile).

3. **Store config hash** in workload status to enable efficient change detection.

4. **Test the full rotation loop** — write a secret to Vault, wait for ESO sync, trigger operator reconcile, verify component sees new value via `wasi:config/store.get()`.
