// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! The driver's configuration file: what an operator decides once per
//! deployment, as the in-tree OpenShell drivers take it. A caller's sandbox
//! spec never changes any of this.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::{ateapi, fragment, launch, template};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    /// An ActorTemplate fragment in YAML with protojson field names: the
    /// containers, volumes, resources and snapshot config every sandbox gets.
    /// The request's image goes into the container named `sandbox`.
    template_file: Option<PathBuf>,
    /// An EgressPolicy in YAML; its rules go on every sandbox's actor before
    /// the first resume.
    egress_policy_file: Option<PathBuf>,
    /// The launch endpoint. Without it the driver hands out no bundles and
    /// sandboxes run with whatever credentials their image carries.
    launch: Option<LaunchSettings>,
    /// The gateway the supervisors dial, put into every bundle.
    gateway: Option<GatewaySettings>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchSettings {
    /// Where the endpoint listens, such as `0.0.0.0:17790`.
    pub listen: String,
    /// Bundles are kept under this directory until their sandbox is deleted.
    pub state_dir: PathBuf,
    /// The actor JWT issuer and the audience the egress rule asks for.
    pub issuer: String,
    pub audience: String,
    /// The IdP's JWKS URL, and the CA that signs its certificate.
    pub jwks_url: String,
    pub jwks_ca_file: Option<PathBuf>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GatewaySettings {
    /// What the supervisors dial, `host:port`.
    endpoint: String,
    tls_ca_file: Option<PathBuf>,
    tls_server_name: Option<String>,
}

pub struct Loaded {
    pub template_fragment: Option<ateapi::ActorTemplate>,
    pub egress_rules: Vec<ateapi::EgressRule>,
    pub launch: Option<LaunchSettings>,
    pub gateway: Option<launch::Gateway>,
}

/// Read a TOML file; paths in it are relative to its directory.
pub fn load(path: &Path) -> Result<Loaded, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let file: File = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let read = |name: &Path| -> Result<(PathBuf, String), String> {
        let p = dir.join(name);
        let text = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        Ok((p, text))
    };

    let template_fragment = match file.template_file {
        Some(name) => {
            let (p, text) = read(&name)?;
            let fragment: ateapi::ActorTemplate =
                fragment::from_yaml("substrate.ateapi.v1.ActorTemplate", &text)
                    .map_err(|e| format!("{}: {e}", p.display()))?;
            template::check_fragment(&fragment).map_err(|e| format!("{}: {e}", p.display()))?;
            Some(fragment)
        }
        None => None,
    };
    let egress_rules = match file.egress_policy_file {
        Some(name) => {
            let (p, text) = read(&name)?;
            let policy: ateapi::EgressPolicy =
                fragment::from_yaml("substrate.ateapi.v1.EgressPolicy", &text)
                    .map_err(|e| format!("{}: {e}", p.display()))?;
            if policy.rules.is_empty() {
                return Err(format!("{}: no rules", p.display()));
            }
            policy.rules
        }
        None => Vec::new(),
    };
    let launch = file.launch.map(|mut launch| {
        launch.state_dir = dir.join(&launch.state_dir);
        launch.jwks_ca_file = launch.jwks_ca_file.map(|p| dir.join(p));
        launch
    });
    let gateway = match file.gateway {
        Some(gateway) => Some(launch::Gateway {
            endpoint: gateway.endpoint,
            tls_ca_pem: match gateway.tls_ca_file {
                Some(name) => Some(read(&name)?.1),
                None => None,
            },
            tls_server_name: gateway.tls_server_name,
        }),
        None => None,
    };
    Ok(Loaded {
        template_fragment,
        egress_rules,
        launch,
        gateway,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_the_fragment_and_the_rules_next_to_the_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("driver.toml"),
            "template_file = \"template.yaml\"\negress_policy_file = \"egress.yaml\"\n\n[launch]\nlisten = \"127.0.0.1:0\"\nstate_dir = \"state\"\nissuer = \"https://idp.ate-system.svc\"\naudience = \"driver\"\njwks_url = \"https://idp.ate-system.svc/openid/v1/jwks\"\n\n[gateway]\nendpoint = \"gateway.example:17670\"\ntls_ca_file = \"gateway-ca.pem\"\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("gateway-ca.pem"), "PEM").unwrap();
        std::fs::write(
            dir.path().join("template.yaml"),
            "containers:\n- name: sandbox\n  image: placeholder@sha256:p\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("egress.yaml"),
            "rules:\n- http:\n    hostnames: [model.example]\n",
        )
        .unwrap();
        let loaded = load(&dir.path().join("driver.toml")).unwrap();
        assert_eq!(loaded.template_fragment.unwrap().containers.len(), 1);
        assert_eq!(loaded.egress_rules.len(), 1);
        assert_eq!(loaded.launch.unwrap().state_dir, dir.path().join("state"));
        assert_eq!(loaded.gateway.unwrap().tls_ca_pem.as_deref(), Some("PEM"));

        std::fs::write(dir.path().join("driver.toml"), "templte_file = \"x\"\n").unwrap();
        assert!(load(&dir.path().join("driver.toml")).is_err());
    }
}
