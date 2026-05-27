// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! `ActorTemplate` CRD support.
//!
//! Mirrors `agent-substrate/substrate/api/v1alpha1/actortemplate_types.go`
//! closely enough that this crate can synthesize and apply a template, then
//! wait for Substrate's controller to advance its phase to `Ready` (which
//! signals that the golden snapshot has been captured and the template is
//! usable for actor creation).
//!
//! The mirror is intentionally minimal: only the fields the driver writes
//! or reads back are typed. Anything Substrate's controller may add to the
//! CRD over time falls through `serde(default)` and is ignored on read.

use std::time::Duration;

use k8s_openapi::api::core::v1::{ContainerPort, EnvVar, ObjectReference};
use kube::CustomResource;
use kube::api::{Api, Patch, PatchParams};
use kube::{Client, ResourceExt};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

const APPLY_FIELD_MANAGER: &str = "openshell-driver-substrate";
const READY_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// A single workload container the actor will run inside the gVisor
/// sandbox. Mirrors the Substrate `Container` Go struct -- only the
/// fields the driver writes are listed; ports and env are pass-through.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Container {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub image: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ports: Vec<ContainerPort>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<EnvVar>,
    /// Substrate's per-container security context (CRD field
    /// `securityContext`). When set, the additional capabilities are
    /// merged with the cluster's default sandbox set inside the OCI
    /// bundle builder. Older Substrate versions (pre-`SecurityContext`
    /// support) silently ignore the field on apply, so emitting it
    /// unconditionally is safe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub security_context: Option<ContainerSecurityContext>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<ContainerResources>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ContainerResources {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu: Option<GpuResource>,
}

/// Mirror of substrate's GPUResource.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct GpuResource {
    #[serde(default = "default_gpu_count")]
    pub count: i32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub device: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub driver_capabilities: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub driver_version: String,
}

fn default_gpu_count() -> i32 {
    1
}

/// Substrate subset of K8s `SecurityContext` -- only what the
/// ActorTemplate CRD admits today.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ContainerSecurityContext {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<Capabilities>,
}

/// Linux capability adjustments applied on top of Substrate's default
/// sandbox set (`CAP_AUDIT_WRITE`, `CAP_KILL`, `CAP_NET_BIND_SERVICE`).
/// Names may carry or omit the `CAP_` prefix; the OCI builder
/// normalizes them.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Default)]
pub struct Capabilities {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub add: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub drop: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct SnapshotsConfig {
    pub location: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RunscPlatformConfig {
    pub sha256_hash: String,
    pub url: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RunscConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amd64: Option<RunscPlatformConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arm64: Option<RunscPlatformConfig>,
}

/// `ate.dev/v1alpha1 ActorTemplate` CRD root. The derive macro emits
/// the wrapper struct (`ActorTemplate`) plus glue for `kube::Api`.
#[derive(CustomResource, Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[kube(
    group = "ate.dev",
    version = "v1alpha1",
    kind = "ActorTemplate",
    namespaced,
    status = "ActorTemplateStatus"
)]
#[serde(rename_all = "camelCase")]
pub struct ActorTemplateSpec {
    pub pause_image: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub containers: Vec<Container>,
    pub snapshots_config: SnapshotsConfig,
    pub worker_pool_ref: ObjectReference,
    pub runsc: RunscConfig,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ActorTemplateStatus {
    /// Substrate's controller advances through:
    /// `""` (initial) → `ResumeGoldenActor` → `WaitGoldenActor` → `Ready` | `Failed`.
    #[serde(default)]
    pub phase: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub golden_actor_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub golden_snapshot: String,
}

/// Errors specific to template management. Lifted into
/// `SubstrateDriverError` at the lib boundary.
#[derive(Debug, thiserror::Error)]
pub enum TemplateError {
    #[error("kube client error: {0}")]
    Kube(#[from] kube::Error),
    #[error("ActorTemplate {namespace}/{name} reached phase Failed; aborting create")]
    PhaseFailed { namespace: String, name: String },
    #[error(
        "timed out waiting for ActorTemplate {namespace}/{name} to reach Ready (last phase: {last_phase:?})"
    )]
    Timeout {
        namespace: String,
        name: String,
        last_phase: String,
    },
}

/// Server-side apply the template into the cluster. Idempotent: replays
/// of the same spec produce the same template (the controller treats
/// "spec unchanged" as a no-op and does not retake the golden snapshot).
pub async fn apply(client: &Client, template: &ActorTemplate) -> Result<(), TemplateError> {
    let ns = template
        .namespace()
        .expect("ActorTemplate must be namespaced before apply");
    let api: Api<ActorTemplate> = Api::namespaced(client.clone(), &ns);
    let name = template.name_any();
    let params = PatchParams::apply(APPLY_FIELD_MANAGER).force();
    api.patch(&name, &params, &Patch::Apply(template))
        .await
        .map(|_| ())
        .map_err(TemplateError::from)
}

/// Block until the template reports `status.phase == "Ready"` or
/// `Failed`. Polls every `READY_POLL_INTERVAL`; gives up after
/// `timeout`.
pub async fn wait_until_ready(
    client: &Client,
    namespace: &str,
    name: &str,
    timeout: Duration,
) -> Result<(), TemplateError> {
    let api: Api<ActorTemplate> = Api::namespaced(client.clone(), namespace);
    let started = std::time::Instant::now();
    loop {
        let tmpl = api.get_status(name).await?;
        let phase = tmpl.status.unwrap_or_default().phase;
        match phase.as_str() {
            "Ready" => return Ok(()),
            "Failed" => {
                return Err(TemplateError::PhaseFailed {
                    namespace: namespace.to_string(),
                    name: name.to_string(),
                });
            }
            _ => {}
        }
        if started.elapsed() > timeout {
            return Err(TemplateError::Timeout {
                namespace: namespace.to_string(),
                name: name.to_string(),
                last_phase: phase,
            });
        }
        tokio::time::sleep(READY_POLL_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actor_template_camel_cases_correctly() {
        // The CRD spec wire format is camelCase (matches the Go struct
        // tags); the Rust struct field is snake_case. Verify the
        // serde rename rolls through.
        let tmpl = ActorTemplate::new(
            "supervisor",
            ActorTemplateSpec {
                pause_image: "registry.k8s.io/pause:3.10.2@sha256:abc".into(),
                containers: vec![Container {
                    name: "supervisor".into(),
                    image: "localhost:5001/openshell-sandbox-m0@sha256:def".into(),
                    command: vec!["/usr/local/bin/openshell-sandbox".into()],
                    ports: vec![],
                    env: vec![],
                    security_context: None,
                    resources: None,
                }],
                snapshots_config: SnapshotsConfig {
                    location: "gs://ate-snapshots/ate-openshell-m0/".into(),
                },
                worker_pool_ref: ObjectReference {
                    name: Some("openshell-m0-pool".into()),
                    namespace: Some("ate-openshell-m0".into()),
                    ..Default::default()
                },
                runsc: RunscConfig {
                    amd64: Some(RunscPlatformConfig {
                        sha256_hash: "a397be1abc".into(),
                        url: "gs://gvisor/releases/nightly/2026-05-19/x86_64/runsc".into(),
                    }),
                    arm64: None,
                },
            },
        );
        let json = serde_json::to_value(&tmpl).unwrap();
        let spec = json.get("spec").expect("has spec");
        assert!(spec.get("pauseImage").is_some(), "camelCase: pauseImage");
        assert!(
            spec.get("snapshotsConfig").is_some(),
            "camelCase: snapshotsConfig"
        );
        assert!(
            spec.get("workerPoolRef").is_some(),
            "camelCase: workerPoolRef"
        );
        assert!(spec.get("runsc").is_some(), "lower: runsc");
        let runsc = spec.get("runsc").unwrap();
        assert!(
            runsc.get("amd64").unwrap().get("sha256Hash").is_some(),
            "camelCase: sha256Hash"
        );
    }

    #[test]
    fn status_phase_default_is_empty() {
        // The initial phase reported by Substrate is the empty string;
        // make sure our deserialization treats a missing field the
        // same way.
        let s: ActorTemplateStatus = serde_json::from_str("{}").unwrap();
        assert_eq!(s.phase, "");
        let s: ActorTemplateStatus = serde_json::from_str(r#"{"phase":"Ready"}"#).unwrap();
        assert_eq!(s.phase, "Ready");
    }
}
