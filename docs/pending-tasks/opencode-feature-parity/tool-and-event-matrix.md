# OpenCode v2 tool, input, event, control and lifecycle matrix

**Parity scope (PD-14, 2026-10-07):** targets below implement LUNA's shared mechanisms, the same ones Codex and Claude use. Events for OpenCode-only features are ignored with bounded diagnostics, or *contained*. See [provider-parity-contract.md](provider-parity-contract.md).

Baseline OpenCode **v2.0.22** (`527f0b93`), checked against v2.0.24 (`e7a34f09`) and v2-head (`4a0f273f`). LUNA `9b3e9a2e`. All OpenCode citations are `oc2:` = `packages/` @`527f0b93`; LUNA `oc:` = `crates/harness/src/opencode/mod.rs`. The primary analysis is [notes/B-protocol-lifecycle.md](notes/B-protocol-lifecycle.md), with LUNA downstream traces in [notes/C-luna-integration.md](notes/C-luna-integration.md). Request-body findings were re-checked by the coordinator against `git show <sha>:packages/protocol/openapi.json` (E2).

**Version note.** Every request-contract change listed in §2 landed in **v2.0.4** (`13453f2da8c` and siblings, 2026-09-13). It is unchanged through v2.0.22, v2.0.24 and head. Between 2.0.22 and 2.0.24 the only API change is the new `vcs.init`, plus additive fields: `/api/info.capabilities`, `session.create.parentID`, and a 404 `LocationNotFoundError` for a missing directory. 2.0.24 → head has no OpenAPI or event-union change. Head-only behaviour that matters for LUNA's MCP tools: tool and namespace length caps were dropped (`c2f87853c6a`), and the server always reports `opencode` as the MCP client name (`b78d10cd716`); both affect `<server>_<tool>` parsing and `luna` tool naming on upgrade.

**Operation-count provenance.**
- The committed `packages/protocol/openapi.json` at `527f0b93` has **136** operations, and the coordinator's per-tag checks used that file.
- The 2.0.22 **source** groups define 140: they add `server.pair`, `server.connect`, `credential.list` and `credential.create`, which the committed JSON lacks.
- The docs sidebar counts 141.

So B's "pairing/credential routes added in 2.0.23–2.0.24" describes the committed JSON artifact, not the code. OC-00 must capture the **live** `/openapi.json` from the running server as the fingerprint baseline.

## 1. Transports

| Transport | v2.0.22 status | LUNA use | Decision |
| --- | --- | --- | --- |
| `opencode serve` HTTP `/api/*` (136 ops; OpenAPI at `/openapi.json`) | canonical | yes (`default` mode) | **Keep**. Switch the launch mode to `--stdio --port 0` (OC-02). |
| Global SSE `GET /api/event` (all locations and sessions; `data:` frames only; 15 s heartbeat; 4,096-frame overflow fails the stream; no replay) | supported | yes | Keep. Add envelope `seq` tracking and gap detection (OC-08). |
| Per-session durable log `GET /api/experimental/session/{id}/log?after=<seq>&follow=true` | experimental | no | Use for replay behind a feature gate, with message-list paging as the fallback (OC-08). |
| WebSocket | PTY only | no | n/a |
| ACP `opencode acp` | private server translating the same HTTP+SSE; "one prompt at a time" | no (left 2026-08-22) | Rejected: lossy subset. |
| `@opencode/client` / `@opencode/sdk` | TypeScript; embedded in a JS process | no | Rejected: sidecar cost, no wire gain. |
| `opencode run --format json` | lossy one-shot NDJSON | no | Rejected. |
| `--server <url>` attach to the managed service | supported | test hook only (`oc:252-256`) | Out of scope (OC-16, P3). |

## 2. Endpoints LUNA calls (v2 branch) and required fixes

| # | LUNA call | LUNA sends today | v2.0.22 contract | Outcome at 2.0.22 | Sev | Fix (WP) |
| --- | --- | --- | --- | --- | --- | --- |
| E1 | `GET /api/health` (`oc:474-490`) | — | **removed since 2.0.4** | empty 404 | S0 | Probe `GET /api/info` (any status with a JSON `version` = 2.x; 200 = ready; 503 = booting). Then fall back to `GET /global/health` for 1.x (OC-02). |
| E2 | `GET /global/health` | — | web UI `index.html`, 200 `text/html` | `detect()` returns `None` → 300 s timeout; attached mode defaults to V1 | S0 | As E1. With `--stdio`, the `{"url"}` line replaces polling. |
| E3 | `POST /api/session` (`oc:2206-2226`) | `{location:{directory}, permission:[{permission, pattern, action}]}` | `{id?, title?, agent?, model?, location?, metadata?, permissions?: [{action, resource, effect}], parentID? (24)}` | rules dropped if excess keys are ignored, or 400 on every create if strict (Q-3) | S1 | Send `permissions` as a v2 ruleset using v2 action names. Optionally a caller `id` (OC-03, OC-04). |
| E4 | `PATCH /api/session/{id}` on resume (`oc:1628-1635`) | `{permission:[…]}` | `{title?, metadata?, permissions?}` → 204, then `session.permissions` | rules dropped | S1 | As E3. Confirm via `session.permissions` (OC-04). |
| E5 | `GET /api/session/{id}` | — | `{data: Session.Info}` | works | — | Also read `outcome` and `time.idle` on resync (OC-08). |
| E6 | `GET /api/model` | — | `{id, providerID, name, limit, variants[], capabilities, cost, …}` | works (503 until models.dev loads) | — | Keep capabilities, cost and provider name (OC-10). |
| E7 | `GET /api/command` | — | `[{name, description?}]` | works | — | Query the run location; refresh on `command.updated` (OC-14). |
| E8 | `POST /api/session/{id}/model` | `{model:{providerID, id, variant?}}` | same | works | — | Read the effective model back (OC-10). |
| E9 | `POST /api/session/{id}/prompt` (`oc:2358-2372`, `:2478-2484`) | `{text, files:[{uri, name}]}` | `{id?: "msg_…", text, files?: [{uri, name?, description?, mention?}], agents?, skills?, metadata?, delivery?: "steer"/"queue", resume?}` → 200 inbox item | works. Response discarded; no `id`; default delivery `steer` | S3 | Send a caller `id` and explicit `delivery`; keep the inbox `id` (OC-07). Typed `agents` and `skills` (OC-09). |
| E10 | `POST /api/session/{id}/command` (`oc:2427-2461`) | `{command, text}` | **`{name, text, files?, agents?, skills?, delivery?}`**; 204 after the callback; 404/500 tagged errors | **400**, ignored → 60 s stall | S1 | Send `name`; check status; surface errors; forward attachments (OC-03). |
| E11 | `POST /api/session/{id}/interrupt` (`oc:908-921`) | JSON `null` | no body; `?resume=true`; `{interrupted: bool}` | works (E5) | S3 | Send no body; inspect `interrupted:false`; call before SIGTERM (OC-02, OC-07). |
| E12 | `POST /api/session/{id}/compact` (`oc:1161-1183`) | JSON **`null`** | object `{id?, delivery?}` → inbox item `type:"compaction"` | likely 400 (E5) | S2 | Send `{id}`; wait for `session.compaction.ended` or failed before reporting (OC-03, OC-10). |
| E13 | `GET /api/session/active` | — | `{data:{<id>:{type:"running"}}}` (local to the answering process) | works | — | Keep. |
| E14 | `GET /api/session/{id}/message` (`oc:778-791`) | no query | paginated; **default limit 50, order desc**; no v1 `info` wrapper | first 50 newest only | S1 (fork) | Page with `cursor` and `order=asc`; parse v2 items (OC-12). |
| E15 | `POST /api/session/{id}/fork` (`oc:793-856`) | `{messageID}` or `{}` | **`{before?: "msg_…"}`**, `additionalProperties:false` (OpenAPI) | full-history fork or 400; then readback fails → orphan fork | S1 | `{before}` is **exclusive**: the first message *after* the kept prefix, i.e. the next user message (`oc2:core/src/session/projector.ts:136-138`, `seq < boundary`). Omit `before` to fork through the newest message. Stop using the synthetic `"usage"` id as a checkpoint; delete or adopt the orphan on failure (OC-03, OC-12). |
| E16 | `POST /api/session/{s}/permission/{id}/reply` (`oc:2895-2897`, `:2989-2994`) | `{reply}` | **`{decision: "once"/"always"/"reject", message?}`**, `required:["decision"]` | **400**, no fallback, swallowed → tool blocks, turn hangs | **S0** | `{decision, message?}`; surface failures (OC-03, OC-04). |
| E17 | `GET /api/event` | `Accept: text/event-stream` + Basic | same | works | — | Track `id`, `durable.seq`, `location` (OC-08). |
| — | Spawn `opencode serve --port <free> --hostname 127.0.0.1` with `OPENCODE_SERVER_PASSWORD`, `OPENCODE_CLIENT=luna` (`oc:515-569`) | — | `--stdio` deletes the password from the tool env and exits on stdin EOF | works. TOCTOU port race; password readable by agent tools; no lease | S1/S3 | `serve --stdio --port 0`, `OPENCODE_PASSWORD`, piped stdin (OC-02). |

**Endpoints LUNA never calls but needs:**
- `GET /api/info` (OC-02).
- `/openapi.json` for the compatibility fingerprint (OC-02).
- `POST …/form/{id}/reply {answer}` and `DELETE …/form/{id}` (OC-05).
- `GET /api/permission/request` and `GET /api/form` for pending-request resync (OC-04/05/08).
- `…/inbox` list, patch and delete (OC-07).
- `…/experimental/session/{id}/log` (OC-08).
- `…/revert/stage {messageID, files:false}`, `DELETE …/revert` and `…/revert/commit` (OC-12).
- `…/diff` (OC-06/15).
- `PUT /api/experimental/mcp/{server} {config}` and `PUT …/instructions/entries/{key} {value}` (OC-13).
- `GET /api/agent`, `/api/skill`, `/api/mcp` and `/api/config` (OC-14).
- `…/background` (OC-15).

### Payload examples (redacted)

```json
// Permission: v2.0.22 wire (oc2:schema/src/permission.ts:24-44)
{"id":"evt_…","created":1759700000000,"type":"permission.asked",
 "data":{"id":"per_…","sessionID":"ses_…","action":"shell","resources":["git push origin main"],
         "metadata":{"command":"git push origin main"},
         "source":{"type":"tool","messageID":"msg_…","id":"call_…"}}}
// LUNA today → 400 InvalidRequestError (≥2.0.4)
POST /api/session/ses_…/permission/per_…/reply   {"reply":"once"}
// required
POST /api/session/ses_…/permission/per_…/reply   {"decision":"once"}
POST /api/session/ses_…/permission/per_…/reply   {"decision":"reject","message":"Skip this command; continue without pushing."}
```

```json
// Question / MCP elicitation form: v2.0.22 schema (oc2:schema/src/form.ts:6-172).
// form.created is ephemeral with data {form: Form.Info} and NO top-level sessionID,
// so LUNA drops it at oc:3634-3641.
{"id":"evt_…","type":"form.created",
 "data":{"form":{"id":"frm_…","sessionID":"ses_…",          // "global" sentinel for MCP elicitation
   "title":"…","metadata":{…},
   "fields":[                                                  // NonEmptyArray<Field>, tagged by "type"
     {"key":"q1","type":"multiselect","title":"…","options":[{"value":"a","label":"A","description":"…"}],
      "minItems":1,"maxItems":1,"custom":true,"required":true},
     {"key":"q2","type":"string","title":"…","format":"uri","options":[…],"custom":true,
      "when":[{"key":"q1","op":"eq","value":"a"}]},           // conditional visibility (AND)
     {"key":"auth","type":"external","url":"https://…","title":"…"}]}}}   // URL-mode elicitation
// Reply: Form.Reply = {answer: Record<key, string | number | boolean | string[]>}
POST /api/session/ses_…/form/frm_…/reply  {"answer":{"q1":["a"],"q2":"https://…"}}   // 409 FormAlreadySettledError if settled
DELETE /api/session/ses_…/form/frm_…                                                   // cancel
// Form.State: pending | answered{answer} | cancelled{message?}
```

Field kinds and constraints LUNA's shared question panel and MCP form contract must cover:
- `string`: `format` email, uri, date or date-time; min/max length; `pattern`; `placeholder`; `default`; optional `options` plus `custom`.
- `number` and `integer`: min/max, `default`.
- `boolean`: `default`.
- `multiselect`: `options` with descriptions; min/max items; `custom`; `default`.
- `external`: URL mode.
- Plus `required`, `hidden` and conditional `when`.

Unsupported kinds must be declined with a cancel message, never left to hang (OCF-56).

```json
// Prompt with correlation and explicit delivery (target)
POST /api/session/ses_…/prompt
{"id":"msg_<luna-queue-id-derived>","text":"…","delivery":"queue",
 "files":[{"uri":"file:///abs/host/path/img.png","name":"img.png"}]}
→ 200 {"data":{"id":"msg_…","sessionID":"ses_…","type":"user","delivery":"queue","time":{"created":…}}}
// then session.inbox.enqueued{inboxID} → session.inbox.delivered{inboxID} → … → session.execution.succeeded
```

```json
// Fork and rewind (target)
POST /api/session/ses_…/fork          {"before":"msg_<user message to cut before>"}
POST /api/session/ses_…/revert/stage  {"messageID":"msg_…","files":false}   // LUNA contract: files are kept
POST /api/session/ses_…/revert/commit
```

## 3. Inputs

| Input path | Native v2.0.22 | LUNA today | Gap | WP |
| --- | --- | --- | --- | --- |
| Initial text | `prompt.text` | sent | — | — |
| Initial images and files | `prompt.files[].uri` (`file:`/`data:`); 20 MiB; PNG/JPEG/GIF/WebP shown to the model; PDF, audio and video not sent; HTTP(S) URIs unsupported | `{uri, name}` plus path text appended to the prompt | No admission check (size, type, model image capability); duplicate path text; no `description` | OC-09 |
| Follow-up with images | same | fresh resumed run (warm routing refused) | Works via a cold restart only | OC-09 |
| Queued row with images | same | held `AttachmentsUnsupported` | Deliver at idle as native file parts | OC-09 |
| Steer while busy | `delivery:"steer"` incl. `files` | engine refuses images; adapter posts `attachments: &[]`; `TurnBoundary` mode | No native steering at all | OC-07, OC-09 |
| Retry after failure | same | LUNA retry re-runs the request | Images depend on the route | OC-09 |
| Restart between staging and dispatch | — | shared staging (`pending://`) | Untested for OpenCode | OC-09 |
| Slash command with attachments | `command.files` | dropped (`oc:2418-2460`) | Forward | OC-03 |
| Typed mention | `files[].mention` | rejected (`pi:26-30`) | Not pursued: LUNA mentions are Codex plugin/app identities (PD-14) | — |
| Typed skill | `skills[]` | rejected | Like Claude: map LUNA skill items to `skills[{id}]` from `skill.list` | OC-09, OC-14 |
| Agent mention | `agents[{name}]` (doesn't switch agent) | none | Not pursued (PD-14) | — |
| Synthetic input | `session.synthetic` | none | Not a user surface; observe only | OC-07 |
| User shell | `session.shell` | none | Not pursued (PD-14) | — |
| Compaction | `compact {id?, delivery?}` | `null` body | Fix body and lifecycle | OC-10 |

## 4. Tool vocabulary (v2.0.22 names) → LUNA mapping

| v2 tool | LUNA today (`oc:3534-3624`) | Target LUNA projection | WP |
| --- | --- | --- | --- |
| `read` | `ReadFile{path}` | Keep path, offset and limit; image/PDF result content → attachment refs (byte counts in the doc) | OC-06 |
| `glob`, `grep` | `Glob{pattern}`, `Search{pattern, path}` | Keep `path` and `include` separately | OC-06 |
| `edit`, `write` | `EditFile`, `WriteFile` | Keep the structured `files` diff → `ToolDiff`; doc sanitization unchanged | OC-06 |
| `patch` | `ApplyPatch{path:None}` | Per-file operations and partial-apply status → diffs | OC-06 |
| `shell` | **`Unknown{"shell"}`**, input stripped from the doc | `Exec{command, workdir, timeout, background}`; live output via `ToolActivity`; exit status; managed full-output file reference | OC-06 |
| `webfetch`, `websearch` | `WebFetch{url}`, `WebSearch{query}` | Keep `format`; the provider form goes through OC-05 | OC-06 |
| `question` | — (form dropped) | Shared question panel (OC-05); the tool card shows the answer | OC-05 |
| `skill` | `Unknown` | Skill-activation card | OC-06 |
| `subagent` | **`Unknown`**, spawn never registered | Spawn chip (`Agent: <description>`), child bound by `metadata.sessionID` / `session.created.parentID`, background flag, model | OC-11 |
| `execute` (Code Mode) | `Unknown` | Code Mode card plus nested tool calls if emitted (Q-12) | OC-06 |
| `opencode_session_rename`, `opencode_session_move`, `opencode_models` | `Unknown` | Typed cards; trigger the session-state observation (OC-14) | OC-06, OC-14 |
| `opencode_list_mcp_resources`, `opencode_read_mcp_resource` | `Unknown` | MCP resource card (server, uri) | OC-06 |
| `<server>_<tool>` (MCP) | `Unknown` (input stripped) | `ToolCall::Mcp{server, tool, args}`. Disambiguate the `_` split with `mcp.list` names. `lunaToolOutput` for rich content. | OC-06 |
| plugin tools `<ns>_<tool>` | `Unknown` | Generic but with input preserved; bounded diagnostics | OC-06 |
| `browser.*` | — | n/a (only attached by the OpenCode desktop) | — |
| v1: `bash`, `task`, `todowrite`, `list`, `multiedit`, `lsp*`, `batch` | mapped (v1 only) | Keep the v1 path; imported v1 history | OC-06 |

**Output caps:**
- The adapter's `OUTPUT_CAP` of 4,096 B (`oc:3520-3532`) must go. Full output goes to the host journal per `docs/reference/tool-output.md`.
- The synced doc keeps the existing preview limits (`ses:65-130`).
- Native truncation (`tool_output.max_lines/max_bytes`, 2000 lines / 50 KiB) writes the remainder to a managed file whose path is in `metadata`. LUNA should record that reference and fetch it on demand through the host, or label the output as natively truncated.

## 5. Event families: complete v2.0.22 list (94 types)

Handling today, from B §3.2: 19 consumed, 75 dropped. "Target handling" is the proposed LUNA behaviour, reusing existing `AgentEvent` variants and shared surfaces. "—" means keep as is or ignore. D = durable (carries `durable.seq`), E = ephemeral.

| # | v2 event | data fields | D/E | LUNA today (B) | LUNA line | Target handling | WP |
| ---: | --- | --- | --- | --- | --- | --- | --- |
| 1 | `location.shutdown` | — | E | dropped (no `data.sessionID`) | `oc:3638` | Mark the OpenCode server location gone; fail the affected runs visibly | OC-08 |
| 2 | `models-dev.refreshed` | — | E | dropped (no `data.sessionID`) | `oc:3638` | Invalidate the model catalog cache | OC-10 |
| 3 | `credential.updated` | — | E | dropped (no `data.sessionID`) | `oc:3638` | Invalidate account status (read-only) | OC-16 |
| 4 | `credential.switched` | integrationID, credentialID | E | dropped (no `data.sessionID`) | `oc:3638` | Invalidate account status (read-only) | OC-16 |
| 5 | `integration.updated` | — | E | dropped (no `data.sessionID`) | `oc:3638` | Invalidate the catalog and account status | OC-10 |
| 6 | `provider.updated` | — | E | dropped (no `data.sessionID`) | `oc:3638` | Invalidate the model catalog | OC-10 |
| 7 | `model.updated` | — | E | dropped (no `data.sessionID`) | `oc:3638` | Invalidate the model catalog | OC-10 |
| 8 | `agent.updated` | — | E | dropped (no `data.sessionID`) | `oc:3638` | Refresh the agent list (read-only) | OC-14 |
| 9 | `session.created` | sessionID, projectID, location, subpath?, parentID?, slug, title?, agent?, model?, metadata?, permissions?, version | D | normalized → `session.created{info:{id,parentID,title | `oc:slug}}` (child binding)` | Bind the child to its spawn by `parentID` (any depth); record agent, model and location | OC-11 |
| 10 | `session.agent.selected` | sessionID, agent, previous? | D | **dropped** (has sessionID; falls to `_ => Vec::new()`) | `oc:3849` | Effective-agent readback (read-only; PD-1) | OC-10 |
| 11 | `session.model.selected` | sessionID, model, previous? | D | **dropped** (has sessionID; falls to `_ => Vec::new()`) | `oc:3849` | Effective-model and variant readback | OC-10 |
| 12 | `session.moved` | sessionID, location, projectID, subpath? | D | **dropped** (has sessionID; falls to `_ => Vec::new()`) | `oc:3849` | Ignore with a bounded diagnostic: `opencode_session_move` is denied by LUNA rules (PD-14 containment) | OC-04 |
| 13 | `session.renamed` | sessionID, title | D | **dropped** (has sessionID; falls to `_ => Vec::new()`) | `oc:3849` | Ignore (titles come from `run_title`, like Codex and Claude; PD-14) | — |
| 14 | `session.metadata.updated` | sessionID, metadata | D | **dropped** (has sessionID; falls to `_ => Vec::new()`) | `oc:3849` | Ignore (diagnostic only) | — |
| 15 | `session.permissions` | sessionID, permissions | D | **dropped** (has sessionID; falls to `_ => Vec::new()`) | `oc:3849` | Confirm LUNA rules applied (readback) | OC-04 |
| 16 | `session.viewed` | sessionID, idle | D | **dropped** (has sessionID; falls to `_ => Vec::new()`) | `oc:3849` | Ignore (LUNA owns unread state) | — |
| 17 | `session.usage.updated` | sessionID, cost, tokens | E | normalized → `message.updated{id:"usage", tokens}`; `cost` dropped | `oc:3797` | `ProviderAccounting` (cost) plus correct context occupancy; stop synthesizing a `"usage"` message id | OC-10 |
| 18 | `session.deleted` | sessionID | D | **dropped** (has sessionID; falls to `_ => Vec::new()`) | `oc:3849` | Mark the binding broken; show a recoverable error | OC-12 |
| 19 | `session.forked` | sessionID, parentID, boundary, instructions?, instructionEntries? | D | **dropped** (has sessionID; falls to `_ => Vec::new()`) | `oc:3849` | Fork and side-chat receipt and provenance | OC-12 |
| 20 | `session.inbox.delivered` | sessionID, inboxID | D | **dropped** (has sessionID; falls to `_ => Vec::new()`) | `oc:3849` | Incorporation evidence for queue/steer receipts; turn start anchor | OC-07 |
| 21 | `session.inbox.enqueued` | sessionID, inboxID, item | D | **dropped** (has sessionID; falls to `_ => Vec::new()`) | `oc:3849` | Acceptance evidence | OC-07 |
| 22 | `session.inbox.cancelled` | sessionID, inboxID | D | **dropped** (has sessionID; falls to `_ => Vec::new()`) | `oc:3849` | Native steer cancelled → restore the queue row | OC-07 |
| 23 | `session.inbox.delivery.changed` | sessionID, inboxID, delivery | D | **dropped** (has sessionID; falls to `_ => Vec::new()`) | `oc:3849` | Receipt evidence only (no inbox UI, PD-14) | OC-07 |
| 24 | `session.execution.started` | sessionID | D | normalized → `session.status{busy}` | `oc:3675` | Busy (keep); also `ProviderTurnStarted` when not LUNA-initiated | OC-07 |
| 25 | `session.execution.succeeded` | sessionID | D | normalized → `session.idle` | `oc:3683` | Terminal (keep), correlated to the inbox id | OC-07 |
| 26 | `session.execution.failed` | sessionID, error | D | normalized → `session.error{name:error.type, data.message}` + `session.idle`; `status`/`response` dropped | `oc:3687` | Terminal error with status and body preserved | OC-07 |
| 27 | `session.execution.interrupted` | sessionID, reason | D | normalized → `session.interrupted` (any `reason`; reason dropped) | `oc:3679` | Terminal with `reason` (user / shutdown / superseded / inactivity) preserved | OC-07 |
| 28 | `session.instructions.updated` | sessionID, delta, text? | D | **dropped** (has sessionID; falls to `_ => Vec::new()`) | `oc:3849` | Confirm the LUNA runtime-contract entry is present | OC-13 |
| 29 | `session.synthetic` | sessionID, text, description?, metadata? | D | **dropped** (has sessionID; falls to `_ => Vec::new()`) | `oc:3849` | Attribute to a background-subagent completion or external input; `ProviderTurnStarted` for the wake execution | OC-07, OC-11 |
| 30 | `session.skill.activated` | sessionID, id, name, text | D | **dropped** (has sessionID; falls to `_ => Vec::new()`) | `oc:3849` | Skill activation chip | OC-14 |
| 31 | `session.shell.started` | sessionID, shell | D | **dropped** (has sessionID; falls to `_ => Vec::new()`) | `oc:3849` | Ignore (user shell `!cmd` not pursued, PD-14) | — |
| 32 | `session.shell.ended` | sessionID, shell, output | D | **dropped** (has sessionID; falls to `_ => Vec::new()`) | `oc:3849` | Ignore (user shell `!cmd` not pursued, PD-14) | — |
| 33 | `session.step.started` | sessionID, assistantMessageID, agent, model, snapshot?, started | D | normalized → `message.updated{role:assistant,id}`; agent/model/snapshot/started dropped | `oc:3708` | Effective agent and model per step (readback) | OC-10 |
| 34 | `session.step.streamed` | sessionID, assistantMessageID | D | **dropped** (has sessionID; falls to `_ => Vec::new()`) | `oc:3849` | Ignore | — |
| 35 | `session.step.ended` | sessionID, assistantMessageID, finish, rawFinish?, providerState?, cost, tokens, snapshot?, files? | D | **dropped** (has sessionID; falls to `_ => Vec::new()`) | `oc:3849` | Finish reason (`length` / `content-filter`) notice; per-step cost and tokens; snapshot and changed files | OC-07, OC-10 |
| 36 | `session.step.failed` | sessionID, assistantMessageID, error, finish?, rawFinish?, providerState?, cost?, tokens?, snapshot?, files? | D | `error.type=="aborted"` dropped; else → `session.warning` chip; cost/tokens/finish dropped | `oc:3696` | Keep (aborted → ignore; others → error with status) | OC-07 |
| 37 | `session.text.started` | sessionID, assistantMessageID, ordinal | D | normalized → `message.part.updated` text part, id `<msg>:t<ordinal>`, text "" | `oc:3714` | Keep | — |
| 38 | `session.text.delta` | sessionID, assistantMessageID, ordinal, delta | E | normalized → `message.part.delta{field:text}` | `oc:3721` | Keep | — |
| 39 | `session.text.ended` | sessionID, assistantMessageID, ordinal, text, state? | D | normalized → `message.part.updated` with full text; `state` dropped | `oc:3714` | Keep; preserve `state` only if needed for fork/replay | OC-06 |
| 40 | `session.reasoning.started` | sessionID, assistantMessageID, ordinal, state? | D | normalized → reasoning part (text ""); `state` dropped | `oc:3731` | Keep | — |
| 41 | `session.reasoning.delta` | sessionID, assistantMessageID, ordinal, delta | E | normalized → `message.part.delta` on `<msg>:r<ordinal>` | `oc:3721` | Keep | — |
| 42 | `session.reasoning.ended` | sessionID, assistantMessageID, ordinal, text, state? | D | normalized → reasoning part with full text | `oc:3731` | Keep | — |
| 43 | `session.tool.input.started` | sessionID, assistantMessageID, id, name | D | normalized → tool part `pending`; name cached by (session,msg,id) | `oc:3736` | Keep (tool name source) | — |
| 44 | `session.tool.input.delta` | sessionID, assistantMessageID, id, delta | E | **dropped** (has sessionID; falls to `_ => Vec::new()`) | `oc:3849` | Streamed tool input → `ToolActivity` (optional) | OC-06 |
| 45 | `session.tool.input.ended` | sessionID, assistantMessageID, id, text | D | **dropped** (has sessionID; falls to `_ => Vec::new()`) | `oc:3849` | Final raw input text (diagnostic) | OC-06 |
| 46 | `session.tool.called` | sessionID, assistantMessageID, id, input, executed, state? | D | normalized → tool part `running` + input; `executed`/`state` dropped | `oc:3747` | Keep; map v2 tool names (`shell`, `subagent`, `patch`, MCP split) | OC-06 |
| 47 | `session.tool.progress` | sessionID, assistantMessageID, id, metadata | E | **dropped** (has sessionID; falls to `_ => Vec::new()`) | `oc:3849` | `ToolActivity` (live stdout, progress) sharing the call owner | OC-06 |
| 48 | `session.tool.success` | sessionID, assistantMessageID, id, content, metadata?, executed, resultState? | D | normalized → `completed`, output = join of `content[].text`; file content, `metadata`, `resultState` dropped | `oc:3759` | Full output into the journal (no 4 KiB cap); `lunaToolOutput` OpenCode projection with file/image content, metadata, diff, exit code, child `sessionID` | OC-06 |
| 49 | `session.tool.failed` | sessionID, assistantMessageID, id, error, content?, metadata?, executed, resultState? | D | normalized → `error` with `error.message` only; partial `content`/`metadata` dropped | `oc:3780` | Error plus partial content and metadata preserved | OC-06 |
| 50 | `session.retry.scheduled` | sessionID, assistantMessageID, attempt, at, error | D | **dropped** (has sessionID; falls to `_ => Vec::new()`) | `oc:3849` | `ProviderNotice{Retrying}` (temporary), attempt and next time | OC-07 |
| 51 | `session.compaction.started` | sessionID, reason, recent, inputID? | D | **dropped** (has sessionID; falls to `_ => Vec::new()`) | `oc:3849` | `ContextCompaction` lifecycle start (not turn end) | OC-10 |
| 52 | `session.compaction.delta` | sessionID, text | E | **dropped** (has sessionID; falls to `_ => Vec::new()`) | `oc:3849` | Optional summary stream | OC-10 |
| 53 | `session.compaction.ended` | sessionID, reason, model?, providerState?, providerContext?, text, recent, cost?, tokens? | D | **dropped** (has sessionID; falls to `_ => Vec::new()`) | `oc:3849` | `ContextCompaction` completed; refresh the context ring | OC-10 |
| 54 | `session.compaction.failed` | sessionID, reason, error, inputID?, cost?, tokens? | D | **dropped** (has sessionID; falls to `_ => Vec::new()`) | `oc:3849` | `ContextCompaction` failed; visible error | OC-10 |
| 55 | `session.revert.staged` | sessionID, revert | D | **dropped** (has sessionID; falls to `_ => Vec::new()`) | `oc:3849` | Rewind preview state | OC-12 |
| 56 | `session.revert.cleared` | sessionID | D | **dropped** (has sessionID; falls to `_ => Vec::new()`) | `oc:3849` | Rewind cancelled; ignore the empty wake execution (T3) | OC-12 |
| 57 | `session.revert.committed` | sessionID, to | D | **dropped** (has sessionID; falls to `_ => Vec::new()`) | `oc:3849` | Rewind receipt | OC-12 |
| 58 | `filesystem.changed` | file, event | E | dropped (no `data.sessionID`) | `oc:3638` | Ignore (LUNA watches the repo itself) | — |
| 59 | `reference.updated` | — | E | dropped (no `data.sessionID`) | `oc:3638` | Ignore | — |
| 60 | `permission.asked` | id, sessionID, action, resources, save?, metadata?, source?, message? | E | passed through as `permission.asked{properties:data}` — but consumer reads v1 field names (§3.3) | `oc:3840` | Read v2 fields `action`, `resources[]`, `metadata`, `source{messageID,id}`, `message`, `save` → shared approval details | OC-04 |
| 61 | `permission.replied` | sessionID, requestID, reply | E | **dropped** (has sessionID; falls to `_ => Vec::new()`) | `oc:3849` | Close or stale the LUNA card when it was answered elsewhere | OC-04 |
| 62 | `plugin.updated` | — | E | dropped (no `data.sessionID`) | `oc:3638` | Ignore (optional inventory refresh) | OC-14 |
| 63 | `project.updated` | id, canonical, vcs?, name?, icon?, commands?, time, sandboxes | E | dropped (no `data.sessionID`) | `oc:3638` | Ignore | — |
| 64 | `worktree.updated` | projectID | E | dropped (no `data.sessionID`) | `oc:3638` | Ignore (observe only, PD-5) | — |
| 65 | `worktree.resolved` | projectID, directory, previous, adopted? | D | dropped (no `data.sessionID`) | `oc:3638` | Ignore (PD-5) | — |
| 66 | `command.updated` | — | E | dropped (no `data.sessionID`) | `oc:3638` | Refresh the slash-command list for that location | OC-14 |
| 67 | `config.updated` | — | E | dropped (no `data.sessionID`) | `oc:3638` | Invalidate effective-config readback | OC-14 |
| 68 | `skill.updated` | — | E | dropped (no `data.sessionID`) | `oc:3638` | Refresh the skill list | OC-14 |
| 69 | `pty.created` | info | E | dropped (no `data.sessionID`) | `oc:3638` | Ignore | — |
| 70 | `pty.updated` | info | E | dropped (no `data.sessionID`) | `oc:3638` | Ignore | — |
| 71 | `pty.exited` | id, exitCode | E | dropped (no `data.sessionID`) | `oc:3638` | Ignore | — |
| 72 | `pty.deleted` | id | E | dropped (no `data.sessionID`) | `oc:3638` | Ignore | — |
| 73 | `persistent-pty.added` | sessionID, terminal | E | **dropped** (has sessionID; falls to `_ => Vec::new()`) | `oc:3849` | Ignore | — |
| 74 | `persistent-pty.removed` | sessionID, ptyID | E | **dropped** (has sessionID; falls to `_ => Vec::new()`) | `oc:3849` | Ignore | — |
| 75 | `shell.created` | info | E | dropped (no `data.sessionID`) | `oc:3638` | `NativeTaskChanged{kind: Shell}`, like Claude `local_bash` | OC-11 |
| 76 | `shell.exited` | id, exit?, status | E | dropped (no `data.sessionID`) | `oc:3638` | `NativeTaskChanged{kind: Shell}` settled | OC-11 |
| 77 | `shell.deleted` | id | E | dropped (no `data.sessionID`) | `oc:3638` | `NativeTaskChanged{kind: Shell}` removed | OC-11 |
| 78 | `form.created` | form | E | dropped (no `data.sessionID`) | `oc:3638` | Route by `form.sessionID` (or the MCP pseudo-session rule) to the shared question panel or MCP form; arm stall exemption | OC-05 |
| 79 | `form.replied` | id, sessionID, answer | E | **dropped** (has sessionID; falls to `_ => Vec::new()`) | `oc:3849` | Close the LUNA prompt (answered elsewhere or confirmed) | OC-05 |
| 80 | `form.cancelled` | id, sessionID | E | **dropped** (has sessionID; falls to `_ => Vec::new()`) | `oc:3849` | Close the LUNA prompt as cancelled | OC-05 |
| 81 | `websearch.updated` | — | E | dropped (no `data.sessionID`) | `oc:3638` | Ignore | — |
| 82 | `session.status` | sessionID, status | E | **dropped** (has sessionID; falls to `_ => Vec::new()`) | `oc:3849` | Legacy, no v2.0.22 producer: tolerate, don't depend on it | OC-07 |
| 83 | `session.idle` | sessionID | E | **dropped** (has sessionID; falls to `_ => Vec::new()`) | `oc:3849` | Legacy and deprecated, no producer: tolerate | — |
| 84 | `tui.prompt.append` | text | E | dropped (no `data.sessionID`) | `oc:3638` | Ignore (TUI-only) | — |
| 85 | `tui.command.execute` | command | E | dropped (no `data.sessionID`) | `oc:3638` | Ignore (TUI-only) | — |
| 86 | `tui.toast.show` | title?, message, variant, duration? | E | dropped (no `data.sessionID`) | `oc:3638` | Ignore (TUI-only) | — |
| 87 | `tui.session.select` | sessionID | E | **dropped** (has sessionID; falls to `_ => Vec::new()`) | `oc:3849` | Ignore (TUI-only) | — |
| 88 | `installation.updated` | version | E | dropped (no `data.sessionID`) | `oc:3638` | Optional: invalidate cached version and capability record | OC-17 |
| 89 | `installation.update-available` | version | E | dropped (no `data.sessionID`) | `oc:3638` | Optional update hint | OC-17 |
| 90 | `vcs.branch.updated` | branch? | E | dropped (no `data.sessionID`) | `oc:3638` | Ignore | — |
| 91 | `mcp.status.changed` | server | E | dropped (no `data.sessionID`) | `oc:3638` | Integration inventory refresh, like Claude `session_mcp_observation` | OC-14 |
| 92 | `mcp.resources.changed` | server | E | dropped (no `data.sessionID`) | `oc:3638` | Ignore (optional) | — |
| 93 | `rpc.*` | — | E | dropped (no `data.sessionID`) | `oc:3638` | Ignore | — |
| 94 | `server.connected` | — | E | dropped (no `data.sessionID`); only the first byte matters: `stream_bus` announces Connected on first chunk | `oc:3638 (Connected: 3987-3991)` | Connection-generation marker (reconnect accounting) | OC-08 |

**Normalization rules for OC-02/OC-06/OC-08:**
1. Stop dropping frames that lack top-level `data.sessionID` before matching. Route `form.created` by `data.form.sessionID`.
2. Count and surface first-seen unknown event types and decode failures with bounded diagnostics (type and field names only). This replaces the silent drops at `oc:3849` and `oc:4009-4011`.
3. Keep `durable.seq` per session and the connection generation.
4. Decide whether to keep the 1.x-shaped rewrite (`normalize_v2_frame`) or decode v2 natively. B recommends at least extending the normalizer. A native v2 decoder removes the synthetic `"usage"` message id class of bugs, so prefer it if the v1 path is kept separately.

## 6. Controls

| Control | Native v2.0.22 | LUNA today | Target | WP |
| --- | --- | --- | --- | --- |
| Stop | `interrupt` → `{interrupted}`; foreground children interrupted; background children independent | interrupt + 5 s timeout; children force-settled locally | Inspect the result; interrupt background children explicitly when stop policy says so; never report Completed | OC-07, OC-11 |
| Send now (queue) | `interrupt` then prompt | engine Send-now | Correlate the new prompt with the inbox id after the terminal | OC-07 |
| Steer | `prompt{delivery:"steer"}`; `inbox.delivered` | unavailable (`TurnBoundary`) | Advertise mid-turn steering only after OC-07 evidence (PD-2) | OC-07 |
| Cancel a pending steer | `DELETE …/inbox/{id}` | — | Restore the queue row | OC-07 |
| Approval reply | `{decision, message?}` | `{reply}` (400) | Deny = reject with message (model continues); Cancel = reject without message (stops); timeout policy explicit | OC-04 |
| Question/form reply | `form/{id}/reply {answer}`, `DELETE form/{id}` | — | Shared panel | OC-05 |
| Model or variant switch | `POST …/model` | once per run | Like Claude `can_reconfigure_live`: mid-chat switch plus readback | OC-10 |
| Agent switch | `POST …/agent` | none | Not pursued (PD-14): no agent picker for any provider; the effective agent is shown read-only | — |
| Compact | `compact {id}` | `null` body | Lifecycle-correct | OC-10 |
| Fork | `fork {before}` | `{messageID}` | Correct boundary | OC-12 |
| Rewind / edit earlier message | `revert.stage {files:false}` + commit/clear | none | Like Claude: `native_history_turns` plus `rewind_native_conversation` | OC-12 |
| Background | `POST …/background` | none | Not pursued (PD-14) | — |
| Child stop | `interrupt` on the child session | none | `Harness::stop_native_child`, like Claude (settle on the child's terminal, not the ack) | OC-11 |

## 7. Lifecycle contract (target)

| Concern | v2.0.22 behaviour | LUNA target | WP |
| --- | --- | --- | --- |
| Launch | `serve --stdio --port 0`, env `OPENCODE_PASSWORD`, stdin pipe as the ownership lease; prints `{"url"}` after boot; SIGTERM then kill after 3 s | Same as the official `Standalone` launcher | OC-02 |
| Readiness and generation | `/api/info` (auth): 503 + `retry-after` while booting; 200 ready | Detect 2.x by a JSON `version`; 1.x by `/global/health`; fail loudly before the first prompt | OC-02 |
| Compatibility | OpenAPI `info.version` is always `0.0.1` (not a contract); no capabilities document before 2.0.24 | Fingerprint the subset of operations and fields LUNA depends on; pin the event set per verified range; classify the version as known-good, known-bad (2.0.0–2.0.3 with old code) or untested, and run untested versions in degraded mode with a notice | OC-02 |
| Turn correlation | Inbox id from the prompt response; `inbox.delivered` starts the turn; next execution terminal ends it; one terminal per busy period | ACP-style correlation; `ProviderTurnStarted` for executions not started by LUNA | OC-07 |
| Delivery evidence | accepted = 200 inbox item; incorporated = `inbox.delivered`; cancelled = `inbox.cancelled` | Map onto the message-queue contract's accepted, recorded and unknown states; never auto-replay unknown | OC-07 |
| Errors | tagged JSON HTTP errors; `SessionStructuredError{type, message, status?, response?}` on step, tool, execution and compaction failures | Preserve type, status and body (redacted, bounded); retries are temporary notices | OC-07 |
| Reconnect | no replay; 4,096-frame overflow; 15 s heartbeat | Gap detection by `seq`; replay from the session log or paged messages; resync pending permissions and forms; read-idle timeout > 15 s | OC-08 |
| Shutdown | SIGTERM keeps restart claims; only the managed service sweeps them | Interrupt active turns before SIGTERM; never leave claims for the user's background service | OC-02, OC-08 |
| Process model | one process serves many locations | Keep one server per run until measured (B §5.7); move discovery to the run location | OC-02, OC-14 |

## 8. LUNA `AgentEvent` variants OpenCode never emits today

From C §0.1 (`crates/proto/src/agent.rs:961-1253`):

| Variant | OpenCode source | WP |
| --- | --- | --- |
| `ToolActivity` | `session.tool.progress`, `tool.input.delta` | OC-06 |
| `GeneratedImage` | non-text tool or assistant content | OC-06 |
| `ProviderAccounting` | `session.usage.updated.cost`, `step.ended.cost` | OC-10 |
| `ContextCompaction` | `session.compaction.*` | OC-10 |
| `ChildStatus`, `ChildProgress` | child `session.execution.*` (Codex pattern); child step model, usage and tool counts (Claude pattern) | OC-11 |
| `ProviderNotice` (Retrying) | `session.retry.scheduled` | OC-07 |
| `ProviderTurnStarted` | `execution.started` without a LUNA prompt (background wake, synthetic input, revert-clear wake) | OC-07 |
| `ProviderEvidence` | inbox acceptance and delivery evidence | OC-07 |
| `NativeTaskChanged` | background shell jobs (`shell.*`) → `kind: Shell`, like Claude `local_bash` | OC-11 |
| `GoalChanged`, `NativeReview`, `NativeSchedules`, `AsyncQuestions`, `PeerMessage` | n/a (no native counterpart; `/review` is text) | — |

## 9. v1 (1.18.34) path: what differs (to be removed, PD-15)

**Decision 2026-10-07:** LUNA drops OpenCode v1 support. The section below is kept as the record of what the removed path did.

The v1 path stays relevant because a GUI LUNA silently falls back to Homebrew v1 when its interactive login-shell probe fails (F-WS-12). The v1 path consumes `session.status` (including retry), `session.error`, `session.idle`, `message.updated`, `message.part.updated/delta`, `permission.asked` and `question.asked` (`oc:2556-3098`). It still drops `todo.updated`, `session.diff`, `session.compacted`, `message.removed`, `permission.replied` and `question.replied/rejected` (C §3, D3). On v1 the question tool works (without option descriptions), "Allow for task" exists, and nested transcripts work one level deep.
