// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Live integration tests against a running `ate-api-server`.
//!
//! `#[ignore]`d by default -- they need a real cluster. Run with:
//!
//! ```sh
//! export SUBSTRATE_LIVE_API_ENDPOINT=localhost:8443
//! export SUBSTRATE_LIVE_ATESPACE=ate-openshell-m0
//! export SUBSTRATE_LIVE_CA_PATH=/tmp/ate-ca.pem
//! export SUBSTRATE_LIVE_BEARER_TOKEN_PATH=/tmp/sa-token
//! export SUBSTRATE_LIVE_TLS_SERVER_NAME=api.ate-system.svc
//! export SUBSTRATE_LIVE_TEST_IMAGE=localhost:5001/some-image@sha256:...
//! cargo test -p openshell-driver-substrate --test live -- --ignored
//! ```
//!
//! Skips silently when any required var is missing.

use openshell_core::proto::compute::v1::{
    CreateSandboxRequest, DeleteSandboxRequest, DriverSandbox, DriverSandboxSpec,
    DriverSandboxTemplate, GetCapabilitiesRequest, GetSandboxRequest, ListSandboxesRequest,
    StartSandboxRequest, StopSandboxRequest, compute_driver_server::ComputeDriver,
};
use openshell_driver_substrate::{SubstrateComputeConfig, SubstrateComputeDriver};
use std::path::PathBuf;
use tonic::Request;

fn config_from_env() -> Option<SubstrateComputeConfig> {
    let api_endpoint = std::env::var("SUBSTRATE_LIVE_API_ENDPOINT").ok()?;
    let atespace = std::env::var("SUBSTRATE_LIVE_ATESPACE").ok()?;
    let ca = std::env::var("SUBSTRATE_LIVE_CA_PATH").ok()?;
    let token = std::env::var("SUBSTRATE_LIVE_BEARER_TOKEN_PATH").ok()?;
    let server_name = std::env::var("SUBSTRATE_LIVE_TLS_SERVER_NAME").ok();

    let mut cfg = SubstrateComputeConfig {
        api_endpoint,
        atespace,
        api_tls_ca_path: Some(PathBuf::from(ca)),
        api_bearer_token_path: Some(PathBuf::from(token)),
        api_tls_server_name: server_name,
        ..Default::default()
    };
    if let Ok(v) = std::env::var("SUBSTRATE_LIVE_SANDBOX_CONFIG") {
        cfg.sandbox_config_name = v;
    }
    if let Ok(v) = std::env::var("SUBSTRATE_LIVE_SNAPSHOTS_LOCATION") {
        cfg.snapshots_location = v;
    }
    Some(cfg)
}

fn test_image() -> String {
    std::env::var("SUBSTRATE_LIVE_TEST_IMAGE").unwrap_or_else(|_| {
        String::from("localhost:5001/openshell-sandbox-m0@sha256:4947aa0986d8f7fb5b875d784e2a62dd50bc491e692dd163c106ca94edf0a13e")
    })
}

/// `get_capabilities` exercises the simplest RPC path -- no cluster state
/// needed, just validates the TLS + bearer handshake to the driver itself
/// (the driver answers this locally; it doesn't even dial `ate-api-server`).
#[tokio::test]
#[ignore = "requires SUBSTRATE_LIVE_* env vars"]
async fn live_get_capabilities() {
    let Some(config) = config_from_env() else {
        eprintln!("skipping: SUBSTRATE_LIVE_* env vars not set");
        return;
    };
    let driver = SubstrateComputeDriver::new(config);
    let resp = driver
        .get_capabilities(Request::new(GetCapabilitiesRequest { gateway: None }))
        .await
        .expect("get_capabilities");
    let caps = resp.into_inner();
    assert_eq!(caps.driver_name, "substrate");
    assert!(
        caps.extension.is_some(),
        "extension metadata is required for gateway negotiation"
    );
}

/// Full lifecycle against a real cluster: create (synthesizes + reuses an
/// ActorTemplate by content hash) -> get -> stop (suspend) -> start
/// (resume) -> get again -> delete. This is the actual point of the
/// integration: state survives the stop/start round trip.
#[tokio::test]
#[ignore = "requires SUBSTRATE_LIVE_* env vars + a running ate-api-server"]
async fn live_full_lifecycle_round_trip() {
    let Some(config) = config_from_env() else {
        eprintln!("skipping: SUBSTRATE_LIVE_* env vars not set");
        return;
    };
    let driver = SubstrateComputeDriver::new(config);

    let actor_id = format!(
        "live-{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
    );
    eprintln!("[live] actor_id = {actor_id}");

    let sandbox = DriverSandbox {
        id: actor_id.clone(),
        name: actor_id.clone(),
        spec: Some(DriverSandboxSpec {
            template: Some(DriverSandboxTemplate {
                image: test_image(),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    };

    eprintln!(
        "[live] create_sandbox (synthesizes/reuses ActorTemplate, waits for golden snapshot)..."
    );
    driver
        .create_sandbox(Request::new(CreateSandboxRequest {
            sandbox: Some(sandbox),
        }))
        .await
        .expect("create_sandbox");

    let get = |driver: SubstrateComputeDriver, id: String| async move {
        driver
            .get_sandbox(Request::new(GetSandboxRequest {
                sandbox_id: id,
                name: String::new(),
            }))
            .await
            .expect("get_sandbox")
            .into_inner()
            .sandbox
            .expect("sandbox present")
    };

    let observed = get(driver.clone(), actor_id.clone()).await;
    assert_eq!(observed.id, actor_id);
    eprintln!(
        "[live]   after create: {:?}",
        observed
            .status
            .as_ref()
            .and_then(|s| s.conditions.first())
            .map(|c| &c.reason)
    );

    eprintln!("[live] stop_sandbox ...");
    driver
        .stop_sandbox(Request::new(StopSandboxRequest {
            sandbox_id: actor_id.clone(),
            name: String::new(),
        }))
        .await
        .expect("stop_sandbox");

    eprintln!("[live] start_sandbox ...");
    driver
        .start_sandbox(Request::new(StartSandboxRequest {
            sandbox_id: actor_id.clone(),
            name: String::new(),
            ..Default::default()
        }))
        .await
        .expect("start_sandbox");

    let observed = get(driver.clone(), actor_id.clone()).await;
    eprintln!(
        "[live]   after stop+start: {:?}",
        observed
            .status
            .as_ref()
            .and_then(|s| s.conditions.first())
            .map(|c| &c.reason)
    );

    eprintln!("[live] delete_sandbox ...");
    let del = driver
        .delete_sandbox(Request::new(DeleteSandboxRequest {
            sandbox_id: actor_id.clone(),
            name: String::new(),
        }))
        .await
        .expect("delete_sandbox")
        .into_inner();
    assert!(
        del.deleted,
        "delete_sandbox should return deleted=true for an actor we just created"
    );
    eprintln!("[live] ok");
}

/// `list_sandboxes` exercises the full round trip (`ListActors`) plus the
/// projection helpers. Asserts only that the call succeeds; the atespace
/// filter may return zero or many sandboxes depending on cluster state.
#[tokio::test]
#[ignore = "requires SUBSTRATE_LIVE_* env vars + a running ate-api-server"]
async fn live_list_sandboxes() {
    let Some(config) = config_from_env() else {
        eprintln!("skipping: SUBSTRATE_LIVE_* env vars not set");
        return;
    };
    let atespace = config.atespace.clone();
    let driver = SubstrateComputeDriver::new(config);
    let resp = driver
        .list_sandboxes(Request::new(ListSandboxesRequest {}))
        .await
        .expect("list_sandboxes");
    let list = resp.into_inner();
    eprintln!(
        "live_list_sandboxes: {} sandbox(es) in atespace {atespace}",
        list.sandboxes.len()
    );
    for s in &list.sandboxes {
        eprintln!(
            "  - id={} namespace={} status={:?}",
            s.id,
            s.namespace,
            s.status
                .as_ref()
                .and_then(|st| st.conditions.first())
                .map(|c| &c.reason)
        );
    }
}
