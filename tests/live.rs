// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Lifecycle test against a running `ate-api-server`. Ignored by default.
//!
//! ```sh
//! export SUBSTRATE_LIVE_API_ENDPOINT=127.0.0.1:8443
//! export SUBSTRATE_LIVE_ATESPACE=ate-openshell-microvm
//! export SUBSTRATE_LIVE_CA_PATH=creds/ctb.crt
//! export SUBSTRATE_LIVE_BEARER_TOKEN_PATH=creds/token
//! export SUBSTRATE_LIVE_TLS_SERVER_NAME=api.ate-system.svc
//! export SUBSTRATE_LIVE_SNAPSHOTS_LOCATION=gs://ate-snapshots/ate-openshell-microvm/
//! export SUBSTRATE_LIVE_TEST_IMAGE=<SANDBOX_BAKED_IMAGE>
//! cargo test --test live -- --ignored
//! ```
//!
//! Skips when any variable is missing.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use openshell_core::proto::compute::v1::{
    CreateSandboxRequest, DeleteSandboxRequest, DriverSandbox, DriverSandboxSpec,
    DriverSandboxTemplate, GetCapabilitiesRequest, GetSandboxRequest, ListSandboxesRequest,
    StartSandboxRequest, StopSandboxRequest, compute_driver_server::ComputeDriver,
};
use openshell_driver_substrate::{SubstrateComputeConfig, SubstrateComputeDriver};
use tonic::Request;

struct Live {
    config: SubstrateComputeConfig,
    image: String,
}

fn live() -> Option<Live> {
    let var = |k: &str| std::env::var(k).ok();
    let config = SubstrateComputeConfig {
        api_endpoint: var("SUBSTRATE_LIVE_API_ENDPOINT")?,
        atespace: var("SUBSTRATE_LIVE_ATESPACE")?,
        api_tls_ca_path: Some(PathBuf::from(var("SUBSTRATE_LIVE_CA_PATH")?)),
        api_bearer_token_path: Some(PathBuf::from(var("SUBSTRATE_LIVE_BEARER_TOKEN_PATH")?)),
        api_tls_server_name: var("SUBSTRATE_LIVE_TLS_SERVER_NAME"),
        snapshots_location: var("SUBSTRATE_LIVE_SNAPSHOTS_LOCATION")?,
        ..SubstrateComputeConfig::default()
    };
    Some(Live {
        config,
        image: var("SUBSTRATE_LIVE_TEST_IMAGE")?,
    })
}

/// True once `get_sandbox` reports a condition of `kind` with status `True`.
async fn wait_for(driver: &SubstrateComputeDriver, id: &str, kind: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(90);
    while Instant::now() < deadline {
        let sandbox = driver
            .get_sandbox(Request::new(GetSandboxRequest {
                sandbox_id: id.to_string(),
                name: String::new(),
            }))
            .await
            .expect("get_sandbox")
            .into_inner()
            .sandbox
            .expect("sandbox present");
        let conditions = sandbox.status.map(|s| s.conditions).unwrap_or_default();
        if conditions
            .iter()
            .any(|c| c.r#type == kind && c.status == "True")
        {
            return true;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    false
}

#[tokio::test]
#[ignore = "needs SUBSTRATE_LIVE_* and a running ate-api-server"]
async fn capabilities() {
    let Some(live) = live() else { return };
    let caps = SubstrateComputeDriver::new(live.config)
        .get_capabilities(Request::new(GetCapabilitiesRequest { gateway: None }))
        .await
        .expect("get_capabilities")
        .into_inner();
    assert_eq!(caps.driver_name, "substrate");
    assert!(caps.extension.is_some());
    assert!(caps.resource_admission_policy.starts_with("v1:"));
}

/// create -> Ready -> stop -> Suspended -> start -> Ready -> listed -> deleted.
#[tokio::test]
#[ignore = "needs SUBSTRATE_LIVE_* and a running ate-api-server"]
async fn lifecycle() {
    let Some(live) = live() else { return };
    let driver = SubstrateComputeDriver::new(live.config);
    let id = format!(
        "live-{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
    );

    driver
        .create_sandbox(Request::new(CreateSandboxRequest {
            sandbox: Some(DriverSandbox {
                id: id.clone(),
                name: id.clone(),
                spec: Some(DriverSandboxSpec {
                    template: Some(DriverSandboxTemplate {
                        image: live.image,
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            }),
        }))
        .await
        .expect("create_sandbox");
    assert!(
        wait_for(&driver, &id, "Ready").await,
        "{id}: not Ready after create"
    );

    driver
        .stop_sandbox(Request::new(StopSandboxRequest {
            sandbox_id: id.clone(),
            name: String::new(),
        }))
        .await
        .expect("stop_sandbox");
    assert!(
        wait_for(&driver, &id, "Suspended").await,
        "{id}: not Suspended after stop"
    );

    driver
        .start_sandbox(Request::new(StartSandboxRequest {
            sandbox_id: id.clone(),
            name: String::new(),
            ..Default::default()
        }))
        .await
        .expect("start_sandbox");
    assert!(
        wait_for(&driver, &id, "Ready").await,
        "{id}: not Ready after start"
    );

    let listed = driver
        .list_sandboxes(Request::new(ListSandboxesRequest {}))
        .await
        .expect("list_sandboxes")
        .into_inner()
        .sandboxes;
    assert!(listed.iter().any(|s| s.id == id), "{id}: not listed");

    let deleted = driver
        .delete_sandbox(Request::new(DeleteSandboxRequest {
            sandbox_id: id.clone(),
            name: String::new(),
        }))
        .await
        .expect("delete_sandbox")
        .into_inner()
        .deleted;
    assert!(deleted);
}
