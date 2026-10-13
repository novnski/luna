# IC-0: shared artifact, provider and Find contracts

Baseline: `3c959fb3` (main, 13 October 2026). Live status belongs to
`pending_tasks.py`. This ticket ships contracts, read-only probes and
feasibility fixtures. It ships no runtime feature: the native prototype only
compiles with the `browser-fixture` feature.

Read with the package [design](../design.md), [implementation](../implementation.md)
and [verification](../verification.md) documents. It consumes
[CORE-01](../../extensible-core/core-01/contract.md) (operation rows default to
`localOnly`; the unknown-id rule) and the [SW-00 media boundary](../../luna-only-workflows/sw-00/contracts.md#shared-media-and-artifact-references-ic-0-boundary).

**Verification state.** Every statement marked *predicted* comes from reading
installed files on disk: the retained Codex 0.160.0 generated schema
(`target/qa/luna-only-workflows/sw00-20261011T151529Z/codex-schema-0.160.0`),
the Claude 2.1.293 binary's control-request schema text, and the OpenCode
binary's route table. Nothing here was executed while writing it. The probe
and fixtures [below](#commands-and-evidence) turn each prediction into
evidence. Until they run, every *predicted* row is unverified.

## Deliverables

| Path | Purpose |
| --- | --- |
| [`scripts/qa-ic0-provider-probe.py`](../../../../scripts/qa-ic0-provider-probe.py) | `wire`: read-only probe of all three installed providers. |
| [`scripts/fixtures/ic0-mcp-app-server.py`](../../../../scripts/fixtures/ic0-mcp-app-server.py) | A disposable stdio MCP Apps server. It declares one `ui://` view, and its operation log is the server-side oracle. |
| [`crates/ui/src/browser/isolated.rs`](../../../../crates/ui/src/browser/isolated.rs) | `prototype`: the isolated document view (macOS, `browser-fixture` only). |
| [`crates/ui/examples/isolated-document-fixture.rs`](../../../../crates/ui/examples/isolated-document-fixture.rs) | `prototype`: native isolation, clipping, focus, scroll and lifecycle probes. |
| [`crates/ui/examples/ic0-find-feasibility.rs`](../../../../crates/ui/examples/ic0-find-feasibility.rs) | `prototype`: complete-history Find feasibility and failure probes. |

Shared hunks: `pub mod isolated` (cfg-gated) in `crates/ui/src/browser/mod.rs`,
and two `[[example]]` entries in `crates/ui/Cargo.toml`. No dependency was added.

## 1. Wire: provider schemas and live operations (`wire`)

Installed on disk: Codex `0.160.0` (Homebrew cask; LUNA's validated pin is
0.154.0), Claude Code `2.1.293`, and OpenCode at `~/.opencode/bin/opencode`
(SW-00 recorded `2.0.24`). MCP Apps vocabulary follows the 2026-01-26 stable
specification (SEP-1865): `ui://` resources, MIME `text/html;profile=mcp-app`,
and the client extension `io.modelcontextprotocol/ui`.

### HTML tools (`html_preview`, `html_render`)

All three providers receive LUNA tools through one owner, the private loopback
`luna` MCP endpoint in `crates/harness/src/codex/luna_mcp.rs`:

- Codex attaches it on every app-server spawn, including resumed threads.
- Claude attaches it through `--mcp-config` (`claude/luna_tools.rs`).
- OpenCode attaches it through `OPENCODE_CONFIG_CONTENT` (`opencode/luna_tools.rs`).

New tools register in the existing runtime catalog, `CrossTaskTools::catalog`
in `crates/engine/src/cross_task_tools.rs`. No provider-specific registration
is needed.

**Gap (all providers, IC-2).** The endpoint encodes every result as one text
block: `tool_result` builds `[{type:"text"}]`, and `dispatch` serializes with
`value.to_string()`. Codex dynamic tools likewise use `inputText` only. An
`html_preview` screenshot therefore cannot reach any provider as an image
today. IC-2 must emit an MCP `image` content item, plus `inputImage` on the
Codex dynamic path.

Each provider's own MCP client handling of image content (*predicted*;
probe facts `toolCallMutation.contentTypes`):

| Provider | How it handles an `image` item |
| --- | --- |
| Codex | Passes untyped `content` items through. |
| Claude | Runs "the same processing as model-turn MCP calls". |
| OpenCode | Maps image content to `file` parts. This can only be checked in a live H01 turn: OpenCode has no host-side tool call. |

### MCP Apps operations

| Capability | Codex 0.160.0 (*predicted*) | Claude 2.1.293 (*predicted*) | OpenCode (*predicted*) |
| --- | --- | --- | --- |
| App metadata on a call | `mcpToolCall.mcpAppUi {resourceUri, preferredModelDisplayMode}`, legacy `mcpAppResourceUri`, `appContext`. LUNA already keeps `mcpAppUi` in the journal envelope (`codex/normalize.rs` `mcp_output`). | `mcp_status` tools carry `_meta.ui` when `system/init.capabilities` has `mcp_tool_ui_meta_v1` (parsed today in `claude/mcp.rs`). | `GET /api/mcp` returns names and status. Whether tool `_meta` is exposed is unverified (probe fact `mentionsUiMeta`). |
| Live resource read | `mcpServer/resource/read {server, uri, threadId, originCallId}` → `{contents, originCallId}`, on the thread's own connection. | Control request `mcp_read_resource {serverName, uri}` on the run process (`ui://` only; advertised as `mcp_read_resource_v1`). | **Unsupported.** No host route. `GET /api/mcp/resource` lists a catalog only. The model-only tool `opencode_read_mcp_resource` is permission-gated and is not a host bridge. |
| Live tool call | `mcpServer/tool/call {server, threadId, tool, arguments, _meta}` → `{content, structuredContent, isError, _meta}`. | Control request `mcp_call {tool: "mcp__<server>__<tool>", arguments}`. **It runs no permission check** (the CLI trusts its control channel), so LUNA admission must come before every call. | **Unsupported.** No host route. |
| UI extension negotiation | `initialize.capabilities.extensions` exists. The binary contains no `io.modelcontextprotocol/ui` literal, so forwarding to servers is unverified (probe fact `fixtureNegotiation`). | The binary contains `io.modelcontextprotocol/ui` and `text/html;profile=mcp-app`. What it declares to servers is unverified (`fixtureNegotiation`). | Unverified (`fixtureNegotiation`). |
| App → model context | **Unsupported.** No app-server method. | **Unsupported.** No control subtype. | **Unsupported.** |
| App → chat message | Only an ordinary `turn/start` or `turn/steer`, which counts as a user message, not an app-attributed one. IC-4 must label it and confirm it. | Only an ordinary user line. | Only an ordinary prompt. |
| Rendering | LUNA has none: `ProviderMcpAppResourceView.rendered` is always false. | Same. | Same. |
| LUNA adapter today | No `mcp_control`; none of these methods is called. | `ReadAppResource` runs in an **administrative** process (`claude/mcp.rs` `read_app_resource`), never the run's own session. IC-4 must use the run's control channel. | `mcp_control` refuses `ReadAppResource` (`opencode/mod.rs`). |

The fixed outcome for OpenCode is **Unsupported provider**: *"OpenCode exposes
no host operation to read an MCP resource or call an MCP tool in a live
session."* Normal tool output stays unchanged. Revisit this only when the
probe's `openapi.mcpRoutes` changes.

## 2. Shared reference, storage and lifetime (`shared`)

These decisions are settled with SW-00 and SW-09, and CORE-14 versions them.
IC-1 implements them. SW-09 consumes them and adds no store, transport or
reference of its own.

### Identity and the synced reference

- **One reference type.** An additive, optional `artifactRef` object sits on
  the existing completed **tool** part. It is not a new part kind.
  - Older desktop readers ignore it: `DocPartJson` (`crates/doc/src/schema.rs`)
    has no `deny_unknown_fields`, so they render the ordinary tool chip and its
    `output` summary.
  - iOS (`SessionStore.swift`) and edge (`render-parts.ts`) read only the keys
    they know.
  - A new part kind would degrade badly. Old desktop builds map an unknown kind
    to prose from `text`, and iOS drops it.
- **Who writes it.** The engine stamps it from the artifact service's commit
  receipt, the same way it stamps `output_ref`. It is never parsed from tool
  output text. Only the owning host's fold writes tool parts.
- **Shape** (synced, bounded, no secrets):

  ```json
  "artifactRef": {"v": 1, "id": "art_<32 hex>", "kind": "luna.ic/html-document",
    "host": "<deviceId>", "digest": "sha256:<64 hex>", "bytes": 123456,
    "title": "<=96 chars", "summary": "<=160 chars", "heightHint": 480,
    "previewVerified": false}
  ```

  - `kind` is a CORE-01 `NamespacedId`. Values: `luna.ic/html-document`,
    `luna.ic/mcp-app-snapshot` and `luna.sw/tool-image`.
  - An unknown `kind` or `v` keeps the field as written and shows the tool
    fallback.
  - `id` is host-minted. It is never derived from model arguments.
- **Never synced:** HTML or JS, file paths, URLs, bridge tokens, MCP URIs that
  carry tokens, and the full 2 KiB summary. Unlike `MessagePart::Image`, which
  syncs an absolute host path, an artifact is addressed by id and host only.

### Storage and atomic commit (owning host)

- **Layout.** Content-addressed and immutable:
  `{profile}/artifacts/<aa>/<digest>/document.html` plus `manifest.json`
  (resource key → sha256, MIME, size), and `resources/<sha256>`.
- **Receipt.** `(taskId, requestId) → {inputHash, artifactId, state}` lives in
  the engine store.
  - An identical retry returns the same artifact.
  - The same id with different input is `RequestConflict`, following CORE-01
    command identity.
- **Commit order:**
  1. Write a `pending` receipt.
  2. Write bytes to `artifacts/tmp/`, fsync, then rename into place.
  3. Insert an owner row, mark the receipt `committed` and record a journal entry.
  4. Stamp `artifactRef`.
- **Recovery:**
  - A `pending` receipt that never committed: delete its temporary bytes.
  - A `committed` receipt with no stamp:
    - If its run or part still exists, re-stamp it.
    - Otherwise drop the owner row and collect the bytes.
  - A Stop or timeout never publishes later into another turn. That is the
    existing turn/call fence.

### Ownership, deletion, forks, rollback

- **One ownership table.** Reuse `attachment_owners`
  (`crates/sync/src/store.rs`) with a new `owner_kind` of `artifact`:
  - `owner_id` is `<taskId>/<partId>`.
  - `blob_path` is the artifact directory.
- **Delete through the existing guard.** `remove_unowned_attachment` unlinks
  bytes only when no owner remains. Its owner check and unlink share one
  immediate transaction.
- **Chat deletion** removes that chat's `artifact` owner rows, then collects
  unowned bytes. This is stricter than today's attachments:
  `retire_task_local_records` drops only `draft` and `submission` owners, and
  never collects `chat`-owned uploads.
- **Forks never inherit a reference today.** `side_chats.rs` creates either an
  empty native fork or a `contextSnapshot` built from Text parts only (at most
  32 entries, 24,000 characters). Tool parts and their refs are never copied.
  A future fork that copies tool parts must:
  - add its own owner rows;
  - show provenance;
  - keep any app bridge disabled, creating a fresh app only through a new call
    in the fork.
- **Rewind and rollback.** Same-chat rewind replaces conversation entries
  (`replace_conversation_entries`).
  - Owner rows of removed parts are dropped when the rewrite commits, not while
    it is still staged.
  - Their app bridges and model context are invalidated at once.
- **Archive, snooze and Board status** do not touch references.

### Host reads (IC-1 RPC)

- **The RPC.** `ReadArtifactChunk {artifactId, owner: {taskId, partId}, offset}`
  returns `{data (base64, 45,000-byte chunks), nextOffset, done, digest, mime}`.
- **Catalog row.** `query`, `deviceAddressable` (forwarded to the `host` in the
  ref), `localOnly`, 30-second deadline. This matches `ReadAttachmentChunk` and
  `FetchToolBlob`.
- **No remote-access scope in V1.** A row grants nothing on its own (CORE-01).
- **Authorization:**
  - An owner row must exist for exactly that `(task, part) → artifact`.
  - The task must not be deleted.
  - There is no path parameter.
  - The caller's focused pane is never consulted.
- **Do not reuse `ReadAttachmentChunk`.** Its jail is the uploads directory
  plus *every* workspace chat cwd. It is path-addressed and not owner-scoped.
- **Export** uses the same authorization and returns only the document and its
  resources.
- **A second device** shows *Waiting for host* or *Unavailable* until it has
  the bytes and their digest matches. It caches by digest.
- **The renderer** receives bytes from the engine and serves them from memory
  through `luna-artifact://localhost/<view>/…` (see the prototype). It never
  uses `file://` or signed URLs.

### Privacy and MCP app snapshots

- **Host-private record.** Kept in local SQLite. It is never synced or exported.
  - Source identity: task, run, message, part and call.
  - Full summary (at most 2 KiB) and the resource manifest.
  - For MCP apps, the origin: provider, provider session id and generation,
    server, tool, `ui://` URI and `originCallId`.
- **Bridge authority** requires a live session whose generation matches. A
  stored snapshot always renders read-only without a bridge.
- **App model context** is task-local on the host. It never enters export,
  synced previews or forks.
- **Computer-use omission stays.** No artifact is created from a
  `dataOmitted` desktop screenshot.
- **SW-09** turns provider images with recoverable bytes into
  `luna.sw/tool-image` artifacts through this same service:
  - Claude `image` blocks, Codex MCP `image` items and OpenCode `file` items
    already sit in the host journal envelope.
  - Absent bytes show *unavailable*, never a picture inferred from metadata.
- **Find (IC-5)** indexes `title` and the synced `summary` only.

## 3. Prototype: native renderer and Find (`prototype`)

### Isolated document failure contract (recorded before the code)

| ID | Credible failure | Probe and pass condition |
| --- | --- | --- |
| R1 | The probe cannot see a leak, so a "refusal" proves nothing. | The control view (no CSP, sandbox or shell) must hit the TCP sentinel: `isolation.positive-control-leaks`. |
| R2 | Network escapes through fetch, XHR, image, CSS, beacon, WebSocket, WebRTC, form, self or top navigation, or popup. | Zero TCP requests under `/doc-a/` and zero UDP packets: `isolation.no-network`. |
| R3 | `file://` is reachable. | The image errors and the fetch never reads the file sentinel: `isolation.no-file`. |
| R4 | Cookies, localStorage or IndexedDB persist or are shared. | Each attempt throws or fails: `isolation.no-storage *`. A fresh nonpersistent store backs every view. |
| R5 | A child frame reaches the native bridge, or a forged message is accepted. | Every child post is `Refused{mainFrame:false}` and no `forged` message is accepted: `isolation.host-bridge-refuses-child`. |
| R6 | The document reaches its parent, or Wry's `window.ipc` is visible. | `isolation.no-parent-access`, `isolation.no-wry-ipc`. |
| R7 | A navigation outside the allowlist commits. | The only allowed navigations are the main-frame shell and the subframe document: `isolation.navigation-allowlist`. |
| R8 | A popup, download or permission is granted. | No `NewWindow` or `Download` events. Permissions are always denied: Wry's default **grants** media capture when no handler is set (`wry_web_view_ui_delegate.rs`). |
| R9 | A policy breaks legitimate content. | The prepared resource loads through the scheme: `isolation.permitted-resource-loads`, `load.served *`. |
| R10 | Native content shows outside the virtualized list mask, or swallows hits there. | A point above the list edge misses the view and a visible point hits it: `clip.list-mask-and-hit-testing`. |
| R11 | Menus or overlays fall beneath native content. | GPUI's overlay plane intercepts above a live view, and hits are restored on dismiss: `overlay.menu-above-document`. |
| R12 | Focus is trapped in the web content. | A click enters and native Escape returns to GPUI: `focus.enter-and-escape`. |
| R13 | A resize reloads the document or moves the scroll anchor. | Same view, load count and anchor, and the CSS height follows the native height: `expand.same-view-no-reload`. |
| R14 | Off-screen rows leave orphan views, or the view budget is exceeded. | No visible view without a painted row, live views ≤ 3, eviction then recreation: `lifecycle.*`. |
| R15 | Split panes overlap their clips. | `split.panes-clip-independently`. |
| R16 | Teardown leaks native hosts. | `teardown.no-native-views`. |
| O1 | Wheel over a document neither scrolls it nor chains to the transcript. | Recorded in `wheel.overDocuments`, gated by a plain-row positive control (`wheel.synthetic-delivery-control`). |
| O2 | Hidden views keep animating. | `performance.animationFramesPerSecond.hiddenIncludingTransitions` is recorded. It is a finding, not a gate. |

**How the layers work.**

- The shell and document are served from memory by a `luna-artifact` scheme
  handler.
  - The document response carries
    `default-src 'none'; connect-src 'none'; img-src data: luna-artifact:; form-action 'none'; …`.
  - The document runs in `<iframe sandbox="allow-scripts">`.
- The navigation delegate allows exactly two URLs. The bridge accepts only
  main-frame messages from the shell origin and URL carrying a per-view token,
  which is injected into the main frame only.
- An Escape key monitor is consumed only while the view has focus.
- The clip view reuses the browser's mask and hit-test geometry.
- Native callbacks enqueue `DocumentEvent`s.

**Known risks.**

- **WebRTC:** CSP does not govern ICE. R2's UDP sentinel is the evidence. A
  leak is a design blocker for that path, and the mitigation (disabling peer
  connections) belongs to IC-1.
- **Fixture helpers:** IC-1 must move `click`, `press_escape`, `scroll_wheel`,
  `hit` and `overlay_at` behind `fixture_*` names before promotion.
- **macOS version:** only the QA Mac's version is proved. The minimum supported
  macOS is untested.

### Find feasibility (complete history)

**Source of truth.** The selected chat's history is fully resident.

- `WatchDocMessages` sends a reset of the full `TranscriptSnapshot.entries`
  (`doc_host.rs`).
- The client keeps it in `AppState.transcript`, and subagent and child docs in
  `sub_transcripts` (`ui/src/state.rs`).
- There is no paging on that path, so IC-5 needs no new engine API for
  desktop chats.

**Paged readers.** `ReadSideChat` pages 50 entries with a `before` cursor
(`side_chats.rs`), and cold side chats read SQL rows. A search over those
readers must show *Partial history*, never zero.

**Projection.** One segment per leaf block:

- paragraph, heading, list item, quote child, table cell or code block, built
  with LUNA's `markdown::parse_full`;
- inline runs joined, so formatting does not split a phrase;
- link destinations excluded (`style.link`) and labels included;
- Error text included.

Reasoning, tool calls and results, question answers and images are excluded.

**Matching.** Literal and non-overlapping. Case-insensitive matching uses
`char::to_lowercase` with a per-byte offset map back to source characters, so
byte offsets are never reused across a fold.

**Identity.** `(messageId, partId, blockPath, byte range)`, never a list index.

| ID | Failure the probe must catch |
| --- | --- |
| F1 | A phrase split by `**bold**` is missed. |
| F2 | A match spans two blocks or two messages. |
| F3 | A link destination is searchable, or a link label is not. |
| F4 | A folded match maps to the wrong source bytes. The Kelvin sign shrinks 3→1 byte. The naive mapping is recorded beside it. |
| F5 | Capital sharp s (`ẞ`→`ß`), emoji or CJK fail to match. |
| F6 | A multiline phrase inside one code block is missed. |
| F7 | Reasoning, tool input or tool output is searchable. |
| F8 | A whitespace-only query matches. |
| F9 | The oldest entry is unreachable, or is found by index instead of id. |
| F10 | Matches overlap. |
| F11 | The case-sensitive toggle is ignored. |
| F12 | Streaming or rollback renumbers or loses the active match. |
| F13 | Observation: a lowercase fold does not equate `İ` with `I`. IC-5 decides whether that is acceptable. |

**Performance.** The run records timings against the provisional budget (first
match ≤ 150 ms; no chunk over one 16 ms frame) on a 10,000-entry, ~10 MiB
history round-tripped through `SessionDoc` snapshot bytes:

- reopen and read;
- projection;
- index bytes;
- rare and common first match;
- full count;
- maximum chunk;
- streaming update.

These are measurements, not gates.

## 4. Commands and evidence

Run them in this order from the repository root. Each writes under one run
directory. Keep each command's exit code in the run log.

```sh
RUN="$PWD/target/qa/interactive-chat-surfaces/ic0-$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p "$RUN"

# wire (about 2-4 min; no login, model turn or user config; needs codex, claude, opencode on PATH)
python3 scripts/qa-ic0-provider-probe.py "$RUN/provider"; echo "provider exit=$?"

# prototype: compile both examples, plus the default build (the module is cfg-gated)
scripts/cargo.sh check --locked -p luna-ui
scripts/cargo.sh build --locked -p luna-ui --features browser-fixture \
  --example isolated-document-fixture --example ic0-find-feasibility

# Find feasibility (debug build checks correctness; use --release for representative timings)
target/debug/examples/ic0-find-feasibility "$RUN/find"; echo "find exit=$?"

# native renderer (macOS GUI session, about 60 s; it moves the cursor, so leave the mouse alone)
scripts/run-macos-browser-fixture.sh target/debug/examples/isolated-document-fixture "$RUN/renderer"; echo "renderer exit=$?"

(cd "$RUN" && find . -type f ! -name sha256.txt -print0 | sort -z | xargs -0 shasum -a 256 > sha256.txt)
```

Expected results:

- **Provider:** exit 0 means every `EXPECTED` fact held. Exit 1 lists each
  `drift` row, either a prediction that is wrong or a live probe that was
  blocked. Send the JSON so the contract can be corrected. It never rewrites
  itself.
- **Find:** exit 0 means F1–F12 passed.
- **Renderer:** exit 0 means every R-check passed. A failure names its check in
  `result.json`.

Evidence to retain:

- `provider/ic0-provider-probe.json`;
- the generated `provider/codex-schema/`;
- the fixture operation logs;
- `find/result.json`;
- `renderer/result.json` and PNG captures;
- `sha256.txt`.

Commit the reviewed JSON summaries beside this README under `evidence/`.

The fixture and the probe use no LUNA app, data directory or IPC port. They
coexist with the running LUNA Dev. Format and lint:

- `scripts/cargo.sh clippy --locked -p luna-ui --features browser-fixture --example isolated-document-fixture --example ic0-find-feasibility`
- the configured `rustfmt` check;
- `python3 -m py_compile` on both Python files.

## 5. Criteria mapping and open items

| Criterion | Delivered here | Accepted when |
| --- | --- | --- |
| `wire` | The provider probe, the fixture server and the predicted operation table. | The probe exits 0, or each recorded drift is reflected back into section 1. |
| `shared` | Section 2: reference, storage, ownership, reads, privacy and compatibility, grounded in current owners. | Reviewed against SW-00/SW-09. CORE-14 adopts `artifactRef.kind` as its content type id. |
| `prototype` | The isolated view, the native fixture (R1–R16, O1–O2) and the Find probe (F1–F13). | Both fixtures exit 0, with observations recorded. A failing R-check is a design blocker for that path. |
| `handoff` | The commands above. | Orchestrator: exit codes, hashes, queue checkpoint and delivery. No LUNA Dev refresh is needed: the default build is unchanged. |

Not settled here:

- Live H01 (real model turns calling the HTML tools) and the A01 Codex app
  round trip belong to IC-2 and IC-4.
- OpenCode's handling of MCP image content needs a live turn.
- The minimum macOS version.
- WebRTC suppression if R2 leaks over UDP.
- Wheel-chaining policy (O1).
- Hidden-view pausing (O2).
- Turkish dotted-I folding (F13).
