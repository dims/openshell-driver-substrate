# SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

# Minimal sandbox policy: default-deny egress, with an exact host:port allow
# list read from `data.network_policies`. It answers every query the
# supervisor makes and nothing more.
#
# To allow a host, add it to data.yaml:
#
#   network_policies:
#     models:
#       endpoints:
#         - host: api.example.com
#           ports: [443]
#
# An endpoint with no `binaries` allows any binary; an endpoint with no
# `rules` applies no L7 restriction.

package openshell.sandbox

# --- Static policy data, queried once at sandbox startup ---

filesystem_policy := data.filesystem_policy

landlock_policy := data.landlock

process_policy := data.process

# --- L4: per-CONNECT decision ---

default allow_network = false

allow_network if {
	network_policy_for_request
}

default network_action := "deny"

network_action := "allow" if {
	network_policy_for_request
}

network_policy_for_request if {
	some name
	policy := data.network_policies[name]
	endpoint_allowed(policy, input.network)
	binary_allowed(policy, input.exec)
}

endpoint_allowed(policy, network) if {
	some endpoint
	endpoint := policy.endpoints[_]
	lower(endpoint.host) == lower(network.host)
	endpoint.ports[_] == network.port
}

# No `binaries` on the policy means any binary may use it.
binary_allowed(policy, _) if {
	not policy.binaries
}

binary_allowed(policy, exec) if {
	policy.binaries[_] == exec.path
}

# --- Matched policy name, for audit logging ---
#
# Collected into a set and reduced with min() so that two policies covering the
# same endpoint do not raise a complete-rule conflict.

_matching_policy_names contains name if {
	some name
	policy := data.network_policies[name]
	endpoint_allowed(policy, input.network)
	binary_allowed(policy, input.exec)
}

matched_network_policy := min(_matching_policy_names) if {
	count(_matching_policy_names) > 0
}

# --- Denial diagnostics ---

deny_reason := "missing input.network" if {
	not input.network
}

deny_reason := "missing input.exec" if {
	input.network
	not input.exec
}

deny_reason := "no network policies are defined" if {
	input.network
	input.exec
	not network_policy_for_request
	count(data.network_policies) == 0
}

deny_reason := reason if {
	input.network
	input.exec
	not network_policy_for_request
	count(data.network_policies) > 0
	reason := sprintf(
		"%s:%d not allowed for binary '%s'",
		[input.network.host, input.network.port, input.exec.path],
	)
}

# --- L7: per-request decision inside an allowed tunnel ---
#
# An endpoint that declares no `rules` imposes no L7 restriction, so an allowed
# CONNECT carries its requests. There is no deny-rule engine here.

default allow_request = false

default deny_request = false

allow_request if {
	some name
	policy := data.network_policies[name]
	endpoint_allowed(policy, input.network)
	binary_allowed(policy, input.exec)
	endpoint := policy.endpoints[_]
	not endpoint.rules
}

request_deny_reason := reason if {
	input.request
	not allow_request
	reason := sprintf("%s %s not permitted by policy", [input.request.method, input.request.path])
}
