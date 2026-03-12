use std::{collections::HashSet, env};

use wash_runtime::{
    host::HostConfig,
    oci::{self, OciConfig},
    washlet::image_pull_secret_to_oci_config,
};

#[tokio::main]
async fn main() {
    let insecure_registries = env::var("INSECURE_REGISTRIES")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect::<HashSet<_>>();

    let image = "ghcr.io/bettyblocks/http-wrapper:1.2.0";
    let image_pull_secret = None;

    let config = HostConfig::default();

    let oci_config = image_pull_secret_to_oci_config(&config, &image_pull_secret);
    let oci_config = OciConfig {
        insecure_registries: insecure_registries.clone(),
        ..oci_config
    };

    match oci::pull_component(image, oci_config).await {
        Ok(_bytes) => println!("Successfully pulled"),
        Err(e) => {
            eprintln!("failed to pull component image {}: {}", image, e);
        }
    };

    let image = "ghcr.io/bettyblocks/data-api:1.1.6";
    let image_pull_secret = None;

    let oci_config = image_pull_secret_to_oci_config(&config, &image_pull_secret);
    let oci_config = OciConfig {
        insecure_registries: insecure_registries.clone(),
        ..oci_config
    };

    match oci::pull_component(image, oci_config).await {
        Ok(_bytes) => println!("Successfully pulled"),
        Err(e) => {
            eprintln!("failed to pull component image {}: {}", image, e);
        }
    };

    let image = "registry.services.docker/979fcd1ebb354eecb0f8b02b94b4eb62_custom:1.132.0-2025-11-28t12-39-40-474050z";
    // let image = "registry.services.docker/979fcd1ebb354eecb0f8b02b94b4eb62_custom:1.132.0-2025-11-27t15-00-14-343415z";
    let image_pull_secret = None;

    let oci_config = image_pull_secret_to_oci_config(&config, &image_pull_secret);
    let oci_config = OciConfig {
        insecure_registries: insecure_registries.clone(),
        ..oci_config
    };

    match oci::pull_component(image, oci_config).await {
        Ok(_bytes) => println!("Successfully pulled"),
        Err(e) => {
            eprintln!("failed to pull component image {}: {}", image, e);
        }
    };

    let image = "bettywasmregistryedge.azurecr.io/11a8e0229901401da254765224ce9c44_http_hello_world:1.106.1-2025-05-26t14-21-08-566208z";
    // let image = "registry.services.docker/979fcd1ebb354eecb0f8b02b94b4eb62_custom:1.132.0-2025-11-27t15-00-14-343415z";
    let image_pull_secret = None;

    let oci_config = image_pull_secret_to_oci_config(&config, &image_pull_secret);
    let oci_config = OciConfig {
        insecure_registries,
        ..oci_config
    };

    match oci::pull_component(image, oci_config).await {
        Ok(_bytes) => println!("Successfully pulled"),
        Err(e) => {
            eprintln!("failed to pull component image {}: {}", image, e);
        }
    };
}
