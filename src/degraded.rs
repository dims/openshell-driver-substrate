// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Substrate-side [`openshell_sandbox::SandboxFailureHandler`] that
//! tolerates the bootstrap syscalls gVisor refuses by design
//! (`unshare(CLONE_NEWNET)`, `seccomp(SECCOMP_SET_MODE_FILTER)`).
//! Safe to install because gVisor itself is the enforcing boundary in
//! this deployment; the supervisor's in-process hardening is
//! defense-in-depth that adds nothing.

use openshell_sandbox::{SandboxFailureHandler, SandboxFailureKind};

#[derive(Debug, Default, Clone, Copy)]
pub struct DegradedHandler;

impl SandboxFailureHandler for DegradedHandler {
    fn handle(
        &self,
        kind: SandboxFailureKind,
        err: miette::Report,
    ) -> miette::Result<()> {
        let subsystem = match kind {
            SandboxFailureKind::NetworkNamespaceCreate => "network-namespace",
            SandboxFailureKind::SupervisorSeccompInstall => "supervisor-seccomp",
            SandboxFailureKind::WorkloadSeccompInstall => "workload-seccomp",
        };
        tracing::warn!(
            subsystem,
            error = %err,
            "Sandbox bootstrap subsystem unavailable; \
             continuing in outer-sandbox-managed degraded mode"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn degraded_handler_swallows_all_kinds() {
        let handler = DegradedHandler;
        for kind in [
            SandboxFailureKind::NetworkNamespaceCreate,
            SandboxFailureKind::SupervisorSeccompInstall,
            SandboxFailureKind::WorkloadSeccompInstall,
        ] {
            let err = miette::miette!("simulated kernel refusal");
            assert!(
                handler.handle(kind, err).is_ok(),
                "DegradedHandler must return Ok for {kind:?}"
            );
        }
    }
}
