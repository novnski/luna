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
collisions. LUNA claims `luna://` at startup only when its own Info.plist
declares that scheme, so a QA bundle never rebinds the user's handler. A QA
build from before that gate may have left `luna://` pointing at a QA bundle id
(`lsregister -dump` shows it under `handlerpref id: luna`); the next `LUNA Dev`
start claims it back. A stopped packaged peer still reserves its identity, port and
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

**QF-25 shared background service.** OpenCode 2's CLI commands attach to a
shared background service unless they get `--standalone` or `--server`. The
service is found through `$XDG_STATE_HOME/opencode/service.json` and spawned
as a bare `opencode serve --service`, which runs outside the profile and
opens the user's real `~/.local/share/opencode` database (observed in the
first paid QA round with `auth`, `models`, `stats`, `session` and `run`).
*Expected:* the wrapper appends `--standalone` (a private server inside the
profile) to `auth`, `session`, `models`, `stats`, `run`, `api`, `reload`,
`mini` and the bare TUI invocation, unless the caller already passed
`--standalone` or `--server`. It refuses `service`, `pair`, `uninstall` and
`upgrade`/`update` with exit 64, because they manage the shared service or
the real installation. `serve` and `acp` (what LUNA and the T2 rig spawn) and
the `debug`, `mcp` and `plugin` commands, which have no `--standalone` flag,
pass through unchanged. No `export` or `exec` sits inside these blocks, so
LUNA's storage replay still evaluates the wrapper. `mcp list` was observed
(second paid round) to still attach to the shared service and leave a detached
`opencode serve --service` behind, so the script also merges
`"disabled": true` into the profile's service config,
`<profile>/config/opencode/service.json`. With it, OpenCode 2.0.22 gives every
CLI command a private server; any other key (the service password OpenCode
generates there) is kept and never printed. Regenerate an existing profile's
wrapper and service config in place with `scripts/qa-opencode-env.sh
<profile>`; its data, credentials and other config are kept. Check it with
`<profile>/bin/opencode auth list` (a signed-in profile lists its integration
as `stored`) and `<profile>/bin/opencode mcp list` while no `opencode serve
--service` process appears. Older profiles need `--standalone` passed by hand
and must not run `mcp` until regenerated.

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
app environment are honored the same way. A new profile has no OpenCode Go
credential, so the QA Usage page shows OpenCode history but no Go limits. Once
the user signs the profile in themselves (`opencode auth login` for OpenCode
Go through the profile wrapper), OpenCode 2 saves the key in the profile
database's `credential` table and the limits reader uses that key, as the
spawned server does.

**History against `opencode stats` (S6).** The two do not select the same
rows, so compare them with `scripts/qa-opencode-usage-reconcile.py --db
<profile>/data/opencode/opencode.db --luna <LUNA per-model totals> --stats
<opencode stats --models --cost --days N --json>`. The LUNA input is the
`qa-opencode-usage.mjs` extract or the `usage_opencode_readback` example
output. OpenCode 2.0.22's stats counts only `assistant` rows per model and
adds compaction usage only from persisted `session.usage.recorded` events,
which a default server does not write; LUNA also counts the `compaction`
rows (OCH-16), like Codex's compaction `token_count`. The script checks each
reader against the database over its own window and splits the per-model
difference into compaction rows and window skew. It exits 0 only when both
readers match the database exactly. A plain sum of the `assistant` rows is a
third, different total: it lacks the fork-copy exclusion both readers apply.

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

`scripts/qa-opencode-keepalive-proxy.py` is a fault wrapper for LUNA's own
HTTP client. Give it to LUNA as the OpenCode executable (a harness probe's
first argument, or `OPENCODE_EXECUTABLE`) with the profile wrapper in
`LUNA_KA_REAL_OPENCODE`. It starts the real `opencode serve` and puts a
loopback proxy in front of it. `opencode serve` 2.0.22 advertises
`Keep-Alive: timeout=5` but closes an idle connection at 6.00 s, and a
request that meets that close is lost. The proxy holds the server's idle
close back from LUNA, so a request on a connection idle for
`LUNA_KA_IDLE_MS` (default 6000) is reset every time, not only on a lucky
boundary hit. `LUNA_KA_RESET_REPLIES=N` also resets the first N permission
or form replies. `LUNA_KA_LOG` receives the request lines and faults, never
headers. With `opencode_input_probe`'s `PROBE_ANSWER_DELAY_MS=7000`, this
reproduces the S1 lost answer (round 2) at the harness level.

In the signed QA app, `OPENCODE_EXECUTABLE` is fixed to the profile wrapper,
so the fault goes into the wrapper itself for one run: replace its final
`exec '<opencode>' "$@"` line with `exec /usr/bin/env
LUNA_KA_REAL_OPENCODE='<opencode>' LUNA_KA_RESET_REPLIES=3 LUNA_KA_LOG=<file>
<python3> <repo>/scripts/qa-opencode-keepalive-proxy.py "$@"`. The QA app
starts a new `opencode serve` for each turn, so the next turn goes through
the proxy without a refresh. The proxy resets the first three replies per
server, so LUNA's three delivery attempts fail and the card or question is
shown again; the second answer is delivered. Restore the wrapper with `scripts/qa-opencode-env.sh <profile>`
and check that its SHA-256 matches the value from before the edit (round-3
S1 evidence: `e2e-paid-r3/s1/reshow`).

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
node scripts/qa-opencode-live.mjs answer --run <dir> \
  --action allow-once|deny|cancel|pick:<text>|text:<text>|dismiss|mcp:Accept
node scripts/qa-opencode-live.mjs capture [--run <dir>] [--label <name>]
node scripts/qa-opencode-live.mjs rehearse [run options]   # fake engine, R0
```

`run` also takes `--answer none|allow-once|deny|cancel` (approvals) and
`--pick <option text>` (questions); an approval or question without a policy
ends the turn wait and fails `turn-N-unanswered`. `--capture` records a window
capture after each turn (`--ax` adds AX readback, see below). The driver also
refuses to run while any other Luna GUI is running. `answer` answers the current pending
approval or question on a previous run's chat (an operator helper; the send
lands in that run's manifest). `capture` never sends.

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
its byte offsets, and keeps persistent `WatchDocMessages`, `WatchQueue`,
`WatchSessions`, `WatchTaskCommands` and `WatchNativeTasks` subscriptions for
`observer.json`. Sends use the UI path: `ExecuteTaskCommand` `session.run`
when the chat is idle and `queueMessage` (hold for turn end) when it is busy,
with the message id as the command id. A turn settles from those frames, tied
to its own user row: the session is idle or errored with nothing streaming,
and the reply after the row settled or the session was seen active after it
(a provider wake turn never settles another turn). Error scenarios pass
`--expect-status errored`. At the end it copies the run's slice of each
`opencode-wire-<pid>.jsonl` into `wire/` and splits it into
`native-frames.jsonl` (`sse*` records) and `http.jsonl`, then checks that each
user message appears exactly once. Evidence goes to
`target/qa/opencode/<scenario>/run-*` with a `latest` pointer, or under
`--evidence-root`. `verify-restart` requires a new app PID and checks that each
recorded send survives exactly once with no replayed queue row.
`--provider claude-code` runs the same steps for PD-14 comparison diffs.

### In-app scenario runner (S1..S8)

`scripts/qa-opencode-scenarios.mjs` runs named scenarios through the same
shared plumbing (`scripts/qa-opencode-lib.mjs`, `scripts/qa-opencode-driver.mjs`).
Every write is the UI's RPC with the UI's fences
(`crates/ui/src/task_commands.rs`): approvals through `session.respondApproval`
(card choices become `providerChoice{index}`, otherwise the decision enum),
questions and MCP forms through `session.respondInput`, Stop through a
turn-fenced `session.interrupt`, fork/edit/rewind through `forkTask`,
`editEarlierMessage` and `rewindConversation` with the UI's history review
token, side chats through `CreateSideChat` (native fork, or `forceSnapshot`
with an excerpt) and `SendSideChat`, Review through `nativeReview`, plus
`compactContext`, `setModel`, `steerQueuedNow`, `goal` and image uploads
(`UploadChunk`/`UploadCommit`).

| Id | Validation rows | Mode | Steps |
| --- | --- | --- | --- |
| S1 | OCE-16, OCE-05 | ask | allow once; deny with feedback; cancel turn; single and multi-select question; stale answer after Stop |
| S2 | OCE-01, OCE-21 | full | two native subagents (foreground and background); background shell native-task row; Stop with a running child |
| S3 | OCE-18 | full | `luna` `get_workspace` on fresh, resumed, forked and post-compaction sessions; refused from a native subagent; cross-project request |
| S4 | OCE-04, OCE-05, OCE-09 | ask | `pre`: completed turn, pending approval, queued row, then `restart-required.json`; the QA operator restarts the app gracefully; `post`: readback, stale or live answer, continuation |
| S5 | OCE-10, OCE-24 | full | three marker turns; fork from user and assistant rows; fork recall; native-fork and snapshot side chats (two messages each); edit earlier message; same-chat rewind with unrelated file changes; recall after rewind |
| S6 | OCE-13 | full | catalog before and after refresh; turn with a variant; switch variant with `setModel`; Usage history only with `--allow-usage-store` |
| S7 | OCE-08, OCE-14, OCE-20 | full | image on the first turn, on a queued row and on a steered row (staged SHA-256 compared with `files[]` in the wire log); manual compaction then a `luna` call; project command discovery, `/qa-echo` and the generated title |
| S8 | OCE-23 | full | Review of uncommitted changes, a commit and a base branch; goals readback |

Live-only extras (never rehearsed) add S2X, S4X, S4S, S8X, S9U and S9Q.
S9Q (T3, run for both providers and diff) covers nonblocking questions
through LUNA's `ask_user` tool: the turn ends without anyone answering, the
card stays open, a late answer is delivered as a new turn tagged
`[luna-question:<card id>]`, a different later answer is delivered as an
amendment, repeating the latest answer is idempotent, and an answer sent
while the agent works is steered into the running turn before its final
reply (whether the model then mentions it is recorded as model behaviour). S4S also
checks the engine's provider-neutral Stalled notice: start the mock with a
`--slow-ms` above 120000 so one streamed word is followed by a quiet stretch
longer than the 120 s window; the notice must appear while the turn is still
working and clear when the provider resumes. S9U (T3, run for both providers
and diff) covers unattended answers: a question whose agent marked an option
"(Recommended)" is answered with it after the delay, `holdInput` (the panel's
Keep waiting) keeps the question open, and a question without a
recommendation always waits. Set `unattendedAnswerSeconds` in the QA data
directory's `ui-settings.json` to match `--unattended-ms` (default 15000)
before launching the QA app. S1's cancel-turn step requires the unified
cancel outcome for every provider: Idle, never Errored, with the entry
stamped `aborted`.

```bash
node scripts/qa-opencode-scenarios.mjs list
node scripts/qa-opencode-scenarios.mjs dry-run S1 --provider opencode
node scripts/qa-opencode-scenarios.mjs run S1 --provider opencode --tier T3 \
  --evidence-root /abs/evidence/dir
node scripts/qa-opencode-scenarios.mjs run S1 --provider claude-code --tier T3 \
  --evidence-root /abs/evidence/dir
node scripts/qa-opencode-scenarios.mjs run S4 --provider opencode --tier T3 --phase pre
#   ...the QA operator quits and relaunches the QA app gracefully...
node scripts/qa-opencode-scenarios.mjs run S4 --provider opencode --tier T3 \
  --phase post --run <run dir printed by pre>
node scripts/qa-opencode-diff.mjs compare <opencode run> <claude run> [--allow allow.json]
```

Prompts depend only on the scenario and a per-run nonce, so OpenCode and
Claude get identical text; `--tier T2` OpenCode runs append the mock
provider's `[[mock:...]]` marker. Model-prose criteria (a marker in the reply)
are decisive at T3 and informational at T2; informational rows never decide
`pass.json`. The question steps of S1 and the `luna` calls of S3 and S7 need a
real model (the mock has no scripted question or `luna` tool calls). A failure
stops the scenario, sends at most one Stop per chat it created that is still
running (QF-22) and keeps every artifact; an unknown send stops it without
further writes. Each run directory holds the §2 artifacts plus
`normalized/steps-<phase>.json` (the parity-diff input: per step and chat
role, the document layout, tool multiset, session statuses, queue and
native-task rows, context control, receipts, approval and question shapes),
`normalized/*-frames-<role>.jsonl`, `state/<milestone>.json`,
`screenshots/` and `ax/`, and `manifest.modelUsage`.

Window captures follow QF-19: `screencapture -x -o -l<window>` of the largest
layer-0 window owned by the verified listener PID, only when the no-prompt
Screen Recording probe passes. Otherwise `screenshots/<label>.json` records
`blocked` and the document state stands in. AX readback (a bounded System
Events walk of the QA window into `ax/<label>.json`) is opt-in with `--ax`,
because driving System Events can raise the macOS Automation prompt the first
time. Grant Screen Recording (and, for `--ax`, Automation and Accessibility)
to the terminal that runs the runner when UI evidence is the claim.

`scripts/qa-opencode-diff.mjs compare` refuses runs of different scenarios,
step lists or masked prompt hashes (exit 2), lists every difference by step
and path in `diff.json` and `diff.md`, and exits 1 while any difference is
unexplained. `template <diff.json>` writes an allowance skeleton whose `TODO`
reasons `compare` refuses until each is replaced with the native OpenCode
behaviour it records (and the same note goes into the OpenCode run's
`result.md`).

Offline checks need no app: `self-check` (both scripts) tests the failure
cases above against the shared plumbing; `rehearse [S#|all]` runs every
scenario for both providers against `scripts/qa-opencode-fake-engine.mjs`, a
scripted fake engine the runner spawns itself, and diffs each pair;
`rehearse-faults` drops one `ExecuteTaskCommand` reply (read back by command
id, never resent) and loses one command outright (the run stops with the send
`unknown` and no later writes). Rehearsals are tier R0 and write
`rehearsal.json`, never `pass.json` (QF-21): they prove the runner's plumbing,
not LUNA, OpenCode or Claude behaviour.

### In-app scenario driver: failure cases (written before the tooling)

The scenario runner drives the in-app scenarios S1..S8 through the RPCs the UI
uses. These cases were recorded before its code and each names the outcome the
tooling must produce.

12. **QF-12 approval answered off the UI path.** OpenCode approval cards carry
    `details.choices` (`Deny`, `Allow once`, `Cancel turn`); sending the
    `allowOnce` enum, or a label the card does not offer, is not what the UI
    sends. *Expected:* the driver maps a requested action exactly like
    `approval_decision` in `crates/ui/src/composer.rs` (card choices give
    `providerChoice{index}`, otherwise `allowOnce`/`allowTask`/`deny`/`cancel`)
    through `ExecuteTaskCommand` `session.respondApproval`, records the card
    and the decision, and fails the step when the requested choice is absent
    instead of silently sending Deny.
13. **QF-13 LUNA cross-task approval answered over JSON RPC.** The UI answers
    `lunaCrossTask` approvals through an in-process route only. *Expected:*
    the driver refuses them and records `not-rpc-answerable`.
14. **QF-14 stale or resolved request answered as live.** *Expected:* only an
    unresolved `input` part seen in the live transcript mirror is answered as
    live. Stale-answer probes (after Stop, after restart) are labelled probes;
    their receipt (rejected, conflict or applied-without-effect) is the
    evidence and they are never retried.
15. **QF-15 polling instead of subscriptions.** *Expected:* waits are driven by
    persistent `WatchDocMessages`, `WatchSessions`, `WatchQueue`,
    `WatchTaskCommands` and `WatchNativeTasks` frames. A transcript desync
    (delta `count`, append `len` or a missing anchor) resubscribes for a reset
    and is recorded in `observer.json`. No RPC is issued inside a wait loop;
    snapshots are single reads at milestones.
16. **QF-16 history review token forged or stale.** *Expected:* fork, edit and
    rewind tokens are built like the UI: a fresh `GetTaskCommandSnapshot`, the
    binding's provider session, the chat's account and cwd, and the mirrored
    message text (text parts trimmed, joined by a newline). A conflict receipt
    is recorded and never retried with a refreshed token.
17. **QF-17 unknown outcome replayed.** A lost reply to `ExecuteTaskCommand`,
    `QueueMessage`, `CreateSideChat`, `SendSideChat` or an answer.
    *Expected:* the send is in `manifest.json` before its RPC; a lost task
    command is read back once by `commandId`; without a receipt the send stays
    `unknown`, the scenario stops, and nothing is resent.
18. **QF-18 provider-specific steps break the parity diff.** *Expected:* both
    providers get identical prompts built from the scenario definition and the
    run nonce only; the manifest records each prompt's SHA-256 with the nonce
    masked; the diff tool refuses runs of different scenarios, step lists or
    masked prompt hashes.
19. **QF-19 wrong window captured, or a permission prompt raised.**
    *Expected:* the window is the largest layer-0 CGWindowList window owned by
    the verified QA listener PID; `screencapture -x -o -l<id>` runs only when
    the no-prompt Screen Recording probe passes; otherwise the capture is
    recorded as `blocked` and AX or document readback is used. Screenshots are
    labelled presentation-only.
20. **QF-20 the diff hides real differences.** *Expected:* normalization drops
    only volatile values (ids, timestamps, nonces, model prose, paths) and keeps
    part and tool kinds, layout, statuses, receipt intents and reasons, and
    approval and question shapes. Every difference is listed; an `--allow`
    entry needs a reason and only marks a listed difference as expected.
21. **QF-21 a rehearsal is mislabelled as evidence.** The runner can rehearse
    against a fake engine to prove its own plumbing. *Expected:* rehearsals
    are tier `R0`, write `rehearsal.json` and never `pass.json`, and run only
    against a fake engine the runner spawned itself (the listener PID is that
    child), never against an app.
22. **QF-22 a failed scenario leaves a turn running.** *Expected:* on failure
    the runner sends at most one turn-fenced Stop per chat it created that
    still has an active turn, records it, and never quits the app.
23. **QF-23 the Usage store is read during an OpenCode run.** *Expected:* S6
    calls `GetUsageSnapshot` only with `--allow-usage-store`, which the user
    approves per run (see the known limit above).
24. **QF-24 restart scenarios restart the app.** *Expected:* S4 never quits or
    launches an app. Its `pre` phase leaves a pending approval and a queued
    row, writes `restart-required.json` and exits; the QA operator restarts the
    app gracefully; `post` requires a different app PID.

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
