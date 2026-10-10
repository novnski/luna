# Implementation phases

Every phase was **not started at the planning baseline**. Live phase status and claims now belong to [the shared queue](../README.md); use it before implementation. Existing files below are integration owners; new filenames are proposals. Inspect the relevant crate manifest and current owner before changing it. Prefer a small coherent implementation over introducing every proposed module mechanically.

## IC-0 — establish capability and failure contracts

**Ownership:** engine runtime/tool delivery, harness provider bridges, document/reference storage, UI browser/transcript/Markdown. This is a discovery/prototype phase, not runtime delivery.

1. Record current repo SHA, installed Codex/Claude/OpenCode versions and the actual protocol/schema used by each. Trace how a LUNA tool result reaches the journal, document and transcript in each provider, including resumed chats and stopped calls.
2. Establish artifact storage/transport ownership: local bytes, authenticated remote reads, existing sync limits, deletion/fork retention and crash recovery. Confirm how older clients decode additive presentation references.
3. Probe Codex's actual live resource/tool operations and UI negotiation. Separately probe Claude and OpenCode. Record unsupported operations without implementing a second client or reading saved credentials. Resource inspection and live app interaction must have different evidence.
4. Prototype one offline interactive document in the actual native view stack: scroll it through a long virtualized transcript, open splits/menus, focus/escape, expand and detach. Prove isolation/network controls on the minimum supported macOS version. Treat an overlay, clipping or sandbox failure as a design blocker for that path.
5. Locate the selected chat's canonical message-body source and prove complete-history coverage. Specify display-text/range mapping for Markdown, Unicode, code, edits and streaming. Preserve the existing cancelled Plan/Build decision.
6. Record failure cases from [verification.md](verification.md) before writing production code. Give each acceptance contract one primary E2E owner. Any necessary isolated test must have its failure matrix first; do not write unit tests after implementation.

**Output:** an IC-0 evidence directory and concise updates to this package identifying exact production owners, provider operation names, spec/SDK revision, compatibility choice, isolation mechanism, representative performance budgets and remaining capability gaps.

**Gate:** feasible native renderer and actual all-provider HTML tool delivery are demonstrated. The MCP Apps path has a real Codex operation contract; inability to discover a live operation blocks that portion instead of being papered over with configuration guesses. No phase is declared delivered based on these probes alone.

## IC-1 — artifact service, references and isolated renderer

**Likely owners:** `crates/proto/src`, `crates/doc/src/parts.rs`, `crates/engine/src` asset/RPC paths, `crates/ui/src/browser` platform boundary. Proposed additions: `crates/proto/src/chat_artifacts.rs`, `crates/engine/src/chat_artifacts.rs`, `crates/ui/src/artifacts/`.

1. Add the bounded typed artifact reference, caller-owned publication identity and request receipt. Use an additive document field or a tested older-reader fallback chosen in IC-0.
2. Implement immutable preparation, explicit image manifests, size validation, atomic publication, authorized lazy retrieval and source/export. Reuse existing blob transport without allowing it to fetch arbitrary paths or other tasks' private outputs.
3. Add recovery/reference reconciliation for cancelled writes, request retries, deletion, rollback and forks. Shared bytes are removed only when every authorized reference is gone.
4. Implement the isolated platform view with offline HTML policy, separate storage, scoped asset loading, navigation/permission restrictions, view-generation checks and event queuing. The regular browser keeps its current behavior.
5. Add theme/size transport, keyboard escape, bounded view lifecycle and native placement/clipping support. Do not yet connect pages to engine tools.

**Gate:** a real saved document renders and reopens after restart; denied access and malicious file/network/navigation attempts are proven refused at the owning boundaries. An older/fallback reader can still open the conversation. Native virtualized scrolling does not expose orphan views or hidden pages above overlays.

## IC-2 — HTML preview and publish across Codex, Claude and OpenCode

**Likely owners:** `crates/engine/src/cross_task_tools.rs`, `crates/engine/src/runtime_contract.rs`, new artifact service, `crates/harness/src/codex/luna_mcp.rs`, `crates/engine/src/claude_mcp.rs`, `crates/engine/src/opencode_mcp.rs`, provider result normalizers.

1. Add both HTML tools to the existing capability/runtime catalog and authority path. Keep provider defaults, access modes, turn/call fences and idempotency semantics consistent with ordinary LUNA tools.
2. Implement preview runtime discovery/preparation, isolated capture, measured heights, screenshot image output and console diagnostics. Queue bounded capture work and propagate Stop/timeouts without detached late publication.
3. Register real callable tools in all three provider bridges, including existing/resumed sessions and their supported mode variants. Return the same normalized artifact reference and error categories through each native transport.
4. Preserve screenshot images/full structured diagnostics through each provider's result encoding and LUNA's host journal/privacy policy. Text wrapping or compact preview limits must not destroy presentation metadata.
5. Add compact provider guidance: prepare a self-contained document, preview before claiming visual correctness, publish before final text, use theme tokens, give a useful summary, and report unavailable preview honestly. Guidance is supplementary to callable tools.

**Gate:** real Codex, Claude Code and OpenCode each call preview and publish an interactive visual in an ordinary LUNA chat. Capture shows the actual page and console check; publication returns a durable reference and renders through the owning call. Provider call acknowledgements alone do not pass this gate.

## IC-3 — transcript presentation, persistence and user controls

**Likely owners:** `crates/ui/src/transcript.rs`, its row projection/agent-card modules, `crates/ui/src/markdown`, new artifacts UI, shell/workbench routing, document fold and lazy RPC reads.

1. Project the typed reference into the original completed tool row with stable message/part/artifact keys. Preserve tool details and fallback summary. Do not append a duplicate visual at the end of the conversation when capture lands later.
2. Add title, loading/error states, expand/collapse, source inspection and save. Reuse the live view across expansion where possible and preserve the composer draft/focus.
3. Connect theme changes, responsive dimensions and bounded height updates to the existing virtualized scroll anchor. Verify multiple visuals, live messages, older history and split panes together.
4. Prove restart, archive, Board-linked main/side placement, fork snapshots, rollback and deletion semantics. Verify an authorized second device fetches the correct bytes or gets an explicit host-unavailable state.
5. Keep Mermaid, ordinary images, existing file previews and normal browser tabs on their current paths. No HTML document should obtain a tool bridge because it shares a renderer with an MCP App.

**Gate:** native visual lifecycle and persistence E2Es pass with pixel evidence and authoritative readback. Native Mac acceptance includes actual chart interaction, theme/width changes and scroll stability, not only a static rendered fixture.

## IC-4 — MCP Apps capture and live request host

**Likely owners:** `crates/proto/src/provider_mcp.rs`, `crates/harness/src/mcp_control.rs`, provider adapters/session registry, `crates/engine/src` run/request authority, new app host service and artifacts UI. Existing administrative control-plane probes are inspection owners, not live-interaction owners.

1. Add explicit live-session app capability reporting. For Codex, implement only operation names and negotiated extension support established from its installed schema. Keep ordinary MCP output unchanged when app metadata is absent.
2. Bind app metadata to the originating server/tool. Read and capture its declared UI document within the same actual provider session; retain tool result and capture fallback. Publish before terminal completion only within the bounded capture budget, settling cancellation/failure consistently.
3. Extend the isolated renderer with a typed app host bridge: initialization/replay, host context, allowed server tool/resource operations, app messages, context updates, expansion, links, explicit downloads and teardown. Ordinary HTML gets none of these capabilities.
4. Admit every request against exact app/view/session generation, server/tool visibility and current access. Implement confirmation, denial, stale-session rejection, cancellation, bounded results and duplicate/uncertain-outcome receipts.
5. Persist only the latest permitted model context on the owning host. Deliver it at the next normal user turn through an actual provider-supported untrusted-context mechanism. Removal/rollback invalidates it; source forks do not transfer live authority or context.
6. Revisit Claude/OpenCode live capability probes. Implement adapters when verified operations exist; otherwise keep normal tool output plus a precise unsupported/interactivity-unavailable state. Never launch another unrestricted administrative process, create a second MCP connection, or copy credentials as a workaround.

**Gate:** a real Codex session with a small standards-based MCP server shows its app, performs a read-only call, confirms and completes one real mutation, refuses a declined mutation, and accepts one explicit app message exactly once. Verify stopped/rebound sessions and malformed references. Capability fallback is accepted for Claude/OpenCode MCP Apps only; it never satisfies their required HTML support.

## IC-5 — selected-conversation search

**Likely owners:** `crates/ui/src/transcript.rs`, `crates/ui/src/markdown/parser.rs` and `render.rs`, shell actions/menu/command palette, canonical normalized-message access. Proposed addition: `crates/ui/src/transcript/find.rs`; use engine/RPC work only if IC-0 proves authorized message history is paged.

1. Add focused Find action routing and a chat-local Find state with generation, query, case mode, progress, stable matches and active anchor. Preserve file/diff/browser Find and composer focus rules.
2. Build display-text segments with original/rendered range maps, excluding private/secret/hidden content. Join inline styles without joining unrelated blocks. Search code and ordinary plan text without creating a new plan workflow.
3. Search complete authorized history off the UI thread. Reuse normalized messages; load real missing pages through the existing authority path when necessary. Support progressive counts, cancellation and truthful partial-history states.
4. Highlight visible matches and reveal off-screen matches by stable message/part/block identity. Temporarily open only the required fold, preserve manual changes, and map ranges correctly through Markdown and Unicode transformations.
5. Incrementally update changed parts while streaming; handle new history, edits, rollback and provider switches without renumbering the active match by accident. Keep query/view state local and out of CRDT content.
6. Integrate visible artifact titles and summaries without indexing artifact source, app DOM, secret context or full private tool output. Verify search while an interactive view has been mounted and while another pane is active.

**Gate:** native Find navigates both visible and off-screen body matches, including a phrase across formatting runs and a Unicode case-fold mapping, and remains stable during streaming/rollback. Representative history meets the measured performance budget. An unavailable history page is visibly partial, never a false zero.

## IC-6 — integrated acceptance and delivery

1. Execute the owned E2Es and live/native scenarios in [verification.md](verification.md), keeping logs, protocol/version facts, authorized readbacks, screenshots/video and SHA-256 manifests. An independent protocol fixture complements but does not replace real provider/browser evidence.
2. Run focused checks with `scripts/cargo.sh --locked`, affected formatting/Clippy and CI-equivalent checks. Use the build budget and shared lock; do not bypass a busy/budget-blocked exit or compile an unrelated workspace repeatedly.
3. Use the QA app rules: one running LUNA GUI app, graceful quit, four unique QA identity overrides, preserved data/cache and a verified exact process/port/signature. Do not force-quit the user's app or use real private MCP services for fixture mutations.
4. After each meaningful code milestone and its checks, run `scripts/run-macos-dev.sh --refresh`; verify exact LUNA Dev process, IPC listener and deep/strict signature. Record build time and testing limitations.
5. Update these status tables only for evidenced milestones; put durable shipped behavior in the relevant reference/user documentation. Keep unresolved provider/platform limitations explicit.
6. Preserve/hash evidence, clean only disposable task-owned resources, reopen the primary LUNA Dev after QA stops, and use commit-and-push to deliver verified `origin/main`.

**Final acceptance:** three-provider HTML replies, working Codex MCP Apps with honest other-provider states, complete-history conversation Find, retained evidence, verified native handoff and remote Git readback. Do not mark the package complete while a required live path, isolation guarantee, search behavior or evidence artifact remains missing.
