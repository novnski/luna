#!/usr/bin/env python3
"""Repeatable real-provider sidebar acceptance. Run with agent-python.

Only the reserved Sidebar QA owner on 59787 is accepted. This uses a disposable
projectless parent with Codex gpt-6-luna and one harmless side-chat question.
An existing evidence directory is read-only verification; it never replays a
create/send. UI screenshots and parent-draft insertion are separate native QA.
"""
import argparse
import asyncio
import hashlib
import json
from pathlib import Path
import time
import uuid

import websockets


class Rpc:
    def __init__(self, websocket):
        self.ws = websocket
        self.next_id = 0

    async def request(self, method, params=None, stream=False):
        self.next_id += 1
        request_id = self.next_id
        await self.ws.send(json.dumps({"id": request_id, "method": method, "params": params or {}}))
        deadline = time.monotonic() + 90
        while time.monotonic() < deadline:
            raw = await asyncio.wait_for(self.ws.recv(), deadline - time.monotonic())
            for line in raw.splitlines():
                frame = json.loads(line)
                if frame.get("id") != request_id:
                    continue
                if "err" in frame:
                    raise RuntimeError(f"{method}: {frame['err']}")
                if "ok" in frame:
                    return frame["ok"]
                if stream and "item" in frame:
                    await self.ws.send(json.dumps({"id": request_id, "cancel": True}))
                    return frame["item"]
        raise TimeoutError(method)

    async def transcript(self, task):
        result = await self.request("WatchDocMessages", {"chatId": task}, stream=True)
        frame = result.get("frame", result)
        if "reset" not in frame:
            raise RuntimeError("Expected a fresh transcript reset")
        return frame["reset"]


def save(path, value):
    path.write_text(json.dumps(value, indent=2, ensure_ascii=False) + "\n")


def text(entries, role):
    return "\n".join(part.get("text", "") for entry in entries if entry["role"] == role
                     for part in entry["parts"] if part.get("kind") == "text")


def digest(entries):
    return hashlib.sha256(json.dumps(entries, sort_keys=True).encode()).hexdigest()


async def main(args):
    worktree = Path(__file__).resolve().parent.parent
    data = (worktree / "target/luna-qa-data/sidebar").resolve()
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=True)
    async with websockets.connect("ws://127.0.0.1:59787", max_size=16 * 1024 * 1024) as websocket:
        rpc = Rpc(websocket)
        root = await rpc.request("LocalOwnerRootClaim")
        expected = hashlib.sha256(b"LUNA local root v1\0" + bytes(data)).hexdigest()
        assert root["claim"] == expected, "IPC listener is not the reserved Sidebar QA profile"
        info = await rpc.request("EngineInfo")
        if (out / "run.json").exists():
            run = json.loads((out / "run.json").read_text())
            assert run.get("side_chat_id"), "Prior run did not finish; inspect it instead of replaying"
            history = await rpc.request("ReadSideChat", {"id": run["side_chat_id"]})
            assert history["meta"]["state"] != "active"
            assert run["marker"] in text(history["entries"], "assistant")
            sessions = await rpc.request("WatchSessions", stream=True)
            assert not any(row["chatId"] == run["side_chat_id"] and row["status"] in ("working", "awaitingInput", "awaitingApproval") for row in sessions)
            save(out / "restart-history-readback.json", {"history": history, "provider_runtime_active": False})
            print("SIDEBAR_RESTART_HISTORY_PASS")
            return
        task = str(uuid.uuid4())
        marker = "LUNA_SIDEBAR_" + uuid.uuid4().hex[:10]
        run = {"parent_id": task, "marker": marker, "model": "gpt-6-luna", "created_at_ms": int(time.time() * 1000)}
        save(out / "run.json", run)  # No replay even if a later response is lost.
        config = {"harness": "codex", "model": "gpt-6-luna", "reasoning": "high",
                  "sandbox": "read-only", "permissionMode": "ask", "effectivePermissionMode": "ask"}
        await rpc.request("Mutate", {"op": "createChat", "chatId": task, "deviceId": info["deviceId"], "config": config, "cwd": str(out)})
        await rpc.request("Mutate", {"op": "renameChat", "chatId": task, "title": "Sidebar QA · context and history"})
        snapshot = (await rpc.request("GetTaskCommandSnapshot", {"taskId": task}))["snapshot"]
        prompt = f"Remember this exact harmless marker for this conversation: {marker}. Reply exactly PARENT_READY. Do not use tools or change files."
        command_id = str(uuid.uuid4())
        request = {"prompt": prompt, "harness": "codex", "model": "gpt-6-luna", "reasoning": "high",
                   "cwd": str(out), "sandbox": "read-only", "permissionMode": "ask", "resume": None}
        receipt = await rpc.request("ExecuteTaskCommand", {"commandId": command_id, "taskId": task,
            "expectedTaskRevision": snapshot["taskRevision"], "expectedBindingGeneration": snapshot.get("bindingGeneration"),
            "intent": {"kind": "session", "command": {"kind": "run", "request": request, "messageId": str(uuid.uuid4())}}})
        save(out / "parent-admission.json", receipt)
        before = None
        for _ in range(90):
            await asyncio.sleep(1)
            entries = await rpc.transcript(task)
            if text(entries, "assistant").strip() == "PARENT_READY" and all(entry.get("status") != "streaming" for entry in entries):
                before = entries
                break
        assert before is not None, "Parent did not complete"
        save(out / "parent-before.json", before)
        await asyncio.sleep(2)
        created = await rpc.request("CreateSideChat", {"parentId": task, "requestId": str(uuid.uuid4())})
        assert created["meta"]["contextMode"] == "nativeFork"
        side = created["meta"]["id"]
        lease = created["lease"]
        run.update(side_chat_id=side, parent_digest=digest(before))
        save(out / "run.json", run)
        save(out / "side-created.json", created["meta"])
        await rpc.request("SendSideChat", {"id": side, "lease": lease, "messageId": str(uuid.uuid4()),
            "text": "What exact LUNA_SIDEBAR_ marker did I ask you to remember in the parent conversation? Reply with only that marker. Do not use tools or change files."})
        complete = None
        for _ in range(90):
            await asyncio.sleep(1)
            page = await rpc.request("ReadSideChat", {"id": side, "lease": lease})
            save(out / "side-last-observation.json", page)
            if text(page["entries"], "assistant").strip() == marker and not page["working"]:
                complete = page
                break
        assert complete is not None, "Side chat did not demonstrate inherited native context"
        save(out / "side-complete.json", complete)
        await rpc.request("UnloadSideChat", {"id": side, "lease": lease})
        history = await rpc.request("ReadSideChat", {"id": side})
        assert history["meta"]["state"] == "unloaded"
        assert marker in text(history["entries"], "assistant")
        assert "PARENT_READY" not in text(history["entries"], "assistant"), "Parent transcript leaked into side history"
        after = await rpc.transcript(task)
        assert digest(before) == digest(after), "Side chat changed the parent transcript"
        sessions = await rpc.request("WatchSessions", stream=True)
        assert not any(row["chatId"] == side for row in sessions), "Unloaded side runtime remained registered"
        save(out / "history-readback.json", history)
        save(out / "parent-after.json", after)
        save(out / "result.json", {"passed": True, "model": "gpt-6-luna", "native_context_preserved": True,
            "parent_unchanged": True, "side_runtime_registered_after_history_read": False,
            "elapsed_ms": int(time.time() * 1000) - run["created_at_ms"]})
        print("SIDEBAR_NATIVE_CONTEXT_HISTORY_PASS")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("output", type=Path)
    asyncio.run(main(parser.parse_args()))
