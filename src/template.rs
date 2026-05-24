// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! `ActorTemplate` CRD support: a minimal mirror of
//! `agent-substrate/substrate/api/v1alpha1/actortemplate_types.go`
//! sufficient for the driver to synthesize a template, apply it, and
//! wait for Substrate's controller to advance its phase to `Ready`
//! (golden snapshot captured). Fields beyond what the driver writes
//! or reads back fall through `serde(default)`.

use std::time::Duration;

use k8s_openapi::api::core::v1::{ContainerPort, EnvVar, ObjectReference};
use kube::CustomResource;
use kube::api::{Api, Patch, PatchParams};
use kube::{Client, ResourceExt};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

const APPLY_FIELD_MANAGER: &str = "openshell-driver-substrate";
const READY_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Minimal mirror of Substrate's `Container` Go struct: only the
/// fields the driver writes are typed.
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

/// `ate.dev/v1alpha1 ActorTemplate` CRD root; the derive emits the
/// wrapper struct plus `kube::Api` glue.
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

/// Template-management errors; lifted into `SubstrateDriverError`.
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

/// Server-side apply. Idempotent: replays of the same spec are no-ops
/// in Substrate's controller, so the golden snapshot is not retaken.
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
        // The CRD's wire format is camelCase; our Rust fields are
        // snake_case. Verify the serde rename rolls through.
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
        // Substrate reports the initial phase as empty string; a
        // missing field must deserialize the same way.
        let s: ActorTemplateStatus = serde_json::from_str("{}").unwrap();
        assert_eq!(s.phase, "");
        let s: ActorTemplateStatus = serde_json::from_str(r#"{"phase":"Ready"}"#).unwrap();
        assert_eq!(s.phase, "Ready");
    }
}
