#!/usr/bin/env python3
"""CORE-01 independent contract reader (C5-C9).

A second implementation, outside Rust, that reads only the published contract
(`crates/rpc/contract/v1`) and frames recorded from a real engine by
`scripts/qa-contract-wire.mjs`:

  C5  every recorded request/reply frame, receipt, command read and envelope
      validates against the generated JSON Schemas;
  C9  each reply is classified success/error/stream from the frame schema
      alone and matches what the session expected;
  C7  the reordered-key retry returned the original receipt;
  C8  the shared id table agrees with the published id schemas;
  C6  (with a second capture) every reply equals the earlier capture's,
      except volatile ids/clocks, and receipt payload hashes are identical.

    agent-python scripts/qa-contract-reader.py <contract-dir> <ids.json> <before/frames.json> [<after/frames.json>] <report.json>
"""

import json
import sys
from pathlib import Path

from jsonschema import Draft202012Validator

EXPECTED_ERRORS = {"unknown-method", "bad-params"}
VOLATILE = {"acceptedAtMs", "committedAtMs", "observedAtMs", "createdAt", "updatedAt", "deviceId",
            "profileId", "id", "fetchedAtMs", "lastMessageAt", "path", "cwd", "roomGen",
            "executionEnvironment"}
PATH_FIELDS = {"cwd", "path"}
# Optional intent fields the engine serializes as null when omitted.
NORMALIZED_OPTIONALS = {"rename": {}, "setPinned": {"afterTaskId": None, "beforeTaskId": None}}


def load(path):
    return json.loads(Path(path).read_text())


def validator(contract, name):
    schema = load(contract / "schemas" / name)
    Draft202012Validator.check_schema(schema)
    return Draft202012Validator(schema)


def errors(v, value):
    return [f"{'/'.join(map(str, e.absolute_path))}: {e.message}" for e in v.iter_errors(value)]


def canonical_hash(value):
    """Independent re-implementation of the engine's command payload hash:
    keys sorted recursively, compact JSON, SHA-256 hex."""
    import hashlib
    text = json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)
    return hashlib.sha256(text.encode()).hexdigest()


def check_hashes(frames, problems, tag):
    """C6b: path-free intents hash to exactly the engine's receipt payloadHash."""
    checked = 0
    for f in frames:
        request, reply = f["request"], f["reply"]
        ok = reply.get("ok")
        if request["method"] != "ExecuteTaskCommand" or not isinstance(ok, dict) or "payloadHash" not in ok:
            continue
        intent = request["params"]["intent"]
        if intent.get("kind") not in NORMALIZED_OPTIONALS or f["label"] == "rename-id-conflict":
            continue
        # The hash covers the engine-normalized intent: optional fields the
        # caller omitted are present as null.
        intent = {**NORMALIZED_OPTIONALS[intent["kind"]], **intent}
        checked += 1
        if canonical_hash(intent) != ok["payloadHash"]:
            problems.append(f"{tag} C6b {f['label']}: independent hash {canonical_hash(intent)[:12]} != engine {ok['payloadHash'][:12]}")
    return checked


def classify(frame_validator, frame):
    """Which oneOf branch of the frame schema the reply satisfies."""
    schema = frame_validator.schema
    hits = [b["required"][0] for b in schema["oneOf"]
            if Draft202012Validator({**{k: v for k, v in schema.items() if k != "oneOf"}, **b}).is_valid(frame)]
    return hits[0] if len(hits) == 1 else f"ambiguous:{hits}"


def normalize(value):
    if isinstance(value, dict):
        return {k: ("<volatile>" if k in VOLATILE else normalize(v)) for k, v in value.items()}
    if isinstance(value, list):
        return [normalize(v) for v in value]
    return value


def check_capture(contract, frames, problems, tag):
    server = validator(contract, "server-frame.json")
    client = validator(contract, "client-frame.json")
    receipt = validator(contract, "command-receipt.json")
    read = validator(contract, "task-command-read.json")
    envelope = validator(contract, "task-command-envelope.json")
    classes = {}
    for f in frames:
        label, request, reply = f["label"], f["request"], f["reply"]
        for e in errors(client, request):
            problems.append(f"{tag} {label} request: {e}")
        for e in errors(server, reply):
            problems.append(f"{tag} {label} reply: {e}")
        kind = classify(server, reply)
        classes[label] = kind
        expected = "err" if label in EXPECTED_ERRORS else ("item" if request["method"].startswith("Watch") else "ok")
        if kind != expected:
            problems.append(f"{tag} {label}: classified {kind}, expected {expected}")
        if request["method"] == "ExecuteTaskCommand" and label not in EXPECTED_ERRORS:
            for e in errors(envelope, request["params"]):
                problems.append(f"{tag} {label} envelope: {e}")
        if kind == "ok" and request["method"] in ("ExecuteTaskCommand", "GetTaskCommandReceipt"):
            for e in errors(receipt, reply["ok"]):
                problems.append(f"{tag} {label} receipt: {e}")
        if kind == "ok" and request["method"] == "GetTaskCommandSnapshot":
            for e in errors(read, reply["ok"]):
                problems.append(f"{tag} {label} read: {e}")
    by_label = {f["label"]: f["reply"] for f in frames}
    if by_label.get("rename-reordered-retry") != by_label.get("rename") and \
            normalize(by_label.get("rename-reordered-retry")) != normalize(by_label.get("rename")):
        problems.append(f"{tag} C7: reordered retry receipt differs from the original")
    original = (by_label.get("rename") or {}).get("ok", {})
    retry = (by_label.get("rename-reordered-retry") or {}).get("ok", {})
    if original.get("payloadHash") != retry.get("payloadHash") or original.get("commandId") != retry.get("commandId"):
        problems.append(f"{tag} C7: reordered retry changed command identity")
    return classes


def check_ids(contract, table, problems):
    schemas = {"namespacedId": validator(contract, "namespaced-id.json"),
               "providerRef": validator(contract, "provider-ref.json")}
    for row in table:
        for kind, accepted in row["accepts"].items():
            ok = schemas[kind].is_valid(row["value"])
            if ok != accepted:
                problems.append(f"C8 {kind} {row['value']!r}: schema says {ok}, table says {accepted}")


def main(argv):
    contract, ids, before = Path(argv[0]), load(argv[1]), load(argv[2])
    after = load(argv[3]) if len(argv) == 5 else None
    report_path = argv[-1]
    problems = []
    report = {"before": check_capture(contract, before, problems, "before"),
              "independentHashes": {"before": check_hashes(before, problems, "before")}}
    check_ids(contract, ids, problems)
    if after is not None:
        report["after"] = check_capture(contract, after, problems, "after")
        report["independentHashes"]["after"] = check_hashes(after, problems, "after")
        old = {f["label"]: f["reply"] for f in before}
        new = {f["label"]: f["reply"] for f in after}
        if old.keys() != new.keys():
            problems.append(f"C6: labels differ {sorted(old.keys() ^ new.keys())}")
        requests_old = {f["label"]: f["request"].get("params", {}).get("intent") for f in before}
        requests_new = {f["label"]: f["request"].get("params", {}).get("intent") for f in after}
        path_dependent = []
        for label in sorted(old.keys() & new.keys()):
            ho = (old[label].get("ok") or {}).get("payloadHash") if isinstance(old[label].get("ok"), dict) else None
            hn = (new[label].get("ok") or {}).get("payloadHash") if isinstance(new[label].get("ok"), dict) else None
            same_intent = requests_old.get(label) == requests_new.get(label)
            only_paths = normalize(requests_old.get(label)) == normalize(requests_new.get(label))
            if ho != hn and not same_intent and only_paths:
                path_dependent.append(label)
            elif ho != hn:
                problems.append(f"C6 {label}: payloadHash {ho} -> {hn} for {'the same' if same_intent else 'a different'} intent")
            strip = (lambda r: {**r, "ok": {k: v for k, v in r["ok"].items() if k != "payloadHash"}}) \
                if label in path_dependent else (lambda r: r)
            if normalize(strip(old[label])) != normalize(strip(new[label])):
                problems.append(f"C6 {label}: reply changed")
        report["pathDependentHashes"] = path_dependent
    report.update({"idRows": len(ids), "frames": len(before), "problems": problems, "passed": not problems})
    Path(report_path).write_text(json.dumps(report, indent=1) + "\n")
    print(json.dumps({"passed": not problems, "problems": len(problems)}))
    for p in problems[:20]:
        print(" ", p)
    return 0 if not problems else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
