#!/usr/bin/env python3
"""Repeatable real-provider sidebar acceptance. Run with agent-python.

Only the reserved Sidebar QA owner on 59787 is accepted. This uses a disposable
projectless parent with Codex gpt-6-luna and one harmless side-chat question.
An existing evidence directory is read-only verification; it never replays a
create/send. UI screenshots and parent-draft insertion are separate native QA.
New runs share one stable provider cwd; evidence output directories must not
relocate the device's shared projectless workspace between acceptance drivers.
"""
import argparse
import asyncio
import copy
import hashlib
import json
from pathlib import Path
import time
import uuid

import websockets


class Transcript:
    def __init__(self):
        self.entries = []
        self.ready = asyncio.Event()
        self.error = None
        self.frames = 0

    def fail(self, error):
        self.error = error
        self.ready.set()

    def apply(self, update):
        if self.error:
            return
        frame = update.get("frame", update)
        try:
            if "reset" in frame:
                self.entries = copy.deepcopy(frame["reset"])
                self.ready.set()
            else:
                if not self.ready.is_set():
                    raise ValueError("delta arrived before initial reset")
                removed = set(frame.get("remove", []))
                self.entries = [entry for entry in self.entries if entry["id"] not in removed]
                for change in frame.get("upsert", []):
                    entry = copy.deepcopy(change["entry"])
                    self.entries = [row for row in self.entries if row["id"] != entry["id"]]
                    anchor = change.get("after")
                    at = 0 if anchor is None else next(i + 1 for i, row in enumerate(self.entries) if row["id"] == anchor)
                    self.entries.insert(at, entry)
                for change in frame.get("append", []):
                    entry = next(row for row in self.entries if row["id"] == change["entry"])
                    part = next(part for part in entry["parts"] if part["id"] == change["part"] and part["kind"] in ("text", "reasoning"))
                    part["text"] += change["text"]
                    if len(part["text"].encode("utf-8")) != change["len"]:
                        raise ValueError("text append byte length mismatch")
                if len(self.entries) != frame["count"]:
                    raise ValueError("transcript count mismatch")
            self.frames += 1
        except (KeyError, ValueError, TypeError, StopIteration) as error:
            self.fail(RuntimeError(f"Transcript desync; preserve evidence and stop: {error}"))


class Rpc:
    def __init__(self, websocket):
        self.ws = websocket
        self.next_id = 0
        self.pending = {}
        self.watches = {}
        self.streams = {}
        self.transcript_subscriptions = {}
        self.reader = asyncio.create_task(self._read())

    async def __aenter__(self):
        return self

    async def __aexit__(self, *_):
        await self.close()

    async def _read(self):
        failure = RuntimeError("RPC connection ended")
        try:
            async for raw in self.ws:
                for line in raw.splitlines():
                    frame = json.loads(line)
                    rid = frame.get("id")
                    state = self.streams.get(rid)
                    future = self.pending.get(rid)
                    if "err" in frame:
                        error = RuntimeError(str(frame["err"]))
                        if state:
                            state.fail(error)
                        if future and not future.done():
                            future.set_exception(error)
                    elif "item" in frame and state:
                        state.apply(frame["item"])
                    elif future and not future.done() and ("ok" in frame or "item" in frame):
                        future.set_result(frame.get("ok", frame.get("item")))
                    elif frame.get("done") and state:
                        state.fail(RuntimeError("Transcript connection closed"))
        except asyncio.CancelledError:
            raise
        except Exception as error:
            failure = RuntimeError(f"RPC connection failed: {error}")
        finally:
            for future in self.pending.values():
                if not future.done():
                    future.set_exception(failure)
            for state in self.streams.values():
                state.fail(failure)

    async def request(self, method, params=None, stream=False):
        self.next_id += 1
        request_id = self.next_id
        future = asyncio.get_running_loop().create_future()
        self.pending[request_id] = future
        try:
            await self.ws.send(json.dumps({"id": request_id, "method": method, "params": params or {}}))
            return await asyncio.wait_for(future, 90)
        finally:
            self.pending.pop(request_id, None)
            if stream or future.cancelled():
                try:
                    await self.ws.send(json.dumps({"id": request_id, "cancel": True}))
                except websockets.ConnectionClosed:
                    pass

    async def transcript(self, task):
        if task not in self.watches:
            self.next_id += 1
            rid = self.next_id
            state = Transcript()
            self.watches[task] = (rid, state)
            self.streams[rid] = state
            self.transcript_subscriptions[task] = self.transcript_subscriptions.get(task, 0) + 1
            await self.ws.send(json.dumps({"id": rid, "method": "WatchDocMessages", "params": {"chatId": task}}))
        _, state = self.watches[task]
        await asyncio.wait_for(state.ready.wait(), 90)
        if state.error:
            raise state.error
        return copy.deepcopy(state.entries)

    async def unwatch(self, task):
        held = self.watches.pop(task, None)
        if held:
            rid, _ = held
            self.streams.pop(rid, None)
            try:
                await self.ws.send(json.dumps({"id": rid, "cancel": True}))
            except websockets.ConnectionClosed:
                pass

    async def close(self):
        for task in list(self.watches):
            await self.unwatch(task)
        self.reader.cancel()
        await asyncio.gather(self.reader, return_exceptions=True)


def save(path, value):
    path.write_text(json.dumps(value, indent=2, ensure_ascii=False) + "\n")


def text(entries, role):
    return "\n".join(part.get("text", "") for entry in entries if entry["role"] == role
                     for part in entry["parts"] if part.get("kind") == "text")


def digest(entries):
    return hashlib.sha256(json.dumps(entries, sort_keys=True).encode()).hexdigest()


def provider_cwd(worktree):
    cwd = (worktree / "target/qa/sidebar/provider-workspace").resolve()
    cwd.mkdir(parents=True, exist_ok=True)
    return str(cwd)


async def main(args):
    worktree = Path(__file__).resolve().parent.parent
    data = (worktree / "target/luna-qa-data/sidebar").resolve()
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=True)
    async with websockets.connect("ws://127.0.0.1:59787", max_size=16 * 1024 * 1024) as websocket:
        async with Rpc(websocket) as rpc:
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
            cwd = provider_cwd(worktree)
            run = {"parent_id": task, "marker": marker, "model": "gpt-6-luna", "cwd": cwd, "created_at_ms": int(time.time() * 1000)}
            save(out / "run.json", run)  # No replay even if a later response is lost.
            config = {"harness": "codex", "model": "gpt-6-luna", "reasoning": "high",
                      "sandbox": "read-only", "permissionMode": "ask", "effectivePermissionMode": "ask"}
            await rpc.request("Mutate", {"op": "createChat", "chatId": task, "deviceId": info["deviceId"], "config": config, "cwd": cwd})
            await rpc.request("Mutate", {"op": "renameChat", "chatId": task, "title": "Sidebar QA · context and history"})
            await rpc.transcript(task)  # One subscription exists before provider work.
            snapshot = (await rpc.request("GetTaskCommandSnapshot", {"taskId": task}))["snapshot"]
            prompt = f"Remember this exact harmless marker for this conversation: {marker}. Reply exactly PARENT_READY. Do not use tools or change files."
            command_id = str(uuid.uuid4())
            parent_message_id = str(uuid.uuid4())
            run.update(parent_command_id=command_id, parent_message_id=parent_message_id)
            save(out / "run.json", run)
            request = {"prompt": prompt, "harness": "codex", "model": "gpt-6-luna", "reasoning": "high",
                       "cwd": cwd, "sandbox": "read-only", "permissionMode": "ask", "resume": None}
            receipt = await rpc.request("ExecuteTaskCommand", {"commandId": command_id, "taskId": task,
                "expectedTaskRevision": snapshot["taskRevision"], "expectedBindingGeneration": snapshot.get("bindingGeneration"),
                "intent": {"kind": "session", "command": {"kind": "run", "request": request, "messageId": parent_message_id}}})
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
            run["side_request_id"] = str(uuid.uuid4())
            save(out / "run.json", run)
            created = await rpc.request("CreateSideChat", {"parentId": task, "requestId": run["side_request_id"]})
            assert created["meta"]["contextMode"] == "nativeFork"
            side = created["meta"]["id"]
            lease = created["lease"]
            run.update(side_chat_id=side, parent_digest=digest(before))
            save(out / "run.json", run)
            save(out / "side-created.json", created["meta"])
            run["side_message_id"] = str(uuid.uuid4())
            save(out / "run.json", run)
            await rpc.request("SendSideChat", {"id": side, "lease": lease, "messageId": run["side_message_id"],
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
                "transcript_subscriptions": rpc.transcript_subscriptions,
                "elapsed_ms": int(time.time() * 1000) - run["created_at_ms"]})
            print("SIDEBAR_NATIVE_CONTEXT_HISTORY_PASS")

if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("output", type=Path)
    asyncio.run(main(parser.parse_args()))
