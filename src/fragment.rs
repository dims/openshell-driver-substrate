// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Decoding Substrate messages from protojson, for the ActorTemplate
//! fragment and the EgressPolicy a driver configuration file carries.

use std::sync::OnceLock;

use prost::Message;
use prost_reflect::{DescriptorPool, DynamicMessage, MessageDescriptor};
use serde::Deserializer;

/// Written by build.rs next to the generated client.
static ATEAPI_DESCRIPTORS: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/ateapi.bin"));

fn descriptor(name: &str) -> MessageDescriptor {
    static POOL: OnceLock<DescriptorPool> = OnceLock::new();
    POOL.get_or_init(|| {
        DescriptorPool::decode(ATEAPI_DESCRIPTORS).expect("descriptor set from build.rs")
    })
    .get_message_by_name(name)
    .unwrap_or_else(|| panic!("{name} is not in ateapi.proto"))
}

/// Decode `substrate.ateapi.v1.<name>` the protojson way: field names in
/// either case, enums by name, int64 as strings or numbers. An unknown field
/// is an error.
pub fn decode<'de, T: Message + Default>(
    name: &str,
    de: impl Deserializer<'de>,
) -> Result<T, String> {
    DynamicMessage::deserialize(descriptor(name), de)
        .map_err(|e| e.to_string())?
        .transcode_to()
        .map_err(|e| e.to_string())
}

/// `decode` from YAML text, which also covers JSON.
pub fn from_yaml<T: Message + Default>(name: &str, text: &str) -> Result<T, String> {
    decode(name, serde_yaml_ng::Deserializer::from_str(text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ateapi;

    const TEMPLATE: &str = "substrate.ateapi.v1.ActorTemplate";

    #[test]
    fn decodes_protojson_in_either_case_and_rejects_unknown_fields() {
        let t: ateapi::ActorTemplate = from_yaml(
            TEMPLATE,
            "containers:\n- name: a\n  image: img@sha256:abc\nsnapshotConfig:\n  preferredFidelity: SNAPSHOT_FIDELITY_MEMORY\nresources:\n  limits:\n    cpu_millicores: '1000'\n    memoryBytes: 1073741824\n",
        )
        .unwrap();
        assert_eq!(t.containers[0].image, "img@sha256:abc");
        assert_eq!(
            t.snapshot_config.unwrap().preferred_fidelity,
            ateapi::SnapshotFidelity::Memory as i32
        );
        assert_eq!(
            t.resources.unwrap().limits.unwrap(),
            ateapi::Resources {
                cpu_millicores: 1000,
                memory_bytes: 1 << 30,
            }
        );

        let err = from_yaml::<ateapi::ActorTemplate>(
            TEMPLATE,
            "containers:\n- name: a\n  imagePullPolicy: Always\n",
        )
        .unwrap_err();
        assert!(err.contains("imagePullPolicy"), "{err}");

        let p: ateapi::EgressPolicy = from_yaml(
            "substrate.ateapi.v1.EgressPolicy",
            "rules:\n- http:\n    hostnames: [model.example]\n    ports:\n      numbers: [11434]\n",
        )
        .unwrap();
        assert_eq!(p.rules.len(), 1);
    }
}
