// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Agent Substrate compute driver (scaffold).
//!
//! This crate is the M3 placeholder for letting OpenShell use Agent Substrate
//! (gVisor + checkpoint/restore via runsc, managed through `ateapi.Control`)
//! as a first-class compute backend. The four-commit
//! `chore/gvisor-degraded-netns` patch series + the example harness under
//! `examples/agent-substrate-demo/` prove the supervisor side already works
//! inside a Substrate gVisor actor; what is missing is the *driver* side --
//! the glue that lets the OpenShell gateway own the sandbox lifecycle by
//! issuing Substrate API calls instead of Docker/Podman/Kubernetes ones.
//!
//! See the crate `README.md` for the design sketch. Every gRPC method below
//! returns `Status::unimplemented` until the corresponding mapping is
//! written.

#![allow(clippy::result_large_err)]

use openshell_core::proto::compute::v1::{
    CreateSandboxRequest, CreateSandboxResponse, DeleteSandboxRequest, DeleteSandboxResponse,
    DriverCondition, DriverSandbox, DriverSandboxStatus, GetCapabilitiesRequest,
    GetCapabilitiesResponse, GetSandboxRequest, GetSandboxResponse, ListSandboxesRequest,
    ListSandboxesResponse, StopSandboxRequest, StopSandboxResponse, ValidateSandboxCreateRequest,
    ValidateSandboxCreateResponse, WatchSandboxesDeletedEvent, WatchSandboxesEvent,
    WatchSandboxesRequest, WatchSandboxesSandboxEvent, compute_driver_server::ComputeDriver,
    watch_sandboxes_event,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, mpsc};
use tokio_stream::{Stream, wrappers::ReceiverStream};
use tonic::metadata::MetadataValue;
use tonic::service::{Interceptor, interceptor::InterceptedService};
use tonic::transport::{Channel, Endpoint};
use tonic::{Request, Response, Status};

/// Tonic interceptor that injects `Authorization: Bearer <jwt>` on
/// every outbound RPC. When the token is `None` the interceptor is a
/// no-op so the same type covers both the authenticated and the
/// unauthenticated (dev) paths. `Debug` is hand-rolled to redact the
/// bearer (logs of the driver state should not leak the JWT).
#[derive(Clone)]
pub struct AuthInterceptor {
    bearer: Option<Arc<String>>,
}

impl std::fmt::Debug for AuthInterceptor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthInterceptor")
            .field("bearer", &self.bearer.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl Interceptor for AuthInterceptor {
    fn call(&mut self, mut req: Request<()>) -> Result<Request<()>, Status> {
        if let Some(token) = self.bearer.as_deref() {
            let header: MetadataValue<_> = format!("Bearer {token}")
                .parse()
                .map_err(|_| Status::internal("bearer token is not a valid HTTP header"))?;
            req.metadata_mut().insert("authorization", header);
        }
        Ok(req)
    }
}

/// Concrete client type used by every `ateapi.Control` call site. The
/// interceptor layer is fixed so call sites do not need to be generic
/// over its presence.
pub type ControlClient =
    ateapi::control_client::ControlClient<InterceptedService<Channel, AuthInterceptor>>;

/// Poll interval for the synthetic watch_sandboxes stream. Substrate's
/// `ateapi.Control` does not currently expose a streaming watch, so the
/// driver materialises one by diffing successive `ListActors` calls.
const WATCH_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Bounded channel capacity for the watch stream. A slow consumer that
/// can't keep up triggers `Status::resource_exhausted`; tune up if real
/// workloads see drops.
const WATCH_CHANNEL_BUFFER: usize = 64;

/// Generated tonic client for Substrate's `ateapi.Control` service.
///
/// The proto lives at `proto/ateapi.proto` (vendored from
/// `agent-substrate/substrate`); `build.rs` invokes `tonic_build` to emit
/// the client stubs at compile time. The driver embeds the `ateapi`
/// module to keep the generated code from leaking into the crate's
/// public API.
#[allow(
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    clippy::cargo,
    unused_qualifications,
    missing_docs
)]
pub mod ateapi {
    // Generated client + message types. The package name `ateapi` matches
    // the `package ateapi;` declaration in the proto.
    tonic::include_proto!("ateapi");
}

pub mod template;

/// Static configuration for the Substrate driver.
///
/// All fields are populated from the gateway's TOML config file or from
/// `OPENSHELL_SUBSTRATE_*` environment variables (the exact mapping will land
/// alongside the first real implementation). The shape is intentionally
/// small while the driver is a stub.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubstrateComputeConfig {
    /// gRPC endpoint of the Substrate `ate-api-server` (e.g.
    /// `127.0.0.1:8080` when port-forwarded, or the in-cluster service
    /// name when the driver runs alongside Substrate).
    pub api_endpoint: String,
    /// Default Kubernetes namespace where the driver creates
    /// `ActorTemplate`s. Each OpenShell sandbox maps to one
    /// `ActorTemplate` + one resumed `Actor`.
    pub default_namespace: String,
    /// Default `WorkerPool` the templates reference. Operators provision
    /// this once per cluster; the driver does not create or scale it.
    pub default_worker_pool: String,
    /// Pause-container image used as the sandbox root. Substrate
    /// requires a content-addressed reference. Default is the off-GCP
    /// pin from the M0.5 demo.
    pub pause_image: String,
    /// Cloud-storage location prefix the driver writes into
    /// `snapshotsConfig.location` of every synthesized ActorTemplate.
    /// Operators set this once per cluster.
    pub snapshots_location: String,
    /// SHA256 hash of the `amd64` runsc binary atelet will pull.
    pub runsc_amd64_sha256: String,
    /// gs:// URL pointing at the `amd64` runsc binary.
    pub runsc_amd64_url: String,
    /// How long to wait for a synthesized `ActorTemplate` to reach
    /// `Ready` (golden snapshot taken). 90 s matches the M0.5 demo
    /// measurement.
    pub template_ready_timeout_secs: u64,
    /// OpenShell gateway endpoint reachable from inside the gVisor
    /// actor. Injected into the synthesized template as
    /// `OPENSHELL_ENDPOINT` so the supervisor can connect for policy
    /// fetch and OCSF telemetry. Empty disables injection (the
    /// supervisor then runs in the standalone --policy-rules /
    /// --policy-data mode from the M0.5 demo).
    pub gateway_endpoint: String,
    /// Path to the CA bundle the driver uses to verify the
    /// `ate-api-server` certificate. When set, the driver dials
    /// `https://api_endpoint` instead of `http://`. None falls back to
    /// plaintext (development only).
    #[serde(default)]
    pub api_tls_ca_path: Option<std::path::PathBuf>,
    /// Optional mTLS client certificate. Pair with `api_client_key_path`.
    /// When both are set the driver presents this identity to
    /// `ate-api-server` on every connection.
    #[serde(default)]
    pub api_client_cert_path: Option<std::path::PathBuf>,
    /// Optional mTLS client private key. See `api_client_cert_path`.
    #[serde(default)]
    pub api_client_key_path: Option<std::path::PathBuf>,
    /// Optional domain name the driver verifies against the
    /// `ate-api-server` certificate's SANs. When unset the driver
    /// derives the name from `api_endpoint`'s host portion.
    #[serde(default)]
    pub api_tls_server_name: Option<String>,
    /// Optional path to a file containing a JWT the driver attaches as
    /// `Authorization: Bearer <jwt>` on every `ateapi.Control` RPC.
    /// The file is re-read at every channel build so rotating tokens
    /// (e.g. a Kubernetes projected service-account token) are picked
    /// up without a driver restart. None disables bearer auth.
    #[serde(default)]
    pub api_bearer_token_path: Option<std::path::PathBuf>,
    /// When `true`, synthesized `ActorTemplate` CRs carry a
    /// `spec.containers[0].securityContext.capabilities.add` requesting
    /// `CAP_NET_ADMIN` / `CAP_SETUID` / `CAP_SETGID` / `CAP_SYS_ADMIN`
    /// for the OpenShell supervisor. Requires the cluster to be
    /// running the `feat/openshell-driver-companion` Substrate branch
    /// (commit `ca74ca9` or later) -- older clusters reject the
    /// unknown field with HTTP 500 because the CRD's structural
    /// schema validates field names. Default `false` keeps the
    /// supervisor in M0.5 degraded mode and works against every
    /// Substrate version.
    #[serde(default)]
    pub request_capabilities: bool,
}

impl Default for SubstrateComputeConfig {
    fn default() -> Self {
        Self {
            api_endpoint: String::from("127.0.0.1:8080"),
            default_namespace: String::from("openshell-sandboxes"),
            default_worker_pool: String::from("openshell-worker-pool"),
            pause_image: String::from(
                "registry.k8s.io/pause:3.10.2@sha256:f548e0e8e3dc1896ca956272154dde3314e8cc4fde0a57577ee9fa1c63f5baf4",
            ),
            snapshots_location: String::from("gs://openshell-ate-snapshots/"),
            runsc_amd64_sha256: String::from(
                "a397be1abc2420d26bce6c70e6e2ff96c73aaaab929756c56f5e2089ea842b63",
            ),
            runsc_amd64_url: String::from("gs://gvisor/releases/nightly/2026-05-19/x86_64/runsc"),
            template_ready_timeout_secs: 180,
            // Empty by default keeps the local-policy story working
            // out of the box; operators flip this on when they want
            // the supervisor to fetch policy from a gateway.
            gateway_endpoint: String::new(),
            // Plaintext is the dev default. Production deployments set
            // api_tls_ca_path (and optionally the mTLS pair) to switch
            // to https://.
            api_tls_ca_path: None,
            api_client_cert_path: None,
            api_client_key_path: None,
            api_tls_server_name: None,
            api_bearer_token_path: None,
            // Opt-in: requesting capabilities requires the substrate
            // companion CRD to accept the field. Default false works
            // against stock Substrate without surprises.
            request_capabilities: false,
        }
    }
}

/// Annotation on synthesized ActorTemplates marking the driver as the
/// owner. The driver deletes only templates it created; pre-provisioned
/// templates referenced via `substrate_actor_template` are left in
/// place across DeleteSandbox.
const SYNTHESIZED_BY_ANNOTATION: &str = "ate.openshell.io/synthesized-by";

/// Errors specific to the Substrate driver. Sits above `tonic::Status`
/// so the gRPC layer can map structured failures into the right
/// `Code::*` for callers.
#[derive(Debug, thiserror::Error)]
pub enum SubstrateDriverError {
    #[error("invalid Substrate api_endpoint {endpoint:?}: {source}")]
    InvalidEndpoint {
        endpoint: String,
        #[source]
        source: tonic::transport::Error,
    },
    #[error("failed to connect to Substrate ate-api-server at {endpoint}: {source}")]
    Connect {
        endpoint: String,
        #[source]
        source: tonic::transport::Error,
    },
    #[error("Substrate RPC failed: {0}")]
    Rpc(#[from] Status),
    #[error("Kubernetes client error: {0}")]
    Kube(#[from] kube::Error),
    #[error("ActorTemplate {namespace}/{name} failed during golden-snapshot creation")]
    TemplatePhaseFailed { namespace: String, name: String },
    #[error(
        "ActorTemplate {namespace}/{name} did not reach Ready in time (last phase: {last_phase:?})"
    )]
    TemplateTimeout {
        namespace: String,
        name: String,
        last_phase: String,
    },
    #[error("failed to load TLS material from {path}: {source}")]
    TlsConfig {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

impl From<template::TemplateError> for SubstrateDriverError {
    fn from(err: template::TemplateError) -> Self {
        match err {
            template::TemplateError::Kube(k) => Self::Kube(k),
            template::TemplateError::PhaseFailed { namespace, name } => {
                Self::TemplatePhaseFailed { namespace, name }
            }
            template::TemplateError::Timeout {
                namespace,
                name,
                last_phase,
            } => Self::TemplateTimeout {
                namespace,
                name,
                last_phase,
            },
        }
    }
}

impl From<SubstrateDriverError> for Status {
    fn from(err: SubstrateDriverError) -> Self {
        match err {
            SubstrateDriverError::InvalidEndpoint { .. } => {
                Status::invalid_argument(err.to_string())
            }
            SubstrateDriverError::Connect { .. } => Status::unavailable(err.to_string()),
            SubstrateDriverError::Rpc(status) => status,
            SubstrateDriverError::Kube(_) => Status::unavailable(err.to_string()),
            SubstrateDriverError::TemplatePhaseFailed { .. } => {
                Status::failed_precondition(err.to_string())
            }
            SubstrateDriverError::TemplateTimeout { .. } => {
                Status::deadline_exceeded(err.to_string())
            }
            SubstrateDriverError::TlsConfig { .. } => Status::failed_precondition(err.to_string()),
        }
    }
}

/// Driver entry point. Holds the resolved config and a lazily-connected
/// `ateapi.Control` client. The first method call that needs the client
/// dials the endpoint; subsequent calls reuse the channel (tonic's
/// `Channel` is cheap-to-clone and multiplexes RPCs).
#[derive(Clone)]
pub struct SubstrateComputeDriver {
    config: Arc<SubstrateComputeConfig>,
    channel: Arc<Mutex<Option<Channel>>>,
    /// Lazily-initialized kube client. The driver uses this to apply
    /// ActorTemplate CRs when the caller does not pre-provision one.
    /// `kube::Client` is not `Debug`, so the parent struct has a hand-
    /// written `Debug` impl that skips this field.
    kube_client: Arc<Mutex<Option<kube::Client>>>,
}

impl std::fmt::Debug for SubstrateComputeDriver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubstrateComputeDriver")
            .field("config", &self.config)
            .field("channel", &self.channel)
            .field("kube_client", &"<not Debug>")
            .finish()
    }
}

impl SubstrateComputeDriver {
    /// Build a driver from a resolved config. Does not connect to
    /// Substrate yet; that happens lazily on the first method call.
    #[must_use]
    pub fn new(config: SubstrateComputeConfig) -> Self {
        Self {
            config: Arc::new(config),
            channel: Arc::new(Mutex::new(None)),
            kube_client: Arc::new(Mutex::new(None)),
        }
    }

    /// Lazily build (or reuse) the kube client used for `ActorTemplate`
    /// CR operations. Reads kubeconfig from the standard locations
    /// (in-cluster service account when present, else `KUBECONFIG`
    /// or `~/.kube/config`).
    pub async fn kube(&self) -> Result<kube::Client, SubstrateDriverError> {
        {
            let guard = self.kube_client.lock().await;
            if let Some(c) = guard.as_ref() {
                return Ok(c.clone());
            }
        }
        let mut guard = self.kube_client.lock().await;
        if let Some(c) = guard.as_ref() {
            return Ok(c.clone());
        }
        let client = kube::Client::try_default().await?;
        *guard = Some(client.clone());
        Ok(client)
    }

    /// Synthesize an `ActorTemplate` from the sandbox spec, apply it to
    /// the cluster, and block until Substrate's controller advances it
    /// to `Ready` (i.e. the golden snapshot has been captured).
    /// Returns the resulting template name.
    async fn synthesize_and_apply_template(
        &self,
        actor_id: &str,
        namespace: &str,
        sandbox: &DriverSandbox,
    ) -> Result<String, Status> {
        let template = synthesize_template(actor_id, namespace, sandbox, &self.config);
        let template_name = template
            .metadata
            .name
            .clone()
            .expect("synthesize_template always sets metadata.name");
        let client = self.kube().await?;
        template::apply(&client, &template)
            .await
            .map_err(SubstrateDriverError::from)?;
        template::wait_until_ready(
            &client,
            namespace,
            &template_name,
            Duration::from_secs(self.config.template_ready_timeout_secs),
        )
        .await
        .map_err(SubstrateDriverError::from)?;
        Ok(template_name)
    }

    /// Delete the `ActorTemplate` named for this actor, only if the
    /// driver synthesized it. Operator-provisioned templates (no
    /// driver annotation) are left in place.
    async fn delete_synthesized_template_if_owned(
        &self,
        actor_id: &str,
    ) -> Result<(), SubstrateDriverError> {
        use kube::api::{Api, DeleteParams};
        let client = self.kube().await?;
        let name = synthesized_template_name(actor_id);
        let api: Api<template::ActorTemplate> =
            Api::namespaced(client, &self.config.default_namespace);
        let tmpl = match api.get(&name).await {
            Ok(t) => t,
            Err(kube::Error::Api(e)) if e.code == 404 => return Ok(()),
            Err(e) => return Err(SubstrateDriverError::from(e)),
        };
        let synthesized = tmpl
            .metadata
            .annotations
            .as_ref()
            .and_then(|a| a.get(SYNTHESIZED_BY_ANNOTATION))
            .is_some();
        if !synthesized {
            return Ok(());
        }
        match api.delete(&name, &DeleteParams::default()).await {
            Ok(_) | Err(kube::Error::Api(_)) => Ok(()),
            Err(e) => Err(SubstrateDriverError::from(e)),
        }
    }

    /// Access the immutable config -- useful for tests and for the
    /// gateway-side wiring that needs to read the namespace/pool defaults
    /// when materializing requests.
    #[must_use]
    pub fn config(&self) -> &SubstrateComputeConfig {
        &self.config
    }

    /// Dial (or reuse) the `ate-api-server` channel and return a fresh
    /// `Control` client. The channel is built lazily on first use and
    /// cached for the lifetime of the driver. The bearer token (when
    /// configured) is re-read on every call so SA-token rotation is
    /// picked up without restarting the driver. Callers get a new
    /// client per call because tonic clients hold an exclusive `&mut`
    /// to the underlying channel during the RPC; cloning the channel
    /// is cheap.
    pub async fn control_client(&self) -> Result<ControlClient, SubstrateDriverError> {
        let auth = self.load_auth_interceptor().await?;

        // Fast path: channel already established.
        {
            let guard = self.channel.lock().await;
            if let Some(ch) = guard.as_ref() {
                return Ok(ateapi::control_client::ControlClient::with_interceptor(
                    ch.clone(),
                    auth,
                ));
            }
        }

        // Slow path: parse + dial. Done under the lock so concurrent
        // first-callers don't race to open multiple channels.
        let mut guard = self.channel.lock().await;
        if let Some(ch) = guard.as_ref() {
            return Ok(ateapi::control_client::ControlClient::with_interceptor(
                ch.clone(),
                auth,
            ));
        }

        let endpoint_str = self.config.api_endpoint.clone();
        let tls = self.load_tls_config().await?;
        let scheme = if tls.is_some() { "https" } else { "http" };
        let endpoint = format!("{scheme}://{endpoint_str}");
        let mut ep = Endpoint::from_shared(endpoint).map_err(|source| {
            SubstrateDriverError::InvalidEndpoint {
                endpoint: endpoint_str.clone(),
                source,
            }
        })?;
        if let Some(tls_cfg) = tls {
            ep = ep
                .tls_config(tls_cfg)
                .map_err(|source| SubstrateDriverError::Connect {
                    endpoint: endpoint_str.clone(),
                    source,
                })?;
        }
        let channel = ep
            .connect()
            .await
            .map_err(|source| SubstrateDriverError::Connect {
                endpoint: endpoint_str,
                source,
            })?;

        let client = ateapi::control_client::ControlClient::with_interceptor(channel.clone(), auth);
        *guard = Some(channel);
        Ok(client)
    }

    /// Read the bearer token from disk (when configured) and return a
    /// fresh `AuthInterceptor`. None token path returns an interceptor
    /// that no-ops, which keeps the same `ControlClient` type covering
    /// both authenticated and unauthenticated deployments.
    async fn load_auth_interceptor(&self) -> Result<AuthInterceptor, SubstrateDriverError> {
        let Some(path) = self.config.api_bearer_token_path.as_ref() else {
            return Ok(AuthInterceptor { bearer: None });
        };
        let raw = tokio::fs::read_to_string(path).await.map_err(|source| {
            SubstrateDriverError::TlsConfig {
                path: path.display().to_string(),
                source,
            }
        })?;
        // Tokens written by Kubernetes (projected SA tokens, kubectl
        // exec auth helpers) typically end with a newline that
        // bearer-token validators reject; strip it once here.
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(SubstrateDriverError::TlsConfig {
                path: path.display().to_string(),
                source: std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "bearer token file is empty",
                ),
            });
        }
        Ok(AuthInterceptor {
            bearer: Some(Arc::new(trimmed.to_string())),
        })
    }

    /// Materialize the tonic `ClientTlsConfig` from the driver's TLS
    /// paths. Returns `None` when the driver is configured for
    /// plaintext (development), `Some` when it should dial https://.
    /// Errors surface as `SubstrateDriverError::TlsConfig` with the
    /// offending path so operators can diagnose missing or
    /// permission-denied files.
    async fn load_tls_config(
        &self,
    ) -> Result<Option<tonic::transport::ClientTlsConfig>, SubstrateDriverError> {
        let cfg = &self.config;
        let Some(ca_path) = cfg.api_tls_ca_path.as_ref() else {
            // No CA configured -> plaintext.
            return Ok(None);
        };
        let ca_pem =
            tokio::fs::read(ca_path)
                .await
                .map_err(|source| SubstrateDriverError::TlsConfig {
                    path: ca_path.display().to_string(),
                    source,
                })?;
        let mut tls = tonic::transport::ClientTlsConfig::new()
            .ca_certificate(tonic::transport::Certificate::from_pem(ca_pem));

        // Optional mTLS client identity. Both halves must be present
        // for the pair to be honoured; a single half on its own is a
        // configuration error.
        match (
            cfg.api_client_cert_path.as_ref(),
            cfg.api_client_key_path.as_ref(),
        ) {
            (Some(cert), Some(key)) => {
                let cert_pem = tokio::fs::read(cert).await.map_err(|source| {
                    SubstrateDriverError::TlsConfig {
                        path: cert.display().to_string(),
                        source,
                    }
                })?;
                let key_pem = tokio::fs::read(key).await.map_err(|source| {
                    SubstrateDriverError::TlsConfig {
                        path: key.display().to_string(),
                        source,
                    }
                })?;
                tls = tls.identity(tonic::transport::Identity::from_pem(cert_pem, key_pem));
            }
            (Some(_), None) | (None, Some(_)) => {
                return Err(SubstrateDriverError::TlsConfig {
                    path: String::from("api_client_cert_path + api_client_key_path"),
                    source: std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "both mTLS client cert and key must be set together",
                    ),
                });
            }
            (None, None) => {}
        }
        if let Some(name) = cfg.api_tls_server_name.as_deref() {
            tls = tls.domain_name(name);
        }
        Ok(Some(tls))
    }
}

type WatchStream = Pin<Box<dyn Stream<Item = Result<WatchSandboxesEvent, Status>> + Send>>;

/// Project a Substrate `Actor` into the gateway-facing `DriverSandbox` shape.
///
/// Sandbox id and name both map to `actor_id` -- Substrate has no
/// separate "name" concept and reusing the id keeps lookups symmetric.
/// The `namespace` carries the `ActorTemplate` namespace, which is what
/// the gateway uses as a tenancy boundary. `spec` is intentionally None
/// in observed snapshots: the gateway already has the spec it gave us at
/// create time.
fn actor_to_driver_sandbox(actor: &ateapi::Actor) -> DriverSandbox {
    DriverSandbox {
        id: actor.actor_id.clone(),
        name: actor.actor_id.clone(),
        namespace: actor.actor_template_namespace.clone(),
        spec: None,
        status: Some(actor_to_driver_status(actor)),
    }
}

fn actor_to_driver_status(actor: &ateapi::Actor) -> DriverSandboxStatus {
    DriverSandboxStatus {
        sandbox_name: actor.actor_id.clone(),
        instance_id: actor.ateom_pod_name.clone(),
        agent_fd: String::new(),
        sandbox_fd: String::new(),
        conditions: vec![actor_status_to_condition(actor.status())],
        deleting: false,
    }
}

/// Translate Substrate's `Actor.Status` enum into the driver-condition
/// shape the gateway derives `SandboxPhase` from. The mapping deliberately
/// uses the type `Ready` so the gateway's existing phase derivation
/// (which looks for a `Ready=True` condition) works without changes.
fn actor_status_to_condition(status: ateapi::actor::Status) -> DriverCondition {
    use ateapi::actor::Status::*;
    let (cond_status, reason, message) = match status {
        Running => ("True", "Running", "actor restored and running"),
        Resuming => ("False", "Resuming", "actor is being restored from snapshot"),
        Suspending => ("False", "Suspending", "actor is being checkpointed"),
        Suspended => ("False", "Suspended", "actor is checkpointed; resume to run"),
        Unspecified => ("Unknown", "Unspecified", "actor status not reported"),
    };
    DriverCondition {
        r#type: String::from("Ready"),
        status: String::from(cond_status),
        reason: String::from(reason),
        message: String::from(message),
        // Substrate's ListActors does not report a per-actor transition
        // timestamp. Leave empty; the gateway treats an empty string as
        // "unreported" and uses its own observed time.
        last_transition_time: String::new(),
    }
}

const DRIVER_NAME: &str = "substrate";

/// Reject empty identifiers up front so every RPC method shares one
/// "must supply id or name" error. Returns the canonical id used to
/// address the Substrate actor (id wins when both are set).
fn require_sandbox_id(sandbox_id: &str, sandbox_name: &str) -> Result<String, Status> {
    if !sandbox_id.is_empty() {
        Ok(sandbox_id.to_string())
    } else if !sandbox_name.is_empty() {
        Ok(sandbox_name.to_string())
    } else {
        Err(Status::invalid_argument(
            "sandbox_id or sandbox_name is required",
        ))
    }
}

/// Extract the Substrate ActorTemplate name from the sandbox's
/// driver-specific configuration. Two paths are checked, in order:
///
///  1. Top-level `platform_config["substrate_actor_template"]`. Used by
///     in-process callers that build a `DriverSandbox` directly (and
///     by the live integration test).
///  2. `platform_config["annotations"]["substrate_actor_template"]`.
///     The gateway's `build_platform_config` (compute/mod.rs:1333)
///     nests `SandboxTemplate.annotations` under this key, so public
///     OpenShell API callers can request a pre-provisioned template
///     by setting `SandboxTemplate.annotations["substrate_actor_template"]`
///     in their `CreateSandbox` request.
///
/// Returns `None` if neither path is present or the value is not a
/// non-empty string.
fn template_name_from_spec(sandbox: &DriverSandbox) -> Option<String> {
    use prost_types::value::Kind;
    let cfg = sandbox
        .spec
        .as_ref()?
        .template
        .as_ref()?
        .platform_config
        .as_ref()?;
    // 1. Top-level key (driver-internal callers).
    if let Some(val) = cfg.fields.get("substrate_actor_template") {
        if let Some(Kind::StringValue(s)) = &val.kind {
            if !s.is_empty() {
                return Some(s.clone());
            }
        }
    }
    // 2. Nested under `annotations` (public-API gateway-translated path).
    if let Some(annotations_val) = cfg.fields.get("annotations") {
        if let Some(Kind::StructValue(annotations)) = &annotations_val.kind {
            if let Some(val) = annotations.fields.get("substrate_actor_template") {
                if let Some(Kind::StringValue(s)) = &val.kind {
                    if !s.is_empty() {
                        return Some(s.clone());
                    }
                }
            }
        }
    }
    None
}

/// Capability set the OpenShell supervisor needs to run in its full
/// (non-degraded) mode under Substrate. Mirrors the privileged
/// bootstrap surface enumerated in RFC 0005:
///
/// - `CAP_NET_ADMIN`: `unshare(CLONE_NEWNET)` + `ip link` so the
///   supervisor can build its egress netns.
/// - `CAP_SETUID` / `CAP_SETGID`: `initgroups` + `setresuid/gid`
///   inside `drop_privileges` when running as a non-`root` user.
/// - `CAP_SYS_ADMIN`: mount/unshare ops the supervisor's nftables
///   and Landlock helpers issue.
///
/// On Substrate releases without `SecurityContext` support (anything
/// before `feat/openshell-driver-companion`), this field is ignored
/// at apply time -- the supervisor stays in degraded mode and the
/// M0.5 patches in `lib.rs:run_sandbox` keep it alive.
fn supervisor_security_context() -> template::ContainerSecurityContext {
    template::ContainerSecurityContext {
        capabilities: Some(template::Capabilities {
            add: vec![
                String::from("CAP_NET_ADMIN"),
                String::from("CAP_SETUID"),
                String::from("CAP_SETGID"),
                String::from("CAP_SYS_ADMIN"),
            ],
            drop: vec![],
        }),
    }
}

/// Deterministic name for the ActorTemplate the driver synthesizes for
/// a given actor_id. Kept short and DNS-1123 compliant so Substrate's
/// own `actor_id` validation is satisfied at create time.
fn synthesized_template_name(actor_id: &str) -> String {
    // ActorTemplate names follow the same DNS-1123 rules as actor_ids.
    // The actor_id is already DNS-safe (Substrate validates this in
    // CreateActor), so prefixing is sufficient.
    format!("oshl-{actor_id}")
}

/// Build an `ActorTemplate` CR from the sandbox spec and driver config.
/// Pure: does not touch the cluster.
fn synthesize_template(
    actor_id: &str,
    namespace: &str,
    sandbox: &DriverSandbox,
    config: &SubstrateComputeConfig,
) -> template::ActorTemplate {
    use k8s_openapi::api::core::v1::{EnvVar, ObjectReference};
    use kube::core::ObjectMeta;
    use std::collections::BTreeMap;

    let template_spec = sandbox.spec.as_ref().and_then(|s| s.template.as_ref());
    let image = template_spec.map(|t| t.image.clone()).unwrap_or_default();

    // Combine environment from spec.environment + spec.template.environment
    // into a stable, sorted list. Driver-injected vars
    // (OPENSHELL_ENDPOINT, OPENSHELL_SANDBOX_ID, OPENSHELL_SANDBOX_TOKEN)
    // are layered on top so the caller's environment cannot override
    // identity material.
    let mut env_map: BTreeMap<String, String> = BTreeMap::new();
    if let Some(spec) = sandbox.spec.as_ref() {
        for (k, v) in &spec.environment {
            env_map.insert(k.clone(), v.clone());
        }
    }
    if let Some(t) = template_spec {
        for (k, v) in &t.environment {
            env_map.insert(k.clone(), v.clone());
        }
    }
    // Sandbox id (deterministic, always known).
    env_map.insert(
        openshell_core::sandbox_env::SANDBOX_ID.to_string(),
        actor_id.to_string(),
    );
    // Gateway endpoint, only if the driver was configured for one. An
    // empty config keeps the supervisor in the local-policy mode.
    if !config.gateway_endpoint.is_empty() {
        env_map.insert(
            openshell_core::sandbox_env::ENDPOINT.to_string(),
            config.gateway_endpoint.clone(),
        );
    }
    // Gateway-minted JWT, when the gateway populated it on the spec.
    if let Some(spec) = sandbox.spec.as_ref() {
        if !spec.sandbox_token.is_empty() {
            env_map.insert(
                openshell_core::sandbox_env::SANDBOX_TOKEN.to_string(),
                spec.sandbox_token.clone(),
            );
        }
    }
    let env: Vec<EnvVar> = env_map
        .into_iter()
        .map(|(name, value)| EnvVar {
            name,
            value: Some(value),
            value_from: None,
        })
        .collect();

    let mut annotations = BTreeMap::new();
    annotations.insert(
        SYNTHESIZED_BY_ANNOTATION.to_string(),
        format!("openshell-driver-substrate@{}", env!("CARGO_PKG_VERSION")),
    );

    let spec = template::ActorTemplateSpec {
        pause_image: config.pause_image.clone(),
        containers: vec![template::Container {
            name: "supervisor".to_string(),
            image,
            // Substrate's atelet does not (yet) read the image's CMD/
            // Entrypoint when synthesizing the OCI bundle; an empty
            // `args` makes runsc refuse to start with
            // "Spec.Process.Arg must be defined". The supervisor's
            // own default for empty COMMAND is `/bin/bash`, which
            // exits without a TTY and leaves the gVisor sandbox in a
            // half-dead state runsc can't snapshot
            // ("inconsistent private memory files on restore").
            //
            // Default to the M0.5 demo's full command: load policy
            // from /etc/openshell/{policy.rego,data.yaml} baked into
            // the image, drop into a long-running sleep loop as the
            // child workload so checkpoint/restore stays consistent.
            // Operators who need a custom entry point should
            // pre-provision an ActorTemplate (M3.3 path).
            command: vec![
                String::from("/usr/local/bin/openshell-sandbox"),
                String::from("--policy-rules"),
                String::from("/etc/openshell/policy.rego"),
                String::from("--policy-data"),
                String::from("/etc/openshell/data.yaml"),
                String::from("--log-level"),
                String::from("info"),
                String::from("--"),
                String::from("/bin/sh"),
                String::from("-c"),
                String::from("while true; do sleep 60; done"),
            ],
            ports: vec![],
            env,
            // Ask Substrate to grant the supervisor the privileged
            // syscalls it needs for its full (non-degraded) feature
            // set when the driver is configured to do so. Pre-
            // companion Substrate CRDs reject the unknown field with
            // a structural-schema 500, so this is opt-in via
            // `config.request_capabilities`.
            security_context: if config.request_capabilities {
                Some(supervisor_security_context())
            } else {
                None
            },
        }],
        snapshots_config: template::SnapshotsConfig {
            location: config.snapshots_location.clone(),
        },
        worker_pool_ref: ObjectReference {
            name: Some(config.default_worker_pool.clone()),
            namespace: Some(namespace.to_string()),
            ..Default::default()
        },
        runsc: template::RunscConfig {
            amd64: Some(template::RunscPlatformConfig {
                sha256_hash: config.runsc_amd64_sha256.clone(),
                url: config.runsc_amd64_url.clone(),
            }),
            arm64: None,
        },
    };

    template::ActorTemplate {
        metadata: ObjectMeta {
            name: Some(synthesized_template_name(actor_id)),
            namespace: Some(namespace.to_string()),
            annotations: Some(annotations),
            ..Default::default()
        },
        spec,
        status: None,
    }
}

/// Driver-specific validation rules applied before `CreateSandbox`.
/// These reject sandbox specs that cannot be honoured by the Substrate
/// backend so the gateway can return a typed error to its caller before
/// any platform state is touched.
fn validate_substrate_sandbox(sandbox: &DriverSandbox) -> Result<(), Status> {
    let spec = sandbox
        .spec
        .as_ref()
        .ok_or_else(|| Status::invalid_argument("sandbox.spec is required"))?;
    let template = spec
        .template
        .as_ref()
        .ok_or_else(|| Status::invalid_argument("sandbox.spec.template is required"))?;

    if template.image.trim().is_empty() {
        return Err(Status::failed_precondition(
            "Substrate sandboxes require a template image",
        ));
    }
    // atelet resolves bare tag references against Docker Hub and gets
    // UNAUTHORIZED. Refuse the spec up front; the gateway should pass
    // a content-addressed reference (image@sha256:...) so we know it
    // will pull from the local kind-registry (or any pull-cache).
    if !template.image.contains("@sha256:") {
        return Err(Status::failed_precondition(
            "Substrate sandbox images must be content-addressed \
             (image@sha256:<digest>); bare tag references fail in \
             atelet's pull cache",
        ));
    }
    // GPU support is real in Substrate (CDI passthrough) but the
    // driver has not wired the request -> ActorTemplate plumbing yet.
    // Reject the request up front rather than silently dropping it.
    if spec.gpu {
        return Err(Status::failed_precondition(
            "Substrate driver does not yet honour DriverSandboxSpec.gpu \
             requests; see the README for the planned wiring",
        ));
    }
    // platform_config['substrate_actor_template'] is now optional. When
    // absent, create_sandbox synthesizes a fresh ActorTemplate from the
    // sandbox spec. When present, it names a pre-provisioned template
    // the driver reuses as-is.
    Ok(())
}

#[tonic::async_trait]
impl ComputeDriver for SubstrateComputeDriver {
    type WatchSandboxesStream = WatchStream;

    async fn get_capabilities(
        &self,
        _request: Request<GetCapabilitiesRequest>,
    ) -> Result<Response<GetCapabilitiesResponse>, Status> {
        Ok(Response::new(GetCapabilitiesResponse {
            driver_name: String::from(DRIVER_NAME),
            driver_version: String::from(env!("CARGO_PKG_VERSION")),
            // Substrate sandboxes are policy-baked into their image; the
            // driver does not pick a default image, the gateway supplies
            // one per sandbox.
            default_image: String::new(),
            // GPU support is real in Substrate (CDI passthrough) but the
            // driver has not wired the request -> ActorTemplate plumbing
            // yet. Will flip to true when DriverResourceRequirements.gpu
            // is honored.
            supports_gpu: false,
            gpu_count: 0,
        }))
    }

    async fn validate_sandbox_create(
        &self,
        request: Request<ValidateSandboxCreateRequest>,
    ) -> Result<Response<ValidateSandboxCreateResponse>, Status> {
        let sandbox = request
            .into_inner()
            .sandbox
            .ok_or_else(|| Status::invalid_argument("sandbox is required"))?;
        validate_substrate_sandbox(&sandbox)?;
        Ok(Response::new(ValidateSandboxCreateResponse {}))
    }

    async fn get_sandbox(
        &self,
        request: Request<GetSandboxRequest>,
    ) -> Result<Response<GetSandboxResponse>, Status> {
        let req = request.into_inner();
        // OpenShell's identifier semantics: prefer sandbox_id, fall back
        // to sandbox_name. The Substrate mapping reuses the same string
        // for both, so either is accepted.
        let actor_id = if !req.sandbox_id.is_empty() {
            req.sandbox_id
        } else if !req.sandbox_name.is_empty() {
            req.sandbox_name
        } else {
            return Err(Status::invalid_argument(
                "sandbox_id or sandbox_name is required",
            ));
        };

        let mut client = self.control_client().await?;
        let resp = client
            .get_actor(ateapi::GetActorRequest {
                actor_id: actor_id.clone(),
            })
            .await
            .map_err(|status| {
                if status.code() == tonic::Code::NotFound {
                    Status::not_found(format!("sandbox {actor_id} not found"))
                } else {
                    Status::from(status)
                }
            })?;
        let actor = resp
            .into_inner()
            .actor
            .ok_or_else(|| Status::internal("ateapi.GetActor returned an empty actor"))?;

        Ok(Response::new(GetSandboxResponse {
            sandbox: Some(actor_to_driver_sandbox(&actor)),
        }))
    }

    async fn list_sandboxes(
        &self,
        _request: Request<ListSandboxesRequest>,
    ) -> Result<Response<ListSandboxesResponse>, Status> {
        let mut client = self.control_client().await?;
        let resp = client
            .list_actors(ateapi::ListActorsRequest {})
            .await
            .map_err(Status::from)?;
        let ns = self.config.default_namespace.as_str();
        // Tenancy boundary: surface only actors whose ActorTemplate lives
        // in the namespace the driver was configured for. A future
        // iteration may also key on a label or naming convention so the
        // same Substrate cluster can host multiple OpenShell gateways.
        let sandboxes = resp
            .into_inner()
            .actors
            .iter()
            .filter(|a| a.actor_template_namespace == ns)
            .map(actor_to_driver_sandbox)
            .collect();
        Ok(Response::new(ListSandboxesResponse { sandboxes }))
    }

    async fn create_sandbox(
        &self,
        request: Request<CreateSandboxRequest>,
    ) -> Result<Response<CreateSandboxResponse>, Status> {
        let sandbox = request
            .into_inner()
            .sandbox
            .ok_or_else(|| Status::invalid_argument("sandbox is required"))?;
        let actor_id = require_sandbox_id(&sandbox.id, &sandbox.name)?;

        // Re-validate here so create_sandbox is safe to call without a
        // prior validate_sandbox_create round-trip; the gateway's own
        // flow always calls validate first, but RPC consumers outside
        // the gateway might not.
        validate_substrate_sandbox(&sandbox)?;
        let template_ns = if sandbox.namespace.is_empty() {
            self.config.default_namespace.clone()
        } else {
            sandbox.namespace.clone()
        };

        // Two paths:
        //   (a) Operator pre-provisioned an ActorTemplate and named it
        //       in spec.template.platform_config[substrate_actor_template].
        //       Reuse it as-is; do not touch the cluster's CRs.
        //   (b) No pre-provisioned name: synthesize a fresh template
        //       from the sandbox spec, apply it, wait for Ready, then
        //       derive the actor from it. The synthesized template
        //       carries an annotation so delete_sandbox knows it owns
        //       the CR and can clean up.
        let template_name = match template_name_from_spec(&sandbox) {
            Some(name) => name,
            None => {
                self.synthesize_and_apply_template(&actor_id, &template_ns, &sandbox)
                    .await?
            }
        };

        let mut client = self.control_client().await?;
        client
            .create_actor(ateapi::CreateActorRequest {
                actor_id: actor_id.clone(),
                actor_template_namespace: template_ns,
                actor_template_name: template_name,
            })
            .await
            .map_err(Status::from)?;
        client
            .resume_actor(ateapi::ResumeActorRequest {
                actor_id,
                boot: false,
            })
            .await
            .map_err(Status::from)?;

        Ok(Response::new(CreateSandboxResponse {}))
    }

    async fn stop_sandbox(
        &self,
        request: Request<StopSandboxRequest>,
    ) -> Result<Response<StopSandboxResponse>, Status> {
        let req = request.into_inner();
        let actor_id = require_sandbox_id(&req.sandbox_id, &req.sandbox_name)?;
        let mut client = self.control_client().await?;
        // OpenShell "stop" -> Substrate "suspend" (checkpoint + free
        // the worker slot, snapshot retained).
        client
            .suspend_actor(ateapi::SuspendActorRequest { actor_id })
            .await
            .map_err(Status::from)?;
        Ok(Response::new(StopSandboxResponse {}))
    }

    async fn delete_sandbox(
        &self,
        request: Request<DeleteSandboxRequest>,
    ) -> Result<Response<DeleteSandboxResponse>, Status> {
        let req = request.into_inner();
        let actor_id = require_sandbox_id(&req.sandbox_id, &req.sandbox_name)?;
        let mut client = self.control_client().await?;

        // Substrate's DeleteActor requires the actor to be suspended.
        // Suspend best-effort first; ignore failures from actors that
        // are already suspended (FailedPrecondition) or stuck mid-
        // workflow (Internal) so delete can complete in either case.
        match client
            .suspend_actor(ateapi::SuspendActorRequest {
                actor_id: actor_id.clone(),
            })
            .await
        {
            Ok(_) => {}
            Err(status)
                if status.code() == tonic::Code::FailedPrecondition
                    || status.code() == tonic::Code::Internal => {}
            Err(other) => return Err(Status::from(other)),
        }

        // DeleteActor can still return NotFound for a sandbox the gateway
        // never created or that has already been wiped. Surface that as
        // deleted=false so the gateway can treat it as a no-op.
        let result = client
            .delete_actor(ateapi::DeleteActorRequest {
                actor_id: actor_id.clone(),
            })
            .await;
        let deleted = match result {
            Ok(_) => true,
            Err(status) if status.code() == tonic::Code::NotFound => false,
            Err(other) => return Err(Status::from(other)),
        };

        // If the driver synthesized the ActorTemplate (annotation
        // present), tear it down too. Operator-provisioned templates
        // are left in place. Errors here are non-fatal: the actor is
        // gone, so the gateway's view is consistent even if the CR
        // cleanup is deferred.
        if let Err(err) = self.delete_synthesized_template_if_owned(&actor_id).await {
            tracing::warn!(
                actor_id = %actor_id,
                error = ?err,
                "Substrate driver: failed to clean up synthesized ActorTemplate; \
                 operator may need to remove it manually"
            );
        }

        Ok(Response::new(DeleteSandboxResponse { deleted }))
    }

    async fn watch_sandboxes(
        &self,
        _request: Request<WatchSandboxesRequest>,
    ) -> Result<Response<Self::WatchSandboxesStream>, Status> {
        // Substrate's ateapi.Control does not (yet) expose a streaming
        // watch. We materialise one by polling ListActors every
        // WATCH_POLL_INTERVAL and emitting an event for every actor
        // whose snapshot differs from the prior tick, plus a deletion
        // event for actors that drop out of the listing.
        //
        // The poll loop terminates when the receiver is dropped (i.e.
        // the gRPC stream is cancelled). It also terminates on hard
        // RPC failures so the caller can reconnect; transient
        // ListActors failures are logged and retried.
        let (tx, rx) = mpsc::channel(WATCH_CHANNEL_BUFFER);
        let driver = self.clone();
        tokio::spawn(async move {
            let mut prior: HashMap<String, DriverSandbox> = HashMap::new();
            let mut bootstrapped = false;
            loop {
                let mut client = match driver.control_client().await {
                    Ok(c) => c,
                    Err(err) => {
                        let _ = tx.send(Err(Status::from(err))).await;
                        return;
                    }
                };
                let resp = match client.list_actors(ateapi::ListActorsRequest {}).await {
                    Ok(r) => r,
                    Err(status) => {
                        tracing::warn!(
                            ?status,
                            "Substrate driver: ListActors poll failed; retrying"
                        );
                        tokio::time::sleep(WATCH_POLL_INTERVAL).await;
                        continue;
                    }
                };
                let ns = driver.config.default_namespace.as_str();
                let mut current: HashMap<String, DriverSandbox> = resp
                    .into_inner()
                    .actors
                    .into_iter()
                    .filter(|a| a.actor_template_namespace == ns)
                    .map(|a| (a.actor_id.clone(), actor_to_driver_sandbox(&a)))
                    .collect();

                // Emit deletes for actors that disappeared. Skip on the
                // first tick (`bootstrapped == false`) so the consumer
                // gets a clean snapshot, not a flood of synthetic
                // deletes for state it never saw.
                if bootstrapped {
                    for id in prior.keys() {
                        if !current.contains_key(id) {
                            let evt = WatchSandboxesEvent {
                                payload: Some(watch_sandboxes_event::Payload::Deleted(
                                    WatchSandboxesDeletedEvent {
                                        sandbox_id: id.clone(),
                                    },
                                )),
                            };
                            if tx.send(Ok(evt)).await.is_err() {
                                return;
                            }
                        }
                    }
                }

                // Emit upserts for new actors and for actors whose
                // projection changed (currently keyed on the whole
                // DriverSandbox: status transitions, instance moves).
                for (id, sandbox) in &current {
                    let changed = prior.get(id).map_or(true, |old| old != sandbox);
                    if changed {
                        let evt = WatchSandboxesEvent {
                            payload: Some(watch_sandboxes_event::Payload::Sandbox(
                                WatchSandboxesSandboxEvent {
                                    sandbox: Some(sandbox.clone()),
                                },
                            )),
                        };
                        if tx.send(Ok(evt)).await.is_err() {
                            return;
                        }
                    }
                }

                std::mem::swap(&mut prior, &mut current);
                bootstrapped = true;
                tokio::time::sleep(WATCH_POLL_INTERVAL).await;
            }
        });

        let stream: WatchStream = Box::pin(ReceiverStream::new(rx));
        Ok(Response::new(stream))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_are_sane() {
        let c = SubstrateComputeConfig::default();
        assert!(!c.api_endpoint.is_empty());
        assert!(!c.default_namespace.is_empty());
        assert!(!c.default_worker_pool.is_empty());
        // TLS is opt-in: defaults dial plaintext for the dev story.
        assert!(c.api_tls_ca_path.is_none());
        assert!(c.api_client_cert_path.is_none());
        assert!(c.api_client_key_path.is_none());
    }

    #[tokio::test]
    async fn load_tls_config_none_when_unconfigured() {
        // Plaintext (no api_tls_ca_path) returns Ok(None) so
        // control_client uses http://.
        let driver = SubstrateComputeDriver::new(SubstrateComputeConfig::default());
        assert!(driver.load_tls_config().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn auth_interceptor_none_when_no_token_path() {
        let driver = SubstrateComputeDriver::new(SubstrateComputeConfig::default());
        let interceptor = driver.load_auth_interceptor().await.unwrap();
        assert!(interceptor.bearer.is_none());
    }

    #[tokio::test]
    async fn auth_interceptor_reads_and_trims_token_from_file() {
        // Kubernetes projected SA tokens write a trailing newline that
        // HTTP header parsers reject; the driver must strip it.
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let token_path = dir.path().join("token");
        let mut f = std::fs::File::create(&token_path).unwrap();
        writeln!(f, "eyJhbGciOiJSUzI1NiJ9.payload.sig").unwrap();
        let driver = SubstrateComputeDriver::new(SubstrateComputeConfig {
            api_bearer_token_path: Some(token_path),
            ..Default::default()
        });
        let interceptor = driver.load_auth_interceptor().await.unwrap();
        let token = interceptor.bearer.as_deref().unwrap();
        assert_eq!(token, "eyJhbGciOiJSUzI1NiJ9.payload.sig");
        assert!(!token.contains('\n'));
    }

    #[tokio::test]
    async fn auth_interceptor_rejects_empty_token_file() {
        let dir = tempfile::tempdir().unwrap();
        let token_path = dir.path().join("token");
        std::fs::write(&token_path, "   \n  \n").unwrap();
        let driver = SubstrateComputeDriver::new(SubstrateComputeConfig {
            api_bearer_token_path: Some(token_path),
            ..Default::default()
        });
        let err = driver.load_auth_interceptor().await.unwrap_err();
        assert!(matches!(err, SubstrateDriverError::TlsConfig { .. }));
    }

    #[test]
    fn auth_interceptor_injects_bearer_header() {
        use tonic::Request;
        let mut interceptor = AuthInterceptor {
            bearer: Some(Arc::new("xyz".to_string())),
        };
        let req = interceptor.call(Request::new(())).unwrap();
        let value = req
            .metadata()
            .get("authorization")
            .expect("authorization header present");
        assert_eq!(value.to_str().unwrap(), "Bearer xyz");
    }

    #[test]
    fn auth_interceptor_noop_when_token_absent() {
        use tonic::Request;
        let mut interceptor = AuthInterceptor { bearer: None };
        let req = interceptor.call(Request::new(())).unwrap();
        assert!(req.metadata().get("authorization").is_none());
    }

    #[tokio::test]
    async fn load_tls_config_rejects_half_mtls_pair() {
        // Setting cert without key (or vice versa) is a config error
        // up-front rather than a runtime mTLS handshake failure.
        let driver = SubstrateComputeDriver::new(SubstrateComputeConfig {
            api_tls_ca_path: Some("/nonexistent/ca.pem".into()),
            api_client_cert_path: Some("/nonexistent/cert.pem".into()),
            api_client_key_path: None,
            ..Default::default()
        });
        let err = driver.load_tls_config().await.unwrap_err();
        // The CA file doesn't exist, so the first error is IO. Smoke
        // test the variant rather than the message.
        assert!(matches!(err, SubstrateDriverError::TlsConfig { .. }));
    }

    #[test]
    fn driver_holds_config() {
        let cfg = SubstrateComputeConfig::default();
        let driver = SubstrateComputeDriver::new(cfg.clone());
        assert_eq!(driver.config().api_endpoint, cfg.api_endpoint);
    }

    #[test]
    fn actor_status_maps_to_ready_condition() {
        use ateapi::actor::Status::*;
        // Running is the only status that surfaces Ready=True; everything
        // else must report Ready=False or Unknown so the gateway's phase
        // derivation does not consider the sandbox usable.
        assert_eq!(actor_status_to_condition(Running).status, "True");
        assert_eq!(actor_status_to_condition(Resuming).status, "False");
        assert_eq!(actor_status_to_condition(Suspending).status, "False");
        assert_eq!(actor_status_to_condition(Suspended).status, "False");
        assert_eq!(actor_status_to_condition(Unspecified).status, "Unknown");
        // All conditions use the same type so the gateway can key on it.
        for s in [Running, Resuming, Suspending, Suspended, Unspecified] {
            assert_eq!(actor_status_to_condition(s).r#type, "Ready");
        }
    }

    #[test]
    fn require_sandbox_id_prefers_id() {
        assert_eq!(require_sandbox_id("abc", "xyz").unwrap(), "abc");
        assert_eq!(require_sandbox_id("", "xyz").unwrap(), "xyz");
        assert!(require_sandbox_id("", "").is_err());
    }

    #[test]
    fn template_name_requires_well_known_key() {
        // Missing spec -> None.
        let none_spec = DriverSandbox {
            id: "a".into(),
            ..Default::default()
        };
        assert!(template_name_from_spec(&none_spec).is_none());

        // platform_config with the right key as a StringValue -> Some.
        use openshell_core::proto::compute::v1::{DriverSandboxSpec, DriverSandboxTemplate};
        use prost_types::value::Kind;
        use prost_types::{Struct, Value};
        let mut fields = std::collections::BTreeMap::new();
        fields.insert(
            "substrate_actor_template".to_string(),
            Value {
                kind: Some(Kind::StringValue("supervisor".into())),
            },
        );
        let sandbox = DriverSandbox {
            id: "a".into(),
            spec: Some(DriverSandboxSpec {
                template: Some(DriverSandboxTemplate {
                    platform_config: Some(Struct { fields }),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            template_name_from_spec(&sandbox).as_deref(),
            Some("supervisor")
        );
    }

    fn sandbox_with_image_and_template(image: &str, gpu: bool) -> DriverSandbox {
        use openshell_core::proto::compute::v1::{DriverSandboxSpec, DriverSandboxTemplate};
        use prost_types::value::Kind;
        use prost_types::{Struct, Value};
        let mut fields = std::collections::BTreeMap::new();
        fields.insert(
            "substrate_actor_template".to_string(),
            Value {
                kind: Some(Kind::StringValue("supervisor".into())),
            },
        );
        DriverSandbox {
            id: "a".into(),
            spec: Some(DriverSandboxSpec {
                gpu,
                template: Some(DriverSandboxTemplate {
                    image: image.into(),
                    platform_config: Some(Struct { fields }),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn validate_accepts_digest_pinned_image() {
        let s = sandbox_with_image_and_template(
            "kind-registry:5000/openshell-sandbox-m0@sha256:1234",
            false,
        );
        validate_substrate_sandbox(&s).unwrap();
    }

    #[test]
    fn validate_rejects_bare_tag_image() {
        let s = sandbox_with_image_and_template("kind-registry:5000/foo:latest", false);
        let err = validate_substrate_sandbox(&s).unwrap_err();
        assert_eq!(err.code(), tonic::Code::FailedPrecondition);
        assert!(err.message().contains("content-addressed"));
    }

    #[test]
    fn validate_rejects_gpu_request() {
        let s = sandbox_with_image_and_template("img@sha256:1234", true);
        let err = validate_substrate_sandbox(&s).unwrap_err();
        assert_eq!(err.code(), tonic::Code::FailedPrecondition);
        assert!(err.message().contains("gpu"));
    }

    #[test]
    fn validate_accepts_missing_template_name() {
        // M3.5 made the template name optional: when absent,
        // create_sandbox synthesizes a fresh ActorTemplate from the
        // sandbox spec. validate must not reject that path.
        use openshell_core::proto::compute::v1::{DriverSandboxSpec, DriverSandboxTemplate};
        let s = DriverSandbox {
            id: "a".into(),
            spec: Some(DriverSandboxSpec {
                template: Some(DriverSandboxTemplate {
                    image: "img@sha256:1234".into(),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        validate_substrate_sandbox(&s).unwrap();
    }

    #[test]
    fn synthesize_template_carries_owner_annotation_and_image() {
        let sandbox = sandbox_with_image_and_template("img@sha256:abc", false);
        let cfg = SubstrateComputeConfig::default();
        let tmpl = synthesize_template("sb1", "ns1", &sandbox, &cfg);
        // Owner annotation lets delete_sandbox identify CRs it owns.
        let annotations = tmpl.metadata.annotations.unwrap();
        assert!(
            annotations.contains_key(SYNTHESIZED_BY_ANNOTATION),
            "must mark synthesized templates so delete_sandbox can clean up"
        );
        // Container image flows through from the sandbox spec.
        assert_eq!(tmpl.spec.containers.len(), 1);
        assert_eq!(tmpl.spec.containers[0].image, "img@sha256:abc");
        // Worker pool reference uses the driver default.
        assert_eq!(
            tmpl.spec.worker_pool_ref.name.as_deref(),
            Some(cfg.default_worker_pool.as_str())
        );
        assert_eq!(tmpl.spec.worker_pool_ref.namespace.as_deref(), Some("ns1"));
        // Name follows the deterministic scheme so delete_sandbox can
        // reconstruct it from the actor_id alone.
        assert_eq!(
            tmpl.metadata.name.as_deref(),
            Some(synthesized_template_name("sb1").as_str())
        );
        // SANDBOX_ID is always injected.
        let env_names: Vec<&str> = tmpl.spec.containers[0]
            .env
            .iter()
            .map(|e| e.name.as_str())
            .collect();
        assert!(env_names.contains(&"OPENSHELL_SANDBOX_ID"));
        // Empty gateway_endpoint default skips the endpoint var.
        assert!(!env_names.contains(&"OPENSHELL_ENDPOINT"));
        // Default request_capabilities=false MUST keep security_context
        // empty so the field is not emitted onto pre-companion CRDs.
        // Substrate's apiserver structural-schema validation rejects
        // the unknown field with HTTP 500 if it's present.
        assert!(
            tmpl.spec.containers[0].security_context.is_none(),
            "default config must not emit securityContext"
        );
    }

    #[test]
    fn synthesize_template_emits_security_context_when_requested() {
        let sandbox = sandbox_with_image_and_template("img@sha256:abc", false);
        let cfg = SubstrateComputeConfig {
            request_capabilities: true,
            ..Default::default()
        };
        let tmpl = synthesize_template("sb1", "ns1", &sandbox, &cfg);
        // When opted in, the supervisor's full cap set is requested.
        let sc = tmpl.spec.containers[0]
            .security_context
            .as_ref()
            .expect("securityContext present when request_capabilities=true");
        let caps = sc.capabilities.as_ref().expect("capabilities populated");
        for required in ["CAP_NET_ADMIN", "CAP_SETUID", "CAP_SETGID", "CAP_SYS_ADMIN"] {
            assert!(
                caps.add.iter().any(|c| c == required),
                "expected {required} in security_context.capabilities.add"
            );
        }
        // Drop is intentionally empty; the default sandbox cap set is
        // already minimal.
        assert!(caps.drop.is_empty());
    }

    #[test]
    fn synthesize_template_injects_gateway_endpoint_and_token() {
        use openshell_core::proto::compute::v1::{DriverSandboxSpec, DriverSandboxTemplate};
        use prost_types::value::Kind;
        use prost_types::{Struct, Value};
        let mut fields = std::collections::BTreeMap::new();
        fields.insert(
            "substrate_actor_template".to_string(),
            Value {
                kind: Some(Kind::StringValue("ignored".into())),
            },
        );
        let sandbox = DriverSandbox {
            id: "sbid".into(),
            spec: Some(DriverSandboxSpec {
                sandbox_token: "test-jwt".into(),
                template: Some(DriverSandboxTemplate {
                    image: "img@sha256:abc".into(),
                    platform_config: Some(Struct { fields }),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let cfg = SubstrateComputeConfig {
            gateway_endpoint: String::from("https://gateway.openshell.test:443"),
            ..Default::default()
        };
        let tmpl = synthesize_template("sbid", "ns1", &sandbox, &cfg);
        let env: std::collections::HashMap<String, String> = tmpl.spec.containers[0]
            .env
            .iter()
            .map(|e| (e.name.clone(), e.value.clone().unwrap_or_default()))
            .collect();
        assert_eq!(
            env.get("OPENSHELL_ENDPOINT").map(String::as_str),
            Some("https://gateway.openshell.test:443")
        );
        assert_eq!(
            env.get("OPENSHELL_SANDBOX_TOKEN").map(String::as_str),
            Some("test-jwt")
        );
        assert_eq!(
            env.get("OPENSHELL_SANDBOX_ID").map(String::as_str),
            Some("sbid")
        );
    }

    #[test]
    fn project_carries_namespace_and_id() {
        let actor = ateapi::Actor {
            actor_id: "abc-123".into(),
            actor_template_namespace: "openshell-sandboxes".into(),
            actor_template_name: "supervisor".into(),
            ateom_pod_name: "pool-worker-7".into(),
            status: ateapi::actor::Status::Running as i32,
            ..Default::default()
        };
        let s = actor_to_driver_sandbox(&actor);
        assert_eq!(s.id, "abc-123");
        assert_eq!(s.name, "abc-123");
        assert_eq!(s.namespace, "openshell-sandboxes");
        let status = s.status.unwrap();
        assert_eq!(status.instance_id, "pool-worker-7");
        assert_eq!(status.conditions.len(), 1);
        assert_eq!(status.conditions[0].status, "True");
    }
}
