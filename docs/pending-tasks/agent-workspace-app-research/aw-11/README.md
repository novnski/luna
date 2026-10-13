# AW-11: portable project configuration with personal overrides

Baseline: `6a0ad25c` (13 October 2026). Live status belongs to
`pending_tasks.py`. This file is the contract, the failure cases (written
before the code), the criteria mapping, the exact commands and the native QA
steps. Sources: [opportunities](../luna-opportunities.md) row 11 and
"Before implementing the larger ideas", [settings analysis](../settings-analysis.md)
("Shared repository configuration plus personal override", "Scope should
follow the key"), [product findings](../product-findings.md) (Conductor
"Setup, run, and feedback are connected").

## Rebased against current main

| Research row | Current main | Decision |
| --- | --- | --- |
| Opportunity 11: project configuration with personal overrides ("schema, precedence, trust, migration, changed-source handling") | Gap. Commands were device-local only (`project_command_configs`), and `project_commands.rs` said "No repository hook discovery". | **Implemented here** for commands: schema v1, precedence, device-local trust, changed-content handling. Migration of existing personal commands into a file is an optional follow-up. |
| "File-backed project settings: define ownership, precedence, schema validation, local-vs-shared intent, and changed-command review. Do not execute a newly discovered script simply because a file exists." | Gap | **Implemented here.** Reading never runs anything; acceptance is a separate, content-pinned decision. |
| Settings: shared repository configuration plus personal override (P2) | Partial: device-local project actions | **Implemented here** for commands. |
| Settings: "Repository configuration ... precedence needs a written specification and conflict tests before implementation" | Missing | This document plus `crates/engine/tests/project_config.rs`. |
| Settings: saved named project run commands (Existing) | Shipped: `ProjectCommands`, receipts, stop/retry, preview URL, Terminal → Commands | **Reused, not rebuilt.** The repository layer only adds runnable entries to the same owner. |
| Settings: setup on new checkout with wait policy (Existing) | Shipped for personal commands | Unchanged. Repository commands can never be setup commands (the v1 schema has no such key, so a file that declares one is refused). |
| Settings: port allocation and named service discovery | Partial (preview URL) | Out of scope; AW-04 owns actual service URLs. `preview-url` is carried as data and validated as HTTP(S). |
| Settings: effective-value provenance, row reset, search (P1) | Partial | Out of scope; AW-01 owns the settings registry. Command provenance here is shown at the existing project-commands owner only. |
| Settings: share/export only portable preferences (P2), acceptance scenario 7 | Partial: backups | Out of scope. |
| Acceptance scenario 3 (project defaults inherit/override) | Shipped: `Space.executionDefaults` + `ProjectExecutionDefaults::resolve` | Unchanged. The repository file **cannot** set provider, model, checkout or permission mode: a checked-in file must never widen execution permission. |
| Product finding: "Personal local settings are distinct from reviewed team settings" | Gap | Implemented: repository commands are shown as "From `.luna/project.toml`", personal ones as "Saved on this device", and the personal one wins by ID. |

Passing behaviour that needed no rebuild: personal command CRUD and CAS
saves, receipts and attempt fencing, stop/retry, setup gating, restart
cancellation, backup inclusion, Terminal → Commands scope fencing.

## Contract

### Schema (version 1)

The file is `.luna/project.toml` at the root of the folder LUNA treats as the
checkout: the task's `cwd` in Terminal → Commands, the project folder in
Settings → Projects. Each checkout reads its own copy, so a worktree on
another branch sees that branch's file.

```toml
version = 1

[[commands]]
id = "test"                         # 1–64 of [A-Za-z0-9._-], starts alphanumeric
name = "Test"                       # 1–120 bytes, no control characters
command = "cargo test --locked"     # 1–16384 bytes; LF and tab are the only control characters

[[commands]]
id = "web"
name = "Start web"
command = """
cd web
npm run dev
"""
preview-url = "http://localhost:5173"   # optional; full HTTP or HTTPS URL
```

Keys anywhere else, including `setup-on-worktree-create`, `wait-for-setup`,
`env`, provider/model/permission keys and any future key, refuse the whole
file. `version` other than the integer `1` refuses the whole file and says so.
At most 24 commands; the file is at most 64 KiB of UTF-8. Bidirectional
formatting characters (U+061C, U+200E/F, U+202A–U+202E, U+2066–U+2069) are
refused in names, commands and URLs, because they make the reviewed text
differ from the bytes that run.

The canonical action of a repository command is a `ProjectAction` with
`setupOnWorktreeCreate: false` and `waitForSetup: true`. Its **digest** is the
existing receipt digest, `sha256(serde_json(ProjectAction))`, so a receipt's
`actionDigest` equals the accepted digest.

### Precedence

1. This device's personal commands (Settings → Projects), in saved order.
2. Then repository commands, in file order, that are **accepted** and that no
   personal command overrides by ID.

`ProjectCommandsSnapshot.actions` is exactly that runnable list. Unaccepted
or changed repository commands appear only in `snapshot.repository.commands`
with their trust state; they never get a Run control and Run refuses them.
Removing a personal override returns the repository command in its own trust
state (unaccepted unless this device accepted that exact content).

### Ownership and trust

| Data | Owner | Scope | Trusted? |
| --- | --- | --- | --- |
| `.luna/project.toml` | The repository (versioned with the branch) | Per checkout | **No.** Untrusted input, read without executing anything |
| Personal commands | `project_command_configs` (docs.sqlite3) | Project × this device | Yes, authored here |
| Accepted digests | `project_command_trust` (docs.sqlite3, migration v21) | Project × this device × command ID | The user's decision |
| Receipts | `project_command_runs`, now with `source: personal \| repository` | This device | Capture exactly what ran |

- Accepting is `SetProjectCommandTrust {projectId, chatId?, actionId, digest, trusted: true}`.
  The engine re-reads the file (the task's checkout when `chatId` is given,
  else the project folder) and refuses unless that command's current digest
  equals `digest`, the content the user reviewed. Accepting never runs.
- One digest per command ID: accepting new content replaces the old
  acceptance; `trusted: false` revokes it. Any change to the name, command or
  preview URL is a new digest, so the command becomes `changed` and
  unrunnable until accepted again.
- Run resolves trust from the current file both before awaiting setup and
  again under the command gate, and runs the content it read (never a later
  re-read). Retry of a repository receipt requires the checkout's current,
  non-overridden command to be accepted with the receipt's digest.
- Trust is device-local: it never syncs, it is included in backups like the
  other command rows, it is removed with the project
  (`retire_project_local_records`, boot-time prune), and the ledger keeps
  the 128 most recently accepted IDs per project and device.
- The file is read only from inside the checkout. Symbolic links are
  resolved and must stay inside it; the final open uses
  `O_NOFOLLOW | O_NONBLOCK` and requires a regular file, so a FIFO cannot
  block the engine.

### RPC (CORE-01 catalog rows, both `local`/`localOnly`)

| Method | Id | Kind | Params → reply |
| --- | --- | --- | --- |
| `GetProjectConfiguration` | `luna.core/get-project-configuration` | query | `{projectId}` → `ProjectRepositoryConfig` (project folder) |
| `SetProjectCommandTrust` | `luna.core/set-project-command-trust` | command | `{projectId, chatId?, actionId, digest, trusted}` → `ProjectRepositoryConfig` |
| `ProjectCommandStatus` (existing) | unchanged id | query | now also carries `repository` for the task's checkout |

`ProjectRepositoryConfig {checkout, file, state: absent|loaded|refused|unavailable, detail?, commands: [{action, digest, trust: accepted|unaccepted|changed, overridden}]}`.

## Failure contract (written before the code)

| # | Failure | Required behaviour | Owner test |
| --- | --- | --- | --- |
| F1 | No file, no `.luna`, or `.luna` is a file | `absent`; personal commands unaffected | refusals: `absent`, `luna-is-a-file` |
| F2 | Unknown schema version | `refused`: "Unsupported schema version N. This LUNA reads version 1 only." | refusals: `unknown-version` |
| F3 | Missing or non-integer `version` | `refused` naming `version = 1` | `missing-version`, `string-version` |
| F4 | Invalid TOML | `refused` with line and parser message | `malformed` |
| F5 | Unknown key (top level or command, incl. setup keys) | `refused` naming the key | `setup-key-is-unknown`, `unknown-top-level-key` |
| F6 | Invalid command values (duplicate or path-like ID, control or bidi characters, non-HTTP preview) | `refused` naming the command | `duplicate-id`, `path-like-id`, `hidden-direction`, `escape-sequence`, `file-preview` |
| F7 | File or `.luna` links outside the checkout | `refused`: "resolves outside the checkout"; outside file untouched | `link-to-file-outside`, `link-luna-dir-outside` |
| F8 | FIFO / non-regular file | `refused` without blocking | `fifo` |
| F9 | Oversized or non-UTF-8 | `refused` | `oversized`, `not-utf8` |
| F10 | Any refused file | Snapshot succeeds, personal command runs, acceptance refused ("is not in use here") | refusal loop + final personal run |
| F11 | Link inside the checkout | Loaded (still unaccepted) | `link-inside` |
| T1 | Discovered command | Shown `unaccepted`, not in `actions`, Run refused, no receipt, no marker | precedence: discovery block |
| T2 | Personal command with the same ID | Personal content runs; repository entry `overridden` | precedence: `lint` |
| T3 | Accept with a stale digest | Refused | precedence: stale digest |
| T4 | Accept | `accepted`, runnable, **nothing runs** | precedence: after-accept block |
| T5 | Run accepted command | Runs exactly the accepted content; receipt `source: repository`, `actionDigest` = accepted digest | precedence: `build` receipt |
| T6 | Another checkout changes the command | That checkout shows `changed`; the original stays `accepted` | precedence: worktree scope |
| T7 | Accepted command changes in place | `changed`; Run and Retry refused; nothing runs | precedence: pulled change |
| T8 | Revoke | `unaccepted` | precedence: revoke |
| T9 | Remove the override | Repository command returns `unaccepted` | precedence: override removed |
| T10 | Restart | Trust persists; nothing runs on boot | precedence: restart |
| S1 | Ledger replace/revoke/device isolation, backup snapshot, project retirement | One digest per ID; foreign device sees none; VACUUM copy carries rows; retirement clears them | `crates/sync/tests/project_command_store.rs` |
| U1 | Terminal → Commands | No Run for unaccepted; Accept only after Review discloses the exact content; one trust request with the painted digest; zero Run requests; Run and "Repository" badge after acceptance | `commands_panel_runs_repository_commands_only_after_exact_acceptance` |
| U2 | Settings → Projects | Trust badge per state; Accept/Revoke/Override per state; overridden rows have no trust controls; personal row shows "Overrides repository"; Accept pins the painted digest (no `chatId`); Override opens a prefilled, unsaved draft | `settings_card_shows_repository_trust_and_pins_the_displayed_digest` |

## Criteria mapping

- **rebase**: the table above; existing owners (`ProjectCommands`,
  `DocsStore` project-command rows, `ProjectCommandSettings`,
  `ProjectCommandPanel`, CORE-01 catalog) are extended, not duplicated.
- **scope**: the contract and failure table above; required work is the
  command layer only. Optional follow-ups are listed below.
- **verification**: the engine E2E retains scope (per-checkout file),
  provenance (`source`, `overridden`), native receipts (real PTY runs and
  marker files), draft (Override opens an unsaved draft, U2) and refusal
  evidence as JSON; the UI tests retain rendered-selector and RPC traces.
- **handoff**: commands and native QA steps below.

## Files

Owned: `crates/engine/src/project_commands.rs`,
`crates/engine/src/project_commands/project_config.rs` (new),
`crates/engine/tests/project_config.rs` (new),
`crates/proto/src/project_commands.rs`,
`crates/sync/src/store/project_commands.rs`,
`crates/sync/tests/project_command_store.rs`,
`crates/ui/src/project_commands.rs`,
`crates/ui/src/project_commands_repository_tests.rs` (new),
`crates/ui/src/project_commands_engine_epoch_tests.rs` (fixture fields + module hook).

Shared, additive: `crates/sync/src/store.rs` (migration v21, project
retirement), `crates/engine/src/backup.rs` (`CURRENT_SCHEMA` 21, table
inventory), `crates/engine/src/rpc.rs` (two params fields, two match arms),
`crates/rpc/src/lib.rs`, `crates/rpc/src/operations.rs`,
`crates/rpc/contract/v1/operations.json` (two rows), migration-replay
fixtures `crates/sync/tests/side_chat_store.rs`,
`crates/engine/tests/backup_restore.rs`, `crates/engine/tests/board_store_e2e.rs`
(drop the new table; the board test's pinned schema numbers 20 → 21).

## Commands

```sh
stamp=$(date +%Y%m%d-%H%M%S)
export LUNA_PROJECT_CONFIG_QA_ARTIFACTS="$PWD/target/qa/aw-11/$stamp/engine"
export LUNA_EPOCH_QA_ARTIFACTS="$PWD/target/qa/aw-11/$stamp/ui"
export CARGO_INCREMENTAL=0
scripts/cargo.sh test --locked -p luna-engine --test project_config -- --nocapture --test-threads=1
scripts/cargo.sh test --locked -p luna-sync --test project_command_store --test side_chat_store
scripts/cargo.sh test --locked -p luna-ui --lib project_commands -- --nocapture
# Regressions at the reused owners and the migration/catalog consumers:
scripts/cargo.sh test --locked -p luna-engine --test project_commands --test project_commands_context --test project_commands_worktree_setup --test backup_restore --test board_store_e2e
scripts/cargo.sh test --locked -p luna-rpc --features schema --test contract
scripts/cargo.sh test --locked -p luna-rpc --lib remote_auth
# If the contract test reports operations.json stale, regenerate it:
scripts/cargo.sh run --locked -p luna-rpc --features schema --example contract
```

Expected: every command exits 0. The engine run writes
`precedence-trust-scope.json` and `refusals.json`; the UI run writes
`commands-repository-trust.json` and `settings-repository-trust.json`.
Hash them with `shasum -a 256 target/qa/aw-11/$stamp/*/*.json`.

## Native QA (LUNA Dev)

Use a disposable repository; every command only writes a marker file.

1. `mkdir -p /tmp/aw11 && cd /tmp/aw11 && git init -q -b main && mkdir .luna`
   and write `.luna/project.toml`:
   `version = 1`, then `[[commands]]` with `id = "marker"`,
   `name = "Marker"`, `command = "date > ran-marker"`. Commit it. Add
   `/tmp/aw11` as a project in LUNA Dev and open a task in it.
2. Settings → Projects → the project: the **Repository commands** card shows
   Marker as **Not accepted** with its full command, plus Accept and
   Override. Terminal → Commands shows Marker with **Review** and no Run.
   `ls /tmp/aw11` has no `ran-marker`.
3. Terminal → Commands → Review → Accept. Run appears with a **Repository**
   badge; still no `ran-marker`. Run it: `ran-marker` appears and the receipt
   shows **Repository**.
4. Edit the file's command to `date > ran-changed` without committing.
   Within a second the panel shows **Changed since accepted** and no Run;
   Retry on the earlier receipt is refused with "since it ran". No
   `ran-changed` exists.
5. Settings → Override on Marker: the editor is titled "Override repository
   command on this device" and prefilled; Save. The personal row shows
   **Overrides repository**, the repository row **Overridden on this
   device**; Run in Terminal → Commands runs the personal content.
6. Write `version = 2`: Settings shows "LUNA ignored .luna/project.toml:
   Unsupported schema version 2…"; the personal command still runs.
7. Replace the file with a symbolic link to a file outside `/tmp/aw11`:
   refused with "resolves outside the checkout".
8. Quit and reopen LUNA Dev: the acceptance from step 3 is still listed
   (restore the step 1 file first); nothing ran on launch.

Read back device-local trust with
`sqlite3 "<LUNA Dev data dir>/profiles/<profile>/docs.sqlite3" 'SELECT project_id,action_id,digest FROM project_command_trust'`
and keep screenshots of steps 2–7.

## Limitations

- Acceptance pins the command text, not the scripts or files it invokes
  (`./scripts/dev.sh` can still change). The Settings footer says so.
- One accepted digest per ID: moving between branches with different
  versions of one command needs re-acceptance on each switch.
- Settings reads the project folder; a task in a worktree on another branch
  reads that worktree's file in Terminal → Commands.
- The panel re-reads the file on its one-second status poll while expanded;
  Settings re-reads on open, after a save and after a trust decision (no
  file watcher).
- CRLF line endings inside a multi-line command are refused (CR is a control
  character).
- A swap of an intermediate directory between the containment check and the
  open is not fully excluded; the final component cannot be a link.

## Follow-ups

Required for this ticket: none beyond the delivery above.

Optional, not started:
- Project execution defaults in the file (provider, model, checkout), never
  permission mode, layered under `Space.executionDefaults`.
- Hiding or disabling one repository command on this device.
- Moving existing personal commands into a checked-in file (migration).
- A file watcher for the Settings card.
- AW-04 service URL readback; AW-01 settings search and provenance for these
  rows.
