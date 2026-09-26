#!/usr/bin/env python3
"""Bounded real native-subagent acceptance, only after explicit native grant.

prepare creates an inspectable disposable parent without provider work. Select
that task and open Agents in the QA UI, then run once. One native child runs
only /bin/sleep 20 and returns a marker. It never substitutes summaries for an
agent. Retain the task for native click/AX/screenshots before archiving it.
"""
import argparse
import asyncio
import hashlib
import importlib.util
import json
from pathlib import Path
import time
import uuid

import websockets

spec = importlib.util.spec_from_file_location("sidebar_driver", Path(__file__).with_name("qa-sidebar.py"))
driver = importlib.util.module_from_spec(spec)
spec.loader.exec_module(driver)


def agents(entries):
    result = []
    for entry in entries:
        for part in entry["parts"]:
            call = part.get("call", {})
            name = call.get("name", call.get("tool", ""))
            if part.get("kind") == "tool" and (name == "Agent" or name.startswith("Agent: ")):
                result.append({key: part[key] for key in ("id", "call", "resolved", "isError", "subagentRef", "subagentStatus", "subagentTail", "subagentActivity") if key in part})
    return result


async def main(args):
    worktree = Path(__file__).resolve().parent.parent
    root = (worktree / "target/luna-qa-data/sidebar").resolve()
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=True)
    async with websockets.connect("ws://127.0.0.1:59787", max_size=16 * 1024 * 1024) as ws:
        async with driver.Rpc(ws) as rpc:
            claim = await rpc.request("LocalOwnerRootClaim")
            assert claim["claim"] == hashlib.sha256(b"LUNA local root v1\0" + bytes(root)).hexdigest(), "Wrong QA profile"
            config = {"harness": "codex", "model": "gpt-6-luna", "reasoning": "high", "sandbox": "workspace-write", "permissionMode": "auto", "effectivePermissionMode": "auto"}
            path = out / "run.json"
            if args.mode == "prepare":
                assert not path.exists(), "Existing run is inspectable; do not recreate it"
                tag = uuid.uuid4().hex[:10]
                run = {"parent_id": str(uuid.uuid4()), "child_marker": "LUNA_NATIVE_CHILD_" + tag,
                       "parent_marker": "LUNA_NATIVE_PARENT_" + tag, "model": "gpt-6-luna", "executed": False}
                driver.save(path, run)
                info = await rpc.request("EngineInfo")
                await rpc.request("Mutate", {"op": "createChat", "chatId": run["parent_id"], "deviceId": info["deviceId"], "config": config, "cwd": str(out)})
                await rpc.request("Mutate", {"op": "renameChat", "chatId": run["parent_id"], "title": "Sidebar QA · real native agent"})
                print("SIDEBAR_NATIVE_AGENT_PREPARED " + run["parent_id"], flush=True)
                return
            run = json.loads(path.read_text())
            assert not run["executed"], "Run already attempted; inspect evidence instead of replaying"
            task = run["parent_id"]
            await rpc.transcript(task)
            snapshot = (await rpc.request("GetTaskCommandSnapshot", {"taskId": task}))["snapshot"]
            prompt = ("Use exactly ONE native subagent. Ask it to run exactly /bin/sleep 20 once, then reply exactly "
                      + run["child_marker"] + ". Use gpt-6-luna for the child if native model selection is supported. "
                      "Do not write files, inspect files, access the network, or use other child tools. "
                      "Wait for that native child to finish, then reply exactly " + run["parent_marker"]
                      + ". Only native spawn/wait and the child's sleep are requested. If native subagents are unavailable, say so; do not simulate one.")
            command = {"commandId": str(uuid.uuid4()), "taskId": task,
                       "expectedTaskRevision": snapshot["taskRevision"], "expectedBindingGeneration": snapshot.get("bindingGeneration"),
                       "expectedTurnId": snapshot.get("activeTurnId"), "intent": {"kind": "session", "command": {"kind": "run", "messageId": str(uuid.uuid4()),
                           "request": {**config, "prompt": prompt, "cwd": str(out), "resume": None}}}}
            run.update(executed=True, command_id=command["commandId"])
            driver.save(path, run)
            driver.save(out / "request.json", command)
            receipt = await rpc.request("ExecuteTaskCommand", command)
            driver.save(out / "admission.json", receipt)
            assert receipt["status"] in ("pending", "applied"), receipt
            observations = []
            last = None
            running = False
            started = time.monotonic()
            rows = []
            while time.monotonic() - started < 180:
                entries = await rpc.transcript(task)
                rows = agents(entries)
                if rows != last:
                    last = rows
                    observations.append({"elapsed_seconds": round(time.monotonic() - started, 3), "agents": rows})
                    driver.save(out / "native-observations.json", observations)
                    driver.save(out / "parent-last.json", entries)
                    print(json.dumps({"event": "native-agent-update", "agents": rows}), flush=True)
                running |= any(row.get("subagentStatus") == "running" and row.get("subagentRef") for row in rows)
                if driver.text(entries, "assistant").strip().endswith(run["parent_marker"]) and all(entry.get("status") != "streaming" for entry in entries):
                    break
                await asyncio.sleep(0.2)
            else:
                raise RuntimeError("Native subagent acceptance timed out; inspect saved provider evidence, do not replay")
            assert len(rows) == 1, "Expected exactly one actual native spawn"
            assert running, "No active native child with transcript reference was observed"
            driver.save(out / "parent-complete.json", entries)
            child = rows[0].get("subagentRef")
            assert child and rows[0].get("subagentStatus") in ("done", "idle"), "Child completion unavailable"
            child_entries = await rpc.transcript(child)
            assert run["child_marker"] in driver.text(child_entries, "assistant"), "Actual child transcript did not retain the result"
            driver.save(out / "child-result.json", child_entries)
            run["child_doc_id"] = child
            driver.save(path, run)
            driver.save(out / "result.json", {"passed": True, "native_spawn_count": 1, "running_observed": True,
                "result_from_child_transcript": True, "child_doc_id": child, "transcript_subscriptions": rpc.transcript_subscriptions})
            print("SIDEBAR_NATIVE_AGENT_OBSERVATION_PASS", flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("mode", choices=("prepare", "run"))
    parser.add_argument("output", type=Path)
    asyncio.run(main(parser.parse_args()))
