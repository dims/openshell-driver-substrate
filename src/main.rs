// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Standalone binary serving `openshell.compute.v1.ComputeDriver` over a
//! Unix socket. Point an unmodified OpenShell gateway at the socket (the
//! same out-of-process pattern its in-tree Docker/Podman/Kubernetes
//! drivers use) to hand it Substrate as a compute backend.

use std::path::PathBuf;

use clap::Parser;
use openshell_core::proto::compute::v1::compute_driver_server::ComputeDriverServer;
use openshell_driver_substrate::{SubstrateComputeConfig, SubstrateComputeDriver, configfile};

#[derive(Parser)]
#[command(name = "openshell-driver-substrate")]
struct Args {
    /// Unix socket to serve the compute-driver gRPC service on.
    #[arg(long, env = "OPENSHELL_COMPUTE_DRIVER_SOCKET")]
    bind_socket: PathBuf,

    /// gRPC endpoint of Substrate's `ate-api-server`.
    #[arg(long, env = "SUBSTRATE_API_ENDPOINT", default_value = "127.0.0.1:8080")]
    api_endpoint: String,

    /// Substrate atespace every Actor/ActorTemplate is created in.
    #[arg(long, env = "SUBSTRATE_ATESPACE", default_value = "default")]
    atespace: String,

    /// Worker labels the synthesized ActorTemplates select, as key=value pairs
    /// separated by commas. The pool they name decides the sandbox class.
    #[arg(
        long,
        env = "SUBSTRATE_WORKER_SELECTOR",
        default_value = "workload=openshell-microvm"
    )]
    worker_selector: String,

    /// Seconds to wait for a synthesized ActorTemplate's golden snapshot.
    #[arg(
        long,
        env = "SUBSTRATE_TEMPLATE_READY_TIMEOUT_SECS",
        default_value_t = 180
    )]
    template_ready_timeout_secs: u64,

    /// OpenShell gateway endpoint injected into actors as OPENSHELL_ENDPOINT.
    #[arg(long, env = "OPENSHELL_GATEWAY_ENDPOINT", default_value = "")]
    gateway_endpoint: String,

    /// CA bundle verifying ate-api-server's certificate. Unset dials plaintext.
    #[arg(long, env = "SUBSTRATE_API_TLS_CA")]
    api_tls_ca: Option<PathBuf>,

    /// mTLS client certificate (pairs with --api-tls-key).
    #[arg(long, env = "SUBSTRATE_API_TLS_CERT")]
    api_tls_cert: Option<PathBuf>,

    /// mTLS client private key (pairs with --api-tls-cert).
    #[arg(long, env = "SUBSTRATE_API_TLS_KEY")]
    api_tls_key: Option<PathBuf>,

    /// TLS server-name override for ate-api-server's certificate.
    #[arg(long, env = "SUBSTRATE_API_TLS_SERVER_NAME")]
    api_tls_server_name: Option<String>,

    /// Bearer token file (e.g. a projected ServiceAccount token),
    /// re-read on every reconnect.
    #[arg(long, env = "SUBSTRATE_API_BEARER_TOKEN_PATH")]
    api_bearer_token_path: Option<PathBuf>,

    /// Configuration file (TOML): the ActorTemplate fragment and the egress
    /// policy every sandbox gets.
    #[arg(long, env = "SUBSTRATE_DRIVER_CONFIG")]
    config: Option<PathBuf>,

    #[arg(long, env = "OPENSHELL_LOG_LEVEL", default_value = "info")]
    log_level: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(&args.log_level))
        .init();

    let worker_selector: std::collections::BTreeMap<String, String> = args
        .worker_selector
        .split(',')
        .filter(|kv| !kv.is_empty())
        .map(|kv| {
            kv.split_once('=')
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .ok_or_else(|| format!("--worker-selector: {kv:?} is not key=value"))
        })
        .collect::<Result<_, _>>()?;
    if worker_selector.is_empty() {
        return Err("--worker-selector needs at least one key=value".into());
    }
    let loaded = match &args.config {
        Some(path) => configfile::load(path)?,
        None => configfile::Loaded {
            template_fragment: None,
            egress_rules: Vec::new(),
        },
    };
    let driver = SubstrateComputeDriver::new(SubstrateComputeConfig {
        api_endpoint: args.api_endpoint,
        atespace: args.atespace,
        worker_selector,
        template_ready_timeout_secs: args.template_ready_timeout_secs,
        gateway_endpoint: args.gateway_endpoint,
        api_tls_ca_path: args.api_tls_ca,
        api_client_cert_path: args.api_tls_cert,
        api_client_key_path: args.api_tls_key,
        api_tls_server_name: args.api_tls_server_name,
        api_bearer_token_path: args.api_bearer_token_path,
        template_fragment: loaded.template_fragment,
        egress_rules: loaded.egress_rules,
        ..Default::default()
    });

    if let Some(parent) = args.bind_socket.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // A stale socket from an earlier run is removed; a live one is not.
    if tokio::net::UnixStream::connect(&args.bind_socket)
        .await
        .is_ok()
    {
        return Err(format!("{} is already served", args.bind_socket.display()).into());
    }
    let _ = std::fs::remove_file(&args.bind_socket);
    let listener = tokio::net::UnixListener::bind(&args.bind_socket)?;
    tracing::info!(socket = %args.bind_socket.display(), "starting openshell-driver-substrate");

    // Reflection, so grpcurl needs no proto files. v1alpha serves older clients.
    let reflection = || {
        tonic_reflection::server::Builder::configure()
            .register_encoded_file_descriptor_set(openshell_core::FILE_DESCRIPTOR_SET)
            .with_service_name("openshell.compute.v1.ComputeDriver")
    };
    tonic::transport::Server::builder()
        .add_service(ComputeDriverServer::new(driver))
        .add_service(reflection().build_v1()?)
        .add_service(reflection().build_v1alpha()?)
        .serve_with_incoming(tokio_stream::wrappers::UnixListenerStream::new(listener))
        .await?;
    Ok(())
}
