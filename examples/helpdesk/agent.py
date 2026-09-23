#!/usr/bin/env python3
"""Helpdesk agent: chat triage with RAM-only history, plus an egress probe.

History lives in a Python list so a suspend/resume proves the actor kept its
memory. The model endpoint comes from the environment the supervisor injects;
the agent never holds a credential, the supervisor's proxy attaches it.
"""
import http.server
import json
import os
import time
import urllib.error
import urllib.request

PORT = int(os.environ.get("HELPDESK_PORT", "8080"))
# Injected by the supervisor via provider credentials. No key ever lands here.
BASE = os.environ.get("OPENAI_BASE_URL") or os.environ.get("OPENSHELL_INFERENCE_BASE", "")
MODEL = os.environ.get("HELPDESK_MODEL", "gpt-oss:20b-cloud")

SYSTEM = ("You are a triage assistant for a hosted-service helpdesk. Answer "
          "technical operations questions briefly and actionably.")
history = [{"role": "system", "content": SYSTEM}]
booted = time.time()


def fetch(url, data=None, timeout=30):
    req = urllib.request.Request(url, method="POST" if data else "GET")
    body = None
    if data is not None:
        req.add_header("Content-Type", "application/json")
        body = json.dumps(data).encode()
    with urllib.request.urlopen(req, body, timeout=timeout) as r:
        return r.status, r.read().decode("utf-8", "replace")


class Handler(http.server.BaseHTTPRequestHandler):
    def _send(self, code, payload):
        raw = json.dumps(payload).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(raw)))
        self.end_headers()
        self.wfile.write(raw)

    def do_GET(self):
        if self.path.startswith("/status"):
            self._send(200, {
                "turns": (len(history) - 1) // 2,
                "uptime_seconds": round(time.time() - booted, 1),
                "model": MODEL,
                "inference_base": BASE,
            })
        elif self.path.startswith("/egress"):
            # Proves whether anything reaches the outside, and how it fails.
            url = "https://example.com/"
            if "?url=" in self.path:
                url = self.path.split("?url=", 1)[1]
            try:
                status, body = fetch(url, timeout=15)
                self._send(200, {"url": url, "reached": True,
                                 "http_status": status, "bytes": len(body)})
            except urllib.error.HTTPError as e:
                self._send(200, {"url": url, "reached": True, "http_status": e.code})
            except Exception as e:  # noqa: BLE001 - report whatever stopped it
                self._send(200, {"url": url, "reached": False,
                                 "error": f"{type(e).__name__}: {e}"})
        else:
            self._send(404, {"error": "not found"})

    def do_POST(self):
        if not self.path.startswith("/chat"):
            self._send(404, {"error": "not found"})
            return
        length = int(self.headers.get("Content-Length", "0"))
        try:
            ask = json.loads(self.rfile.read(length) or b"{}").get("message", "")
        except json.JSONDecodeError:
            self._send(400, {"error": "body must be JSON"})
            return
        if not BASE:
            self._send(503, {"error": "no inference endpoint injected"})
            return
        history.append({"role": "user", "content": ask})
        try:
            _, raw = fetch(f"{BASE.rstrip('/')}/chat/completions",
                           {"model": MODEL, "messages": history})
            reply = json.loads(raw)["choices"][0]["message"]["content"]
        except Exception as e:  # noqa: BLE001 - surface the failure to the caller
            history.pop()
            self._send(502, {"error": f"{type(e).__name__}: {e}"})
            return
        history.append({"role": "assistant", "content": reply})
        self._send(200, {"reply": reply, "turns": (len(history) - 1) // 2})

    def log_message(self, fmt, *args):
        print("agent " + fmt % args, flush=True)


if __name__ == "__main__":
    print(f"helpdesk agent on :{PORT} base={BASE!r}", flush=True)
    http.server.ThreadingHTTPServer(("0.0.0.0", PORT), Handler).serve_forever()
