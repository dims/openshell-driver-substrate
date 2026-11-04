// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! `ActorTemplate` synthesis.
//!
//! Substrate's `ActorTemplate` is a plain `Control` gRPC resource: no CRD and
//! no Kubernetes client. A template is derived from the sandbox spec and the
//! driver config, and named by a hash of its own encoding, so repeat
//! `create_sandbox` calls for the same workload reuse the golden snapshot and
//! any change to what the template contains gets a new one.
//!
//! Nothing that varies per sandbox goes into a template. A snapshot freezes
//! process memory, so an environment variable set here comes back identical
//! in every actor restored from it. The sandbox id, the gateway-minted
//! sandbox token and the request's environment are per sandbox and are left
//! out for that reason; the bundle carries the environment to the workload.
//!
//! The sandbox class is the worker pool's: the template selects its workers
//! by the labels in the driver config, and the pool those labels name decides
//! the class.

use std::time::Duration;

use openshell_core::proto::compute::v1::DriverSandbox;
use prost::Message;
use sha2::{Digest, Sha256};
use tonic::Status;

use crate::{ControlClient, SubstrateComputeConfig, ateapi};

const READY_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Every template this driver makes is named `oshl-` plus a hash.
pub const NAME_PREFIX: &str = "oshl-";

/// The fragment's container that takes the request's image.
pub const SANDBOX_CONTAINER: &str = "sandbox";

/// The configuration file's ActorTemplate fragment, checked once: the driver
/// names and places the template, no container may add a capability, and a
/// container named `sandbox` takes the request's image.
#[derive(Debug, Clone)]
pub struct Fragment(ateapi::ActorTemplate);

impl Fragment {
    pub fn new(fragment: ateapi::ActorTemplate) -> Result<Self, String> {
        if fragment.metadata.is_some() {
            return Err("metadata is the driver's".to_string());
        }
        if fragment.worker_selector.is_some() {
            return Err("workerSelector is the driver's (--worker-selector)".to_string());
        }
        if fragment.status.is_some() {
            return Err("status is read-only".to_string());
        }
        if !fragment
            .containers
            .iter()
            .any(|c| c.name == SANDBOX_CONTAINER)
        {
            return Err(format!("no container named {SANDBOX_CONTAINER}"));
        }
        for c in &fragment.containers {
            let adds = c
                .security_context
                .as_ref()
                .and_then(|s| s.capabilities.as_ref())
                .is_some_and(|caps| !caps.add.is_empty());
            if adds {
                return Err(format!(
                    "container {}: capabilities.add is not allowed",
                    c.name
                ));
            }
        }
        Ok(Self(fragment))
    }
}

impl std::ops::Deref for Fragment {
    type Target = ateapi::ActorTemplate;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Default for Fragment {
    /// One `sandbox` container, the request's image with its own entrypoint.
    fn default() -> Self {
        Self(ateapi::ActorTemplate {
            containers: vec![ateapi::Container {
                name: SANDBOX_CONTAINER.to_string(),
                ..Default::default()
            }],
            ..Default::default()
        })
    }
}

/// Everything the golden snapshot depends on.
struct Inputs {
    image: String,
    limits: ateapi::Resources,
}

fn inputs_for(
    sandbox: &DriverSandbox,
    config: &SubstrateComputeConfig,
) -> Result<Inputs, TemplateError> {
    Ok(Inputs {
        image: sandbox
            .spec
            .as_ref()
            .and_then(|s| s.template.as_ref())
            .map(|t| t.image.clone())
            .unwrap_or_default(),
        limits: resources_for(sandbox, config)?,
    })
}

/// The template's resource limits: the sandbox template's `cpu_limit` and
/// `memory_limit`, Kubernetes quantities, when set; the fragment's limits
/// next; the config defaults last.
pub fn resources_for(
    sandbox: &DriverSandbox,
    config: &SubstrateComputeConfig,
) -> Result<ateapi::Resources, TemplateError> {
    let resources = sandbox
        .spec
        .as_ref()
        .and_then(|s| s.template.as_ref())
        .and_then(|t| t.resources.as_ref());
    let cpu = resources
        .map(|r| r.cpu_limit.trim())
        .filter(|q| !q.is_empty());
    let memory = resources
        .map(|r| r.memory_limit.trim())
        .filter(|q| !q.is_empty());
    let fragment = config
        .template_fragment
        .resources
        .as_ref()
        .and_then(|r| r.limits.as_ref());
    let fallback = |from_fragment: Option<i64>, default: i64| {
        from_fragment.filter(|v| *v > 0).unwrap_or(default)
    };
    Ok(ateapi::Resources {
        cpu_millicores: match cpu {
            Some(q) => cpu_millicores(q).ok_or_else(|| {
                TemplateError::InvalidSpec(format!(
                    "cpu_limit {q:?}: expected a positive quantity such as 2, 0.5 or 500m"
                ))
            })?,
            None => fallback(
                fragment.map(|l| l.cpu_millicores),
                config.default_cpu_millicores,
            ),
        },
        memory_bytes: match memory {
            Some(q) => memory_bytes(q).ok_or_else(|| {
                TemplateError::InvalidSpec(format!(
                    "memory_limit {q:?}: expected a positive integer, with Ki, Mi, Gi, Ti, k, M, G or T"
                ))
            })?,
            None => fallback(
                fragment.map(|l| l.memory_bytes),
                config.default_memory_bytes,
            ),
        },
    })
}

fn digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// A Kubernetes CPU quantity as millicores: "2", "0.5", "500m". Finer than a
/// millicore, zero, or signed is rejected.
fn cpu_millicores(q: &str) -> Option<i64> {
    if let Some(m) = q.strip_suffix('m') {
        if !digits(m) {
            return None;
        }
        return m.parse::<i64>().ok().filter(|v| *v > 0);
    }
    let (whole, frac) = q.split_once('.').unwrap_or((q, ""));
    if !digits(whole) || frac.len() > 3 || !frac.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let whole: i64 = whole.parse().ok()?;
    let frac: i64 = if frac.is_empty() {
        0
    } else {
        format!("{frac:0<3}").parse().ok()?
    };
    whole
        .checked_mul(1000)?
        .checked_add(frac)
        .filter(|v| *v > 0)
}

/// A Kubernetes memory quantity as bytes: an unsigned integer, with a binary
/// (Ki, Mi, Gi, Ti) or decimal (k, M, G, T) suffix or none.
fn memory_bytes(q: &str) -> Option<i64> {
    const SUFFIXES: [(&str, i64); 8] = [
        ("Ki", 1 << 10),
        ("Mi", 1 << 20),
        ("Gi", 1 << 30),
        ("Ti", 1 << 40),
        ("k", 1_000),
        ("M", 1_000_000),
        ("G", 1_000_000_000),
        ("T", 1_000_000_000_000),
    ];
    let (number, scale) = SUFFIXES
        .iter()
        .find_map(|(suffix, scale)| q.strip_suffix(suffix).map(|n| (n, *scale)))
        .unwrap_or((q, 1));
    if !digits(number) {
        return None;
    }
    number
        .parse::<i64>()
        .ok()?
        .checked_mul(scale)
        .filter(|v| *v > 0)
}

/// Deterministic, DNS-1123-safe template name: SHA-256 of the template's
/// own encoding with its metadata blanked. prost encodes a given struct the
/// same way every time, so the name is stable across toolchains.
pub fn name_for(
    sandbox: &DriverSandbox,
    config: &SubstrateComputeConfig,
) -> Result<String, TemplateError> {
    let mut template = synthesize("", "", sandbox, config)?;
    template.metadata = None;
    let digest = Sha256::digest(template.encode_to_vec());
    let hex: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
    Ok(format!("{NAME_PREFIX}{hex}"))
}

/// Build an `ActorTemplate` from the sandbox spec and driver config. Pure:
/// does not touch the cluster. The fragment's containers, every one with
/// `drop: ALL` (openshell-sandbox refuses to run with any capability in its
/// bounding set, and Substrate grants a few by default), the `sandbox`
/// container with the request's image; the fragment's volumes, snapshot
/// config and golden egress policy; the limits merged in `resources_for`.
pub fn synthesize(
    name: &str,
    atespace: &str,
    sandbox: &DriverSandbox,
    config: &SubstrateComputeConfig,
) -> Result<ateapi::ActorTemplate, TemplateError> {
    let inputs = inputs_for(sandbox, config)?;
    let fragment = &config.template_fragment;
    let mut containers = fragment.containers.clone();
    for c in &mut containers {
        c.security_context = Some(drop_all());
    }
    let i = containers
        .iter()
        .position(|c| c.name == SANDBOX_CONTAINER)
        .expect("Fragment::new requires a sandbox container");
    containers[i].image = inputs.image;
    Ok(ateapi::ActorTemplate {
        metadata: Some(ateapi::ResourceMetadata {
            atespace: atespace.to_string(),
            name: name.to_string(),
            ..Default::default()
        }),
        worker_selector: Some(ateapi::Selector {
            match_labels: config.worker_selector.clone(),
        }),
        containers,
        volumes: fragment.volumes.clone(),
        // Whole-memory snapshots unless the fragment says otherwise, so a
        // restored sandbox is the frozen process and not a fresh boot.
        snapshot_config: fragment.snapshot_config.clone().or_else(|| {
            Some(ateapi::SnapshotConfig {
                storage_location: String::new(),
                preferred_fidelity: ateapi::SnapshotFidelity::Memory as i32,
                golden_snapshot_config: Some(ateapi::GoldenSnapshotConfig {
                    mode: ateapi::GoldenSnapshotMode::Enabled as i32,
                }),
            })
        }),
        resources: Some(ateapi::ResourceRequirements {
            limits: Some(inputs.limits),
        }),
        golden_egress_policy: fragment.golden_egress_policy.clone(),
        status: None,
    })
}

fn drop_all() -> ateapi::SecurityContext {
    ateapi::SecurityContext {
        capabilities: Some(ateapi::Capabilities {
            drop: vec!["ALL".to_string()],
            ..Default::default()
        }),
    }
}

/// Errors specific to template management. Lifted into
/// `SubstrateDriverError` at the lib boundary.
#[derive(Debug, thiserror::Error)]
pub enum TemplateError {
    #[error("invalid sandbox spec: {0}")]
    InvalidSpec(String),
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
/// name and an existing, healthy template is `CreateActorTemplate`
/// (`AlreadyExists`) and one `GetActorTemplate`.
pub async fn ensure_ready(
    client: &mut ControlClient,
    name: &str,
    atespace: &str,
    sandbox: &DriverSandbox,
    config: &SubstrateComputeConfig,
) -> Result<(), TemplateError> {
    let template = synthesize(name, atespace, sandbox, config)?;
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
                // Terminal. Delete the template so the next create rebuilds
                // the golden snapshot instead of finding this failure again.
                if let Err(status) = client
                    .delete_actor_template(ateapi::DeleteActorTemplateRequest {
                        actor_template: Some(ateapi::ObjectRef {
                            atespace: atespace.to_string(),
                            name: name.to_string(),
                        }),
                        options: None,
                    })
                    .await
                {
                    tracing::warn!(%atespace, %name, %status, "failed template not deleted");
                }
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
    use std::collections::BTreeMap;

    use openshell_core::proto::compute::v1::{
        DriverResourceRequirements, DriverSandboxSpec, DriverSandboxTemplate,
    };

    use super::*;

    fn sandbox(image: &str, env: &[(&str, &str)]) -> DriverSandbox {
        DriverSandbox {
            id: "sb-1".to_string(),
            spec: Some(DriverSandboxSpec {
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

    fn with_limits(mut sb: DriverSandbox, cpu: &str, memory: &str) -> DriverSandbox {
        let template = sb.spec.as_mut().unwrap().template.as_mut().unwrap();
        template.resources = Some(DriverResourceRequirements {
            cpu_limit: cpu.to_string(),
            memory_limit: memory.to_string(),
            ..Default::default()
        });
        sb
    }

    // Pinned: any change to what a template contains renames every existing
    // template and orphans its golden snapshot, so it must be a visible,
    // deliberate change.
    #[test]
    fn name_is_stable_and_dns_safe() {
        let config = SubstrateComputeConfig::default();
        let name = name_for(&sandbox("img@sha256:abc", &[]), &config).unwrap();
        assert_eq!(name, "oshl-a608eea347a94582");
        assert!(
            name.chars()
                .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
        );
    }

    #[test]
    fn name_covers_everything_the_snapshot_depends_on() {
        let config = SubstrateComputeConfig::default();
        let name = |sb: &DriverSandbox, c: &SubstrateComputeConfig| name_for(sb, c).unwrap();
        let base = name(&sandbox("img@sha256:abc", &[]), &config);
        assert_ne!(base, name(&sandbox("img@sha256:def", &[]), &config));
        assert_ne!(
            base,
            name(
                &with_limits(sandbox("img@sha256:abc", &[]), "2", ""),
                &config
            )
        );
        // The request's environment goes to the workload in the bundle, not
        // into the shared snapshot.
        assert_eq!(
            base,
            name(&sandbox("img@sha256:abc", &[("A", "1")]), &config)
        );
        let other_pool = SubstrateComputeConfig {
            worker_selector: BTreeMap::from([("workload".to_string(), "other".to_string())]),
            ..SubstrateComputeConfig::default()
        };
        assert_ne!(base, name(&sandbox("img@sha256:abc", &[]), &other_pool));
    }

    // Two sandboxes with the same inputs share one template and one
    // snapshot, so nothing that identifies one sandbox may be in it.
    #[test]
    fn synthesize_bakes_no_per_sandbox_identity() {
        let config = SubstrateComputeConfig::default();
        let sb = sandbox("img@sha256:abc", &[("CALLER", "x")]);
        let tmpl = synthesize("oshl-abc123", "ws", &sb, &config).unwrap();
        assert!(
            tmpl.containers[0].env.is_empty(),
            "neither the request's environment nor the sandbox token is in the template"
        );
        assert_eq!(
            tmpl.worker_selector
                .as_ref()
                .map(|s| s.match_labels.clone()),
            Some(config.worker_selector.clone()),
            "the pool decides the sandbox class"
        );
        let drops = tmpl.containers[0]
            .security_context
            .as_ref()
            .and_then(|s| s.capabilities.as_ref())
            .map(|c| c.drop.clone())
            .unwrap_or_default();
        assert_eq!(drops, ["ALL"], "the sandbox must start capability-free");
        let snapshot = tmpl.snapshot_config.as_ref().unwrap();
        assert_eq!(
            snapshot.preferred_fidelity,
            ateapi::SnapshotFidelity::Memory as i32
        );
        assert!(snapshot.storage_location.is_empty());
        assert_eq!(
            tmpl.resources.as_ref().and_then(|r| r.limits.as_ref()),
            Some(&ateapi::Resources {
                cpu_millicores: config.default_cpu_millicores,
                memory_bytes: config.default_memory_bytes,
            })
        );
    }

    #[test]
    fn resources_follow_the_spec_limits_or_the_defaults() {
        let config = SubstrateComputeConfig::default();
        let sb = with_limits(sandbox("img@sha256:abc", &[]), "500m", "256Mi");
        assert_eq!(
            resources_for(&sb, &config).unwrap(),
            ateapi::Resources {
                cpu_millicores: 500,
                memory_bytes: 256 << 20,
            }
        );
        let partial = with_limits(sandbox("img@sha256:abc", &[]), "", "1Gi");
        assert_eq!(
            resources_for(&partial, &config).unwrap(),
            ateapi::Resources {
                cpu_millicores: config.default_cpu_millicores,
                memory_bytes: 1 << 30,
            }
        );
        let bad = with_limits(sandbox("img@sha256:abc", &[]), "two", "");
        assert!(matches!(
            resources_for(&bad, &config),
            Err(TemplateError::InvalidSpec(_))
        ));
    }

    fn fragment(yaml: &str) -> Result<Fragment, String> {
        Fragment::new(
            crate::fragment::from_yaml("substrate.ateapi.v1.ActorTemplate", yaml).unwrap(),
        )
    }

    const SHAPE: &str = "containers:\n- name: relay\n  image: relay@sha256:r\n- name: sandbox\n  image: placeholder@sha256:p\n  env:\n  - name: FOO\n    value: bar\n  volumeMounts:\n  - name: data\n    mountPath: /data\nvolumes:\n- name: data\n  durableDir: {}\nresources:\n  limits:\n    cpuMillicores: 2000\n    memoryBytes: 2147483648\n";

    #[test]
    fn fragment_shapes_the_template_and_the_driver_keeps_its_say() {
        let config = SubstrateComputeConfig {
            template_fragment: fragment(SHAPE).unwrap(),
            ..SubstrateComputeConfig::default()
        };
        let sb = sandbox("img@sha256:abc", &[("CALLER", "x")]);
        let tmpl = synthesize("oshl-x", "ws", &sb, &config).unwrap();
        let names: Vec<&str> = tmpl.containers.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["relay", "sandbox"]);
        let s = &tmpl.containers[1];
        assert_eq!(s.image, "img@sha256:abc");
        let env: Vec<(&str, &str)> = s
            .env
            .iter()
            .map(|e| (e.name.as_str(), e.value.as_str()))
            .collect();
        assert_eq!(env, [("FOO", "bar")]);
        for c in &tmpl.containers {
            let drops = &c
                .security_context
                .as_ref()
                .unwrap()
                .capabilities
                .as_ref()
                .unwrap()
                .drop;
            assert_eq!(drops, &["ALL"], "{}", c.name);
        }
        assert_eq!(s.volume_mounts[0].mount_path, "/data");
        assert_eq!(tmpl.volumes.len(), 1);
        assert_eq!(
            tmpl.resources.unwrap().limits.unwrap(),
            ateapi::Resources {
                cpu_millicores: 2000,
                memory_bytes: 2 << 30,
            }
        );
        // The spec's own limit wins over the fragment's; the name covers the fragment.
        let sb = with_limits(sandbox("img@sha256:abc", &[]), "500m", "");
        assert_eq!(
            resources_for(&sb, &config).unwrap(),
            ateapi::Resources {
                cpu_millicores: 500,
                memory_bytes: 2 << 30,
            }
        );
        assert_ne!(
            name_for(&sb, &config).unwrap(),
            name_for(&sb, &SubstrateComputeConfig::default()).unwrap()
        );
    }

    #[test]
    fn fragment_rejects_what_the_driver_owns() {
        for (what, yaml) in [
            ("metadata", "metadata:\n  name: x\n"),
            (
                "workerSelector",
                "workerSelector:\n  matchLabels:\n    a: b\n",
            ),
            ("status", "status: {}\n"),
            (
                "capabilities.add",
                "containers:\n- name: sandbox\n  securityContext:\n    capabilities:\n      add: [NET_ADMIN]\n",
            ),
            (
                "no sandbox container",
                "containers:\n- name: relay\n  image: relay@sha256:r\n",
            ),
        ] {
            assert!(fragment(yaml).is_err(), "{what}");
        }
        assert!(fragment(SHAPE).is_ok());
    }

    #[test]
    fn kubernetes_quantities() {
        for (q, want) in [
            ("2", Some(2000)),
            ("500m", Some(500)),
            ("0.5", Some(500)),
            ("1.25", Some(1250)),
            ("0", None),
            ("x", None),
            ("1.2345", None),
            ("-0.5", None),
            ("+1", None),
            ("9223372036854775807", None),
        ] {
            assert_eq!(cpu_millicores(q), want, "cpu {q:?}");
        }
        for (q, want) in [
            ("256Mi", Some(256 << 20)),
            ("1Gi", Some(1 << 30)),
            ("1k", Some(1000)),
            ("1G", Some(1_000_000_000)),
            ("12", Some(12)),
            ("1Gib", None),
            ("0", None),
            ("-1Gi", None),
        ] {
            assert_eq!(memory_bytes(q), want, "memory {q:?}");
        }
    }
}
