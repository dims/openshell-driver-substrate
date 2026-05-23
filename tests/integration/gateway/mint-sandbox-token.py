#!/usr/bin/env python3
# Offline JWT minter for the OpenShell gateway-driven supervisor features
# (§7b POC). Produces a sandbox JWT signed with the Ed25519 key the gateway
# is configured to trust.
#
# Usage:
#   mint-sandbox-token.py --sandbox-id <id> \
#                         [--signing-key /tmp/openshell-jwt-signing.pem] \
#                         [--kid-file /tmp/openshell-jwt-kid] \
#                         [--gateway-id openshell-poc] \
#                         [--ttl-secs 3600]
#
# Prints the JWT to stdout (no trailing newline).
#
# Default claim layout mirrors what the gateway's `IssueSandboxToken` would
# produce (`iss`, `aud`, `sub`, `iat`, `exp`, plus `kid` in the header).
import argparse
import time
from pathlib import Path

import jwt


def main() -> None:
    p = argparse.ArgumentParser()
    p.add_argument("--sandbox-id", required=True, help="Sub claim (actor_id).")
    p.add_argument("--signing-key", default="/tmp/openshell-jwt-signing.pem",
                   help="Ed25519 PKCS#8 PEM private key.")
    p.add_argument("--kid-file", default="/tmp/openshell-jwt-kid",
                   help="File whose contents become the JWT 'kid' header.")
    p.add_argument("--gateway-id", default="openshell-poc",
                   help="iss + aud claims; must match gateway.toml gateway_id.")
    p.add_argument("--ttl-secs", type=int, default=3600)
    args = p.parse_args()

    signing_key = Path(args.signing_key).read_text()
    kid = Path(args.kid_file).read_text().strip()
    now = int(time.time())

    token = jwt.encode(
        {
            "iss": args.gateway_id,
            "aud": args.gateway_id,
            "sub": args.sandbox_id,
            "iat": now,
            "exp": now + args.ttl_secs,
        },
        signing_key,
        algorithm="EdDSA",
        headers={"kid": kid},
    )
    print(token, end="")


if __name__ == "__main__":
    main()
