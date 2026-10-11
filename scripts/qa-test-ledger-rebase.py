#!/usr/bin/env python3
"""Rebase the 9 October test-audit ledgers onto the current tree (CORE-02, E04).

For every ledger row whose test names a Rust function (`...::name`):

  R/F  the test must still be declared somewhere in crates/ or apps/;
  C/D  the test must still be absent, and every keeper the evidence names
       (backticked `...::name` or `name` that was a test at audit time) must
       still be declared, or the contract it carried has been lost.

Also counts current Rust test declarations. Read-only; writes one JSON report.

    python3 scripts/qa-test-ledger-rebase.py <report.json> [--ledgers DIR]
"""

import argparse
import json
import re
import subprocess
import sys
from collections import Counter, defaultdict
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
ROW = re.compile(r"^\|\s*([RFCD])\s*\|\s*(.+?)\s*\|\s*(.*)\|\s*$")
IDENT = re.compile(r"^[a-z_][a-z0-9_]*$")
TEST_ATTR = re.compile(r"#\[(?:tokio::|gpui::)?test\b")
FN = re.compile(r"\bfn\s+([a-z_][a-z0-9_]*)\s*[<(]")


def declared_tests():
    """Every Rust function declared directly under a test attribute, plus all
    function names (keepers may be helpers or inline tests)."""
    files = subprocess.run(["git", "ls-files", "crates", "apps"], cwd=ROOT,
                           capture_output=True, text=True, check=True).stdout.split()
    tests, functions = Counter(), set()
    for rel in files:
        if not rel.endswith(".rs"):
            continue
        lines = (ROOT / rel).read_text(errors="replace").splitlines()
        pending = False
        depth = 0
        for line in lines:
            stripped = line.strip()
            if depth:
                # Inside a multi-line attribute such as #[cfg_attr(\n ..., ignore = ...)].
                depth += stripped.count("[") - stripped.count("]")
                continue
            if TEST_ATTR.match(stripped):
                pending = True
                continue
            if stripped.startswith("#["):
                depth = max(stripped.count("[") - stripped.count("]"), 0)
                continue
            match = FN.search(stripped)
            if match:
                functions.add(match.group(1))
                if pending:
                    tests[match.group(1)] += 1
                pending = False
            elif pending and stripped and not stripped.startswith(("#", "//")):
                pending = False
    return tests, functions


def test_name(cell):
    text = cell.strip().strip("`").strip()
    if "::" not in text:
        return None
    name = text.rsplit("::", 1)[1].strip().strip("`")
    return name if IDENT.match(name) else None


def keepers(evidence):
    names = set()
    for token in re.findall(r"`([^`]+)`", evidence):
        tail = token.rsplit("::", 1)[-1].strip()
        if IDENT.match(tail) and "_" in tail and len(tail) > 12:
            names.add(tail)
    return names


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("report", type=Path)
    parser.add_argument("--ledgers", type=Path, default=ROOT / "docs/audits/2026-10-09-test-audit")
    args = parser.parse_args()
    tests, functions = declared_tests()
    lanes = {}
    problems = defaultdict(list)
    audited_names = set()
    rows_by_lane = {}
    for ledger in sorted(args.ledgers.glob("L*.md")):
        rows = []
        for line in ledger.read_text().splitlines():
            match = ROW.match(line)
            if match:
                rows.append(match.groups())
        rows_by_lane[ledger.stem] = rows
        for _, cell, _ in rows:
            name = test_name(cell)
            if name:
                audited_names.add(name)
    for lane, rows in rows_by_lane.items():
        counts = Counter()
        for mark, cell, evidence in rows:
            name = test_name(cell)
            counts[mark] += 1
            if not name:
                counts["non-rust-or-unnamed"] += 1
                continue
            counts["rust"] += 1
            present = tests[name] > 0
            if mark in "RF":
                if present:
                    counts["still-present"] += 1
                else:
                    counts["gone-since-audit"] += 1
                    problems["retained-now-absent"].append({"lane": lane, "mark": mark, "test": cell.strip("` ")})
            else:
                if present:
                    problems["removed-but-declared"].append({"lane": lane, "mark": mark, "test": cell.strip("` ")})
                for keeper in sorted(keepers(evidence)):
                    if keeper == name or keeper not in audited_names:
                        continue
                    if keeper not in functions:
                        problems["keeper-missing"].append({"lane": lane, "mark": mark, "test": cell.strip("` "),
                                                           "keeper": keeper})
        lanes[lane] = dict(counts)
    report = {
        "rustTestDeclarations": sum(tests.values()),
        "distinctRustTestNames": len(tests),
        "lanes": lanes,
        "problems": {kind: rows for kind, rows in problems.items()},
        "problemCounts": {kind: len(rows) for kind, rows in problems.items()},
    }
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=1) + "\n")
    print(json.dumps({"rustTestDeclarations": report["rustTestDeclarations"],
                      "problemCounts": report["problemCounts"]}))
    return 0


if __name__ == "__main__":
    sys.exit(main())
