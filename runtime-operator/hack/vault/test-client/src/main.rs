use anyhow::{bail, Context, Result};
use base64::Engine;
use k8s_openapi::api::core::v1::Secret;
use kube::{
    api::{Api, Patch, PatchParams},
    Client,
};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const NAMESPACE: &str = "wasmcloud-system";
const SECRET_NAME: &str = "mcp-jwt-credentials";
const WRITABLE_SECRET_NAME: &str = "mcp-jwt-credentials-writable";
const COMPONENT_URL: &str = "http://config-echo.localhost.direct:8000/";
const POLL_INTERVAL: Duration = Duration::from_secs(5);
const POLL_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Deserialize)]
struct ConfigResponse {
    config: BTreeMap<String, String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    println!("=== Config Update E2E Test (PushSecret Flow) ===\n");

    let http = reqwest::Client::new();

    // Step 1: Read baseline config from the component
    println!("Step 1: Reading baseline config from component...");
    let baseline = fetch_config(&http).await
        .context("Failed to fetch baseline config. Is the config-echo workload running?")?;

    let baseline_issuer = baseline.config.get("JWT_ISSUER")
        .context("JWT_ISSUER not found in baseline config")?
        .clone();
    println!("  Baseline JWT_ISSUER: {baseline_issuer}");
    println!("  Baseline config keys: {:?}\n", baseline.config.keys().collect::<Vec<_>>());

    // Step 2: Patch K8s Secret with a new value (triggers PushSecret → Vault → ExternalSecret)
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let rotated_issuer = format!("RotatedIssuer-{timestamp}");

    println!("Step 2: Patching K8s Secret '{WRITABLE_SECRET_NAME}' in namespace '{NAMESPACE}'...");
    println!("  Setting JWT_ISSUER = {rotated_issuer}");
    patch_secret(&rotated_issuer).await
        .context("Failed to patch K8s Secret")?;
    println!("  Secret patched successfully.\n");

    // Step 3: Poll component until it sees the new value
    println!("Step 3: Polling component for updated config...");
    println!("  Flow: K8s Secret → PushSecret → Vault → ExternalSecret → K8s Secret → Operator → RPC → Component");
    println!("  Polling every {}s, timeout {}s\n", POLL_INTERVAL.as_secs(), POLL_TIMEOUT.as_secs());

    let start = std::time::Instant::now();
    loop {
        if start.elapsed() > POLL_TIMEOUT {
            bail!(
                "FAIL: Timed out after {}s. Component still reports JWT_ISSUER != '{rotated_issuer}'",
                POLL_TIMEOUT.as_secs()
            );
        }

        tokio::time::sleep(POLL_INTERVAL).await;

        match fetch_config(&http).await {
            Ok(resp) => {
                if let Some(current) = resp.config.get("JWT_ISSUER") {
                    let elapsed = start.elapsed().as_secs();
                    if current == &rotated_issuer {
                        println!("\n=== PASS ===");
                        println!("  Component saw updated config after {elapsed}s");
                        println!("  Before: {baseline_issuer}");
                        println!("  After:  {current}");
                        println!("  Hot update confirmed — no restart needed.");
                        return Ok(());
                    }
                    println!("  [{elapsed}s] JWT_ISSUER = {current} (waiting for: {rotated_issuer})");
                }
            }
            Err(e) => {
                let elapsed = start.elapsed().as_secs();
                println!("  [{elapsed}s] Fetch error: {e} (retrying...)");
            }
        }
    }
}

async fn fetch_config(http: &reqwest::Client) -> Result<ConfigResponse> {
    let resp = http
        .get(COMPONENT_URL)
        .timeout(Duration::from_secs(5))
        .send()
        .await?
        .json::<ConfigResponse>()
        .await?;
    Ok(resp)
}

async fn patch_secret(new_issuer: &str) -> Result<()> {
    let client = Client::try_default().await?;
    let secrets: Api<Secret> = Api::namespaced(client, NAMESPACE);

    let b64 = base64::engine::general_purpose::STANDARD;

    // Read the current values from the ESO-owned secret so we can
    // seed the writable copy with all existing keys.
    let current = secrets.get(SECRET_NAME).await?;
    let mut data = serde_json::Map::new();

    if let Some(current_data) = &current.data {
        for (key, value) in current_data {
            // Re-encode existing values
            data.insert(
                key.clone(),
                serde_json::Value::String(b64.encode(&value.0)),
            );
        }
    }

    // Override JWT_ISSUER
    data.insert(
        "JWT_ISSUER".to_string(),
        serde_json::Value::String(b64.encode(new_issuer)),
    );

    let patch = serde_json::json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {
            "name": WRITABLE_SECRET_NAME,
            "namespace": NAMESPACE,
        },
        "data": data
    });

    // Server-side apply the writable secret (creates if missing, updates if exists).
    // Flow: writable secret → PushSecret → Vault → ExternalSecret → mcp-jwt-credentials
    secrets
        .patch(
            WRITABLE_SECRET_NAME,
            &PatchParams::apply("config-update-test"),
            &Patch::Apply(&patch),
        )
        .await?;

    Ok(())
}
