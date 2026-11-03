use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;

use openshell_core::SandboxSessionId;
use openshell_core::jwt::{
    CredentialEpoch, SandboxLaunchAuthentication, SandboxRuntimeIdentity, SessionJwtIssuer,
    SessionRotation, SessionVerificationKey, SupervisorAuthBundle, SystemJwtClock,
};
use openshell_core::sandbox_generation::SandboxGenerationId;
use openshell_isolation_interface::contract::{ProvisioningEvidence, ResolvedWorkloadIdentity};
use openshell_sandbox_backend::boundary_protocol::{
    BoundaryConfig, BoundaryListener, GatewayVerificationKey, SandboxRuntimeDescriptor,
    SandboxTlsClientConfig, SandboxTlsServerConfig, SandboxTransport,
    generate_sandbox_tls_material,
};
use serde::Serialize;

/// Where the supervisor listens and the sandbox dials. Every Substrate actor
/// has this address (`internal/ateworker/actornet`), and the containers of one
/// actor share the guest's network namespace. OpenShell refuses a boundary
/// listener on loopback and a supervisor peer on the sandbox's own address, so
/// the supervisor is the listener and the sandbox connects to it.
const CONTROL_ADDRESS: SocketAddr =
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(169, 254, 17, 2)), 17777);

#[derive(Serialize)]
struct SubstrateProvisioningEvidence<'a> {
    generation: &'a str,
    backend: &'static str,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = PathBuf::from(
        std::env::args()
            .nth(1)
            .expect("usage: bootstrap-gen <out-dir>"),
    );

    let signing_key_pem = std::fs::read(out_dir.join("signing.key.pem"))?;
    let public_key_pem = std::fs::read_to_string(out_dir.join("signing.pub.pem"))?;

    let gateway_id = "substrate-driver-test";
    let sandbox_id_str = "test-sandbox-1";
    let generation_str = "generation-1";

    let issuer = SessionJwtIssuer::from_ed25519_pem(
        &signing_key_pem,
        "test-key-1",
        gateway_id,
        // Non-expiring: a gateway would refresh the tokens, this harness has
        // nothing to refresh from, and a snapshot can sit for days.
        None,
        Arc::new(SystemJwtClock),
    )?;

    let identity = SandboxRuntimeIdentity {
        sandbox_id: openshell_core::jwt::SandboxId::parse(sandbox_id_str)?,
        runtime_generation: SandboxGenerationId::parse(generation_str)?,
        auth_epoch: CredentialEpoch::new(1)?,
    };
    let minted = issuer.mint_pair(&identity)?;

    let session_id = SandboxSessionId::new();
    let tls = generate_sandbox_tls_material(session_id)?;

    let verification_key_gw = GatewayVerificationKey {
        key_id: "test-key-1".to_string(),
        public_key_pem: public_key_pem.clone(),
    };
    let verification_key_session = SessionVerificationKey {
        key_id: "test-key-1".to_string(),
        public_key_pem: public_key_pem.into_bytes(),
    };

    let workload_identity = ResolvedWorkloadIdentity::new(
        65532,
        65532,
        Vec::new(),
        "test".to_string(),
        "sha256:test".to_string(),
    )?;

    let evidence = SubstrateProvisioningEvidence {
        generation: generation_str,
        backend: "microvm",
    };
    let encoded = serde_json::to_vec(&evidence)?;
    let provisioning_evidence =
        ProvisioningEvidence::from_enforcement_evidence(generation_str, &encoded)?;
    provisioning_evidence.validate(generation_str)?;

    let boundary_config = BoundaryConfig {
        boundary_id: sandbox_id_str.to_string(),
        generation: generation_str.to_string(),
        session_id,
        session_rotation: SessionRotation::new(1)?,
        auth_epoch: CredentialEpoch::new(1)?,
        gateway_id: gateway_id.to_string(),
        verification_keys: vec![verification_key_gw],
        listener: BoundaryListener::RuntimeInitiatedTlsTcp {
            authority: tls.server_name.clone(),
            addresses: vec![CONTROL_ADDRESS],
            tls: SandboxTlsServerConfig {
                certificate_chain_path: PathBuf::from("/.openshell/channel/sandbox/server.crt"),
                private_key_path: PathBuf::from("/.openshell/channel/sandbox/server.key"),
            },
            route: None,
        },
        resource_claims: BTreeMap::new(),
        resource_claim_files: BTreeMap::new(),
        workload_identity: workload_identity.clone(),
        provisioning_evidence: provisioning_evidence.clone(),
        // BOOTSTRAP_CHILD_ENV="A=1,B=2" reaches the workload's environment.
        child_env: std::env::var("BOOTSTRAP_CHILD_ENV")
            .unwrap_or_default()
            .split(',')
            .filter_map(|kv| kv.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    };

    let runtime_descriptor = SandboxRuntimeDescriptor {
        boundary_id: sandbox_id_str.to_string(),
        generation: generation_str.to_string(),
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
        provisioning_evidence,
    };

    let auth_bundle = SupervisorAuthBundle {
        session_id,
        runtime_generation: SandboxGenerationId::parse(generation_str)?,
        session_rotation: SessionRotation::new(1)?,
        auth_epoch: CredentialEpoch::new(1)?,
        gateway_token: minted.gateway.token,
        gateway_expires_at: minted.gateway.expires_at,
        sandbox_token: minted.sandbox.token,
        sandbox_expires_at: minted.sandbox.expires_at,
        ssh_host_private_key: None,
        traces_otlp_headers: None,
        ocsf_otlp_headers: None,
        agent_otlp_headers: None,
        otlp_tls: Vec::new(),
        traces_otlp_tls: None,
        ocsf_otlp_tls: None,
        agent_otlp_tls: None,
    };

    let launch_authentication = SandboxLaunchAuthentication {
        supervisor: auth_bundle.clone(),
        gateway_id: gateway_id.to_string(),
        verification_keys: vec![verification_key_session],
    };
    launch_authentication.validate()?;

    std::fs::write(
        out_dir.join("bootstrap.json"),
        serde_json::to_vec_pretty(&boundary_config)?,
    )?;
    std::fs::write(out_dir.join("server.crt"), &tls.certificate_chain_pem)?;
    std::fs::write(out_dir.join("server.key"), &tls.private_key_pem)?;
    std::fs::write(
        out_dir.join("runtime-descriptor.json"),
        serde_json::to_vec_pretty(&runtime_descriptor)?,
    )?;
    std::fs::write(
        out_dir.join("auth.json"),
        serde_json::to_vec_pretty(&auth_bundle)?,
    )?;

    println!("wrote bootstrap files to {}", out_dir.display());
    Ok(())
}
