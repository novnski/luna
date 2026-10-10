#!/usr/bin/env python3
"""Subprocess E2E for repository queue claims, recovery and durable acceptance."""
import argparse
from concurrent.futures import ThreadPoolExecutor
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parent.parent
CLI = ROOT / "scripts/pending_tasks.py"


def main(output):
    output.mkdir(parents=True, exist_ok=False)
    repo = output / "repo"
    repo.mkdir()
    commands = []
    cases = []

    def shell(argv, cwd=repo):
        result = subprocess.run(argv, cwd=cwd, capture_output=True, text=True, timeout=30)
        if result.returncode:
            raise AssertionError(f"command failed: {argv}: {result.stderr}")
        return result.stdout.strip()

    shell(["git", "init", "-b", "main"])
    shell(["git", "config", "user.email", "queue-fixture@example.invalid"])
    shell(["git", "config", "user.name", "Queue E2E"])
    folder = repo / "docs/pending-tasks"
    folder.mkdir(parents=True)
    (folder / "spec.md").write_text("# Independent fixture contract\n")
    (repo / "proof.txt").write_text("Independent acceptance and stopped-worker readback\n")

    def spec(ident, resource, status="ready", deps=None):
        return {"id": ident, "title": "Fixture " + ident, "group": "fixture", "priority": 1,
                "order": int(ident[-1]), "status": status, "aliases": [], "depends_on": deps or [],
                "resources": [resource], "sources": ["docs/pending-tasks/spec.md"],
                "outcome": "Independently observable fixture outcome", "resume": "Inspect proof and retained work, then continue",
                "criteria": [{"id": "behavior", "text": "Independent behavior proof", "weight": 2},
                             {"id": "delivery", "text": "Repeatable delivered evidence", "weight": 1}]}

    tasks = [spec("A-1", "engine/commands"), spec("A-2", "engine"),
             spec("B-1", "engine/commands", deps=["A-1"]), spec("C-1", "ui/files"),
             spec("D-1", "other", "deferred"), spec("X-1", "external", "blocked")]
    tasks[-1]["blocker"] = "External fixture service unavailable"
    reference = spec("R-1", "reference", "done")
    reference.update(kind="reference", baseline_evidence=[{"ref": "docs/pending-tasks/spec.md"}])
    catalog = {"schema_version": 1, "revision": 1,
               "groups": [{"id": "fixture", "title": "Fixture", "milestones": {"first": ["A-1", "B-1"]}}],
               "tasks": tasks + [reference]}
    catalog_path = folder / "catalog.json"
    catalog_path.write_text(json.dumps(catalog, indent=2) + "\n")
    shell(["git", "add", "docs", "proof.txt"])
    shell(["git", "commit", "-m", "Independent queue fixture"])
    peer = output / "peer"
    shell(["git", "worktree", "add", "-b", "peer", str(peer)])

    def call(*args, worktree=repo, expected=0, state=None):
        argv = [sys.executable, str(CLI), "--catalog", str(worktree / "docs/pending-tasks/catalog.json")]
        if state:
            argv += ["--state-dir", str(state)]
        argv += list(args)
        result = subprocess.run(argv, cwd=worktree, capture_output=True, text=True, timeout=30)
        record = {"command": argv, "exit_code": result.returncode, "stdout": result.stdout, "stderr": result.stderr}
        commands.append(record)
        if result.returncode != expected:
            raise AssertionError(f"expected exit {expected}: {args}: {result.stdout} {result.stderr}")
        body = json.loads(result.stdout)
        return body.get("result", body)

    def case(name):
        cases.append(name)

    try:
        assert call("validate")["tasks"] == 7
        status = call("status")
        assert status["active_scope"]["tasks"] == 5 and status["active_scope"]["acceptance_percent"] == 0
        assert call("next")["reserved"] is False
        assert call("list", "--status", "in_progress") == []
        case("honest active-scope progress; read-only next does not reserve")

        with ThreadPoolExecutor(max_workers=2) as pool:
            # Both real subprocesses race. Exactly one wins; the loser may be either process.
            def contender(i, tree):
                argv = [sys.executable, str(CLI), "--catalog", str(tree / "docs/pending-tasks/catalog.json"),
                        "claim", "A-1", "--owner", f"agent-{i}", "--session", f"session-{i}"]
                result = subprocess.run(argv, cwd=tree, capture_output=True, text=True, timeout=30)
                commands.append({"command": argv, "exit_code": result.returncode, "stdout": result.stdout, "stderr": result.stderr})
                return result.returncode, json.loads(result.stdout)
            futures = [pool.submit(contender, 1, repo), pool.submit(contender, 2, peer)]
            outcomes = [f.result() for f in futures]
        assert sorted(code for code, _ in outcomes) == [0, 1]
        claimed = next(body["result"] for code, body in outcomes if code == 0)
        token = claimed["claim_token"]
        assert call("show", "A-1", worktree=peer)["lease"]["session"] == claimed["lease"]["session"]
        call("claim", "A-2", "--owner", "other", "--session", "overlap", expected=1)
        call("claim", "B-1", "--owner", "other", "--session", "dependency", expected=1)
        second = call("claim", "--owner", "independent", "--session", "independent")
        assert second["id"] == "C-1"
        call("claim", "--owner", "independent", "--session", "independent", expected=1)
        case("cross-worktree atomic claim; resource-prefix conflicts and dependency/session exclusion")

        call("check", "A-1", "behavior", "--token", "wrong", "--note", "should fail", "--evidence", "proof.txt", expected=1)
        call("check", "A-1", "behavior", "--token", token, "--note", "Missing evidence", "--evidence", "absent.txt", expected=1)
        partial = call("check", "A-1", "behavior", "--token", token, "--note", "Behavior accepted", "--evidence", "proof.txt")
        assert partial["progress_percent"] == 66.7
        call("finish", "A-1", "--token", token, "--note", "Premature completion", "--evidence", "proof.txt", expected=1)
        call("pause", "A-1", "--token", token, "--note", "Behavior passed; next run delivery and inspect partial files")
        assert call("next")["candidate"]["id"] == "A-1"
        call("claim", "A-2", "--owner", "other", "--session", "paused-overlap", expected=1)
        resumed = call("claim", "--owner", "resumer", "--session", "resumed")
        newer = resumed["claim_token"]
        assert resumed["id"] == "A-1" and newer != token and len(resumed["checks"]) == 1
        call("heartbeat", "A-1", "--token", token, expected=1)
        call("heartbeat", "A-1", "--token", newer)
        call("block", "A-1", "--token", newer, "--note", "Await fixture delivery; partial work is retained", "--reason", "fixture dependency offline")
        assert call("next")["candidate"] is None
        call("unblock", "A-1", "--note", "Fixture service readback now ready", "--evidence", "proof.txt")
        resumed = call("claim", "--owner", "finisher", "--session", "finisher")
        newer = resumed["claim_token"]
        call("check", "A-1", "delivery", "--token", newer, "--note", "Delivered", "--evidence", "commit:" + shell(["git", "rev-parse", "HEAD"]))
        call("review", "A-1", "--token", newer, "--note", "Review current patch and evidence")
        call("finish", "A-1", "--token", newer, "--note", "Accepted and delivered", "--evidence", "proof.txt")
        assert call("show", "A-1")["status"] == "done"
        assert any(e["action"] == "pause" for e in call("history", "A-1"))
        case("criterion evidence, progress, handoff, blocked work, stale-token fencing and completion gates")

        expired = call("claim", "B-1", "--owner", "crashed", "--session", "crashed", "--lease-seconds", "1")
        deadline = expired["lease"]["expires_at"]
        while time.time() <= deadline:
            time.sleep(min(0.05, max(0.001, deadline - time.time())))
        assert "B-1" in call("status")["expired_claims"]
        call("claim", "B-1", "--owner", "stealer", "--session", "stealer", expected=1)
        call("heartbeat", "B-1", "--token", expired["claim_token"], expected=1)
        call("recover", "B-1", "--note", "Independent worker/process readback confirms prior fixture stopped", "--evidence", "proof.txt")
        assert call("next")["candidate"]["id"] == "B-1"
        recovered = call("claim", "--owner", "recovered", "--session", "recovered")
        call("check", "B-1", "behavior", "--token", expired["claim_token"], "--note", "Old worker cannot write", "--evidence", "proof.txt", expected=1)
        call("pause", "B-1", "--token", recovered["claim_token"], "--note", "Resume B-1 from real retained proof")
        case("expired claims block duplication; evidenced recovery creates fresh authority")

        new_task = spec("N-1", "new-area")
        new_task["aliases"] = ["LEGACY-N"]
        incoming = output / "new-task.json"
        incoming.write_text(json.dumps(new_task))
        call("add", "--file", str(incoming))
        assert call("show", "LEGACY-N")["id"] == "N-1"
        call("claim", "N-1", "--owner", "stale", "--session", "stale", worktree=peer, expected=1)
        call("sync", worktree=peer, expected=1)
        call("snapshot", "--output", str(output / "stale.json"), worktree=peer, expected=1)
        current = json.loads(catalog_path.read_text())
        assert current["revision"] == 2
        (peer / "docs/pending-tasks/catalog.json").write_text(catalog_path.read_text())
        assert call("sync", worktree=peer)["active_scope"]["tasks"] == 6
        assert call("show", "A-1")["status"] == "done"
        assert call("show", "C-1")["lease"]["session"] == "independent"
        case("new-task intake and aliases; stale-worktree fencing; sync preserves progress and active claims")

        modified = json.loads(catalog_path.read_text())
        modified["revision"] += 1
        next(t for t in modified["tasks"] if t["id"] == "C-1")["outcome"] = "Changed active contract"
        catalog_path.write_text(json.dumps(modified))
        call("sync", expected=1)
        catalog_path.write_text(json.dumps(current, indent=2) + "\n")
        case("catalog update cannot rewrite live contracts")

        concurrent_files = []
        for ident in ("N-2", "N-3"):
            path = output / (ident + ".json")
            path.write_text(json.dumps(spec(ident, ident.lower())))
            concurrent_files.append(path)
        def intake(path):
            argv = [sys.executable, str(CLI), "--catalog", str(catalog_path), "add", "--file", str(path)]
            result = subprocess.run(argv, cwd=repo, capture_output=True, text=True, timeout=30)
            commands.append({"command": argv, "exit_code": result.returncode, "stdout": result.stdout, "stderr": result.stderr})
            return path, result.returncode
        with ThreadPoolExecutor(max_workers=2) as pool:
            outcomes = list(pool.map(intake, concurrent_files))
        for path, code in outcomes:
            if code:
                call("add", "--file", str(path))
        current = json.loads(catalog_path.read_text())
        assert {"N-1", "N-2", "N-3"} <= {t["id"] for t in current["tasks"]}
        assert current["revision"] == 4
        assert call("show", "A-1")["status"] == "done"
        case("concurrent future intake preserves both additions and existing acceptance")

        for title, mutate in [
            ("cycle", lambda d: d["tasks"][0]["depends_on"].append("B-1")),
            ("missing dependency", lambda d: d["tasks"][0]["depends_on"].append("UNKNOWN-1")),
            ("duplicate alias", lambda d: d["tasks"][0]["aliases"].append("A-2")),
            ("source escape", lambda d: d["tasks"][0]["sources"].append("../../../outside.md")),
            ("bad milestone", lambda d: d["groups"][0]["milestones"].update(invalid=["UNKNOWN-1"]))
        ]:
            bad = json.loads(json.dumps(current))
            mutate(bad)
            catalog_path.write_text(json.dumps(bad))
            call("validate", expected=1)
            case("catalog rejects " + title)
        catalog_path.write_text(json.dumps(current, indent=2) + "\n")

        snapshot = output / "checkpoint.json"
        call("snapshot", "--output", str(snapshot))
        exported = json.loads(snapshot.read_text())
        assert token not in snapshot.read_text() and newer not in snapshot.read_text()
        fresh = output / "restored-state"
        call("restore", "--file", str(snapshot), state=fresh)
        restored = call("show", "C-1", state=fresh)
        assert restored["status"] == "paused" and restored["lease"] is None
        assert call("show", "A-1", state=fresh)["progress_percent"] == 100
        assert any(e["action"].startswith("imported:") for e in call("history", "A-1", state=fresh))
        original_times = [e["at"] for e in call("history", "A-1")]
        assert original_times == [e["at"] for e in call("history", "A-1", state=fresh)]
        call("restore", "--file", str(snapshot), expected=1)
        corrupt = json.loads(snapshot.read_text())
        next(t for t in corrupt["tasks"] if t["id"] == "B-1")["status"] = "done"
        corrupt_path = output / "invalid-snapshot.json"
        corrupt_path.write_text(json.dumps(corrupt))
        call("restore", "--file", str(corrupt_path), state=output / "invalid-state", expected=1)
        call("snapshot", "--output", str(catalog_path), expected=1)
        portable = folder / "progress.json"
        portable.write_text(json.dumps({"catalog_hash": "wrong"}))
        call("validate", expected=1)
        portable.unlink()
        case("portable restart keeps acceptance/history and strips live authority; rejects corrupt or destructive restore")

        call("activate", "D-1", "--note", "Selected optional fixture scope")
        call("defer", "D-1", "--note", "Optional scope postponed")
        call("unblock", "X-1", "--note", "Fresh external readback succeeds", "--evidence", "proof.txt")
        call("cancel", "X-1", "--note", "Fixture scope no longer required")
        call("uncheck", "C-1", "behavior", "--token", second["claim_token"], "--note", "Invalidate only current criterion")
        call("pause", "C-1", "--token", second["claim_token"], "--note", "Finish fixture safely; retain partial context")
        case("explicit deferred/blocked/cancelled lifecycle and criterion invalidation")
        result = {"ok": True, "exit_code": 0, "cases": cases, "command_count": len(commands),
                  "repo": str(repo), "peer": str(peer), "command": f"python3 scripts/qa-pending-tasks.py --output {output}"}
    except Exception as error:
        result = {"ok": False, "exit_code": 1, "cases": cases, "error": str(error), "command_count": len(commands)}
    (output / "commands.json").write_text(json.dumps(commands, indent=2) + "\n")
    (output / "result.json").write_text(json.dumps(result, indent=2) + "\n")
    hashes = {p.name: hashlib.sha256(p.read_bytes()).hexdigest() for p in (output / "commands.json", output / "result.json")}
    (output / "sha256.json").write_text(json.dumps(hashes, indent=2) + "\n")
    print(json.dumps(result, indent=2))
    return result["exit_code"]


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    sys.exit(main(args.output.resolve()))
