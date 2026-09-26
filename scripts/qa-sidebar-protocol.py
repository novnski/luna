#!/usr/bin/env python3
"""Loopback transport acceptance for qa-sidebar.py; no LUNA/provider access.

Authored before persistent-watch implementation. Failure cases: discarded
interleaved frames, duplicate watches, incorrect UTF-8/count/anchor folding,
mutated prior snapshots, silent desync, missing cancellation, hanging disconnect.
Run: agent-python scripts/qa-sidebar-protocol.py <result.json>
"""
import asyncio
import importlib.util
import json
from pathlib import Path
import sys

import websockets

spec = importlib.util.spec_from_file_location("sidebar_driver", Path(__file__).with_name("qa-sidebar.py"))
driver = importlib.util.module_from_spec(spec)
spec.loader.exec_module(driver)


async def main(output):
    observed = {"watches": 0, "cancels": 0}
    watch = None

    async def server(ws):
        nonlocal watch
        async for raw in ws:
            req = json.loads(raw)
            rid = req["id"]
            if req.get("cancel"):
                observed["cancels"] += 1
                continue
            method = req["method"]
            if method == "WatchDocMessages":
                observed["watches"] += 1
                watch = rid
                await ws.send(json.dumps({"id": rid, "item": {"reset": [
                    {"id": "u", "role": "user", "parts": [{"kind": "text", "id": "t", "text": "seed"}]}
                ]}}))
            elif method == "Interleave":
                frames = [
                    {"id": watch, "item": {"append": [{"entry": "u", "part": "t", "text": "Ω", "len": 6}], "count": 1}},
                    {"id": watch, "item": {"upsert": [{"after": "u", "entry": {"id": "a", "role": "assistant", "parts": [{"kind": "text", "id": "ta", "text": "done"}]}}], "count": 2}},
                    {"id": rid, "ok": "ACK"},
                ]
                await ws.send("\n".join(json.dumps(frame) for frame in frames))
            elif method == "Remove":
                await ws.send(json.dumps({"id": watch, "item": {"remove": ["u"], "count": 1}}))
                await ws.send(json.dumps({"id": rid, "ok": True}))
            elif method == "Desync":
                await ws.send(json.dumps({"id": watch, "item": {"count": 99}}))
                await ws.send(json.dumps({"id": rid, "ok": True}))
            elif method == "Stats":
                await ws.send(json.dumps({"id": rid, "ok": observed}))
            elif method == "Disconnect":
                await ws.close()

    async with websockets.serve(server, "127.0.0.1", 0) as listener:
        port = listener.sockets[0].getsockname()[1]
        async with websockets.connect(f"ws://127.0.0.1:{port}") as ws:
            async with driver.Rpc(ws) as rpc:
                before = await rpc.transcript("fixture")
                assert await rpc.request("Interleave") == "ACK"
                current = await rpc.transcript("fixture")
                assert current[0]["parts"][0]["text"] == "seedΩ"
                assert current[1]["parts"][0]["text"] == "done"
                assert before[0]["parts"][0]["text"] == "seed"
                for _ in range(20):
                    assert await rpc.transcript("fixture") == current
                await rpc.request("Remove")
                assert [entry["id"] for entry in await rpc.transcript("fixture")] == ["a"]
                assert (await rpc.request("Stats"))["watches"] == 1
                await rpc.request("Desync")
                try:
                    await rpc.transcript("fixture")
                    raise AssertionError("desync was silently accepted")
                except RuntimeError as error:
                    assert "desync" in str(error).lower()
                await rpc.unwatch("fixture")
                assert await rpc.request("Stats") == {"watches": 1, "cancels": 1}
                try:
                    await asyncio.wait_for(rpc.request("Disconnect"), 2)
                    raise AssertionError("disconnect accepted as a successful reply")
                except RuntimeError as error:
                    assert "connection" in str(error).lower()
    result = {"passed": True, "real_provider_calls": 0, "checks": [
        "interleaved stream and reply", "single persistent subscription", "reset/upsert/remove/UTF-8 append",
        "immutable returned snapshot", "desync refused without resubscription", "exact cancellation", "disconnect fails promptly"
    ], **observed}
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(result, indent=2) + "\n")
    print("SIDEBAR_PERSISTENT_PROTOCOL_PASS")


if __name__ == "__main__":
    asyncio.run(main(Path(sys.argv[1])))
