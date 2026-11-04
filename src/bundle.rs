// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! A sandbox's launch bundle from the gateway's launch authentication: the
//! files the supervisor and the sandbox read, built the way
//! openshell-driver-docker writes them, with the boundary on the actor
//! address.

use std::collections::{BTreeMap, HashMap};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;

use openshell_core::jwt::SandboxLaunchAuthentication;
use openshell_core::proto::compute::v1::DriverSandbox;
use openshell_core::sandbox_env::{self, MainProcessConfig};
use openshell_isolation_interface::contract::{ProvisioningEvidence, ResolvedWorkloadIdentity};
use openshell_sandbox_backend::boundary_protocol::{
    BoundaryConfig, BoundaryListener, GatewayVerificationKey, SandboxRuntimeDescriptor,
    SandboxTlsClientConfig, SandboxTlsServerConfig, SandboxTransport,
    generate_sandbox_tls_material,
};

use crate::launch::{Bundle, Gateway};

/// Every Substrate actor has this address (`internal/ateworker/actornet`).
/// The supervisor listens on it and the sandbox dials it: OpenShell refuses
/// a boundary listener on loopback and a supervisor peer on the sandbox's
/// own address, and the two containers share the guest's network namespace.
pub const CONTROL_ADDRESS: SocketAddr =
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(169, 254, 17, 2)), 17777);

/// Where the launcher puts the sandbox's half inside its container.
pub const SANDBOX_DIR: &str = "/.openshell/channel/sandbox";

/// The sandbox process's uid and gid; the sandbox image fixes them.
pub const SANDBOX_UID: u32 = 65532;

/// Build the bundle for one generation of a sandbox.
pub fn build(
    sandbox: &DriverSandbox,
    launch_authentication: &[u8],
    resume: bool,
    gateway: &Gateway,
) -> Result<Bundle, String> {
    let auth: SandboxLaunchAuthentication = serde_json::from_slice(launch_authentication)
        .map_err(|e| format!("launch authentication: {e}"))?;
    auth.validate()
        .map_err(|e| format!("launch authentication: {e}"))?;
    let supervisor = &auth.supervisor;
    let generation = supervisor.runtime_generation.to_string();
    let session_id = supervisor.session_id;
    let tls = generate_sandbox_tls_material(session_id).map_err(|e| e.to_string())?;

    let spec = sandbox.spec.as_ref();
    let image = spec
        .and_then(|s| s.template.as_ref())
        .map(|t| t.image.clone())
        .unwrap_or_default();
    let workload_identity = ResolvedWorkloadIdentity::new(
        SANDBOX_UID,
        SANDBOX_UID,
        Vec::new(),
        "image".to_string(),
        image,
    )
    .map_err(|e| e.to_string())?;
    // The sandbox and the supervisor only compare their copies of this.
    let evidence = ProvisioningEvidence::from_enforcement_evidence(
        &generation,
        format!(
            "{{\"backend\":\"agent-substrate\",\"actor\":{:?}}}",
            sandbox.id
        )
        .as_bytes(),
    )
    .map_err(|e| e.to_string())?;
    let verification_keys = auth
        .verification_keys
        .iter()
        .map(|key| {
            Ok(GatewayVerificationKey {
                key_id: key.key_id.clone(),
                public_key_pem: String::from_utf8(key.public_key_pem.clone())
                    .map_err(|e| format!("verification key {}: {e}", key.key_id))?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;

    let boundary = BoundaryConfig {
        boundary_id: sandbox.id.clone(),
        generation: generation.clone(),
        session_id,
        session_rotation: supervisor.session_rotation,
        auth_epoch: supervisor.auth_epoch,
        gateway_id: auth.gateway_id.clone(),
        verification_keys,
        listener: BoundaryListener::RuntimeInitiatedTlsTcp {
            authority: tls.server_name.clone(),
            addresses: vec![CONTROL_ADDRESS],
            tls: SandboxTlsServerConfig {
                certificate_chain_path: PathBuf::from(format!("{SANDBOX_DIR}/server.crt")),
                private_key_path: PathBuf::from(format!("{SANDBOX_DIR}/server.key")),
            },
            route: None,
        },
        resource_claims: BTreeMap::new(),
        resource_claim_files: BTreeMap::new(),
        workload_identity: workload_identity.clone(),
        provisioning_evidence: evidence.clone(),
        child_env: child_env(sandbox),
    };
    let descriptor = SandboxRuntimeDescriptor {
        boundary_id: sandbox.id.clone(),
        generation: generation.clone(),
        session_id,
        workload_identity,
        transport: SandboxTransport::RuntimeInitiatedTcp {
            authority: tls.server_name.clone(),
            listen_address: CONTROL_ADDRESS,
        },
        tls: SandboxTlsClientConfig {
            server_name: tls.server_name.clone(),
            trust_anchor_pem: tls.trust_anchor_pem.clone(),
        },
        host_gateway_ip: None,
        resource_claims: BTreeMap::new(),
        provisioning_evidence: evidence,
    };
    let epoch = serde_json::to_value(supervisor.auth_epoch)
        .ok()
        .and_then(|v| v.as_u64())
        .ok_or("auth epoch is not a number")?;
    Ok(Bundle {
        generation,
        epoch,
        resume,
        gateway: gateway.clone(),
        sandbox_id: sandbox.id.clone(),
        sandbox_name: sandbox.name.clone(),
        log_level: spec
            .map(|s| s.log_level.clone())
            .filter(|l| !l.is_empty())
            .unwrap_or_else(|| "info".to_string()),
        supervisor: BTreeMap::from([
            ("runtime-descriptor.json".to_string(), json(&descriptor)?),
            ("auth.json".to_string(), json(supervisor)?),
        ]),
        sandbox: BTreeMap::from([
            ("bootstrap.json".to_string(), json(&boundary)?),
            ("server.crt".to_string(), tls.certificate_chain_pem),
            ("server.key".to_string(), tls.private_key_pem),
        ]),
        main_process_spec: Some(
            MainProcessConfig::encode_driver_spec(spec).map_err(|e| e.to_string())?,
        ),
    })
}

fn json<T: serde::Serialize>(value: &T) -> Result<String, String> {
    serde_json::to_string(value).map_err(|e| e.to_string())
}

/// The workload's environment: the template's, then the spec's over it,
/// minus the names OpenShell reserves for itself.
fn child_env(sandbox: &DriverSandbox) -> HashMap<String, String> {
    let spec = sandbox.spec.as_ref();
    let mut env: HashMap<String, String> = spec
        .and_then(|s| s.template.as_ref())
        .map(|t| t.environment.clone())
        .unwrap_or_default();
    if let Some(spec) = spec {
        env.extend(spec.environment.clone());
    }
    for reserved in [
        sandbox_env::ENDPOINT,
        sandbox_env::GATEWAY_TLS_SERVER_NAME,
        sandbox_env::MAIN_PROCESS_SPEC,
        sandbox_env::NETWORK_RUNTIME_CAPABILITIES,
        sandbox_env::OCI_IMAGE_USER,
        sandbox_env::SANDBOX,
        sandbox_env::SANDBOX_GID,
        sandbox_env::SANDBOX_ID,
        sandbox_env::SANDBOX_TOKEN,
        sandbox_env::SANDBOX_TOKEN_FILE,
        sandbox_env::SANDBOX_UID,
        sandbox_env::SSH_SOCKET_PATH,
        sandbox_env::TLS_CA,
        sandbox_env::TLS_CERT,
        sandbox_env::TLS_KEY,
        sandbox_env::USER_ENVIRONMENT,
    ] {
        env.remove(reserved);
    }
    env
}
