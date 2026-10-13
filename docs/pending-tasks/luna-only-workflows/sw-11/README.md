# SW-11: past work, skills and general-work scope (E03/E14)

Baseline: main `f2e2c848`, 14 October 2026. Live status belongs to
`pending_tasks.py`. Read with the SW-11 sections of the package
[implementation](../implementation.md#sw-11--migrate-skills-history-and-general-work-context)
and [verification](../verification.md#e14--past-work-and-general-work-scope-sw-11),
[SW-03](../sw-03/README.md) (reviewed skill guidance) and
[IC-5](../../interactive-chat-surfaces/ic-5/README.md) (Find in Conversation).

**Verification state.** Written without compiling or running anything. Every
command below is for the orchestrator; nothing here is evidence until it runs.

## Owners reused (no second index, search or store)

| Need | Existing owner |
| --- | --- |
| Carry local work into the signed-in profile | `LocalImporter` (`crates/engine/src/local_import.rs`): rows, docs, journals, ledger claims |
| Adopt a provider task | `PreviewProviderTaskImport` / `AdoptProviderTask` (`TaskIdentityService`, Codex only; metadata only, never `includeTurns`) |
| Find past work across chats | L14 derived index (`activity_history.rs`, `activity_events`, `SearchSessionHistory`) and the command palette's recorded-content rows |
| Find a passage inside the opened chat | IC-5 `transcript::find` (complete rows, off-screen reveal) |
| Fresh handoff | "Start new task with context…": `PortableFork` context transfer (preview → apply) or the draft-staged visible-context fallback |
| Skill migration | `install_skill` / `save_skill_document` (revision-checked copy), SW-03 guidance delivery |
| General-work scope | Spaces (device + folder), Git optional; the fresh task stays in its source's host, project and folder |

## Demonstrated gaps

1. **Provider imports are invisible and silent (migration).** `adopt_preview`
   materializes title, archive state and the binding, never the provider's
   earlier transcript (`fake-codex.sh` refuses `thread/read includeTurns`, as
   LUNA reads metadata only). `build_activity_envelopes` derives rows only
   from transcript entries and receipts, so an imported task has **zero**
   index rows: recorded-content search cannot surface it and nothing states
   that its history is not in LUNA.
2. **Past-work results are unattributed (journey).** The palette's
   recorded-content line was `Recorded content · project · model-or-"Unknown
   model" · outcome`: no provider, no date, no archive state, and an
   index row without a transcript message (above) would open with a dangling
   `navigate_to_message("")` and no explanation.
3. **A fresh handoff claims the past task's branch (journey/scope).**
   `new_context_transfer_target` clones the source row, so the new
   portable task carried the source's run-start `source_context`. Before it
   ever ran, it showed the past (typically merged) branch, matched that
   branch's pull request and, being unarchived, registered a PR watch for it.
4. **A fresh handoff hides the access level it keeps (scope).** The portable
   dialog said "Creates a new task on {provider · model · reasoning}" while
   the task keeps the source's `permission_mode`; an old Full-access task's
   mode was carried without being named.

5. **Real chats' rows carried no provider or model (native QA N3, found on
   `62da9b65`).** The index joined journaled `SessionStarted` metadata to
   transcript entries by `assistant_message_id`, but the run loop mints its
   own entry ids (`entry_id = new_id()` in `sessions.rs`); the provider's id
   names an entry only after a steer adopts it. Every real reply and prompt
   therefore indexed `harness`/`model` as absent, and the palette correctly
   omitted them. The J1 fixture and the older `session_history` case had set
   entry ids to the provider's id by hand, which hid the gap. Fix in
   `activity_history.rs`: an entry the task wrote itself (`created_at` not
   before the task) takes the provider every journaled run of the task
   reported, and that model only if the runs also agree. Mixed providers, a
   configured provider no journaled run reports, and copied history older
   than the task stay unknown; current settings are never used as history.

Verified without code change: local import already keeps rows verbatim and
publishes them after their docs and journals, so the index ingests imported
Codex/Claude history with journaled provider/model; skill install already
copies the reviewed tree byte-for-byte (its revision is path-relative); IC-5
already searches complete history and reveals off-screen rows; the portable
task already resumes no provider session and starts with an empty binding.
Native Claude session import is unsupported and refused explicitly.

## Failure contract (written before the code)

| ID | Behavior | Credible failure | Owner test |
| --- | --- | --- | --- |
| M1 | Local import keeps Codex/Claude provider settings, created/last-message times, project, host, archive state and run-start context; global search finds each passage with its journaled provider/model and recorded time | Rows published before journals (provider/model lost), archive/time/context dropped, imported content unsearchable | `local_import::imported_codex_and_claude_history_stays_attributed_and_hands_off_fresh` |
| M2 | A Codex provider import is found through exactly one boundary row (Withheld, provider, import time, project, "not copied into LUNA" reason, no message id); no transcript entry or other row is invented; archive kept; a provider-owned import binds no resume session; a Claude native import is refused before any task exists | Zero rows (invisible), a fabricated message, duplicate boundary rows, a resume session for a read-only import, a silent Claude task | `task_identity::provider_import_is_found_with_explicit_unavailable_history_and_no_session` |
| M4 | A real run's prompt and reply rows carry the provider and model its journaled runs reported; once runs disagree on the model, the provider stays and the model is unknown | Real chats' rows without provider/model (N3), or a later model guessed onto older rows | `session_history::real_run_rows_carry_the_provider_and_model_their_runs_reported` |
| M3 | Installing a reviewed skill to another provider keeps its tree revision | Install rewrites bytes, so a reviewed selection no longer names the destination's content | `skill_library_e2e::file_backed_provider_skill_lifecycle` (one added assertion) |
| J1 | A recorded-content row names project, provider and model only when recorded, the date and the task's archive state; a boundary row reads "History unavailable" | "Unknown model", no provider/date/archive, a boundary indistinguishable from content | `shell::exit_regressions::past_work_search_opens_imported_task_truthfully_and_hands_off_fresh` |
| J2 | A boundary row opens its task (no navigation target) and states the reason; a content row opens without that claim | Boundary refuses to open, opens silently, or a content row claims missing history | same |
| J3 | The fresh handoff from the task search opened carries that task's visible context into an unowned new-task draft; the past task stays archived | Handoff from the previously open task, or into the past task | same |
| H1 | The portable new task claims no branch/PR context before its first run, resumes no provider session, is not archived, runs with the settings chosen for it, stays on its source's host, project and plain folder, and has no binding | Old branch/PR shown and watched; provider session or archive inherited | M1 test (handoff half) |
| H2 | The portable dialog names the access level the new task keeps | Full access carried unseen | Native QA step N5 |
| F1 | Off-screen passage in the opened chat via Find | Only mounted rows searched | IC-5 C1/S01 owner tests and fixture (consumed, not duplicated) |

Authoring gate: each new test owns a distinct boundary. M1/M2 drive the real
engine (local import, native Codex metadata peer, RPC, SQLite index, context
transfer); J1–J3 drive the production palette, shell open path and composer
handoff over the index rows M1/M2 prove the engine writes. No production
seam is test-only: `command_history_attribution` is the render path's own
helper, `ShellDraftRpc::with_history` is test fixture code. Expected strings
are literals, never produced by the helper under test. Baseline failures:
M1 fails at `created.source_context == None`; M2 times out at "never became
searchable"; J1/J2 do not compile on baseline (the attribution helper is new)
and, by the render code they replace, showed "Unknown model" with no
provider, date or archive state.

## Delivered

- `crates/engine/src/activity_history.rs` (owned, engine/history):
  `import_boundary_event` adds one `Withheld` row per provider-imported task,
  keyed by the immutable first import binding (`import:{task}:{binding}`), at
  the import time, `harness` = the provider that owns the history, no model,
  title, path or provider content, `message_id` empty and an explicit
  `unavailable_reason`. Remote tasks keep the existing remote reason.
  `uniform_run_metadata` attributes a task's own entries to the single
  provider (and model) its journaled runs reported (gap 5). The live
  bootstrap, rebuild and backup projection all use this builder.
- `crates/ui/src/shell/command_palette.rs` (owned, ui/history):
  `history_result_attribution` / `Shell::command_history_attribution` render
  `Recorded content | History unavailable · project · provider · model ·
  date · outcome · Archived` (absent facts stay absent; another device keeps
  its short form).
- `crates/ui/src/shell.rs` (shared, additive hunks): `open_session_history_result`
  skips navigation for a row without a message and shows the row's
  `unavailable_reason` as a status notice after opening the task; test
  fixture `ShellDraftRpc` answers `SearchSessionHistory`; the journey test.
- `crates/engine/src/rpc/context_transfer.rs` (shared, one line):
  `new_context_transfer_target` clears `source_context`; the first dispatch
  stamps the checkout the task actually runs in.
- `crates/ui/src/shell/findings.rs` (shared, one line): the new-task dialog's
  config line adds `execution_view::permission_title(config.permission_mode)`.
- Tests: `crates/engine/tests/local_import.rs` (M1, hermetic assembly),
  `crates/engine/tests/task_identity.rs` (M2), one assertion in
  `crates/engine/tests/skill_library_e2e.rs` (M3),
  `crates/engine/tests/session_history.rs` (M4, a real scripted run).

No dependency, RPC method, operation catalog, document schema, persistence,
proto or `Cargo.lock` change. `crates/ui/src/transcript/*` is untouched (IC-5
is consumed as is).

## Commands and evidence

Run from the repository root; keep each exit code.

```sh
RUN="$PWD/target/qa/luna-only-workflows/sw11-$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p "$RUN"
git rev-parse HEAD > "$RUN/revision.txt"; git status --short > "$RUN/working-tree.txt"

scripts/cargo.sh check --locked -p luna-engine --tests 2>&1 | tee "$RUN/check-engine.log"; echo "check-engine exit=${PIPESTATUS[0]}"
scripts/cargo.sh check --locked -p luna-ui --tests 2>&1 | tee "$RUN/check-ui.log"; echo "check-ui exit=${PIPESTATUS[0]}"

# M1, M2, M3 and the owners the changed builder/transfer feed.
CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-engine \
  --test local_import --test task_identity --test skill_library_e2e \
  --test context_transfer --test session_history --test drafts --test backup_restore \
  2>&1 | tee "$RUN/engine-tests.log"; echo "engine-tests exit=${PIPESTATUS[0]}"
CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-engine --lib activity_history 2>&1 | tee "$RUN/engine-lib.log"; echo "engine-lib exit=${PIPESTATUS[0]}"

# J1-J3 and the palette/handoff neighbours sharing the fixture.
CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-ui --lib -- \
  shell::exit_regressions::past_work shell::exit_regressions::command_palette \
  shell::exit_regressions::context_handoff shell::exit_regressions::cancelled_or_profile_replaced \
  --test-threads=1 2>&1 | tee "$RUN/ui-tests.log"; echo "ui-tests exit=${PIPESTATUS[0]}"
# F1 consumed: IC-5's owner tests (unchanged code, rerun on this tree).
CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-ui --lib find -- --test-threads=1 2>&1 | tee "$RUN/ic5-find.log"; echo "ic5 exit=${PIPESTATUS[0]}"

scripts/cargo.sh clippy --locked -p luna-engine -p luna-ui --tests 2>&1 | tail -n 60 > "$RUN/clippy.log"; echo "clippy exit=${PIPESTATUS[0]}"
```

Expected: every exit 0. Baseline-failure proof (optional, on a scratch copy
of `f2e2c848` plus only the new tests): M1 fails at the `source_context`
assertion and M2 at "the imported task never became searchable".

## Native QA (orchestrator)

Use a disposable QA identity with all four overrides
([QA guide](../../../reference/qa-infrastructure.md)); quit the primary app
gracefully first. Sanitized prompts only; no production data.

```sh
export LUNA_DEV_APP_NAME='LUNA QA Past Work' LUNA_DEV_BUNDLE_ID=com.novnski.luna.qa.pastwork \
  LUNA_DEV_IPC_PORT=59851 LUNA_DEV_DATA_DIR="$PWD/target/luna-qa-data/sw11-past-work"
CARGO_INCREMENTAL=0 scripts/run-macos-dev.sh --refresh
FOLDER="$RUN/plain-folder"; mkdir -p "$FOLDER"   # not a Git repository
```

IPC calls reuse SW-03's one-shot helper pattern
(`target/qa/luna-only-workflows/sw03-20261013/qa_rpc.py`, with the port set
to 59851): stdin `{"method": …, "params": …}`, output the `ok` value.

- **N1 seed.** Add `$FOLDER` as a project. In it run one Codex chat and one
  Claude chat (Full or Auto as used), each with a short opening reply and a
  long final reply (ask for a 150-line numbered checklist whose line 120
  contains a unique token such as `SW11-DEEP-TOKEN`). Archive the Codex chat.
- **N2 provider import.** Create a disposable Codex thread outside LUNA in
  `$FOLDER` (`codex exec --json --skip-git-repo-check -C "$FOLDER" "Reply OK"`;
  its `thread.started` event names the thread id). `PreviewProviderTaskImport`
  `{"providerId":"codex","providerTaskId":<id>,"ownership":"providerOwned","spaceId":<project>}`
  then `AdoptProviderTask {"previewId":…}`; save both replies. Then the same
  preview with `"providerId":"claude-code"` must fail with "unsupported".
- **N3 find the old task.** Cmd+K, type a phrase from the archived Codex
  chat's opening reply. Capture the row: `Recorded content · <project> ·
  Codex · <model> · <date> · Done · Archived`. Enter opens that chat.
- **N4 off-screen passage (IC-5).** Cmd+F in the opened chat, type
  `SW11-DEEP-TOKEN`: `1 of 1`, the passage is revealed although it starts far
  above the viewport. Escape restores the view and the composer focus.
- **N5 explicit fresh handoff.** Chat menu → *Start new task with context…*.
  The dialog body names provider · model · reasoning · **access level**
  (for example `Full access`). Create the task; it shows no branch or PR,
  is not archived, and lives in the same project/host. Send one short
  message; `GetTaskIdentity` for the new task shows a binding whose
  provider task id differs from the source's. The source stays archived.
- **N6 unavailable history.** Cmd+K, type `imported from codex`. Capture the
  row `History unavailable · <project> · Codex · <date>`. Enter opens the
  imported task with the notice "History from before this Codex import was
  not copied into LUNA…". `SearchSessionHistory {"query":"","scope":"global"}`
  holds exactly one row for that task.
- **N7 skills (E03 migration slice).** Reuse SW-03's driver: install a Codex
  global skill into the Claude project scope through the library, then
  `native_skills.py catalog` in the Claude chat lists it as *Guidance only*
  with the same revision the source reported; no hook, command or permission
  grant appears.
- **N8 scope.** No permission prompt or grant appears during N3–N6 beyond
  the chats' own modes; Settings → Projects shows the plain folder project
  with no repository requirement.

Retain under `$RUN`: the logs above, IPC replies (`preview-*.json`,
`adopt.json`, `claude-refusal.json`, `search-*.json`, `identity-*.json`),
PNGs for N3–N6, then `(cd "$RUN" && find . -type f ! -name sha256.txt -print0 | sort -z | xargs -0 shasum -a 256 > sha256.txt)`.
Afterwards quit the QA app gracefully, then `scripts/run-macos-dev.sh --refresh`
for the primary LUNA Dev and verify its process, IPC port and
`codesign --verify --deep --strict 'target/luna-macos-dev/LUNA Dev.app'`.

## Limitations and open questions

- The portable new task still inherits its source's `cwd` and `checkout_id`
  (the existing context-carrying contract). A source that ran in a since
  removed or merged worktree hands that path to the fresh task; starting
  fresh work at the project root instead is a product decision, not taken
  here.
- The imported task's transcript itself shows no banner; the explicit
  statement lives in global search (row and open notice). An in-conversation
  notice belongs to the transcript or document owner.
- Provider import has no desktop UI (IPC only) and only Codex is supported;
  Claude native history stays unavailable by explicit refusal. Older or
  cloud-only provider history is never filled in.
- Skill copies record no "copied from" provenance (no new persistence); the
  retained attribution is provider, scope and the unchanged revision.
- Recorded-content search indexes a bounded summary of each message
  (240 characters); deep passages are found inside the opened chat with
  Find, which is why the journey joins the two owners.
- Dates render in local time (`%b %-d, %Y`); the UI test pins 12:00 UTC so
  its literal holds in every common zone.
