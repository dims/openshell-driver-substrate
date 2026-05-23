// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Compile the vendored Substrate protos into a tonic client.

use std::env;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=proto");

    // Match openshell-core's pattern: use the bundled protoc from
    // protobuf-src so the build does not depend on a system protoc that
    // may lack the well-known type includes.
    //
    // SAFETY: build scripts are single-threaded.
    #[allow(unsafe_code)]
    unsafe {
        env::set_var("PROTOC", protobuf_src::protoc());
    }

    tonic_build::configure()
        .build_server(false) // client-only -- the driver speaks to ate-api-server
        .build_client(true)
        .compile_protos(&["proto/ateapi.proto"], &["proto"])?;

    Ok(())
}
