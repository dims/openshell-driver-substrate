use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use openshell_core::SandboxSessionId;
use openshell_core::jwt::{
    CredentialEpoch, SandboxLaunchAuthentication, SandboxRuntimeIdentity, SessionJwtIssuer,
    SessionRotation, SessionVerificationKey, SupervisorAuthBundle, SystemJwtClock,
};
use openshell_core::sandbox_generation::SandboxGenerationId;
use openshell_isolation_interface::contract::{
    OuterFenceGuarantee, OuterFenceGuarantees, ResolvedWorkloadIdentity,
};
use openshell_sandbox_backend::boundary_protocol::{
    BoundaryConfig, BoundaryListener, GatewayVerificationKey, SandboxRuntimeDescriptor,
    SandboxTlsClientConfig, SandboxTlsServerConfig, SandboxTransport, generate_sandbox_tls_material,
};
use serde::Serialize;

#[derive(Serialize)]
struct SubstrateOuterFenceEvidence<'a> {
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
        Duration::from_secs(3600),
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

    let evidence = SubstrateOuterFenceEvidence {
        generation: generation_str,
        backend: "microvm",
    };
    let encoded = serde_json::to_vec(&evidence)?;
    let outer_fence = OuterFenceGuarantees::from_enforcement_evidence(
        generation_str,
        [
            OuterFenceGuarantee::DefaultDenyEgress,
            OuterFenceGuarantee::NoUnmanagedEgressPath,
            OuterFenceGuarantee::RevocationVerified,
            OuterFenceGuarantee::ControllerLossFailsClosed,
        ],
        &encoded,
    )?;
    outer_fence.validate(generation_str)?;

    // TCP on loopback, not a unix socket on a shared volume. Containers of one
    // actor share the guest's network namespace, so loopback reaches across
    // them; a socket on a durable-dir volume does not.
    let control_port: u16 = 17777;
    let bind_address = std::net::SocketAddr::from(([0, 0, 0, 0], control_port));
    let dial_address = std::net::SocketAddr::from(([127, 0, 0, 1], control_port));

    let boundary_config = BoundaryConfig {
        boundary_id: sandbox_id_str.to_string(),
        generation: generation_str.to_string(),
        session_id,
        session_rotation: SessionRotation::new(1)?,
        auth_epoch: CredentialEpoch::new(1)?,
        gateway_id: gateway_id.to_string(),
        verification_keys: vec![verification_key_gw],
        listener: BoundaryListener::TlsTcp {
            address: bind_address,
            tls: SandboxTlsServerConfig {
                certificate_chain_path: PathBuf::from("/.openshell/channel/sandbox/server.crt"),
                private_key_path: PathBuf::from("/.openshell/channel/sandbox/server.key"),
            },
        },
        resource_claims: BTreeMap::new(),
        resource_claim_files: BTreeMap::new(),
        workload_identity: workload_identity.clone(),
        outer_fence: outer_fence.clone(),
        child_env: HashMap::new(),
    };

    let runtime_descriptor = SandboxRuntimeDescriptor {
        boundary_id: sandbox_id_str.to_string(),
        generation: generation_str.to_string(),
        session_id,
        workload_identity,
        transport: SandboxTransport::Tcp {
            authority: tls.server_name.clone(),
            addresses: vec![dial_address],
        },
        tls: SandboxTlsClientConfig {
            server_name: tls.server_name.clone(),
            trust_anchor_pem: tls.trust_anchor_pem.clone(),
        },
        host_gateway_ip: None,
        resource_claims: BTreeMap::new(),
        outer_fence,
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
