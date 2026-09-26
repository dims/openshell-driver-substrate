// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! `ActorTemplate` synthesis.
//!
//! Substrate's `ActorTemplate` is a plain `ateapi.Control` gRPC resource: no
//! CRD and no Kubernetes client. A template is derived from the sandbox spec
//! and the driver config, and named by a hash of its own encoding, so repeat
//! `create_sandbox` calls for the same workload reuse the golden snapshot and
//! any change to what the template contains gets a new one.
//!
//! Nothing that varies per sandbox goes into a template. A snapshot freezes
//! process memory, so an environment variable set here comes back identical
//! in every actor restored from it. The sandbox id and the gateway-minted
//! sandbox token are per sandbox and are left out for that reason.

use std::collections::BTreeMap;
use std::time::Duration;

use openshell_core::proto::compute::v1::DriverSandbox;
use prost::Message;
use sha2::{Digest, Sha256};
use tonic::Status;

use crate::{ControlClient, SubstrateComputeConfig, ateapi};

const READY_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Everything the golden snapshot depends on.
struct Inputs {
    image: String,
    command: Vec<String>,
    env: BTreeMap<String, String>,
}

fn inputs_for(sandbox: &DriverSandbox, config: &SubstrateComputeConfig) -> Inputs {
    let spec = sandbox.spec.as_ref();
    let template_spec = spec.and_then(|s| s.template.as_ref());

    // spec.environment, then spec.template.environment over it, then the
    // gateway endpoint over both so a caller cannot redirect it.
    let mut env = BTreeMap::new();
    if let Some(s) = spec {
        env.extend(s.environment.clone());
    }
    if let Some(t) = template_spec {
        env.extend(t.environment.clone());
    }
    if !config.gateway_endpoint.is_empty() {
        env.insert(
            openshell_core::sandbox_env::ENDPOINT.to_string(),
            config.gateway_endpoint.clone(),
        );
    }

    Inputs {
        image: template_spec.map(|t| t.image.clone()).unwrap_or_default(),
        command: spec.map(|s| s.command.clone()).unwrap_or_default(),
        env,
    }
}

/// Deterministic, DNS-1123-safe template name: SHA-256 of the template's
/// own encoding with its metadata blanked. prost encodes a given struct the
/// same way every time, so the name is stable across toolchains.
#[must_use]
pub fn name_for(sandbox: &DriverSandbox, config: &SubstrateComputeConfig) -> String {
    let mut template = synthesize("", "", sandbox, config);
    template.metadata = None;
    let digest = Sha256::digest(template.encode_to_vec());
    let hex: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
    format!("oshl-{hex}")
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
    let inputs = inputs_for(sandbox, config);
    let env = inputs
        .env
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
            image: inputs.image,
            command: inputs.command,
            args: vec![],
            env,
            wakeup_probe: None,
            // The Landlock probe writes under /tmp, and the stock sandbox
            // image has no /tmp.
            volume_mounts: vec![ateapi::VolumeMount {
                name: "tmp".to_string(),
                mount_path: "/tmp".to_string(),
            }],
            // openshell-sandbox refuses to run with any capability in its
            // bounding set; substrate grants a few by default.
            security_context: Some(ateapi::SecurityContext {
                capabilities: Some(ateapi::Capabilities {
                    drop: vec!["ALL".to_string()],
                    ..Default::default()
                }),
            }),
            resources: None,
        }],
        volumes: vec![ateapi::Volume {
            name: "tmp".to_string(),
            durable_dir: Some(ateapi::DurableDirVolumeSource {}),
            ..Default::default()
        }],
        snapshot_config: Some(ateapi::SnapshotConfig {
            on_pause: ateapi::SnapshotContentScope::Full as i32,
            on_commit: ateapi::SnapshotContentScope::Full as i32,
            on_resume: Some(ateapi::OnResumeConfig {
                from_data: ateapi::ResumeSource::ColdBoot as i32,
            }),
            storage_location: config.snapshots_location.clone(),
        }),
        sandbox_config: Some(ateapi::SandboxConfig {
            sandbox_class: ateapi::SandboxClass::Microvm as i32,
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
        // Same name means the same inputs, so the existing golden snapshot
        // is valid for this sandbox too.
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
    use openshell_core::proto::compute::v1::{DriverSandboxSpec, DriverSandboxTemplate};

    use super::*;

    fn sandbox(image: &str, command: &[&str], env: &[(&str, &str)]) -> DriverSandbox {
        DriverSandbox {
            id: "sb-1".to_string(),
            spec: Some(DriverSandboxSpec {
                command: command.iter().map(|s| s.to_string()).collect(),
                sandbox_token: "gateway-minted-jwt".to_string(),
                environment: env
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
                template: Some(DriverSandboxTemplate {
                    image: image.to_string(),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    // Pinned: any change to what a template contains renames every existing
    // template and orphans its golden snapshot, so it must be a visible,
    // deliberate change.
    #[test]
    fn name_is_stable_and_dns_safe() {
        let config = SubstrateComputeConfig::default();
        let name = name_for(&sandbox("img@sha256:abc", &["/bin/foo"], &[]), &config);
        assert_eq!(name, "oshl-c6270ac92d8e942c");
        assert!(
            name.chars()
                .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
        );
    }

    #[test]
    fn name_covers_everything_the_snapshot_depends_on() {
        let config = SubstrateComputeConfig::default();
        let base = name_for(&sandbox("img@sha256:abc", &["/bin/foo"], &[]), &config);
        assert_ne!(
            base,
            name_for(&sandbox("img@sha256:def", &["/bin/foo"], &[]), &config)
        );
        assert_ne!(
            base,
            name_for(&sandbox("img@sha256:abc", &["/bin/bar"], &[]), &config)
        );
        assert_ne!(
            base,
            name_for(
                &sandbox("img@sha256:abc", &["/bin/foo"], &[("A", "1")]),
                &config
            )
        );
        // Environment order does not matter; the endpoint does.
        assert_eq!(
            name_for(
                &sandbox("i@sha256:a", &[], &[("A", "1"), ("B", "2")]),
                &config
            ),
            name_for(
                &sandbox("i@sha256:a", &[], &[("B", "2"), ("A", "1")]),
                &config
            )
        );
        let with_endpoint = SubstrateComputeConfig {
            gateway_endpoint: "gateway:443".to_string(),
            ..SubstrateComputeConfig::default()
        };
        assert_ne!(
            base,
            name_for(
                &sandbox("img@sha256:abc", &["/bin/foo"], &[]),
                &with_endpoint
            )
        );
        let other_config = SubstrateComputeConfig {
            sandbox_config_name: "microvm-2".to_string(),
            ..SubstrateComputeConfig::default()
        };
        assert_ne!(
            base,
            name_for(
                &sandbox("img@sha256:abc", &["/bin/foo"], &[]),
                &other_config
            )
        );
    }

    // Two sandboxes with the same inputs share one template and one
    // snapshot, so nothing that identifies one sandbox may be in it.
    #[test]
    fn synthesize_bakes_no_per_sandbox_identity() {
        let config = SubstrateComputeConfig {
            gateway_endpoint: "gateway:443".to_string(),
            ..SubstrateComputeConfig::default()
        };
        let sb = sandbox("img@sha256:abc", &["/bin/foo"], &[("CALLER", "x")]);
        let tmpl = synthesize("oshl-abc123", "ws", &sb, &config);
        let env = &tmpl.containers[0].env;
        let get = |k: &str| env.iter().find(|e| e.name == k).map(|e| e.value.as_str());

        assert_eq!(
            get(openshell_core::sandbox_env::ENDPOINT),
            Some("gateway:443")
        );
        assert_eq!(get("CALLER"), Some("x"));
        assert_eq!(get(openshell_core::sandbox_env::SANDBOX_ID), None);
        assert!(
            env.iter().all(|e| e.value != "gateway-minted-jwt"),
            "the sandbox token must not be in the template"
        );
        assert_eq!(
            tmpl.sandbox_config.as_ref().map(|s| s.sandbox_class),
            Some(ateapi::SandboxClass::Microvm as i32)
        );
        let drops = tmpl.containers[0]
            .security_context
            .as_ref()
            .and_then(|s| s.capabilities.as_ref())
            .map(|c| c.drop.clone())
            .unwrap_or_default();
        assert_eq!(drops, ["ALL"], "the sandbox must start capability-free");
        assert!(
            tmpl.containers[0]
                .volume_mounts
                .iter()
                .any(|m| m.mount_path == "/tmp"),
            "the Landlock probe needs a writable /tmp"
        );
    }
}
