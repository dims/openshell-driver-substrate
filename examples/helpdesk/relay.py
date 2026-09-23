# SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Loopback relay: accepts on the actor's address and connects to the agent
over loopback. It runs as a plain container next to the sandbox, so it is not
mediated; it stands in for the gateway's loopback service endpoint, which needs
the gateway path. The agent only ever sees a loopback peer, which its network
broker allows."""
import asyncio
import os

LISTEN = int(os.environ.get("RELAY_PORT", "8081"))
TARGET = int(os.environ.get("HELPDESK_PORT", "8080"))


async def pipe(reader, writer):
    try:
        while data := await reader.read(65536):
            writer.write(data)
            await writer.drain()
    except (ConnectionError, asyncio.IncompleteReadError):
        pass
    finally:
        writer.close()


async def handle(client_r, client_w):
    try:
        agent_r, agent_w = await asyncio.open_connection("127.0.0.1", TARGET)
    except OSError:
        client_w.close()
        return
    await asyncio.gather(pipe(client_r, agent_w), pipe(agent_r, client_w))


async def main():
    server = await asyncio.start_server(handle, "0.0.0.0", LISTEN)
    print(f"relay :{LISTEN} -> 127.0.0.1:{TARGET}", flush=True)
    async with server:
        await server.serve_forever()


asyncio.run(main())
