# SW-00 migration baseline

Source: `83ede04b28fdb760c62eb1dc1d91ebf9388a1689`, 11 October 2026, macOS 27.0.1
(26A434), Apple M3 Max. The plan was added in `7b269800`. Its recorded planning
baseline `06ea5cd5` does not resolve in this repository. Runtime drift since
`7b269800` is in `crates/ui` only (right rail, child-chat pane, related chats).
No engine, harness, proto or RPC source changed, so the plan's owner names still hold.

This ticket records facts, owners and contracts. It ships no runtime feature.
Read the [owner map and workflow set](ownership.md), then the
[failure, media and helper-control contracts](contracts.md). Live status
belongs to `pending_tasks.py`.

## Providers, accounts and modes

| Provider | Installed | LUNA's recorded contract | Account (type only) |
| --- | --- | --- | --- |
| Codex | `codex-cli 0.160.0` | Adapter validated on 0.154.0. The constant is not enforced at runtime. | ChatGPT login; LUNA `codex-native` |
| Claude Code | `2.1.293` | Ask/Auto grant and fixtures recorded on 2.1.286. The echo check is a minimum version, which 2.1.293 meets. | claude.ai, `max`, first-party; LUNA `claude-native` |
| OpenCode | `v2.0.24` | Outside the two-provider switching set. IC keeps its all-provider scope. | not examined |

Both primary CLIs are newer than their recorded versions. Checks of the drift:

- **Claude.** A credential-free launch probe on the real 2.1.293 CLI shows the
  same built-in tool sets as the 2.1.286 record. Ask/Auto get exactly
  `ASK_AUTO_TOOL_GRANT` (23 tools). Full gets the CLI default set, which adds
  `CronCreate` and `Workflow` and drops `Glob`/`Grep`. The probe used an isolated
  config and a closed loopback endpoint, so no model turn ran.
  ([probe](evidence/claude-launch-probe.json))
- **Codex.** All 63 app-server method literals LUNA uses exist in the generated
  0.160.0 schema. Its counts are 142 client requests, 168 server notifications and
  35 server requests. Only `rawResponseItem/completed` is gone, and LUNA merely
  consumed it on child threads. ([drift](evidence/codex-method-drift.json))
- **Not covered by either check.** Behavior beyond the method names and tool
  lists is unverified on the newer CLIs until SW-01/SW-02 run them live.

**Selected switching modes:**

| Provider | Selected modes | Observed native use since 10 July ([counts](evidence/native-mode-usage.json)) |
| --- | --- | --- |
| Codex | **Ask** and **Full** | Codex Desktop: 671 sessions ended in `never`/`danger-full-access` (Full) and 586 in `on-request`/`workspace-write` (Ask). |
| Claude | **Ask** and **Full**; **Auto** secondary | 246 transcripts mostly ran `acceptEdits` (Ask), 94 `bypassPermissions` (Full) and 31 `auto`. |

The 391 Codex `never`/`read-only` sessions are mostly helper and reviewer
threads, not a user mode choice. Codex Auto (`auto_review`) can't be told apart
in turn-context policy fields, so it stays unselected until the user picks it.

All 21 chats in the primary LUNA Dev app run in Full. They are QA and trial
chats, not the working population, and do not override the native choice above.

## Effective delivery

### Existing chats

Source: [`GetRuntimeContract` per chat](evidence/luna-live-baseline.json), read-only.

- **Codex:** 4 chats; all have an acknowledged `codexMcpTools` contract.
- **Claude:** 17 chats. 12 have an acknowledged `claudeMcpTools` contract. The
  other 5 never started a provider session.
- **Staleness:** all 16 acknowledged contracts report `restartRequired: true`.
  Their attached LUNA tool sets are 11, 12 or 26 tools, depending on when the
  provider process last launched. A chat keeps its older tool set until it restarts.
- **Always omitted:** `interrupt_thread`, `archive_thread`, `open_thread`,
  `fork_thread` and `move_thread`, each with the reason "no verified cross-task
  command adapter is attached".
- **Actual calls** ([counts](evidence/luna-chat-tool-usage.json)): four Claude
  root chats called `mcp__luna` 11–26 times. No LUNA chat has ever called
  Executor, Intercom, Slack or another external MCP server.

### Provider readback per launch mode

Source: [`GetIntegrationInventory` with Check provider](evidence/luna-live-baseline.json), both projects.

| Server (provider scope) | Claude Ask/Auto (restricted) | Claude Full | Codex |
| --- | --- | --- | --- |
| `executor` (Claude user scope, HTTP, 3 tools) | not loaded | connected | configured; health and attachment unverified |
| claude.ai Intercom / Slack / Gmail / Calendar / Docs | connected (15/13/23/9/8 tools) | connected | n/a |
| claude.ai Drive / Spotify | needs auth | needs auth | n/a |
| `computer-use`, `moralis-admin-support`, `node_repl`, `RepoPrompt`, `codex` | n/a | n/a | configured in `config.toml` and `config/read`; unverified |

- **claude.ai connectors** load in restricted mode as well. So for Claude
  Ask/Auto, the Executor-only routes are the real gap: Moralis Admin, Infra
  Monitor and Better Stack go through Executor. Intercom and Slack do not.
- **Claude session attachment** is unknown. The session observation is held in
  memory only, and none existed after this engine start.
- **Codex** gives LUNA no MCP status or attachment readback.

### Skills

- **Codex:** 33 home skills (21 enabled) plus 17 app-server skills, and project
  `.agents/skills`.
- **Claude:** 2 global skills plus 1 project skill. Ask/Auto ignore user,
  project and local settings, and with them commands, skills, output styles and
  hooks (`harness/src/claude/extensions.rs`). In those modes skills can only
  arrive as guidance (SW-03).

### Fresh and resumed sessions

- **Claude, fresh:** the probe confirms what each mode loads. Restricted runs drop
  a user-scope fixture server and keep the dynamic `--mcp-config` fixture. Full
  loads both.
- **Claude, resumed:** not proven live. Against the closed endpoint the CLI
  recorded no user line, so `--resume` had nothing to load.
- **Codex, fresh and resumed:** the adapter attaches the `luna` endpoint to every
  spawn, including resumes. That is by construction and also unproven live.
- **Who closes this:** live resumed-session evidence with a nonce read is E01,
  owned by SW-01.

## Fresh owner results

The 13 engine suites named in [verification](../verification.md) all pass:
107 passed, 0 failed, 2 ignored. Run time was 79 s with `CARGO_INCREMENTAL=0`
([summary](evidence/engine-suites-summary.txt)). The two ignored tests are
live-gated: `real_claude_remembers_codeword_across_engine_restart` (spends tokens)
and `live_github_observation_uses_real_bounded_read_api`. A passing baseline
means later tickets must show a failing reproduction before changing these owners.

## Repeat and evidence

[commands.json](evidence/commands.json) has the exact commands, exit codes and
timings. [platform.json](evidence/platform.json) has versions.
[sha256.json](evidence/sha256.json) hashes the committed evidence plus the local
raw run directory, `target/qa/luna-only-workflows/sw00-20261011T151529Z/` (logs
and the generated Codex schema). The two new runners are read-only:

```sh
LUNA_BASELINE_EVIDENCE_DIR=target/qa/luna-only-workflows/<run> node scripts/qa-workflow-baseline.mjs
python3 scripts/qa-claude-launch-probe.py target/qa/luna-only-workflows/<run>
```

The first runner reads a running engine (`LUNA_IPC_PORT`, default 59777) and
starts only the providers' control-only and catalog processes, as Settings >
Check provider does. The second needs no login and runs in about a second. It
exits 1 if the restricted tool set differs from `ASK_AUTO_TOOL_GRANT`, or if a
restricted run loads a user-scope server. Its Full run is the positive control:
it proves the isolated config is actually read. Re-run it whenever the CLI
version changes.

**Limitations:**
- No live model turn, nonce read, approval, restart or external MCP call was
  made. Those are the positive cases of E01–E05, owned by SW-01, SW-02 and SW-04.
- The mode counts are per file. They are lower bounds and include every
  originator.
- Committed evidence holds IDs, type labels, counts and redacted provider text
  only. It contains no titles, prompts, customer data or credentials.
