// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Live integration tests against a running `ate-api-server`.
//!
//! These tests are `#[ignore]`d by default because they require an
//! external cluster. Run them explicitly with:
//!
//! ```sh
//! export SUBSTRATE_LIVE_API_ENDPOINT=localhost:8443
//! export SUBSTRATE_LIVE_NAMESPACE=ate-openshell-m0
//! export SUBSTRATE_LIVE_CA_PATH=/tmp/ate-ca.pem
//! export SUBSTRATE_LIVE_BEARER_TOKEN_PATH=/tmp/sa-token
//! export SUBSTRATE_LIVE_TLS_SERVER_NAME=api.ate-system.svc
//! cargo test -p openshell-driver-substrate --test live -- --ignored
//! ```
//!
//! Required env vars:
//!
//! - `SUBSTRATE_LIVE_API_ENDPOINT`: `host:port` of the
//!   `ate-api-server`. Typically a `kubectl port-forward` target.
//! - `SUBSTRATE_LIVE_NAMESPACE`: ActorTemplate namespace the driver
//!   filters by.
//! - `SUBSTRATE_LIVE_CA_PATH`: path to the CA bundle that signs the
//!   api-server's TLS cert (e.g. the `servicedns.podcert.ate.dev`
//!   signer's root).
//! - `SUBSTRATE_LIVE_BEARER_TOKEN_PATH`: path to a JWT minted with
//!   `kubectl create token <sa> --audience=api.ate-system.svc`.
//! - `SUBSTRATE_LIVE_TLS_SERVER_NAME` (optional): SAN to verify
//!   against the server cert. Defaults to the host portion of
//!   `SUBSTRATE_LIVE_API_ENDPOINT`.
//!
//! Skips silently when any required var is missing.

use openshell_core::proto::compute::v1::{
    CreateSandboxRequest, DeleteSandboxRequest, DriverSandbox, DriverSandboxSpec,
    DriverSandboxTemplate, GetCapabilitiesRequest, GetSandboxRequest, ListSandboxesRequest,
    StopSandboxRequest, compute_driver_server::ComputeDriver,
};
use openshell_driver_substrate::{SubstrateComputeConfig, SubstrateComputeDriver};
use prost_types::value::Kind;
use prost_types::{Struct, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;
use tonic::Request;

fn config_from_env() -> Option<SubstrateComputeConfig> {
    let api_endpoint = std::env::var("SUBSTRATE_LIVE_API_ENDPOINT").ok()?;
    let default_namespace = std::env::var("SUBSTRATE_LIVE_NAMESPACE").ok()?;
    let ca = std::env::var("SUBSTRATE_LIVE_CA_PATH").ok()?;
    let token = std::env::var("SUBSTRATE_LIVE_BEARER_TOKEN_PATH").ok()?;
    let server_name = std::env::var("SUBSTRATE_LIVE_TLS_SERVER_NAME").ok();

    // Optional overrides for the synthesized-template path. Default
    // values are the driver-side defaults; tests that exercise the
    // synthesize-and-apply flow set these to match the cluster's
    // existing WorkerPool and snapshot bucket.
    let default_worker_pool = std::env::var("SUBSTRATE_LIVE_WORKER_POOL").ok();
    let snapshots_location = std::env::var("SUBSTRATE_LIVE_SNAPSHOTS_LOCATION").ok();
    let runsc_amd64_sha = std::env::var("SUBSTRATE_LIVE_RUNSC_AMD64_SHA").ok();
    let runsc_amd64_url = std::env::var("SUBSTRATE_LIVE_RUNSC_AMD64_URL").ok();
    let pause_image = std::env::var("SUBSTRATE_LIVE_PAUSE_IMAGE").ok();

    let mut cfg = SubstrateComputeConfig {
        api_endpoint,
        default_namespace,
        api_tls_ca_path: Some(PathBuf::from(ca)),
        api_bearer_token_path: Some(PathBuf::from(token)),
        api_tls_server_name: server_name,
        ..Default::default()
    };
    if let Some(v) = default_worker_pool {
        cfg.default_worker_pool = v;
    }
    if let Some(v) = snapshots_location {
        cfg.snapshots_location = v;
    }
    if let Some(v) = runsc_amd64_sha {
        cfg.runsc_amd64_sha256 = v;
    }
    if let Some(v) = runsc_amd64_url {
        cfg.runsc_amd64_url = v;
    }
    if let Some(v) = pause_image {
        cfg.pause_image = v;
    }
    Some(cfg)
}

/// `get_capabilities` exercises the simplest RPC path. Validates that
/// TLS + bearer auth handshake succeeds end-to-end without needing
/// any preexisting cluster state.
#[tokio::test]
#[ignore = "requires SUBSTRATE_LIVE_* env vars + a running ate-api-server"]
async fn live_get_capabilities() {
    let Some(config) = config_from_env() else {
        eprintln!("skipping: SUBSTRATE_LIVE_* env vars not set");
        return;
    };
    let driver = SubstrateComputeDriver::new(config);
    let resp = driver
        .get_capabilities(Request::new(GetCapabilitiesRequest {}))
        .await
        .expect("get_capabilities");
    let caps = resp.into_inner();
    assert_eq!(caps.driver_name, "substrate");
    assert!(
        !caps.driver_version.is_empty(),
        "driver_version is the crate's package version, never empty"
    );
}

/// Full write path: create -> get -> stop -> delete against a real
/// cluster, using a pre-provisioned `supervisor` ActorTemplate so the
/// test does not need CR-apply RBAC.
#[tokio::test]
#[ignore = "requires SUBSTRATE_LIVE_* env vars + a running ate-api-server"]
async fn live_write_path_round_trip() {
    let Some(config) = config_from_env() else {
        eprintln!("skipping: SUBSTRATE_LIVE_* env vars not set");
        return;
    };
    let template_name =
        std::env::var("SUBSTRATE_LIVE_TEMPLATE_NAME").unwrap_or_else(|_| "supervisor".to_string());
    let driver = SubstrateComputeDriver::new(config.clone());

    // Build a sandbox whose platform_config selects the pre-provisioned
    // template (skips the kube-rs apply path so this test does not need
    // ActorTemplate RBAC).
    let actor_id = format!(
        "live-write-{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
    );
    eprintln!("[live_write] actor_id = {actor_id}");
    eprintln!("[live_write] template  = {template_name}");

    let mut platform_fields = BTreeMap::new();
    platform_fields.insert(
        "substrate_actor_template".to_string(),
        Value {
            kind: Some(Kind::StringValue(template_name)),
        },
    );
    let sandbox = DriverSandbox {
        id: actor_id.clone(),
        name: actor_id.clone(),
        namespace: config.default_namespace.clone(),
        spec: Some(DriverSandboxSpec {
            template: Some(DriverSandboxTemplate {
                // Digest-pinned; validate_sandbox_create rejects bare tags.
                image: std::env::var("SUBSTRATE_LIVE_TEST_IMAGE").unwrap_or_else(|_| {
                    String::from(
                        "localhost:5001/openshell-sandbox-m0@sha256:4947aa0986d8f7fb5b875d784e2a62dd50bc491e692dd163c106ca94edf0a13e",
                    )
                }),
                platform_config: Some(Struct {
                    fields: platform_fields,
                }),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    };

    // 1. Create + resume.
    eprintln!("[live_write] create_sandbox ...");
    driver
        .create_sandbox(Request::new(CreateSandboxRequest {
            sandbox: Some(sandbox.clone()),
        }))
        .await
        .expect("create_sandbox");
    eprintln!("[live_write]   STATUS_RUNNING expected");

    // 2. get_sandbox round-trip.
    eprintln!("[live_write] get_sandbox ...");
    let got = driver
        .get_sandbox(Request::new(GetSandboxRequest {
            sandbox_id: actor_id.clone(),
            sandbox_name: String::new(),
        }))
        .await
        .expect("get_sandbox")
        .into_inner();
    let observed = got.sandbox.expect("got sandbox");
    assert_eq!(observed.id, actor_id);
    let status_reason = observed
        .status
        .as_ref()
        .and_then(|st| st.conditions.first())
        .map(|c| c.reason.clone())
        .unwrap_or_default();
    eprintln!("[live_write]   observed status reason = {status_reason}");

    // 3. Suspend.
    eprintln!("[live_write] stop_sandbox ...");
    driver
        .stop_sandbox(Request::new(StopSandboxRequest {
            sandbox_id: actor_id.clone(),
            sandbox_name: String::new(),
        }))
        .await
        .expect("stop_sandbox");

    // 4. Delete (cleanup).
    eprintln!("[live_write] delete_sandbox ...");
    let del = driver
        .delete_sandbox(Request::new(DeleteSandboxRequest {
            sandbox_id: actor_id.clone(),
            sandbox_name: String::new(),
        }))
        .await
        .expect("delete_sandbox")
        .into_inner();
    assert!(
        del.deleted,
        "delete_sandbox should return deleted=true for an actor we just created"
    );
    eprintln!("[live_write] ok");
}

/// Synthesized-template path: the driver applies an ActorTemplate CR
/// via kube-rs, waits for `Ready`, then creates + resumes the actor.
/// `delete_sandbox` reaps both. Needs a kubeconfig.
#[tokio::test]
#[ignore = "requires SUBSTRATE_LIVE_* env vars + a kubeconfig + a real WorkerPool"]
async fn live_synthesized_template_round_trip() {
    let Some(config) = config_from_env() else {
        eprintln!("skipping: SUBSTRATE_LIVE_* env vars not set");
        return;
    };
    let driver = SubstrateComputeDriver::new(config.clone());

    let actor_id = format!(
        "live-synth-{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
    );
    eprintln!("[live_synth] actor_id = {actor_id}");
    eprintln!(
        "[live_synth] config: worker_pool={} snapshots_location={} runsc_amd64_sha={}",
        config.default_worker_pool, config.snapshots_location, config.runsc_amd64_sha256
    );

    // No platform_config[substrate_actor_template] -> driver synthesizes.
    let sandbox = DriverSandbox {
        id: actor_id.clone(),
        name: actor_id.clone(),
        namespace: config.default_namespace.clone(),
        spec: Some(DriverSandboxSpec {
            template: Some(DriverSandboxTemplate {
                image: std::env::var("SUBSTRATE_LIVE_TEST_IMAGE").unwrap_or_else(|_| {
                    String::from(
                        "localhost:5001/openshell-sandbox-m0@sha256:4947aa0986d8f7fb5b875d784e2a62dd50bc491e692dd163c106ca94edf0a13e",
                    )
                }),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    };

    eprintln!("[live_synth] create_sandbox (synthesizes ActorTemplate, waits for Ready)...");
    driver
        .create_sandbox(Request::new(CreateSandboxRequest {
            sandbox: Some(sandbox.clone()),
        }))
        .await
        .expect("create_sandbox synthesized path");
    eprintln!("[live_synth]   template applied + actor resumed");

    // Read back via the driver to confirm the projection.
    let got = driver
        .get_sandbox(Request::new(GetSandboxRequest {
            sandbox_id: actor_id.clone(),
            sandbox_name: String::new(),
        }))
        .await
        .expect("get_sandbox")
        .into_inner();
    let observed = got.sandbox.expect("got sandbox");
    assert_eq!(observed.id, actor_id);
    eprintln!(
        "[live_synth]   observed status reason = {:?}",
        observed
            .status
            .as_ref()
            .and_then(|st| st.conditions.first())
            .map(|c| &c.reason)
    );

    // Cleanup: delete_sandbox should also tear down the synthesized
    // ActorTemplate (it carries the owner annotation).
    eprintln!("[live_synth] delete_sandbox (also drops the synthesized ActorTemplate)...");
    let del = driver
        .delete_sandbox(Request::new(DeleteSandboxRequest {
            sandbox_id: actor_id.clone(),
            sandbox_name: String::new(),
        }))
        .await
        .expect("delete_sandbox")
        .into_inner();
    assert!(del.deleted);
    eprintln!("[live_synth] ok");
}

/// `list_sandboxes` exercises the full Substrate round-trip
/// (`ListActors`) plus the projection helpers. Asserts only that the
/// call succeeds; the namespace filter may return zero or many
/// sandboxes depending on cluster state at run time.
#[tokio::test]
#[ignore = "requires SUBSTRATE_LIVE_* env vars + a running ate-api-server"]
async fn live_list_sandboxes() {
    let Some(config) = config_from_env() else {
        eprintln!("skipping: SUBSTRATE_LIVE_* env vars not set");
        return;
    };
    let ns = config.default_namespace.clone();
    let driver = SubstrateComputeDriver::new(config);
    let resp = driver
        .list_sandboxes(Request::new(ListSandboxesRequest {}))
        .await
        .expect("list_sandboxes");
    let list = resp.into_inner();
    eprintln!(
        "live_list_sandboxes: {} sandbox(es) in namespace {ns}",
        list.sandboxes.len(),
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
