#!/usr/bin/env python3
"""GPU passthrough actor: holds a CUDA buffer that survives suspend/resume.

Endpoints:
  GET  /info       -> dev_ptr, size_bytes, driver_version, uptime_seconds
  GET  /sum        -> sum + sample byte of a 4 KiB device-memory probe
  POST /set?val=N  -> cuMemsetD8_v2 the buffer to byte N
"""
import ctypes
import http.server
import json
import os
import socketserver
import sys
import time

SIZE = 1 << 20      # 1 MiB
PROBE = 1 << 12     # 4 KiB

libcuda = ctypes.CDLL("libcuda.so.1")


def call(name, *args):
    rc = getattr(libcuda, name)(*args)
    if rc != 0:
        raise RuntimeError(f"{name}: CUresult={rc}")


call("cuInit", 0)
_ctx = ctypes.c_void_p()
call("cuCtxCreate_v2", ctypes.byref(_ctx), 0, 0)
_dptr = ctypes.c_void_p()
call("cuMemAlloc_v2", ctypes.byref(_dptr), SIZE)
call("cuMemsetD8_v2", _dptr, 0x42, SIZE)
_drv = ctypes.c_int(0)
libcuda.cuDriverGetVersion(ctypes.byref(_drv))

boot = time.time()
last_set = 0x42


class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/info":
            self._j(200, {
                "dev_ptr": hex(_dptr.value or 0),
                "size_bytes": SIZE,
                "driver_version": _drv.value,
                "uptime_seconds": round(time.time() - boot, 1),
            })
        elif self.path == "/sum":
            buf = (ctypes.c_uint8 * PROBE)()
            call("cuMemcpyDtoH_v2", buf, _dptr, PROBE)
            self._j(200, {"sum": sum(buf), "sample": buf[0], "probe_bytes": PROBE})
        else:
            self.send_error(404)

    def do_POST(self):
        global last_set
        if not self.path.startswith("/set"):
            self.send_error(404)
            return
        val = 0
        if "?" in self.path:
            for kv in self.path.split("?", 1)[1].split("&"):
                if kv.startswith("val="):
                    try:
                        val = int(kv.split("=", 1)[1]) & 0xFF
                    except ValueError:
                        self.send_error(400, "val must be 0-255")
                        return
        call("cuMemsetD8_v2", _dptr, val, SIZE)
        last_set = val
        self._j(200, {"ok": True, "val": val})

    def _j(self, status, body):
        b = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(b)))
        self.end_headers()
        self.wfile.write(b)

    def log_message(self, fmt, *args):
        sys.stderr.write(f"[gpu-counter] {fmt % args}\n")


port = int(os.environ.get("PORT", "80"))
sys.stderr.write(f"[gpu-counter] dev_ptr={hex(_dptr.value or 0)} driver={_drv.value} :{port}\n")
with socketserver.TCPServer(("0.0.0.0", port), H) as srv:
    srv.serve_forever()
