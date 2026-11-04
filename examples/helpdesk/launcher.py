# SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""The first process of the supervisor container and of the sandbox container.

With LAUNCH_URL set, the supervisor role fetches the bundle the gateway minted
for this actor from the driver's launch endpoint (the actor's name and atespace
come from the systemInfo mount; atenet-egress puts the actor JWT on the
request), writes the supervisor's files, lays the sandbox's files out on the
shared durable dir and starts the supervisor; a newer generation replaces
both. The sandbox role watches that dir and runs openshell-sandbox on the
current generation. Without LAUNCH_URL both roles use the files the image and
its volumes carry, which is the demo without a gateway.
"""
import json
import os
import shutil
import signal
import subprocess
import sys
import time
import urllib.error
import urllib.request

ROLE = sys.argv[1] if len(sys.argv) > 1 else ""
LAUNCH_URL = os.environ.get("LAUNCH_URL", "").rstrip("/")
SHARED = "/run/openshell"                   # durable dir, both containers
CURRENT = f"{SHARED}/current"               # the generation laid out below
SANDBOX_BUNDLES = f"{SHARED}/bundle"        # <generation>/bootstrap.json, server.crt, server.key
SUPERVISOR_DIR = f"{SHARED}/supervisor"     # <generation>/runtime-descriptor.json, auth.json
STATIC_SUPERVISOR = "/opt/supervisor-files/supervisor"
SANDBOX_DIR = "/.openshell/channel/sandbox"
IDENTITY = "/run/ate"
LISTENER = " 0211FEA9:4571 "                # 169.254.17.2:17777, little-endian hex, in /proc/net/tcp
child = None
expected_exit = False


def log(msg):
    print(f"launcher[{ROLE}] {msg}", flush=True)


def write_private(path, data):
    os.makedirs(os.path.dirname(path), mode=0o700, exist_ok=True)
    tmp = f"{path}.tmp"
    with open(tmp, "w", opener=lambda p, f: os.open(p, f, 0o600)) as f:
        f.write(data)
    os.replace(tmp, path)


def terminate(proc, what):
    if proc is None or proc.poll() is not None:
        return
    log(f"stopping {what}")
    proc.terminate()
    try:
        proc.wait(10)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait()


def on_signal(signum, _frame):
    global expected_exit
    expected_exit = True
    terminate(child, "the child")
    sys.exit(128 + signum)


def listener_up():
    with open("/proc/net/tcp") as f:
        return any(LISTENER in line and " 0A " in line for line in f)


def wait_for_listener(seconds):
    for _ in range(seconds * 2):
        if listener_up():
            return True
        time.sleep(0.5)
    return False


def read(path):
    with open(path) as f:
        return f.read()


# --- supervisor role -----------------------------------------------------

def start_supervisor(args, env):
    global child
    full = dict(os.environ, OPENSHELL_ADMITTED_ISOLATION_BACKEND="openshell-sandbox", **env)
    child = subprocess.Popen(["/openshell-supervisor", *args], env=full)
    log(f"supervisor pid {child.pid}")


def apply_bundle(bundle, running):
    """Lay the generation out, replace the supervisor, then publish it to the sandbox."""
    gen = bundle["generation"]
    sup = f"{SUPERVISOR_DIR}/{gen}"
    for name, data in bundle["supervisor"].items():
        write_private(f"{sup}/{name}", data)
    for name, data in bundle["sandbox"].items():
        write_private(f"{SANDBOX_BUNDLES}/{gen}/{name}", data)
    env = {
        "OPENSHELL_ENDPOINT": bundle["gateway"]["endpoint"],
        "OPENSHELL_SANDBOX_ID": bundle["sandbox_id"],
        "OPENSHELL_SANDBOX": bundle["sandbox_name"],
        "OPENSHELL_LOG_LEVEL": bundle["log_level"],
        "OPENSHELL_NETWORK_RUNTIME_CAPABILITIES": "policy-dns-transparent-tcp",
    }
    if bundle.get("main_process_spec"):
        env["OPENSHELL_MAIN_PROCESS_SPEC"] = bundle["main_process_spec"]
    if bundle["gateway"].get("tls_ca_pem"):
        write_private(f"{sup}/gateway-ca.pem", bundle["gateway"]["tls_ca_pem"])
        env["OPENSHELL_TLS_CA"] = f"{sup}/gateway-ca.pem"
    if bundle["gateway"].get("tls_server_name"):
        env["OPENSHELL_GATEWAY_TLS_SERVER_NAME"] = bundle["gateway"]["tls_server_name"]
    terminate(child, f"the supervisor of generation {running}")
    start_supervisor(
        ["--backend-descriptor-file", f"{sup}/runtime-descriptor.json",
         "--auth-bundle-file", f"{sup}/auth.json"],
        env,
    )
    if not wait_for_listener(30):
        log("the new supervisor never listened on 169.254.17.2:17777")
        sys.exit(1)
    write_private(CURRENT, gen)
    log(f"generation {gen} (epoch {bundle['epoch']}, resume={bundle['resume']}) published")
    return gen


def fetch(name, atespace, generation):
    url = f"{LAUNCH_URL}/v1/launch/{atespace}/{name}"
    if generation:
        url += f"?generation={generation}"
    req = urllib.request.Request(url, headers={"Authorization": "Bearer x"})
    with urllib.request.urlopen(req, timeout=40) as r:
        if r.status == 204:
            return None
        return json.load(r)


def supervisor_role():
    if not LAUNCH_URL:
        start_supervisor(
            ["--role", "isolation-backend", "--sandbox-id", "test-sandbox-1",
             "--backend-descriptor-file", f"{STATIC_SUPERVISOR}/runtime-descriptor.json",
             "--auth-bundle-file", f"{STATIC_SUPERVISOR}/auth.json",
             "--policy-rules", f"{STATIC_SUPERVISOR}/policy.rego",
             "--policy-data", f"{STATIC_SUPERVISOR}/data.yaml",
             "--log-level", "info", "--", "python3", "/opt/helpdesk/agent.py"],
            {},
        )
        sys.exit(child.wait())
    name = read(f"{IDENTITY}/actor-name").strip()
    atespace = read(f"{IDENTITY}/atespace").strip()
    log(f"actor {atespace}/{name}, bundles from {LAUNCH_URL}")
    running = None
    while True:
        if child is not None and child.poll() is not None:
            log(f"the supervisor exited with {child.returncode}")
            sys.exit(1)
        try:
            bundle = fetch(name, atespace, running)
        except urllib.error.HTTPError as e:
            log(f"launch endpoint answered {e.code}; retrying")
            time.sleep(5)
            continue
        except (urllib.error.URLError, OSError, ValueError) as e:
            log(f"launch endpoint unreachable ({e}); retrying")
            time.sleep(2)
            continue
        if bundle and bundle["generation"] != running:
            running = apply_bundle(bundle, running)


# --- sandbox role ---------------------------------------------------------

def start_sandbox():
    global child
    if not wait_for_listener(120):
        log("no supervisor listener on 169.254.17.2:17777")
        sys.exit(1)
    # no_new_privs: the runtime does not set it; prctl lets a process set it for itself.
    child = subprocess.Popen(["setpriv", "--no-new-privs", "--", "/openshell-sandbox",
                              "--bootstrap", f"{SANDBOX_DIR}/bootstrap.json", "--log-level", "info"])
    log(f"openshell-sandbox pid {child.pid}")


def sandbox_role():
    # The workload's resolver is the sandbox's DNS relay. Substrate writes the
    # actor gateway into the base layer on every activation; the image's /etc
    # belongs to the sandbox user, and the edit rides in the snapshot.
    write_private("/etc/resolv.conf.sandbox", "nameserver 127.0.0.53\n")
    os.replace("/etc/resolv.conf.sandbox", "/etc/resolv.conf")
    if not LAUNCH_URL:
        start_sandbox()
        sys.exit(child.wait())
    running = None
    while True:
        if child is not None and child.poll() is not None:
            log(f"openshell-sandbox exited with {child.returncode}")
            sys.exit(1)
        try:
            gen = read(CURRENT).strip()
        except FileNotFoundError:
            time.sleep(1)
            continue
        if gen != running:
            terminate(child, f"the sandbox of generation {running}")
            src = f"{SANDBOX_BUNDLES}/{gen}"
            os.makedirs(SANDBOX_DIR, mode=0o700, exist_ok=True)
            for name in ("bootstrap.json", "server.crt", "server.key"):
                shutil.copyfile(f"{src}/{name}", f"{SANDBOX_DIR}/{name}")
                os.chmod(f"{SANDBOX_DIR}/{name}", 0o600)
            start_sandbox()
            running = gen
            log(f"generation {gen} running")
        time.sleep(1)


signal.signal(signal.SIGTERM, on_signal)
signal.signal(signal.SIGINT, on_signal)
if ROLE == "supervisor":
    supervisor_role()
elif ROLE == "sandbox":
    sandbox_role()
else:
    log("usage: launcher.py supervisor|sandbox")
    sys.exit(2)
