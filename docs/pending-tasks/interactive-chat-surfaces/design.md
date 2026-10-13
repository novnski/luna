# Design and contracts

This is a proposed LUNA design. Existing code and upstream evidence are listed in [README.md](README.md); implementation sequencing is in [implementation.md](implementation.md). Parameter and type names below are proposed application contracts, not claims about provider-native APIs.

## Product behavior

### HTML replies

An agent calls `html_preview`, checks its screenshot/console diagnostics, then calls `html_render`. A completed call becomes one visual card at the original tool part's position. Its title, Expand, View source, Save, and an accessible text summary remain discoverable. The final assistant reply is ordinary text and need not duplicate the visual.

Publishing is independent of preview availability: a host without the managed preview runtime may publish a valid document, returning `previewVerified:false`. The agent must not say it visually checked that document. Invalid HTML policy, unreadable requested images or denied files cause explicit preparation errors rather than silently altered output.

The page uses LUNA's font and semantic theme tokens, reacts to light/dark changes and width changes, and can update its own chart/table controls. HTML pages receive only theme, dimensions and permitted link/resize messages. They cannot call tools, send chat messages, inspect the transcript or access the engine. MCP Apps obtain those additional operations only through their separate scoped host.

Expand moves the existing view into a larger presentation when the platform permits; it should preserve transient input state. If a renderer must recreate a view, show the reload and document that page-local state is transient. Saved document bytes are durable; JavaScript heap state is not a saved workspace in V1.

### MCP Apps

The original MCP call and its result remain inspectable even when the app replaces the default tool row. The card names its originating server/tool, offers Expand and tool details, and shows Connected, Session unavailable, Unsupported provider, Loading, or Failed based on observed state. Rendering a stored snapshot does not imply a live connection.

The app can interact only through its recorded originating session/server. It cannot select another chat/server, start a provider, change a connection, reuse an administrative inspection process or register an MCP server. A user resumes the owning conversation through the normal UI when interaction needs a live session.

App tool requests pass normal server/tool visibility and LUNA access admission. A server's read-only annotation can avoid an additional app prompt only after those checks succeed. Mutation/unknown annotations require a concrete confirmation showing server, tool and relevant arguments. Annotation metadata never grants broader authority. App-to-chat messages require an explicit confirmation, are visibly attributed to the app, and enter the normal queue exactly once.

Latest app context may accompany the next user message as bounded, untrusted context. It is not a user instruction, permission grant or reason to start a turn. Keep only the latest accepted update for each app; exclude removed/rolled-back app instances. Durable context cannot contain a bridge credential or transport token.

### Conversation search

Expose **Find in Conversation** through the menu and command palette. With transcript focus, Cmd+F opens a compact Find bar scoped to the selected chat. A file/diff/browser with focus keeps its own Find behavior. Find never creates a chat or sends a message.

The bar has query, case-sensitive toggle, previous/next, current/total count and Close. Enter moves forward, Shift+Enter backward, Escape closes. Navigation wraps. Opening selects the previous query for that chat; a short single-line transcript selection can seed a new query. Preserve the composer draft and restore its previous focus only if the user has not moved focus elsewhere.

Search matches canonical displayed text, including code and visible plan text. Formatting markers and hidden link destinations are excluded; inline styling must not split a phrase. A displayed label is searchable. Artifact titles are searchable; raw HTML/JavaScript, hidden app data, private tool arguments/results, secret-input values and hidden reasoning are not. Generic full tool-output search and app-internal DOM search are outside V1.

Matches do not span distinct messages or independent blocks. They may span inline runs within one block. Use literal non-overlapping matches; case-insensitive Unicode matching must preserve a mapping back to source/display ranges. Do not lowercase a string then reuse its byte offsets. Diacritic-insensitive matching and regex are deferred. Whitespace-only queries produce no matches.

## Ownership and data flow

| Object | Owner and identity |
| --- | --- |
| HTML publication | Exact calling task, run, message and tool part; identity supplied by engine authority, never model arguments. |
| Artifact bytes | Owning host's asset store; immutable prepared document and resource manifest with a digest. |
| Transcript reference | Stable artifact ID, kind, safe title/summary, digest, byte length and host routing reference. No HTML or credentials in the synced document. |
| MCP app instance | Originating task/run/tool part, provider-session identity and generation, original MCP server/tool/resource, immutable captured document. |
| Renderer view | Local view instance and random bridge token; never synced or exported. A recreated view gets a new token. |
| Search match | Task/message/part/block identity, logical text range and revision/generation; never a current list index. |

HTML flow: provider LUNA tool call → caller/run fence → engine preparation and artifact commit → normalized tool presentation/reference → document fold → transcript row → lazy authorized asset fetch → isolated renderer.

MCP flow: native tool completion → validated app reference → resource read through that same live provider client → immutable capture → typed presentation reference → isolated renderer → scoped app request bridge → originating provider/server → actual result.

Search flow: selected chat's authorized normalized message snapshot/deltas → display-text projection with offset map → background matcher → stable match anchors → transcript range highlighting and virtualized reveal. Activity/metadata search must not substitute for message-body search.

## Proposed HTML tool contracts

Deliver these tools using the existing LUNA tool gateway in all three providers. Public names are `html_preview` and `html_render`; private registration names may have the normal provider prefix. Do not offer a second direct registration or ask agents to create files and open a browser as the primary workflow.

Offer them in existing restricted/read-only modes when caller authority permits creating a reply; neither tool edits the workspace. Image preparation still obeys filesystem admission, and page publication never widens that admission. Preserve actual native tool permission behavior in each harness rather than silently changing the chat's access mode.

| Tool | Proposed input | Proposed result |
| --- | --- | --- |
| `html_preview` | `requestId`, `html`, optional `title`, `width`, `appearance`, `images` | Prepared-document digest, screenshot image block, actual viewport/captured/content dimensions, bounded console messages, preparation warnings and whether capture is complete. |
| `html_render` | `requestId`, `html`, `title`, `summary`, optional `height`, `images`, matching `previewDigest` | `artifactId`, digest, safe title/summary, actual display height metadata, `previewVerified`, durable receipt and a typed visual reference. |

`requestId` is unique for a new operation and stable on retry. The exact prepared bytes, image manifest, theme/layout bootstrap version and requested input determine the preview binding. A supplied `previewDigest` must match those prepared bytes; a mismatch is rejected as `PreviewMismatch`, not treated as a verified preview. A preview under one appearance is not certification of every appearance/width.

`images` is a bounded explicit manifest of `{key, path}` for images owned/readable on the task's host. HTML uses a documented placeholder per key. Resolve paths only under the existing filesystem authority; canonicalize symlinks, validate actual image bytes and reject missing/denied/non-image files. Do not scan arbitrary JS strings for paths, follow file URLs or read a guessed file outside the manifest. Agent-provided `data:` images also pass type/size checks.

Proposed initial limits: HTML source 2 MiB UTF-8; prepared document plus resources 25 MiB; 16 images, each at most 8 MiB; title 96 Unicode characters; summary 2 KiB; preview width 320–1600 CSS px; inline height 240–1200 CSS px; console diagnostics 64 KiB. Limits apply before expensive copies/decoding as well as after preparation. Change them only with representative measurements recorded in IC-0/IC-1.

A summary is required for accessibility, older-client fallback and unavailable rendering. Preview returns the screenshot as a real image result plus textual/structured diagnostics, not a base64 string in the synced transcript. Retain authorized bytes through the existing host output mechanism. OpenCode or Claude text wrapping must not drop the typed visual reference or screenshot image.

Keep synced summaries within the existing tool-output preview policy (currently 160 characters); the full bounded summary stays on the owning host. Generated-page preview images are not screenshots of the user's desktop. Preserve the existing stricter computer-use redaction for actual desktop captures rather than conflating these result categories.

Structured failures include `ArtifactTooLarge`, `ImageNotFound`, `ImageDenied`, `ImageInvalid`, `DocumentPolicyViolation`, `PreviewUnavailable`, `PreviewFailed`, `PreviewMismatch`, `StaleRun`, `RequestConflict` and `StoreFailed`. Distinguish unsupported capability from a transient runtime failure. A timeout/cancellation leaves no unpublished orphan and does not publish later into another turn.

## Storage, compatibility and lifecycle

Use a single engine-owned artifact service, with the existing host asset/attachment transport adapted only after verifying its ownership and retention semantics. Commit artifact bytes atomically before a document reference can become durable. Journal the receipt and source tool identity. Retrying an identical request recovers the same artifact/reference; the same key with different input returns conflict. Recovery reconciles interrupted commits and removes only unreferenced task-created temporary data.

A prepared document is immutable. The source inspector/export shows that document and embedded resources, excluding bridge tokens, signed URLs and provider credentials. Reopening/exporting does not need a model process and does not trigger an MCP action.

Prefer an additive typed presentation reference on an existing completed tool/message part after proving older-client decoding. If a new part kind is required, supply a tested compatibility path rather than making older readers reject a whole conversation. Keep the normal text/tool fallback available. Never encode a visual reference solely as an unvalidated text marker supplied by arbitrary tool output.

Chat synchronization carries bounded safe metadata and authorized lazy asset routing. A second device sees the correct artifact only after bytes are actually available; otherwise it shows Waiting for host or Unavailable, never a blank success. New rendering capability does not change existing chat-sharing authority or upload all retained private MCP results. Raw server URIs containing tokens stay in the owning host's private record.

Deleting a chat drops its references; remove bytes only when no fork/chat/export operation still owns them. Rollback hides artifacts from removed parts, invalidates their app bridge/context and preserves any reference retained by another conversation. Archive/snooze/Board status changes do not delete artifacts. A read-only snapshot inherited by a context fork does not acquire the source chat's live MCP authority: show provenance, keep its bridge disabled, and create a fresh app only through a new call in the fork.

## Renderer isolation and native integration

Create a separate render mode/platform adapter with an ephemeral, nonpersistent storage partition. The ordinary browser currently has no page-to-engine IPC; adding the app bridge must not grant it to ordinary tabs. Artifact content is untrusted even when generated by the user's agent.

Serve only the selected immutable document/resources from a scoped asset origin, never `file://`. Disable same-origin privileges relative to LUNA, top-level navigation, script-created windows/dialogs, general downloads and automatic permission grants. Native callbacks enqueue events rather than re-enter GPUI. Validate frame/view origin, bridge token, instance ID and generation on every message.

The native WebView loads a minimal trusted outer host shell, which contains the untrusted document in an opaque-origin sandboxed iframe. Do not grant `allow-same-origin`; grant scripts and only the additional permissions this document type actually needs. The shell implements the standard parent/frame relationship expected by MCP Apps and validates `event.source` before forwarding a typed message. Native IPC belongs only to that shell and must validate the originating frame; direct child-frame calls are refused. Copying a webpage into a top-level WebView with a bridge is not an equivalent sandbox.

Ordinary HTML is offline: deny requests outside its prepared resources and forbid remote form submission. For MCP Apps, translate declared CSP/permissions into an enforceable view policy; missing declarations deny network resources. Enforce allowed destinations at the browser/network boundary, including redirects, IPv4/IPv6, loopback/private/link-local addresses, DNS changes and WebRTC. JavaScript API replacement is not a security boundary. If the current platform cannot enforce a requested network permission, return that precise limitation rather than opening unrestricted browsing. Tool-backed offline apps can still work through the scoped host bridge.

IC-0 must prove real WebView placement/clipping under GPUI's virtualized list, scrolling, splits, menus and overlays. Use the platform's child-view positioning support, and do not mount every historical page. Initial budget: at most three live document views per window, with one authoritative view per app instance. Detach/pause off-screen views, retain document references, and surface transient-state reload where necessary. Full-screen promotion should reuse the same live view.

Allocate a bounded card before loading. Resize messages update the same row identity and preserve the active scroll anchor; they cannot steal focus or keep scrolling the user to the bottom. When outside the viewport, stop timers/animations using actual platform lifecycle controls or destroy the view. Measure idle CPU/GPU and memory rather than assuming CSS visibility suspends execution.

Map semantic theme variables for background, foreground, muted text, border, accent, chart palette and typography. Separate rendering tokens from product implementation details. Update appearance and dimensions without reloading a live app unnecessarily. All cards have usable text alternatives and keyboard-accessible controls; focus can enter and escape the document.

## Preview runtime

Use one managed, version-pinned headless browser per owning host for capture and height measurement, with a bounded concurrency of one and cancellation tied to the requesting run. Reuse an existing suitable production runtime if discovery proves it; do not repurpose a user's installed browser/profile or add Node as an application runtime by accident.

First-use preparation reports actual availability and installation/progress state. Missing native dependencies or a denied download produce a specific unavailable result. Normal publishing remains possible. The preview uses the same prepared document, theme bootstrap and resource/network policy as the visible renderer. Record any browser-engine differences: screenshot success in Chromium cannot prove native WebKit layout or isolation.

Capture includes screenshot dimensions, measured content height and diagnostics for console logs/errors and uncaught exceptions. Cap huge pages, execution duration and resource/memory growth; report clipping and timeout honestly. Preview fixtures must exercise real browser loading and enforcement, not a service that returns a supplied PNG.

## MCP Apps provider and host contracts

Separate capabilities for app metadata, resource read, live tool call, live resource read, model-context delivery and native rendering. Never derive all of them from `ReadSupported` or a provider name.

| Provider | Required first behavior |
| --- | --- |
| Codex | Discover and negotiate the UI extension using the installed app-server schema. Capture declared resources from the actual originating session. Route supported host operations through that live client; prove a real server result. Do not assume the current LUNA adapter already exposes the required native operations. |
| Claude Code | Keep existing inspection available. Probe the active CLI session for resource/tool operations and declaration metadata. Implement app interaction only if that exact path is verified; an unrestricted administrative probe is never an interaction fallback. |
| OpenCode | Keep inventory honest. Check the installed server's actual schema, tool metadata and live session operations; its current LUNA `ReadAppResource` control is unsupported. Preserve normal output until a verified adapter bridge exists. |

Capture only a valid app resource declared for the originating server/tool and matching the expected MIME/URI. Check document sizes before decode, inject policy before scripts, and save a reference only after capture succeeds. Bound capture wait (initial target 20 seconds) and settle fallback before the terminal run event if capture fails or is cancelled. A later capture cannot resurrect a stopped row.

Implement the negotiated host subset: initialization and original tool input/result replay; theme/dimension changes; permitted resource/tool requests; explicit app messages; bounded model-context updates; links; inline/expanded display; explicit file save; and teardown. Each operation has a typed request/result, size/time limits, cancellation and capability advertisement. Unsupported optional operations return a protocol error. Do not advertise model sampling or agent-driven app automation without implementing and verifying them.

Only the originating server's app-visible tools/resources are eligible. Revalidate the session generation and current admission before dispatch and again when accepting a result. Session restart/rebinding invalidates old bridge handles. Stop does not automatically restart an agent. App tool calls follow the chat's [permission mode](../permission-modes/README.md): Full runs them once without a prompt, Auto leaves them to the provider's reviewer, and a call declined in Auto (or in a legacy Ask confirmation) performs no upstream call. Avoid duplicate mutations/messages on repeated bridge IDs, uncertain outcomes or UI remounts; an uncertain mutation is shown for inspection, not blindly retried.

Model-context budget: 16 KiB per app, 64 KiB aggregate per task. Context is latest-value, instance-scoped and labelled untrusted when delivered through each provider's actual supported context mechanism. Do not turn it into a normal user message to fake unsupported context injection. Export/source and ordinary synced previews exclude it. Context persistence is task-local on the owning host; a fork receives no source app context by default.

## Search architecture and limits

Build a task-local display-text projection from normalized message parts, reusing the Markdown parser's visible-text semantics and stable identities. Search runs over the complete authorized message body, not only mounted rows, tool previews or native CLI history from unrelated sessions. IC-0 establishes whether that body is already resident. Reuse it when complete; if history is genuinely paged, extend the authoritative message-loading path with cancellable cursors. Do not add a new metadata search API and call it full conversation search.

Run matching on the background executor, update only changed parts during streaming, and cancel obsolete query generations. Publish matches progressively with an explicit Searching/partial count until coverage is complete. An unavailable host/history page is Partial history, not zero matches. Avoid eager asset downloads or full private-output fetches while searching.

Every match maps projected text ranges back to its Markdown/code block. Highlight all currently rendered matches and distinguish the active match. Revalidate its revision before revealing it. Reveal loads the necessary history, temporarily opens only the fold needed for the match and scrolls by stable message/part/block identity. Never jump using an index captured before streaming or history insertion.

Keep the active match stable when newer text arrives. If it disappears after rollback/edit, choose the next surviving logical match and update the count. Searching, highlighting and navigation do not alter messages, publish CRDT mutations, claim messages read, restart providers or replace normal scroll anchors. Closing restores search-induced folds/viewport only when the user has not subsequently changed them.

Provisional performance gate: a representative 10,000-entry/10 MiB conversation returns its first match within 150 ms after dispatch on the QA Mac, without a UI-thread task exceeding one 16 ms frame. Record hardware, cold/warm timings and index memory before adopting this budget; target a 20 MiB search working set with chunked matching when larger histories would exceed it. Keep regex, whole-tool-output indexing and a global search service out of this feature.
