#!/usr/bin/env python3
"""Repository backlog: versioned contracts and transactional local coordination."""
import argparse
import contextlib
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import re
import secrets
import sqlite3
import subprocess
import sys
import tempfile
import time
from zoneinfo import ZoneInfo, ZoneInfoNotFoundError

ROOT = Path(__file__).resolve().parent.parent
STATUSES = {"ready", "in_progress", "paused", "blocked", "review", "done", "deferred", "cancelled"}
LIVE = {"in_progress", "review"}


def fail(message):
    raise ValueError(message)


def encode(value):
    return json.dumps(value, ensure_ascii=False, sort_keys=True)


def digest(value):
    return hashlib.sha256(encode(value).encode()).hexdigest()


def atomic_json(path, data):
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix=".pending-", dir=path.parent)
    try:
        with os.fdopen(fd, "w") as stream:
            stream.write(json.dumps(data, ensure_ascii=False, indent=2) + "\n")
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def git(*args, root=ROOT):
    return subprocess.check_output(["git", "-C", str(root), *args], text=True).strip()


def read_catalog(path):
    data = json.loads(path.read_text())
    validate(data, path.parent.parent.parent)
    return data


def validate(data, root):
    if not isinstance(data, dict):
        fail("catalog must be a JSON object")
    if data.get("schema_version") != 1 or type(data.get("revision")) is not int or data["revision"] < 1:
        fail("catalog needs schema_version=1 and a positive integer revision")
    groups = data.get("groups", [])
    if not isinstance(groups, list) or not groups or any(not isinstance(g, dict) or not g.get("id") or not g.get("title") for g in groups):
        fail("catalog needs named groups")
    group_ids = [g["id"] for g in groups]
    if len(group_ids) != len(set(group_ids)):
        fail("duplicate group")
    tasks = data.get("tasks", [])
    if not isinstance(tasks, list) or any(not isinstance(t, dict) for t in tasks):
        fail("tasks must be a list of JSON objects")
    names = {}
    ids = set()
    for task in tasks:
        ident = task.get("id", "")
        if not re.fullmatch(r"[A-Z][A-Z0-9]*-[A-Z0-9]+", ident) or ident in ids:
            fail(f"invalid or duplicate task ID: {ident}")
        ids.add(ident)
        for name in [ident, *task.get("aliases", [])]:
            if not isinstance(name, str) or not name or name in names:
                fail(f"duplicate or invalid alias: {name}")
            names[name] = ident
        if task.get("group") not in group_ids or task.get("status") not in STATUSES - LIVE:
            fail(f"invalid group or initial status: {ident}")
        if type(task.get("priority")) is not int or task["priority"] not in range(4):
            fail(f"priority must be 0..3: {ident}")
        for key in ("title", "outcome", "resume"):
            if not isinstance(task.get(key), str) or not task[key].strip():
                fail(f"missing {key}: {ident}")
        for key in ("sources", "resources", "criteria"):
            if not isinstance(task.get(key), list) or not task[key]:
                fail(f"missing {key}: {ident}")
        if not isinstance(task.get("depends_on"), list) or any(not isinstance(d, str) for d in task["depends_on"]):
            fail(f"invalid dependencies: {ident}")
        if any(not isinstance(r, str) or not re.fullmatch(r"[a-z0-9_./-]+", r) for r in task["resources"]):
            fail(f"invalid resource: {ident}")
        criteria = task["criteria"]
        if any(not isinstance(c, dict) for c in criteria):
            fail(f"criteria must be JSON objects: {ident}")
        cids = [c.get("id") for c in criteria]
        if any(not isinstance(c, str) or not re.fullmatch(r"[a-z0-9-]+", c) for c in cids) or len(cids) != len(set(cids)):
            fail(f"invalid or duplicate criteria: {ident}")
        if any(not c.get("text") or type(c.get("weight")) is not int or c["weight"] < 1 for c in criteria):
            fail(f"invalid criterion text/weight: {ident}")
        for source in task["sources"]:
            if not isinstance(source, str):
                fail(f"invalid source: {ident}")
            relative = Path(source.split("#", 1)[0])
            resolved = (root / relative).resolve()
            if relative.is_absolute() or not resolved.is_relative_to(root.resolve()) or not resolved.is_file():
                fail(f"missing or escaping source for {ident}: {source}")
        if task["status"] == "done" and not task.get("baseline_evidence"):
            fail(f"completed baseline needs evidence: {ident}")
        if task["status"] == "blocked" and not task.get("blocker"):
            fail(f"blocked baseline needs a reason: {ident}")
    if not tasks:
        fail("empty catalog")
    graph = {t["id"]: t["depends_on"] for t in tasks}
    visiting, visited = set(), set()

    def visit(ident):
        if ident in visiting:
            fail(f"dependency cycle at {ident}")
        if ident in visited:
            return
        if ident not in graph:
            fail(f"unknown dependency: {ident}; use canonical IDs")
        visiting.add(ident)
        for dep in graph[ident]:
            visit(dep)
        visiting.remove(ident)
        visited.add(ident)

    for ident in graph:
        visit(ident)
    for group in groups:
        for member in group.get("milestones", {}).values():
            if not member or any(ident not in graph for ident in member):
                fail(f"invalid milestone in {group['id']}")
    return names


def evidence(values, root):
    if not values:
        fail("at least one verifiable --evidence is required")
    records = []
    for value in values:
        if value.startswith("commit:"):
            rev = value[7:]
            if not re.fullmatch(r"[0-9a-f]{7,40}", rev):
                fail("commit evidence must use a hexadecimal SHA")
            sha = git("rev-parse", "--verify", rev + "^{commit}", root=root)
            records.append({"ref": "commit:" + sha})
        else:
            path = Path(value)
            path = path if path.is_absolute() else root / path
            if not path.is_file():
                fail(f"evidence file does not exist: {value}")
            hashed = hashlib.sha256()
            with path.open("rb") as stream:
                for block in iter(lambda: stream.read(1024 * 1024), b""):
                    hashed.update(block)
            records.append({"ref": value, "sha256": hashed.hexdigest()})
    return records


def note(value):
    if not value or not value.strip():
        fail("a useful --note is required")
    return value.strip()


def event(db, ident, action, detail):
    db.execute("INSERT INTO events(task, at, action, detail) VALUES (?, ?, ?, ?)",
               (ident, time.time(), action, encode(detail)))


def save(db, task):
    db.execute("INSERT OR REPLACE INTO tasks(id, data) VALUES (?, ?)", (task["id"], encode(task)))


def rows(db):
    return {ident: json.loads(data) for ident, data in db.execute("SELECT id, data FROM tasks")}


def initial(spec):
    completed = spec["status"] == "done"
    return {"id": spec["id"], "spec": spec, "status": spec["status"], "checks": {
        c["id"]: {"at": None, "note": "Documented shipped baseline; not recertified by queue setup",
                  "evidence": spec["baseline_evidence"]} for c in spec["criteria"]
    } if completed else {}, "lease": None, "attempts": 0, "updated_at": time.time(),
        "note": spec["resume"], "blocker": spec.get("blocker"), "last_worker": None}


def reconcile(db, catalog):
    current = rows(db)
    revision = db.execute("SELECT value FROM meta WHERE key='catalog_revision'").fetchone()
    old_hash = db.execute("SELECT value FROM meta WHERE key='catalog_hash'").fetchone()
    if revision and (catalog["revision"] < int(revision[0]) or
                     catalog["revision"] == int(revision[0]) and old_hash[0] != digest(catalog)):
        fail("older or conflicting catalog revision; update from main and increment revision for edits")
    specs = {t["id"]: t for t in catalog["tasks"]}
    if set(current) - set(specs):
        fail("catalog may not delete existing tickets; cancel/defer them and retain history")
    for ident, spec in specs.items():
        if ident not in current:
            save(db, initial(spec))
            event(db, ident, "catalog_add", {"revision": catalog["revision"]})
            continue
        task = current[ident]
        if task["spec"] != spec:
            if task["status"] in LIVE:
                fail(f"cannot edit an owned contract: {ident}; pause with a handoff first")
            if task["attempts"] and task["status"] not in {"done", "cancelled"} and not set(task["spec"]["resources"]) <= set(spec["resources"]):
                fail(f"cannot remove reserved code areas from unfinished work: {ident}")
            old_criteria = {c["id"]: c for c in task["spec"]["criteria"]}
            if any(old_criteria.get(c["id"]) != c for c in spec["criteria"] if c["id"] in task["checks"]):
                fail(f"cannot rewrite accepted criteria: {ident}; add a new criterion or new ticket")
            if set(task["checks"]) - {c["id"] for c in spec["criteria"]}:
                fail(f"cannot delete accepted criteria: {ident}")
            if task["status"] == "done" and set(task["checks"]) != {c["id"] for c in spec["criteria"]}:
                fail(f"cannot expand a completed ticket: {ident}; add a follow-up")
            task["spec"] = spec
            save(db, task)
            event(db, ident, "catalog_update", {"revision": catalog["revision"]})
    for key, value in (("catalog_hash", digest(catalog)), ("catalog_revision", str(catalog["revision"])),
                       ("groups", encode(catalog["groups"]))):
        db.execute("INSERT OR REPLACE INTO meta VALUES (?, ?)", (key, value))


def overlap(first, second):
    return first == second or first.startswith(second + "/") or second.startswith(first + "/")


def eligibility(task, all_tasks):
    reasons = []
    if task["status"] not in {"ready", "paused"}:
        reasons.append("status: " + task["status"])
    reasons.extend("dependency: " + dep for dep in task["spec"]["depends_on"]
                   if all_tasks[dep]["status"] != "done")
    for other in all_tasks.values():
        reserved = other["status"] in LIVE or other["attempts"] > 0 and other["status"] not in {"done", "cancelled"}
        if other["id"] != task["id"] and reserved and any(
                overlap(a, b) for a in task["spec"]["resources"] for b in other["spec"]["resources"]):
            reasons.append("resource reserved by: " + other["id"] + " (" + other["status"] + ")")
    return reasons


def public(task, all_tasks):
    data = dict(task)
    if task["lease"]:
        data["lease"] = {k: v for k, v in task["lease"].items() if k != "token"}
        data["lease"]["expired"] = time.time() >= data["lease"]["expires_at"]
    total = sum(c["weight"] for c in task["spec"]["criteria"])
    done = sum(c["weight"] for c in task["spec"]["criteria"] if c["id"] in task["checks"])
    data.update(progress_percent=round(100 * done / total, 1), accepted_units=done, total_units=total,
                ineligible_reasons=eligibility(task, all_tasks))
    return data


def select(all_tasks, group=None):
    available = [t for t in all_tasks.values() if not eligibility(t, all_tasks)
                 and (group is None or t["spec"]["group"] == group)]
    return sorted(available, key=lambda t: (0 if t["status"] == "paused" or t["attempts"] else 1,
                                          t["spec"]["priority"], t["spec"].get("order", 999), t["id"]))


def authorize(task, token):
    lease = task["lease"]
    if task["status"] not in LIVE or not lease or not token or not secrets.compare_digest(lease["token"], token):
        fail("not the current claim holder; use the exact token returned by claim")
    if time.time() >= lease["expires_at"]:
        fail("claim expired; verify the previous worker is stopped, then recover and claim again")


def release(task):
    if task["lease"]:
        task["last_worker"] = {k: v for k, v in task["lease"].items() if k != "token"}
    task["lease"] = None


def summary(all_tasks, groups):
    result = {"counts": {s: sum(t["status"] == s for t in all_tasks.values()) for s in sorted(STATUSES)},
              "groups": [], "expired_claims": [t["id"] for t in all_tasks.values() if t["lease"]
                                               and time.time() >= t["lease"]["expires_at"]]}
    active = [t for t in all_tasks.values() if t["status"] not in {"deferred", "cancelled"}
              and t["spec"].get("kind") != "reference"]

    def coverage(tasks):
        total = sum(c["weight"] for t in tasks for c in t["spec"]["criteria"])
        accepted = sum(c["weight"] for t in tasks for c in t["spec"]["criteria"] if c["id"] in t["checks"])
        done = sum(t["status"] == "done" for t in tasks)
        return {"tasks": len(tasks), "done": done, "remaining": len(tasks) - done,
                "accepted_units": accepted, "total_units": total,
                "acceptance_percent": round(100 * accepted / total, 1) if total else None}

    result["active_scope"] = coverage(active)
    result["percentage_basis"] = "weighted accepted criteria; excludes deferred, cancelled and shipped reference tickets; not time or effort"
    for group in groups:
        group_tasks = [t for t in active if t["spec"]["group"] == group["id"]]
        result["groups"].append({**group, **coverage(group_tasks), "milestone_status": {
            name: {"done": all(all_tasks[i]["status"] == "done" for i in ids), "members": ids}
            for name, ids in group.get("milestones", {}).items()}})
    result["next"] = [t["id"] for t in select(all_tasks)[:8]]
    return result


def markdown_report(all_tasks, groups, output, root, state_dir):
    report = summary(all_tasks, groups)
    active = report["active_scope"]
    try:
        report_zone = ZoneInfo("Europe/Bucharest")
    except ZoneInfoNotFoundError:
        report_zone = timezone.utc
    lines = ["# Pending task checkpoint", "",
             "Generated " + datetime.now(report_zone).isoformat(timespec="seconds") + ".",
             "This is a saved checkpoint. Run `python3 scripts/pending_tasks.py status` for live ownership.", "",
             f"Active scope: **{active['acceptance_percent']}% accepted**, **{active['done']}/{active['tasks']} Done**.",
             "Coverage measures accepted criteria, not elapsed time or estimated effort.", "",
             "Next eligible: " + (", ".join(report["next"]) or "none"), "",
             "Live coordination store: `" + str(state_dir) + "`.", ""]
    for group in groups:
        lines.extend(["## " + group["title"], "", "| Ticket | Status | Accepted | Owner / last worker |", "| --- | --- | --- | --- |"])
        for task in all_tasks.values():
            if task["spec"]["group"] != group["id"]:
                continue
            view = public(task, all_tasks)
            spec = task["spec"]
            worker = view["lease"] or task["last_worker"] or {}
            title = (spec["id"] + " — " + spec["title"]).replace("|", "\\|")
            source = root / spec["sources"][0].split("#", 1)[0]
            link = Path(os.path.relpath(source, output.parent)).as_posix()
            owner = worker.get("owner", "—").replace("|", "\\|").replace("\n", " ")
            status = task["status"] + (" (reference)" if spec.get("kind") == "reference" else "")
            lines.append(f"| [{title}]({link}) | {status} | {view['progress_percent']}% | {owner} |")
        lines.append("")
    output.write_text("\n".join(lines).rstrip() + "\n")


def parser():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--catalog", type=Path, default=ROOT / "docs/pending-tasks/catalog.json")
    p.add_argument("--state-dir", type=Path, help="shared local coordination directory (default: Git common directory)")
    sub = p.add_subparsers(dest="command", required=True)
    for name in ("validate", "sync", "status"):
        sub.add_parser(name)
    for name in ("list", "next"):
        q = sub.add_parser(name)
        q.add_argument("--group")
        q.add_argument("--status", choices=sorted(STATUSES)) if name == "list" else None
        if name == "list":
            q.add_argument("--full", action="store_true")
    for name in ("show", "history"):
        sub.add_parser(name).add_argument("id")
    q = sub.add_parser("claim")
    q.add_argument("id", nargs="?")
    q.add_argument("--group")
    q.add_argument("--owner", required=True)
    q.add_argument("--session", required=True, help="actual chat/run identifier or link")
    q.add_argument("--lease-seconds", type=int, default=3600)
    for name in ("heartbeat", "check", "uncheck", "pause", "block", "review", "finish"):
        q = sub.add_parser(name)
        q.add_argument("id")
        q.add_argument("--token", required=True)
        if name in {"check", "uncheck"}:
            q.add_argument("criterion")
        if name == "heartbeat":
            q.add_argument("--lease-seconds", type=int, default=3600)
        else:
            q.add_argument("--note", required=True)
        if name in {"check", "finish"}:
            q.add_argument("--evidence", action="append", required=True)
        if name == "block":
            q.add_argument("--reason", required=True)
    for name in ("recover", "unblock", "activate", "defer", "cancel"):
        q = sub.add_parser(name)
        q.add_argument("id")
        q.add_argument("--note", required=True)
        if name in {"recover", "unblock"}:
            q.add_argument("--evidence", action="append", required=True)
    q = sub.add_parser("add")
    q.add_argument("--file", type=Path, required=True)
    q = sub.add_parser("snapshot")
    q.add_argument("--output", type=Path, default=ROOT / "docs/pending-tasks/progress.json")
    q = sub.add_parser("restore")
    q.add_argument("--file", type=Path, required=True)
    return p


def run(args):
    catalog = read_catalog(args.catalog)
    root = args.catalog.resolve().parent.parent.parent
    if args.command == "validate":
        checkpoint = args.catalog.parent / "progress.json"
        if checkpoint.exists() and json.loads(checkpoint.read_text()).get("catalog_hash") != digest(catalog):
            fail("portable checkpoint does not match catalog; run snapshot from the current shared store before committing")
        return {"ok": True, "tasks": len(catalog["tasks"]), "groups": len(catalog["groups"]),
                "revision": catalog["revision"], "catalog_hash": digest(catalog)}
    common = Path(git("rev-parse", "--path-format=absolute", "--git-common-dir", root=root))
    directory = (args.state_dir or common / "luna-pending-tasks").resolve()
    directory.mkdir(parents=True, exist_ok=True)
    with contextlib.closing(sqlite3.connect(directory / "state.sqlite3", timeout=10, isolation_level=None)) as db:
        db.execute("PRAGMA journal_mode=WAL")
        db.execute("PRAGMA synchronous=FULL")
        db.execute("BEGIN IMMEDIATE")
        try:
            db.execute("CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT NOT NULL)")
            db.execute("CREATE TABLE IF NOT EXISTS tasks(id TEXT PRIMARY KEY, data TEXT NOT NULL)")
            db.execute("CREATE TABLE IF NOT EXISTS events(seq INTEGER PRIMARY KEY, task TEXT, at REAL, action TEXT, detail TEXT)")
            schema = db.execute("SELECT value FROM meta WHERE key='schema'").fetchone()
            if schema and schema[0] != "1":
                fail("unsupported coordination store schema; preserve it and upgrade the tool")
            db.execute("INSERT OR IGNORE INTO meta VALUES ('schema', '1')")
            if not db.execute("SELECT 1 FROM tasks LIMIT 1").fetchone():
                reconcile(db, catalog)
                checkpoint = args.catalog.parent / "progress.json"
                if checkpoint.exists() and args.command != "restore":
                    restore(db, json.loads(checkpoint.read_text()), catalog)
            stored_hash = db.execute("SELECT value FROM meta WHERE key='catalog_hash'").fetchone()[0]
            stale = stored_hash != digest(catalog)
            reads = {"status", "list", "next", "show", "history", "snapshot"}
            if stale and args.command not in reads | {"sync"}:
                fail("checkout catalog differs from shared state; update from main, then run sync")
            if args.command == "sync":
                reconcile(db, catalog)
                stale = False
            if args.command == "add":
                spec = json.loads(args.file.read_text())
                if spec.get("status") not in {"ready", "deferred", "blocked"}:
                    fail("new tasks start ready, deferred or blocked")
                catalog["tasks"].append(spec)
                catalog["revision"] += 1
                validate(catalog, root)
                reconcile(db, catalog)
                atomic_json(args.catalog, catalog)
            if args.command == "restore":
                if db.execute("SELECT 1 FROM events WHERE action != 'catalog_add' LIMIT 1").fetchone():
                    fail("restore only into a fresh store; never overwrite live work")
                restore(db, json.loads(args.file.read_text()), catalog)
            all_tasks = rows(db)
            groups = json.loads(db.execute("SELECT value FROM meta WHERE key='groups'").fetchone()[0])
            if getattr(args, "group", None) and args.group not in {g["id"] for g in groups}:
                fail("unknown group: " + args.group)
            task = None
            if getattr(args, "id", None):
                names = {n: t["id"] for t in all_tasks.values() for n in [t["id"], *t["spec"].get("aliases", [])]}
                if args.id not in names:
                    fail("unknown ticket: " + args.id)
                task = all_tasks[names[args.id]]
            command = args.command
            if command == "claim":
                if not args.owner.strip() or not args.session.strip() or not 1 <= args.lease_seconds <= 86400:
                    fail("claim needs owner/session and a lease of 1..86400 seconds")
                if any(t["lease"] and t["lease"]["session"] == args.session for t in all_tasks.values()):
                    fail("this session already owns a ticket; show/checkpoint/pause it before claiming another")
                choices = select(all_tasks, args.group)
                task = task or (choices[0] if choices else None)
                if not task:
                    fail("no eligible task; inspect status/list for blockers, owners or deferred decisions")
                reasons = eligibility(task, all_tasks)
                if reasons:
                    fail("ticket cannot be claimed: " + "; ".join(reasons))
                now = time.time()
                task["status"] = "in_progress"
                task["attempts"] += 1
                task["lease"] = {"token": secrets.token_hex(24), "owner": args.owner, "session": args.session,
                                 "checkout": str(root), "claimed_at": now, "heartbeat_at": now,
                                 "expires_at": now + args.lease_seconds}
            elif command in {"heartbeat", "check", "uncheck", "pause", "block", "review", "finish"}:
                authorize(task, args.token)
                if command == "heartbeat":
                    if not 1 <= args.lease_seconds <= 86400:
                        fail("lease must be 1..86400 seconds")
                    task["lease"].update(heartbeat_at=time.time(), expires_at=time.time() + args.lease_seconds)
                else:
                    task["note"] = note(args.note)
                if command in {"check", "uncheck"}:
                    if args.criterion not in {c["id"] for c in task["spec"]["criteria"]}:
                        fail("unknown acceptance criterion")
                    if command == "check":
                        task["checks"][args.criterion] = {"at": time.time(), "note": task["note"],
                                                       "evidence": evidence(args.evidence, root)}
                    else:
                        task["checks"].pop(args.criterion, None)
                elif command in {"pause", "block"}:
                    task["status"] = "paused" if command == "pause" else "blocked"
                    task["blocker"] = note(args.reason) if command == "block" else None
                    release(task)
                elif command == "review":
                    task["status"] = "review"
                elif command == "finish":
                    if set(task["checks"]) != {c["id"] for c in task["spec"]["criteria"]}:
                        fail("cannot finish: acceptance criteria remain unchecked")
                    task["completion_evidence"] = evidence(args.evidence, root)
                    task["status"] = "done"
                    task["blocker"] = None
                    release(task)
            elif command in {"recover", "unblock", "activate", "defer", "cancel"}:
                task["note"] = note(args.note)
                if command == "recover":
                    if not task["lease"] or time.time() < task["lease"]["expires_at"]:
                        fail("only an expired claim can be recovered; an active worker must pause itself")
                    task["recovery_evidence"] = evidence(args.evidence, root)
                    task["status"] = "paused"
                    release(task)
                elif command == "unblock":
                    if task["status"] != "blocked":
                        fail("ticket is not blocked")
                    task["unblock_evidence"] = evidence(args.evidence, root)
                    task["status"] = "paused" if task["attempts"] else "ready"
                    task["blocker"] = None
                else:
                    allowed = {"deferred"} if command == "activate" else {"ready", "paused", "blocked", "deferred"}
                    if task["status"] not in allowed:
                        fail("cannot change an owned or completed ticket through this command")
                    task["status"] = {"activate": "ready", "defer": "deferred", "cancel": "cancelled"}[command]
                    task["blocker"] = None
            mutations = {"claim", "heartbeat", "check", "uncheck", "pause", "block", "review", "finish",
                         "recover", "unblock", "activate", "defer", "cancel"}
            if command in mutations:
                task["updated_at"] = time.time()
                save(db, task)
                event(db, task["id"], command, {"note": task["note"], "status": task["status"],
                      "worker": public(task, all_tasks)["lease"] or task["last_worker"],
                      "criterion": getattr(args, "criterion", None), "checks": task["checks"],
                      "blocker": task["blocker"], "evidence": task.get("completion_evidence") or
                      task.get("recovery_evidence") or task.get("unblock_evidence")})
                all_tasks[task["id"]] = task
            if command == "snapshot":
                if stale:
                    fail("sync the current catalog before exporting a portable snapshot")
                if args.output.resolve() == args.catalog.resolve() or args.output.resolve().is_relative_to(directory):
                    fail("snapshot output cannot replace the catalog or live coordination store")
                snapshot = {"schema_version": 1, "catalog_hash": digest(catalog),
                            "created_at": time.time(), "summary": summary(all_tasks, groups),
                            "tasks": [{k: v for k, v in t.items() if k not in {"spec", "lease"}}
                                      | {"status": "paused" if t["status"] in LIVE else t["status"],
                                         "last_worker": public(t, all_tasks)["lease"] or t["last_worker"]}
                                      for t in all_tasks.values()],
                            "events": [dict(seq=s, task=t, at=a, action=c, detail=json.loads(d))
                                       for s, t, a, c, d in db.execute("SELECT * FROM events ORDER BY seq")]}
                atomic_json(args.output, snapshot)
                markdown_report(all_tasks, groups, args.output.with_suffix(".md"), root, directory)
                result = {"snapshot": str(args.output), "report": str(args.output.with_suffix(".md")),
                          "live_claims_exported_as": "paused; no claim tokens"}
            elif command == "history":
                result = [dict(seq=s, at=a, action=c, detail=json.loads(d)) for s, a, c, d in
                          db.execute("SELECT seq, at, action, detail FROM events WHERE task=? ORDER BY seq", (task["id"],))]
            elif command == "show" or command in mutations:
                result = public(task, all_tasks)
                if command == "claim":
                    result["claim_token"] = task["lease"]["token"]
            elif command == "list":
                result = [public(t, all_tasks) for t in all_tasks.values()
                          if (not args.group or t["spec"]["group"] == args.group)
                          and (not args.status or t["status"] == args.status)]
                if not args.full:
                    result = [{"id": t["id"], "title": t["spec"]["title"], "group": t["spec"]["group"],
                               "priority": t["spec"]["priority"], "status": t["status"],
                               "progress_percent": t["progress_percent"], "lease": t["lease"],
                               "ineligible_reasons": t["ineligible_reasons"]} for t in result]
            elif command == "next":
                choices = select(all_tasks, args.group)
                result = {"reserved": False, "candidate": public(choices[0], all_tasks) if choices else None}
            else:
                result = summary(all_tasks, groups)
            db.execute("COMMIT")
        except Exception:
            db.execute("ROLLBACK")
            raise
    return {"ok": True, "state_dir": str(directory), "catalog_stale": stale, "result": result}


def restore(db, snapshot, catalog):
    if snapshot.get("schema_version") != 1 or snapshot.get("catalog_hash") != digest(catalog):
        fail("snapshot schema/catalog mismatch; recover against its matching committed catalog first")
    current = rows(db)
    entries = snapshot.get("tasks", [])
    if len(entries) != len(current) or {t.get("id") for t in entries} != set(current):
        fail("snapshot must contain every task exactly once")
    for entry in entries:
        ident = entry["id"]
        task = current[ident]
        if entry.get("status") not in STATUSES or not isinstance(entry.get("checks"), dict):
            fail("invalid snapshot task state")
        if not set(entry["checks"]) <= {c["id"] for c in task["spec"]["criteria"]}:
            fail("snapshot has unknown acceptance criteria")
        if any(not isinstance(check, dict) or not check.get("evidence") or not check.get("note") for check in entry["checks"].values()):
            fail("snapshot has unevidenced accepted criteria")
        if entry["status"] == "done" and set(entry["checks"]) != {c["id"] for c in task["spec"]["criteria"]}:
            fail("snapshot completed task has unchecked criteria")
        if entry["status"] == "done" and task["spec"].get("kind") != "reference" and not entry.get("completion_evidence"):
            fail("snapshot completed task lacks delivery evidence")
        if entry["status"] == "blocked" and not entry.get("blocker"):
            fail("snapshot blocked task lacks a reason")
        task.update({k: entry.get(k) for k in ("checks", "attempts", "updated_at", "note", "blocker", "last_worker")})
        if type(task["attempts"]) is not int or task["attempts"] < 0 or not task["note"]:
            fail("invalid snapshot handoff")
        task["status"] = "paused" if entry["status"] in LIVE else entry["status"]
        task["lease"] = None
        for key in ("completion_evidence", "recovery_evidence", "unblock_evidence"):
            if key in entry:
                task[key] = entry[key]
        save(db, task)
    db.execute("DELETE FROM events WHERE action='catalog_add'")
    for item in snapshot.get("events", []):
        if (item.get("task") is not None and item["task"] not in current) or not isinstance(item.get("detail"), dict):
            fail("invalid snapshot history")
        action = item["action"] if item["action"].startswith("imported:") else "imported:" + item["action"]
        db.execute("INSERT INTO events(task, at, action, detail) VALUES (?, ?, ?, ?)",
                   (item["task"], item["at"], action, encode(item["detail"])))
    event(db, None, "snapshot_restore", {"source_created_at": snapshot.get("created_at"), "live_authority_restored": False})


if __name__ == "__main__":
    try:
        print(json.dumps(run(parser().parse_args()), ensure_ascii=False, indent=2))
    except (ValueError, KeyError, TypeError, AttributeError, OSError, sqlite3.Error, subprocess.SubprocessError) as error:
        print(json.dumps({"ok": False, "error": str(error)}))
        sys.exit(1)
