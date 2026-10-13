# ZI-05: search checkout text and open the exact matching line

Baseline: main `8758745a`. Shared queue owns status. Specification:
[implementation § ZI-05](../implementation.md#zi-05--search-workspace-text-and-open-the-matching-line),
[research § ZI-05](../research.md#zi-05-search-source-text-not-only-names-or-recorded-conversations).

## Gap at the baseline

| Surface | What it searches | Text-to-line? |
| --- | --- | --- |
| `SearchWorkspaceFiles` (`crates/engine/src/workspace_files.rs`) | Path/name fuzzy rank over the authorized checkout | No: path, name, kind, score |
| Files search box (`crates/ui/src/files/search.rs`) | That RPC | No |
| Command palette (`crates/ui/src/shell/command_palette.rs`) | Commands, chats, recorded activity (L14) | No: transcripts only |

No on-disk source search existed, so finding an error string or config key in
the owning checkout needed an agent turn or a terminal. The gap is one
operation on the existing host service plus a mode of the existing Files
search, a palette entry and the existing editor. No index, watcher, cache,
dependency or second file service is added.

## Contract

### Operation

`SearchWorkspaceText` (`luna.core/search-workspace-text` v1): query, unary,
device-addressable, remote scope `files-read`, 30 s forward deadline. Request
(`SearchWorkspaceTextRequest`): the Files `WorkspaceTarget` (chat or space),
`query`, `includeIgnored`, `limit`, optional `budget` (`maxFiles`, `maxBytes`,
`maxElapsedMs`, each clamped to the host maximum), `searchSession` and
`generation`. Reply (`WorkspaceTextSearchResult`): `checkoutId`, echoed
`query`/`generation`, `caseSensitive`, `status` (`complete` | `partial` |
`superseded`, unknown values read as incomplete), `limits`, grouped `files`
(`path`, `contentHash`, `matches`, `matchesTruncated`), counts
(`matchCount`, `filesScanned`, `bytesScanned`, `skippedBinary`,
`skippedLarge`, `skippedUnreadable`) and `elapsedMs`.

### Authorization, ignore and symlinks

- The target resolves through `WorkspaceFiles::resolve_target`, the same code
  as every Files read: chat/space must belong to this device, the folder must
  be the project or one of its registered worktrees, plain folders keep their
  device-scoped identity. A remote caller needs `files-read`. An addressed
  device that cannot be reached is an error (`remote routing unavailable` or
  a transport error), never a local search.
- The walk uses the Files tree's `ignore` configuration: `.gitignore`, git
  excludes and global excludes; hidden files are shown. `includeIgnored`
  (the existing "Show all files" toggle) disables the standard filters.
- `.git` (any case, directory or worktree file) is never entered, in either
  mode. Internal `.luna-save-*.tmp` files are skipped.
- Symlinks are never followed (`follow_links(false)`, symlink entries are not
  files). The final component is opened `O_NOFOLLOW | O_NONBLOCK`, so an
  entry swapped for a symlink or FIFO after the walk is refused or skipped;
  each file's parent is canonicalized against the canonical root once per
  directory. A component swapped in the instant between that check and the
  open is the residual race shared with `ReadWorkspaceFile`.

### Literal query

- The query is the user's bytes: never trimmed (the UI skips only an
  all-blank input), never a pattern, never a command. 1–256 characters, one
  line (no `\n`/`\r`), no NUL.
- Smart case: any uppercase character (Unicode `is_uppercase`) makes the
  search case-sensitive. Otherwise ASCII letters match case-insensitively and
  every other character must match exactly (no Unicode folding, so offsets
  never move).
- Matches are leftmost and non-overlapping (`aa` in `aaaa` is two matches).

### Lines, byte ranges and previews

- `line` is 1-based, split on `\n`. A line's text excludes `\n`/`\r\n`; on
  line 1 it also excludes a UTF-8 BOM. That is exactly the text a Files editor
  shows (it strips the BOM and normalizes CRLF).
- `byteStart..byteEnd` are UTF-8 byte offsets in that line text; `column` is
  the 1-based character column, for display only.
- `preview` is at most 48 bytes before the match (never before the line's
  leading indentation), up to 160 bytes of the match and 96 bytes after, cut
  on character boundaries; `previewMatchStart..End` locate the match in it and
  `previewTruncatedBefore/After` say whether line text was cut.
- `contentHash` is the SHA-256 of the searched bytes, the same hash
  `ReadWorkspaceFile` reports, so the UI knows whether it opened the bytes that
  were searched.

### Budgets and truthful status

| Budget | Host maximum | As-you-type pass (UI) | When reached |
| --- | --- | --- | --- |
| Files opened | 50,000 | 25,000 | `partial` + `files` |
| Bytes read | 256 MiB | 128 MiB | `partial` + `bytes` (stops before a file that does not fit) |
| Elapsed, from arrival (queueing included) | 5,000 ms | 1,500 ms | `partial` + `elapsed` |
| Reported matches | 500 (`limit`, default 200; UI asks 200) | 200 | `partial` + `results` |
| Matches per file | 20 | 20 | `partial` + `fileMatches`, `matchesTruncated` |
| File size | 8 MiB (the largest file Files can display) | same | larger text files: `partial` + `largeFiles`; binary ones are not gaps |
| Reply size | ≈512 KiB estimated | same | `partial` + `results` (relay messages are capped at 1 MiB) |
| Concurrent scans per host | 2 | — | further requests queue inside their own elapsed budget |
| RPC deadline | 8 s (elapsed + 3 s) | — | error |

Binary (a NUL anywhere) and non-UTF-8 files are counted in `skippedBinary`:
Files cannot show them as text either, so they are not coverage gaps.
Unreadable entries (permission, escape, non-UTF-8 path, walker error) set
`unreadable`. `complete` means every eligible file was searched and every
match fit; zero matches is then "No matches in N files". `partial` with zero
matches reads "No matches yet · <reasons>". The UI offers **Search all files**
(host maxima) when an as-you-type answer stopped at `elapsed`/`files`/`bytes`.

### Generations, cancellation and retention

- The UI keeps one `searchSession` (UUID) per Files surface and a monotonic
  `generation`. The host admits a request only if its generation is not older
  than the session's latest; admitting a newer one cancels the session's
  running scan (its answer is `superseded`, with no files). A late older
  request is answered `superseded` without scanning.
- A scan stops when its RPC is dropped (timeout or closed connection), when
  superseded, or at shutdown (then the call errors).
- The UI publishes an answer only if its generation, query and complete
  workspace context (target, owning device, cwd, checkout id) still match. A
  new keystroke, mode switch, root/host change or ignore-scope change drops the
  in-flight task and re-runs the current query against the new target.
- Nothing is indexed, cached or watched. The host retains the last generation
  of at most 256 sessions (an integer and an optional token each); snippets
  live only in the Files surface's in-memory result.

### Navigation

A selected match opens through the existing Files route: a browser surface
promotes itself to an editor, an editor tab of the same file navigates in
place, and another file emits `FilesEvent::OpenFileAt`, for which the shell
opens or focuses that owning chat's file tab and navigates it. Navigation waits
until the document is loaded; a clean copy whose hash differs from the
searched hash is first re-read from the owning host; unsaved edits are never
reloaded. It then re-locates the literal in the text the editor (or read-only
preview) shows:

| Outcome | Selection | Notice |
| --- | --- | --- |
| Exact, same bytes | the byte range | none |
| Exact, file differs (or another checkout) | the byte range | "…the match was re-verified" |
| Moved | the nearest occurrence (fewest lines away) | "…the nearest match is on line N" |
| Missing | cursor at the searched line, nothing selected | "…“query” is no longer in it" |
| File deleted / unreadable / image / no longer text | none | specific message |

Markdown files switch to source. Editable files select the UTF-8 range in the
editor and scroll the row to the viewport centre (to the nearest edge with
soft wrap). Read-only previews (over 1 MiB, mixed line endings) scroll to and
tint the row.

## Failure contract before code

| # | Credible failure (spec item) | Contract | Owning proof |
| --- | --- | --- | --- |
| F1 | A late answer for an old query/chat/root/host opens the wrong checkout (1) | UI fence on generation + query + full context; host session supersession; navigation re-verifies bytes and checkout identity | UI `late_answers_never_publish…`, `changed_vanished…` host switch; engine `…fences_generations…`; remote `…answers_from_the_owning_host` |
| F2 | Reads outside the root, escaping symlinks, `.git`, ignored/private files, another device's chat/space (2) | Files authorization; ignore semantics; no symlink following; `O_NOFOLLOW`; `.git` filtered | engine oracle test (git `ls-files` oracle, symlinks in and out of root, `.git` probe), `…shares_files_read_authorization` |
| F3 | Literal input treated as regex/shell, spaces/tabs/backslashes altered (3) | Byte-literal needle; validation only rejects empty, multi-line, NUL, >256 chars | engine oracle over punctuation, `  needle`, `\tneedle`, `$(touch …)` with sentinel |
| F4 | Binary/oversized files or huge lines cause unbounded work (3) | 8 KiB binary probe; 8 MiB file cap; per-file, result, byte, file, elapsed and reply budgets; bounded preview | engine budgets test; 105 KB single-line file preview |
| F5 | Partial, elapsed, host failure or limits look like "no matches" (4) | `status` + `limits`; UI states Unsupported/Unreachable/Failed; distinct summaries | engine `…every_budget_as_partial`; UI `changed_vanished_capped_and_unavailable…` |
| F6 | UTF-8 byte columns used as characters; trimmed preview mispoints (5) | Byte ranges in line text; preview offsets; editor `set_selected_range` (UTF-8) | engine oracle (columns, BOM, CRLF); UI exact selection after multibyte text; `locate_text_match` table |
| F7 | Changed/deleted files show a falsely precise match (5) | `contentHash`; re-read clean stale copies; Exact/Moved/Missing; deleted → notice, no selection | UI moved and vanished cases; engine hash/delete readback |
| F8 | Name/content/history results confused; search steals a draft (6) | Separate toggle, placeholder, labels; palette entry distinct from recorded-content entries; Files input is not the composer | UI tests; native QA |
| F9 | A cache/watch/index survives root removal (6) | None exists; bounded session table only | code review |
| F10 | Superseded or dropped scans keep consuming the host | Token cancelled on supersede, drop, shutdown; 2 permits | engine `…fences_generations…` (race-tolerant invariant) |
| F11 | Older or offline hosts | `UnknownMethod` → "older LUNA" state; routing failure → "unreachable"; no local fallback | UI states; `remote_target_without_links_fails_clearly` |
| F12 | Unicode/spaced paths break routing | Wire paths are UTF-8 relative paths; non-UTF-8 paths are `unreadable` | engine fixture (`src/unicode café/naïve file.rs`), UI root `plain folder ü` |

Authoring gate: one engine owner (`crates/engine/tests/workspace_files.rs`)
drives the real RPC against temp repositories with two independent oracles —
`git ls-files --exclude-standard` for eligibility and a naive per-line byte
scan written from this contract (no `ignore`, no `str::find`). One relay case
in `device_routing.rs` owns host routing. The UI owner
(`crates/ui/src/files/text_search_e2e.rs`) drives the Files surface against the
real engine over the RPC transport and reads the real editor's selection. The
pure `locate_text_match` keeps one table at its boundary. The request
`budget` is a host contract for constrained callers (the UI's as-you-type
pass; RM-06 later), which is also how the engine test reaches the file, byte
and elapsed limits without 50,000-file fixtures.

## Measured budgets

Read-only on 2026-10-13, Apple M3 Max (14 cores, 36 GiB), macOS, APFS. The
eligible set is what the Files walk sees (`rg --files --hidden -g '!.git'`,
the same `ignore` crate). Scan times are a ripgrep proxy (`rg -j1 --hidden
-g '!.git' -F -i -c --max-filesize 8M`, single thread, mmap and SIMD), not
LUNA measurements; the probe below records LUNA's own numbers.

| Repo | Eligible files | Eligible bytes | Text / binary (8 KiB NUL probe) | Largest text file | Text > 8 MiB | rg first pass | rg warm |
| --- | --- | --- | --- | --- | --- | --- | --- |
| luna | 3,378 | 119.6 MiB | 3,058 / 348 | 0.87 MiB (`shell.rs`) | 0 | 0.22–0.44 s | 0.07–0.11 s |
| codex | 8,811 | 83.0 MiB | 8,802 / 9 | 1.56 MiB (`MODULE.bazel.lock`) | 0 | 1.18–1.33 s | 0.19–0.24 s |
| opencode | 6,563 | 118.5 MiB | 6,227 / 336 | 4.72 MiB (JSON fixture) | 0 | 0.65–0.81 s | 0.14–0.18 s |
| t3code | 27,786 | 307.2 MiB | 27,493 / 293 | 3.10 MiB (generated TS) | 0 | 3.43–4.04 s | 0.73–0.85 s |
| valorant-tracker | 194 | 14.8 MiB | 127 / 67 | 0.48 MiB (JSON fixture) | 0 | 0.04 s | < 0.01 s |

rg searched 51.6 MB (luna), 85.9 MB (codex), 48.3–53.3 MB (opencode) and
198.6–204.2 MB (t3code): binary files stop at the probe. The parallel rg pass
on t3code took the same 0.85 s, so the walk, not matching, dominates.

Choices: 50,000 files and 256 MiB cover the largest representative checkout
(t3code: 27,786 files, ~195 MiB actually read because binary files stop at
the probe) with ~1.8× / 1.3× headroom; 5 s covers its cold pass; the
interactive pass (25,000 files / 128 MiB / 1.5 s) completes the other four
repos and reports t3code honestly as partial with **Search all files**. 8 MiB
per file includes every measured text file (the largest is 4.72 MiB), so no
representative checkout is reported partial for size, and equals the Files
preview limit. Two permits keep a host's disk and CPU available while a user
types in two surfaces. No index is
justified: warm full scans of the largest repo are sub-second with the same
walker, and upstream's index (53 lockfile entries, native builds, watcher,
retention) is not adopted. Upstream latency/memory targets are not LUNA
results.

## Delivery

Owned (`engine/workspace-files`, `ui/files`, `ui/palette`):

- `crates/engine/src/workspace_files/text_search.rs` (new): validation,
  budget resolution, session admission/supersession, walk, bounded reads,
  literal matching, previews, status.
- `crates/engine/src/workspace_files.rs`: `pub mod text_search`, session table
  and scan permits on `WorkspaceFilesInner`, cleared at shutdown.
- `crates/ui/src/files/text_search.rs` (new): text mode state, fenced requests,
  summaries, result rows, `TextNavigationTarget`, `locate_text_match`.
- `crates/ui/src/files/client.rs`: `search_text` and `TextSearchError`
  (Unsupported / Unreachable / Failed).
- `crates/ui/src/files/search.rs`: mode and text state on `FileSearchState`.
- `crates/ui/src/files/mod.rs`: `FilesEvent::OpenFileAt`, mode toggle button
  ("Search text in files", toggled state), input routing by mode, restart on
  root/host/ignore-scope change (also fences the name search).
- `crates/ui/src/files/preview.rs`: pending navigation, re-read of stale clean
  copies, editor selection and centering, read-only row tint, notice banner.
- `crates/ui/src/shell/command_palette.rs`: "Search text in this task's
  files" entry (shown for a non-empty query when the active chat has a
  workspace) that opens the chat's Files surface in text mode.

Shared, additive hunks:

- `crates/proto/src/entities.rs`: request/result types.
- `crates/rpc/src/lib.rs`: `SEARCH_WORKSPACE_TEXT` and its `ALL` entry.
- `crates/rpc/src/operations.rs` + `crates/rpc/contract/v1/operations.json`:
  catalog row (regenerate to confirm).
- `crates/engine/src/rpc.rs`: handler arm with the 8 s deadline.
- `crates/ui/src/shell.rs`: two `OpenFileAt` arms and `open_file_surface_at`.
- `crates/engine/tests/device_routing.rs`: one test and one assertion.

Tests and tooling: `crates/engine/tests/workspace_files.rs` (+4 tests),
`crates/ui/src/files/text_search_e2e.rs` (new, 2 tests),
`crates/engine/examples/workspace_text_search_probe.rs` (new).

## Criteria mapping

| Criterion | Evidence |
| --- | --- |
| contract | This file: authorization/ignore/symlink, literal query, byte/range and budget contracts with measured repos; F1–F12. |
| delivery | Existing `WorkspaceFiles` host service and Files search/palette; generation fences on query/root/host; cancelled bounded scans; re-validated navigation. |
| verification | Engine oracle, budgets, fence and authorization tests; relay owner-host test; UI E2E with real engine and real editor selection; probe JSON for latency/bytes/RSS/concurrency; native QA below. |
| handoff | Orchestrator: commands, exit codes, hashes, queue, delivery and LUNA Dev refresh. |

| Test | Failure modes |
| --- | --- |
| `workspace_text_search_matches_an_independent_literal_oracle` | F2, F3, F4 (long line), F6, F7 (hash/delete readback, no cache), F12 |
| `workspace_text_search_reports_every_budget_as_partial` | F4, F5: elapsed 0 vs complete zero, results, files, bytes, per-file cap, large text vs large binary, unreadable (skipped as root) |
| `workspace_text_search_fences_generations_and_rejects_bad_requests` | F1, F10, F3 validation, session bounds |
| `workspace_text_search_shares_files_read_authorization` | F2: other device chat/space, missing chat, checkout path outside |
| `device_routing::workspace_text_search_answers_from_the_owning_host` | F1, F11: same chat id/path, different bytes on two hosts; hashes and checkout ids match each host's own read |
| `device_routing::remote_target_without_links_fails_clearly` (+1 assertion) | F11: offline owner is an error |
| `files::preview::text_search_e2e::late_answers_never_publish_and_matches_select_exact_editor_bytes` | F1, F6, navigation in place and via `OpenFileAt`, row 400 revealed |
| `files::preview::text_search_e2e::changed_vanished_capped_and_unavailable_answers_stay_truthful` | F5, F7, F11, host switch mid-flight |
| `files::text_search::tests::located_ranges_follow_utf8_bytes_and_report_changed_text_truthfully` | F6, F7 table for the pure re-locator |

None of these were run by the author. Against the baseline they fail to
compile (no operation, types or UI mode): the gap is the operation itself.

## Commands for the orchestrator

```sh
scripts/cargo.sh run --locked -p luna-rpc --features schema --example contract
git diff --stat crates/rpc/contract   # expect only the new catalog row
scripts/cargo.sh check --locked -p luna-proto -p luna-rpc -p luna-engine --all-targets
scripts/cargo.sh check --locked -p luna-ui --tests
art="target/qa/zi-05/$(git rev-parse --short HEAD)"; mkdir -p "$art"
CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-rpc --features schema --test contract 2>&1 | tee "$art/rpc-contract.log"
CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-rpc --lib 2>&1 | tee "$art/rpc-lib.log"
CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-engine --test workspace_files 2>&1 | tee "$art/engine-workspace-files.log"
CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-engine --test device_routing -- workspace_ remote_target_without_links 2>&1 | tee "$art/engine-device-routing.log"
CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-engine --lib workspace_files 2>&1 | tee "$art/engine-lib.log"
CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-ui --lib files:: -- --test-threads=1 2>&1 | tee "$art/ui-files.log"
CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-ui --lib command_palette -- --test-threads=1 2>&1 | tee "$art/ui-palette.log"
scripts/cargo.sh clippy --locked -p luna-engine -p luna-ui --all-targets -- -D warnings
GIT_OPTIONAL_LOCKS=0 scripts/cargo.sh run --locked -p luna-engine --example workspace_text_search_probe -- \
  "$PWD/$art/workspace-text-search-bench.json" "$PWD" \
  /Users/iulian/workspace/code/personal/codex /Users/iulian/workspace/code/personal/opencode \
  /Users/iulian/workspace/code/personal/t3code /Users/iulian/workspace/code/personal/valorant-tracker
shasum -a 256 "$art"/*.log "$art"/*.json > "$art/sha256.txt"
scripts/run-macos-dev.sh --refresh
```

Expected: the contract regeneration adds only the
`luna.core/search-workspace-text` row (already hand-applied); 4 new tests pass
in `workspace_files`, 2 selected in `device_routing` (with the other
`workspace_` relay test), 3 new UI tests pass with the existing Files and
palette suites. The `locked.txt` assertion degrades to "readable" when tests
run as root. The probe uses the dev profile (workspace crates at
`opt-level = 0`, as in LUNA Dev); append `--release` only if a production-speed
number is wanted and the build budget allows. The probe registers each root as
a disposable plain-folder space in a temp data dir and never writes to it.

## Native QA (LUNA Dev)

Use a disposable repository (e.g. `cp -R` of a fixture under `/tmp`) with: a
Unicode/spaced path, a line with multibyte text before a match, a CRLF file, a
file over 1 MiB containing the literal, a `.gitignore`d file and a symlink to a
file outside the repo.

1. Open a chat on that repo, open Files, press the **Search text in files**
   toggle (VoiceOver: "Search text in files, toggle button, on"). Type a
   literal with punctuation and two leading spaces; confirm the summary
   ("N matches in M files") and that ignored/symlinked files are absent; toggle
   "Show all files" and confirm the ignored file appears.
2. Use ↓/↑ and Enter on a match after multibyte text: the editor opens with
   exactly the literal selected (read with AX `AXSelectedText`) and the row
   visible; repeat for the > 1 MiB file (read-only preview, tinted row).
3. With the result list open, edit the file on disk (insert a line above the
   match) and activate the stale row: expect the "nearest match is on line N"
   notice and the shifted selection. Delete another matched file and activate
   it: expect "could not be opened" and no selection. Make an unsaved edit in
   an open tab and activate a match in it: the edit is kept.
4. Command palette (⌘K): type the literal, choose **Search text in this
   task's files**: Files opens in text mode with the same query; recorded
   content entries and ordinary actions are unchanged; the composer draft is
   unchanged.
5. A chat owned by a paired remote device (fixture host with the same relative
   path and different bytes): results show the remote bytes; opening a match
   shows the remote file. Stop the remote host and search again: the summary
   says the device is unreachable, never "No matches".
6. Retain a short recording of search-to-line navigation, AX readback
   (selected text, focused element), screenshots and the probe JSON; hash them
   into `$art/sha256.txt`.

## Limitations

- The elapsed and file/byte maxima are reached through the request budget,
  not real 5 s / 50,000-file / 256 MiB scans. Mid-scan cancellation is
  asserted as an invariant that holds whichever way the race goes; a scan's
  wall-clock stop after supersede is code-reviewed.
- Smart case folds ASCII only (`É` does not match `é`). Multi-line literals
  are rejected rather than searched.
- Read-only previews tint the matched row instead of selecting the range.
  Soft-wrapped editors scroll the match to the nearest edge, not the centre.
- A component swapped for a symlink between the per-directory check and the
  open is the same residual race as the existing Files read path. Windows
  opens without `O_NOFOLLOW` (not built here).
- Dev builds (`opt-level = 0` for workspace crates) scan noticeably slower than
  release; on large checkouts the 1.5 s as-you-type pass may report `partial`
  sooner in LUNA Dev than in a release app.
- `maxFiles`/`maxBytes` have one desktop caller (the as-you-type pass); RM-06
  is the intended constrained consumer. No phone UI is added here.
- No keyboard shortcut was added (the shared shortcut map is untouched); the
  toggle and palette entry are the entry points.
