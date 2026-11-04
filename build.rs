// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Compile the vendored Substrate protos into a tonic client.

use std::env;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=proto");

    // The same vendored protoc and include tree openshell-core builds with, so
    // the build depends on no system protoc and needs no compile of libprotobuf.
    //
    // SAFETY: build scripts are single-threaded.
    #[allow(unsafe_code)]
    unsafe {
        env::set_var("PROTOC", protoc_bin_vendored::protoc_bin_path()?);
        env::set_var("PROTOC_INCLUDE", protoc_bin_vendored::include_path()?);
    }

    let out_dir = std::path::PathBuf::from(env::var("OUT_DIR")?);
    tonic_prost_build::configure()
        .build_server(false) // client-only -- the driver speaks to ate-api-server
        .build_client(true)
        // Deterministic map encoding: template names hash the encoded template.
        .btree_map(".")
        // fragment.rs decodes protojson through this descriptor set.
        .file_descriptor_set_path(out_dir.join("ateapi.bin"))
        .compile_protos(&["proto/ateapi.proto"], &["proto"])?;

    Ok(())
}
