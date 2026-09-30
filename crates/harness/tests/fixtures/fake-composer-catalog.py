#!/usr/bin/env python3
"""Deterministic app-server peer. No credentials, model or real provider access."""
import json
import os
import sys

cwd = os.getcwd()
for line in sys.stdin:
    req = json.loads(line)
    method = req.get("method")
    params = req.get("params", {})
    if "id" not in req:
        continue
    result = {}
    if method == "initialize":
        result = {"userAgent": "catalog-fixture/1"}
    elif method == "account/read":
        result = {"account": {"type": "chatgpt", "accountId": "fixture-account"}}
    elif method in ("plugin/installed", "plugin/list"):
        def plugin(identity, **extra):
            return dict(id=identity, name="Same name", installed=True, enabled=True,
                        availability="AVAILABLE", interface={"displayName": "Same name"}, **extra)
        rows = [plugin("opaque@catalog"), plugin("admin@catalog")]
        rows[1]["availability"] = "DISABLED_BY_ADMIN"
        rows.append(plugin("off@catalog")); rows[-1]["enabled"] = False
        rows.append(plugin("suggestion@catalog")); rows[-1]["installed"] = False
        result = {"marketplaces": [{"name": "fixture", "plugins": rows}]}
    elif method == "app/installed":
        if params.get("threadId"):
            print(json.dumps({"id": req["id"], "error": {"code": -32600, "message": "thread not found"}}), flush=True)
            continue
        result = {"apps": [{"id": "app_exact", "enabled": True, "callable": True, "runtimeName": "Same name"},
                           {"id": "blocked", "enabled": True, "callable": False}]}
    elif method == "app/read":
        result = {"apps": [{"id": "app_exact", "name": "Same name", "description": "Callable app"}], "missingAppIds": []}
    elif method == "skills/list":
        result = {"data": [{"cwd": cwd, "skills": [
            {"name": "fixture-skill", "path": cwd + "/Skill (one)/SKILL.md", "enabled": True},
            {"name": "disabled-skill", "path": cwd + "/off/SKILL.md", "enabled": False}], "errors": []}]}
    elif method == "thread/read":
        result = {"thread": {"id": params.get("threadId")}}
    else:
        print(json.dumps({"id": req["id"], "error": {"code": -32601, "message": "Method not found"}}), flush=True)
        continue
    print(json.dumps({"id": req["id"], "result": result}), flush=True)
