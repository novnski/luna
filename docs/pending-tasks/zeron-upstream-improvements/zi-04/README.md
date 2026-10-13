# ZI-04: open typed tool files in the owning Files surface

Baseline: main `f2e2c848`. Shared queue owns status. Specification:
[implementation § ZI-04](../implementation.md#zi-04--open-typed-tool-files-in-the-existing-files-surface),
[research § ZI-04](../research.md#zi-04-connect-tool-file-badges-to-the-shipped-file-route).

## Gap at the baseline

| Surface | Baseline | Gap |
| --- | --- | --- |
| Tool chip header (`crates/ui/src/transcript.rs`, `chip_header_row`) | Read/write/edit/patch/image calls show a file badge with hover tint | No activation, no focus target, no label: the badge is part of the card's expand button |
| Workspace link route (`crates/ui/src/shell.rs`, `activate_session_link` → `open_workspace_file_link` → `add_file_surface_for_owner`) | Markdown file links open the owning chat's Files tab, deduplicated per (pane, owner, path) | Not reachable from a badge; rejects every link from a Side chat's workbench pane (the side-chat move into the workbench dropped the old Related-panel source check); a file already shown by the Files place opens a second buffer |
| Resolver (`crates/ui/src/workspace_links.rs`) | Root-relative, escape-refusing, reads `:line`/`#L` suffixes from Markdown targets | A raw tool path such as `src/lib.rs:12` would open `src/lib.rs`, not the file the tool named |

No new backend, reader, editor, path authority or dependency is added.

## Contract

### Activation

- A badge exists for typed paths only: `ReadFile`, `WriteFile`, `EditFile`,
  `ApplyPatch { path: Some }` and `ViewImage`. Each Codex/OpenCode multi-file
  change already arrives as one tool part per file (`f3`, `f3#1`, …), so every
  badge names its own file. Output, prose and `ApplyPatch { path: None }`
  never become links.
- The badge is its own control inside the card header: role Button, label
  `Open <checkout path> in Files`, tab stop with the inset focus ring, full
  typed path as tooltip, one `on_click` (pointer, Enter, Space; GPUI's single
  keyboard click). It stops propagation, so the card never expands or
  collapses and the header's own button keeps its label and behavior.
- Activation goes through the view's existing `LinkUi` route (the same one as
  Markdown file links) with `LinkAction::Primary`, `navigation: Err` (never a
  web destination), `source_session` = the owning chat: the main view's
  selected chat, a workbench pane's own chat, a subagent tab's parent chat.
  Never the shell's current selection. A view without the route (the Board
  authoring discussion) keeps today's plain badge.

### Resolution and authority

- The typed path is wrapped as a canonical `luna-file:` target
  (`workspace_links::file_mention_target`) and classified with the shell's own
  `resolve_workspace_file_link` against the owning chat's `cwd`, so the badge
  offers exactly what the shell will open, and file names are never read as
  `:line`, `#L` or URLs.
- Refused at render time (inert, `Role::Label`, reason in the tooltip and the
  accessible label; a click keeps today's card behavior):
  - `This chat has no workspace folder`;
  - `Outside this chat's workspace` (absolute path not under the root,
    component-wise: `/work/app-other` is outside `/work/app`);
  - `Not a workspace file path` (`..`, `.`, empty components, `\`, `:`,
    control characters, a directory, the root itself, empty).
- The shell re-resolves on activation (the same resolver) and opens
  `add_file_surface_for_owner(owner, path)`: existing tab dedup per (pane,
  owner, path), the Files place if it is already that chat's editor of the
  file, else a new editor tab. An existing tab is only focused: no reload, an
  unsaved buffer stays. Nothing reads files in the transcript.
- A Side chat pane is accepted as a link source only while its view is live in
  the current Main's workbench (`workbench.has_view`); views exist only for
  tabs in the layout. The stale/switching guards are unchanged
  (`selected_chat == active_chat`, other sources rejected).
- Host and bytes are the Files surface's own: `FilesRequestContext::for_chat`
  addresses the owning device (`targetDeviceId`) for a remote chat, and the
  host enforces root, symlink and regular-file rules. Deleted files open a
  tab with the host's `file not found`; a symlinked file is a read-only entry
  without text; a symlinked folder is not traversed; an unreachable host is
  the Files tab's existing error. There is no local fallback read.
- If the owning surface declines an activation (the chat stopped being the
  one shown, its folder changed), the badge shows `Couldn't open in Files
  from here` beside it until the next activation.

## Failure contract before code

| # | Credible failure (spec item) | Contract | Owning proof |
| --- | --- | --- | --- |
| F1 | Activation bubbles into expansion, fires twice, moves the caret or loses a queued draft (1) | Own button; `stop_propagation`; GPUI single keyboard click; route opens Files only | UI `badges_hand_their_own_file…` (card state, one activation per click/Enter/Space, header control still toggles); e2e draft + no send/queue/write RPC |
| F2 | A side/Board badge opens under the selected main chat; a remote path is read from the viewing Mac (2) | `source_session` = owning chat; shell resolves under that chat's `cwd`; workbench pane accepted as source; Files addresses the owning device | UI side-pane activation names the side chat; e2e `a_side_chat_badge…` (side bytes, not main's same-named file); e2e `a_remote_chat_badge…` (remote bytes, local decoy with same chat id/path never asked) |
| F3 | Arbitrary strings, escaping paths, URLs or unsupported host paths become file reads; a deleted file looks opened (3) | Typed paths only; shell resolver over the exact path; inert reasons; host refusals shown by Files | UI table `typed_paths_open_only_as_written…`; e2e: `/etc/hosts` never read, symlink file/folder refused with no outside bytes in any reply, deleted file read errors `file not found` |
| F4 | Repeated activation duplicates tabs, breaks unsaved prompts or replaces another editor's draft (4) | Existing (pane, owner, path) dedup; focus without reload; Files-place editor reused | e2e: 4 badges → 4 tabs, repeats keep 4, typed unsaved buffer survives re-activation |
| F5 | A truncated/multibyte path cannot be discovered or keyboard-activated; the whole card becomes an ambiguous file button (5) | Badge-scoped button with checkout-path label and full-path tooltip; header button unchanged | UI: `src/naïve café.rs` via click, Enter, Space; native QA VoiceOver |

Authoring gate (test-audit): two owners, no production seam. The transcript
owner (`crates/ui/src/transcript/file_badges.rs`, tests module) paints real
badges in a main transcript and a side-chat pane transcript and drives them
by pointer and keys; its route records what the badge hands over and reads the
transcript's real card state (the only place it exists). The shell owner
(`crates/ui/src/files/tool_file_open_e2e.rs`, a child of `shell`) clicks real
badges in the production main transcript and reads the real Files tabs and
editor; two real `EngineCore`s answer behind a relay that dispatches only on
the UI's `targetDeviceId`, with different bytes at the same relative paths, so
misrouting is a wrong SHA-256 (computed in the test from the bytes it wrote).
The Side chat case calls `activate_session_link` with the pane transcript's
own `LinkUi` inside the update that mounts the pane (the test platform has no
workbench engine); the badge half of that chain is the transcript owner's.
`debug_selector` on the badge is the GPUI test-build bounds lookup already
used for right-pane tabs (a no-op in release).

## Delivery

Owned (`ui/transcript`, `ui/files`):

- `crates/ui/src/transcript/file_badges.rs` (new): typed path selection,
  `badge_target` classifier, `Transcript::tool_file_badge`, `render_file_badge`,
  refusal notice; tests.
- `crates/ui/src/transcript.rs`: `mod file_badges`, `file_badge_refusal`
  field, per-paint badges in `render_tool_group`, `chip_header_row` /
  `chip_header` / `tool_chip` take the badge and render it plus the refusal.
- `crates/ui/src/files/tool_file_open_e2e.rs` (new): shell/Files E2E.

Shared, minimal hunks:

- `crates/ui/src/workspace_links.rs`: `file_mention_target` (uses the existing
  scheme and encoder).
- `crates/ui/src/shell.rs`: `activate_session_link` accepts a live workbench
  pane source; `open_workspace_file_link` focuses the Files place when it is
  already that chat's editor of the file; `#[cfg(all(test, unix))]` include
  of `files/tool_file_open_e2e.rs`.

Behavior change beyond badges: Markdown links (file and web) in a Side chat's
workbench pane now open like the Main's instead of being silently rejected.

## Criteria mapping

| Criterion | Evidence |
| --- | --- |
| delivery | One badge control per typed file → existing `LinkUi` → `open_workspace_file_link` (resolver) → `add_file_surface_for_owner` (dedup, unsaved handling) |
| verification | UI owner + e2e owner below (main/side/remote, read/write/multi-file, deleted/escaping/unsupported, hashes); Board ticket chats are main or side chats (native QA) |
| handoff | Orchestrator: commands, exit codes, hashes, queue, delivery and LUNA Dev refresh |

| Test | Failure modes |
| --- | --- |
| `transcript::file_badges::tests::typed_paths_open_only_as_written_inside_the_owning_folder` | F3, F5 (Unicode, spaces, `#`, `:line`, `..`, `\`, URL, directory, root, no folder) |
| `transcript::file_badges::tests::badges_hand_their_own_file_to_the_owning_route_once_without_toggling_the_card` | F1, F2 (side pane source/root), F3 (inert outside paths), F5, refusal notice |
| `shell::tool_file_open_e2e::badges_open_the_owning_chats_exact_file_once_and_keep_drafts_and_buffers` | F1 (draft, no send/queue/write), F3 (deleted, symlinks, outside), F4 |
| `shell::tool_file_open_e2e::a_side_chat_badge_opens_under_the_side_chats_own_checkout` | F2 (fails on the baseline: the shell rejected pane sources) |
| `shell::tool_file_open_e2e::a_remote_chat_badge_reads_the_owning_hosts_bytes` | F2, F4 |

None of these were run by the author. Against the baseline the UI tests fail
to compile (no badge control), and the side-chat activation is `Rejected`.

## Commands for the orchestrator

```sh
scripts/cargo.sh check --locked -p luna-ui --tests
art="target/qa/zi-04/$(git rev-parse --short HEAD)"; mkdir -p "$art"
CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-ui --lib file_badges -- --test-threads=1 2>&1 | tee "$art/ui-file-badges.log"
CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-ui --lib tool_file_open_e2e -- --test-threads=1 2>&1 | tee "$art/ui-tool-file-open-e2e.log"
CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-ui --lib workspace_links transcript_links_open_new_tabs_and_reject_stale_sessions markdown_preview_events_open_browser -- --test-threads=1 2>&1 | tee "$art/ui-link-regressions.log"
CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-ui --lib transcript:: -- --test-threads=1 2>&1 | tee "$art/ui-transcript.log"
CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-ui --lib files:: -- --test-threads=1 2>&1 | tee "$art/ui-files.log"
scripts/cargo.sh clippy --locked -p luna-ui --all-targets -- -D warnings
shasum -a 256 "$art"/*.log > "$art/sha256.txt"
scripts/run-macos-dev.sh --refresh
```

Expected: 2 new tests in `file_badges`, 3 in `tool_file_open_e2e` (unix
only), the existing link, transcript (including IC-5 Find) and Files suites
unchanged.

## Native QA (LUNA Dev)

Fixture: a disposable repository (`cp -R` under `/tmp`) containing
`src/naïve café.rs`, `notes/plan.md`, `src/a.rs`, `src/b.rs`, `src/gone.rs`,
`src/escape.rs -> /tmp/zi04-outside/secret.txt` and `linked -> /tmp/zi04-outside`.

1. Main chat (Claude or Codex, Full access): ask it to read
   `src/naïve café.rs`, create `notes/plan.md`, and change `src/a.rs` and
   `src/b.rs` in one patch (Codex shows two badges). Type a draft and queue a
   message first. Click each badge: the owning chat's Files tab opens on that
   path; `shasum -a 256` of the file equals the editor's Select All → Copy →
   `pbpaste | shasum -a 256` (LF files). The card stays collapsed; draft text,
   queued message and attachments are unchanged; nothing is sent.
2. Keyboard: Tab to a badge (focus ring on the badge, not the card), Enter and
   Space each open once; VoiceOver reads "Open src/naïve café.rs in Files,
   button". Card header Enter still expands the card.
3. Click the same badge twice: one tab. Type in that editor without saving,
   activate the badge again: same tab, edit kept. Open the file from the
   Files tree first, then click its badge: the Files place comes forward, no
   second tab.
4. Delete `src/gone.rs`, click its badge: the tab says the file was not found.
   `src/escape.rs`: read-only "Symlink targets are read-only."; a badge for a
   path under `linked/`: the host's refusal. Ask for `/etc/hosts`: the badge
   is inert, hover shows "Outside this chat's workspace · /etc/hosts",
   VoiceOver reads the reason.
5. Side chat in the workbench and a Board ticket's chat (Go to chat): repeat a
   read badge; the tab belongs to that chat (its worktree/checkout), and a
   same-named file in Main's checkout is not what opens.
6. A chat owned by a paired remote device whose checkout has the same
   relative path with different bytes: the remote bytes open. Stop the remote
   host and click again: the Files tab's unreachable error, never local bytes.
7. Retain `tool-file-navigation-e2e.json` (case, owner, host, path,
   expected/actual SHA-256, draft and expansion before/after), AX screenshots
   and a short click/keyboard recording; hash them into `$art/sha256.txt`.

## Limitations

- File names containing `:` (legal on macOS) are refused by the shared
  resolver and stay inert, as Markdown links already do; widening the
  resolver is outside this task.
- `ApplyPatch` without a path (a patch the harness could not split) and diff
  stat rows inside an expanded card stay plain; only typed per-file badges
  open.
- Paths outside the chat's `cwd` but inside another registered worktree of
  the same project are refused: the owning chat's folder is the authority.
- The Side chat E2E drives the shell route with the pane transcript's own
  route rather than a painted pane (no workbench engine in the GPUI test
  platform); the painted side-pane badge is the transcript owner's and
  native QA's.
- No default-editor launcher (spec Work 4).
