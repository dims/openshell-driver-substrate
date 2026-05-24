#!/usr/bin/env python3
"""Helpdesk agent: chat-style triage assistant with persistent RAM-only memory.

Runs inside an OpenShell sandbox. Calls Ollama Cloud via inference.local; keeps
chat history in a Python list that survives gVisor checkpoint/restore.
"""
import http.server
import json
import os
import socketserver
import sys
import time
from urllib.request import Request, urlopen
from urllib.error import URLError, HTTPError

# HTTPS is required: INFERENCE_LOCAL_PORT = 443 is hardcoded in OpenShell's
# proxy.rs. http://inference.local would route through handle_forward_proxy
# and be denied by OPA.
OLLAMA_BASE = os.environ.get("OPENSHELL_INFERENCE_BASE", "https://inference.local/v1")
MODEL = os.environ.get("HELPDESK_MODEL", "gpt-oss:20b-cloud")
PROBE_URL = os.environ.get("HELPDESK_PROBE_URL", "http://evil.example.com/")

SYSTEM_PROMPT = """You are a triage assistant for a hosted-service helpdesk.
You answer technical operations questions about cloud infrastructure
(databases, networking, kubernetes, observability). Keep replies brief and
actionable. Maintain conversation context across turns."""

chat_history: list[dict] = [{"role": "system", "content": SYSTEM_PROMPT}]
boot_time = time.time()


class HelpdeskHandler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/status":
            self._json(200, {
                "history_turns": (len(chat_history) - 1) // 2,
                "uptime_seconds": round(time.time() - boot_time, 1),
                "model": MODEL,
            })
        else:
            self.send_error(404)

    def do_POST(self):
        if self.path == "/chat":
            self._handle_chat()
        elif self.path == "/probe":
            self._handle_probe()
        else:
            self.send_error(404)

    def _handle_chat(self):
        length = int(self.headers.get("Content-Length", "0"))
        try:
            payload = json.loads(self.rfile.read(length).decode("utf-8"))
            user_msg = payload["message"]
            assert isinstance(user_msg, str)
        except (json.JSONDecodeError, KeyError, AssertionError):
            self.send_error(400, 'expected {"message": "..."} where message is a string')
            return

        chat_history.append({"role": "user", "content": user_msg})

        req = Request(
            f"{OLLAMA_BASE}/chat/completions",
            data=json.dumps({
                "model": MODEL, "messages": chat_history, "stream": False,
            }).encode("utf-8"),
            headers={"Content-Type": "application/json"},
        )
        try:
            with urlopen(req, timeout=60) as resp:
                data = json.loads(resp.read().decode("utf-8"))
            assistant = data["choices"][0]["message"]["content"]
        except Exception as e:
            self._json(502, {"error": f"{type(e).__name__}: {e}"})
            return

        chat_history.append({"role": "assistant", "content": assistant})
        self._json(200, {
            "reply": assistant,
            "history_turns": (len(chat_history) - 1) // 2,
        })

    def _handle_probe(self):
        try:
            with urlopen(PROBE_URL, timeout=10) as resp:
                self._json(500, {
                    "unexpected_success": True,
                    "status": resp.status,
                    "url": PROBE_URL,
                })
        except HTTPError as e:
            self._json(200, {
                "blocked": True, "url": PROBE_URL,
                "http_status": e.code, "reason": str(e),
                "explanation": "OpenShell HTTP CONNECT proxy denied per OPA policy",
            })
        except URLError as e:
            self._json(200, {
                "blocked": True, "url": PROBE_URL, "reason": str(e),
                "explanation": "OpenShell HTTP CONNECT proxy denied per OPA policy",
            })

    def _json(self, status, body):
        encoded = json.dumps(body).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(encoded)))
        self.end_headers()
        self.wfile.write(encoded)

    def log_message(self, fmt, *args):
        sys.stderr.write(f"[helpdesk] {fmt % args}\n")


def main():
    port = int(os.environ.get("PORT", "80"))
    # Sanity-dump proxy-related env so we can confirm the supervisor injected
    # HTTPS_PROXY before the first inference call lands.
    proxy_env = {k: v for k, v in os.environ.items()
                 if "PROXY" in k.upper() or k.upper() in ("NO_PROXY", "SSL_CERT_FILE",
                 "REQUESTS_CA_BUNDLE", "CURL_CA_BUNDLE")}
    sys.stderr.write(f"[helpdesk] proxy_env={proxy_env}\n")
    # If the supervisor didn't inject HTTPS_PROXY, fall back to 127.0.0.1:3128
    # explicitly. The supervisor's HTTP CONNECT proxy always binds that port.
    if "HTTPS_PROXY" not in os.environ and "https_proxy" not in os.environ:
        sys.stderr.write("[helpdesk] HTTPS_PROXY unset; falling back to http://127.0.0.1:3128\n")
        os.environ["HTTPS_PROXY"] = "http://127.0.0.1:3128"
        os.environ["HTTP_PROXY"] = "http://127.0.0.1:3128"
    sys.stderr.write(f"[helpdesk] listening on :{port}, model={MODEL}\n")
    with socketserver.TCPServer(("0.0.0.0", port), HelpdeskHandler) as srv:
        srv.serve_forever()


if __name__ == "__main__":
    main()
