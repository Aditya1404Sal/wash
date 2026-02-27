use anyhow::{bail, Context, Result};
use base64::Engine;
use k8s_openapi::api::core::v1::{ConfigMap, Secret};
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
const CONFIGMAP_NAME: &str = "mcp-servers-config";
const COMPONENT_URL: &str = "http://config-echo.localhost.direct:8000/";
const POLL_INTERVAL: Duration = Duration::from_secs(5);
const POLL_TIMEOUT_SECRET: Duration = Duration::from_secs(180);
const POLL_TIMEOUT_CONFIGMAP: Duration = Duration::from_secs(60);

#[derive(Deserialize)]
struct ConfigResponse {
    config: BTreeMap<String, String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let http = reqwest::Client::new();
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();

    // ── Test 1: Secret rotation via Vault/ESO ──
    println!("=== Test 1: Secret Hot-Update (via Vault/ESO) ===\n");

    let baseline = fetch_config(&http)
        .await
        .context("Failed to fetch baseline config")?;
    let baseline_issuer = baseline
        .config
        .get("JWT_ISSUER")
        .context("JWT_ISSUER not found in baseline config")?
        .clone();
    println!("Baseline JWT_ISSUER: {baseline_issuer}");

    let rotated_issuer = format!("RotatedIssuer-{timestamp}");
    println!("Patching secret, setting JWT_ISSUER = {rotated_issuer}");
    patch_secret(&rotated_issuer).await?;

    let secret_elapsed =
        poll_for_config_value(&http, "JWT_ISSUER", &rotated_issuer, POLL_TIMEOUT_SECRET).await?;
    println!("\nTest 1: PASS ({secret_elapsed}s) — {baseline_issuer} → {rotated_issuer}\n");

    // ── Test 2: ConfigMap rotation (direct) ──
    println!("=== Test 2: ConfigMap Hot-Update (direct) ===\n");

    let current_config = fetch_config(&http).await?;
    let baseline_region = current_config
        .config
        .get("REGION")
        .context("REGION not found — is mcp-servers-config ConfigMap applied?")?
        .clone();
    println!("Baseline REGION: {baseline_region}");

    let new_region = format!("eu-central-{timestamp}");
    println!("Patching configmap, setting REGION = {new_region}");
    patch_configmap(&new_region).await?;

    let cm_elapsed =
        poll_for_config_value(&http, "REGION", &new_region, POLL_TIMEOUT_CONFIGMAP).await?;
    println!("\nTest 2: PASS ({cm_elapsed}s) — {baseline_region} → {new_region}\n");

    // ── Summary ──
    println!("All tests PASSED");
    println!("  Secret:    {secret_elapsed}s (via Vault/ESO)");
    println!("  ConfigMap: {cm_elapsed}s (direct)");

    Ok(())
}

async fn poll_for_config_value(
    http: &reqwest::Client,
    key: &str,
    expected: &str,
    timeout: Duration,
) -> Result<u64> {
    let start = std::time::Instant::now();
    loop {
        if start.elapsed() > timeout {
            bail!(
                "Timed out after {}s waiting for {key} = '{expected}'",
                timeout.as_secs()
            );
        }
        tokio::time::sleep(POLL_INTERVAL).await;
        match fetch_config(http).await {
            Ok(resp) => {
                if let Some(current) = resp.config.get(key) {
                    let elapsed = start.elapsed().as_secs();
                    if current == expected {
                        return Ok(elapsed);
                    }
                    println!("  [{elapsed}s] {key} = {current}");
                }
            }
            Err(e) => println!("  [{}s] error: {e}", start.elapsed().as_secs()),
        }
    }
}

async fn fetch_config(http: &reqwest::Client) -> Result<ConfigResponse> {
    Ok(http
        .get(COMPONENT_URL)
        .timeout(Duration::from_secs(5))
        .send()
        .await?
        .json()
        .await?)
}

async fn patch_secret(new_issuer: &str) -> Result<()> {
    let client = Client::try_default().await?;
    let secrets: Api<Secret> = Api::namespaced(client, NAMESPACE);
    let b64 = base64::engine::general_purpose::STANDARD;

    let current = secrets.get(SECRET_NAME).await?;
    let mut data = serde_json::Map::new();
    if let Some(current_data) = &current.data {
        for (key, value) in current_data {
            data.insert(key.clone(), serde_json::Value::String(b64.encode(&value.0)));
        }
    }
    data.insert(
        "JWT_ISSUER".to_string(),
        serde_json::Value::String(b64.encode(new_issuer)),
    );

    let patch = serde_json::json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": { "name": WRITABLE_SECRET_NAME, "namespace": NAMESPACE },
        "data": data
    });

    secrets
        .patch(
            WRITABLE_SECRET_NAME,
            &PatchParams::apply("config-update-test"),
            &Patch::Apply(&patch),
        )
        .await?;
    Ok(())
}

async fn patch_configmap(new_region: &str) -> Result<()> {
    let client = Client::try_default().await?;
    let configmaps: Api<ConfigMap> = Api::namespaced(client, NAMESPACE);

    let patch = serde_json::json!({
        "apiVersion": "v1",
        "kind": "ConfigMap",
        "metadata": { "name": CONFIGMAP_NAME, "namespace": NAMESPACE },
        "data": { "REGION": new_region }
    });

    configmaps
        .patch(
            CONFIGMAP_NAME,
            &PatchParams::apply("config-update-test").force(),
            &Patch::Apply(&patch),
        )
        .await?;
    Ok(())
}
