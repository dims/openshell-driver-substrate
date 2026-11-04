#!/usr/bin/env python3
"""Helpdesk agent: chat triage with RAM-only history, plus an egress probe.

History lives in a Python list so a suspend and resume proves the actor kept
its memory. With OPENAI_BASE_URL set the replies come from that
OpenAI-compatible endpoint; without it the agent answers on its own, so the
lifecycle checks need no model. With an argument it is a client for the
server on loopback, for `openshell sandbox exec`.
"""
import http.server
import json
import os
import sys
import threading
import time
import urllib.error
import urllib.request

PORT = int(os.environ.get("HELPDESK_PORT", "8080"))
BASE = os.environ.get("OPENAI_BASE_URL", "")
MODEL = os.environ.get("HELPDESK_MODEL", "")

SYSTEM = ("You are a triage assistant for a hosted-service helpdesk. Answer "
          "technical operations questions briefly and actionably.")
history = [{"role": "system", "content": SYSTEM}]
chat_lock = threading.Lock()  # one chat at a time keeps the history in order
booted = time.time()


def fetch(url, data=None, timeout=30):
    req = urllib.request.Request(url, method="POST" if data else "GET")
    body = None
    if data is not None:
        req.add_header("Content-Type", "application/json")
        body = json.dumps(data).encode()
    with urllib.request.urlopen(req, body, timeout=timeout) as r:
        return r.status, r.read().decode("utf-8", "replace")


def reply_to(ask):
    """The model's answer, or the agent's own when no endpoint is configured."""
    if not BASE:
        return f"turn {(len(history) + 1) // 2}: you said {ask!r}"
    _, raw = fetch(f"{BASE.rstrip('/')}/chat/completions",
                   {"model": MODEL, "messages": history}, timeout=80)
    return json.loads(raw)["choices"][0]["message"]["content"]


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
        with chat_lock:
            history.append({"role": "user", "content": ask})
            try:
                reply = reply_to(ask)
            except Exception as e:  # noqa: BLE001 - report the failure to the caller
                history.pop()
                self._send(502, {"error": f"{type(e).__name__}: {e}"})
                return
            history.append({"role": "assistant", "content": reply})
        self._send(200, {"reply": reply, "turns": (len(history) - 1) // 2})

    def log_message(self, fmt, *args):
        print("agent " + fmt % args, flush=True)


def client(args):
    """status | chat <message> | egress <url>: ask the server on loopback."""
    base = f"http://127.0.0.1:{PORT}"
    if args[0] == "status":
        url, data, timeout = f"{base}/status", None, 30
    elif args[0] == "chat":
        url, data, timeout = f"{base}/chat", {"message": " ".join(args[1:])}, 90
    elif args[0] == "egress":
        url, data, timeout = f"{base}/egress?url={args[1]}", None, 30
    else:
        sys.exit("usage: agent.py [status | chat <message> | egress <url>]")
    for attempt in range(10):  # right after Ready the server may still be binding
        try:
            status, body = fetch(url, data, timeout=timeout)
            break
        except urllib.error.HTTPError as e:
            status, body = e.code, e.read().decode("utf-8", "replace")
            break
        except ConnectionRefusedError:
            if attempt == 9:
                raise
            time.sleep(0.5)
    print(body)
    sys.exit(0 if status == 200 else 1)


if __name__ == "__main__":
    if len(sys.argv) > 1:
        client(sys.argv[1:])
    print(f"helpdesk agent on :{PORT} base={BASE!r}", flush=True)
    http.server.ThreadingHTTPServer(("0.0.0.0", PORT), Handler).serve_forever()
