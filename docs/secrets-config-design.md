# Secrets & Configuration Design

## Problem

Wasm components need access to configuration (MCP server definitions, feature flags) and secrets (JWT keys, API keys). Today:

1. Betty configurations live in Redis — we need to remove that dependency
2. There's no vault-backed secret store integration
3. Config/secrets can't be updated without restarting workloads
4. No clear separation between sensitive and non-sensitive configuration

## Architecture

### Storage Layers

```
┌─────────────────────────────────────────────────┐
│                 Source of Truth                  │
│  ┌──────────────┐  ┌────────────────────────┐   │
│  │  Vault KV    │  │  K8s ConfigMap (etcd)  │   │
│  │  (encrypted) │  │  (plaintext)           │   │
│  └──────┬───────┘  └──────────┬─────────────┘   │
│         │                     │                  │
│  ┌──────▼───────┐             │                  │
│  │  ESO         │             │                  │
│  │  ExternalSecret            │                  │
│  │  (30s poll)  │             │                  │
│  └──────┬───────┘             │                  │
│         │                     │                  │
│  ┌──────▼───────┐             │                  │
│  │  K8s Secret  │             │                  │
│  └──────┬───────┘             │                  │
└─────────┼─────────────────────┼─────────────────┘
          │                     │
          ▼                     ▼
┌─────────────────────────────────────────────────┐
│           Runtime Operator (K8s)                │
│  WorkloadReconciler                             │
│    secretFrom ──► ResolveSecretFrom()           │
│    configFrom ──► ResolveConfigFrom()           │
│    inline     ──► MergeMaps()                   │
│                                                 │
│  Merge order: inline < configFrom < secretFrom  │
│                                                 │
│  On change: WorkloadUpdateConfig RPC ──►        │
└───────────────────────────────┬─────────────────┘
                                │ NATS RPC
                                ▼
┌─────────────────────────────────────────────────┐
│           wash-runtime Host                     │
│                                                 │
│  WorkloadUpdateConfig handler:                  │
│    HostWorkload::Running(ResolvedWorkload)      │
│      └─► components: Arc<RwLock<HashMap>>       │
│            └─► plugins["wasi-config"]           │
│                  └─► update_config(id, map)     │
│                        └─► Arc<RwLock<HashMap>> │
│                              .write()           │
│                              .insert(id, map)   │
└───────────────────────────────┬─────────────────┘
                                │
                                ▼
┌─────────────────────────────────────────────────┐
│           Wasm Component                        │
│                                                 │
│  wasi:config/store.get("JWT_PUBLIC_KEY")        │
│    └─► reads from shared Arc<RwLock<HashMap>>   │
│    └─► sees updated value immediately           │
│                                                 │
│  wasi:cli/environment (env vars)                │
│    └─► baked into WasiCtx at store creation     │
│    └─► static per invocation, NOT hot-updatable │
│                                                 │
│  wasi:keyvalue (NATS JetStream KV)              │
│    └─► runtime mutable by component itself      │
│    └─► Redis replacement for Betty configs      │
└─────────────────────────────────────────────────┘
```

### Config Type Classification

| Config type | Mutable at runtime? | Storage | K8s resource | Component interface | Hot-updatable? |
|-------------|---------------------|---------|--------------|---------------------|----------------|
| Non-sensitive config | No (deployment-time) | etcd | ConfigMap | `wasi:config/store` | Yes (via WorkloadUpdateConfig RPC) |
| Secrets | No (deployment-time) | Vault → ESO → K8s Secret | Secret | `wasi:config/store` | Yes (via WorkloadUpdateConfig RPC) |
| Auth profiles (static) | No | Vault → K8s Secret | Secret | `wasi:config/store` | Yes |
| Betty builder configs | **Yes** (runtime) | NATS JetStream KV | N/A | `wasi:keyvalue` | Yes (runtime read/write) |
| User session data | **Yes** (runtime) | NATS JetStream KV | N/A | `wasi:keyvalue` | Yes |

### Component Interface Guidelines

- **`wasi:config/store.get(key)`** — For deployment-time configuration injected by the operator. Read-only from the component's perspective. Backed by `Arc<RwLock<HashMap>>` — supports hot updates via `WorkloadUpdateConfig` RPC without workload restart.

- **`wasi:cli/environment`** (env vars) — Static per invocation. Baked into `WasiCtx` at store creation. Cannot be hot-updated. Avoid for secrets that need rotation.

- **`wasi:keyvalue`** — For runtime-mutable data. Components can read AND write. Backed by NATS JetStream KV in the washlet path. This is the Redis replacement for Betty configurations.

### K8s Operator Side: How Updates Flow

```
                    ┌──────────────────────┐
                    │ External trigger:    │
                    │ • Vault secret       │
                    │   updated            │
                    │ • ConfigMap edited   │
                    │   via kubectl/API    │
                    └──────────┬───────────┘
                               │
                    ┌──────────▼───────────┐
                    │ ESO ExternalSecret   │
                    │ refreshInterval: 30s │
                    │ Syncs Vault → K8s    │
                    │ Secret               │
                    └──────────┬───────────┘
                               │
                    ┌──────────▼───────────┐
                    │ K8s Secret/ConfigMap │
                    │ updated in-cluster   │
                    └──────────┬───────────┘
                               │
                    ┌──────────▼───────────┐
                    │ Operator watches     │
                    │ Secret & ConfigMap   │
                    │ changes (future)     │
                    │                      │
                    │ Detects version      │
                    │ change via           │
                    │ resourceVersion hash │
                    └──────────┬───────────┘
                               │
                    ┌──────────▼───────────┐
                    │ Re-materializes      │
                    │ config via           │
                    │ MaterializeConfigLayer│
                    └──────────┬───────────┘
                               │
              ┌────────────────┴────────────────┐
              │                                 │
   ┌──────────▼──────────┐          ┌──────────▼──────────┐
   │ WorkloadUpdateConfig│          │ WorkloadStop +      │
   │ RPC (hot update)    │          │ WorkloadStart       │
   │                     │          │ (restart, fallback) │
   │ Updates             │          │                     │
   │ wasi:config/store   │          │ Updates everything  │
   │ only                │          │ including env vars  │
   └─────────────────────┘          └─────────────────────┘
```

The operator can choose between:
1. **Hot update** (`WorkloadUpdateConfig` RPC) — updates `wasi:config/store` without restart. Components see new values on next `get()` call.
2. **Restart** (stop + start) — updates everything including `wasi:cli/environment`. Required if env vars need to change.

### Implementation: WorkloadUpdateConfig RPC

**Proto** (`workload_service.proto`):
```protobuf
service WorkloadService {
  rpc WorkloadStart(WorkloadStartRequest) returns (WorkloadStartResponse);
  rpc WorkloadStatus(WorkloadStatusRequest) returns (WorkloadStatusResponse);
  rpc WorkloadStop(WorkloadStopRequest) returns (WorkloadStopResponse);
  rpc WorkloadUpdateConfig(WorkloadUpdateConfigRequest) returns (WorkloadUpdateConfigResponse);
}

message WorkloadUpdateConfigRequest {
  string workload_id = 1;
  map<string, string> environment = 2;  // merged config+secrets
}

message WorkloadUpdateConfigResponse {
  WorkloadStatus workload_status = 1;
}
```

**Runtime** (`host/mod.rs`):
- Look up `HostWorkload::Running(ResolvedWorkload)` by workload_id
- Iterate all components via `ResolvedWorkload.components()`
- For each component, call `plugin.update_config(component_id, new_config)` on the `wasi-config` plugin
- The `update_config` method writes to the shared `Arc<RwLock<HashMap>>`, making new values visible to all future `get()` calls

**HostPlugin trait** (`plugin/mod.rs`):
```rust
async fn update_config(
    &self,
    _component_id: &str,
    _config: HashMap<String, String>,
) -> anyhow::Result<()> {
    Ok(())  // default no-op
}
```

**WasiConfig override** (`plugin/wasi_config.rs` and `washlet/plugins/wasi_config.rs`):
```rust
async fn update_config(
    &self,
    component_id: &str,
    config: HashMap<String, String>,
) -> anyhow::Result<()> {
    self.config.write().await.insert(component_id.into(), config);
    Ok(())
}
```

### Local Development Setup

See `runtime-operator/hack/vault/README.md` for the local Kind + Vault + ESO setup.

### Key Files

| File | Role |
|------|------|
| `crates/wash-runtime/src/plugin/mod.rs` | `HostPlugin` trait with `update_config` |
| `crates/wash-runtime/src/plugin/wasi_config.rs` | Standalone WasiConfig with `Arc<RwLock<HashMap>>` |
| `crates/wash-runtime/src/washlet/plugins/wasi_config.rs` | NATS-connected WasiConfig |
| `crates/wash-runtime/src/host/mod.rs` | `HostApi` trait + `Host` implementation |
| `crates/wash-runtime/src/washlet/mod.rs` | NATS command handler (`handle_command`) |
| `crates/wash-runtime/src/types.rs` | Request/response types |
| `proto/wasmcloud/runtime/v2/workload_service.proto` | RPC definitions |
| `runtime-operator/internal/controller/runtime/workload_controller.go` | K8s reconciler |
| `runtime-operator/internal/controller/runtime/host_client.go` | Operator → host RPC client |
| `runtime-operator/internal/controller/runtime/utils.go` | `MaterializeConfigLayer`, `ResolveSecretFrom` |
