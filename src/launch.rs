// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! The launch endpoint: where a launcher inside an actor fetches the bundle
//! the gateway minted for its sandbox. The actor proves who it is with the
//! actor JWT that atenet-egress puts on the request; the driver verifies it
//! against the IdP's published keys and answers with that actor's bundle
//! when it is newer than the generation the launcher already has, or 204
//! after a long poll.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, watch};

const LONG_POLL: Duration = Duration::from_secs(20);

/// What a sandbox needs to start or to take new credentials: the files for
/// each container and where the gateway is.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Bundle {
    pub generation: String,
    pub epoch: u64,
    /// The gateway resumed the sandbox: the supervisor is replaced and the
    /// sandbox and its workload stay. False restarts all of it.
    pub resume: bool,
    pub gateway: Gateway,
    /// What the supervisor is told it serves, and how loud it is.
    pub sandbox_id: String,
    pub sandbox_name: String,
    pub log_level: String,
    /// File name to contents for the supervisor container.
    pub supervisor: BTreeMap<String, String>,
    /// File name to contents for the sandbox container.
    pub sandbox: BTreeMap<String, String>,
    /// `OPENSHELL_MAIN_PROCESS_SPEC` for the supervisor, when the spec names
    /// a command.
    pub main_process_spec: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Gateway {
    pub endpoint: String,
    pub tls_ca_pem: Option<String>,
    pub tls_server_name: Option<String>,
}

/// Bundles by actor name, kept on disk under `dir` so a driver restart
/// loses none.
pub struct BundleStore {
    dir: PathBuf,
    bundles: Mutex<HashMap<String, Bundle>>,
    changed: watch::Sender<u64>,
}

impl BundleStore {
    pub fn open(dir: PathBuf) -> std::io::Result<Self> {
        std::fs::create_dir_all(&dir)?;
        let mut bundles = HashMap::new();
        for entry in std::fs::read_dir(&dir)? {
            let path = entry?.path();
            let Some(name) = path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_suffix(".json"))
            else {
                continue;
            };
            let bundle: Bundle = serde_json::from_slice(&std::fs::read(&path)?).map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("{}: {e}", path.display()),
                )
            })?;
            bundles.insert(name.to_string(), bundle);
        }
        Ok(Self {
            dir,
            bundles: Mutex::new(bundles),
            changed: watch::Sender::new(0),
        })
    }

    pub async fn put(&self, name: &str, bundle: Bundle) -> std::io::Result<()> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let path = self.dir.join(format!("{name}.json"));
        let tmp = self.dir.join(format!("{name}.json.tmp"));
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?
            .write_all(&serde_json::to_vec(&bundle)?)?;
        std::fs::rename(&tmp, &path)?;
        self.bundles.lock().await.insert(name.to_string(), bundle);
        self.changed.send_modify(|v| *v += 1);
        Ok(())
    }

    pub async fn get(&self, name: &str) -> Option<Bundle> {
        self.bundles.lock().await.get(name).cloned()
    }

    /// The gateway's sandbox name for each actor that has a bundle.
    pub async fn names(&self) -> HashMap<String, String> {
        self.bundles
            .lock()
            .await
            .iter()
            .map(|(actor, bundle)| (actor.clone(), bundle.sandbox_name.clone()))
            .collect()
    }

    pub async fn remove(&self, name: &str) -> std::io::Result<()> {
        self.bundles.lock().await.remove(name);
        match std::fs::remove_file(self.dir.join(format!("{name}.json"))) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
}

/// Verifies Substrate actor JWTs against the IdP's published keys.
pub struct ActorJwtVerifier {
    issuer: String,
    audience: String,
    jwks_url: String,
    http: reqwest::Client,
    keys: Mutex<Option<JwkSet>>,
}

#[derive(Deserialize)]
struct Claims {
    sub: String,
}

impl ActorJwtVerifier {
    pub fn new(
        issuer: String,
        audience: String,
        jwks_url: String,
        ca_pem: Option<&[u8]>,
    ) -> Result<Self, String> {
        let mut http = reqwest::Client::builder()
            .use_rustls_tls()
            .timeout(Duration::from_secs(10));
        if let Some(pem) = ca_pem {
            http = http.add_root_certificate(
                reqwest::Certificate::from_pem(pem).map_err(|e| e.to_string())?,
            );
        }
        Ok(Self {
            issuer,
            audience,
            jwks_url,
            http: http.build().map_err(|e| e.to_string())?,
            keys: Mutex::new(None),
        })
    }

    /// The actor the token names, as `atespace/name`.
    pub async fn verify(&self, token: &str) -> Result<String, String> {
        let header = jsonwebtoken::decode_header(token).map_err(|e| e.to_string())?;
        if !matches!(header.alg, Algorithm::ES256 | Algorithm::RS256) {
            return Err(format!("algorithm {:?} is not accepted", header.alg));
        }
        let kid = header.kid.ok_or("token has no kid")?;
        let key = match self.key(&kid, false).await? {
            Some(key) => key,
            None => self
                .key(&kid, true)
                .await?
                .ok_or_else(|| format!("unknown key {kid}"))?,
        };
        let mut validation = Validation::new(header.alg);
        validation.set_issuer(&[&self.issuer]);
        validation.set_audience(&[&self.audience]);
        let claims = jsonwebtoken::decode::<Claims>(token, &key, &validation)
            .map_err(|e| e.to_string())?
            .claims;
        claims
            .sub
            .strip_prefix("actor/")
            .map(str::to_string)
            .ok_or_else(|| format!("subject {} is not an actor", claims.sub))
    }

    async fn key(&self, kid: &str, refresh: bool) -> Result<Option<DecodingKey>, String> {
        let mut keys = self.keys.lock().await;
        if refresh || keys.is_none() {
            let set: JwkSet = self
                .http
                .get(&self.jwks_url)
                .send()
                .await
                .and_then(reqwest::Response::error_for_status)
                .map_err(|e| format!("{}: {e}", self.jwks_url))?
                .json()
                .await
                .map_err(|e| format!("{}: {e}", self.jwks_url))?;
            *keys = Some(set);
        }
        let Some(jwk) = keys.as_ref().and_then(|set| set.find(kid)) else {
            return Ok(None);
        };
        DecodingKey::from_jwk(jwk)
            .map(Some)
            .map_err(|e| e.to_string())
    }
}

pub struct LaunchService {
    pub atespace: String,
    pub store: Arc<BundleStore>,
    pub verifier: ActorJwtVerifier,
}

#[derive(Deserialize)]
struct LaunchQuery {
    generation: Option<String>,
}

pub fn router(service: Arc<LaunchService>) -> Router {
    Router::new()
        .route("/v1/launch/{atespace}/{name}", get(launch))
        .with_state(service)
}

async fn launch(
    State(svc): State<Arc<LaunchService>>,
    Path((atespace, name)): Path<(String, String)>,
    Query(query): Query<LaunchQuery>,
    headers: HeaderMap,
) -> Response {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    let Some(token) = token else {
        tracing::warn!(%atespace, %name, "launch request without a bearer token");
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let actor = match svc.verifier.verify(token).await {
        Ok(actor) => actor,
        Err(err) => {
            tracing::warn!(%atespace, %name, %err, "launch request refused");
            return StatusCode::UNAUTHORIZED.into_response();
        }
    };
    if atespace != svc.atespace || actor != format!("{atespace}/{name}") {
        tracing::warn!(%atespace, %name, %actor, "launch request for another actor");
        return StatusCode::FORBIDDEN.into_response();
    }
    let mut changes = svc.store.changed.subscribe();
    let deadline = tokio::time::Instant::now() + LONG_POLL;
    loop {
        if let Some(bundle) = svc.store.get(&name).await
            && query.generation.as_deref() != Some(bundle.generation.as_str())
        {
            tracing::info!(%name, generation = %bundle.generation, "bundle handed out");
            return Json(bundle).into_response();
        }
        tokio::select! {
            _ = changes.changed() => {}
            () = tokio::time::sleep_until(deadline) => return StatusCode::NO_CONTENT.into_response(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundle(generation: &str) -> Bundle {
        Bundle {
            generation: generation.to_string(),
            epoch: 1,
            resume: false,
            gateway: Gateway::default(),
            sandbox_id: "alice".to_string(),
            sandbox_name: "alice".to_string(),
            log_level: "info".to_string(),
            supervisor: BTreeMap::from([("auth.json".to_string(), "{}".to_string())]),
            sandbox: BTreeMap::new(),
            main_process_spec: None,
        }
    }

    #[tokio::test]
    async fn store_keeps_bundles_across_reopen_with_private_files() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let store = BundleStore::open(dir.path().to_path_buf()).unwrap();
        store.put("alice", bundle("g1")).await.unwrap();
        let mode = std::fs::metadata(dir.path().join("alice.json"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        drop(store);
        let store = BundleStore::open(dir.path().to_path_buf()).unwrap();
        assert_eq!(store.get("alice").await, Some(bundle("g1")));
        store.remove("alice").await.unwrap();
        store.remove("alice").await.unwrap();
        assert_eq!(store.get("alice").await, None);
    }
}
