# SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Loopback relay: accepts on the actor's address and connects to the workload
over loopback. It runs as a plain container next to the sandbox, so it is not
mediated; it stands in for the gateway's loopback service endpoint. The
workload only ever sees a loopback peer, which its network broker allows.
RELAY_PORT is where it listens (8081), WORKLOAD_PORT where it connects (8080);
the wakeup probe gets a 200 on 17903."""
import asyncio
import os

LISTEN = int(os.environ.get("RELAY_PORT", "8081"))
TARGET = int(os.environ.get("WORKLOAD_PORT", "8080"))
HEALTH = 17903


async def pipe(reader, writer):
    """Copy until EOF, then half-close so the other direction can finish."""
    try:
        while data := await reader.read(65536):
            writer.write(data)
            await writer.drain()
        if writer.can_write_eof():
            writer.write_eof()
    except OSError:
        pass


async def handle(client_r, client_w):
    try:
        agent_r, agent_w = await asyncio.open_connection("127.0.0.1", TARGET)
    except OSError:
        client_w.close()
        return
    try:
        await asyncio.gather(pipe(client_r, agent_w), pipe(agent_r, client_w))
    finally:
        agent_w.close()
        client_w.close()


async def health(reader, writer):
    await reader.readuntil(b"\r\n\r\n")
    writer.write(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
    await writer.drain()
    writer.close()


async def main():
    server = await asyncio.start_server(handle, "0.0.0.0", LISTEN)
    probe = await asyncio.start_server(health, "0.0.0.0", HEALTH)
    print(f"relay :{LISTEN} -> 127.0.0.1:{TARGET}", flush=True)
    async with server, probe:
        await server.serve_forever()


asyncio.run(main())
