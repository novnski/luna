# Pending implementation tasks and E2E acceptance

All five tasks are **not started** at the recorded source cut. Rebase the [evidence](research.md) against current main and active plans first. Do not cherry-pick an entire upstream commit: LUNA's engine guards, provider adapters, transcript identities, Board and workbench have diverged.

For future implementation, record the failure contract and author meaningful E2E scenarios before production code, through the repository's [test-audit gate](../../../.codex/skills/test-audit/SKILL.md). Reuse an existing strongest-boundary suite where possible. Do not add unit tests after implementation or expose test-only production seams. Retain a repeatable artifact containing source SHA, commands, configuration, expected/actual state and evidence hashes. Synthetic peer scenarios verify deterministic failures; required native/live journeys establish the user-facing behavior. Follow the QA app isolation and graceful-quit rules in [AGENTS.md](../../../AGENTS.md).

## ZI-01 — isolate and bound turn snapshots

**P1 · not started. Owners:** engine `diff_sync`, snapshot lifetime and Latest turn RPC; existing Changes presentation only where partial/unavailable state needs clarification. Independent of remote-client or Board work.

**Outcome:** using an agent and inspecting Latest turn preserves the checkout's real index and does not add app-generated objects to its Git database. Snapshot work, retained storage and unavailable/partial results are explicit.

### Failure contract before code

1. An untracked build tree or dataset makes `git add` hash unlimited bytes, creates loose repository objects or monopolizes a turn-start hook.
2. A hung Git child, clean filter or filesystem survives capture cancellation/timeout.
3. Omitting a path only in the second capture creates a false mass addition/deletion. An omitted changed path looks like a complete zero-change result.
4. A failed new capture, non-repository cwd or slower old capture leaves a stale previous-turn baseline associated with the new turn.
5. A snapshot store is removed while an RPC/viewer still needs it, or never removed after its final reader/turn/chat ends.
6. A crash sweep removes another engine/profile's live store. Paths containing spaces/alternate separators, linked worktrees, bare/common Git dirs or unborn HEAD break object lookup.
7. Temporary-index capture alters staged changes, tracked files, Git refs or repository settings. Reads mistakenly look for private objects in the checkout alone.

### Work

1. Introduce a narrowly owned snapshot handle containing checkout identity, tree, capture generation, omitted paths and an engine-owned object-store lifetime. Use LUNA's scoped data root and existing write ownership; keep app and checkout identities separate.
2. Redirect writes through an absolute `GIT_OBJECT_DIRECTORY`; use the checkout's actual object directory as an alternate. Resolve common Git dirs through Git, not a hard-coded `.git/objects`. Apply the same object context to every tree/diff read. Encode alternate paths correctly for the target OS.
3. Bound untracked-file bytes/count, listing output, total capture time and concurrent/retained capture work. Upstream starts at 8 MiB per untracked file, 10,000 files/256 MiB for untracked content and 120 seconds overall; review these defaults against representative LUNA repos before adopting them. Include tracked files subject to an explicit overall failure/timeout policy; never silently report an excluded tracked change as complete.
4. Layer a comparison snapshot on the turn-start tree and apply a consistent omission set on both sides. Prefer excluding the largest untracked units while retaining ordinary source files. Expose a bounded partial reason/count to the existing diff UI; do not sync arbitrary omitted-path inventories.
5. Cancel old capture generations and publish only the matching successful one. A failure or cwd change makes the new turn scope unavailable; it cannot fall back to a prior turn. Give Git children kill-on-drop plus bounded termination cleanup.
6. Release stores only after their last reader. Remove abandoned stores only within the current profile/process ownership contract after startup has proved they are stale. Do not delete existing checkout objects, run Git GC or attempt to repair historical repositories as part of this task.

### E2E gate and artifact

- In a disposable ordinary repository and linked worktree, start real engine turns and query Latest turn after staged, unstaged, pre-existing untracked and newly created files change. Compare the returned patch with a separately captured Git oracle. Real index hash, refs and object-file contents/count stay unchanged by app snapshots.
- Cover oversized/untracked-directory budgets and a directory crossing the threshold during a turn. Expect a partial marker and no fabricated additions/deletions; small new source files remain inspectable. Cover a deleted/renamed tracked file and unborn HEAD.
- Make a controlled Git/filter child hang; cancel/time out through the public engine path. Retain child-process readback showing it exited, and prove a later capture works. Exercise overlapping starts, failure after a prior good turn and a move outside Git.
- Hold a diff reader while replacing/archiving its chat; its store survives until the reader finishes. Restart after a controlled interrupted capture and prove only owned stale stores are collected.
- Retain `snapshot-e2e.json`, before/after repository object/index manifests, canonical expected/actual patches, omitted-path cases, process receipts and one native capture of the complete/partial/unavailable presentation. Record cold/warm durations and bytes; set regression limits from measured baselines, not upstream claims.

## ZI-02 — bound workspace registry history

**P1 · not started. Owners:** existing registry document, `WorkspaceHost`, engine profile/configuration and guarded snapshot persistence. Keep the current registry wire protocol; no replacement sync/store architecture.

**Outcome:** a permanently local profile stores current row state without retaining an ever-growing server-ack queue. A temporarily offline synced profile retains exactly the operations needed for correct convergence and notifications.

### Failure contract before code

1. Repeated status/rename/placement writes to a fixed row set grow `pending` and snapshot bytes without bound in a local profile.
2. Treating absent connectivity as local-only loses offline intent or resurrects a remote tombstone on reconnect.
3. Coalescing across an in-flight batch changes its content/id, drops a cascade operation, reorders chat labels against session notifications, or hides working → needs-input → working/completed transitions.
4. Migration changes HLC ordering, loses a deletion, restores a terminal task to live state, or publishes rows before the guarded snapshot commit succeeds.
5. Rollback through `RegistryDoc::from_bytes`, restore/import or restart loses the profile mode and resumes unbounded queueing, or incorrectly folds a synced replica.
6. Compaction blocks startup on a huge snapshot, mutates another profile, or implicitly creates an edge connection/sign-in.

### Work

1. Pass an explicit immutable profile capability from engine assembly to `WorkspaceHost`. Only the permanently local scope may fold writes directly into local authoritative rows. An edge-disabled development/synced profile remains sync-capable and must retain intent. Preserve the existing restart boundary between separate local and account profiles.
2. On local-profile load, apply old queued operations in their original semantic order through the existing row merge, preserving HLCs, tombstones and effective row values. Retire that queue only through guarded persistence. New local writes update rows and generation/watch notifications without an ack queue.
3. Preserve the mode across every current reconstruction/rollback path. Keep LUNA-specific row kinds, Board/placement projections, task durability and device-write barriers. Do not replace current modules with upstream copies.
4. In a separately reviewable phase of this same task, compact only a demonstrably equivalent unsent tail in synced profiles. Keep in-flight content/ids and multi-operation atomic batches unchanged. Preserve notification transitions and chat/session ordering; retaining extra operations is preferable to an unproved merge. Keep server op/byte limits and reconnect cursor semantics.
5. Benchmark before/after with the same row set, growing repeated-write counts and an older queued snapshot. Measure pending operations, serialized snapshot bytes, load/save wall time and engine/app footprint. Report registry improvements independently from total startup and graphics-driver footprint.
6. Do not add automatic VACUUM, data deletion, journal rewrite, watcher consolidation or diagnostics UI as incidental work. If measurements find another bottleneck, bring evidence to its existing lifecycle/performance owner.

### E2E gate and artifact

- Through real engine mutations, apply 25,000 and then 100,000 repeated supported updates to a fixed fixture row set, including rename, terminal/live status, archive/restore and placement. Permanently local pending operations stay zero after migration; snapshot growth follows changed row values, not historical write count. After restart, canonical row values and visible sidebar/Board state match the pre-restart oracle.
- Start from an older pending snapshot with cascades, deletions and legacy migration. Exercise a guarded persistence failure/rollback and a second restart. Preserve all current public outcomes and restore the correct mode.
- With two isolated synced engines and the real existing registry transport fixture, disconnect one, change/delete related rows from both, reconnect and compare server/client canonical rows and notification sequence. Include an in-flight batch, an atomic cascade and needs-input/completed transitions. An offline synced profile must not behave like a permanently local profile.
- Retain `registry-growth-e2e.json`, redacted aggregate byte/time/operation samples, before/after canonical-state hashes, reconnect/notification receipts, migration/failure artifacts and native readback. Use disposable profiles, not the user's registry. Do not claim a fixed 200 MB target or a platform-wide startup improvement without measurement.

## ZI-03 — preserve and show the actual current checklist

**P2 · not started. Owners:** shared Todo wire/fold, native Codex/Claude normalization, selected-chat transcript projection and composer presentation. Dependency: validate current provider event shapes; no dependency on a new workflow engine. SW-05/SW-10 consume evidence without creating another checklist owner.

**Outcome:** the selected chat can show a compact current step and completed count from the latest provider checklist, with optional expansion. It is read-only; it does not start, reorder, complete or stop tasks.

### Failure contract before code

1. Native Codex `turn/plan/updated` is dropped, or an active item is indistinguishable from every pending item.
2. A late child/old-turn event replaces the parent's current checklist, or reused singleton ids cause duplicate rows or stale-echo suppression across segments.
3. An empty update leaves the previous list visible; clearing/switching/restoring a chat briefly shows another chat's plan.
4. A spinner keeps running after stop/error, a pending item is labeled active without provider evidence, or a legacy/unknown status breaks deserialization.
5. The tray covers the queue/question/IME draft, takes keyboard focus, or rescans the entire transcript on every keystroke/animation.
6. A permanently expanded long list adds distracting chrome; hidden completed history reappears on an unchanged update.

### Work

1. Add backwards-compatible optional status to `TodoItem`, retaining legacy `done` behavior and a documented pending/in-progress/completed mapping. Read old snapshots and unknown future values safely. Cancelled/unknown items cannot become completed or animate as active by inference.
2. Consume current Codex plan notifications with the existing live-plan identity and strict parent/child/turn routing. Preserve Claude TodoWrite status. Update the existing durable fold, serialization and any TS/mobile readers that need the additive field, without introducing a second plan store.
3. Audit the installed supported OpenCode 2 event/tool contract. Map a native checklist only if it actually exists; otherwise leave the view absent for that provider. Do not port upstream's obsolete OpenCode 1 `todowrite` branch.
4. Derive the newest selected-chat checklist from existing parts/revisions, including singleton updates across segments and empty clearing. Cache against chat/revision, and reject old async results after navigation.
5. Place a compact summary beside/above the existing composer context: completed count plus the explicitly active item. Without an active item, say which item is next without claiming it runs. Expand on explicit user action; cap visible rows and preserve per-chat collapse/dismissal for the current list. No checklist means no new surface. No automatic pane opening or agent commands.
6. Stop active animation when the owning turn ends. Keep draft/queue/question ownership, reduced motion, native AX labels and keyboard activation intact. Distinguish provider checklist completion from agent-turn/task completion.

### E2E gate and artifact

- Run native Codex and Claude tasks that publish a plan, change the active step, complete it and clear it. Compare actual sanitized provider events, the engine transcript and the visible summary. Confirm one current plan and no duplicated text/tool rows across a follow-up turn.
- Include old `{text, done}` snapshots, unknown/cancelled statuses, child updates and late previous-turn frames through an existing deterministic peer scenario before code. Restart/reopen the same chat and switch rapidly among main/side/Board chats; each checklist retains the right owner.
- Stop/error during an active step, edit a queued message, answer a blocking question and compose Unicode/IME input with a long checklist. No live spinner after stop, no obscured input, no unsolicited send or focus jump. OpenCode gets a positive native gate only if a real supported source was found; otherwise retain the capability decision.
- Retain `current-checklist-e2e.json`, sanitized native event excerpts, durable transcript/owner readback, keyboard/AX evidence, light/dark/reduced-motion screenshots and a recording of updates while typing/navigation occurs.

## ZI-04 — open typed tool files in the existing Files surface

**P2 · not started. Owners:** transcript file-badge interaction and existing shell workspace-file route. No new backend, editor or file authority. Scope: typed read/write/edit/patch paths whose existing resolver supports opening.

**Outcome:** activating a file badge opens that file in the correct chat/host's existing Files surface without changing the tool card's expansion or any draft.

### Failure contract before code

1. Badge activation bubbles into tool expansion, fires twice, moves the caret or loses a queued draft.
2. A side/Board chat badge opens under the selected main chat, or a remote path is read from the viewing Mac's similarly named directory.
3. An arbitrary string/terminal output, escaping path, URL or unsupported host path becomes a file-read grant. A deleted file looks successfully opened.
4. Repeated activation creates duplicate tabs, breaks existing unsaved-editor prompts or replaces another editor's draft.
5. A truncated/multibyte path cannot be discovered or keyboard-activated, or the entire tool card becomes an ambiguous file button.

### Work

1. Supply the existing workspace `LinkUi`/activation route to typed file badges. Resolve against the original owning chat's approved roots and host, not the current UI selection or a shell command. Keep unsupported badges inert or explain unavailable state using current patterns.
2. Give the badge its own labeled native button/focus target, full-path tooltip and one activation handler. Stop propagation so expansion/select/copy behavior is unchanged. Reuse Files-tab deduplication and existing unsaved-change handling.
3. Verify multi-file patches map each badge to its actual path. Reuse existing missing-file, remote-unreachable and read-only behavior. Do not parse arbitrary tool prose into file links.
4. Do not add a default-editor launcher in this task. Any later such action must prove the workspace is on the viewing device and use the established platform opening API.

### E2E gate and artifact

- In an actual native LUNA chat, have a supported provider read/edit/write a fixture file and produce a multi-file patch. Click and keyboard-activate the badges; independently compare Files-tab owner/path/content hash with the owning checkout. Opening twice reuses the tab and never sends a prompt.
- Repeat from a Side chat and a Board ticket, then with a remote fixture host containing a same-named path with different bytes. The correct owner/host wins. Preserve draft text, attachment/queue state, tool expansion and an existing unsaved editor.
- Exercise deleted files, Unicode/spaced paths and paths the current resolver refuses. Show truthful failure/inert behavior; no fallback local read, external scheme or new path permission. Tab/Enter/Space work once, with visible focus and a useful AX label.
- Retain `tool-file-navigation-e2e.json`, tab/host/owner receipts, expected/actual content hashes, draft/expansion before/after values, native AX screenshots and a short click/keyboard recording.

## ZI-05 — search workspace text and open the matching line

**P2 · not started. Owners:** existing `WorkspaceFiles` read scope/RPC, file search or a scoped mode of the existing command palette, and Files editor navigation. ZI-04's owning-chat file route can be reused; it is not a prerequisite to a correct host search. L14/IC-5 remain transcript-search owners; RM-06 consumes the host capability for phone UI later.

**Outcome:** the user can find a literal symbol, error string or config value in the selected checkout, then open its file at the matching line. Results remain attached to the correct host/checkout and communicate incomplete work honestly.

### Failure contract before code

1. A result from an old query/chat/root arrives late and opens the wrong checkout, especially when local and remote roots share path names.
2. A query reads outside approved roots, follows an escaping symlink or scans `.git`, ignored/private files or another profile without the existing explicit read scope.
3. Regex/command/query syntax is inferred from literal input, altering spaces, backslashes or punctuation, or executing a shell command. Binary/oversized files and huge lines cause unbounded reads, matches or allocations.
4. Partial indexing, elapsed-budget exhaustion, host failure or result limits appear as a complete zero-match answer.
5. UTF-8 byte columns are treated as editor character positions, a trimmed preview points at the wrong column, or files changed/deleted after search show a falsely precise stale match.
6. Name/content/history results are confused, search steals a queued draft, or a cache/watch/index survives root removal indefinitely.

### Work

1. Add a bounded literal text-search operation to the existing owning-host workspace service. Reuse target/profile/checkout read authorization and ignore semantics. Return root-relative path, 1-based line, explicit source byte ranges, bounded preview and truncated/unavailable state; preserve literal query bytes and document smart-case behavior. Skip binary/oversized content and report partial coverage when a limit prevents a complete answer.
2. Run traversal/reads off the UI and async core, with cancellation, elapsed/file/byte/result budgets and bounded per-file matches. Benchmark small/large representative checkouts before choosing an index. Prefer existing dependencies initially; any added index must share per-root ownership, bound watchers/cached contents, invalidate changes and evict idle roots. Do not warm/index a home directory or every project by default.
3. Add an explicit workspace-text search mode to the existing Files search or command palette. Label it distinctly from file-name and recorded-content searches; preserve current Board, settings, history and workbench actions. Debounce and generation-fence requests without replacing stable result identity/focus on each response.
4. Open the exact owning-chat/host file through existing Files tabs and navigate to the actual line/range, correctly translating UTF-8 ranges. Re-read/validate changed bytes before claiming an exact highlight; changed/deleted files show a truthful fallback. Preserve unsaved-editor handling and composer/queue drafts.
5. Older or unavailable hosts show a capability/unavailable result, not local fallback or zero matches. Search snippets stay in ephemeral host/UI result state; do not add a synced source-content store or activity-history indexing side effect.

### E2E gate and artifact

- Search a disposable repository/plain-folder fixture containing Unicode, spaced paths, repeated literal strings, mixed case, punctuation and tabs. Compare path/line/ranges with an independent literal-search oracle; exercise a live changed file, deletion and result activation into the real native editor.
- Search a remote fixture containing different bytes at the same relative paths as the Mac. Rapidly switch chats/queries during delayed responses. Only the matching host/root/query generation may publish or navigate. Existing conversation/history search and ordinary palette actions still work.
- Exercise ignored files, `.git`, escaping symlinks, binary/oversized files, long lines, result/time limits, cancellation and unsupported/offline hosts. Partial/unavailable differs from complete no matches; no unapproved read or new shell execution occurs.
- Retain `workspace-text-search-e2e.json`, canonical match manifests, source/editor range receipts, owning-host identity and draft/unsaved-state readback, native keyboard/AX evidence and a recording of search-to-line navigation. Measure cold/warm latency, bytes, peak footprint, concurrent requests and any idle eviction against the same current LUNA baseline. Do not adopt upstream's latency or memory targets as measured LUNA results.

## Delivery boundary

Implement and review the tasks independently, beginning with ZI-01 then ZI-02. Pursue ZI-03/ZI-04/ZI-05 after confirming their narrow UI contracts against current main; do not bundle unrelated cosmetic parity. Run the affected crate's required locked compile/build, focused E2E and configured formatting/lint checks. After each meaningful runtime change, refresh and verify the exact **LUNA Dev** bundle/process/IPC/signature as required by AGENTS.md; report build duration and real limitations. Hash/retain reviewed evidence and clean only scoped disposable fixtures/QA bundles, then use commit-and-push. This document itself is a specification, not evidence that these gates have passed.
