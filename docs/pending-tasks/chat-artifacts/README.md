# ART-01 — Per-chat Artifacts panel for generated deliverables

**Requested:** 11 October 2026. **Priority:** P1. **Group:** `artifacts`.
Live status and ownership belong to the [shared queue](../WORKFLOW.md).
This document specifies future implementation; it does not certify runtime delivery.

## User outcome

When a chat produces a PDF, document, spreadsheet, presentation or image, the
user can find it in that chat's **Artifacts** section without scrolling back
through messages or asking the agent for its path. The collection survives
restart and remains accessible from the native iOS client. Ordinary scripts,
source code and website files do not appear automatically.

The panel is a collection of delivered outputs, not a workspace file tree,
Git Changes list, input-attachment list or global document library. Main,
side and Board chats retain their existing identities and ownership.

## Reference research, pinned 11 October 2026

These are source/documentation findings, not a live UI comparison. Local T3 and
BB checkouts were inspected without updating them. Before implementation,
record current versions and run the comparison journey below; do not assume
that an app's file viewer proves automatic artifact discovery.

| App and evidence | Observed behavior | What LUNA should investigate or reuse conceptually |
| --- | --- | --- |
| **T3**, `477282263a9115f911380b5aec7d4973267e4edc`: [panel resources](https://github.com/pingdotgg/t3code/blob/477282263a9115f911380b5aec7d4973267e4edc/apps/web/src/rightPanelStore.ts#L24), [file preview routing](https://github.com/pingdotgg/t3code/blob/477282263a9115f911380b5aec7d4973267e4edc/apps/web/src/browser/openFileInPreview.ts#L36), [PDF/HTML view](https://github.com/pingdotgg/t3code/blob/477282263a9115f911380b5aec7d4973267e4edc/apps/web/src/components/files/BrowserDocumentFrame.tsx#L14), [HTML publication](https://github.com/pingdotgg/t3code/blob/477282263a9115f911380b5aec7d4973267e4edc/apps/server/src/htmlRender/HtmlRender.ts#L416). | Panels distinguish workspace files, attachment-backed files and browser previews. PDF/HTML file opens use host asset URLs. HTML publication creates a thread-bound attachment reference and removes the file on failure before returning the reference. | Trace registration through provider result → attachment/reference → panel; compare identity, retry cleanup, preview and export. Inspected code does **not establish a deliverables-only automatic Artifacts collection**. Artifact-template cards are reusable skill templates, a different feature. |
| **BB**, `a2bbfaadaa91084cde68caa06954c8927f0015a4`: [storage browser](https://github.com/get-bb/bb/blob/a2bbfaadaa91084cde68caa06954c8927f0015a4/apps/app/src/components/secondary-panel/ThreadStorageBrowser.tsx#L29), [search/lifecycle controller](https://github.com/get-bb/bb/blob/a2bbfaadaa91084cde68caa06954c8927f0015a4/apps/app/src/components/secondary-panel/useThreadStorageBrowser.ts#L48), [native file types](https://github.com/get-bb/bb/blob/a2bbfaadaa91084cde68caa06954c8927f0015a4/apps/app/src/lib/workspace-open-target-preference.ts#L20), [inline visuals](https://github.com/get-bb/bb/blob/a2bbfaadaa91084cde68caa06954c8927f0015a4/plugins/inline-vis/PLUGIN_OVERVIEW.md), [task attachments](https://github.com/get-bb/bb/blob/a2bbfaadaa91084cde68caa06954c8927f0015a4/plugins/tasks/skills/tasks/SKILL.md#L54). | Thread storage has a searchable file tree, loading/empty/error states and selected-file previews. Native-opening preferences recognize common document/media formats. Inline visuals can use thread-storage files. Task workers can explicitly attach reports/images. | Compare output storage versus workspace files, mobile selection, missing-file handling and explicit attachment provenance. Neither the file-type list nor task attachments prove automatic chat-output detection. Do not copy its generic storage tree as the requested filtered collection. |
| **Codex / ChatGPT desktop**, [official Work with files documentation](https://learn.chatgpt.com/docs/artifacts-viewer), fetched 11 October 2026. | Documentation confirms generated document, spreadsheet, presentation and PDF previews beside chat, with optional automatic opening after task completion; supported previews permit targeted revision. It also documents HTML previews, whereas this request excludes ordinary web files from automatic collection. | Verify the actual installed app's Artifacts entry point, automatic inclusion rules, file updates, removal, reopen and source-message links. The documentation establishes preview behavior, not exact collection/indexing semantics. Keep desktop evidence distinct from CLI behavior. |

For each accessible app, use one disposable chat and the same set: report.pdf,
report.docx, table.xlsx, slides.pptx, chart.png, build.py, page.html, app.ts and
an explicitly delivered notes.md. Record app/source version, capture/readback,
which files appear automatically, when they appear, whether a plain path
mention is enough, updates to the same path, duplicate names in two folders,
relaunch, missing source, preview/open/export, and mobile behavior. Trace the
T3/BB mechanism in source as well as the user flow. If an app/account/device is
unavailable, record the exact unverified behavior; do not infer feature parity
from a similarly named component. This comparison informs LUNA's implementation
and is not a request to copy every reference-app feature.

## Scope and ownership

ART-01 owns discovery/registration, the per-chat index, desktop panel, native
iOS collection and the complete generated-output retrieval journey.

| Existing owner | ART-01 integration |
| --- | --- |
| IC-0 / CORE-14 | Reuse canonical owner-scoped content/asset references, compatibility and renderer registration. Never introduce another blob store or custom document-part envelope. |
| IC-1 | Reuse the artifact service's storage, lazy reads, retention and export. Extend its shared contract only where ordinary document/media outputs need metadata or registration; keep HTML isolation in IC. |
| IC-2/3/4, SW-07/09, CORE-15 | Retain HTML/MCP Apps, inline visual result and video-renderer ownership. Rendering a tool result does not automatically make it a final deliverable. Published interactive HTML stays with IC and is excluded from this task's default file collection. |
| ZI-04 / existing file viewers | Reuse typed file activation and authorized native preview/open routes. Do not build Office editors or new PDF/image renderers as part of this task. |
| IOS-02 / RM-06 | Place the native collection in the established phone shell and use host-authorized file/asset reads. IOS-06 retains richer inline renderer behavior; this task owns the collection and document open/share flow. |
| CORE-12 | Use the native contribution/view-model contract already required by CORE-14; no permanent special-case second sidebar framework. |

Queue prerequisites are **IC-1, ZI-04, IOS-02 and RM-06**. CORE-14/CORE-12 are
already transitive prerequisites. Desktop implementation can be exercised before
phone acceptance, but ART-01 is not complete until both required surfaces pass.
Do not add a reverse dependency from those foundations to ART-01. Other tasks'
active contracts and claims remain unchanged.

## Behavior to implement

1. **Classify deliverables explicitly.** Default formats: PDF, DOC/DOCX, ODT/RTF,
   XLS/XLSX/ODS/CSV, PPT/PPTX/ODP and supported image formats. Confirm the exact
   viewer-supported list at implementation. Markdown/text reports require
   explicit output publication; ordinary README/source/config/log files do not
   qualify. Exclude scripts, executables, HTML/HTM, CSS, JS/TS, build products,
   caches and intermediate conversions. Do not silently include audio/video,
   archives or a whole generated website in V1; those need an explicit later
   scope decision. Extension/MIME checks help classification but cannot alone
   establish that a file was produced for this chat. An image used as a website
   asset or an internal QA screenshot is not automatically a delivered image.
2. **Register outputs across providers.** Trace real Codex, Claude and OpenCode
   result/file-write/publication paths. Normalize available structured output
   evidence and provide one host operation for agents/tools to publish final
   files when native wires lack that metadata. Integrate it into provider tool
   guidance so normal generation flows populate the panel without a second
   user request. Handle a shell-generated PDF too; typed image tools alone are
   insufficient. Prove actual automatic collection, not just a manual demo API.
   Avoid recursively watching/indexing every workspace file or guessing from
   arbitrary prose. Bounded legacy backfill may use persisted trusted references;
   unknown old outputs stay explicitly unavailable rather than triggering a
   disk crawl.
3. **Persist identity and provenance.** Retain artifact/revision IDs, chat and
   owning host/profile, generating turn/tool receipt when available, display
   name, type, byte size, creation/update time, content hash and authorized asset
   reference. Keep bytes in the existing host store. Publish atomically after a
   completed stable file read, with idempotent retry receipts. Preserve immutable
   delivered bytes when the source later changes or disappears; show original
   path availability separately. Decide and document read consistency and size
   limits before implementation. Hashes alone do not confer access.
4. **Make the collection easy to use.** Provide a reachable Artifacts entry with
   count, newest-first list, readable names/type/size, lazy thumbnails, filename
   search and simple type filters. Each item supports preview/open, save/export,
   copy location when meaningful, reveal original on the owning desktop and
   jump to its source message. Do not steal focus or switch panels while the user
   types. Include empty, publishing, unavailable, error and oversized states.
   Keyboard/AX navigation and a narrow layout are required. Unsupported previews
   still offer an authorized download/open fallback.
5. **Handle revisions and mistakes.** Repeated registration of identical output
   in the same chat is idempotent. A later delivered version groups under the
   same logical item with a visible current revision; old published revisions
   remain distinguishable. Same basename in different directories/chats/hosts
   never silently merges. Define how explicit replacement chooses logical
   identity. Removing/hiding an item from the collection does not delete the
   source file; deleting shared stored bytes follows the existing reference
   lifetime policy. User inputs stay outside the default generated-output list.
6. **Use the same index on iOS.** Show the collection for the selected host/chat,
   native image/PDF previews or compatible open fallback, and Files/share-sheet
   export. Retain useful cached metadata while offline, with truthful download
   availability. Reconnect updates the existing item rather than duplicating it.
   A macOS absolute path is never treated as an iPhone local file. No second
   phone-only collection or automatic cloud upload is introduced.

## Failure contracts and acceptance evidence

Write the test-audit authoring gate before implementation. Extend the real
artifact service/provider and native journey owners; avoid source-string tests,
post-implementation unit tests, duplicate renderer suites or test-only exports.

| ID | Observable acceptance and credible failures |
| --- | --- |
| A01 — Capture | On each of Codex, Claude and OpenCode, generate PDF/DOCX/XLSX/PPTX/PNG outputs and a shell-created deliverable. They appear without asking for paths or adding each manually. Negative controls include script/source/HTML, input attachment, nonexistent prose path, internal screenshot and intermediate conversion. Record exact expected inclusion set and actual registrations. |
| A02 — Authority | Wrong chat/host/profile, guessed asset ID, revoked grant, traversal, symlink escape and signed/credential-bearing URL cannot grant reads. Resolve and validate at the owning host, including race-safe reads. Preserve existing private tool-output omission rules. Verify denied reads independently; thumbnail generation must obey the same authority. |
| A03 — Lifetime | Interrupted publication/retry, repeated native event, streaming partial file, same-path new revision, same-name distinct file, app restart, source deletion, missing asset, fork and chat deletion preserve correct index/bytes or a precise unavailable state. Independently hash exported bytes. Removing a panel item leaves its original file intact. |
| A04 — Desktop | In a long chat, find and preview/export the output solely through Artifacts; jump to the correct offscreen source. Verify keyboard/AX, focus/draft preservation, lazy preview teardown, and bounded behavior with 1,000 metadata entries and oversized/malformed files. Adopt measured budgets from CORE-00; retain raw timings and memory. |
| A05 — iOS | Against the same host/chat, compare index IDs/revisions, preview PDF/image, export to Files/share, switch host, disconnect/reconnect and revoke access. Retain physical-device evidence for the finished flow; unsupported Office preview uses the documented native open/export fallback. |
| A06 — Delivery | Retain app/source/provider versions, commands/exit codes, sanitized registration/read receipts, independent file hashes, native screenshots/AX and a SHA-256 evidence manifest. Run applicable focused build/E2E/lint/format, review, refresh/verify LUNA Dev, verify main delivery and checkpoint queue criteria. State any unavailable platform/provider evidence without marking it passed. |

Research acceptance records observed versus unverified behavior for T3, BB and
Codex. Implementation acceptance requires LUNA's own end-to-end results; a source
comparison or a populated mock panel cannot satisfy A01–A05.

## First implementation step / resume

Claim ART-01 through the queue, read prerequisites and their retained evidence,
then refresh the reference comparison and current LUNA provider result →
asset/reference → document/RPC → desktop/iOS file-open paths. Record the exact
publication, classification, immutable revision and authority decisions with
existing owners before code. Checkpoint only evidenced criteria; pause with
the exact next command and retained paths if the run is unfinished.
