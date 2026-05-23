// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Drop-in substrate-aware supervisor: registers [`DegradedHandler`]
//! and then delegates to the standard `openshell-sandbox` CLI.

use openshell_driver_substrate::DegradedHandler;
use openshell_sandbox::{cli, set_failure_handler};

fn main() -> miette::Result<()> {
    set_failure_handler(Box::new(DegradedHandler))
        .map_err(|_| miette::miette!("sandbox failure handler already registered"))?;
    cli::run()
}
