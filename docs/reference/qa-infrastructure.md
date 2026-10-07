# Parallel audit infrastructure acceptance

Write and exercise these failure cases before changing the wrappers. The
repeatable subprocess harness is `scripts/verify-qa-infrastructure.sh`; it
retains logs under `target/qa/qa-infrastructure.*` and removes its own fixtures.
It does not compile Luna or operate the main app.

1. Reject partial identities, unsafe names/identifiers, the main IPC port,
   nonnumeric ports/revisions, shared or escaping data paths, and identity
   collisions before building or requesting any quit.
2. Preserve the existing bundle and running process after a failed build,
   failed signing, refused graceful quit, occupied port, or failed launch.
   Quit by the exact process previously matched to the selected bundle.
3. Serialize two build subprocesses across linked Git worktrees; reject a
   start without its growth reservation, reject limits above the operating
   ceiling, stop a growing child before the configured threshold, release
   the lock after failure/signals, and preserve child exit codes.
4. Count the primary target, linked targets and shared mbx store once per
   canonical path. Never delete artifacts, replace target directories, or
   modify global Cargo configuration.
5. Real macOS acceptance: after the current Luna app saves and quits, build
   and refresh `LUNA QA Infrastructure`, bundle ID
   `com.novnski.luna.qa.infrastructure`, port `59785`, worktree-local data.
   Record build duration, exact PID/executable, plist identity, signature,
   IPC ownership, storage before/after and unchanged primary bundle hash.
6. Refuse a second running LUNA app before building or replacing a bundle,
   preserving the current app and its data. Serialize app operations across
   linked worktrees; a busy app slot must leave the existing bundle intact.

The harness uses disposable Git worktrees and command-boundary fault
injection for destructive/failure paths. It is an integration check of the
actual scripts, not a replacement for the final real signed-app acceptance.
Run only one Luna GUI app at a time. The primary app can remain running during
source review and ordinary Cargo checks; let it save and quit gracefully before
starting a QA app. Never force-quit it to free the slot. Keep the real QA app
available until the coordinator reviews its native evidence. After acceptance,
preserve and hash the evidence, verify no task or process still needs the exact
bundle, quit it gracefully, unregister it from LaunchServices and remove only
that disposable QA bundle. Retain its data and the Cargo cache unless a
separate scoped cleanup has been reviewed. Reopen the primary `LUNA Dev` app
for the user's handoff after the QA app stops. No unit tests are added after
implementation.

## Worker setup

After the coordinator merges this infrastructure, incorporate that commit
into each audit branch before any build. Run all build/check/test/clippy
commands with `scripts/cargo.sh` and `--locked`. Exit 75 requires coordination
with the current lock owner or storage cleanup, not a cache-mode retry.

| Checkout | App name | Bundle ID suffix | Port |
| --- | --- | --- | --- |
| e577 | LUNA QA Providers | providers | 59781 |
| c906 | LUNA QA Tasks | tasks | 59782 |
| abab | LUNA QA Workspace | workspace | 59783 |
| 1e48 | LUNA QA History | history | 59784 |
| b1c8 | LUNA QA Infrastructure | infrastructure | 59785 |
| audit-integration | LUNA QA Integration | integration | 59786 |
| sidebar feature worktree | LUNA QA Sidebar | sidebar | 59787 |
| OpenCode parity QA worktree | LUNA QA OpenCode | opencode | 59840 |

Example, from the provider worktree root (substitute its row for each peer):

```bash
export LUNA_DEV_APP_NAME='LUNA QA Providers'
export LUNA_DEV_BUNDLE_ID='com.novnski.luna.qa.providers'
export LUNA_DEV_IPC_PORT=59781
export LUNA_DEV_DATA_DIR="$PWD/target/luna-qa-data/providers"
python3 scripts/build-budget.py --status
scripts/cargo.sh check --locked -p luna-harness
scripts/run-macos-dev.sh --refresh
codesign --verify --deep --strict "target/luna-macos-dev/$LUNA_DEV_APP_NAME.app"
plutil -p "target/luna-macos-dev/$LUNA_DEV_APP_NAME.app/Contents/Info.plist"
lsof -nP -iTCP:"$LUNA_DEV_IPC_PORT" -sTCP:LISTEN
ps -p <listener-pid> -o pid=,command=
```

The four QA identity overrides above remain required. A QA bundle also sets
the runtime `LUNA_WORKTREES_DIR` to `$LUNA_DEV_DATA_DIR/worktrees` in its signed
plist and LaunchServices invocation, so a synthetic New worktree does not land
under the user's `~/.luna/worktrees`. An optional `LUNA_DEV_WORKTREES_DIR` can
choose another non-symlink directory **inside that same QA data directory**;
the wrapper rejects an empty, external, shared, or non-owned path before build.
The same signed plist sets `LUNA_QA_MCP_ISOLATION=1`: provider processes in this
QA app omit saved user MCP servers while retaining LUNA's per-session `luna`
tools. The primary LUNA Dev bundle sets it to `0`, so even an inherited shell
value cannot isolate ordinary user chats. Verify the signed plist and a QA
provider launch before relying on this isolation; the QA data directory alone
does not isolate `CODEX_HOME`, `CLAUDE_CONFIG_DIR`, or Executor.
The primary LUNA Dev bundle does not set this QA-only worktree override, and
production Repos keeps its user-facing default. A QA setup failure can retain
its Git checkout for inspection; use the recorded checkout path and Git
registration for recovery, never a broad filesystem cleanup.

Confirm the listener PID's executable is the exact chosen worktree bundle.
Record the refresh duration from its output and retain `last-build.log`.
The primary app retains `LUNA Dev`, `com.novnski.luna.dev`, port 59777 and
its existing data. A QA bundle uses the selected name throughout signing,
replacement and rollback; graceful quit targets its exact executable/PID.
QA bundles omit the release/dev URL schemes to avoid deep-link registration
collisions. A stopped packaged peer still reserves its identity, port and
data path. Assign distinct settings before any parallel packaging begins;
the linked-bundle collision scan is not a global atomic identity registry.

The Git common directory's `luna-app-operation.lock` serializes launch and
refresh operations across linked worktrees; `target/luna-macos-dev/.refresh-lock`
also prevents competing operations in one checkout. The script checks for a
different running Luna app before building and again before replacing a bundle.
Exit 75 means the app slot is busy. After an uncatchable kill, inspect the
recorded PID and remove only a proven stale lock directory when no app
operation remains. Never force-quit an app or remove data to release a lock.
The default primary app invocation and bundle path remain unchanged.

## OpenCode QA (OC-01)

This section is the OpenCode v2 verification contract from the OpenCode
parity plan (package OC-01). It covers the dedicated QA identity, storage
isolation, the real-model policy, the evidence artifact and the tools under
`scripts/qa-opencode-*`. OpenCode v1 is not supported (PD-15); every tool
refuses a binary whose `--version` major is not 2.

### Identity

| Setting | Value |
| --- | --- |
| `LUNA_DEV_APP_NAME` | `LUNA QA OpenCode` |
| `LUNA_DEV_BUNDLE_ID` | `com.novnski.luna.qa.opencode` |
| `LUNA_DEV_IPC_PORT` | `59840` |
| `LUNA_DEV_DATA_DIR` | `$PWD/target/luna-qa-data/opencode` (inside the QA worktree) |
| `LUNA_DEV_OPENCODE_EXECUTABLE` | `$LUNA_DEV_DATA_DIR/opencode-profile/bin/opencode` (profile wrapper) |
| `LUNA_DEV_OPENCODE_WIRE_LOG` | `$LUNA_DEV_DATA_DIR/opencode-wire` |

All four `LUNA_DEV_*` identity overrides stay mandatory, exactly as for every
other QA bundle. The two OpenCode overrides are QA-only: `run-macos-dev.sh`
rejects them for the primary `LUNA Dev` bundle, requires both paths to live
inside the QA data directory without symlinks, and stamps them into the signed
plist (and the LaunchServices invocation) as `OPENCODE_EXECUTABLE` and
`LUNA_OPENCODE_WIRE_LOG`. The primary bundle never carries either value.

```bash
export LUNA_DEV_APP_NAME='LUNA QA OpenCode'
export LUNA_DEV_BUNDLE_ID='com.novnski.luna.qa.opencode'
export LUNA_DEV_IPC_PORT=59840
export LUNA_DEV_DATA_DIR="$PWD/target/luna-qa-data/opencode"
scripts/qa-opencode-env.sh "$LUNA_DEV_DATA_DIR/opencode-profile" >/dev/null
export LUNA_DEV_OPENCODE_EXECUTABLE="$LUNA_DEV_DATA_DIR/opencode-profile/bin/opencode"
export LUNA_DEV_OPENCODE_WIRE_LOG="$LUNA_DEV_DATA_DIR/opencode-wire"
# Then the usual sequence: build-budget status, let the current app quit
# gracefully, scripts/run-macos-dev.sh --refresh, codesign/plutil/lsof/ps.
```

For a T2 app run, create the profile with `--offline`, start the mock provider
(it prints its URL; keep it running for the whole run), point the profile at
it, and pass the same URL to the driver:

```bash
scripts/qa-opencode-env.sh --offline "$LUNA_DEV_DATA_DIR/opencode-profile" >/dev/null
scripts/qa-opencode-mock-provider.py serve --port 0 --log "$LUNA_DEV_DATA_DIR/mock-requests.jsonl"
scripts/qa-opencode-mock-provider.py write-config \
  --profile "$LUNA_DEV_DATA_DIR/opencode-profile" --base-url http://127.0.0.1:<port>
node scripts/qa-opencode-live.mjs run --tier T2 --mock-url http://127.0.0.1:<port> \
  --mock-log "$LUNA_DEV_DATA_DIR/mock-requests.jsonl"
```

A T3 run uses a profile without `--offline` and needs no mock. Re-running
`qa-opencode-env.sh` on an existing profile only regenerates its wrapper, which
the QA app picks up at the next OpenCode launch without a refresh. The mock
entry in `opencode.json` stays until removed, so a T3 run names its model
explicitly or lets the driver pick a free model.

### Failure cases (written before the tooling)

Each case names its validation-plan id where one exists and the outcome the
tooling must produce.

1. **QF-01 (OCF-140) user storage reached.** A run inherits the user's
   `~/.config/opencode`, `~/.local/share/opencode` (database, and the legacy
   `auth.json` that the v2 credential-import migration reads), cache or state.
   *Expected:* the profile wrapper exports all four `XDG_*` roots,
   `OPENCODE_CONFIG_DIR` and `OPENCODE_DB` inside the profile before `exec`.
   `qa-opencode-env.sh` refuses a profile root that is `$HOME`, `/`, or inside
   any real OpenCode directory (`~/.config/opencode`, `~/.local/share/opencode`,
   `~/.local/share/opencode-v1`, `~/.cache/opencode`, `~/.local/state/opencode`,
   `~/.opencode`).
2. **QF-02 (OCF-140) home-directory discovery.** OpenCode 2 loads skills from
   `~/.claude/skills` and `~/.agents/skills`. It also walks up from the session
   directory for `opencode.json`, `.opencode`, `.claude`, `.agents` and
   `AGENTS.md`, stopping at `$HOME` when the directory is inside it. A fixture
   under `~/workspace` therefore loads the user's workspace skills and
   instructions. *Expected:* the wrapper sets `OPENCODE_TEST_HOME` to the
   profile's empty `home/` (opt out with `--real-home`), and every OpenCode
   fixture repository lives under the system temporary directory, outside
   `$HOME`. The driver and the T2 rig refuse a fixture path inside `$HOME`.
3. **QF-03 (OCF-03, OCF-04) a wrapper chain hides the binary.** A wrapper such
   as `opencode2` (`exec "$(dirname "$0")/opencode"`), or `opencode1`, which
   re-exports `XDG_*` to `~/.local/share/opencode-v1` and runs Homebrew 1.x.
   *Expected:* the manifest records every hop with its SHA-256 and hashes the
   final native executable. `qa-opencode-env.sh` refuses a chain that exports
   or changes environment (it would undo isolation) and pins the absolute final
   binary in the profile wrapper, so discovery and runs use the same binary.
4. **QF-04 (OCF-142) secrets in evidence.** HTTP Basic headers, the server
   password, provider API keys or bearer tokens in any artifact. OpenCode
   2.0.22 returns every provider's `settings`, **including `apiKey`**, in
   `GET /api/model` and `GET /api/model/default` (observed in the T2 rig), so a
   raw capture of those responses leaks a configured key. *Expected:* captures
   store header **names** only; string values of secret-named fields (`apiKey`,
   `authToken`, `password`, `token`, ...) are replaced with `<redacted>` except
   the known placeholders `public` and the mock's fixed key; the mock provider
   logs only whether an `Authorization` header was present;
   `qa-opencode-manifest.py scan` fails the run (no `pass.json`) when a known
   secret value, a credential pattern or an unredacted secret-named field
   appears. The adapter wire log (`LUNA_OPENCODE_WIRE_LOG`) needs the same field
   redaction.
5. **QF-05 (OCF-143) tier mislabelled.** *Expected:* every manifest carries an
   explicit `tier` (`T0`..`T4`). A `T2` manifest requires the effective model
   readback to be the mock provider. A `T3` manifest requires a non-mock
   effective model. A mismatch fails the run.
6. **QF-06 (OCF-141) quota or credentials used unexpectedly.** *Expected:* T2
   profiles set `OPENCODE_DISABLE_MODELS_FETCH=1`, make the mock the default
   model and contain no credentials. T3 runs default to OpenCode's free models
   and record the effective model from server readback.
7. **QF-07 mock provider ambiguity.** An unknown scenario name, a malformed
   script file, or a scripted tool the request does not offer. *Expected:* the
   mock refuses to start on a bad scenario or script. When the request offers no
   matching tool it answers with plain text and logs the request as
   `unscripted-tool`; it never invents a tool the model was not given.
8. **QF-08 server boot or shutdown failure.** No `{"url"}` line from
   `opencode serve --stdio --port 0` within the boot budget, `/api/info` stuck
   at 503, or a server that ignores stdin EOF. *Expected:* the T2 rig records
   the stderr tail and fails with a named step. It closes stdin and waits up to
   `--grace` seconds (default 10), then sends SIGTERM and, 3 s later, SIGKILL
   (OpenCode's own standalone launcher uses the same 3 s escalation). It
   records which step ended the process, and a run that needed a signal fails
   `clean-shutdown-on-stdin-eof`. No server outlives the rig.
9. **QF-09 wrong app.** A bundle, plist identity, signature, IPC listener or
   `OPENCODE_EXECUTABLE` that does not match the OpenCode QA identity.
   *Expected:* the live driver refuses before any RPC write.
10. **QF-10 replayed send.** A timed-out or failed send is replayed by the
    driver. *Expected:* the manifest records each attempt before the RPC. An
    attempt without a recorded outcome stays `unknown` and is never resent.
    `run` always starts a new run directory; `snapshot` and `verify-restart`
    never send.
11. **QF-11 wire capture missing.** `LUNA_OPENCODE_WIRE_LOG` is unset in the
    plist, or its directory is absent or empty after a turn. *Expected:* the
    manifest records `wireLog.present=false` (or zero captured bytes) and the
    run cannot pass. The driver never fabricates `native-frames.jsonl`.

### Storage isolation

`scripts/qa-opencode-env.sh <profile-dir>` creates a profile and prints shell
exports. Every OpenCode process in a QA run goes through the profile's
`bin/opencode` wrapper, whether the T2 rig, a harness example or the QA app
starts it. The wrapper sets:

| Variable | Value | Why |
| --- | --- | --- |
| `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_CACHE_HOME`, `XDG_STATE_HOME` | `<profile>/{config,data,cache,state}` | Config, database, credentials, logs, npm packages and locks |
| `OPENCODE_CONFIG_DIR` | `<profile>/config/opencode` | Overrides an inherited value |
| `OPENCODE_DB` | `<profile>/data/opencode/opencode.db` | Overrides an inherited value |
| `OPENCODE_TEST_HOME` | `<profile>/home` (omitted with `--real-home`) | Stops `~/.claude` and `~/.agents` skill discovery |
| `TMPDIR` | `<profile>/tmp` | Keeps `os.tmpdir()/opencode` per profile |
| `OPENCODE_DISABLE_AUTOUPDATE` | `1` | No binary self-update |
| `OPENCODE_DISABLE_MODELS_FETCH` | `1` with `--offline` (T2) | Bundled models.dev snapshot, no network |
| `OPENCODE_DISABLE_PROJECT_CONFIG` | `1` with `--no-project-config` | Scenarios that must ignore fixture project config |

`OPENCODE_CONFIG_DIR` plus `OPENCODE_DB` alone is **not** isolation. The data
root (logs, the `auth.json` import), cache (provider npm installs) and state
(locks) would still be the user's, so always use the four `XDG_*` roots. The
wrapper does not unset `OPENCODE_CONFIG_CONTENT`, because LUNA may pass inline
config that way. OpenCode can install npm packages on first boot (provider SDKs,
formatters) and has no flag to disable that, so the profile cache is
disposable. The mock provider uses `@ai-sdk/openai-compatible`, which v2.0.22
maps to a bundled package, so a T2 boot installs nothing.

`LUNA_QA_MCP_ISOLATION` needs no OpenCode branch. OpenCode reads MCP servers
only from its own config files, and the isolated profile's config contains
none. LUNA's per-session `luna` server is attached by the adapter and is
unaffected. A scenario that relies on fixture project config says so in
`result.md`.

**LUNA-process readers.** The OpenCode Go limits reader (PD-17,
`GetUsageSnapshot` with `includeLimits`), OpenCode usage history, the skills
catalog's OpenCode config root and the native extensions view run inside LUNA,
not in the OpenCode child. They resolve their directories through
`luna_harness::opencode::OpencodeStorage`, which replays the static `export`
and `unset` lines of the `OPENCODE_EXECUTABLE` wrapper chain over the app
environment, exactly as the spawned server sees them. In the QA app they
therefore read the profile's `data/opencode` and `config/opencode`, never the
user's. A wrapper that changes storage in a way LUNA cannot evaluate (command
substitution, `source`, a conditional export) makes those readers report the
OpenCode store as unavailable instead of falling back to `$HOME`. Explicit
`XDG_*`, `OPENCODE_CONFIG_DIR`, `OPENCODE_DB` and `OPENCODE_DATA_DIR` in the
app environment are honored the same way. The profile has no OpenCode Go
account, so the QA Usage page shows OpenCode history but no Go limits.

### Real-model policy (PD-18)

- Real-AI (T3) runs are authorized for every OpenCode scenario. OpenCode runs
  default to OpenCode's free models inside the isolated QA profile, which need
  no login. Discover them from `GET /api/model` in that profile instead of
  hard-coding ids.
- A paid OpenCode model is used only after the user signs in to that isolated
  QA profile themselves: `eval "$(scripts/qa-opencode-env.sh <profile>)"`, then
  `opencode auth login` in their own terminal. Agents never copy, read or move
  keys, `auth.json` or the user's OpenCode database into a profile.
- Claude comparison runs (PD-14 parity diffs) use the user's normal Claude Code
  login through the same LUNA QA app and driver (`--provider claude-code`).
- T2 runs use the mock provider below and must not reach any real model.
- Report model usage (provider, model, number of turns) after each batch.

### T2 rig: mock OpenAI-compatible provider

`scripts/qa-opencode-mock-provider.py` uses only the Python standard library
and has two subcommands:

- `serve --port 0 --log <requests.jsonl> [--scenario NAME] [--script FILE]`
  prints one `{"url": ...}` line, then serves `GET /v1/models` and streaming or
  non-streaming `POST /v1/chat/completions`. Scenarios: `text`, `reasoning`,
  `tool-shell`, `subagent`, `image`, `error-401`, `error-429`, `error-500`,
  `context-overflow` and `drop` (closes the stream mid-reply). A request picks
  its scenario from a `[[mock:<name>]]` marker in the latest user message, then
  from the next queued script entry, then from `--scenario`. After a tool result
  it replies with text quoting the tool output, so tool rounds terminate. The
  `image` scenario replies with the SHA-256 and media type of each image it
  received, which proves which bytes reached the model. `POST /__mock/script`
  queues entries at runtime, and `GET /__mock/state` reports counters.
  Requests without tools (title and summary agents) get a fixed text reply and
  never consume the script. Compaction requests do carry tools, so prefer
  markers over the script queue in scenarios that may compact.
  `error-429-once`, `error-500-once` and `slow` support retry, steer and stop
  scenarios.
- `write-config --profile <dir> --base-url <url> [--exclusive]` writes
  `<profile>/config/opencode/opencode.json` in the v2 schema (`providers`,
  `package: aisdk:@ai-sdk/openai-compatible`, `settings.baseURL`) with a
  `luna-mock` provider as the default model (`mock-model`, variants `low` and
  `high` that add `reasoning_effort` to the request body, text and image input,
  zero cost), plus `share: disabled` and `update: disable`. It refuses a
  directory that is not a QA profile. `--exclusive` adds `provider.use` rules
  that deny every other provider. `GET /api/model` still lists the other
  providers with these rules; whether they refuse a turn is unverified.

`scripts/qa-opencode-t2.py` composes these into one repeatable run: profile,
mock, config, `opencode serve --stdio --port 0`, `/api/info`, `/api/model`,
optional session turns (`--turn <scenario>`), clean shutdown by closing stdin,
then the manifest, secret scan and `SHA256SUMS`. It is a harness-level rig and
does not need or touch a LUNA app. Its turns drive the raw v2 API
(`POST /api/session`, `POST /api/session/{id}/prompt`, `GET /api/event`) and
answer any `permission.asked` with `{"decision": "once"}`. The `image` turn
attaches a generated PNG through `files[]` and checks that the mock received
those exact bytes.

```bash
scripts/qa-opencode-t2.py --evidence-root <dir> --scenario t2-turns \
  --turn text --turn reasoning --turn tool-shell --turn subagent \
  --turn image --turn error-401 --turn context-overflow
```

Rig evidence is T2 for the OpenCode server and the mock only. It proves
nothing about LUNA's adapter, engine, document or UI, and nothing about real
model output.

### Live app driver

`scripts/qa-opencode-live.mjs` follows `scripts/qa-task-work-live.mjs`:

```bash
node scripts/qa-opencode-live.mjs run --tier T2|T3 [--provider opencode] \
  [--model <provider/model>] [--variant low] [--scenario oce-01] \
  [--prompt <text>]... [--prompt-file prompt.txt]... [--permission-mode full] \
  [--expect-status idle|errored] [--mock-url http://127.0.0.1:<port>] \
  [--mock-log <requests.jsonl>]
node scripts/qa-opencode-live.mjs snapshot [--run <dir>]
node scripts/qa-opencode-live.mjs verify-restart [--run <dir>]
```

It asserts the QA identity before any write: bundle path, codesign, plist id,
port and data dir, `LUNA_QA_MCP_ISOLATION=1`, `OPENCODE_EXECUTABLE` being a
profile wrapper inside the QA data dir, and the listener PID's executable. The
`LUNA_DEV_*` variables can point it at another QA identity, never at the
primary app. `--tier` is mandatory: T2 must use a `luna-mock/` model and,
with `--mock-url`, prove the mock served the run; T3 must use a real model.
Without `--model` it picks `luna-mock/mock-model` for T2 and an OpenCode free
model from `ListModels` for T3. `--variant` sets LUNA's `reasoning`, the shared
variant mechanism.

It writes `manifest.json` before each send, records the wire-log directory and
its byte offsets, and keeps persistent `WatchDocMessages`, `WatchQueue` and
`WatchSessions` subscriptions for `observer.json`; turn completion (`working`
then `idle`/`errored`, or an advanced `lastCompletedTurn`) is read from those
frames, not by polling. Error scenarios pass `--expect-status errored`. At the end it copies the run's slice of the wire
log into `wire/` (and to `native-frames.jsonl` and `http.jsonl` when the
adapter writes files with those names), then checks that each user message
appears exactly once. Evidence goes to `target/qa/opencode/<scenario>/run-*`
with a `latest` pointer, or under `--evidence-root`. `verify-restart` requires
a new app PID and checks that each recorded send survives exactly once with no
replayed queue row. `--provider claude-code` runs the same steps for PD-14
comparison diffs. Approval- and question-driven scenarios still need an
answering step added to the driver.

### Evidence artifact

Every run directory follows validation-plan §2: `manifest.json`,
`commands.jsonl`, `native-frames.jsonl` plus `http.jsonl` with a redaction
manifest, `native-readback/`, LUNA state, `observer.json`, `result.md`,
`SHA256SUMS`, and `pass.json` only when every criterion passed.
`scripts/qa-opencode-manifest.py` provides:

- `binary [--path P]`: the binary resolution record. It holds the resolved
  path, the wrapper chain with per-hop SHA-256, the final native executable,
  `--version` (run inside a throwaway isolated profile), the resolver source,
  PATH and the launch context.
- `write --dir D --set key=json ...`: merges fields into `manifest.json`.
- `scan --dir D [--secret-env NAME ...]`: the OCF-142 secret scan.
- `sums --dir D`: writes `SHA256SUMS` for every file except itself.

Name the tier of every claim; a lower tier never substitutes for a higher one.
T0 is a fake transport, T1 replays recorded frames, T2 is the real `opencode`
binary plus the mock provider, T3 is the real binary plus a real model, and T4
is the signed QA app's native UI on top of T2 or T3.
