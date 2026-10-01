#!/usr/bin/env python3
"""Deterministic Claude Code 2.1.286 peer for CC-10 history tests.

Frame and transcript shapes come from the installed 2.1.286 binary's SDK
schema and from init-only probes of the real CLI
($B/evidence/cc10-history/probes): stdin user lines carry a client `uuid`
that becomes the transcript uuid and is echoed as `user_message_uuid(s)`;
`rewind_conversation` persists an explicit `last-prompt` leaf;
`rewind_files` answers `{canRewind, filesChanged, insertions, deletions}`;
`--fork-session` without a user message persists nothing; a missing
`--resume-session-at` anchor exits 1 before initialize.

No model, network or real Claude configuration is involved. The config root
is CLAUDE_CONFIG_DIR only when it names a `.fake-claude-config` directory,
otherwise `<cwd>/.fake-claude-config`, so a developer's real
CLAUDE_CONFIG_DIR is never written. Knobs: `<config>/fake-claude-knobs.json`.
Every invocation's argv is appended to `<config>/fake-claude-argv.jsonl`.
"""

import difflib
import hashlib
import json
import os
import re
import sys
import uuid as uuidlib

VALUE_FLAGS = {
    "--resume", "--session-id", "--resume-session-at", "--resume-drops-turn", "--rewind-files",
    "--model", "--effort", "--permission-mode", "--permission-prompt-tool", "--input-format",
    "--output-format", "--thinking-display", "--append-system-prompt", "--settings",
    "--setting-sources", "--mcp-config", "--system-prompt", "--tools",
}
CHAIN_TYPES = {"user", "assistant", "system", "attachment", "progress"}
CHECKPOINT_ENV = "CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING"
VERSION = "2.1.286"


def parse_args(argv):
    flags, index = {}, 0
    while index < len(argv):
        token = argv[index]
        if token.startswith("--") and "=" in token:
            key, value = token.split("=", 1)
            flags[key] = value
        elif token in VALUE_FLAGS:
            flags[token] = argv[index + 1] if index + 1 < len(argv) else ""
            index += 1
        elif token.startswith("--"):
            flags[token] = True
        index += 1
    return flags


FLAGS = parse_args(sys.argv[1:])
CWD = os.getcwd()
_env = os.environ.get("CLAUDE_CONFIG_DIR", "")
CONFIG = _env if ".fake-claude-config" in _env else os.path.join(CWD, ".fake-claude-config")
os.makedirs(CONFIG, exist_ok=True)


def _record_crash(kind, value, trace):
    import traceback
    with open(os.path.join(CONFIG, "fake-claude-error.log"), "a") as handle:
        handle.write("".join(traceback.format_exception(kind, value, trace)))
    sys.__excepthook__(kind, value, trace)


sys.excepthook = _record_crash
with open(os.path.join(CONFIG, "fake-claude-argv.jsonl"), "a") as log:
    log.write(json.dumps({"argv": sys.argv[1:], "cwd": CWD,
                          "checkpointing": os.environ.get(CHECKPOINT_ENV) == "1"}) + "\n")
try:
    with open(os.path.join(CONFIG, "fake-claude-knobs.json")) as handle:
        KNOBS = json.load(handle)
except FileNotFoundError:
    KNOBS = {}

CONTROL_ONLY = FLAGS.get("--setting-sources") == "" and "--strict-mcp-config" in FLAGS
if CONTROL_ONLY and "--dangerously-skip-permissions" in FLAGS:
    sys.stderr.write("fake: history control process must never bypass permissions\n")
    sys.exit(3)


def emit(frame):
    sys.stdout.write(json.dumps(frame) + "\n")
    sys.stdout.flush()


def now(step=[0]):
    step[0] += 1
    return "2026-10-01T12:%02d:%02d.000Z" % (step[0] // 60 % 60, step[0] % 60)


def project_dir():
    return os.path.join(CONFIG, "projects", re.sub(r"[^a-zA-Z0-9]", "-", CWD))


def find_transcript(session):
    root = os.path.join(CONFIG, "projects")
    if os.path.isdir(root):
        for name in sorted(os.listdir(root)):
            candidate = os.path.join(root, name, session + ".jsonl")
            if os.path.isfile(candidate):
                return candidate
    return None


def read_entries(path):
    with open(path) as handle:
        return [json.loads(line) for line in handle if line.strip()]


def append(path, row):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "a") as handle:
        handle.write(json.dumps(row) + "\n")


def is_chain(entry):
    return isinstance(entry.get("uuid"), str) and entry.get("type") in CHAIN_TYPES \
        and entry.get("isSidechain") is not True


def active_chain(entries):
    by_uuid, last, explicit = {}, None, None
    for entry in entries:
        if is_chain(entry):
            by_uuid.setdefault(entry["uuid"], entry)
            last, explicit = entry["uuid"], None
        elif entry.get("type") == "last-prompt" and entry.get("explicit") is True:
            explicit = ("leaf", entry.get("leafUuid"))
    cursor = explicit[1] if explicit else last
    chain = []
    while cursor:
        entry = by_uuid[cursor]
        chain.append(entry)
        cursor = entry.get("parentUuid")
    return list(reversed(chain))


def is_human(entry):
    if entry.get("type") != "user" or entry.get("isMeta") or "toolUseResult" in entry:
        return False
    content = entry.get("message", {}).get("content")
    return isinstance(content, str) or (isinstance(content, list) and any(
        block.get("type") != "tool_result" for block in content))


def text_of(entry):
    content = entry.get("message", {}).get("content")
    if isinstance(content, str):
        return content
    return "\n".join(block.get("text", "") for block in content or [] if block.get("type") == "text")


def stream_text(text):
    """Root text reaches LUNA only as partial deltas (`--include-partial-messages`)."""
    for event in ({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}},
                  {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": text}},
                  {"type": "content_block_stop", "index": 0}):
        emit({"type": "stream_event", "event": event, "parent_tool_use_id": None,
              "session_id": SESSION, "uuid": str(uuidlib.uuid4())})


def response(request_id, payload):
    emit({"type": "control_response",
          "response": {"subtype": "success", "request_id": request_id, "response": payload}})


def error_response(request_id, message):
    emit({"type": "control_response",
          "response": {"subtype": "error", "request_id": request_id, "error": message}})


# ── session state ──────────────────────────────────────────────────────────

RESUME = FLAGS.get("--resume")
FORK = "--fork-session" in FLAGS
if RESUME:
    SOURCE_PATH = find_transcript(RESUME)
    if SOURCE_PATH is None:
        sys.stderr.write("No conversation found with session ID: %s\n" % RESUME)
        sys.exit(1)
    LOADED = active_chain(read_entries(SOURCE_PATH))
    anchor = FLAGS.get("--resume-session-at")
    if anchor:
        positions = [i for i, entry in enumerate(LOADED) if entry["uuid"] == anchor]
        if KNOBS.get("fork_validation_fail") or not positions:
            emit({"type": "result", "subtype": "error_during_execution", "is_error": True,
                  "errors": [], "session_id": FLAGS.get("--session-id", RESUME)})
            sys.stderr.write("No message found with message.uuid of: %s\n" % anchor)
            sys.exit(1)
        LOADED = LOADED[:positions[0] + 1]
    SESSION = FLAGS.get("--session-id") or str(uuidlib.uuid4()) if FORK else RESUME
    PATH = None if FORK else SOURCE_PATH
    if not FORK:
        append(PATH, {"type": "mode", "mode": "normal", "sessionId": SESSION})
else:
    LOADED = []
    SESSION = FLAGS.get("--session-id") or str(uuidlib.uuid4())
    PATH = os.path.join(project_dir(), SESSION + ".jsonl")
LEAF = LOADED[-1]["uuid"] if LOADED else None
INIT_SENT = False


def base(kind):
    return {"type": kind, "sessionId": SESSION, "cwd": CWD, "version": VERSION, "gitBranch": "",
            "isSidechain": False, "userType": "external", "entrypoint": "sdk-cli",
            "timestamp": now()}


# ── file checkpoints ───────────────────────────────────────────────────────

def history_dir():
    return os.path.join(CONFIG, "file-history", SESSION)


def snapshots(entries):
    by_message = {}
    for entry in entries:
        if entry.get("type") == "file-history-snapshot":
            merged = by_message.setdefault(entry["messageId"], {})
            merged.update(entry["snapshot"].get("trackedFileBackups", {}))
    return by_message


def backup(relative):
    absolute = os.path.join(CWD, relative)
    os.makedirs(history_dir(), exist_ok=True)
    if not os.path.exists(absolute):
        return {"backupFileName": None, "version": 1, "backupTime": now()}
    with open(absolute, "rb") as handle:
        data = handle.read()
    name = hashlib.sha256(relative.encode()).hexdigest()[:16] + "@v" + str(len(os.listdir(history_dir())) + 1)
    with open(os.path.join(history_dir(), name), "wb") as handle:
        handle.write(data)
    return {"backupFileName": name, "version": 1, "backupTime": now()}


def snapshot_turn(user_uuid, edits):
    if os.environ.get(CHECKPOINT_ENV) != "1":
        return
    tracked = set()
    for files in snapshots(read_entries(PATH) if os.path.exists(PATH) else []).values():
        tracked.update(files)
    tracked.update(path for path, _ in edits)
    backups = {path: backup(path) for path in sorted(tracked)}
    append(PATH, {"type": "file-history-snapshot", "messageId": user_uuid,
                  "snapshot": {"messageId": user_uuid, "trackedFileBackups": backups,
                               "timestamp": now()}, "isSnapshotUpdate": False})


def rewind_files(target, dry_run):
    if os.environ.get(CHECKPOINT_ENV) != "1":
        return {"canRewind": False, "error": "File rewinding is not enabled."}
    entries = read_entries(SOURCE_PATH)
    by_message = snapshots(entries)
    if target not in by_message:
        return {"canRewind": False, "error": "No file checkpoint found for this message."}
    order = [entry["messageId"] for entry in entries if entry.get("type") == "file-history-snapshot"]
    wanted = dict(by_message[target])
    for later in order[order.index(target) + 1:]:
        for path, record in by_message[later].items():
            wanted.setdefault(path, record)
    changed, insertions, deletions = [], 0, 0
    for path, record in sorted(wanted.items()):
        absolute = os.path.join(CWD, path)
        current = open(absolute).read() if os.path.exists(absolute) else None
        name = record.get("backupFileName")
        target_text = open(os.path.join(history_dir(), name)).read() if name else None
        if current == target_text:
            continue
        changed.append(absolute)
        for line in difflib.ndiff((current or "").splitlines(), (target_text or "").splitlines()):
            insertions += line.startswith("+ ")
            deletions += line.startswith("- ")
        if not dry_run:
            if target_text is None:
                os.remove(absolute)
            else:
                with open(absolute, "w") as handle:
                    handle.write(target_text)
    return {"canRewind": True, "filesChanged": changed, "insertions": insertions, "deletions": deletions}


# ── conversation rewind (mirrors the CLI's rv/RE helpers) ──────────────────

def rewind_conversation(target, last_seen):
    global LEAF
    chain = active_chain(read_entries(SOURCE_PATH))
    index = next((i for i, entry in enumerate(chain) if entry["uuid"] == target), -1)
    seen = next((i for i, entry in enumerate(chain) if entry["uuid"] == last_seen), -1)
    later = any(i > index and i > seen and is_human(entry) and entry["uuid"] not in (target, last_seen)
                for i, entry in enumerate(chain))
    if index < 0:
        return refusal("target_not_found", "target not found")
    if later:
        return refusal("stale_target" if last_seen is None else "unseen_later_turn",
                       "stale target" if last_seen is None else "unseen later turn")
    anchor, preceding = None, None
    for entry in reversed(chain[:index]):
        if entry.get("type") == "assistant":
            preceding = entry["uuid"]
            anchor = anchor or entry["uuid"]
            break
        if entry.get("type") == "user" and anchor is None:
            anchor = entry["uuid"]
    append(SOURCE_PATH, {"type": "last-prompt", "leafUuid": anchor, "explicit": True,
                         "rewound": True, "sessionId": SESSION})
    LEAF = anchor
    return {"rewound": True, "targetMessageUuid": target, "prefillText": text_of(chain[index]),
            "precedingAssistantUuid": preceding}


def refusal(reason, message):
    return {"rewound": False, "prefillText": None, "precedingAssistantUuid": None,
            "error": message, "reason": reason}


# ── turns ──────────────────────────────────────────────────────────────────

def init_frame():
    global INIT_SENT
    if not INIT_SENT:
        INIT_SENT = True
        emit({"type": "system", "subtype": "init", "session_id": SESSION, "cwd": CWD,
              "model": "claude-haiku-4-5-20251001", "tools": ["Edit", "Write"], "uuid": str(uuidlib.uuid4())})


def run_turn(frame, folded=None):
    global LEAF
    if FORK:
        sys.stderr.write("fake: --fork-session with a model turn is not modeled\n")
        sys.exit(4)
    client = frame.get("uuid")
    user_uuid = client or str(uuidlib.uuid4())
    prompt = text_of({"message": frame.get("message", {})})
    edits = re.findall(r"edit:([\w./-]+)=(\S+)", prompt)
    init_frame()
    snapshot_turn(user_uuid, edits)
    user = dict(base("user"), parentUuid=LEAF, uuid=user_uuid, promptId=str(uuidlib.uuid4()),
                message={"role": "user", "content": frame.get("message", {}).get("content", "")})
    append(PATH, user)
    LEAF = user_uuid
    consumed = [client] if client else []
    for extra in folded or []:
        extra_uuid = extra.get("uuid") or str(uuidlib.uuid4())
        append(PATH, dict(base("user"), parentUuid=LEAF, uuid=extra_uuid, promptId=str(uuidlib.uuid4()),
                          message={"role": "user", "content": extra.get("message", {}).get("content", "")}))
        LEAF = extra_uuid
        if extra.get("uuid"):
            consumed.append(extra["uuid"])
    for path, content in edits:
        with open(os.path.join(CWD, path), "w") as handle:
            handle.write(content + "\n")
    reply = "done: " + prompt[:40]
    assistant_uuid = str(uuidlib.uuid4())
    message = {"id": "msg_" + assistant_uuid[:8], "type": "message", "role": "assistant",
               "model": "claude-haiku-4-5-20251001", "content": [{"type": "text", "text": reply}],
               "stop_reason": "end_turn", "stop_sequence": None,
               "usage": {"input_tokens": 3, "output_tokens": 2}}
    append(PATH, dict(base("assistant"), parentUuid=LEAF, uuid=assistant_uuid, message=message,
                      requestId="req_" + assistant_uuid[:8]))
    LEAF = assistant_uuid
    duration_uuid = str(uuidlib.uuid4())
    append(PATH, dict(base("system"), parentUuid=LEAF, uuid=duration_uuid, subtype="turn_duration",
                      durationMs=5, isMeta=False))
    LEAF = duration_uuid
    append(PATH, {"type": "last-prompt", "lastPrompt": prompt, "leafUuid": LEAF, "sessionId": SESSION})
    stream_text(reply)
    assistant_frame = {"type": "assistant", "message": message, "parent_tool_use_id": None,
                       "session_id": SESSION, "uuid": assistant_uuid}
    if client:
        assistant_frame["user_message_uuid"] = client
    emit(assistant_frame)
    result = {"type": "result", "subtype": "success", "is_error": False, "result": reply,
              "session_id": SESSION, "usage": {"input_tokens": 3, "output_tokens": 2},
              "uuid": str(uuidlib.uuid4())}
    if "scenario:error" in prompt:
        result.update(subtype="error_during_execution", is_error=True, errors=["synthetic failure"])
    if "scenario:no-uuid" not in prompt and consumed:
        result["user_message_uuid"] = consumed[-1]
        result["user_message_uuids"] = consumed
    if "scenario:foreign-uuid" in prompt:
        # A turn answering a send LUNA never made (another client/peer).
        result.pop("user_message_uuid", None)
        result["user_message_uuids"] = [str(uuidlib.uuid4())]
    emit(result)
    if "scenario:dup-result" in prompt:
        emit(result)
    if "scenario:autonomous" in prompt:
        # A native background-task wake: its own native turn, no client uuid.
        wake_uuid = str(uuidlib.uuid4())
        append(PATH, dict(base("user"), parentUuid=LEAF, uuid=wake_uuid, promptId=str(uuidlib.uuid4()),
                          message={"role": "user", "content": "<task-notification>done</task-notification>"}))
        wake_reply = str(uuidlib.uuid4())
        append(PATH, dict(base("assistant"), parentUuid=wake_uuid, uuid=wake_reply,
                          message=dict(message, id="msg_wake", content=[{"type": "text", "text": "noted"}])))
        LEAF = wake_reply
        stream_text("noted")
        emit({"type": "assistant", "message": dict(message, content=[{"type": "text", "text": "noted"}]),
              "parent_tool_use_id": None, "session_id": SESSION, "uuid": wake_reply})
        emit({"type": "result", "subtype": "success", "is_error": False, "result": "noted",
              "session_id": SESSION, "usage": {"input_tokens": 1, "output_tokens": 1}})


def handle_control(frame):
    request_id = frame.get("request_id", "")
    request = frame.get("request", {})
    subtype = request.get("subtype")
    knob = KNOBS.get(subtype, {})
    if subtype == "initialize":
        response(request_id, {"commands": [], "account": {}, "pid": os.getpid(), "models": [
            {"value": "haiku", "resolvedModel": "claude-haiku-4-5-20251001",
             "displayName": "Haiku", "description": "synthetic"}]})
    elif subtype == "rewind_conversation":
        if knob.get("reply"):
            response(request_id, knob["reply"])
        elif knob.get("die_after_request"):
            if knob.get("persist_before_dying"):
                rewind_conversation(request.get("target_message_uuid"),
                                    request.get("last_seen_user_message_uuid"))
            sys.exit(9)
        else:
            response(request_id, rewind_conversation(request.get("target_message_uuid"),
                                                     request.get("last_seen_user_message_uuid")))
    elif subtype == "rewind_files":
        result = rewind_files(request.get("user_message_id"), bool(request.get("dry_run")))
        if knob.get("die_after_request"):
            sys.exit(9)
        if not result.get("canRewind") and not request.get("dry_run"):
            error_response(request_id, "Failed to rewind: " + result.get("error", "Unexpected error"))
        else:
            response(request_id, result)
    elif subtype == "fork_conversation":
        response(request_id, {"forked": False, "error": "unsupported", "reason": "unsupported"})
    elif subtype == "interrupt":
        response(request_id, {})
    else:
        error_response(request_id, "Unsupported control request subtype: %s" % subtype)


def main():
    pending = []
    while True:
        line = pending.pop(0) if pending else sys.stdin.readline()
        if not line:
            return 0
        line = line.strip()
        if not line:
            continue
        frame = json.loads(line)
        if frame.get("type") == "control_request":
            handle_control(frame)
        elif frame.get("type") == "user":
            folded = []
            if "scenario:batch" in text_of({"message": frame.get("message", {})}):
                folded.append(json.loads(sys.stdin.readline()))
            run_turn(frame, folded)


if __name__ == "__main__":
    sys.exit(main())
