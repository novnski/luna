#!/usr/bin/env python3
"""Deterministic Claude Code 2.1.286 peer for CC-06 (delivery receipts, result
origin, runtime readback/live switches and live accounting).

Frame shapes follow the installed 2.1.286 binary's embedded SDK schema
(SDKUserMessageReplay, SDKResultMessage with user_message_uuids/origin/
total_cost_usd/modelUsage, rate_limit_event.rate_limit_info, system/api_retry)
and two real initialization-only probes (get_settings / apply_flag_settings /
set_model control_response shapes). No model is ever called.

Behaviour is a persistent stream-json session: it answers control requests at
any time, replays each stdin user line with its uuid (``--replay-user-messages``)
when the line is incorporated, and ends on stdin EOF. The FIRST user line picks
a scenario (``cc06:<name>`` in its text). With a ``.cc06-log`` marker in the
cwd (or ``CC06_FAKE_LOG``), one JSON line per spawn argv and stdin line is logged.
"""
import json
import os
import sys
import time
import uuid as uuidlib

ARGV = sys.argv[1:]
# A test opts in by placing `.cc06-log` in the run's cwd (per-test isolation
# without process-wide environment variables).
LOG = os.environ.get("CC06_FAKE_LOG") or (
    os.path.join(os.getcwd(), "cc06-fake.log")
    if os.path.exists(os.path.join(os.getcwd(), ".cc06-log")) else None)
LADDERS = {  # mirrors what initialize advertises below
    "claude-opus-5-5": ["low", "medium", "high", "xhigh", "max"],
    "claude-sonnet-5-5": ["low", "medium", "high", "max"],
    "claude-haiku-4-5-20251001": [],
}


def log(entry):
    if LOG:
        with open(LOG, "a") as handle:
            handle.write(json.dumps(entry) + "\n")


def arg(name):
    for index, value in enumerate(ARGV):
        if value == name and index + 1 < len(ARGV):
            return ARGV[index + 1]
        if value.startswith(name + "="):
            return value.split("=", 1)[1]
    return None


def emit(frame):
    sys.stdout.write(json.dumps(frame) + "\n")
    sys.stdout.flush()


def base_model(model):
    return model[:-4] if model and model.endswith("[1m]") else model


class Session:
    def __init__(self):
        resume = arg("--resume")
        self.session_id = resume or "sess-cc06"
        self.model = arg("--model") or "claude-opus-5-5"
        requested = arg("--effort")
        ladder = LADDERS.get(base_model(self.model))
        # The real CLI applies no effort to a model without a ladder.
        self.effort = None if ladder == [] else requested
        self.fast = False
        self.cost = 0.0
        self.tokens = {"in": 0, "out": 0, "cr": 0, "cc": 0}
        self.results = 0
        self.scenario = ""
        self.settings_override = None  # (model, effort) forced readback
        self.reject_set_model = False

    # -- frames ------------------------------------------------------------
    def init(self):
        frame = {"type": "system", "subtype": "init", "cwd": os.getcwd(),
                 "session_id": self.session_id, "tools": ["Bash", "Read"], "mcp_servers": [],
                 "model": self.model, "permissionMode": "acceptEdits", "slash_commands": [],
                 "apiKeySource": "none", "claude_code_version": "2.1.286",
                 "output_style": "default", "agents": [], "skills": [], "plugins": [],
                 "uuid": str(uuidlib.uuid4()), "fast_mode_state": "off"}
        if self.scenario == "steer-race":
            # 2.1.293 live capture: the init that advertises input receipts.
            frame["claude_code_version"] = "2.1.293"
            frame["capabilities"] = ["interrupt_receipt_v1", "interrupt_cancel_queued_v1",
                                     "interrupt_send_now_v1", "msg_lifecycle_v1",
                                     "request_marker_lists_v1", "ui_surface_v1"]
        emit(frame)

    def lifecycle(self, line, state):
        emit({"type": "command_lifecycle", "command_uuid": line.get("uuid"), "state": state,
              "uuid": str(uuidlib.uuid4()), "session_id": self.session_id})

    def replay(self, line, uuid=None):
        emit({"type": "user", "message": line.get("message"), "session_id": self.session_id,
              "parent_tool_use_id": None, "uuid": uuid or line.get("uuid"),
              "timestamp": "2026-10-01T12:00:00.000Z", "isReplay": True})

    def text(self, text):
        emit({"type": "stream_event", "parent_tool_use_id": None, "session_id": self.session_id,
              "uuid": str(uuidlib.uuid4()),
              "event": {"type": "content_block_delta", "index": 0,
                        "delta": {"type": "text_delta", "text": text}}})
        emit({"type": "assistant", "parent_tool_use_id": None, "session_id": self.session_id,
              "uuid": str(uuidlib.uuid4()),
              "message": {"id": "msg_" + uuidlib.uuid4().hex[:12], "type": "message",
                          "role": "assistant", "model": base_model(self.model),
                          "content": [{"type": "text", "text": text}], "stop_reason": None,
                          "usage": {"input_tokens": 12, "cache_creation_input_tokens": 300,
                                    "cache_read_input_tokens": 4000, "output_tokens": 30}}})

    def rate_limit(self, status="allowed_warning"):
        now = int(time.time())
        emit({"type": "rate_limit_event", "uuid": str(uuidlib.uuid4()),
              "session_id": self.session_id,
              "rate_limit_info": {
                  "status": status, "resetsAt": now + 2 * 3600, "rateLimitType": "five_hour",
                  "utilization": 0.91, "isUsingOverage": False,
                  "unifiedWindows": {
                      "five_hour": {"utilization": 0.91, "resetsAt": now + 2 * 3600},
                      "seven_day": {"utilization": 0.42, "resetsAt": now + 4 * 86400}}}})

    def api_retry(self):
        emit({"type": "system", "subtype": "api_retry", "attempt": 1, "max_retries": 10,
              "retry_delay_ms": 500, "error_status": 529, "error": "overloaded",
              "uuid": str(uuidlib.uuid4()), "session_id": self.session_id})

    def result(self, text, uuids=None, origin="human"):
        # result.usage is the turn's main loop; totals are cumulative.
        turn = {"in": 12, "out": 30, "cr": 4000, "cc": 300}
        for key in turn:
            self.tokens[key] += turn[key]
        self.cost = round(self.cost + 0.0125, 6)
        model = base_model(self.model)
        frame = {
            "type": "result", "subtype": "success", "is_error": False, "duration_ms": 1200,
            "duration_api_ms": 900, "num_turns": 1, "result": text, "stop_reason": "end_turn",
            "session_id": self.session_id, "total_cost_usd": self.cost,
            "usage": {"input_tokens": turn["in"], "cache_creation_input_tokens": turn["cc"],
                      "cache_read_input_tokens": turn["cr"], "output_tokens": turn["out"],
                      "server_tool_use": {"web_search_requests": 0}, "service_tier": "standard"},
            "modelUsage": {model: {
                "inputTokens": self.tokens["in"], "outputTokens": self.tokens["out"],
                "cacheReadInputTokens": self.tokens["cr"],
                "cacheCreationInputTokens": self.tokens["cc"], "webSearchRequests": 0,
                "costUSD": self.cost, "contextWindow": 200000, "maxOutputTokens": 32000}},
            "permission_denials": [], "uuid": str(uuidlib.uuid4()),
            "result_index": self.results, "fast_mode_state": "off",
        }
        if uuids:
            frame["user_message_uuids"] = uuids
        if origin:
            frame["origin"] = {"kind": origin}
        self.results += 1
        emit(frame)

    # -- control -----------------------------------------------------------
    def control(self, request):
        rid = request.get("request_id")
        body = request.get("request", {})
        subtype = body.get("subtype")
        if subtype == "get_settings":
            model, effort = self.settings_override or (self.model, self.effort)
            emit({"type": "control_response", "response": {
                "subtype": "success", "request_id": rid, "response": {
                    "applied": {"model": base_model(model), "effort": effort, "advisor": None,
                                "ultracode": False, "ultracodeRequested": False,
                                "ultracodeAvailable": True},
                    "effective": {"fastMode": self.fast} if self.fast else {},
                    "sources": [{"source": "flagSettings", "settings": {}}]}}})
        elif subtype == "set_model":
            if self.reject_set_model:
                emit({"type": "control_response", "response": {
                    "subtype": "error", "request_id": rid,
                    "error": "Unable to validate model: synthetic rejection",
                    "error_code": "check_failed"}})
                return
            self.model = body.get("model") or "claude-opus-5-5"
            emit({"type": "control_response", "response": {"subtype": "success", "request_id": rid}})
        elif subtype == "apply_flag_settings":
            settings = body.get("settings", {})
            level = settings.get("effortLevel")
            if level in ("low", "medium", "high", "xhigh", "max") and self.scenario != "switch-noapply":
                ladder = LADDERS.get(base_model(self.model))
                self.effort = None if ladder == [] else level
            if "fastMode" in settings:
                self.fast = bool(settings["fastMode"])
            # 2.1.286 acks even values it does not apply (probe: "turbo").
            emit({"type": "control_response", "response": {"subtype": "success", "request_id": rid}})
        elif subtype == "interrupt":
            emit({"type": "control_response", "response": {"subtype": "success", "request_id": rid}})
        else:
            emit({"type": "control_response", "response": {
                "subtype": "error", "request_id": rid,
                "error": "Unknown control request subtype: %s" % subtype}})


def read_line(session):
    """Next stdin user line; control requests are answered on the way."""
    while True:
        raw = sys.stdin.readline()
        if not raw:
            return None
        raw = raw.strip()
        if not raw:
            continue
        log({"event": "stdin", "line": raw})
        frame = json.loads(raw)
        if frame.get("type") == "control_request":
            session.control(frame)
            continue
        if frame.get("type") == "user":
            return frame


def content_text(line):
    content = (line.get("message") or {}).get("content")
    if isinstance(content, str):
        return content
    return "".join(block.get("text", "") for block in content or [] if block.get("type") == "text")


def generic_turn(session, line):
    session.replay(line)
    session.text("echo:%s model=%s effort=%s" % (content_text(line), base_model(session.model),
                                                session.effort))
    session.result("ok", [line.get("uuid")])


def main():
    log({"event": "spawn", "argv": ARGV, "cwd": os.getcwd()})
    first_raw = sys.stdin.readline()
    if not first_raw:
        return
    log({"event": "stdin", "line": first_raw.strip()})
    first = json.loads(first_raw)
    if first.get("type") == "control_request":
        # Discovery probe (`initialize`): answer and exit with the stdin close.
        emit({"type": "control_response", "response": {
            "subtype": "success", "request_id": first.get("request_id"), "response": {
                "commands": [{"name": "review", "description": "Review", "argumentHint": ""}],
                "models": [
                    {"value": "opus", "resolvedModel": "claude-opus-5-5", "displayName": "Opus",
                     "supportsEffort": True, "supportedEffortLevels": LADDERS["claude-opus-5-5"],
                     "supportsFastMode": True},
                    {"value": "sonnet", "resolvedModel": "claude-sonnet-5-5",
                     "displayName": "Sonnet", "supportsEffort": True,
                     "supportedEffortLevels": LADDERS["claude-sonnet-5-5"]},
                    {"value": "haiku", "resolvedModel": "claude-haiku-4-5-20251001",
                     "displayName": "Haiku", "supportsEffort": None,
                     "supportedEffortLevels": None}],
                "output_style": "default", "capabilities": ["ui_surface_v1"]}}})
        while sys.stdin.readline():
            pass
        return

    session = Session()
    text = content_text(first)
    scenario = text.split("cc06:", 1)[1].split()[0] if "cc06:" in text else ""
    session.scenario = scenario
    session.reject_set_model = scenario == "switch-reject"
    session.init()

    if scenario == "delivery":
        session.replay(first)
        session.text("working")
        steer = read_line(session)
        if steer is None:
            return
        # Same text, a uuid LUNA never sent (CLI-originated replay): ignored.
        session.replay(steer, uuid=str(uuidlib.uuid4()))
        session.replay(steer)
        session.replay(steer)  # duplicate acknowledgment (dedupe path)
        session.text("steered:%s" % content_text(steer))
        session.result("done", [first.get("uuid"), steer.get("uuid")])
    elif scenario == "steer-race":
        # 2.1.293 live capture: a steer written while the turn runs is only
        # queued; the turn ends answering the first prompt alone, then the CLI
        # starts the queued line as its own next turn.
        session.lifecycle(first, "queued")
        session.lifecycle(first, "started")
        session.replay(first)
        session.text("working")
        steer = read_line(session)
        if steer is None:
            return
        session.lifecycle(steer, "queued")
        session.text("first answer")
        session.result("first", [first.get("uuid")])
        session.lifecycle(first, "completed")
        session.lifecycle(steer, "started")
        session.init()
        session.replay(steer)
        session.text("steered:%s" % content_text(steer))
        session.result("second", [steer.get("uuid")])
        session.lifecycle(steer, "completed")
    elif scenario == "foreign":
        # A Claude-initiated turn runs before the queued human prompt.
        session.text("background report")
        session.result("bg", None, origin="task-notification")
        session.replay(first)
        session.rate_limit()
        session.text("human answer")
        session.result("answer", [first.get("uuid")])
        # Idle wake: a foreign result with nothing outstanding still ends.
        session.api_retry()
        session.text("idle wake")
        session.result("wake", ["00000000-0000-4000-8000-000000000000"], origin="peer")
    elif scenario in ("readback", "readback-drop"):
        if scenario == "readback-drop":
            session.settings_override = (session.model, None)
        session.replay(first)
        session.text("readback")
        session.result("ok", [first.get("uuid")])
    else:
        generic_turn(session, first)
        if scenario.startswith("switch"):
            session.rate_limit()

    while True:
        line = read_line(session)
        if line is None:
            return
        generic_turn(session, line)


if __name__ == "__main__":
    main()
