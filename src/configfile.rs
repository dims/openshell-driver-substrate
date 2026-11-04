// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! The driver's configuration file: what an operator decides once per
//! deployment, as the in-tree OpenShell drivers take it. A caller's sandbox
//! spec never changes any of this.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::{ateapi, fragment, template};

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
}

pub struct Loaded {
    pub template_fragment: Option<ateapi::ActorTemplate>,
    pub egress_rules: Vec<ateapi::EgressRule>,
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
    Ok(Loaded {
        template_fragment,
        egress_rules,
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
            "template_file = \"template.yaml\"\negress_policy_file = \"egress.yaml\"\n",
        )
        .unwrap();
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

        std::fs::write(dir.path().join("driver.toml"), "templte_file = \"x\"\n").unwrap();
        assert!(load(&dir.path().join("driver.toml")).is_err());
    }
}
