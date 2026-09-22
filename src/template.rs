// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! `ActorTemplate` synthesis.
//!
//! Substrate's `ActorTemplate` is a plain `ateapi.Control` gRPC resource (see
//! `CreateActorTemplate`/`GetActorTemplate` in `proto/ateapi.proto`) — there is
//! no CRD and no Kubernetes client involved. A template is derived once per
//! distinct (image, command) pair and reused by content-derived name, so
//! repeat `create_sandbox` calls for the same workload skip straight to
//! `CreateActor` instead of rebuilding a golden snapshot.

use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::time::Duration;

use openshell_core::proto::compute::v1::DriverSandbox;
use tonic::Status;

use crate::{ControlClient, SubstrateComputeConfig, ateapi};

const READY_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Deterministic, DNS-1123-safe template name derived from the parts of the
/// sandbox spec that affect the golden snapshot (image + command). Same
/// inputs -> same name -> `CreateActorTemplate` returns `AlreadyExists` and
/// the caller reuses the existing golden snapshot instead of rebuilding one.
#[must_use]
pub fn template_name_for(image: &str, command: &[String]) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    image.hash(&mut hasher);
    command.hash(&mut hasher);
    format!("oshl-{:016x}", hasher.finish())
}

/// Build an `ActorTemplate` from the sandbox spec and driver config. Pure:
/// does not touch the cluster.
#[must_use]
pub fn synthesize(
    name: &str,
    atespace: &str,
    sandbox: &DriverSandbox,
    config: &SubstrateComputeConfig,
) -> ateapi::ActorTemplate {
    let template_spec = sandbox.spec.as_ref().and_then(|s| s.template.as_ref());
    let image = template_spec.map(|t| t.image.clone()).unwrap_or_default();
    let command = sandbox
        .spec
        .as_ref()
        .map(|s| s.command.clone())
        .unwrap_or_default();

    // Merge spec.environment + spec.template.environment (template wins on
    // conflict), then layer driver-injected identity vars on top so the
    // caller's environment cannot override them.
    let mut env_map: BTreeMap<String, String> = BTreeMap::new();
    if let Some(spec) = sandbox.spec.as_ref() {
        env_map.extend(spec.environment.clone());
    }
    if let Some(t) = template_spec {
        env_map.extend(t.environment.clone());
    }
    env_map.insert(
        openshell_core::sandbox_env::SANDBOX_ID.to_string(),
        sandbox.id.clone(),
    );
    if !config.gateway_endpoint.is_empty() {
        env_map.insert(
            openshell_core::sandbox_env::ENDPOINT.to_string(),
            config.gateway_endpoint.clone(),
        );
    }
    if let Some(spec) = sandbox.spec.as_ref()
        && !spec.sandbox_token.is_empty()
    {
        // ponytail: OPENSHELL_SANDBOX_TOKEN is openshell-core's test-harness
        // env-var path (see sandbox_env.rs), not the production
        // SANDBOX_TOKEN_FILE bind-mount path. Good enough while proving the
        // driver contract; move to a mounted file if a real deployment
        // needs it.
        env_map.insert(
            openshell_core::sandbox_env::SANDBOX_TOKEN.to_string(),
            spec.sandbox_token.clone(),
        );
    }
    let env: Vec<ateapi::EnvVar> = env_map
        .into_iter()
        .map(|(name, value)| ateapi::EnvVar { name, value })
        .collect();

    ateapi::ActorTemplate {
        metadata: Some(ateapi::ResourceMetadata {
            atespace: atespace.to_string(),
            name: name.to_string(),
            ..Default::default()
        }),
        worker_selector: None,
        containers: vec![ateapi::Container {
            name: "sandbox".to_string(),
            image,
            command,
            args: vec![],
            env,
            readyz: None,
            volume_mounts: vec![],
            security_context: None,
            resources: None,
        }],
        volumes: vec![],
        snapshots_config: Some(ateapi::SnapshotsConfig {
            on_pause: ateapi::SnapshotContentScope::Full as i32,
            on_commit: ateapi::SnapshotContentScope::Full as i32,
            on_resume: Some(ateapi::OnResumeConfig {
                from_data: ateapi::ResumeSource::ColdBoot as i32,
            }),
            storage_location: config.snapshots_location.clone(),
        }),
        sandbox_config: Some(ateapi::SandboxConfig {
            sandbox_class: ateapi::SandboxClass::Gvisor as i32,
            config_name: config.sandbox_config_name.clone(),
        }),
        resources: None,
        status: None,
    }
}

/// Errors specific to template management. Lifted into
/// `SubstrateDriverError` at the lib boundary.
#[derive(Debug, thiserror::Error)]
pub enum TemplateError {
    #[error("Substrate RPC failed while managing ActorTemplate {atespace}/{name}: {source}")]
    Rpc {
        atespace: String,
        name: String,
        #[source]
        source: Status,
    },
    #[error("ActorTemplate {atespace}/{name} failed during golden-snapshot creation: {message}")]
    PhaseFailed {
        atespace: String,
        name: String,
        message: String,
    },
    #[error("timed out waiting for ActorTemplate {atespace}/{name} to reach Ready")]
    Timeout { atespace: String, name: String },
}

/// Ensure a template named `name` exists in `atespace`, creating it from
/// `sandbox` if not already present, then block until its golden snapshot is
/// ready (or failed / timed out). Idempotent: a second call with the same
/// name and an existing, healthy template is a single `GetActorTemplate`.
pub async fn ensure_ready(
    client: &mut ControlClient,
    name: &str,
    atespace: &str,
    sandbox: &DriverSandbox,
    config: &SubstrateComputeConfig,
) -> Result<(), TemplateError> {
    let template = synthesize(name, atespace, sandbox, config);
    let create = client
        .create_actor_template(ateapi::CreateActorTemplateRequest {
            actor_template: Some(template),
        })
        .await;
    match create {
        Ok(_) => {}
        // Reuse: same content hash means the same image + command, so the
        // existing golden snapshot is valid for this create too.
        Err(status) if status.code() == tonic::Code::AlreadyExists => {}
        Err(status) => {
            return Err(TemplateError::Rpc {
                atespace: atespace.to_string(),
                name: name.to_string(),
                source: status,
            });
        }
    }

    let started = std::time::Instant::now();
    loop {
        let resp = client
            .get_actor_template(ateapi::GetActorTemplateRequest {
                actor_template: Some(ateapi::ObjectRef {
                    atespace: atespace.to_string(),
                    name: name.to_string(),
                }),
            })
            .await
            .map_err(|source| TemplateError::Rpc {
                atespace: atespace.to_string(),
                name: name.to_string(),
                source,
            })?
            .into_inner();
        if let Some(status) = resp.status.as_ref()
            && let Some(golden) = status.golden_snapshot_status.as_ref()
        {
            if !golden.error_message.is_empty() {
                return Err(TemplateError::PhaseFailed {
                    atespace: atespace.to_string(),
                    name: name.to_string(),
                    message: golden.error_message.clone(),
                });
            }
            if golden.golden_tag.is_some() {
                return Ok(());
            }
        }
        if started.elapsed() > Duration::from_secs(config.template_ready_timeout_secs) {
            return Err(TemplateError::Timeout {
                atespace: atespace.to_string(),
                name: name.to_string(),
            });
        }
        tokio::time::sleep(READY_POLL_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_name_is_deterministic_and_dns_safe() {
        let a = template_name_for("img@sha256:abc", &["/bin/foo".to_string()]);
        let b = template_name_for("img@sha256:abc", &["/bin/foo".to_string()]);
        let c = template_name_for("img@sha256:def", &["/bin/foo".to_string()]);
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert!(
            a.chars()
                .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
        );
    }

    #[test]
    fn synthesize_injects_sandbox_id_and_endpoint() {
        let sandbox = DriverSandbox {
            id: "actor-1".to_string(),
            spec: Some(openshell_core::proto::compute::v1::DriverSandboxSpec {
                template: Some(openshell_core::proto::compute::v1::DriverSandboxTemplate {
                    image: "img@sha256:abc".to_string(),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let config = SubstrateComputeConfig {
            gateway_endpoint: "gateway:443".to_string(),
            ..SubstrateComputeConfig::default()
        };
        let tmpl = synthesize("oshl-abc123", "ws", &sandbox, &config);
        let env = &tmpl.containers[0].env;
        assert!(
            env.iter()
                .any(|e| e.name == openshell_core::sandbox_env::SANDBOX_ID && e.value == "actor-1")
        );
        assert!(
            env.iter().any(
                |e| e.name == openshell_core::sandbox_env::ENDPOINT && e.value == "gateway:443"
            )
        );
    }
}
