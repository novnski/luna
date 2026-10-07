#!/usr/bin/env python3
"""Reconcile LUNA's OpenCode usage history with `opencode stats` (OCE-13, S6).

  python3 scripts/qa-opencode-usage-reconcile.py --db <profile>/data/opencode/opencode.db \
      --luna <go-usage-LABEL.json | usage_opencode_readback output> \
      --stats <opencode stats --models --cost --days N --json output> [--out report.json]

Both readers select different OpenCode 2 `session_message` rows, by design:

- `opencode stats` per model sums `type = 'assistant'` rows only, joined to
  `session_v2`, minus fork-copied rows, inside its `range`. It adds compaction
  usage to its totals only from persisted `session.usage.recorded` events,
  which OpenCode 2.0.22 writes to the `event` table only when its server runs
  with event persistence; a default install keeps that table empty, so the
  compaction requests are missing from `opencode stats` entirely.
- LUNA (crates/engine/src/usage_opencode_history.rs) also counts the
  `type = 'compaction'` rows (OCH-16): each completed or failed compaction is
  a real model request with its own model, tokens and cost, which OpenCode's
  own session totals include and which LUNA counts for Codex too (Codex
  records its compaction request as a `token_count` event). LUNA's output
  includes reasoning (OCH-07) and drops rows without tokens.
- Title generation is recorded only in OpenCode's session totals
  (`session_v2.cost`/`tokens_*`), with no per-request row; neither reader
  counts it.

The script checks each reader against the database over its own window, then
shows the per-model difference split into compaction rows and window skew.
Exit 0 when both readers match the database exactly (cost: only for models
whose every row reports a positive provider cost; LUNA estimates the others
from its price table). Exit 1 on a mismatch, 64 on bad input.

Failure cases this script guards (QR-*):
- QR-01 It writes to a live provider store. The database opens read-only;
  immutable only when no -wal/-shm/-journal sidecar exists (LUNA's rule), so
  SQLite creates no side file and never waits on a checkpoint.
- QR-02 Message text reaches the report. Only usage fields are selected
  through json_extract, behind json_valid; no ids or text are printed.
- QR-03 Variants split a model. `opencode stats` keys provider/model#variant;
  both sides are summed by model id, LUNA's bucket key.
- QR-04 Different windows are compared. `opencode stats --days 0` starts at
  local midnight; LUNA's `days` request is a rolling window ending at its
  read time. Each reader is checked over its own window, and rows written
  after LUNA's read are outside LUNA's window.
- QR-05 Output definitions differ. LUNA output = output + reasoning.
- QR-06 Step counts differ. `opencode stats` counts every assistant row;
  LUNA records only rows with tokens.
- QR-07 Estimated cost is compared as provider cost (see above).
- QR-08 A naive database sum is mistaken for either reader: it is reported
  separately, without the fork-copy exclusion both readers apply.
- QR-09 A failed compaction without a model is dropped: it takes the
  session's latest earlier assistant model, as LUNA does.
- QR-10 An unknown input shape is silently read as empty: refused with 64.
- QR-11 Legacy v1 rows or pre-migration files are present but not modelled:
  reported as a warning, never as a pass.
"""

from __future__ import annotations

import argparse
import json
import math
import os
import sqlite3
import sys
from pathlib import Path
from urllib.parse import quote

FIELDS = ("input", "output", "reasoning", "cacheRead", "cacheWrite")
COST_TOLERANCE = 1e-7


def die(message: str) -> None:
    print(f"qa-opencode-usage-reconcile: {message}", file=sys.stderr)
    sys.exit(64)


def open_read_only(path: Path) -> sqlite3.Connection:
    if not path.is_file():
        die(f"no database at {path}")
    sidecars = any(Path(f"{path}{suffix}").exists() for suffix in ("-wal", "-shm", "-journal"))
    uri = f"file:{quote(str(path))}?mode=ro" + ("" if sidecars else "&immutable=1")
    db = sqlite3.connect(uri, uri=True, timeout=0.1)
    db.execute("PRAGMA query_only = ON")
    return db


def tokens(value) -> int:
    if not isinstance(value, (int, float)) or not math.isfinite(value) or value <= 0:
        return 0
    return int(value)


def empty() -> dict:
    return {"rows": 0, "rowsWithTokens": 0, "zeroCostRows": 0, "cost": 0.0, **{key: 0 for key in FIELDS}}


def add(total: dict, row: dict) -> None:
    total["rows"] += 1
    values = {key: tokens(row[key]) for key in FIELDS}
    if sum(values.values()) > 0:
        total["rowsWithTokens"] += 1
    for key, value in values.items():
        total[key] += value
    cost = row["cost"]
    if isinstance(cost, (int, float)) and math.isfinite(cost) and cost > 0:
        total["cost"] += cost
    else:
        total["zeroCostRows"] += 1


SELECT_USAGE = """
  json_extract(m.data, '$.tokens.input') AS input,
  json_extract(m.data, '$.tokens.output') AS output,
  json_extract(m.data, '$.tokens.reasoning') AS reasoning,
  json_extract(m.data, '$.tokens.cache.read') AS cacheRead,
  json_extract(m.data, '$.tokens.cache.write') AS cacheWrite,
  json_extract(m.data, '$.cost') AS cost
"""


def stats_selection(db: sqlite3.Connection, start: int, end: int) -> dict:
    """`opencode stats` per-model rows (OpenCode 2.0.22 SessionStats.get)."""
    result: dict[str, dict] = {}
    for row in db.execute(
        f"""SELECT json_extract(m.data, '$.model.id') AS model, {SELECT_USAGE}
            FROM session_message m JOIN session_v2 s ON s.id = m.session_id
            WHERE m.type = 'assistant' AND json_valid(m.data)
              AND m.time_created >= ? AND m.time_created < ?
              AND (s.fork_session_id IS NULL OR m.time_created >= s.time_created)""",
        (start, end),
    ):
        row = dict(row)
        if row["model"]:
            add(result.setdefault(row["model"], empty()), row)
    return result


def naive_selection(db: sqlite3.Connection, start: int, end: int) -> dict:
    """Assistant rows without the fork-copy exclusion (QR-08)."""
    result: dict[str, dict] = {}
    for row in db.execute(
        f"""SELECT json_extract(m.data, '$.model.id') AS model, {SELECT_USAGE}
            FROM session_message m
            WHERE m.type = 'assistant' AND json_valid(m.data)
              AND m.time_created >= ? AND m.time_created < ?""",
        (start, end),
    ):
        row = dict(row)
        if row["model"]:
            add(result.setdefault(row["model"], empty()), row)
    return result


def luna_selection(db: sqlite3.Connection, start: int, end: int) -> tuple[dict, dict]:
    """LUNA's v2 rows (usage_opencode_history.rs read_database + usage_record),
    split into assistant and compaction rows."""
    assistant: dict[str, dict] = {}
    compaction: dict[str, dict] = {}
    for row in db.execute(
        f"""SELECT m.type AS type,
              coalesce(nullif(json_extract(m.data, '$.model.id'), ''),
                       nullif(json_extract(m.data, '$.model.modelID'), ''),
                       nullif(json_extract(m.data, '$.modelID'), ''),
                       CASE WHEN m.type = 'compaction' THEN
                         (SELECT json_extract(a.data, '$.model.id') FROM session_message a
                          WHERE a.session_id = m.session_id AND a.type = 'assistant'
                            AND a.time_created <= m.time_created AND json_valid(a.data)
                          ORDER BY a.time_created DESC LIMIT 1) END) AS model,
              json_extract(m.data, '$.role') AS role,
              coalesce(json_extract(m.data, '$.time.created'), m.time_created) AS at,
              {SELECT_USAGE}
            FROM session_message m LEFT JOIN session_v2 s ON s.id = m.session_id
            WHERE m.type IN ('assistant', 'compaction') AND json_valid(m.data)
              AND (s.fork_session_id IS NULL OR m.time_created >= s.time_created)"""
    ):
        row = dict(row)
        if not row["model"] or (row["role"] is not None and row["role"] != "assistant"):
            continue
        if not isinstance(row["at"], (int, float)) or not start <= row["at"] < end:
            continue
        if sum(tokens(row[key]) for key in FIELDS) == 0:
            continue
        target = compaction if row["type"] == "compaction" else assistant
        add(target.setdefault(row["model"], empty()), row)
    return assistant, compaction


def luna_input(path: Path) -> tuple[dict, int, int]:
    """Per-model LUNA totals and its window from qa-opencode-usage.mjs or the
    usage_opencode_readback example (QR-10)."""
    data = json.loads(path.read_text())
    if "opencodeTotalsByModel" in data:
        models = data["opencodeTotalsByModel"]
        read_at, days = data.get("readAtMs"), data.get("days")
        periods, until = data.get("periodStartsMs") or [], data.get("untilMs")
    elif isinstance(data.get("first"), dict) and "models" in data["first"]:
        first = data["first"]
        models = first["models"]
        read_at, days = first.get("readAtMs"), first.get("days")
        periods, until = first.get("periodStartsMs") or [], first.get("untilMs")
    else:
        die(f"{path}: neither a qa-opencode-usage.mjs extract nor usage_opencode_readback output")
    if not isinstance(read_at, int) or days not in (1, 7, 30, 90):
        die(f"{path}: missing readAtMs or a supported days value")
    start = periods[0] if periods else read_at - days * 86_400_000
    end = until if periods and isinstance(until, int) else read_at + 1
    normalized = {}
    for model, value in models.items():
        normalized[model] = {
            "records": value.get("records", 0),
            "input": value.get("uncachedInput", 0),
            "cacheRead": value.get("cachedInput", 0),
            "cacheWrite": value.get("cacheCreation", 0),
            "outputWithReasoning": value.get("output", 0),
            "reasoning": value.get("reasoning", 0),
            "knownCostUsd": value.get("knownCostUsd", 0.0),
        }
    return normalized, start, end


def stats_input(path: Path) -> tuple[dict, int, int]:
    data = json.loads(path.read_text())
    if not isinstance(data.get("range"), dict) or not isinstance(data.get("models"), list):
        die(f"{path}: not `opencode stats --json` output")
    models: dict[str, dict] = {}
    for entry in data["models"]:
        model = entry.get("model", {}).get("id")
        if not model:
            continue
        total = models.setdefault(model, {"steps": 0, "cost": 0.0, **{key: 0 for key in FIELDS}})
        total["steps"] += entry.get("steps", 0)
        total["cost"] += entry.get("cost", 0.0)
        usage = entry.get("tokens", {})
        total["input"] += usage.get("input", 0)
        total["output"] += usage.get("output", 0)
        total["reasoning"] += usage.get("reasoning", 0)
        total["cacheRead"] += usage.get("cache", {}).get("read", 0)
        total["cacheWrite"] += usage.get("cache", {}).get("write", 0)
    start = data["range"].get("from")
    end = data["range"].get("to")
    if not isinstance(start, int) or not isinstance(end, int):
        die(f"{path}: range.from/to missing")
    return models, start, end


def combine(*parts: dict) -> dict:
    total = empty()
    for part in parts:
        for key in total:
            total[key] += part.get(key, 0)
    return total


def cost_equal(left: float, right: float) -> bool:
    return abs(left - right) <= COST_TOLERANCE


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--db", required=True, type=Path)
    parser.add_argument("--luna", required=True, type=Path)
    parser.add_argument("--stats", required=True, type=Path)
    parser.add_argument("--out", type=Path)
    args = parser.parse_args()

    luna, luna_start, luna_end = luna_input(args.luna)
    stats, stats_start, stats_end = stats_input(args.stats)
    db = open_read_only(args.db)
    db.row_factory = sqlite3.Row
    tables = {row[0] for row in db.execute("SELECT name FROM sqlite_master WHERE type = 'table'")}
    warnings = []
    if "session_message" not in tables or "session_v2" not in tables:
        die(f"{args.db}: no OpenCode 2 session_message/session_v2 tables")
    if "message" in tables:
        warnings.append("v1 `message` table present: LUNA also reads it; not modelled here (QR-11)")
    if (args.db.parent / "storage" / "message").is_dir():
        warnings.append("pre-migration storage/message files present: not modelled here (QR-11)")

    db_stats = stats_selection(db, stats_start, stats_end)
    naive = naive_selection(db, stats_start, stats_end)
    luna_assistant, luna_compaction = luna_selection(db, luna_start, luna_end)
    events = 0
    if "event" in tables:
        events = db.execute(
            "SELECT count(*) FROM event WHERE type LIKE 'session.usage.recorded%' "
            "AND json_valid(data) AND json_extract(data, '$.source') = 'compaction'"
        ).fetchone()[0]

    report = {
        "windows": {
            "stats": {"fromMs": stats_start, "toMs": stats_end},
            "luna": {"fromMs": luna_start, "toMs": luna_end},
        },
        "persistedCompactionUsageEvents": events,
        "warnings": warnings,
        "models": {},
    }
    ok = not warnings
    for model in sorted(set(stats) | set(luna) | set(db_stats) | set(luna_assistant) | set(luna_compaction)):
        reported = stats.get(model, {"steps": 0, "cost": 0.0, **{key: 0 for key in FIELDS}})
        selected = db_stats.get(model, empty())
        stats_ok = all(reported[key] == selected[key] for key in FIELDS) and reported["steps"] == selected["rows"] \
            and cost_equal(reported["cost"], selected["cost"])

        expected = combine(luna_assistant.get(model, empty()), luna_compaction.get(model, empty()))
        observed = luna.get(model, {"records": 0, "input": 0, "cacheRead": 0, "cacheWrite": 0,
                                    "outputWithReasoning": 0, "reasoning": 0, "knownCostUsd": 0.0})
        provider_costed = expected["zeroCostRows"] == 0
        luna_ok = (
            observed["records"] == expected["rowsWithTokens"]
            and observed["input"] == expected["input"]
            and observed["cacheRead"] == expected["cacheRead"]
            and observed["cacheWrite"] == expected["cacheWrite"]
            and observed["outputWithReasoning"] == expected["output"] + expected["reasoning"]
            and observed["reasoning"] == expected["reasoning"]
            and (not provider_costed or cost_equal(observed["knownCostUsd"], expected["cost"]))
        )
        compaction = luna_compaction.get(model, empty())
        skew = {key: luna_assistant.get(model, empty())[key] - selected[key] for key in (*FIELDS, "cost")}
        delta = {
            "input": observed["input"] - reported["input"],
            "output": observed["outputWithReasoning"] - observed["reasoning"] - reported["output"],
            "reasoning": observed["reasoning"] - reported["reasoning"],
            "cacheRead": observed["cacheRead"] - reported["cacheRead"],
            "cacheWrite": observed["cacheWrite"] - reported["cacheWrite"],
            "cost": round(observed["knownCostUsd"] - reported["cost"], 9),
        }
        unexplained = {key: delta[key] - compaction[key] - skew[key] for key in FIELDS}
        unexplained["cost"] = round(delta["cost"] - compaction["cost"] - skew["cost"], 9) if provider_costed else None
        report["models"][model] = {
            "opencodeStats": reported,
            "dbStatsSelection": selected,
            "statsMatchesDb": stats_ok,
            "luna": observed,
            "dbLunaSelection": {"assistant": luna_assistant.get(model, empty()), "compaction": compaction},
            "lunaMatchesDb": luna_ok,
            "costCompared": provider_costed,
            "lunaMinusStats": delta,
            "explainedByCompaction": {key: compaction[key] for key in (*FIELDS, "cost")} | {"rows": compaction["rowsWithTokens"]},
            "explainedByWindowSkew": skew,
            "unexplained": unexplained,
            "naiveDbSumWithoutForkExclusion": naive.get(model, empty()),
        }
        ok = ok and stats_ok and luna_ok

    report["reconciled"] = ok
    if args.out:
        args.out.write_text(json.dumps(report, indent=2) + "\n")
    header = f"{'model':<28} {'reader':<22} {'rows':>5} {'input':>9} {'output':>8} {'reason':>7} {'cacheRd':>9} {'cacheWr':>9} {'cost':>10}"
    print(f"opencode stats window {stats_start}..{stats_end}; LUNA window {luna_start}..{luna_end}")
    print(f"persisted compaction usage events (opencode stats adds these to its totals): {events}")
    print(header)
    for model, entry in report["models"].items():
        s, l = entry["opencodeStats"], entry["luna"]
        c = entry["dbLunaSelection"]["compaction"]
        rows = [
            ("opencode stats", s["steps"], s["input"], s["output"], s["reasoning"], s["cacheRead"], s["cacheWrite"], s["cost"]),
            ("+ compaction rows", c["rowsWithTokens"], c["input"], c["output"], c["reasoning"], c["cacheRead"], c["cacheWrite"], c["cost"]),
            ("LUNA history", l["records"], l["input"], l["outputWithReasoning"] - l["reasoning"], l["reasoning"], l["cacheRead"], l["cacheWrite"], l["knownCostUsd"]),
        ]
        for label, *values in rows:
            *counts, cost = values
            print(f"{model:<28} {label:<22} " + " ".join(f"{v:>{w}}" for v, w in zip(counts, (5, 9, 8, 7, 9, 9))) + f" {cost:>10.6f}")
        verdict = "ok" if entry["statsMatchesDb"] and entry["lunaMatchesDb"] else "MISMATCH"
        print(f"{model:<28} {'unexplained':<22} {entry['unexplained']} stats=db:{entry['statsMatchesDb']} luna=db:{entry['lunaMatchesDb']} cost compared:{entry['costCompared']} -> {verdict}")
    for warning in warnings:
        print(f"warning: {warning}")
    print("RECONCILED" if ok else "NOT RECONCILED")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
