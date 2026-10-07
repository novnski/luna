# B — Protocol and lifecycle (OpenCode v2 ↔ LUNA adapter)

Owner: workstream B. Research date: 2026-10-06. Pins: OpenCode `527f0b93` (v2.0.22), `e7a34f09` (v2.0.24), `4a0f273f` (v2-head); LUNA `9b3e9a2e`. Conventions: see `00-baseline.md`.

Status: COMPLETE (research only; no builds, no sessions/prompts, no server started). Scratch artifacts (OpenAPI copies per pin, extracted event tables, diff scripts) live in the session scratchpad `B/` and are not part of the deliverable.

Severity scale used throughout: **S0** blocks every v2 run or approval · **S1** whole feature broken or safety setting silently unapplied · **S2** narrower breakage · **S3** degraded fidelity.

## 1. Native integration transports (v2.0.22)

Path prefix for all OpenCode citations: snapshot of `527f0b93` (`packages/...`). Docs retrieved 2026-10-06 (E3).

| # | Transport | Status @v2.0.22 | Evidence | Notes for LUNA |
| --- | --- | --- | --- | --- |
| T1 | `opencode serve` HTTP `/api/*` (Effect HttpApi; OpenAPI 3.1 served live at `/openapi.json`) | **supported — the canonical surface** | E2 `server/src/routes.ts:176`; `cli/src/commands/commands.ts:537-551` ("Start the v2 API and web server"); `protocol/openapi.json` info: *"Experimental HttpApi surface for selected instance routes"* (136 ops at 2.0.22); root `V2_HTTP_API_AUDIT.md` (dispositions Keep/Change/Remove/Experimental-only). E3 https://opencode.ai/v2/docs/api/ (spec published as `/v2/openapi.json`) | Everything else (ACP, `run`, TUI, desktop, SDK client) is a client of this surface. |
| T2 | `serve` modes: `default` (foreground), `--stdio` (private, lease-owned), `--service` (managed background daemon) | supported | E2 `cli/src/server-process.ts:20,58-62,163-169,197-210`; `cli/src/services/standalone.ts:18-33` | `--stdio` is the official private-server pattern (§2.1). LUNA uses `default`. |
| T3 | Global SSE `GET /api/event` | supported, **volatile** | E2 `server/src/handlers/event.ts:12-33`; `protocol/src/groups/event.ts:44-52` | Only native live stream; one per server, all locations. No per-directory stream exists at 2.0.22. |
| T4 | Per-session durable SSE `GET /api/experimental/session/{id}/log?after=<seq>&follow=true` | experimental | E2 `protocol/src/groups/session.ts:727-742`; E3 docs/api | Only replay-capable stream: durable events after an exclusive aggregate `seq`, then live. Ephemeral deltas are not in it. |
| T5 | WebSocket | **PTY only** (`GET /api/pty/{id}/connect`, persistent-pty connect, ticket header `x-opencode-ticket`) | E2 `protocol/src/groups/pty.ts:10`; `server/src/handlers/pty-socket.ts`; `server/src/process.ts:189-191` (ticket URLs bypass Basic auth) | No websocket for session events. Out of LUNA's adapter scope. |
| T6 | ACP `opencode acp` | supported, **thin translation over T1+T3** | E2 `cli/src/commands/handlers/acp.ts:12-18` (spawns `Standalone.start()` then speaks ACP over stdio to it); `cli/src/acp/translate.ts:334-342,618-620`; E3 https://opencode.ai/v2/docs/cli/acp/ (*"starts a private OpenCode server for its own use"*, *"A session runs one prompt at a time"*) | Strictly a lossy projection of the HTTP API; ACP now correlates turns via `session.inbox.delivered` (LUNA header's 1.18 ACP critique is outdated for v2, but ACP still cannot carry more than HTTP). |
| T7 | `@opencode/client` (`packages/client`: promise + effect + solid; generated from OpenAPI by `packages/httpapi-codegen`) | supported | E2 `client/src/promise/generated/{client,types}.ts`; `client/src/shared-events.ts:9-148`; E3 https://opencode.ai/v2/docs/build/client/ (*"Live-only, with no replay or automatic reconnection"*) | TypeScript only. Same HTTP+SSE; adds `Service.ensure/discover` (background-service discovery with version predicate) and a 4,096-event per-subscriber buffer. |
| T8 | `@opencode/sdk` (`packages/sdk`) | supported, **embedded in-process** | E3 https://opencode.ai/v2/docs/build/sdk/ (*"opens no HTTP listener and adds no network hop"*); E2 `server/src/routes.ts:99-105` `createEmbeddedRoutes` (password none) | Requires a Bun/JS host process; not usable from Rust without a JS sidecar. |
| T9 | `packages/protocol` / `packages/schema` / `packages/server` / `packages/httpapi-codegen` | internal building blocks | E2 | `protocol` = HttpApi groups + `openapi.json`; `schema` = event + payload schemas (event manifest `schema/src/event-manifest.ts:71-81`); `server` = handlers + process; `httpapi-codegen` = client generator. Not separately consumable transports. |
| T10 | `opencode run --format json` | supported (NDJSON on stdout, client of service/standalone/`--server`) | E2 `cli/src/commands/commands.ts:376-416`; `cli/src/run/noninteractive.ts:101-102` (`{type,timestamp,sessionID,...}`; types `text`, `reasoning`, `step_start`, `tool_use`, `step_finish`, `error`) | Lossy one-shot projection (no permissions/forms round-trip). Not a replacement transport. |
| T11 | Attach/remote: `--server <url>` and `--standalone` on TUI/`run`/`api`; `opencode api <operationId|METHOD path>`; `opencode pair` (+ `POST /api/pair`, `GET /auth/connect/:code`) | supported | E2 `cli/src/commands/commands.ts:15-24,100-115,519-536`; `cli/src/services/server-connection.ts:21-51` (explicit URL: `client.server.info` + version-mismatch warning); `protocol/src/groups/server.ts:36-58` | Default client target is the **managed background service**, not a private server. |
| T12 | What desktop/web/TUI use | T1+T3 via `@opencode/client` | E2 `server-connection.ts` (TUI/run), `services/web-ui.ts:17-26` (web shell served by `serve`, `/api/*` behind auth), desktop origins allowed in `server/src/cors.ts:11-20` (`oc://renderer`, `tauri://localhost`) | Nobody first-party uses ACP for its own UI. |

**Not present in the 2.0.22 HTTP API** (E2, full op list in scratch `openapi-paths-v2.0.22.txt`): `/api/health`, `/global/*`, `abort` (→ `interrupt`), `summarize` (→ `compact`), share/unshare, todo endpoint/event, per-part retrieval (parts live inside message items), `question.*` events (questions are **forms**: `form.created|replied|cancelled`).

## 2. Lifecycle contract (v2.0.22)

### 2.1 Startup, ownership, auth, CORS, instance model `[LIFE]`

- **Who spawns.** Three ownership models (E2 `cli/src/server-process.ts:45-172`):
  - `default`: caller owns the process; lives until signal (`Effect.never`, `:169`). Prints `server listening on http://127.0.0.1:<port>` (`:163`) and, only if no env password was supplied, `server password <pw>` (`:164`).
  - `--stdio`: prints one JSON line `{"url":"http://127.0.0.1:<port>"}` to stdout **after boot completes** (`:163`; `start` returns only after `status.ready`, `server/src/process.ts:93-118,135`), deletes `OPENCODE_PASSWORD`/`OPENCODE_SERVER_PASSWORD` from its env so tools don't inherit it (`:74-77`), and exits when stdin reaches EOF (`:168,197-210`). Official private-server launcher: `serve --stdio --port 0`, env `OPENCODE_PASSWORD`, stdin piped as ownership lease, SIGTERM then force-kill after 3 s (`cli/src/services/standalone.ts:18-33`).
  - `--service`: chdir to home, config/password from service config, default port per channel (`service-config.ts:35`), incumbent detection, registration file; restart continuity resumes executions whose claim was never released (`server/src/process.ts:104-108,237-239`) — only in service mode (needs `lifecycle`).
- **Port/hostname.** Hostname default `127.0.0.1` (`server-process.ts:61`, `server/src/process.ts:53`). With no `--port`: bind 4096, increment on `EADDRINUSE` (`process.ts:139-146`). With `--port N`: bind exactly N, fail on conflict. `--port 0` → OS-assigned (used by standalone).
- **Auth.** Password is **mandatory**: `start` fails `"Missing server password"` without one (`process.ts:51-52`); CLI supplies env or random (`server-process.ts:78-84`). Env precedence: `OPENCODE_PASSWORD` then legacy `OPENCODE_SERVER_PASSWORD` (`cli/src/env.ts:10-13`). HTTP Basic, username fixed `opencode` (`server/src/auth.ts:20`); pairing session tokens `<expiry>.<hmac>` accepted wherever the password is (`auth.ts:32-55`), 30-day TTL, cookie `opencode_session_<port>` (`auth.ts:57-61`). `/api/info` is answered before readiness but still requires auth (`process.ts:185-186`). Unauthenticated exceptions: pairing connect URL, PTY ticket URLs (`process.ts:189-194`), and the web UI shell for every non-`/api`, non-`/auth/` path (`web-ui.ts:16-26`).
- **CORS.** Allowed origins: none (non-browser), `http://localhost:*`, `http://127.0.0.1:*`, `oc://renderer`, `tauri://localhost|tauri.localhost`, `https://*.opencode.ai`, plus `--cors` list (`server/src/cors.ts:11-20`; applied `process.ts:71`, maxAge 86400).
- **Instance model.** One process serves **many locations**: each request resolves a location from query `location[directory]` or header `x-opencode-directory` (URI-decoded) else `process.cwd()` (`server/src/location.ts:39-47`), then provides that location's service graph from `LocationServiceMap` (`location.ts:57-66`). Session routes resolve location from the session (`middleware/session-location.ts`). Global/shared: one SQLite DB under the data dir (`server-process.ts:100-102`), credentials, event bus. Locations can be listed/evicted (`GET|DELETE /api/debug/location`), reloaded (`POST /api/location/reload`), and emit `location.shutdown`.

### 2.2 Readiness and version detection `[LIFE]`

- **Single endpoint:** `GET /api/info` → `{version, pid, urls, paths:{tmp}}` (`server/src/process.ts:200-214`; `protocol/src/groups/server.ts:36`). Readiness is the **status code**: 200 ready, 503 + `retry-after: 1` while starting/stopping, 500 failed. It is answered (with auth) **before** the app graph is ready (`process.ts:185-188`).
- Every other route during boot returns 503 JSON `{"code":"service_starting"|"service_stopping"}` (+`retry-after: 1`) or `{"code":"service_failed",...}` (`process.ts:216-230`).
- `GET /api/health` was **removed** in commit `13453f2da8c` ("refactor(protocol): consolidate server status", 2026-09-13), first absent in **v2.0.4**; absent in every tag 2.0.4…2.0.24 and v2-head (E2, `git show <tag>:packages/protocol/openapi.json`). At 2.0.22 an authenticated `GET /api/health` hits `RouteNotFound` → empty **404** (`web-ui.ts:23-25`); `GET /global/health` falls through to the web UI and returns **200 `text/html` index.html** (`web-ui.ts:26,35-54`).
- Version field: `version` is `OPENCODE_VERSION` (`server-process.ts:91`), e.g. `"2.0.22"`. No protocol/API version field, no capabilities document; OpenAPI `info.version` is `0.0.1` at every pin (not a contract version).

### 2.3 Discovery endpoints `[LIFE][AGT][EXT][MOD]`

All take optional `location` query (or the directory header) and return `{location:{directory}, data}` unless noted (E2 `openapi.json` @527f0b93):

| Need | Endpoint | Notes |
| --- | --- | --- |
| Server/version | `GET /api/info` | §2.2 |
| Location/project | `GET /api/location`, `GET /api/project`, `GET /api/vcs`, `GET /api/vcs/status` | |
| Agents | `GET /api/agent`, `GET /api/agent/{agentID}` | + `agent.updated` event |
| Models | `GET /api/model` (enabled models, flat; `{id, modelID, providerID, name, variants[], limit, enabled, status, capabilities, cost, ...}`), `GET /api/model/default` (nullable) | 503 `ServiceUnavailableError` while models.dev catalog loads |
| Providers | `GET /api/provider`, `GET /api/provider/{id}`, integrations `GET /api/integration` | |
| Commands | `GET /api/command` → `[{name, description?}]` | `command.updated` |
| Skills | `GET /api/skill` | `skill.updated` |
| MCP | `GET /api/mcp`, `GET /api/mcp/resource` | `mcp.status.changed`, `mcp.resources.changed` |
| Config | `GET /api/config` (documents + OpenCode directories), `GET /api/config/shell` | `config.updated` |
| Plugins | `GET /api/plugin` | `plugin.updated` |
| References | `GET /api/reference` | |
| Tools | no tool-catalog endpoint at 2.0.22 | tools only observable through events |
| Paths | `/api/info.paths.tmp`; `opencode debug paths` (CLI) | |
| Active sessions | `GET /api/session/active` → `{data:{<sessionID>:{type:"running"}}}` (`server/src/handlers/session.ts:180-188`) | process-local |

### 2.4 Session/turn endpoint contract `[SES][TURN][INP][PERM]`

E2 `protocol/openapi.json` @527f0b93 (field `?` = optional). Errors are tagged JSON (§2.9). Session-scoped routes resolve location from the session.

| Operation | Method/path | Request | Response |
| --- | --- | --- | --- |
| create | `POST /api/session` | `{id?, title?, agent?, model?:{id,providerID,variant?}, location?:{directory}, metadata?, permissions?: [{action, resource, effect}]}` | `{data: Session.Info}` |
| list | `GET /api/session` | query `limit, order, search, parentID, directory, project, subpath, cursor` | `{data, cursor}` |
| get | `GET /api/session/{id}` | | `{data:{id,parentID?,fork?,projectID,agent?,model?,cost,tokens,outcome?,time,title?,subpath?,metadata?,permissions?,revert?,location}}` |
| update | `PATCH /api/session/{id}` | `{title?, metadata?, permissions?}` | 204; rules emit `session.permissions` |
| delete | `DELETE /api/session/{id}` | | deletes children too |
| switch agent/model | `POST /api/session/{id}/agent`, `POST /api/session/{id}/model` | `{agent}` / `{model:{id,providerID,variant?}}` | 204; next execution |
| prompt | `POST /api/session/{id}/prompt` | `{id?: "msg_…", text, files?: [{uri, name?, description?, mention?}], agents?: [{name}], skills?, metadata?, delivery?: "steer"|"queue", resume?: bool}` | **200 `{data: InboxItem{id, sessionID, time:{created}, type:"user", payload, delivery}}`** = durable admission; 409 if `id` collides |
| command | `POST /api/session/{id}/command` | `{name, text, files?, agents?, skills?, delivery?}` | 204 after the callback; 404 `CommandNotFoundError`, 500 `CommandExecutionError` |
| synthetic | `POST /api/session/{id}/synthetic` | `{id?, text, description?, metadata?, delivery?, resume?}` | inbox item |
| shell | `POST /api/session/{id}/shell` | `{id?, command}` | 204 |
| compact | `POST /api/session/{id}/compact` | `{id?, delivery?}` (object required) | inbox item `type:"compaction"` |
| interrupt (abort) | `POST /api/session/{id}/interrupt[?resume=true]` | no body | `{interrupted: bool}` — *"Interrupt active execution owned by this OpenCode process"* (`protocol/src/groups/session.ts:747-764`) |
| background | `POST /api/session/{id}/background` | | foreground backgroundable tools → background |
| wait (barrier) | `POST /api/experimental/session/{id}/wait` | | 204 when idle (experimental) |
| revert | `POST …/revert/stage`, `DELETE …/revert`, `POST …/revert/commit` | | `session.revert.*` events |
| fork | `POST /api/session/{id}/fork` | `{before?: "msg_…"}` (omit = full history) | `{data: Session.Info}`; `InvalidRequestError{kind:"empty_session"}` |
| context / diff | `GET …/context`, `GET …/diff` | | |
| messages | `GET …/message?limit&order&cursor&type`, `GET …/message/{messageID}` | | `{data:[…], cursor:{previous?,next?}}` (paginated) |
| inbox | `GET …/inbox`, `PATCH …/inbox/{inboxID}` `{delivery}`, `DELETE …/inbox/{inboxID}` | | pending admitted inputs |
| permission | `GET /api/permission/request`, `GET|POST /api/session/{id}/permission`, `GET …/permission/{rid}`, **`POST …/permission/{rid}/reply` `{decision:"once"|"always"|"reject", message?}`** | | 204 |
| forms (questions) | `GET /api/form`, `GET|POST /api/session/{id}/form`, `GET …/form/{fid}`, `POST …/form/{fid}/reply` `{answer:{…}}` (409 `FormAlreadySettledError`), `DELETE …/form/{fid}` | | |
| files | `GET /api/fs/read/*`, `GET /api/fs/list`, `GET /api/fs/find`, `POST /api/experimental/fs/write` | | |
| export/import | `GET /api/experimental/session/{id}/export`, `POST /api/experimental/session/import` | | experimental |
| share/unshare, todo, summarize, per-part get | **absent** | | |

### 2.5 Event subscription `[LIFE][OUT]`

- **Stream.** `GET /api/event` (auth required; LUNA notes the 2.0.3 server needed `Accept: text/event-stream`). Response `text/event-stream`, headers `Cache-Control: no-cache, no-transform`, `X-Accel-Buffering: no` (`server/src/handlers/event.ts:23-32`). Frames are **`data: <json>\n\n` only** — no `id:` and no `event:` lines (`server/src/event-feed.ts:29-31`), even though the OpenAPI response schema describes `{id, event, data}` objects. First frame is always `{"id":"evt_…","type":"server.connected","data":{}}` (`event.ts:14-20`). Heartbeat comment `: heartbeat\n\n` every **15 s** (`event.ts:22`).
- **Scope.** One stream per server process, carrying **every location and every session** (child/subagent sessions included) — *"Subscribe to native events and plugin RPC events across all server locations"* (`protocol/src/groups/event.ts:49-51`). No directory/session filter parameter; consumers filter by `data.sessionID` or `location`.
- **Envelope.** `{id: "evt_…", created: <ms>, type, data, location?: {directory}, metadata?, durable?: {aggregateID, seq, version}}` (`schema/src/event.ts:9-27,61-71,83-135`). Durable events carry a per-aggregate monotonically increasing `seq` (aggregate = session for `session.*`). Ephemeral events (deltas, usage, status, permission, forms, PTY…) have no `durable`. Plugin RPC events: `type: "rpc.<…>"` with required `location` (`protocol/src/groups/event.ts:14-21`).
- **Complete list.** 94 event types at 2.0.22 (92 manifest + `rpc.*` + `server.connected`), from `schema/src/event-manifest.ts:71-81` via the generated union `client/src/promise/generated/types.ts:2417` — full table with payload fields, durability and LUNA handling in **§3.2**. No `session.error`, `message.updated`, `message.part.*`, `question.*`, `todo.*` or `session.diff` events exist in v2.
- **Overflow.** Per-subscriber dropping queue of **4,096** frames; when full the subscriber is removed and its stream **fails** (`event-feed.ts:8,64-68`).

### 2.6 Reconnect/replay `[LIFE][TURN]`

- `/api/event` has **no replay**: no SSE `id:`, no `Last-Event-ID` handling, no `after` cursor; *"Volatile by contract: a slow consumer overflows and fails the stream, and events during disconnection are missed"* (`protocol/src/groups/event.ts:51`). The official client is also *"Live-only, with no replay or automatic reconnection"* (E3 docs/build/client).
- The **only replay path** is the experimental per-session durable log: `GET /api/experimental/session/{id}/log?after=<seq>&follow=true` — *"Reads events after an exclusive aggregate sequence and continues with live events when follow=true"* (`protocol/src/groups/session.ts:727-742`); items are `SessionEvent.Durable | EventLog.Synced`. It omits ephemeral `*.delta`, `tool.progress`, `usage.updated` and all non-session events (permissions, forms).
- State resync without the log: `GET /api/session/{id}` (`outcome`, `time.idle`), `GET /api/session/active`, `GET /api/session/{id}/message` (paginated, **default limit 50, default order `desc`**, `server/src/handlers/message.ts:9,41-46`), `GET /api/session/{id}/inbox`, `GET /api/permission/request`, `GET /api/form`.

### 2.7 Correlation and end-of-turn `[TURN][AGT]`

- **IDs.** Session `ses_…`; event `evt_…`; message/inbox `msg_…` (caller may supply `id` on prompt/synthetic/compact/shell for idempotency — 409 on collision, `server/src/handlers/session.ts:306-340`); assistant output items are keyed `(sessionID, assistantMessageID, ordinal)` for text/reasoning and `(sessionID, assistantMessageID, id)` for tools (`id` = provider call id); child sessions carry `parentID` in `session.created`; permission requests have `id` and `source:{type:"tool", messageID, id}`.
- **Turn = busy period.** `session.execution.started` opens it; exactly **one** terminal per busy period: `session.execution.succeeded | interrupted{reason: user|shutdown|superseded|inactivity} | failed{error}` — *"One terminal observation per busy period, covering every coalesced drain"* (`core/src/session/execution.ts` coordinator `started`/`settled`). Inputs admitted while busy are drained in the same busy period (steers at step boundaries; one queued input at the idle boundary — `core/src/session/inbox.ts:43-48,495-530`), so one terminal can cover several prompts.
- **Per-input correlation.** The prompt response returns the inbox item `id`; `session.inbox.delivered{inboxID}` marks when that input entered the conversation. The official ACP turn starts on `session.inbox.delivered` whose `inboxID == response.id` and ends on the next execution terminal for that session (`cli/src/acp/translate.ts:334-342,618-620`).
- **Steps.** `session.step.started{assistantMessageID, agent, model, started}` → items → `session.step.ended{finish: stop|length|tool-calls|content-filter|error|unknown, cost, tokens}` or `session.step.failed{error}`.
- `session.status` / `session.idle` are in the public manifest (`schema/src/session-status-event.ts`, `session.idle` marked `// deprecated`) but **no producer exists in v2.0.22 core** (repo-wide grep finds only schema, manifest and app test fixtures). Treat as legacy/plugin-only.

### 2.8 Delivery acknowledgement, queueing, steering `[TURN][INP]`

- **Accepted = admitted durably.** `POST …/prompt` returns 200 with the inbox item after a durable write (*"Durably admit one session input and schedule agent-loop execution unless resume is false"*). `session.inbox.enqueued{inboxID,item}` follows.
- **Incorporated.** `session.inbox.delivered{inboxID}` when promoted into history. `session.inbox.cancelled` / `DELETE …/inbox/{id}` withdraws a pending item; `PATCH …/inbox/{id} {delivery}` re-targets it (`session.inbox.delivery.changed`).
- **Native steering.** `delivery` defaults to **`"steer"`** (`core/src/session/prompt.ts:50`; also `session.ts:156,258,297`): a prompt sent while busy is injected at the **next step boundary of the running execution**. `"queue"` parks it until the idle boundary. `resume:false` admits without waking. `interrupt?resume=true` resumes pending steers/control items while queued prompts stay parked.
- `POST …/command` is synchronous (204 only after the command callback); not an admission ack.

### 2.9 Error envelopes `[TURN][MOD]`

- **HTTP.** Tagged JSON `{_tag, message, …}`: 400 `InvalidRequestError{kind?, field?}` (all schema rejections, via `server/src/middleware/schema-error.ts:14-20`), 401 `UnauthorizedError` (`middleware/authorization.ts:50-55`; `www-authenticate: Basic` only for non-browser challenges), 404 `SessionNotFoundError|MessageNotFoundError|PermissionNotFoundError|CommandNotFoundError|…`, 409 `ConflictError|FormAlreadySettledError`, 500 `UnknownError{ref?}|CommandExecutionError`, 503 `ServiceUnavailableError{service?}`; pre-ready 503 `{code:"service_starting"|"service_stopping"}` / `{code:"service_failed", message, action}` (`server/src/process.ts:216-230`).
- **Turn errors.** `SessionStructuredError = {type, message, status?, response?:{body}}`; types include `provider.rate-limit|auth|quota|content-filter|transport|internal|invalid-output|invalid-request|unsupported-operation|no-route|unknown|timeout`, `permission.rejected`, `tool.execution`, `aborted`, `unknown` (`core/src/session/to-session-error.ts:29-85`; `runner/step.ts:61-62`). Carried on `session.step.failed`, `session.tool.failed`, `session.execution.failed`, `session.compaction.failed`, `session.retry.scheduled`.
- **Retry.** Provider retries publish `session.retry.scheduled{assistantMessageID, attempt, at, error}` (`core/src/session/runner/retry.ts:104-110`); schedule is bounded: exponential from 2 s capped at 10 s per gap, `recurs(10)`, jittered, with a lower cap for timeout retries (`retry.ts:43-60`). v2 does **not** retry forever.
- No `session.error` event exists in v2.

### 2.10 Cancellation `[TURN][AGT]`

- `POST /api/session/{id}/interrupt` interrupts the execution **owned by the receiving process** and returns `{interrupted}` (false = idle no-op or another process owns it) (`protocol/src/groups/session.ts:747-764`). The running step ends with `session.step.failed{error:{type:"aborted"}}` (`runner/step.ts:61-62`), then `session.execution.interrupted{reason:"user"}`; user interrupts cancel the session's jobs and release the restart claim (`execution.ts` `settled`).
- **Children.** A foreground `subagent` tool call interrupts its child session and cancels its job when the parent step is interrupted (`core/src/tool/plugin/subagent.ts:233-240`). **Background** subagents (`background:true`) are not tied to the parent step; their completion is later delivered to the parent as `session.synthetic` input (`core/src/session/subagent-completion.ts`), which by default starts a **new** parent execution. Interrupting a child directly is a separate `interrupt` call on the child session.

### 2.11 Shutdown and disposal `[LIFE][WS]`

- `default` mode runs until signal under `NodeRuntime.runMain` (`cli/src/index.ts:147`); finalizers set status `stopping`, drop the app router and `closeAllConnections()` (`server/src/process.ts:86-91,155`). `--stdio` additionally exits on stdin EOF.
- Executions interrupted by shutdown get `reason:"shutdown"` and **keep their durable claim** (`execution.ts` comment: *"shutdown interruption … preserves the claim so the next server start resumes the turn"*). Only the **managed service** sweeps claims at boot (`server/src/process.ts:104-108,237-239`; `core/src/session/execution/restart.ts`: *"unregistered servers sharing the database never sweep"*; resume prompt *"The server restarted while you were working…"*, max 10 resumes per turn). The sweep *"assumes every orphaned claim's owner is dead"*.
- Locations can be disposed individually (`DELETE /api/debug/location`, `location.shutdown` event); there is no HTTP "shutdown server" endpoint except experimental persistent-PTY shutdown/handoff.

## 3. LUNA adapter comparison (LUNA `9b3e9a2e`)

All LUNA citations are `crates/harness/src/opencode/mod.rs:<line>` @`9b3e9a2e` unless noted. "2.0.3" = `d44b52ca`, the version the adapter header claims (`mod.rs:12-14`). Per-tag body checks (scratch `tagcheck.py`) show **every request-contract change below landed in v2.0.4** (audit commit `13453f2da8c` and siblings, 2026-09-13) and is unchanged through v2-head.

### 3.1 Endpoints LUNA calls (V2 branch)

| # | LUNA call (line) | LUNA payload | 2.0.3 | 2.0.22 / 2.0.24 / head | Wire outcome @2.0.22 | Sev | Tags |
| --- | --- | --- | --- | --- | --- | --- | --- |
| E1 | `GET /api/health` readiness+generation probe (`474-490`, spawn loop `576-597`) | — | `{healthy, version}` | **removed** (since 2.0.4) → empty 404 | probe falls to E2 | **S0** | LIFE |
| E2 | `GET /global/health` V1 probe (`476-477`) | — | web UI | web UI `index.html` 200 `text/html` | `resp.json()` fails → `detect()` = `None` every 150 ms until **300 s** timeout → `"opencode serve did not become healthy within 300s"` (`594`). Attached mode (`with_base_url`) instead defaults to **V1** (`508`) and speaks `/global/*` + `/session/*` to a v2 server. | **S0** | LIFE |
| E3 | `POST /api/session` (`2206-2226`) | `{location:{directory}, permission:[{permission, pattern, action}]}` | no permission field | `permissions?: [{action, resource, effect}]` | `permission` is not a v2 field → session created **without LUNA's rules** (E5: Effect Schema ignores excess keys by default; if strict it is a 400 and every run fails). Rule shape is also v1, so renaming alone would still 400. | **S1** | PERM, SES |
| E4 | `PATCH /api/session/{id}` on resume (`1628-1635`) | `{permission:[v1 rules]}` | **route absent** (resume failed at 2.0.3) | `{title?, metadata?, permissions?}` → 204 | rules silently dropped (E5 as E3) | S1 | PERM, SES |
| E5 | `GET /api/session/{id}` (`765-776`, resume `1617-1622`) | — | ok | ok (`{data: Session.Info}`) | works | — | SES |
| E6 | `GET /api/model` (`858-880`, struct `1378-1450`) | — | ok | ok (`id, providerID, name, limit, variants[].id, enabled` present) | works; 5×2 s empty-list retry | — | MOD |
| E7 | `GET /api/command` (`882-890`) | — | ok | ok | works | — | EXT |
| E8 | `POST /api/session/{id}/model` (`935-952`) | `{model:{providerID, id, variant?}}` | ok | ok | works | — | MOD |
| E9 | `POST /api/session/{id}/prompt` (`2478-2484`, body `2358-2372`) | `{text, files:[{uri:"file://…", name}]}` | ok | ok | works; **response (inbox item `id`, `delivery`) discarded**; no caller `id` (no idempotency), no `delivery` (server default `steer`) | S3 | TURN, INP |
| E10 | `POST /api/session/{id}/command` (`2427-2461`, detached task) | `{command, text}` | ok | requires **`name`** | **400 `InvalidRequestError`**; the detached task logs only transport errors (`2458`) → slash command silently never runs → turn sits until the 60 s stall watchdog errors it | **S1** | TURN, EXT |
| E11 | `POST /api/session/{id}/interrupt` (`908-921`) | JSON `null` | ok (`?continue`) | ok (`?resume`) | works (no payload schema; E5 that `null` body is ignored); `{interrupted:false}` not inspected | S3 | TURN |
| E12 | `POST /api/session/{id}/compact` (`923-933`, `1181`) | JSON **`null`** | object `{id?, delivery?}` | same | payload must be an object → E5 likely **400** at 2.0.3 and 2.0.22; response inbox id discarded | S2 | MOD, TURN |
| E13 | `GET /api/session/active` (`894-906`) | — | ok | ok (`{data:{<id>:{type:"running"}}}`) | works; process-local | — | TURN |
| E14 | `GET /api/session/{id}/message` (`778-791`) | no query | paginated | paginated, **limit 50, order desc** | reads only newest 50, newest-first; v2 items have no `info` wrapper | S1 (fork) | SES |
| E15 | `POST /api/session/{id}/fork` (`793-856`) | `{messageID}` or `{}` | needs `{boundary}` | `{before?}` | `messageID` ignored (E5) → full-history fork; then `opencode_history_signatures` demands `info` (`1029-1036`) → error **after** the fork was created (`809` MutationStarted) → orphan fork session; boundary positions computed on a desc 50-item page | **S1** | SES |
| E16 | `POST /api/session/{s}/permission/{id}/reply` (`2895-2897`, `2989-2994`) | `{reply:"once"|"always"|"reject"}` | ok | requires **`decision`** | **400**; V2 has no fallback path → request stays pending → tool blocks; turn hangs (stall watchdog only covers the first event) | **S0** | PERM, TURN |
| E17 | `GET /api/event` (`3935-3976`) | `Accept: text/event-stream`, Basic | ok | ok | works | — | LIFE |
| — | Spawn `opencode serve --port <free> --hostname 127.0.0.1`, env `OPENCODE_SERVER_PASSWORD=<uuid>`, `OPENCODE_CLIENT=luna`, stdout null (`515-569`) | | ok | ok (legacy env name still honored, `cli/src/env.ts:10-13`) | works; free-port probe is a TOCTOU race (`201-206`; `--port N` fails on conflict, `server/src/process.ts:140`) | S3 | LIFE |

Severity: **S0** = blocks every v2 run or every approval; **S1** = a whole feature broken or a safety setting silently not applied; **S2** = feature broken in a narrower path; **S3** = degraded/fidelity.

**Endpoints LUNA never calls** that matter at the wire level: `/api/info` (version), `form.*` (questions), `GET /api/permission/request` / `GET /api/form` (pending-state resync), `…/inbox*` (delivery ack/cancel), `/api/experimental/session/{id}/log` (replay), `/api/agent`, `/api/skill`, `/api/mcp`, `/api/config`, `/api/session/{id}/diff`, `…/revert/*`, `…/background`, `…/shell`.

### 3.2 Event-type table (wire handling)

Every v2.0.22 public event (generated union `client/src/promise/generated/types.ts:2417`; manifest `schema/src/event-manifest.ts:71-81`). D = durable (`durable.seq` present), E = ephemeral. LUNA path: `stream_bus` (`3978-4031`) → `normalize_v2_frame` (`3632-3851`) → 1.x-shaped payloads → `handle_bus_event` (`2556+`). Frames whose `data.sessionID` is absent/empty are dropped first (`3634-3640`); everything else not matched falls to `_ => Vec::new()` (`3849`). Unparseable JSON lines are skipped silently (`4009-4011`). **19 of 94 types are consumed; 75 are dropped.** "Unknown → dropped" applies equally to any future event type.

| v2 event | data fields (`?` optional) | D/E | LUNA wire handling | LUNA `mod.rs` line |
| --- | --- | --- | --- | --- |
| `location.shutdown` | — | E | dropped (no `data.sessionID`) | 3638 |
| `models-dev.refreshed` | — | E | dropped (no `data.sessionID`) | 3638 |
| `credential.updated` | — | E | dropped (no `data.sessionID`) | 3638 |
| `credential.switched` | integrationID, credentialID | E | dropped (no `data.sessionID`) | 3638 |
| `integration.updated` | — | E | dropped (no `data.sessionID`) | 3638 |
| `provider.updated` | — | E | dropped (no `data.sessionID`) | 3638 |
| `model.updated` | — | E | dropped (no `data.sessionID`) | 3638 |
| `agent.updated` | — | E | dropped (no `data.sessionID`) | 3638 |
| `session.created` | sessionID, projectID, location, subpath?, parentID?, slug, title?, agent?, model?, metadata?, permissions?, version | D | normalized → `session.created{info:{id,parentID,title|slug}}` (child binding) | 3818 |
| `session.agent.selected` | sessionID, agent, previous? | D | **dropped** (has sessionID; falls to `_ => Vec::new()`) | 3849 |
| `session.model.selected` | sessionID, model, previous? | D | **dropped** (has sessionID; falls to `_ => Vec::new()`) | 3849 |
| `session.moved` | sessionID, location, projectID, subpath? | D | **dropped** (has sessionID; falls to `_ => Vec::new()`) | 3849 |
| `session.renamed` | sessionID, title | D | **dropped** (has sessionID; falls to `_ => Vec::new()`) | 3849 |
| `session.metadata.updated` | sessionID, metadata | D | **dropped** (has sessionID; falls to `_ => Vec::new()`) | 3849 |
| `session.permissions` | sessionID, permissions | D | **dropped** (has sessionID; falls to `_ => Vec::new()`) | 3849 |
| `session.viewed` | sessionID, idle | D | **dropped** (has sessionID; falls to `_ => Vec::new()`) | 3849 |
| `session.usage.updated` | sessionID, cost, tokens | E | normalized → `message.updated{id:"usage", tokens}`; `cost` dropped | 3797 |
| `session.deleted` | sessionID | D | **dropped** (has sessionID; falls to `_ => Vec::new()`) | 3849 |
| `session.forked` | sessionID, parentID, boundary, instructions?, instructionEntries? | D | **dropped** (has sessionID; falls to `_ => Vec::new()`) | 3849 |
| `session.inbox.delivered` | sessionID, inboxID | D | **dropped** (has sessionID; falls to `_ => Vec::new()`) | 3849 |
| `session.inbox.enqueued` | sessionID, inboxID, item | D | **dropped** (has sessionID; falls to `_ => Vec::new()`) | 3849 |
| `session.inbox.cancelled` | sessionID, inboxID | D | **dropped** (has sessionID; falls to `_ => Vec::new()`) | 3849 |
| `session.inbox.delivery.changed` | sessionID, inboxID, delivery | D | **dropped** (has sessionID; falls to `_ => Vec::new()`) | 3849 |
| `session.execution.started` | sessionID | D | normalized → `session.status{busy}` | 3675 |
| `session.execution.succeeded` | sessionID | D | normalized → `session.idle` | 3683 |
| `session.execution.failed` | sessionID, error | D | normalized → `session.error{name:error.type, data.message}` + `session.idle`; `status`/`response` dropped | 3687 |
| `session.execution.interrupted` | sessionID, reason | D | normalized → `session.interrupted` (any `reason`; reason dropped) | 3679 |
| `session.instructions.updated` | sessionID, delta, text? | D | **dropped** (has sessionID; falls to `_ => Vec::new()`) | 3849 |
| `session.synthetic` | sessionID, text, description?, metadata? | D | **dropped** (has sessionID; falls to `_ => Vec::new()`) | 3849 |
| `session.skill.activated` | sessionID, id, name, text | D | **dropped** (has sessionID; falls to `_ => Vec::new()`) | 3849 |
| `session.shell.started` | sessionID, shell | D | **dropped** (has sessionID; falls to `_ => Vec::new()`) | 3849 |
| `session.shell.ended` | sessionID, shell, output | D | **dropped** (has sessionID; falls to `_ => Vec::new()`) | 3849 |
| `session.step.started` | sessionID, assistantMessageID, agent, model, snapshot?, started | D | normalized → `message.updated{role:assistant,id}`; agent/model/snapshot/started dropped | 3708 |
| `session.step.streamed` | sessionID, assistantMessageID | D | **dropped** (has sessionID; falls to `_ => Vec::new()`) | 3849 |
| `session.step.ended` | sessionID, assistantMessageID, finish, rawFinish?, providerState?, cost, tokens, snapshot?, files? | D | **dropped** (has sessionID; falls to `_ => Vec::new()`) | 3849 |
| `session.step.failed` | sessionID, assistantMessageID, error, finish?, rawFinish?, providerState?, cost?, tokens?, snapshot?, files? | D | `error.type=="aborted"` dropped; else → `session.warning` chip; cost/tokens/finish dropped | 3696 |
| `session.text.started` | sessionID, assistantMessageID, ordinal | D | normalized → `message.part.updated` text part, id `<msg>:t<ordinal>`, text "" | 3714 |
| `session.text.delta` | sessionID, assistantMessageID, ordinal, delta | E | normalized → `message.part.delta{field:text}` | 3721 |
| `session.text.ended` | sessionID, assistantMessageID, ordinal, text, state? | D | normalized → `message.part.updated` with full text; `state` dropped | 3714 |
| `session.reasoning.started` | sessionID, assistantMessageID, ordinal, state? | D | normalized → reasoning part (text ""); `state` dropped | 3731 |
| `session.reasoning.delta` | sessionID, assistantMessageID, ordinal, delta | E | normalized → `message.part.delta` on `<msg>:r<ordinal>` | 3721 |
| `session.reasoning.ended` | sessionID, assistantMessageID, ordinal, text, state? | D | normalized → reasoning part with full text | 3731 |
| `session.tool.input.started` | sessionID, assistantMessageID, id, name | D | normalized → tool part `pending`; name cached by (session,msg,id) | 3736 |
| `session.tool.input.delta` | sessionID, assistantMessageID, id, delta | E | **dropped** (has sessionID; falls to `_ => Vec::new()`) | 3849 |
| `session.tool.input.ended` | sessionID, assistantMessageID, id, text | D | **dropped** (has sessionID; falls to `_ => Vec::new()`) | 3849 |
| `session.tool.called` | sessionID, assistantMessageID, id, input, executed, state? | D | normalized → tool part `running` + input; `executed`/`state` dropped | 3747 |
| `session.tool.progress` | sessionID, assistantMessageID, id, metadata | E | **dropped** (has sessionID; falls to `_ => Vec::new()`) | 3849 |
| `session.tool.success` | sessionID, assistantMessageID, id, content, metadata?, executed, resultState? | D | normalized → `completed`, output = join of `content[].text`; file content, `metadata`, `resultState` dropped | 3759 |
| `session.tool.failed` | sessionID, assistantMessageID, id, error, content?, metadata?, executed, resultState? | D | normalized → `error` with `error.message` only; partial `content`/`metadata` dropped | 3780 |
| `session.retry.scheduled` | sessionID, assistantMessageID, attempt, at, error | D | **dropped** (has sessionID; falls to `_ => Vec::new()`) | 3849 |
| `session.compaction.started` | sessionID, reason, recent, inputID? | D | **dropped** (has sessionID; falls to `_ => Vec::new()`) | 3849 |
| `session.compaction.delta` | sessionID, text | E | **dropped** (has sessionID; falls to `_ => Vec::new()`) | 3849 |
| `session.compaction.ended` | sessionID, reason, model?, providerState?, providerContext?, text, recent, cost?, tokens? | D | **dropped** (has sessionID; falls to `_ => Vec::new()`) | 3849 |
| `session.compaction.failed` | sessionID, reason, error, inputID?, cost?, tokens? | D | **dropped** (has sessionID; falls to `_ => Vec::new()`) | 3849 |
| `session.revert.staged` | sessionID, revert | D | **dropped** (has sessionID; falls to `_ => Vec::new()`) | 3849 |
| `session.revert.cleared` | sessionID | D | **dropped** (has sessionID; falls to `_ => Vec::new()`) | 3849 |
| `session.revert.committed` | sessionID, to | D | **dropped** (has sessionID; falls to `_ => Vec::new()`) | 3849 |
| `filesystem.changed` | file, event | E | dropped (no `data.sessionID`) | 3638 |
| `reference.updated` | — | E | dropped (no `data.sessionID`) | 3638 |
| `permission.asked` | id, sessionID, action, resources, save?, metadata?, source?, message? | E | passed through as `permission.asked{properties:data}` — but consumer reads v1 field names (§3.3) | 3840 |
| `permission.replied` | sessionID, requestID, reply | E | **dropped** (has sessionID; falls to `_ => Vec::new()`) | 3849 |
| `plugin.updated` | — | E | dropped (no `data.sessionID`) | 3638 |
| `project.updated` | id, canonical, vcs?, name?, icon?, commands?, time, sandboxes | E | dropped (no `data.sessionID`) | 3638 |
| `worktree.updated` | projectID | E | dropped (no `data.sessionID`) | 3638 |
| `worktree.resolved` | projectID, directory, previous, adopted? | D | dropped (no `data.sessionID`) | 3638 |
| `command.updated` | — | E | dropped (no `data.sessionID`) | 3638 |
| `config.updated` | — | E | dropped (no `data.sessionID`) | 3638 |
| `skill.updated` | — | E | dropped (no `data.sessionID`) | 3638 |
| `pty.created` | info | E | dropped (no `data.sessionID`) | 3638 |
| `pty.updated` | info | E | dropped (no `data.sessionID`) | 3638 |
| `pty.exited` | id, exitCode | E | dropped (no `data.sessionID`) | 3638 |
| `pty.deleted` | id | E | dropped (no `data.sessionID`) | 3638 |
| `persistent-pty.added` | sessionID, terminal | E | **dropped** (has sessionID; falls to `_ => Vec::new()`) | 3849 |
| `persistent-pty.removed` | sessionID, ptyID | E | **dropped** (has sessionID; falls to `_ => Vec::new()`) | 3849 |
| `shell.created` | info | E | dropped (no `data.sessionID`) | 3638 |
| `shell.exited` | id, exit?, status | E | dropped (no `data.sessionID`) | 3638 |
| `shell.deleted` | id | E | dropped (no `data.sessionID`) | 3638 |
| `form.created` | form | E | dropped (no `data.sessionID`) | 3638 |
| `form.replied` | id, sessionID, answer | E | **dropped** (has sessionID; falls to `_ => Vec::new()`) | 3849 |
| `form.cancelled` | id, sessionID | E | **dropped** (has sessionID; falls to `_ => Vec::new()`) | 3849 |
| `websearch.updated` | — | E | dropped (no `data.sessionID`) | 3638 |
| `session.status` | sessionID, status | E | **dropped** (has sessionID; falls to `_ => Vec::new()`) | 3849 |
| `session.idle` | sessionID | E | **dropped** (has sessionID; falls to `_ => Vec::new()`) | 3849 |
| `tui.prompt.append` | text | E | dropped (no `data.sessionID`) | 3638 |
| `tui.command.execute` | command | E | dropped (no `data.sessionID`) | 3638 |
| `tui.toast.show` | title?, message, variant, duration? | E | dropped (no `data.sessionID`) | 3638 |
| `tui.session.select` | sessionID | E | **dropped** (has sessionID; falls to `_ => Vec::new()`) | 3849 |
| `installation.updated` | version | E | dropped (no `data.sessionID`) | 3638 |
| `installation.update-available` | version | E | dropped (no `data.sessionID`) | 3638 |
| `vcs.branch.updated` | branch? | E | dropped (no `data.sessionID`) | 3638 |
| `mcp.status.changed` | server | E | dropped (no `data.sessionID`) | 3638 |
| `mcp.resources.changed` | server | E | dropped (no `data.sessionID`) | 3638 |
| `rpc.*` | — | E | dropped (no `data.sessionID`) | 3638 |
| `server.connected` | — | E | dropped (no `data.sessionID`); only the first byte matters: `stream_bus` announces Connected on first chunk | 3638 (Connected: 3987-3991) |

What the most consequential drops mean (beyond the wire, noted only):
- `form.created|replied|cancelled` — v2 questions (the `question` tool, `core/src/tool/plugin/question.ts`) and MCP elicitations are forms; `form.created` has no top-level `sessionID` (`data.form.sessionID`). LUNA's `question.asked` handler (`3010+`) never fires on v2 → the agent waits forever on an unanswerable form. `[PERM]`
- `session.retry.scheduled` — the only v2 retry signal; LUNA's retry chip/abort policy keys on `session.status{type:"retry"}` (`2622-2656`), which v2 never emits. `[TURN]`
- `session.step.ended` — finish reason (`length`, `content-filter`), per-step cost/tokens, snapshot, changed `files` lost. `[OUT][MOD]`
- `session.inbox.*`, `session.synthetic` — no delivery acknowledgement; background-subagent completions arriving as synthetic input are invisible. `[TURN][AGT]`
- `session.compaction.*` — native auto/manual compaction progress, summary text and failure invisible. `[MOD]`
- `session.revert.*`, `session.forked`, `session.renamed`, `session.deleted`, `session.moved`, `session.agent.selected`, `session.model.selected`, `session.permissions` — session-state changes made by other clients or by tools (`session_rename`, `session_move`) are not observed. `[SES]`
- `session.shell.*`, `session.skill.activated`, `session.instructions.updated`, `session.tool.progress`, `session.tool.input.delta`, `permission.replied` (approval answered elsewhere → LUNA's prompt goes stale), `mcp.*`, `command.updated`, `agent.updated`, `config.updated`, `model.updated`/`provider.updated` (catalog staleness), `location.shutdown`, `installation.*`. `[EXT][PERM][LIFE]`

### 3.3 `normalize_v2_frame` rewrite losses (wire level) `[OUT][TURN][PERM][AGT]`

| Loss | Where | Effect |
| --- | --- | --- |
| Envelope dropped: `id` (evt), `created`, `durable.{aggregateID,seq,version}`, `location`, `metadata` | `3632-3640` (only `type`/`data` read) | no gap detection, ordering proof or dedupe; no replay cursor; no location attribution |
| `session.execution.interrupted.reason` (`user|shutdown|superseded|inactivity`) collapsed to `session.interrupted` | `3679-3682` | server-side shutdown/inactivity look like user interrupts |
| Structured error → `{name:type, data.message}`; `status`, `response.body` dropped; empty message falls back to type | `3857-3876` | HTTP status / provider body for auth/quota diagnosis lost |
| `session.step.failed{aborted}` dropped; other step failures downgraded to `session.warning` | `3696-3707` | correct for interrupts; a provider failure that the runner then retries is still shown as a chip (acceptable) |
| `session.step.started` → `message.updated{role:assistant}`; `agent`, `model`, `snapshot`, `started` dropped | `3708-3713` | effective agent/model per step not visible |
| text/reasoning snapshots: started → empty text; ended → full text; `state` (provider state/signatures) dropped; part id synthesized `<msgID>:t|r<ordinal>` | `3714-3735`, `3878-3901` | fine for display; ids are not v2 ids |
| tools: name only from `tool.input.started` (cache keyed by session/message/call id, cap 4,096 → **disconnect** if exceeded `4014-4017`); `called.executed/state`, `tool.input.ended.text`, `tool.progress` dropped; `success` keeps only `content[].text` joined by `\n` — `ToolFileContent` (images/files), `metadata` (diffs, exit codes, child `sessionID`), `resultState` dropped; `failed` keeps only `error.message`; matches a non-existent `session.tool.error` | `3736-3796`, `3903-3933` | tool output fidelity loss; subagent child id never reaches `register_spawn` (reads `state.metadata.sessionId`, `3388-3392`) |
| Tool vocabulary is v1: LUNA maps `bash`/`read`/`write`/`edit`/`todowrite`/`task` (`3543-3598`) and spawn detection keys on `task` (`3130,3313,3339`); v2 tools are `shell`, `subagent`, `patch`, `glob`, `grep`, `question`, `webfetch`, `websearch`, `skill`, `opencode`/`session_rename`/`session_move`/`models`, MCP resource tools (`core/src/tool/plugin/*.ts`, e.g. `shell.ts:22`, `subagent.ts:17`) | — | shell calls become `Unknown`; native subagents never bind as spawns (children only via `session.created.parentID`) |
| `session.usage.updated` → synthetic message `id:"usage"` with `tokens`; `cost` dropped; no provider/model → context window unknown (comment `3803-3806`) | `3797-3817` | cost/context gauges degraded |
| `session.created` → `{id, parentID, title|slug}`; `projectID`, `location`, `agent`, `model`, `permissions` dropped | `3818-3838` | |
| `permission.asked` passed through, but consumer reads v1 names `permission`, `patterns`, `messageID` (`2906-2920`, `2936-2938`); v2 sends `action`, `resources[]`, `save?`, `source{messageID,id}`, `message?` | `3840-3848` | prompt text degrades to "OpenCode requests tool permission"; `action_kind` always `Tool`; `turn_id` None; then the reply 400s (E16) |

Example (redacted) of the v2 frame vs what LUNA builds:

```json
// v2.0.22 wire (schema: client/src/promise/generated/types.ts PermissionAsked)
{"id":"evt_…","created":1759700000000,"type":"permission.asked",
 "data":{"id":"per_…","sessionID":"ses_…","action":"shell","resources":["git push"],
         "source":{"type":"tool","messageID":"msg_…","id":"call_…"}}}
// LUNA reply (mod.rs:2989-2994) → 400 InvalidRequestError at ≥2.0.4
POST /api/session/ses_…/permission/per_…/reply   {"reply":"once"}
// required by 2.0.4+
POST /api/session/ses_…/permission/per_…/reply   {"decision":"once"}
```

### 3.4 Detection, auth, startup, reconnect, stall/retry, multi-directory, ownership, shutdown `[LIFE][TURN][PERM][WS]`

- **v1/v2 detection — broken for every v2 release since 2.0.4 (S0).** `Protocol::detect` (`474-490`) accepts a generation only from a 2xx JSON body containing a string `version`, trying `/api/health` then `/global/health`. On ≥2.0.4 both fail (404 / HTML), so a spawned server never "becomes healthy" and the run errors after `DEFAULT_STARTUP_TIMEOUT` = **300 s** (`66`, `576-597`); `probe_models`/`probe_commands` (`283-319`) block the same 300 s. With an attached base URL the cell defaults to **V1** (`508`), mis-speaking the v1 wire to a v2 server. The probe also cannot distinguish "booting" (v2 answers `/api/info` 503 + `retry-after` with a version during boot) from "wrong generation". A robust probe is `GET /api/info` (any status with a JSON `version` = 2.x; 200 = ready) then `GET /global/health` (1.x). Dependence on the literal binary version string is avoidable (§5).
- **Auth.** Basic `opencode:<uuid>` via `OPENCODE_SERVER_PASSWORD` works (legacy name still read, `cli/src/env.ts:10-13`). **Leak (E5, S1 PERM):** in `default` serve mode the password stays in the server's environment and every tool process inherits it (`core/src/environment/exec-defaults.ts:83-84` `extendEnv: true`); only `--stdio` deletes it (`cli/src/server-process.ts:74-77`, comment *"Keep the lease credential out of the environment inherited by tools"*). An agent shell command can therefore call `POST …/permission/{id}/reply` on its own server and bypass LUNA approvals. Same exposure on the 1.x path (not re-checked).
- **Startup timeout.** 300 s (env `LUNA_OPENCODE_STARTUP_TIMEOUT_SECS`), 150 ms poll, 2 s per-probe timeout (`66-70`, `617-624`). A dead child fails fast via `try_wait` (`577-587`). With a correct probe, `--stdio`'s ready line removes the polling and the free-port race entirely.
- **Bus reconnect / replay.** 40 attempts × 250 ms ≈ 10 s, counter reset on each successful connect (`81-82`, `3935-3976`); then `BusMsg::Disconnected` errors the run (`2077-2104`). On reconnect LUNA re-syncs only turn liveness via `GET /api/session/active` (`2063-2075`). **No replay**: frames missed during the gap or dropped by server overflow (4,096-frame queue → stream failure, `server/src/event-feed.ts:64-68`) silently vanish from the transcript; LUNA cannot detect the gap because it discards `durable.seq` (§3.3). No read-idle timeout although v2 heartbeats every 15 s. The first-prompt gate waits ≤15 s for the first SSE chunk (`1760-1780`), which v2 satisfies immediately with `server.connected`.
- **Stall/retry.** Stall watchdog: 60 s from prompt to the first session-scoped event (`90-116`, `1926-1938`, `2029-2054`), satisfied on v2 by `execution.started`; it does **not** cover a turn that later blocks (unanswered form, 400'd permission reply). Retry policy (report at attempt 3, abort at 8, `86-87`, `2624-2656`) keys on `session.status{retry}`, which **v2 never emits** → dead code on v2; v2 retries are bounded server-side (≤10, `core/src/session/runner/retry.ts:45-48`) and visible only as `session.retry.scheduled` (dropped). Interrupt grace 2 s, abort call bounded 5 s (`1942-1972`).
- **Turn end.** LUNA ends a turn on the first execution terminal for its session after busy (`2599-2618`, `idle_ready`), uncorrelated with the prompt's inbox id. It serializes its own sends (steers queued client-side until idle, `1983-2027`; `RequireActiveTurn` steers rejected, `1986-1993`), so this mostly holds; it breaks when other inputs share the busy period (background-subagent `session.synthetic`, other clients, admitted-but-coalesced inputs — one terminal per busy period, §2.7) or when a new execution starts after LUNA settled (unprompted parent execution after a background subagent finishes).
- **Multi-directory / process model.** One `opencode serve` per LUNA run, fork, compaction and discovery probe (`273-281`, `283-319`, `362-381`, `406-418`), each with `current_dir = cwd` and every request scoped by `x-opencode-directory` (percent-encoded, `743-755`, `1195-1206`; server decodes it, `server/src/location.ts:39-47`). Discovery probes pass no directory → the location is the server's inherited cwd (for a launchd LUNA, `/`), so project-scoped models/commands are not discovered, and commands are cached once process-wide (`220`, `399-404`). All these servers share one SQLite DB (`cli/src/server-process.ts:100-102`). v2 is designed for one process serving many locations (§2.1); per-run processes multiply cold starts (plugins/MCP) and make `interrupt`/`session/active` process-local answers relative to the right process only by construction.
- **Ownership and shutdown.** SIGTERM, then SIGKILL after `kill_grace` 3 s (`lib.rs:607-631`); `kill_on_drop(true)` (`539`). No stdin lease: if LUNA dies without dropping the child, a `default`-mode server runs forever (`cli/src/server-process.ts:169` `Effect.never`). SIGTERM during a turn ends it as `interrupted{reason:"shutdown"}` and **keeps the restart claim**; the user's next **managed service** boot (the default target of `opencode`/TUI/`run`) sweeps claims from the shared DB and **resumes LUNA's turn** with *"The server restarted while you were working…"* (`core/src/session/execution/restart.ts`, `server/src/process.ts:237-239`) — model/tool work LUNA never observes. Because the sweep *"assumes every orphaned claim's owner is dead"*, a managed service started while a LUNA-owned server is mid-turn may resume the same session concurrently (E5; needs live confirmation). LUNA should `interrupt` (reason `user`, releases the claim) before SIGTERM.

### 3.5 Version drift 2.0.3 → 2.0.22 → 2.0.24 → v2-head `[LIFE]`

Method: `git show <sha>:packages/protocol/openapi.json` op/schema diffs, generated-client event-union diffs (`events-<sha>.tsv`), `git diff --stat` of `packages/{server,protocol,schema}/src` (scratch `B/`). 2.0.4 tag `466b3e594d`.

| Change | First version | 2.0.22 | 2.0.24 | head | Impact on LUNA |
| --- | --- | --- | --- | --- | --- |
| `GET /api/health` removed; `GET /api/info` `{version,pid,urls,paths}` added (`13453f2da8c`) | 2.0.4 | ✓ | ✓ | ✓ | **breaks boot** (E1/E2) |
| permission reply `reply` → `decision` | 2.0.4 | ✓ | ✓ | ✓ | **breaks approvals** (E16) |
| command body `command` → `name` | 2.0.4 | ✓ | ✓ | ✓ | **breaks slash commands** (E10) |
| fork body `{boundary}` → `{before?}` | 2.0.4 | ✓ | ✓ | ✓ | LUNA's `{messageID}` matched neither; full-history fork (E15) |
| `PATCH /api/session/{id}` added `{title?, permissions?}` (+`metadata?` 2.0.15); create gains `permissions?` (Ruleset `{action,resource,effect}`) | 2.0.4 | ✓ | ✓ | ✓ | resume no longer 404s; LUNA's `permission` key still ignored (E3/E4) |
| interrupt query `continue` → `resume` | 2.0.4 | ✓ | ✓ | ✓ | none (unused) |
| inbox item `timeCreated` → `time.created`; location wrappers reduced to `{directory}`; `/api/project/current` removed | 2.0.4 | ✓ | ✓ | ✓ | none (bodies ignored/unused) |
| events: `catalog.updated` removed; `provider.updated`, `model.updated`, `location.shutdown`, `session.metadata.updated` added; `session.permissions.updated` → `session.permissions`; `session.step.started` gains `started` | ≤2.0.22 | ✓ | ✓ | ✓ | none (all dropped either way) |
| `/api/info` gains `capabilities?:{persistentPty?}`; session create gains `parentID?` and 404 `LocationNotFoundError` for a missing directory (`server/src/location.ts` `locationErrors`); `StructuredError.response?` in OpenAPI; pairing/credential/`vcs/init` routes added | 2.0.23–2.0.24 | — | ✓ | ✓ | additive; a deleted run directory now fails create with 404 instead of creating |
| 2.0.24 → v2-head (`4a0f273f`, 31 commits) | — | | | | **no OpenAPI or event-union change**; 8 lines in `schema/{agent,model}.ts`, `server/{routes,server-info}.ts` |

Conclusion: the adapter's "verified against 2.0.3" pin is the **last release before the v2 API audit**; 2.0.4 through v2-head all break LUNA in the same three hard ways (detection, permission reply, command) plus the silent ones (permission rules, compaction body, fork, forms, retries). Nothing between 2.0.22 and v2-head adds a new LUNA breaker. Live confirmation on 2.0.22 was not performed (no model calls/sessions allowed); the earliest E4 hint is `docs/qa/backend-review-2026-10-03.md:97-100`, which records OpenCode `2.0.14` installed but live single-turn passes only for Codex and Claude.

## 4. Live metadata probe

**`opencode serve` probe: skipped (rule-driven).** The rule requires disabling plugin/npm installation and auto-update with env flags verified in source. v2.0.22 has `OPENCODE_DISABLE_AUTOUPDATE`, `OPENCODE_DISABLE_MODELS_FETCH` and `OPENCODE_DISABLE_PROJECT_CONFIG`, but **no flag that disables npm installation** (full `OPENCODE_*` inventory of `core/cli/server/util` src), while npm install paths are reachable from provider SDK loading, formatter plugins and plugin modules (`core/src/provider.ts:107`, `core/src/plugin/provider/dynamic.ts:9`, `core/src/config/plugin/formatter.ts:23`, `core/src/plugin/module.ts:86`). I could not prove a fresh boot plus `GET /api/model` installs nothing, so no server was started.

**Read-only `--help` (E1, 2026-10-06).** Run with temporary `XDG_{CONFIG,DATA,CACHE,STATE}_HOME` under `scratchpad/B/xdg/`, cwd in scratch, `OPENCODE_DISABLE_AUTOUPDATE=1`:

```
$ env XDG_CONFIG_HOME=…/B/xdg/config XDG_DATA_HOME=…/B/xdg/data XDG_CACHE_HOME=…/B/xdg/cache \
      XDG_STATE_HOME=…/B/xdg/state OPENCODE_DISABLE_AUTOUPDATE=1 timeout 20 ~/.opencode/bin/opencode serve --help
DESCRIPTION
  Start the v2 API and web server
FLAGS
  --hostname string
  --port integer
  --cors string        Additional allowed CORS origin (repeat for multiple origins)
  --service
  --stdio
GLOBAL FLAGS  --help --version --wizard --completions --log-level --print-logs ("server logs require --standalone")
exit=0
```

Its only side effect was `xdg/data/opencode/log/opencode.log` in scratch. No `opencode serve` process left behind (`pgrep -fl 'opencode serve'` empty). This confirms the 2.0.22 flags at E1. It also confirms the log path that LUNA's error hint uses (`mod.rs:1224-1235`).

**Unverified because of the skip:** live `/api/health` → 404 and `/global/health` → HTML on 2.0.22 (E2 only, `cli/src/services/web-ui.ts:17-54` + OpenAPI absence); live `/api/info` and `/openapi.json` bodies; the boot-time 503 sequence.

## 5. Transport recommendation and boot compatibility contract `[LIFE]`

**Keep native HTTP + SSE. Do not switch to ACP, the SDK or `run --format json`.** The evidence:
- ACP in v2 is a translation layer that starts its own private HTTP server and consumes the same `/api/event` (`cli/src/commands/handlers/acp.ts:16-18`). It can only carry a subset of the HTTP contract, and *"A session runs one prompt at a time"* (E3 cli/acp). LUNA's original reasons (child traffic, reasoning, error fidelity) still favor HTTP. The correlation reason does not: v2 ACP does correlate turns.
- `@opencode/sdk` embeds the server in a JS process, and `@opencode/client` is TypeScript only. Either would add a Bun/Node sidecar without adding any wire capability.
- `run --format json` is a lossy one-shot projection with no approvals or forms.

**Fix the HTTP contract instead.** In priority order:
1. **Readiness and generation:** probe `GET /api/info`. A JSON `version` at any status means 2.x; 200 means ready, and 503 with `retry-after` means keep waiting. Only then fall back to `GET /global/health` for 1.x. *(S0)*
2. **Launch with `serve --stdio --port 0`.** Pass `OPENCODE_PASSWORD` in env, pipe stdin as the ownership lease, and use the stdout `{"url"}` line as readiness (`cli/src/services/standalone.ts:18-33`). This removes the free-port race and the health polling. It also removes the password leak to tool processes (`server-process.ts:74-77`), and the server dies on LUNA crash because the OS closes the pipe. Keep the SIGTERM → SIGKILL 3 s escalation, matching standalone. **Call `interrupt` before SIGTERM** whenever a turn is active, so the restart claim is released and a managed service never resumes LUNA's turn.
3. **Fix request bodies:** permission reply `{"decision"}` *(S0)*, command `{"name"}`, session `permissions` as a v2 Ruleset `[{action, resource, effect}]` using v2 action names (`shell`, not `bash`), compact `{}`, fork `{"before"}`. Check every non-2xx in the command path.
4. **Consume v2 events natively, or at least extend the normalizer:**
   - `form.*`: route to LUNA's existing question/form wizard and reply via `POST …/form/{id}/reply {answer}`.
   - `session.retry.scheduled`: route to the existing retry chip. The abort policy is optional, because v2 caps retries at 10.
   - `session.step.ended`: finish reason, cost and tokens.
   - `session.inbox.*` and `session.synthetic`.
   - `session.compaction.*`.
   - v2 `permission.asked` fields.
   - Tool `content` file items and `metadata`, including the child `sessionID` for `subagent`.
   - Log unknown event types instead of dropping them silently.
5. **Correlate turns as the official ACP does.** Send a caller `id: "msg_…"` with each prompt for idempotent retry, keep the response inbox `id`, start the turn on `session.inbox.delivered{inboxID}`, and end it on the next execution terminal. Send an explicit `delivery`: `"queue"` keeps today's turn-boundary semantics, while `"steer"` would let LUNA offer native in-turn steering in its existing composer/queue surfaces.
6. **Replay and resync.** Keep `durable.seq` per session to detect gaps. After a reconnect, either read `GET /api/experimental/session/{id}/log?after=<lastSeq>` (experimental; feature-gate it) or refetch messages with cursor paging (`limit` plus `cursor`, oldest-first via `order=asc`). Also re-read `GET /api/permission/request` and `GET /api/form` for pending prompts.
7. **Optional, measure first:** one long-lived server per LUNA process serving all directories through `x-opencode-directory`, instead of one per run or probe. v2 is built for this (§2.1). It cuts cold starts and makes `interrupt`, `session/active` and the bus process-consistent. The trade-off is weaker per-run failure isolation.

**Versioned compatibility contract to check at boot.** These checks are cheap and authenticated.
1. `GET /api/info`: require `version` to parse as semver `2.x`. Record `version`, `pid` and `capabilities` (2.0.24+). Classify the version as *known-good*, *known-bad* (2.0.0–2.0.3 with the current code), or *untested*. For untested versions, proceed in degraded mode with a visible notice.
2. `GET /openapi.json` (served live, `server/src/routes.ts:176`; auth required): assert that the operation IDs LUNA depends on exist. Assert that their request schemas have the fields LUNA sends, with the right `required` sets:
   - `server.info`
   - `event.subscribe`
   - `session.create` (`permissions` items `{action, resource, effect}`)
   - `session.prompt` (`text`, `files[].uri`, `id`, `delivery`)
   - `session.permission.reply` (`decision`)
   - `session.form.reply` (`answer`)
   - `session.command` (`name`)
   - `session.compact` (object)
   - `session.fork` (`before`)
   - `session.interrupt`
   - `session.switchModel`
   - `session.active`
   - `session.message.list` (`cursor`, `order`)
   - `model.list`
   - `command.list`

   Fingerprint that subset (e.g. SHA-256 of the canonicalized schemas) and compare it against the fingerprint pinned for the verified range. Optional capability: `session.log`.
3. **Event vocabulary:** OpenAPI types events only as a JSON string (`V2EventEncoded`), so the contract cannot be read from the spec. Pin the expected event-type set per verified range (94 types at 2.0.22–head; scratch `events-*.tsv`). At runtime, count and surface first-seen unknown types and decode failures (today `mod.rs:4009-4011` and `3849` drop them silently).
4. On a mismatch, fail loudly with the specific missing operation or field before the first prompt. Never time out after 300 s.

## 6. Ranked findings and unresolved questions

**LUNA wire gaps, by severity.** IDs refer to §3.1; all are E2 for OpenCode and E2 for LUNA, with E5 consequences unless stated otherwise.

| Rank | Finding | Sev | Tags |
| --- | --- | --- | --- |
| 1 | Generation/readiness probe uses the removed `/api/health`. Every v2 ≥2.0.4 spawn times out after 300 s, discovery probes block for the same time, and attached mode mis-detects V1 (E1/E2, §3.4). | S0 | LIFE |
| 2 | Permission reply sends `{reply}`; ≥2.0.4 needs `{decision}`. The reply returns 400 with no fallback, the tool blocks, and the turn hangs (E16). | S0 | PERM, TURN |
| 3 | v2 questions (`form.*`) are dropped (no top-level `sessionID`), so the agent waits forever. | S1 | PERM, TURN |
| 4 | LUNA's permission rules are never applied on v2: key `permission` vs `permissions`, v1 rule shape, v1 action names (E3/E4). | S1 | PERM |
| 5 | Server password is inherited by agent tool processes in `default` serve mode, so an agent can self-approve over the API. | S1 | PERM, LIFE |
| 6 | Slash commands send `{command}`; ≥2.0.4 needs `{name}`. The 400 is ignored and the turn stalls (E10). | S1 | TURN, EXT |
| 7 | Fork is broken: wrong body key, 50-item newest-first page, v1 `info` shape. It fails after creating an orphan fork (E14/E15). | S1 | SES |
| 8 | SIGTERM mid-turn leaves a restart claim, which the user's managed service later resumes. Concurrent resume is possible (E5, unverified). | S1 | LIFE, TURN |
| 9 | No replay and no gap detection. Reconnect or overflow silently loses frames; only liveness is resynced. | S2 | LIFE, OUT |
| 10 | Retry signal mismatch (`session.retry.scheduled` dropped, `session.status` never emitted): no retry chip, and the abort policy is dead code. | S2 | TURN |
| 11 | Compaction posts a JSON `null` body; an object is required (likely 400, E12). | S2 | MOD |
| 12 | Tool vocabulary is v1 (`bash`/`task`/`todowrite`), so `shell` → Unknown and `subagent` spawns don't bind. Tool output keeps only text, and child id and metadata are dropped. | S2 | OUT, AGT, TOOL |
| 13 | Turn end is uncorrelated with the prompt (inbox id discarded). A coalesced busy period or an unprompted parent execution after a background subagent breaks the 1:1 assumption. | S2 | TURN, AGT |
| 14 | Losses: `step.ended` finish/cost dropped, usage cost/context dropped, interrupt reason collapsed, error status/body dropped, session-state events dropped, `permission.replied` dropped (stale prompts). | S3 | OUT, MOD, SES, PERM |
| 15 | Process model: one server per run/probe, discovery in the app's cwd (`/` under launchd), commands cached process-wide, free-port TOCTOU, no stdin lease. | S3 | LIFE, WS, EXT |

**Version-drift breakers.** Every hard breaker (1, 2, 6) and every silent body mismatch (4, 7) arrived in **v2.0.4** and persists unchanged through 2.0.22, 2.0.24 and v2-head. 2.0.23–2.0.24 add only additive fields and routes, plus a 404 `LocationNotFoundError` on create for a missing directory. 2.0.24 → v2-head has no OpenAPI or event-union change.

**Unresolved questions:**
1. **Request bodies with unknown keys:** does Effect HttpApi ignore excess body properties (assumed, E5) or reject them? This decides whether E3/E4/E15 are silent drops or 400s that fail every session create. It needs a POST probe, which this research did not allow.
2. **Null bodies:** does a JSON `null` body 400 on `compact` (E5: yes), and is it ignored on `interrupt` (E5: yes)?
3. **Live confirmation:** the 2.0.22 `/api/health` 404, `/global/health` HTML and 300 s timeout behavior (the probe was skipped, §4). No retained LUNA evidence shows any successful v2 turn after 2.0.3.
4. **Concurrent resume:** can a managed-service boot resume a session whose LUNA-owned server is still live? The source comment says the sweep assumes dead owners.
5. **Permissions without LUNA's rules:** which v2 default permissions apply once LUNA's rules are dropped? Does `.env` reading ask? (Workstream A/C.)
6. **Engine behavior:** how LUNA's engine reacts to executions that start after settle, and to children bound only via `session.created` (workstream C).
7. **Session log endpoint:** stability and cost of `GET /api/experimental/session/{id}/log` as the replay path, and whether a stable equivalent is planned.
