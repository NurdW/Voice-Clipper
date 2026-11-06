#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Throwaway verification probe for the OBS websocket replay-buffer trigger.

The real client lives in Rust (clipctl). This exists only to prove the OBS side
works before the Rust build depends on it: connect, authenticate, report replay
buffer status, save a replay, and confirm a file actually lands on disk.

Wire format verified against obs-websocket 5.7.3 source
(src/websocketserver/types/WebSocketOpCode.h, WebSocketServer_Protocol.cpp):

  op codes: Hello=0 Identify=1 Identified=2 Reidentify=3 Event=5
            Request=6 RequestResponse=7 RequestBatch=8 RequestBatchResponse=9
  The SERVER sends Hello first; a client-sent Hello is rejected as non-Identify.
  Envelope is FLAT in rpcVersion 1: fields live directly in `d`
  (d.requestType / d.requestData / d.requestStatus / d.responseData),
  NOT the older d.data.opType nesting some docs describe.
  requestStatus.result is a boolean, code 1200 == Success.

Usage: obs_probe.py [--save] [--wait SECONDS]
"""

import asyncio
import base64
import hashlib
import json
import os
import pathlib
import sys
import time

import websockets

OP_HELLO, OP_IDENTIFY, OP_IDENTIFIED, OP_EVENT, OP_REQUEST, OP_REQ_RESP = 0, 1, 2, 5, 6, 7
RPC_VERSION = 1


def obs_config_dir() -> pathlib.Path:
    return pathlib.Path(os.environ.get("OBS_CONFIG_DIR", pathlib.Path.home() / ".config/obs-studio"))


def load_ws_settings() -> dict:
    # Password stays in OBS's own config file; never copied into source code.
    cfg = obs_config_dir() / "plugin_config" / "obs-websocket" / "config.json"
    return json.loads(cfg.read_text())


def profile_value(key: str) -> str | None:
    """Read a key from the active profile's basic.ini (last section wins)."""
    basic = obs_config_dir() / "basic" / "basic.ini"
    if not basic.exists():
        profiles = sorted((obs_config_dir() / "basic" / "profiles").glob("*/basic.ini"))
        if not profiles:
            return None
        basic = profiles[0]
    value = None
    for line in basic.read_text().splitlines():
        if line.startswith(f"{key}="):
            value = line.split("=", 1)[1]
    return value


def auth_secret(password: str, salt: str, challenge: str) -> str:
    # Verified against obs-websocket 5.7.3 src/utils/Crypto.cpp:
    #   secret = b64(sha256(password + salt))      <- salt, not challenge
    #   auth   = b64(sha256(secret + challenge))
    seed = hashlib.sha256((password + salt).encode()).digest()
    secret = base64.b64encode(seed).decode()
    return base64.b64encode(hashlib.sha256((secret + challenge).encode()).digest()).decode()


class Client:
    def __init__(self, ws):
        self.ws = ws
        self.pending: dict[str, asyncio.Future] = {}
        self.n = 0

    async def call(self, request_type: str, data: dict | None = None, timeout: float = 10):
        self.n += 1
        rid = f"req-{self.n}"
        d = {"requestId": rid, "requestType": request_type, "rpcVersion": RPC_VERSION}
        if data is not None:
            d["requestData"] = data
        fut = asyncio.get_running_loop().create_future()
        self.pending[rid] = fut
        await self.ws.send(json.dumps({"op": OP_REQUEST, "d": d}))
        return await asyncio.wait_for(fut, timeout=timeout)

    async def pump(self):
        async for raw in self.ws:
            m = json.loads(raw)
            if m.get("op") not in (OP_REQ_RESP, OP_IDENTIFIED):
                continue
            d = m["d"]
            if m["op"] == OP_IDENTIFIED:
                # The Identified frame carries no requestId, only negotiatedRpcVersion.
                fut = self.pending.pop("identify", None)
                if fut and not fut.done():
                    fut.set_result(d)
                continue
            rid = d.get("requestId")
            if rid in self.pending:
                fut = self.pending.pop(rid)
                if not fut.done():
                    fut.set_result(d)


async def main() -> int:
    want_save = "--save" in sys.argv
    wait_s = float(sys.argv[sys.argv.index("--wait") + 1]) if "--wait" in sys.argv else 30.0

    ws_cfg = load_ws_settings()
    port = int(ws_cfg.get("server_port", 4455))

    deadline = time.time() + wait_s
    while True:
        try:
            ws = await websockets.connect(f"ws://127.0.0.1:{port}")
            break
        except OSError:
            if time.time() > deadline:
                print(f"FAIL: nothing listening on 127.0.0.1:{port} after {wait_s}s")
                return 1
            await asyncio.sleep(0.5)

    c = Client(ws)
    hello = json.loads(await asyncio.wait_for(ws.recv(), timeout=10))
    assert hello["op"] == OP_HELLO, f"expected server Hello (op 0), got op {hello['op']}"
    auth = hello["d"].get("authentication") or {}
    print("server hello:", json.dumps(hello["d"].get("versions", {})))

    pump_task = asyncio.ensure_future(c.pump())
    loop = asyncio.get_running_loop()
    c.pending["identify"] = loop.create_future()
    ident_d = {"rpcVersion": RPC_VERSION}
    # This build's Hello carries {challenge, salt} and no `requiresAuth` flag,
    # so the presence of a challenge is what means "you must authenticate".
    if auth.get("challenge") and auth.get("salt"):
        ident_d["authentication"] = auth_secret(ws_cfg["server_password"], auth["salt"], auth["challenge"])
    await ws.send(json.dumps({"op": OP_IDENTIFY, "d": ident_d}))
    ident = await asyncio.wait_for(c.pending["identify"], timeout=10)
    print("identified:", json.dumps(ident))

    for rt in ("GetVersion", "GetReplayBufferStatus", "GetRecordStatus"):
        try:
            r = await c.call(rt)
            st = r.get("requestStatus", {})
            print(f"{rt}: ok={st.get('result')} code={st.get('code')} data={json.dumps(r.get('responseData', {}))}")
        except Exception as exc:  # noqa: BLE001 - probe reports rather than hides
            print(f"{rt}: unavailable -> {type(exc).__name__}: {exc}")

    out_dir = profile_value("FilePath") or profile_value("RecFilePath") or str(pathlib.Path.home())
    print("clip output dir from profile:", out_dir)

    if want_save:
        before = set(pathlib.Path(out_dir).glob("Clip*"))
        r = await c.call("SaveReplayBuffer")
        print("SaveReplayBuffer:", json.dumps(r.get("requestStatus", {})))
        new: set[pathlib.Path] = set()
        for _ in range(60):
            await asyncio.sleep(0.5)
            new = set(pathlib.Path(out_dir).glob("Clip*")) - before
            if new:
                break
        if new:
            for p in sorted(new):
                print(f"NEW FILE: {p} ({p.stat().st_size:,} bytes)")
        else:
            print("FAIL: no new clip file appeared in", out_dir)
            pump_task.cancel()
            return 1

    pump_task.cancel()
    await ws.close()
    return 0


if __name__ == "__main__":
    raise SystemExit(asyncio.run(main()))
