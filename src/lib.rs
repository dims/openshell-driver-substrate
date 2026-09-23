// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Agent Substrate compute driver for OpenShell.
//!
//! Maps OpenShell's `ComputeDriver` gRPC contract onto Substrate's
//! `ateapi.Control`: an OpenShell sandbox is a Substrate `Actor`, created
//! against a content-addressed, auto-synthesized `ActorTemplate` (see
//! `template.rs`). `create`/`start` -> `CreateActor`+`ResumeActor`, `stop` ->
//! `SuspendActor`, `delete` -> `DeleteActor`. The driver runs as its own
//! out-of-process binary (`main.rs`) speaking the same `ComputeDriverServer`
//! protocol as OpenShell's in-tree Docker/Podman/Kubernetes drivers, so
//! wiring it in is gateway configuration, not a source change.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use openshell_core::proto::compute::v1::{
    AuthenticateSandboxRequest, AuthenticateSandboxResponse, CreateSandboxRequest,
    CreateSandboxResponse, DeleteSandboxRequest, DeleteSandboxResponse, DeleteWorkspaceRequest,
    DeleteWorkspaceResponse, DriverCondition, DriverSandbox, DriverSandboxStatus,
    EnsureWorkspaceRequest, EnsureWorkspaceResponse, GetCapabilitiesRequest,
    GetCapabilitiesResponse, GetSandboxRequest, GetSandboxResponse, ListSandboxesRequest,
    ListSandboxesResponse, StartSandboxRequest, StartSandboxResponse, StopSandboxRequest,
    StopSandboxResponse, ValidateSandboxCreateRequest, ValidateSandboxCreateResponse,
    WatchSandboxesDeletedEvent, WatchSandboxesEvent, WatchSandboxesRequest,
    WatchSandboxesSandboxEvent, compute_driver_server::ComputeDriver, watch_sandboxes_event,
};
use tokio::sync::{Mutex, mpsc};
use tokio_stream::{Stream, wrappers::ReceiverStream};
use tonic::metadata::MetadataValue;
use tonic::service::{Interceptor, interceptor::InterceptedService};
use tonic::transport::{Channel, Endpoint};
use tonic::{Request, Response, Status};

/// Tonic interceptor that injects `Authorization: Bearer <jwt>` on every
/// outbound RPC. A `None` token makes the interceptor a no-op, so the same
/// type covers both the authenticated and the plaintext-dev path.
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

/// Concrete client type used by every `ateapi.Control` call site.
pub type ControlClient =
    ateapi::control_client::ControlClient<InterceptedService<Channel, AuthInterceptor>>;

/// Poll interval for the synthetic `watch_sandboxes` stream. `ateapi.Control`
/// has no streaming watch RPC, so the driver materialises one by diffing
/// successive `ListActors` calls.
const WATCH_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Bounded channel capacity for the watch stream.
const WATCH_CHANNEL_BUFFER: usize = 64;

/// Generated tonic client for Substrate's `ateapi.Control` service. The
/// proto lives at `proto/ateapi.proto` (vendored from
/// `agent-substrate/substrate`); `build.rs` compiles it at build time.
#[allow(
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    clippy::cargo,
    unused_qualifications,
    missing_docs
)]
pub mod ateapi {
    tonic::include_proto!("ateapi");
}

pub mod template;

const DRIVER_NAME: &str = "substrate";

/// Static configuration for the Substrate driver, populated from CLI
/// flags / env vars in `main.rs`.
#[derive(Debug, Clone)]
pub struct SubstrateComputeConfig {
    /// gRPC endpoint of Substrate's `ate-api-server` (e.g.
    /// `127.0.0.1:8080` when port-forwarded).
    pub api_endpoint: String,
    /// Substrate atespace every Actor and ActorTemplate is created in.
    /// A single shared atespace is enough for a first-class driver: the
    /// `ComputeDriver` trait only carries workspace identity on
    /// `create_sandbox`, not on get/stop/start/delete, so a per-workspace
    /// atespace can't be resolved for those calls anyway.
    pub atespace: String,
    /// Name of the cluster-provisioned `SandboxConfig` object supplying
    /// the micro-VM assets every synthesized `ActorTemplate` references.
    /// Operators provision this once per cluster; the driver never
    /// creates it.
    pub sandbox_config_name: String,
    /// Object-storage location prefix every synthesized `ActorTemplate`
    /// writes its `snapshotsConfig.storageLocation` to.
    pub snapshots_location: String,
    /// How long to wait for a synthesized `ActorTemplate` to reach a
    /// golden snapshot before giving up.
    pub template_ready_timeout_secs: u64,
    /// OpenShell gateway endpoint reachable from inside the actor.
    /// Injected as `OPENSHELL_ENDPOINT`. Empty disables injection.
    pub gateway_endpoint: String,
    /// Path to the CA bundle used to verify `ate-api-server`'s
    /// certificate. Set to dial `https://`; unset dials plaintext
    /// (development only).
    pub api_tls_ca_path: Option<std::path::PathBuf>,
    /// Optional mTLS client certificate. Pair with `api_client_key_path`.
    pub api_client_cert_path: Option<std::path::PathBuf>,
    /// Optional mTLS client private key.
    pub api_client_key_path: Option<std::path::PathBuf>,
    /// Optional TLS server-name override for the `ate-api-server`
    /// certificate. Derived from `api_endpoint`'s host when unset.
    pub api_tls_server_name: Option<String>,
    /// Path to a bearer token file (e.g. a projected Kubernetes
    /// ServiceAccount token) re-read on every channel build, so rotated
    /// tokens are picked up without a driver restart. Unset disables
    /// bearer auth.
    pub api_bearer_token_path: Option<std::path::PathBuf>,
}

impl Default for SubstrateComputeConfig {
    fn default() -> Self {
        Self {
            api_endpoint: String::from("127.0.0.1:8080"),
            atespace: String::from("default"),
            sandbox_config_name: String::from("microvm"),
            snapshots_location: String::from("gs://openshell-ate-snapshots/"),
            template_ready_timeout_secs: 180,
            gateway_endpoint: String::new(),
            api_tls_ca_path: None,
            api_client_cert_path: None,
            api_client_key_path: None,
            api_tls_server_name: None,
            api_bearer_token_path: None,
        }
    }
}

/// Errors specific to the Substrate driver. Sits above `tonic::Status` so
/// the gRPC layer can map structured failures to the right `Code::*`.
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
    #[error(transparent)]
    Template(#[from] template::TemplateError),
    #[error("failed to load TLS material from {path}: {source}")]
    TlsConfig {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

impl From<SubstrateDriverError> for Status {
    fn from(err: SubstrateDriverError) -> Self {
        match &err {
            SubstrateDriverError::InvalidEndpoint { .. } => {
                Status::invalid_argument(err.to_string())
            }
            SubstrateDriverError::Connect { .. } => Status::unavailable(err.to_string()),
            SubstrateDriverError::Rpc(status) => status.clone(),
            SubstrateDriverError::Template(template::TemplateError::PhaseFailed { .. }) => {
                Status::failed_precondition(err.to_string())
            }
            SubstrateDriverError::Template(template::TemplateError::Timeout { .. }) => {
                Status::deadline_exceeded(err.to_string())
            }
            SubstrateDriverError::Template(template::TemplateError::Rpc { .. }) => {
                Status::unavailable(err.to_string())
            }
            SubstrateDriverError::TlsConfig { .. } => Status::failed_precondition(err.to_string()),
        }
    }
}

/// Driver entry point. Holds the resolved config and a lazily-connected
/// `ateapi.Control` client (tonic's `Channel` is cheap to clone and
/// multiplexes RPCs, so it is dialed once and reused).
#[derive(Clone)]
pub struct SubstrateComputeDriver {
    config: Arc<SubstrateComputeConfig>,
    channel: Arc<Mutex<Option<Channel>>>,
}

impl std::fmt::Debug for SubstrateComputeDriver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubstrateComputeDriver")
            .field("config", &self.config)
            .finish()
    }
}

impl SubstrateComputeDriver {
    #[must_use]
    pub fn new(config: SubstrateComputeConfig) -> Self {
        Self {
            config: Arc::new(config),
            channel: Arc::new(Mutex::new(None)),
        }
    }

    #[must_use]
    pub fn config(&self) -> &SubstrateComputeConfig {
        &self.config
    }

    /// Dial (or reuse) the `ate-api-server` channel and return a fresh
    /// `Control` client. Callers get a new client handle per call because
    /// tonic clients hold an exclusive `&mut` to the channel during an
    /// RPC; cloning the channel itself is cheap.
    pub async fn control_client(&self) -> Result<ControlClient, SubstrateDriverError> {
        let auth = self.load_auth_interceptor().await?;

        {
            let guard = self.channel.lock().await;
            if let Some(ch) = guard.as_ref() {
                return Ok(ateapi::control_client::ControlClient::with_interceptor(
                    ch.clone(),
                    auth,
                ));
            }
        }

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
        // Kubernetes projected SA tokens carry a trailing newline that
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

    async fn load_tls_config(
        &self,
    ) -> Result<Option<tonic::transport::ClientTlsConfig>, SubstrateDriverError> {
        let cfg = &self.config;
        let Some(ca_path) = cfg.api_tls_ca_path.as_ref() else {
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

/// Project a Substrate `Actor` into the gateway-facing `DriverSandbox`
/// shape. Sandbox id and name both map to the actor's resource name --
/// Substrate has no separate "name" concept and reusing it keeps lookups
/// symmetric. `spec` is intentionally `None` in observed snapshots: the
/// gateway already has the spec it gave us at create time.
fn actor_to_driver_sandbox(actor: &ateapi::Actor) -> DriverSandbox {
    let meta = actor.metadata.clone().unwrap_or_default();
    DriverSandbox {
        id: meta.name.clone(),
        name: meta.name.clone(),
        namespace: meta.atespace.clone(),
        spec: None,
        status: Some(actor_to_driver_status(actor)),
        workspace: meta.atespace,
    }
}

fn actor_to_driver_status(actor: &ateapi::Actor) -> DriverSandboxStatus {
    let name = actor
        .metadata
        .as_ref()
        .map(|m| m.name.clone())
        .unwrap_or_default();
    let status = actor.status.clone().unwrap_or_default();
    let state = status.state();
    let instance_id = status
        .worker_assignment
        .as_ref()
        .map(|w| w.worker_pod.clone())
        .unwrap_or_default();
    DriverSandboxStatus {
        name,
        instance_id,
        agent_fd: String::new(),
        sandbox_fd: String::new(),
        conditions: vec![actor_state_to_condition(state)],
        deleting: state == ateapi::ActorState::Deleting,
        resolved_identity: None,
        fence_evidence: None,
    }
}

/// Translate Substrate's `ActorState` into the driver-condition shape the
/// gateway derives `SandboxPhase` from. Uses type `Ready` so the gateway's
/// existing "Ready=True" phase derivation works without changes.
fn actor_state_to_condition(state: ateapi::ActorState) -> DriverCondition {
    use ateapi::ActorState::{
        Crashed, Deleting, Paused, Pausing, Resuming, Reverting, Running, Suspended, Suspending,
        Unspecified,
    };
    let (status, reason, message) = match state {
        Running => ("True", "Running", "actor restored and running"),
        Resuming => ("False", "Resuming", "actor is being restored from snapshot"),
        Suspending => ("False", "Suspending", "actor is being checkpointed"),
        Suspended => ("False", "Suspended", "actor is checkpointed; resume to run"),
        Pausing => ("False", "Pausing", "actor is being paused"),
        Paused => ("False", "Paused", "actor is paused; resume to run"),
        Crashed => ("False", "Crashed", "actor crashed"),
        Deleting => ("False", "Deleting", "actor is being deleted"),
        Reverting => (
            "False",
            "Reverting",
            "actor is reverting to its last snapshot",
        ),
        Unspecified => ("Unknown", "Unspecified", "actor status not reported"),
    };
    DriverCondition {
        r#type: String::from("Ready"),
        status: String::from(status),
        reason: String::from(reason),
        message: String::from(message),
        transition_time: None,
    }
}

/// Reject empty identifiers up front so every RPC method shares one "must
/// supply id or name" error. Returns the canonical name used to address
/// the Substrate actor (id wins when both are set; the driver uses the
/// same string for both).
fn require_actor_name(sandbox_id: &str, sandbox_name: &str) -> Result<String, Status> {
    if !sandbox_id.is_empty() {
        Ok(sandbox_id.to_string())
    } else if !sandbox_name.is_empty() {
        Ok(sandbox_name.to_string())
    } else {
        Err(Status::invalid_argument("sandbox_id or name is required"))
    }
}

/// Driver-specific validation applied before `CreateSandbox`, so the
/// gateway gets a typed error before any platform state is touched.
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
    if !template.image.contains("@sha256:") {
        return Err(Status::failed_precondition(
            "Substrate sandbox images must be content-addressed \
             (image@sha256:<digest>); bare tag references fail atelet's pull cache",
        ));
    }
    Ok(())
}

#[tonic::async_trait]
impl ComputeDriver for SubstrateComputeDriver {
    type WatchSandboxesStream = WatchStream;

    async fn authenticate_sandbox(
        &self,
        _request: Request<AuthenticateSandboxRequest>,
    ) -> Result<Response<AuthenticateSandboxResponse>, Status> {
        Err(Status::unimplemented(
            "substrate driver does not authenticate sandbox credentials",
        ))
    }

    async fn get_capabilities(
        &self,
        _request: Request<GetCapabilitiesRequest>,
    ) -> Result<Response<GetCapabilitiesResponse>, Status> {
        Ok(Response::new(GetCapabilitiesResponse {
            driver_name: String::from(DRIVER_NAME),
            driver_version: String::from(env!("CARGO_PKG_VERSION")),
            // Substrate sandboxes are policy-baked into their image; the
            // driver does not pick a default, the gateway supplies one.
            default_image: String::new(),
            gateway_manages_lifecycle: false,
            supports_sandbox_authentication: false,
            driver_reports_runtime_readiness: false,
            resource_capabilities: None,
            rootfs_tar_staging_dir: String::new(),
            rootfs_tar_max_bytes: 0,
            extension: Some(openshell_core::extension_protocol::extension_metadata(
                openshell_core::extension_protocol::ExtensionFamily::Compute,
                "openshell-driver-substrate",
                env!("CARGO_PKG_VERSION"),
                [],
            )),
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
        let actor_name = require_actor_name(&req.sandbox_id, &req.name)?;
        let mut client = self.control_client().await?;
        let actor = client
            .get_actor(ateapi::GetActorRequest {
                actor: Some(ateapi::ObjectRef {
                    atespace: self.config.atespace.clone(),
                    name: actor_name.clone(),
                }),
            })
            .await
            .map_err(|status| {
                if status.code() == tonic::Code::NotFound {
                    Status::not_found(format!("sandbox {actor_name} not found"))
                } else {
                    status
                }
            })?
            .into_inner();

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
            .list_actors(ateapi::ListActorsRequest::default())
            .await?;
        let ns = self.config.atespace.as_str();
        let sandboxes = resp
            .into_inner()
            .actors
            .iter()
            .filter(|a| a.metadata.as_ref().is_some_and(|m| m.atespace == ns))
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
        validate_substrate_sandbox(&sandbox)?;
        let actor_name = require_actor_name(&sandbox.id, &sandbox.name)?;
        let atespace = self.config.atespace.clone();

        let template_spec = sandbox.spec.as_ref().and_then(|s| s.template.as_ref());
        let image = template_spec.map(|t| t.image.clone()).unwrap_or_default();
        let command = sandbox
            .spec
            .as_ref()
            .map(|s| s.command.clone())
            .unwrap_or_default();
        let template_name = template::template_name_for(&image, &command);

        let mut client = self.control_client().await?;
        template::ensure_ready(
            &mut client,
            &template_name,
            &atespace,
            &sandbox,
            &self.config,
        )
        .await
        .map_err(SubstrateDriverError::from)?;

        client
            .create_actor(ateapi::CreateActorRequest {
                actor: Some(ateapi::Actor {
                    metadata: Some(ateapi::ResourceMetadata {
                        atespace: atespace.clone(),
                        name: actor_name.clone(),
                        ..Default::default()
                    }),
                    actor_template: Some(ateapi::ObjectRef {
                        atespace: atespace.clone(),
                        name: template_name,
                    }),
                    ..Default::default()
                }),
            })
            .await?;
        client
            .resume_actor(ateapi::ResumeActorRequest {
                actor: Some(ateapi::ObjectRef {
                    atespace,
                    name: actor_name,
                }),
            })
            .await?;

        Ok(Response::new(CreateSandboxResponse::default()))
    }

    async fn start_sandbox(
        &self,
        request: Request<StartSandboxRequest>,
    ) -> Result<Response<StartSandboxResponse>, Status> {
        let req = request.into_inner();
        let actor_name = require_actor_name(&req.sandbox_id, &req.name)?;
        let mut client = self.control_client().await?;
        client
            .resume_actor(ateapi::ResumeActorRequest {
                actor: Some(ateapi::ObjectRef {
                    atespace: self.config.atespace.clone(),
                    name: actor_name,
                }),
            })
            .await?;
        Ok(Response::new(StartSandboxResponse::default()))
    }

    async fn stop_sandbox(
        &self,
        request: Request<StopSandboxRequest>,
    ) -> Result<Response<StopSandboxResponse>, Status> {
        let req = request.into_inner();
        let actor_name = require_actor_name(&req.sandbox_id, &req.name)?;
        let mut client = self.control_client().await?;
        // OpenShell "stop" -> Substrate "suspend" (checkpoint + free the
        // worker slot, snapshot retained).
        client
            .suspend_actor(ateapi::SuspendActorRequest {
                actor: Some(ateapi::ObjectRef {
                    atespace: self.config.atespace.clone(),
                    name: actor_name,
                }),
            })
            .await?;
        Ok(Response::new(StopSandboxResponse {}))
    }

    async fn delete_sandbox(
        &self,
        request: Request<DeleteSandboxRequest>,
    ) -> Result<Response<DeleteSandboxResponse>, Status> {
        let req = request.into_inner();
        let actor_name = require_actor_name(&req.sandbox_id, &req.name)?;
        let mut client = self.control_client().await?;
        // any_state: true -- delete regardless of RUNNING/SUSPENDED, no
        // separate best-effort-suspend-first dance needed.
        //
        // ponytail: templates are reused by content hash across actors
        // (see template.rs), so this deliberately does not garbage-collect
        // the ActorTemplate -- deleting it here could break a sibling
        // actor sharing the same golden snapshot. Add template GC (e.g. a
        // reference count, or a periodic sweep) if unused templates
        // measurably pile up.
        let result = client
            .delete_actor(ateapi::DeleteActorRequest {
                actor: Some(ateapi::ObjectRef {
                    atespace: self.config.atespace.clone(),
                    name: actor_name,
                }),
                any_state: true,
            })
            .await;
        let deleted = match result {
            Ok(_) => true,
            Err(status) if status.code() == tonic::Code::NotFound => false,
            Err(other) => return Err(other),
        };
        Ok(Response::new(DeleteSandboxResponse { deleted }))
    }

    async fn watch_sandboxes(
        &self,
        _request: Request<WatchSandboxesRequest>,
    ) -> Result<Response<Self::WatchSandboxesStream>, Status> {
        // ateapi.Control has no streaming watch RPC (substrate #153); this
        // materialises one by polling ListActors and diffing snapshots.
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
                let resp = match client
                    .list_actors(ateapi::ListActorsRequest::default())
                    .await
                {
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
                let ns = driver.config.atespace.as_str();
                let mut current: HashMap<String, DriverSandbox> = resp
                    .into_inner()
                    .actors
                    .into_iter()
                    .filter(|a| a.metadata.as_ref().is_some_and(|m| m.atespace == ns))
                    .map(|a| {
                        let sandbox = actor_to_driver_sandbox(&a);
                        (sandbox.id.clone(), sandbox)
                    })
                    .collect();

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

                for (id, sandbox) in &current {
                    let changed = prior.get(id) != Some(sandbox);
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

    async fn ensure_workspace(
        &self,
        _request: Request<EnsureWorkspaceRequest>,
    ) -> Result<Response<EnsureWorkspaceResponse>, Status> {
        let mut client = self.control_client().await?;
        match client
            .create_atespace(ateapi::CreateAtespaceRequest {
                atespace: Some(ateapi::Atespace {
                    metadata: Some(ateapi::ResourceMetadata {
                        name: self.config.atespace.clone(),
                        ..Default::default()
                    }),
                }),
            })
            .await
        {
            Ok(_) => {}
            Err(status) if status.code() == tonic::Code::AlreadyExists => {}
            Err(status) => return Err(status),
        }
        Ok(Response::new(EnsureWorkspaceResponse {}))
    }

    async fn delete_workspace(
        &self,
        _request: Request<DeleteWorkspaceRequest>,
    ) -> Result<Response<DeleteWorkspaceResponse>, Status> {
        // ponytail: no-op. The driver's atespace is shared across every
        // OpenShell workspace (see SubstrateComputeConfig::atespace), so
        // deleting it here on one workspace's teardown would break every
        // other workspace. Wire this up if/when workspaces get their own
        // atespace.
        Ok(Response::new(DeleteWorkspaceResponse {}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_are_sane() {
        let c = SubstrateComputeConfig::default();
        assert!(!c.api_endpoint.is_empty());
        assert!(!c.atespace.is_empty());
        assert!(!c.sandbox_config_name.is_empty());
        assert!(c.api_tls_ca_path.is_none());
    }

    #[tokio::test]
    async fn load_tls_config_none_when_unconfigured() {
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
        let mut interceptor = AuthInterceptor { bearer: None };
        let req = interceptor.call(Request::new(())).unwrap();
        assert!(req.metadata().get("authorization").is_none());
    }

    #[tokio::test]
    async fn load_tls_config_rejects_half_mtls_pair() {
        let driver = SubstrateComputeDriver::new(SubstrateComputeConfig {
            api_tls_ca_path: Some("/nonexistent/ca.pem".into()),
            api_client_cert_path: Some("/nonexistent/cert.pem".into()),
            api_client_key_path: None,
            ..Default::default()
        });
        let err = driver.load_tls_config().await.unwrap_err();
        assert!(matches!(err, SubstrateDriverError::TlsConfig { .. }));
    }

    #[test]
    fn require_actor_name_prefers_id_over_name() {
        assert_eq!(require_actor_name("id", "name").unwrap(), "id");
        assert_eq!(require_actor_name("", "name").unwrap(), "name");
        assert!(require_actor_name("", "").is_err());
    }

    #[test]
    fn actor_state_running_maps_to_ready_true() {
        let cond = actor_state_to_condition(ateapi::ActorState::Running);
        assert_eq!(cond.r#type, "Ready");
        assert_eq!(cond.status, "True");
    }
}
