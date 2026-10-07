# A — OpenCode v2 native capability inventory

Owner: workstream A. Research date: 2026-10-06. Conventions, pins, evidence levels and category codes: see `00-baseline.md`.
Source citations are `path:line @527f0b93` relative to the v2.0.22 snapshot root (= repo root at commit `527f0b931d1f9b3ebd34e106c51b31ce5db5b075`).

Status: COMPLETE (2026-10-06). 183 provisional rows: SES 18 · INP 14 · TURN 10 · OUT 9 · TOOL 21 · MOD 16 · PERM 13 · EXT 20 · AGT 13 · ADV 8 · WS 14 · HOST 12 · CLIENT 15. LIFE (transport/lifecycle) is not tabled separately: the transport-relevant rows are A-WS-01/02/03 and A-CLIENT-10/13, and payload detail belongs to workstream B.

Method: (1) BFS crawl of every v2 docs page (§1); (2) read-only inspection of the v2.0.22 snapshot (`packages/{cli,core,schema,protocol,plugin,server,tui,gui-extensions,plugin-browser,desktop,ai}` plus root `github/`, `sdks/`); (3) `--help` of every installed v2.0.22 subcommand plus `opencode1 --help`; (4) `git log/diff/grep/merge-base` across the pins. Nothing was run that changes state: no `run`, `serve`, TUI, auth, or model calls.

## 1. Docs navigation tree (retrieval timestamps)

Retrieval method: `curl -sSL` of each page (raw HTML, read-only GET) by a BFS crawler over every `/v2/docs/**` link found on any v2 docs page; 63 unique pages, all HTTP 200. Timestamps are UTC per page (2026-10-06). The site has five top tabs (Docs, CLI, Build, API, Console) plus a "V1" link to `/docs` (v1 docs, out of scope). Note: the Intro page download links point at **2.0.6** binaries (`opencode.ai/files/bin/2.0.6/...`), i.e. the docs build is not pinned to v2.0.22.

API tab: single long page `https://opencode.ai/v2/docs/api/` with 29 operation families (counts in sidebar): server 3, location 2, agent 2, plugin 3, session 44, model 2, generate 1, provider 2, integration 11, mcp 6, credential 5, project 2, form 1, permission 7, filesystem 4, command 1, skill 1, rpc 1, event 1, pty 7, persistentPty 11, shell 5, reference 1, worktree 4, vcs 6, debug 2, migration 1, websearch 2, config 3 (payloads owned by workstream B).

- **Docs** (top tab)
  - Intro — https://opencode.ai/v2/docs/ — HTTP 200 @ 2026-10-06T20:31:45Z
  - Config — https://opencode.ai/v2/docs/config/ — HTTP 200 @ 2026-10-06T20:31:48Z
  - Migrate from V1 — https://opencode.ai/v2/docs/migrate-v1/ — HTTP 200 @ 2026-10-06T20:31:49Z
  - Troubleshooting — https://opencode.ai/v2/docs/troubleshooting/ — HTTP 200 @ 2026-10-06T20:31:50Z
  - _Configure_
  - Agents — https://opencode.ai/v2/docs/agents/ — HTTP 200 @ 2026-10-06T20:31:50Z
  - Models — https://opencode.ai/v2/docs/models/ — HTTP 200 @ 2026-10-06T20:31:51Z
  - Skills — https://opencode.ai/v2/docs/skills/ — HTTP 200 @ 2026-10-06T20:31:52Z
  - Themes — https://opencode.ai/v2/docs/themes/ — HTTP 200 @ 2026-10-06T20:31:52Z
  - Commands — https://opencode.ai/v2/docs/commands/ — HTTP 200 @ 2026-10-06T20:31:53Z
  - Plugins — https://opencode.ai/v2/docs/plugins/ — HTTP 200 @ 2026-10-06T20:31:53Z
  - Providers — https://opencode.ai/v2/docs/providers/ — HTTP 200 @ 2026-10-06T20:31:54Z
  - Websearch — https://opencode.ai/v2/docs/websearch/ — HTTP 200 @ 2026-10-06T20:31:55Z
  - Network — https://opencode.ai/v2/docs/network/ — HTTP 200 @ 2026-10-06T20:31:55Z
  - Snapshots — https://opencode.ai/v2/docs/snapshots/ — HTTP 200 @ 2026-10-06T20:31:56Z
  - Compaction — https://opencode.ai/v2/docs/compaction/ — HTTP 200 @ 2026-10-06T20:31:56Z
  - Formatters — https://opencode.ai/v2/docs/formatters/ — HTTP 200 @ 2026-10-06T20:31:57Z
  - References — https://opencode.ai/v2/docs/references/ — HTTP 200 @ 2026-10-06T20:31:57Z
  - Attachments — https://opencode.ai/v2/docs/attachments/ — HTTP 200 @ 2026-10-06T20:31:58Z
  - Tools — https://opencode.ai/v2/docs/tools/ — HTTP 200 @ 2026-10-06T20:31:59Z
  - MCP servers — https://opencode.ai/v2/docs/mcp-servers/ — HTTP 200 @ 2026-10-06T20:31:59Z
  - Permissions — https://opencode.ai/v2/docs/permissions/ — HTTP 200 @ 2026-10-06T20:32:00Z
  - Policies — https://opencode.ai/v2/docs/policies/ — HTTP 200 @ 2026-10-06T20:32:00Z
  - Instructions — https://opencode.ai/v2/docs/instructions/ — HTTP 200 @ 2026-10-06T20:32:01Z
  - Sharing — https://opencode.ai/v2/docs/sharing/ — HTTP 200 @ 2026-10-06T20:32:01Z
  - Warming — https://opencode.ai/v2/docs/warming/ — HTTP 200 @ 2026-10-06T20:32:02Z
- **CLI** (top tab)
  - Intro — https://opencode.ai/v2/docs/cli/ — HTTP 200 @ 2026-10-06T20:31:46Z
  - TUI — https://opencode.ai/v2/docs/cli/tui/ — HTTP 200 @ 2026-10-06T20:32:04Z
  - Settings — https://opencode.ai/v2/docs/cli/config/ — HTTP 200 @ 2026-10-06T20:32:04Z
  - Web — https://opencode.ai/v2/docs/cli/web/ — HTTP 200 @ 2026-10-06T20:32:05Z
  - Providers — https://opencode.ai/v2/docs/cli/providers/ — HTTP 200 @ 2026-10-06T20:32:03Z
  - Commands — https://opencode.ai/v2/docs/cli/commands/ — HTTP 200 @ 2026-10-06T20:32:05Z
  - ACP — https://opencode.ai/v2/docs/cli/acp/ — HTTP 200 @ 2026-10-06T20:32:06Z
  - Theme — https://opencode.ai/v2/docs/cli/theme/ — HTTP 200 @ 2026-10-06T20:32:07Z
  - Plugins — https://opencode.ai/v2/docs/cli/plugins/ — HTTP 200 @ 2026-10-06T20:32:07Z
  - Keybinds — https://opencode.ai/v2/docs/cli/keybinds/ — HTTP 200 @ 2026-10-06T20:32:08Z
- **Build** (top tab)
  - Intro — https://opencode.ai/v2/docs/build/ — HTTP 200 @ 2026-10-06T20:31:46Z
  - _Plugins_
  - Overview — https://opencode.ai/v2/docs/build/plugins/ — HTTP 200 @ 2026-10-06T20:32:09Z
  - RPC — https://opencode.ai/v2/docs/build/plugins/rpc/ — HTTP 200 @ 2026-10-06T20:32:10Z
  - CLI — https://opencode.ai/v2/docs/build/plugins/cli/ — HTTP 200 @ 2026-10-06T20:32:10Z
  - Migrate from V1 — https://opencode.ai/v2/docs/build/plugins/migrate-v1/ — HTTP 200 @ 2026-10-06T20:32:11Z
  - _Client_
  - JavaScript — https://opencode.ai/v2/docs/build/client/ — HTTP 200 @ 2026-10-06T20:32:12Z
  - _SDK_
  - Overview — https://opencode.ai/v2/docs/build/sdk/ — HTTP 200 @ 2026-10-06T20:32:13Z
  - Cloudflare — https://opencode.ai/v2/docs/build/sdk/cloudflare/ — HTTP 200 @ 2026-10-06T20:32:13Z
  - _Effect_
  - Plugins — https://opencode.ai/v2/docs/build/plugins/effect/ — HTTP 200 @ 2026-10-06T20:32:14Z
  - RPC — https://opencode.ai/v2/docs/build/plugins/effect/rpc/ — HTTP 200 @ 2026-10-06T20:32:15Z
  - Client — https://opencode.ai/v2/docs/build/client/effect/ — HTTP 200 @ 2026-10-06T20:32:16Z
  - SDK — https://opencode.ai/v2/docs/build/sdk/effect/ — HTTP 200 @ 2026-10-06T20:32:16Z
- **API** (top tab)
  - API reference (single page; 29 families) — https://opencode.ai/v2/docs/api/ — HTTP 200 @ 2026-10-06T20:31:47Z
- **Console** (top tab)
  - Intro — https://opencode.ai/v2/docs/console/ — HTTP 200 @ 2026-10-06T20:31:48Z
  - Models — https://opencode.ai/v2/docs/console/models/ — HTTP 200 @ 2026-10-06T20:32:17Z
  - Providers — https://opencode.ai/v2/docs/console/providers/ — HTTP 200 @ 2026-10-06T20:32:17Z
  - Web Search — https://opencode.ai/v2/docs/console/websearch/ — HTTP 200 @ 2026-10-06T20:32:18Z
  - Go — https://opencode.ai/v2/docs/console/go/ — HTTP 200 @ 2026-10-06T20:32:03Z
  - _Workspace_
  - Basics — https://opencode.ai/v2/docs/console/workspaces/ — HTTP 200 @ 2026-10-06T20:32:18Z
  - Members — https://opencode.ai/v2/docs/console/members/ — HTTP 200 @ 2026-10-06T20:32:19Z
  - SSO — https://opencode.ai/v2/docs/console/sso/ — HTTP 200 @ 2026-10-06T20:32:19Z
  - SCIM — https://opencode.ai/v2/docs/console/scim/ — HTTP 200 @ 2026-10-06T20:32:20Z
  - Budgets — https://opencode.ai/v2/docs/console/budgets/ — HTTP 200 @ 2026-10-06T20:32:20Z
  - Billing — https://opencode.ai/v2/docs/console/billing/ — HTTP 200 @ 2026-10-06T20:32:20Z
  - _API_
  - Overview — https://opencode.ai/v2/docs/console/api/ — HTTP 200 @ 2026-10-06T20:32:21Z
  - Inference — https://opencode.ai/v2/docs/console/api/inference/ — HTTP 200 @ 2026-10-06T20:32:22Z
  - Providers — https://opencode.ai/v2/docs/console/api/providers/ — HTTP 200 @ 2026-10-06T20:32:22Z
  - Budgets — https://opencode.ai/v2/docs/console/api/budgets/ — HTTP 200 @ 2026-10-06T20:32:23Z


## 2. Installed CLI (`--help`) summary

E1 commands run 2026-10-06 (read-only): `~/.opencode/bin/opencode --version` → `opencode v2.0.22`; `~/.opencode/bin/opencode --help`; `~/.opencode/bin/opencode <sub> --help` for every subcommand; `~/.opencode/bin/opencode <sub> <subsub> --help` for every nested subcommand (zsh `${=c}` word-split; note a quoted two-word arg is silently treated as the positional `<directory>` and prints root help); `~/.opencode/bin/opencode1 --help` (v1.18.34) for comparison. Command tree source: `packages/cli/src/commands/commands.ts:15-554 @527f0b93`.

Root: `opencode [flags] [<directory>]` starts the TUI attached to the **background service** (auto-started/replaced on version mismatch, `packages/cli/src/commands/handlers/default.ts:35-49 @527f0b93`). Root flags: `--standalone` (private server), `--server <url>` (remote/explicit server; `OPENCODE_PASSWORD`/legacy `OPENCODE_SERVER_PASSWORD`, `packages/cli/src/env.ts:10-13`), `--auto`, `-c/--continue`, `-s/--session <id>` (creates the session with that ID if missing, `default.ts:52`), `--prompt`. Hidden: `--yolo`, `--dangerously-skip-permissions` (`commands.ts:31-35`). Global: `-h`, `-v`, `--wizard` (interactive wizard for any command), `--completions bash|zsh|fish|sh`, `--log-level`, `--print-logs` (server logs only with `--standalone`).

| Subcommand | Purpose / notable flags (E1) | Source `@527f0b93` |
| --- | --- | --- |
| `upgrade [target]` (alias `update`) | `-m/--method curl\|npm\|pnpm\|bun\|yarn\|vp\|brew` | `commands.ts:60-74`, `handlers/upgrade.ts`, `services/updater.ts` |
| `uninstall` | `--keep-config`, `--keep-data`, `--dry-run`, `--force` | `commands.ts:75-98` |
| `acp` | Agent Client Protocol server over stdio | `commands.ts:99`, `src/acp/*` |
| `api <operationId \| METHOD path…>` | raw request to running server; `-d`, `-H`, `--param k=v` | `commands.ts:100-116`, `handlers/api.ts` |
| `debug agents\|config\|paths [name]` | paths: db, home, data, config, cache, state, tmp, bin, log, repos | `commands.ts:117-142` |
| `auth list\|login\|logout\|export\|import\|switch` | integrations + credentials; `login [target] --method --answer k=v` (target may be a well-known provider URL); `export` prints secrets as JSON; `switch` = active account per integration (multi-account) | `commands.ts:143-220` |
| `mcp list\|add\|auth\|logout` | `add <name> [-- cmd…] --url --header --env --global` (default writes project config) | `commands.ts:221-259` |
| `plugin list [--builtin]\|add <pkg>\|check\|update\|remove` | npm or Git specifiers; add/remove edit **global** config | `commands.ts:260-303` |
| `models` | list models | `commands.ts:304-307` |
| `stats` | `--days --year --all --project --models --tools --cost --full --limit --json` ("shareable" usage stats incl. tool reliability) | `commands.ts:308-339` |
| `mini` | minimal interactive UI; `--fork --replay/--no-replay --replay-limit -m --agent --prompt` | `commands.ts:340-375` |
| `run [message…]` | non-interactive; `-c -s --fork -m provider/model#variant --agent --format default\|json -f/--file --title --thinking --auto` | `commands.ts:376-417` |
| `session list\|delete\|export\|import` | `list -n --format table\|json`; delete cascades to children; `export --sanitize`; `import <file\|URL> --directory` | `commands.ts:418-470` |
| `service start\|restart\|status\|stop\|get\|set\|unset` | background server lifecycle + service config/env | `commands.ts:471-512`, `services/service-config.ts` |
| `reload` | reload configuration on the running server | `commands.ts:513-518` |
| `pair [--url]` | print one-time links to connect a browser or app | `commands.ts:519-536` |
| `serve` | `--hostname --port --cors (repeat) --service --stdio` ("v2 API and web server") | `commands.ts:537-551` |

v1 → v2 CLI delta (E1 comparison with `opencode1 --help`): **removed** top-level `attach <url>` (→ `--server`), `web` (→ `serve` hosts web UI + `pair`), `github` (agent install), `pr <number>`, `agent` (create/list), `db`, top-level `export`/`import` (→ `session export/import`), `providers` (→ `auth`), `completion` (→ `--completions`), root `--mdns/--mdns-domain/--port/--hostname/--pure/-m/--agent/--fork/--mini` flags. **Added** `api`, `service`, `reload`, `pair`, `mini` (subcommand), `auth export/import/switch`, `plugin check/update/remove/list`, `stats --tools`, `--wizard`, `--standalone`, `--server`.

## 3. Category inventories

Abbreviations used in every table:
- **docs** column: `D:x/` = `https://opencode.ai/v2/docs/x/` (all retrieved 2026-10-06 20:31–20:32Z, see §1). `D:api#op` = API reference anchor.
- **source** column: paths are relative to `packages/` of the v2.0.22 snapshot and pinned `@527f0b93` (omitted per cell for brevity); `root:` = repo root. Protocol endpoint anchors are `protocol/src/groups/<file>.ts:<line>`; payloads are workstream B's.
- **v2.0.22 support** uses the baseline status vocabulary for the *native Execution* dimension only.
- **later** column: `24` = v2.0.24 (`e7a34f09`), `head` = v2-head (`4a0f273f`); "—" = no capability-relevant change found in `git log`/`git diff` for the cited files.
- **evidence**: E1 runtime help, E2 pinned source, E3 docs, E5 inference.

### SES — sessions / persistence

| ID | capability | native surface | v2.0.22 | later | gates / prereqs | docs | source | evidence | notes |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| A-SES-01 | Durable sessions in server-owned SQLite DB (`opencode.db`, channel-specific name for non-release channels; `OPENCODE_DB` override) | server (background service) | supported | — | background service or `--standalone`/`serve` | D:troubleshooting/, D:cli/ | `cli/src/database-path.ts:4-13`; `core/src/session.ts:122,307` | E2,E3 | Sessions are owned by the shared service, not the client. |
| A-SES-02 | List sessions (cursor pagination, order, `parentID` filter; root-only via `null`) | HTTP `session.list`; CLI `session list -n --format table\|json` | supported | — | — | D:api#session.list, D:cli/commands/ | `protocol/src/groups/session.ts:55-63,181`; `cli/src/commands/commands.ts:420-436` | E1,E2,E3 | CLI lists top-level sessions of current project only. |
| A-SES-03 | Create session (optional `parentID` → linked child at parent location); `--session <id>` creates with a chosen ID | HTTP `session.create`; CLI root/run/mini `-s` | supported | — | — | D:api#session.create | `protocol/src/groups/session.ts:220-237`; `cli/src/commands/handlers/default.ts:52-60` | E1,E2,E3 | Client-chosen session IDs are a v2 nicety. |
| A-SES-04 | Get/update session (title, permissions, metadata) | HTTP `session.get`/`session.update` | supported | — | — | D:api#session.update | `protocol/src/groups/session.ts:286,361-376`; `schema/src/session.ts:31-58` | E2,E3 | Session record carries `parentID`, `fork`, `agent`, `model`, `cost`, `tokens`, `outcome` (succeeded/failed/interrupted), `title`, `location`, `subpath`, `metadata`, `permissions`, `revert`. |
| A-SES-05 | Delete session (recursive to children; stops active execution) | HTTP `session.remove`; CLI `session delete` | supported | — | — | D:api#session.remove, D:build/plugins/ | `protocol/src/groups/session.ts:299-307`; `commands.ts:437-443` | E1,E2,E3 | No archive state found in the session schema (E5). |
| A-SES-06 | Fork session (copy projected history before a message, or full) | HTTP `session.fork`; CLI `run/mini --fork`; TUI `/fork`; ACP `session/fork` | supported | — | — | D:api#session.fork, D:cli/acp/ | `protocol/src/groups/session.ts:312-323`; `core/src/session.ts:122,307`; `tui/src/routes/session/index.tsx:938` | E1,E2,E3 | |
| A-SES-07 | Move session to another directory/worktree at a safe boundary (inbox `move` item) | HTTP `session.move`; Code Mode `opencode.session_move`; desktop "Move to worktree" | supported | — | — | D:tools/, D:api#session.move | `protocol/src/groups/session.ts:381-390`; `core/src/tool/plugin/opencode.ts:110-131`; `schema/src/session-inbox.ts:44` | E2,E3 | Instruction state is retained across moves (D:instructions/). |
| A-SES-08 | Rename session (agent-callable) + automatic title generation by hidden `title` agent | HTTP `session.update`; Code Mode `opencode.session_rename`; TUI `/rename` | supported | — | title agent model: `agents.title.model` (v1 `small_model` maps here) | D:tools/, D:agents/, D:migrate-v1/ | `core/src/tool/plugin/opencode.ts:90-106`; `core/src/plugin/agent.ts:139-145`; `core/src/session/title.ts` | E2,E3 | |
| A-SES-09 | Export session JSON (optionally `--sanitize` redaction) / import from file or URL (parents before children) | HTTP `experimental.session.export/import`; CLI `session export/import` | supported (experimental API) | — | — | D:api#experimental.session.export, D:cli/commands/ | `protocol/src/groups/session.ts:243-269`; `core/src/session/transfer.ts:36,62,180` | E1,E2,E3 | Desktop "Export session" in Context tab (`gui-extensions/src/usage`). |
| A-SES-10 | Session stats (activity, usage, tool reliability over time range) | HTTP `experimental.session.stats`; CLI `stats`; TUI `/stats` | supported | 24: stats chart UI tweaks only | — | D:cli/commands/ | `protocol/src/groups/session.ts:201-215`; `core/src/session/stats.ts`; `cli/src/commands/handlers/stats.ts` | E1,E2,E3 | "Shareable" stats output; `--tools` = tool reliability. |
| A-SES-11 | Active sessions (foreground drains owned by this process) | HTTP `session.active` | supported | — | — | D:api#session.active | `protocol/src/groups/session.ts:274-280` | E2,E3 | Busy/idle discovery. |
| A-SES-12 | Session messages / single message / active context (post-compaction) | HTTP `session.messages`, `session.message.get`, `session.context` | supported | — | — | D:api | `protocol/src/groups/message.ts:68`; `session.ts:570-578,784` | E2,E3 | |
| A-SES-13 | Durable session event log with follow (replay after sequence) | HTTP `session.log` (experimental) | supported (experimental) | — | — | D:api#session.log | `protocol/src/groups/session.ts:727-741` | E2,E3 | Owned in detail by B. |
| A-SES-14 | Viewed/unread marker for idle transitions | HTTP `session.view` | supported | 24: TUI surfaces sessions awaiting input (`a2a543c8681`) | — | D:api#session.view | `protocol/src/groups/session.ts:889-898` | E2,E3 | Attention state shared across clients. |
| A-SES-15 | Durable turn recovery after server restart (per-turn resume budget, default 10) | server internal | supported | — | — | (undocumented) | `core/src/session/execution/restart.ts:25-33,75-82` | E2 | Undocumented source capability. |
| A-SES-16 | Snapshots / undo / redo / revert (stage → commit/clear), files restored from separate internal git object DB | HTTP `session.revert.stage/clear/commit`; TUI `/undo` `/redo` (`<leader>u/r`), message "Revert"; config `snapshots` | supported | 24: app commits staged revert before switching selection (`19f8610c0a0`) | Git repo required for file restore; rejected while running | D:snapshots/, D:api#session.revert.stage | `protocol/src/groups/session.ts:534-559`; `core/src/session/revert.ts`; `core/src/snapshot.ts:76-98` | E2,E3 | Captures tracked + non-ignored untracked ≤2 MiB under active dir only. |
| A-SES-17 | Session sharing | config `share`, TUI `/share` | **unavailable** (accepted, not implemented) | head: still `unavailable` | — | D:sharing/, D:config/ | `schema/src/config.ts:41-50`; `tui/src/routes/session/index.tsx:895-903` (`run: () => unavailable("Sharing")`) | E2,E3 | v1 had share; v2 docs: "does not support session sharing yet". `/unshare` is `enabled: false`. |
| A-SES-18 | v1 → v2 data migration (sessions/credentials from v1 storage, `auth.json`) | HTTP `migration.v1.status` (experimental); DB migration | supported | — | — | D:cli/providers/ | `protocol/src/groups/migration.ts:18`; `core/src/database/v1-migration.bun.ts` | E2,E3 | |

### INP — inputs / attachments / queue / draft

| ID | capability | native surface | v2.0.22 | later | gates / prereqs | docs | source | evidence | notes |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| A-INP-01 | Prompt admission is durable inbox input with delivery `steer` (default) or `queue` | HTTP `session.prompt`; TUI Enter = steer, `Ctrl+X Enter` = queue | supported | 24: app/desktop aligned steer/queue UX (`2db9c2e1b39`, `754b144263d`) | — | D:cli/tui/, D:build/plugins/ (prompt hook) | `schema/src/session-inbox.ts:11,37-44`; `protocol/src/groups/session.ts:395-412` | E2,E3 | Steering is native and first-class. |
| A-INP-02 | Inspect / cancel / re-prioritise pending inbox items (switch queue↔steer) | HTTP `session.inbox.list/update/cancel` | supported | — | — | D:api#session.inbox.list | `protocol/src/groups/session.ts:608-644` | E2,E3 | Inbox includes user, synthetic, compaction and move items. |
| A-INP-03 | File/directory attachments by `file:` URI (with `?start&end` line range) or `data:` URI; 20 MiB/item; text, dir listing, PNG/JPEG/GIF/WebP visible to model; PDF/audio/video NOT sent | HTTP prompt `files`; TUI `@path#20-45`; CLI `run -f`; desktop picker/paste/drag | supported | — | model must accept images | D:attachments/, D:cli/tui/ | `schema/src/prompt-input.ts`; `core/src/image.ts` | E2,E3 | HTTP(S) attachment URIs not supported. read tool does pass PDFs (see A-TOOL-01) – asymmetric. |
| A-INP-04 | Image normalisation (auto-resize / reject by width, height, base64 size) | config `media.image.*` | supported | — | — | D:attachments/, D:config/ | `schema/src/config/media.ts`; `core/src/config/plugin/image.ts` | E2,E3 | v1 `attachment` → v2 `media`. |
| A-INP-05 | Agent mentions and explicit skill selection inside a prompt (`@skill-id`) | prompt `agents`, `skills`; TUI `@` | supported | — | — | D:skills/, D:build/plugins/ | `schema/src/prompt-input.ts`; `core/src/session/prompt.ts` | E2,E3 | Agent mentions do not switch the active agent. |
| A-INP-06 | Reference attachments (named external dirs / cached git repos) | config `references`; prompt attachment by alias; HTTP `reference.list` | supported | — | git for remote refs | D:references/ | `protocol/src/groups/reference.ts:24`; `core/src/reference.ts`; `core/src/repository.ts:166` | E2,E3 | Background clone/refresh ≥24h; `OPENCODE_REPO_CLONE_GITHUB_BASE_URL` override (undocumented). |
| A-INP-07 | Synthetic (non-user) input injection | HTTP `session.synthetic`; plugin `ctx.session.synthetic` | supported | — | — | D:api#session.synthetic | `protocol/src/groups/session.ts:456-474` | E2,E3 | Useful for external notifications into a session. |
| A-INP-08 | User shell command in session (`!cmd`), output stays in transcript | HTTP `session.shell`; TUI `!` shell mode | supported | — | — | D:cli/tui/ | `protocol/src/groups/session.ts:479-493` | E2,E3 | Emits `shell.started/ended`. |
| A-INP-09 | Slash commands (custom Markdown/JSON, built-ins `/init`, `/review`, MCP prompts as `/<server>:<prompt>`), `$ARGUMENTS`/`$1..$n`, `` !`shell` `` blocks | HTTP `session.command`, `command.list`; TUI `/` | supported | — | shell blocks run outside permission flow | D:commands/, D:mcp-servers/ | `core/src/plugin/command.ts:30-69`; `protocol/src/groups/session.ts:417-432`; `protocol/src/groups/command.ts:9` | E2,E3 | |
| A-INP-10 | Explicit skill activation for a session | HTTP `experimental.session.skill` | supported (experimental) | — | — | D:api | `protocol/src/groups/session.ts:437-451` | E2,E3 | |
| A-INP-11 | API-managed per-session instruction entries (attach/replace/remove, announced at next step) | HTTP `experimental.session.instructions.entry.*` | supported (experimental) | — | — | D:instructions/, D:api | `protocol/src/groups/session.ts:663-703`; `core/src/session/instruction-entry.ts` | E2,E3 | Distinct from AGENTS.md. |
| A-INP-12 | Per-session shell environment replacement | HTTP `session.environment` | supported | — | — | D:api#session.environment | `protocol/src/groups/session.ts:875-884`; `core/src/session/environment.ts` | E2,E3 | Shell tool also injects `OPENCODE_SESSION_ID` (`core/src/tool/plugin/shell.ts:213`) and `OPENCODE_TERMINAL=1` (`core/src/shell.ts:271`) – undocumented. |
| A-INP-13 | Prompt drafting in `$EDITOR` | TUI `/editor` (`Ctrl+X E`) | supported (client) | — | `VISUAL`/`EDITOR` | D:cli/tui/ | `tui/src/component/prompt/index.tsx:556` | E2,E3 | Client-only. |
| A-INP-14 | Editor selection / active file as prompt context (connects to an IDE's MCP-over-WebSocket server via `CLAUDE_CODE_SSE_PORT` or `OPENCODE_EDITOR_SSE_PORT`; Zed via its DB) | TUI `prompt.editor` setting | supported (client) | — | IDE extension exposing the port | D:cli/config/ (`prompt.editor`) | `tui/src/context/editor.ts:1,117-121`; `tui/src/editor-zed.ts:189` | E2,E3 | Reuses Claude Code IDE-extension port – undocumented mechanism. |

### TURN — steering / interrupt / completion / retry / recovery

| ID | capability | native surface | v2.0.22 | later | gates / prereqs | docs | source | evidence | notes |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| A-TURN-01 | Mid-turn steering (steer input delivered at next step boundary; wakes execution) | HTTP prompt `delivery: steer`, inbox update | supported | — | — | D:cli/tui/, D:api#session.inbox.update | `protocol/src/groups/session.ts:635-644` | E2,E3 | |
| A-TURN-02 | Interrupt with optional resume of pending steering input (`resume=true`) | HTTP `session.interrupt`; ACP `session/cancel`; plugin `interrupt({continue})` | supported | — | owning process only | D:api#session.interrupt, D:cli/acp/ | `protocol/src/groups/session.ts:747-762` | E2,E3 | Returns `interrupted` boolean (idle no-op). |
| A-TURN-03 | Move running foreground tools (shell, subagent) to background | HTTP `session.background` | supported | — | backgroundable tools | D:api#session.background | `protocol/src/groups/session.ts:768-778` | E2,E3 | OpenCode-only ergonomic. |
| A-TURN-04 | Wait for idle | HTTP `experimental.session.wait`; plugin `wait` | supported (experimental) | — | — | D:api | `protocol/src/groups/session.ts:519-529` | E2,E3 | |
| A-TURN-05 | Provider retry with backoff, Retry-After cap 15 min, plugin `retry` hook can override; retry status surfaced (ACP `opencode/retry`) | server; plugin hook `session.retry` | supported | — | — | D:build/plugins/, D:cli/acp/ | `core/src/session/runner/retry.ts:27-59` | E2,E3 | Timeouts retried up to 3× (D:providers/). |
| A-TURN-06 | Context-overflow recovery: compact and retry once | server | supported | — | `compaction.auto` | D:compaction/ | `core/src/session/runner/step.ts:102-113`; `core/src/session/compaction.ts` | E2,E3 | |
| A-TURN-07 | Max agent steps (`steps`), final step strips tools and asks for summary | config `agents.<id>.steps` | supported | — | — | D:agents/ | `core/src/session/runner/max-steps.ts:3-11`; `core/src/session/runner/step.ts:90` | E2,E3 | |
| A-TURN-08 | Session outcome tracking (succeeded / failed / interrupted) | session record | supported | — | — | (API schema) | `schema/src/session.ts:44` | E2 | |
| A-TURN-09 | Durable resume of an in-flight turn after crash/restart (budget 10) | server | supported | — | — | (undocumented) | `core/src/session/execution/restart.ts:25-33` | E2 | See A-SES-15. |
| A-TURN-10 | Doom-loop detection | — | missing | — | — | D:permissions/ ("`doom_loop` … not current V2 Core permission actions") | — | E3 | v1 had `doom_loop`; removed in v2. |

### OUT — output / reasoning / tool results / diffs

| ID | capability | native surface | v2.0.22 | later | gates / prereqs | docs | source | evidence | notes |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| A-OUT-01 | Streaming text + reasoning parts; reasoning show/hide; custom reasoning field for OpenAI-compatible models | events; CLI `run --thinking`; cli.json `session.thinking`; config `compatibility.reasoningField` | supported | — | — | D:models/, D:cli/config/ | `cli/src/commands/commands.ts:376-417`; `schema/src/session-message.ts` | E1,E2,E3 | |
| A-OUT-02 | NDJSON event output for scripts | CLI `run --format json` | supported | — | — | D:cli/commands/ | `cli/src/run/noninteractive.ts` | E1,E3 | |
| A-OUT-03 | Tool-result truncation with full output retained in managed file (`tool_output.max_lines/max_bytes`, defaults 2000 / 51200) | config `tool_output`; tools shell/webfetch | supported | — | — | D:config/, D:tools/ | `core/src/tool-output.ts:16,55,139`; `core/src/tool/AGENTS.md` (Output) | E2,E3 | Managed dirs are pre-allowed for `external_directory`. |
| A-OUT-04 | Structured per-file turn diffs (`from`/`to` range, context lines) + file-diff patches on edit/write/patch results | HTTP `session.diff`; TUI `/diff` viewer (scopes branch/committed/working/turn); desktop Review panel | supported | 24: desktop review controls moved (`e86ec987112`) | — | D:api#session.diff, D:cli/config/ (diffs) | `protocol/src/groups/session.ts:583-602`; `core/src/tool/plugin/file-diff.ts:4-22` | E2,E3 | |
| A-OUT-05 | Images in tool results (read returns PNG/JPEG/GIF/WebP/PDF to model) and transcript previews | read tool; cli.json `session.image_preview` | supported | — | model image input | D:tools/, D:attachments/ | `core/src/tool/plugin/read.ts`; `core/src/tool.ts` (normalizes images) | E2,E3 | |
| A-OUT-06 | Transient text generation from session context (no history mutation) — powers `/btw` | HTTP `session.generate`, `experimental.generate.text`; TUI/desktop `/btw` | supported | 24: desktop keeps each `/btw` in its own tab (`494d70707be`) | — | D:cli/tui/, D:api | `protocol/src/groups/session.ts:709-722`; `protocol/src/groups/generate.ts:8`; `tui/src/feature-plugins/prompt/btw.tsx:57` | E2,E3 | Side questions are OpenCode-only. |
| A-OUT-07 | Usage/cost per session (tokens incl. reasoning & cache, USD cost), ACP `usage_update` | session record; HTTP stats; desktop Context tab | supported | — | model cost metadata | D:cli/acp/ | `schema/src/session.ts:41-42`; `core/src/session/usage.ts` | E2,E3 | |
| A-OUT-08 | Structured (JSON-schema) final output for runs | — | missing (no evidence found) | — | — | — | (none found by `rg structured\|json_schema\|outputSchema` in `core/src/session`) | E5 | |
| A-OUT-09 | Markdown/mermaid/LaTeX rendering in clients | TUI, app/desktop | supported (client) | 24: mermaid fix; head: KaTeX dollar math (`a7523ac4b79`) | — | — | `merman/`, `latex/` packages; `tui/` | E2,E5 | Client rendering only. |

### TOOL — built-in tool registry

Registry model (E2): built-ins are internal plugins registered through `Tool.Service.transform` (`core/src/plugin/internal.ts:213-247`; `core/src/tool/AGENTS.md`). Tool filtering is catalog visibility, not authorization; each leaf asserts its own permission. Tools with `codemode: true` are reachable only through the Code Mode `execute` tool; `execute` is a reserved name (`core/src/tool.ts:238,313-314`). Typed failures return `ToolFailure` to the model; user declines/question dismissals propagate as typed failures at `SessionModelRequest.executeTool`; decline-with-feedback becomes `Permission.CorrectedError` and the model continues (`core/src/permission.ts:66-85,276-292`). "Allow always" saves the tool's proposed patterns (`save:`), project-scoped (`permission.ts:299-318`).

| ID | tool (model name) | params (E2) | approval (action : resource; saved pattern) | failure paths (E2) | v2.0.22 | later | docs | source | evidence | notes |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| A-TOOL-01 | `read` | `path`, `offset` (1-based), `limit` (≤2000) | `read` : normalized path; `.env*` asks by default; external paths also `external_directory` | missing path, binary non-media, oversize; 2000 lines / 50 KiB per page; lines >2000 chars cut; media ≤20 MiB (PNG/JPEG/GIF/WebP/PDF) | supported | — | D:tools/ | `core/src/tool/plugin/read.ts:17-25,30,122-170`; `core/src/tool/read-filesystem.ts` | E2,E3 | Loads nested AGENTS.md between target and workspace (D:instructions/). |
| A-TOOL-02 | `glob` | `pattern`, `path?` (relative), `hidden?`, `limit?` (100) | `glob` : pattern; save `*` | path missing / not dir; 30 s timeout | supported | — | D:tools/ | `core/src/tool/plugin/glob.ts:17-28,46,71,84-132` | E2,E3 | |
| A-TOOL-03 | `grep` | `pattern` (non-empty), `path?`, `include?`, `literal?`, `caseSensitive?` (true), `limit?` (100) | `grep` : pattern (not path) | path missing; 30 s timeout; invalid regex | supported | — | D:tools/ | `core/src/tool/plugin/grep.ts:17-38,65,106-163` | E2,E3 | ripgrep / fff backends (`OPENCODE_DISABLE_FFF`). |
| A-TOOL-04 | `edit` | `path`, `oldString`, `newString`, `replaceAll?` | `edit` : path; save `*` | identical strings; empty oldString; not found; directory; zero or >1 matches | supported | — | D:tools/ | `core/src/tool/plugin/edit.ts:24-34,110-123,136-222` | E2,E3 | Runs formatter if enabled. **No LSP diagnostics** (`edit.ts:107` TODO). Hidden for GPT-family models (patch instead). |
| A-TOOL-05 | `write` | `path`, `content` | `edit` : path; save `*` | — | supported | — | D:tools/ | `core/src/tool/plugin/write.ts:23-28,47-81` | E2,E3 | Creates parents; no LSP (`write.ts:44`). |
| A-TOOL-06 | `patch` | `patchText` (`*** Add/Update/Delete File`, moves) | `edit` : every affected path; save `*` | empty patch; verification failure; partial-apply reports completed ops | supported (GPT models only) | — | D:tools/ | `core/src/tool/plugin/patch.ts:21-25,68-81,98-199,296-307` | E2,E3 | Exposed when model id contains `gpt-` and not `oss`/`gpt-4`; otherwise edit/write. |
| A-TOOL-07 | `shell` | `command`, `workdir?`, `timeout?` ms (default 120000; 0 = none), `background?` | `shell` : each scanner-produced command (tree-sitter default; portable scanner opt-in); saves command prefixes; external workdir/inferred dirs → `external_directory` | execution failure; scanner error (portable) is an error, not a denial | supported | head: portable scanner default on `local`/`dev` channels (`81b92c043ff`) | D:tools/, D:permissions/ | `core/src/tool/plugin/shell.ts:23,47-60,103,125-138,192,213,273` | E2,E3 | Background calls notify session on finish; full output in managed file. Runs with host authority (no OS sandbox). |
| A-TOOL-08 | `webfetch` | `url`, `format` (markdown default / text / html), `timeout?` s (≤120) | `webfetch` : URL; save `*` | binary/non-text rejected | supported | — | D:tools/ | `core/src/tool/plugin/webfetch.ts:15,23-31,104-127` | E2,E3 | |
| A-TOOL-09 | `websearch` | `query` | `websearch` : query | provider 429 → failover (random mode) / fail when all cooling | supported | — | D:tools/, D:websearch/ | `core/src/tool/plugin/websearch.ts:24-43`; `core/src/plugin/websearch/*.ts` | E2,E3 | Providers: exa, firecrawl, parallel, tavily, tinyfish (+Console hosted). `websearch: false` removes tool. |
| A-TOOL-10 | `question` | `questions[]` (header, prompt, choices, `multiple`; free-form always allowed) | `question` : `*`; build/plan allow, general/explore deny | client lacks form support → cancelled; dismissal cancels | supported | 24: ACP lets model continue when a question can't be shown (`e2a700dc901`) | D:tools/ | `core/src/tool/plugin/question.ts:23-27,49-96` | E2,E3 | Backed by the Form system (A-PERM-08). |
| A-TOOL-11 | `skill` | `id` | `skill` : skill id; save id | unknown id / load failure | supported | — | D:tools/, D:skills/ | `core/src/tool/plugin/skill.ts:12-54` | E2,E3 | Injects body + base dir + ≤10 sample files. |
| A-TOOL-12 | `subagent` | `agent`, `description`, `prompt`, `model?` (`provider/model#variant`), `sessionID?` (continue child), `background?` | `subagent` : agent id; save id | invalid model ref; depth limit (default 1; `experimental.subagent_depth`); non-subagent agent | supported | 24: preserves subagent text block boundaries (`f28f62439e3`) | D:tools/, D:agents/ | `core/src/tool/plugin/subagent.ts:19-48,65,79-142` | E2,E3 | `model` param is undocumented in D:tools/. |
| A-TOOL-13 | `execute` (Code Mode) | JavaScript program calling catalog tools (`tools.<ns>.<tool>()`) | `execute` : `*` (controls availability); nested tools enforce own rules | no fs/imports/timers/fetch in sandbox | supported | head: tool name/namespace length caps dropped (`c2f87853c6a`) | D:tools/, D:mcp-servers/ | `core/src/codemode/tool.ts:76`; `core/src/tool.ts:238-277`; `codemode/` package | E2,E3 | MCP tools default into Code Mode (`codemode: true`). |
| A-TOOL-14 | `opencode_session_rename`, `opencode_session_move` (Code Mode namespace `opencode`) | `sessionID?`, `title` / `directory` | none (no built-in permission action) | empty title; move failure | supported | — | D:tools/ | `core/src/tool/plugin/opencode.ts:84-131` | E2,E3 | |
| A-TOOL-15 | `opencode_models` (Code Mode) | `query?`, `provider?`, `all?`, `limit?` (1-100, default 20), `offset?` | default action = tool name | list failure | supported | — | **undocumented** | `core/src/tool/plugin/opencode.ts:27-44,135-196` | E2 | Lets the agent resolve a model name before launching a subagent. |
| A-TOOL-16 | `opencode_list_mcp_resources`, `opencode_read_mcp_resource` (Code Mode) | — / `server`, `uri` | `opencode_list_mcp_resources` / `opencode_read_mcp_resource`; saves server / `server:*` | server not connected or no resources | supported | — | D:mcp-servers/ (resources mentioned) | `core/src/tool/plugin/mcp-resource.ts:10-92` | E2,E3 | Tool names undocumented. |
| A-TOOL-17 | `browser.*` (Code Mode namespace; desktop-attached Chromium via CDP: tabs, navigation, snapshot/search, click/input, screenshots, files ≤5 MiB, console/network, CPU/heap/trace, Lighthouse, tunnel) | per operation | `browser` : `*`; agents deny by default, session-level allow added when desktop attaches a browser; no per-op prompts | needs focused visible tab for screenshots | gated (desktop app) | head: browser bar redesign (desktop) | D:tools/ | `plugin-browser/src/tools.ts:35-49`; `plugin-browser/src/connection.ts:27-49`; `plugin-browser/src/rpc.ts`; `gui-extensions/src/browser/*` | E2,E3 | OpenCode-only. |
| A-TOOL-18 | MCP tools `<server>_<tool>` | server-defined | `<server>_<tool>` : `*` | timeouts (startup 30 s / catalog 30 s / execution 12 h) | supported | 24: retry transient MCP connect failures (`421a04f5fde`) | D:mcp-servers/ | `core/src/tool/mcp.ts`; `core/src/mcp/index.ts` | E2,E3 | Session ID sent in `_meta["ai.opencode/sessionID"]`. |
| A-TOOL-19 | Plugin-defined tools (namespaced `<ns>_<tool>`, override by effective name) | plugin `ctx.tool.transform` | plugin-chosen action (defaults to name) | — | supported | — | D:build/plugins/ | `plugin/src/effect/tool.ts`; `core/src/tool/AGENTS.md` | E2,E3 | |
| A-TOOL-20 | Tool input repair (malformed model tool input) | internal plugin | n/a | — | supported | — | (undocumented) | `core/src/plugin/tool-input-repair.ts`; `internal.ts:214` | E2 | |
| A-TOOL-21 | LSP tool/diagnostics; todo-list tool; plan enter/exit tools; batch tool | — | — | — | **missing** | head: LSP still TODO (`git grep` @4a0f273f) | D:migrate-v1/ (LSP not run; `batch_tool` ignored) | `core/src/file-mutation.ts:131`; `internal.ts:228-245` (no such tools); `rg todowrite` finds only `core/src/database/v1-migration.bun.ts` | E2,E3 | v1.18.34 tool dir (local `dev` @4ac0d9c3 `packages/opencode/src/tool/`) has `lsp`, `todo`/`todowrite`, `plan` (plan-enter/exit), `task`, `apply_patch`; v2 replaces task→`subagent`, apply_patch→`patch`, plan tools→`plan` agent + reminders, and drops lsp/todo. v1 todo records only survive as migrated history (E5). |

### MOD — models / providers / accounts / variants / usage / compaction / effective config

| ID | capability | native surface | v2.0.22 | later | gates / prereqs | docs | source | evidence | notes |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| A-MOD-01 | Model catalog from bundled + fetched models.dev-style catalog (`models.opencode.ai/api.json`), provider plugins, local discovery, config | HTTP `model.list`, `model.default`, `provider.list/get`; CLI `models`; TUI `/models` | supported | 24: non-text models excluded from catalog/default (`944959566fb`); head: models snapshot refresh | `OPENCODE_MODELS_URL`, `OPENCODE_MODELS_PATH`, `OPENCODE_DISABLE_MODELS_FETCH` | D:models/, D:providers/ | `core/src/models-dev.ts:16,252,270`; `protocol/src/groups/model.ts:10,26`; `protocol/src/groups/provider.ts:10,25`; `cli/src/server-process.ts:104-106` | E1,E2,E3 | Catalog/credentials are **location-scoped** (per project). |
| A-MOD-02 | Model selector `provider/model#variant` (string or expanded object) for session, run, agent, command, subagent | HTTP `session.switchModel`; CLI `-m`; config | supported | — | variant must exist in catalog | D:models/ | `core/src/model-resolver.ts`; `protocol/src/groups/session.ts:345-356` | E1,E2,E3 | Root config `model` keeps no variant in v2.0.22 (docs caveat). |
| A-MOD-03 | Variants (reasoning effort / budgets) as array with per-variant settings/headers/body; TUI `/variants` (`/thinking`, `/effort`) | config `providers.*.models.*.variants[]`; TUI | supported | 24: GitLab Duo variant resolution (`5ae8a3c1d3d`); Vertex Claude effort fix | — | D:models/ | `core/src/variant.ts`; `tui/src/app.tsx:931` | E2,E3 | ACP exposes `effort` config option. |
| A-MOD-04 | Custom providers/models (package, `settings`, `headers`, `body`, `capabilities`, `limit`, `cost`, `compatibility`, `disabled`, `canonical`, aliasing via `modelID`) | config `providers` | supported | 24: AI SDK v6 providers installed by default (`fa992020c54`) | — | D:providers/, D:models/ | `schema/src/config/provider.ts:12-15`; `core/src/plugin/provider/configured.ts` | E2,E3 | Fallback caps: tools, text+image in, 200k ctx/32k out. |
| A-MOD-05 | Built-in provider plugins (bedrock, azure (+Entra ID via az), cerebras, chatgpt (ChatGPT/Codex OAuth), cloudflare AI gateway/workers, cohere, digitalocean, github-copilot (device OAuth, GHE), gitlab, google-vertex, kilo, llmgateway, lmstudio, modal, nvidia, ollama, openai, opencode (Console/Zen/Go), openrouter, perplexity, poe, sap-ai-core, snowflake-cortex, venice, vercel, vllm, xai, zenmux) + native `@opencode/ai` runtimes | internal plugins | supported | 24: native Vercel AI Gateway, Venice, Cohere, Google Interactions protocol, Bedrock Mantle Anthropic; legacy OpenAI OAuth renamed "Codex" (`d318cf41250`); ChatGPT `/models` sync paused (`209346a0feb`) | provider credentials | D:providers/ | `core/src/plugin/provider/*.ts`; `core/src/plugin/provider.ts`; `ai/src/providers/*` | E2,E3 | No entitlement claims made here. |
| A-MOD-06 | Local model auto-discovery (Ollama, LM Studio, vLLM) with background refresh | internal plugins `opencode.provider.{ollama,lmstudio,vllm}` | supported | — | local server running | D:models/, D:providers/ | `core/src/plugin/provider/{ollama,lmstudio,vllm}.ts` | E2,E3 | Disable via `"plugins": ["-opencode.provider.ollama"]`. |
| A-MOD-07 | Integrations & credentials: methods API key / OAuth (browser, device code, code paste) / command / environment; provider forms (e.g. Azure resource, GHE domain) | HTTP `integration.*` (11 ops), `credential.*` (5); CLI `auth login --method --answer`; TUI `/connect` | supported | — | interactive terminal for key/form entry in CLI | D:cli/providers/, D:api | `plugin/src/effect/integration.ts:12-100`; `core/src/integration.ts`; `protocol/src/groups/integration.ts:16-192`; `protocol/src/groups/credential.ts:8-61` | E1,E2,E3 | Well-known provider URL login (`auth login <url>`, `experimental.integration.wellknown.add`). |
| A-MOD-08 | Multi-account per integration (newest active; switch, rename, delete; export/import incl. secrets) | CLI `auth list/switch/logout/export/import`; HTTP `credential.activate/update/remove` | supported | — | — | D:cli/providers/ | `cli/src/commands/commands.ts:143-220`; `core/src/credential.ts` | E1,E2,E3 | Credentials in server SQLite; legacy `auth.json` imported once. |
| A-MOD-09 | Compaction: auto near limit, manual (idempotent `id`), `keep.tokens` (15000), `buffer` (10% default), structured summary; native provider compaction (OpenAI Responses encrypted checkpoint) per provider/model | config `compaction`, `providers.*.settings.compaction.type`; HTTP `session.compact`; ACP `/compact`; plugin `compaction` hook | supported | 23/24: honor compaction agent's model (`cb389aa679b`); explain provider-blocked compaction (`64744ce0340`) | — | D:compaction/, D:config/ | `core/src/session/compaction.ts:41,68,191-216,893`; `protocol/src/groups/session.ts:499-513` | E2,E3 | v1 `prune`/`tail_turns` ignored. |
| A-MOD-10 | Session warming (keep provider cache warm with transient requests; default off; 4 min / 30 min) | config `warming` | supported | — | real token cost | D:warming/ | `core/src/plugin/warming.ts:11-24` | E2,E3 | OpenCode-only. |
| A-MOD-11 | Provider transport: HTTP with header/chunk timeouts (5 min defaults), whole-request `timeout`; WebSocket session transport for OpenAI/xAI/Azure Responses | config `providers.*.settings.{headerTimeout,chunkTimeout,timeout,transport}` | supported | — | supported route | D:providers/, D:config/ | `schema/src/config/provider.ts:12-15` | E2,E3 | Plugins with HTTP hooks force HTTP. |
| A-MOD-12 | Per-agent model and request overlays (`agents.<id>.model`, `request.headers/body`) | config | partial — model honoured; `request` overlay preserved but **not sent** | — | — | D:agents/ (warning) | `schema/src/config/agent.ts:12-21` | E2,E3 | Docs-stated limitation. |
| A-MOD-13 | Usage & cost accounting (session tokens: input/output/reasoning/cache; USD) and lifetime stats | session record; HTTP stats; CLI `stats --cost --models`; desktop Context tab | supported | — | catalog cost data | D:cli/commands/ | `schema/src/session.ts:41-42`; `core/src/session/usage.ts`; `core/src/session/stats.ts` | E1,E2,E3 | |
| A-MOD-14 | Effective-config introspection: config sources list, config get/shells, experimental config patch (shell only), location reload | CLI `debug config`, `reload`; HTTP `config.get/shells/update`, `location.get/reload` | supported | — | — | D:cli/commands/, D:api | `protocol/src/groups/config.ts:9-35`; `protocol/src/groups/location.ts:36-51`; `schema/src/config.ts:112-115` | E1,E2,E3 | `Config.Patch` only supports `shell`. |
| A-MOD-15 | Config layering: global `~/.config/opencode/opencode.json(c)` < direct `opencode.json(c)` far→near < `.opencode/opencode.json(c)` far→near; `OPENCODE_CONFIG` (explicit file), `OPENCODE_CONFIG_CONTENT` (inline), `OPENCODE_CONFIG_DIR`, `OPENCODE_DISABLE_PROJECT_CONFIG` (alias `OPENCODE_CONFIG_PROJECT_DISABLE`); hot reload; v1 fields normalized in memory | config files + env | supported | — | — | D:config/, D:migrate-v1/ | `core/src/config/discovery.ts:11,30-75`; `core/src/config/normalize.ts`; `cli/src/server-process.ts:109-114`; `core/src/config/watch.ts` | E2,E3 | Discovery walks to filesystem root (above repo root). `cli.json` is separate (client-only). |
| A-MOD-16 | Provider usage policy (`provider.use` allow/deny), incl. Console-delivered | config `experimental.policies` | supported (experimental) | — | — | D:policies/ | `core/src/config/plugin/policy.ts`; `core/src/managed-policy.ts:7` | E2,E3 | Replaces v1 `enabled_providers`/`disabled_providers`. |

### PERM — permissions / sandbox / approvals / questions / elicitation

| ID | capability | native surface | v2.0.22 | later | gates / prereqs | docs | source | evidence | notes |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| A-PERM-01 | Ordered rules `{action, resource, effect: allow\|ask\|deny}`, last match wins, wildcard `*`/`?`, no match → ask; multi-resource ops: any deny → deny, else any ask → ask | config `permissions`, `agents.<id>.permissions` | supported | — | — | D:permissions/ | `core/src/permission.ts:86-96`; `schema/src/permission.ts` | E2,E3 | Layering: lower-priority config → global → agent rules appended. |
| A-PERM-02 | Default base policy: allow all, ask `external_directory`, ask `*.env`/`*.env.*` reads, allow `*.env.example`; managed dirs (tool-output, shell output, tmp, global config) pre-allowed | built-in | supported | **head: `external_directory` ask removed from defaults and managed-dir allowances deleted (`36a38058fb6`, 2026-10-06)** | — | D:permissions/ | `schema/src/agent.ts:39-51`; `core/src/agent.ts:59-64` | E2,E3 | Docs (retrieved 20:31Z) still show the v2.0.22 default. |
| A-PERM-03 | Approval replies `once` / `always` (save tool-proposed patterns as project-scoped allow rules) / `reject` (rejects all pending in session, optional feedback message → model continues) | HTTP `session.permission.reply`, `permission.request.list`; TUI; ACP options | supported | 24: ACP settles permissions replied elsewhere (`ec90ad50d36`) | client must answer asks | D:permissions/, D:api | `schema/src/permission.ts:41`; `core/src/permission.ts:276-318`; `protocol/src/groups/permission.ts:23-117` | E2,E3 | |
| A-PERM-04 | Saved approvals list/remove | HTTP `permission.saved.list/remove` | supported | — | — | D:api | `protocol/src/groups/permission.ts:37-49`; `core/src/permission/saved.ts` | E2,E3 | Never override a configured deny. |
| A-PERM-05 | Per-session permission rules (session record `permissions`) | HTTP `session.update`; plugin `ctx.session.update` | supported | — | — | (API schema; used by browser plugin) | `schema/src/session.ts:57`; `plugin-browser/src/connection.ts:36-49` | E2 | Undocumented as a user feature. |
| A-PERM-06 | Auto-approve modes: `--auto` (non-denied auto-approved), hidden `--yolo`, `--dangerously-skip-permissions`; TUI `session.permissions: autoaccept` | CLI flags; cli.json | supported | — | — | D:cli/config/ | `cli/src/commands/commands.ts:26-36` | E1,E2,E3 | Configured denies still enforced. |
| A-PERM-07 | Hard-deny policies (`permission` action, `<action>:<value>` resources) evaluated after rules and saved approvals; broader config wins; Console last | config `experimental.policies`; Console | supported (experimental) | — | — | D:policies/ | `core/src/config/plugin/policy.ts`; `core/src/plugin/internal.ts:271` (policy plugin guarded) | E2,E3 | Message "Blocked by configuration policy" / "Blocked by <org>'s policy". |
| A-PERM-08 | Forms: generic structured question/elicitation records (session-scoped and location-global), list/create/get/reply/cancel | HTTP `session.form.*`, `form.list`; question tool; MCP elicitation | supported | — | interactive client | D:api, D:tools/ | `protocol/src/groups/session.ts:797-870`; `protocol/src/groups/form.ts:12`; `core/src/form.ts` | E2,E3 | External clients can create forms too. |
| A-PERM-09 | MCP elicitation (form + URL modes) mapped to Forms under a global pseudo-session | MCP client capability | supported | — | — | (not in D:mcp-servers/) | `core/src/mcp/client.ts:84-101,165-190`; `core/src/mcp/index.ts:91-94,170-248` | E2 | Undocumented in v2 docs. |
| A-PERM-10 | Shell command scanner (tree-sitter bash/PowerShell; experimental portable scanner) producing per-command resources and inferred directories | built-in; config `experimental.portable_shell_scanner` | supported | 24: portable bash/pwsh scanner soundness fixes; head: portable default on local/dev | — | D:permissions/ | `core/src/tool/plugin/shell.ts:125-138`; `schema/src/config/experimental.ts:8-10` | E2,E3 | |
| A-PERM-11 | Permission evaluation hook (plugins can escalate/relax allow/ask; configured deny final) | plugin `ctx.permission.hook("evaluate")` | supported | — | — | D:build/plugins/ | `plugin/src/effect/permission.ts` | E2,E3 | Enables LLM-based safety reviewers. |
| A-PERM-12 | OS-level sandbox | — | **missing** (shell runs with host user authority) | — | — | D:tools/, D:permissions/ (warnings) | (no sandbox implementation for local exec; `rg sandbox` only finds remote workspace drivers) | E2,E3 | Remote workspace "sandboxes" exist as a driver concept (A-WS-07). |
| A-PERM-13 | Child-session permission inheritance | subagent | partial: child uses **its own** agent rules (not a subset of parent); parent's `subagent` rules gate launch; ACP additional-directory grants copied to children | — | — | D:agents/, D:permissions/, D:cli/acp/ | `core/src/tool/plugin/subagent.ts:118-142` | E2,E3 | |

### EXT — MCP / plugins / skills / instructions / memory / hooks / formatters / LSP

| ID | capability | native surface | v2.0.22 | later | gates / prereqs | docs | source | evidence | notes |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| A-EXT-01 | MCP servers: local stdio + remote Streamable HTTP; `disabled`, `cwd`, `environment`, `headers`, `{env:NAME}` substitution; per-server `codemode`; timeouts startup/catalog/execution; protocol `legacy`/`auto`/`2026-07-28` | config `mcp.servers`, `mcp.timeout`; CLI `mcp list/add`; HTTP `mcp.list`, `experimental.mcp.add/remove/connect/disconnect`; TUI `/mcps` | supported | 24: retry transient connect failures; head: always report `opencode` as MCP client name (`b78d10cd716`) | — | D:mcp-servers/ | `schema/src/config/mcp.ts:7-22`; `schema/src/mcp.ts:23-66`; `protocol/src/groups/mcp.ts:10-90`; `cli/src/commands/commands.ts:221-259` | E1,E2,E3 | No MCP over SSE transport. |
| A-EXT-02 | Remote MCP OAuth (discovery, PKCE, refresh, dynamic client registration, pre-registered client, `auth_server_metadata_url`) | CLI `mcp auth/logout`; TUI `/mcps` | supported | 24: OAuth token-exchange failures shown on callback page (`4aba11e9d54`) | browser for auth | D:mcp-servers/ | `core/src/mcp/oauth.ts:475` | E1,E2,E3 | |
| A-EXT-03 | MCP prompts → slash commands `/<server>:<prompt>` with positional args | commands registry | supported | — | — | D:mcp-servers/ | `core/src/plugin/command.ts:57-69` | E2,E3 | |
| A-EXT-04 | MCP resources (catalog + read tools) and server instructions injected as `<mcp_instructions>` (with change diffs) | HTTP `mcp.resource.catalog`; Code Mode tools; system prompt | supported | — | server must expose them | D:mcp-servers/, D:instructions/ | `protocol/src/groups/mcp.ts:90`; `core/src/mcp/instructions.ts:12-105`; `core/src/tool/plugin/mcp-resource.ts` | E2,E3 | |
| A-EXT-05 | MCP list-changed notifications (tools/prompts/resources), roots (legacy era), elicitation; session ID in `_meta` | MCP client | supported | — | — | D:mcp-servers/ (session id only) | `core/src/mcp/client.ts:143-190` | E2,E3 | **Sampling not implemented** (no handler found, E5). |
| A-EXT-06 | Plugins (v2 API, Effect or Promise flavour): loaded from `plugins` config (npm, Git, local path, `file://`, object with `options`), auto-discovered `.opencode/plugin(s)/` and global plugins dir; `-id`/wildcard disable directives; built-ins listable | config `plugins`; CLI `plugin list [--builtin]/add/check/update/remove`; HTTP `plugin.list/check/update` | supported | — | Bun/Node runtime in service | D:plugins/, D:build/plugins/, D:cli/plugins/ | `schema/src/config/plugin.ts`; `core/src/plugin/source-directory.ts:7`; `core/src/plugin/update.ts`; `protocol/src/groups/plugin.ts:10-40`; `core/src/config/normalize.ts:185-188` | E1,E2,E3 | **v1 plugins do not run** (breaking change). Console & policy plugins cannot be disabled (`core/src/plugin/internal.ts:271`). |
| A-EXT-07 | Plugin context domains: agent, aisdk, command, event (subscribe), integration, mcp, model, generate, permission, plugin, provider, reference, rpc, session, shell, skill, storage, tool, vcs, websearch, worktree, experimental.terminal | plugin API | supported | — | — | D:build/plugins/ | `plugin/src/effect/plugin.ts:27-58` | E2,E3 | Context is essentially an in-process server client. |
| A-EXT-08 | Plugin hooks: session `prompt`, `context`, `compaction` (can supply result), `generate`, `title` (can supply result), `model.request`, `http.request`, `http.response`, `experimental.ws.handshake/send/receive`, `retry`; `permission.evaluate`; `shell.create.before`; `tool.execute.before/after`; aisdk hooks | plugin `ctx.<domain>.hook()` | supported (ws hooks experimental) | — | — | D:build/plugins/ (Hooks) | `plugin/src/effect/session.ts:139-171`; `plugin/src/effect/shell.ts:16`; `plugin/src/effect/tool.ts:23-34`; `plugin/src/effect/aisdk.ts:5-21`; e.g. `core/src/plugin/plan.ts:42-60` | E2,E3 | |
| A-EXT-09 | Plugin RPC: plugins expose typed methods/errors/events callable by other plugins and clients | HTTP `rpc.call`; `@opencode/plugin/rpc` | supported | — | — | D:build/plugins/rpc/ | `protocol/src/groups/rpc.ts:14`; `plugin/src/rpc.ts` | E2,E3 | Generic extension channel for external clients. |
| A-EXT-10 | Plugin storage (per-plugin KV) | plugin `ctx.storage` | supported | — | — | D:build/plugins/ | `plugin/src/storage.ts` | E2,E3 | |
| A-EXT-11 | TUI/CLI plugins (Solid components, slots, session panels, routes/tabs, commands/keymaps, dialogs, toasts, attention); server-advertised plugins with a TUI entry auto-load, `cli.json` for CLI-only plugins | `@opencode/plugin/tui`; cli.json `plugins` | supported (client) | 24: select dialog search/actions (`8380c864bf3`) | TUI | D:cli/plugins/, D:build/plugins/cli/ | `plugin/src/tui/*`; `tui/src/feature-plugins/*` | E2,E3 | Client-only extension surface. |
| A-EXT-12 | Desktop/web GUI extensions (built-in: browser, btw, debug, file, pairing, review, ssh, summary, terminal, updater, usage, wsl) | app/desktop extension host | supported (client) | — | desktop/web | (not in docs) | `gui-extensions/AGENTS.md`; `gui-extensions/src/*/index.ts` | E2 | Third-party GUI extension packaging appears supported by the host (`desktop/src/main/extension/*`) – unverified. |
| A-EXT-13 | Skills: `SKILL.md` dirs or root `*.md`; discovery `.opencode/skills`, `~/.config/opencode/skills`, compat `.claude/skills`, `.agents/skills` (global + project walk), `skills` config paths/URLs (HTTP catalog `index.json`); `opencode/autoinvoke:false`; per-skill permissions; built-in skills `opencode`, `report` | config `skills`; HTTP `skill.list`; skill tool; TUI `/skills`; `@skill` | supported | 23: `disable-model-invocation` frontmatter (`f3a23e52b33`) — **absent in v2.0.22** | — | D:skills/ | `core/src/config/plugin/compatibility.ts:40-41`; `core/src/config/plugin/skill.ts`; `core/src/plugin/skill.ts:24-41`; `protocol/src/groups/skill.ts:9` | E2,E3 | Docs list `disable-model-invocation` as supported → v2.0.22 disagreement. |
| A-EXT-14 | Instructions: `AGENTS.md` only (global `~/.config/opencode/AGENTS.md` + upward to home/project root; nested discovered on read/list); live update diffs; **no CLAUDE.md fallback**; `instructions` config accepted but **not loaded** | files; `OPENCODE_DISABLE_PROJECT_CONFIG` | partial | — | — | D:instructions/, D:migrate-v1/ | `core/src/config/plugin/instruction.ts:37-71`; `core/src/instruction-discovery.ts:17,88-145` | E2,E3 | v1 `instructions` globs/URLs and CLAUDE.md fallback regress. |
| A-EXT-15 | Built-in `/init` (guided AGENTS.md setup) and `/review` (commit/branch/pr; default uncommitted) | commands | supported | — | — | D:commands/ (built-ins mentioned generically) | `core/src/plugin/command.ts:30-44`; `core/src/plugin/command/{initialize,review}.txt` | E2,E3 | |
| A-EXT-16 | Persistent/auto memory | — | **missing** | — | — | — | (no memory subsystem in `core/src`; `rg -il memory` hits only unrelated files) | E5 | Users rely on AGENTS.md. |
| A-EXT-17 | Formatters after edit/write/patch (27 built-ins; custom `command`/`extensions`/`environment`; disabled by default) | config `formatter` | supported | — | formatter binaries | D:formatters/ | `core/src/formatter.ts`; `core/src/config/plugin/formatter.ts`; `schema/src/config/formatter.ts` | E2,E3 | v1 had formatters on by default? (unverified). |
| A-EXT-18 | LSP servers / diagnostics | config `lsp` accepted & preserved | **missing** (no runtime) | head: still missing | — | D:migrate-v1/ ("does not run language servers") | `schema/src/config/lsp.ts:7-15`; `core/src/config/normalize.ts:473-480`; `core/src/tool/plugin/edit.ts:107` | E2,E3 | Desktop summary panel still has "Configured LSPs" strings (`gui-extensions/src/summary/i18n/en.ts`). |
| A-EXT-19 | Web search providers as plugin domain (exa, firecrawl, parallel, tavily, tinyfish, Console hosted; `random` failover with cooldown) | config `websearch`; HTTP `websearch.providers/query` | supported | — | provider key or Console | D:websearch/, D:console/websearch/ | `core/src/plugin/websearch/*.ts`; `protocol/src/groups/websearch.ts:10-25`; `core/src/plugin/provider/opencode.ts:25-27,344-345` | E2,E3 | `websearch.query` usable directly by external clients. |
| A-EXT-20 | Model-specific system prompts (anthropic, gpt, gpt-astra, kimi, meta, trinity) and verbosity/optimize plugins | internal | supported | — | — | (undocumented) | `core/src/plugin/system-prompt/*.txt`; `core/src/plugin/verbosity.ts`; `core/src/plugin/optimize.ts:32-43` (OpenAI/Anthropic native-tools optimization intentionally disabled) | E2 | |

### AGT — native agents / subagents / parent-child

| ID | capability | native surface | v2.0.22 | later | gates / prereqs | docs | source | evidence | notes |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| A-AGT-01 | Built-in primary `build` (default; questions allowed) | agent registry | supported | — | — | D:agents/ | `core/src/plugin/agent.ts:87-92` | E2,E3 | |
| A-AGT-02 | Built-in primary `plan` (read-only except `~/.opencode/plan/*`; enter/exit system reminders re-injected after compaction/revert; edit outside plan dir → explicit failure) | agent registry | supported | — | — | D:agents/, D:permissions/ | `core/src/plugin/plan.ts:12-60,93` | E2,E3 | Product decision: LUNA Plan UI cancelled → any LUNA surface over this is `excluded-by-product-decision`; inventory only. |
| A-AGT-03 | Built-in subagent `general` (broad tools; no questions; cannot launch subagents) | agent registry | supported | — | — | D:agents/ | `core/src/plugin/agent.ts:94-103` | E2,E3 | |
| A-AGT-04 | Built-in subagent `explore` (read/glob/grep/webfetch/websearch; asks external dir & .env) | agent registry | supported (no shell) | **24: shell allowed for explore (`7725ac81ebd`) with read-only prompt** | — | D:agents/, D:permissions/ | `core/src/plugin/agent.ts:105-131` | E2,E3 | Docs describe the v2.0.22 rule set. |
| A-AGT-05 | Hidden maintenance agents `compaction`, `title`, `summary` (not selectable) | agent registry | supported (`summary` defined; no caller found) | 23: compaction agent model honoured | — | D:agents/ | `core/src/plugin/agent.ts:133-154` | E2,E3,E5 | `summary` agent appears unused in v2.0.22 core (E5). |
| A-AGT-06 | Custom agents: Markdown (`~/.config/opencode/agents/`, `.opencode/agents/`, legacy `agent/`, `mode/`, `modes/`; nested path → `team/reviewer` ID) or JSON `agents`; fields `model`, `request`, `system`, `description`, `mode` (primary/subagent/all), `hidden`, `color`, `steps`, `disabled`, `permissions`; override/disable built-ins by ID | config / files | supported | — | — | D:agents/, D:migrate-v1/ | `schema/src/config/agent.ts:12-21`; `core/src/config/plugin/agent.ts` | E2,E3 | V1 agent frontmatter auto-translated. No `.claude/agents` compatibility (E2: compat roots used only for skills). |
| A-AGT-07 | Agent selection: `default_agent` (fallback build → first visible primary); switch per session; TUI `/agents`, Shift+Tab cycle | config; HTTP `session.switchAgent`, `agent.list/get`; CLI `--agent`, `debug agents` | supported | — | — | D:agents/ | `schema/src/config.ts:35-37`; `protocol/src/groups/session.ts:329-340`; `protocol/src/groups/agent.ts:10-24` | E1,E2,E3 | Switching agent doesn't change session model. |
| A-AGT-08 | Subagent child sessions (fresh context), foreground or background; continue a child via `sessionID`; parent notified via synthetic message on completion | `subagent` tool; child session `parentID` | supported | 24: app shows running subagents/shells in header (`3a861ff8b0e`) | `subagent` permission | D:tools/, D:agents/ | `core/src/tool/plugin/subagent.ts:19-48,118-142`; `core/src/session/subagent-completion.ts`; `core/src/session/subagent-job.ts`; `core/src/job.ts:15-28` | E2,E3 | |
| A-AGT-09 | Nested subagent depth limit (default 1; `experimental.subagent_depth`) | config | supported | — | — | D:tools/, D:migrate-v1/ | `schema/src/config/experimental.ts:11-13`; `core/src/tool/plugin/subagent.ts:129-132` | E2,E3 | |
| A-AGT-10 | Delegated commands (`subagent: true`) run in background child and report back to parent | command config | supported | — | — | D:commands/ | `schema/src/config/command.ts`; `core/src/session/command.ts` | E2,E3 | |
| A-AGT-11 | Subagent model override by the parent model (`model` param) + agent-callable model search (`opencode_models`) | tools | supported | — | — | (undocumented) | `core/src/tool/plugin/subagent.ts:36-39,79-80`; `core/src/tool/plugin/opencode.ts:135-196` | E2 | |
| A-AGT-12 | Child-session visibility in clients (TUI subagent activity filter; ACP `_meta["opencode/child-session"]` updates) | TUI; ACP | supported (ACP requires `_meta["opencode/child-session-updates"]` client capability in v2.0.22) | 24: TUI keeps filter across navigation | — | D:cli/acp/ | `cli/src/acp/translate.ts:16`; `cli/src/acp/agent.ts:118`; `cli/src/acp/turn.ts:376` | E2,E3 | Docs omit the capability gate. |
| A-AGT-13 | Agent teams / peer-to-peer agents / shared task lists | — | missing | — | — | — | (none found) | E5 | |

### ADV — review / goals / continuations / schedules / monitors / background

| ID | capability | native surface | v2.0.22 | later | gates / prereqs | docs | source | evidence | notes |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| A-ADV-01 | Code review: built-in `/review` command (uncommitted / commit / branch / PR) | command | supported | — | git; `gh` for PR (E5) | D:commands/ | `core/src/plugin/command.ts:43-44`; `core/src/plugin/command/review.txt` | E2,E3 | |
| A-ADV-02 | Review UIs: TUI `/diff` viewer (branch/committed/working/turn scopes, file tree, split/unified), desktop Review panel (files changed, git init) | TUI / desktop | supported (client) | 24: desktop review controls moved | git | D:cli/config/ | `tui/src/feature-plugins/system/diff-viewer.tsx:1203`; `gui-extensions/src/review/*` | E2,E3 | Backed by `vcs.*` and `session.diff` APIs. |
| A-ADV-03 | Background shell jobs and background subagents with completion notifications; job recovery kinds `shell`/`subagent`; statuses running/completed/error/cancelled | shell/subagent `background: true`; HTTP `session.background`, `shell.*` | supported | — | — | D:tools/ | `core/src/job.ts:15-28`; `core/src/session/subagent-completion.ts`; `protocol/src/groups/shell.ts:10-72` | E2,E3 | |
| A-ADV-04 | Side questions `/btw` (transient, not added to history) | TUI / desktop | supported | 24: per-answer tabs (desktop) | — | D:cli/tui/ | `tui/src/feature-plugins/prompt/btw.tsx:57`; `gui-extensions/src/btw/*` | E2,E3 | |
| A-ADV-05 | Session warming (keep-alive) | config `warming` | supported | — | cost | D:warming/ | `core/src/plugin/warming.ts` | E2,E3 | Also A-MOD-10. |
| A-ADV-06 | Goals / continuations (`continue_loop_on_deny` etc.) / scheduled tasks / cron / monitors | — | **missing** | — | — | D:migrate-v1/ (`continue_loop_on_deny` ignored) | (none found in `core/src`) | E3,E5 | |
| A-ADV-07 | GitHub agent (comment `/opencode` / `/oc` in issues/PRs → runner executes) | root `github/action.yml` | **gated / v1-only**: action installs from `opencode.ai/install` and runs `opencode github run`, a command absent from the v2 CLI | — | GitHub App/OIDC (`api.opencode.ai`) | (not in v2 docs) | `root:github/action.yml:44-71`; `root:github/index.ts:235` | E1,E2 | v2 CLI has no `github`/`pr` commands (E1). |
| A-ADV-08 | Read-only exploration / planning before implementation | `plan`, `explore` agents | supported | 24: explore may run shell | — | D:agents/ | see A-AGT-02/04 | E2,E3 | LUNA plan workflow cancelled (product decision). |

### WS — workspace / project / worktree / remote / multi-device / credentials / data ownership

| ID | capability | native surface | v2.0.22 | later | gates / prereqs | docs | source | evidence | notes |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| A-WS-01 | Shared per-user background service owning sessions, config, integrations, permissions, tool execution; auto-start/replace on version mismatch; `--standalone` private server; `service set disabled true` | CLI root, `service *` | supported | 24: service bind retries, shutdown-before-restart, protocol-only mismatch keeps service (`515b3022ac1`, `25c54cbdf75`, `3e77d5c170f`) | — | D:cli/, D:troubleshooting/ | `cli/src/commands/handlers/default.ts:35-49`; `cli/src/services/service-config.ts:24,83-88`; `cli/src/services/server-connection.ts` | E1,E2,E3 | Registry `~/.local/state/opencode/service.json`; private config `~/.config/opencode/service.json`. |
| A-WS-02 | Remote attach to any server (`--server <url>`, password via `OPENCODE_PASSWORD`); `serve --hostname --port --cors`; `serve --stdio` | CLI | supported | — | network reachability; password | D:cli/web/ | `cli/src/commands/commands.ts:15-24,537-551`; `cli/src/env.ts:10-13` | E1,E2,E3 | Replaces v1 `attach`. `--stdio` undocumented. |
| A-WS-03 | Device pairing: one-time links (5 min) + QR; browser cookie sessions 30 days; app scans QR | CLI `pair [--url]`; HTTP `server.pair`, `server.connect` (`/auth/connect/:code`); TUI `/pair` (`/web`) | supported | head: QR includes every pairing address (`4a0f273f`) | shared service enabled | D:cli/web/, D:cli/commands/ | `server/src/pairing.ts:10-27`; `server/src/auth.ts:16,42-57`; `protocol/src/groups/server.ts:36-58` | E1,E2,E3 | OpenCode-only multi-device flow. |
| A-WS-04 | Projects (list, update incl. worktree startup command) and Locations (directory-scoped service maps, reload) | HTTP `project.list/update`, `location.get/reload` | supported | — | — | D:api | `protocol/src/groups/project.ts:11-29`; `protocol/src/groups/location.ts:36-51`; `core/src/project.ts` | E2,E3 | Catalog, credentials and MCP are location-scoped. |
| A-WS-05 | Worktrees: create/list/remove/refresh; parent dir `worktree.directory`; pluggable strategies (git default); startup script with `OPENCODE_WORKTREE_BASE/PATH` | HTTP `worktree.*`; config `worktree`; TUI `/worktrees`; plugin `ctx.worktree` | supported | — | git (or strategy plugin) | D:config/ (Worktrees), D:api | `protocol/src/groups/worktree.ts:22-70`; `core/src/worktree.ts:93-97,230-252`; `core/src/worktree/strategies.ts:27-62` | E2,E3 | |
| A-WS-06 | VCS info: branch, base, status, branch list, diff; git + Mercurial (hg) plugins | HTTP `vcs.*` (6) | supported | — | git/hg | D:api | `protocol/src/groups/vcs.ts:25-83`; `core/src/plugin/vcs/{git,hg}.ts` | E2,E3 | hg support undocumented. |
| A-WS-07 | Remote workspaces (provider drivers provision/connect/destroy; Location `workspaceID`; remote sandbox execution) | internal only | **unavailable** (driver infra, no built-in provider, no public API/plugin domain) | — | — | — | `core/src/workspace.ts:1-60`; `core/src/workspace/driver.ts`; `schema/src/location.ts:11-29`; `server/src/workerd.ts:29` | E2,E5 | Likely groundwork for hosted/cloud runs; not user-facing in v2.0.22. |
| A-WS-08 | Filesystem API (read, list, find, experimental write) | HTTP `fs.*` | supported | — | — | D:api | `protocol/src/groups/fs.ts:32-76` | E2,E3 | |
| A-WS-09 | PTY terminals and persistent PTYs (survive service restart via handoff; read/snapshot/connect tokens) | HTTP `pty.*` (7), `persistentPty.*` (11, experimental); TUI `/terminal`; desktop Terminal | supported | 24: PTY first-output and handoff fixes; TUI gated on server persistent-PTY support | — | D:api | `protocol/src/groups/pty.ts:23-120`; `protocol/src/groups/persistent-pty.ts:23-103`; `core/src/persistent-pty/*`; `cli/src/server-process.ts:46-47` | E2,E3 | Plugins may read terminals (`ctx.experimental.terminal.read`). |
| A-WS-10 | File watching (location watcher; ignore globs); disable via `OPENCODE_DISABLE_FILEWATCHER`/`OPENCODE_FILEWATCHER_DISABLE` | config `watcher.ignore` | supported | — | — | D:config/ | `core/src/filesystem/watcher.ts`; `core/src/config/plugin/location-watcher.ts`; `cli/src/server-process.ts:120` | E2,E3 | |
| A-WS-11 | Credential/data ownership: credentials, sessions, snapshots in server-side data dir/DB; `auth export` prints secrets; `uninstall --keep-config/--keep-data` | CLI | supported | — | — | D:cli/providers/, D:cli/ | `cli/src/commands/commands.ts:75-98,185-200` | E1,E2,E3 | |
| A-WS-12 | ACP additional directories (per-session external-dir grants persisted on session, copied to children) | ACP | supported | head: tighter cwd handling (`f17b15eaa52`, `ed0e7acecdb`) | — | D:cli/acp/ | `cli/src/acp/directories.ts` | E2,E3 | |
| A-WS-13 | SSH remote servers from desktop (installs matching remote server over SSH, askpass) | desktop GUI extension | supported (desktop) | — | SSH access, tar on remote | (not in docs) | `gui-extensions/src/ssh/main.ts`; `cli/src/ssh-askpass.ts:10-26` | E2 | Undocumented. |
| A-WS-14 | WSL integration (Windows desktop) | desktop GUI extension | supported (Windows) | — | Windows | (not in docs) | `gui-extensions/src/wsl/index.ts` (`os: ["windows"]`) | E2 | |

### HOST — hosted / cloud / share / device / browser / updates / compat

| ID | capability | native surface | v2.0.22 | later | gates / prereqs | docs | source | evidence | notes |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| A-HOST-01 | OpenCode Console connection (device login; workspace providers, websearch, remote MCP servers with `auth: console`, policies; refetch ~1/min; SSO-required / signed-out errors) | `/connect` "OpenCode Console account"; internal `opencode` provider plugin (guarded) | supported | 24: release unread Console config responses (`a701129184b`) | Console account; hosted service | D:console/, D:console/workspaces/, D:policies/ | `core/src/plugin/provider/opencode.ts:20-94,172-183,344-345`; `core/src/plugin/internal.ts:271` | E2,E3 | No entitlement claims. |
| A-HOST-02 | Zen / Console inference gateway & Go subscription (`OPENCODE_API_KEY`) | provider `opencode` / `opencode-go` | gated (hosted) | — | paid plan / credits | D:console/models/, D:console/go/, D:providers/ | `ai/src/providers/opencode-zen.ts:20` | E2,E3 | |
| A-HOST-03 | Console workspaces, members, roles, SSO, SCIM, budgets, billing, service accounts, Console API (inference/providers/budgets) | hosted web service | gated (hosted) | head: console docs added (`ba281e7f915`) | Console admin | D:console/* | (hosted; not in CLI source) | E3 | Out of band for local clients except via `/connect`. |
| A-HOST-04 | Hosted web search ($0.01/search) | Console | gated (hosted) | — | admin enables | D:console/websearch/ | `core/src/plugin/provider/opencode.ts:25-27` | E2,E3 | |
| A-HOST-05 | Session sharing / enterprise share service (`share`, `enterprise.url`) | config | unavailable | — | — | D:sharing/ | `schema/src/config.ts:41-50` | E2,E3 | `packages/enterprise` exists but sharing disabled in clients (E2). |
| A-HOST-06 | Updates: `update: disable\|notify\|auto` (global only; default notify); `upgrade [version] --method`; channel-specific update API (`opencode.ai/update/api/<channel>/<artifact>/<dist>`); `OPENCODE_DISABLE_AUTOUPDATE`; TUI `/update`; desktop updater (Beta→Stable migration) | config, CLI, TUI, desktop | supported | 24: Homebrew Core upgrades (`f902a0fae75`) | — | D:config/, D:cli/commands/ | `schema/src/config.ts:38-40`; `cli/src/services/updater.ts:133,183,242,433`; `tui/src/app.tsx:990`; `gui-extensions/src/updater/*` | E1,E2,E3 | Auto-install doesn't restart a running server. |
| A-HOST-07 | Telemetry: only OTLP export when `OTEL_EXPORTER_OTLP_ENDPOINT/HEADERS` set (opt-in); Console requests carry User-Agent | env | supported (opt-in) | — | OTLP endpoint | (undocumented) | `cli/src/index.ts:125-133`; `util/src/observability/otlp.ts`; `core/src/plugin/provider/opencode.ts:172` | E2 | No product analytics SDK found (E5). `username` "used for telemetry identity" per schema description (`schema/src/config.ts:51-53`). |
| A-HOST-08 | Embedded web UI served by the server (password protected; disable via `OPENCODE_DISABLE_EMBEDDED_WEB_UI`) | `serve`, background service | supported | — | — | D:cli/web/, D: (Web) | `cli/src/services/web-ui.ts`; `cli/src/app-assets.ts:12-16`; `desktop/src/main/lifecycle/configure.ts:19` | E2,E3 | |
| A-HOST-09 | Docker images `ghcr.io/anomalyco/opencode:<ver>`; install via curl/brew/npm/bun/pnpm/yarn/vp/AUR; no Windows package managers | distribution | supported | — | — | D: (Intro) | `cli/Dockerfile`; `cli/script/publish*.ts` | E2,E3 | |
| A-HOST-10 | Cloudflare/workerd server build (SDK "Cloudflare" target; FS search/PTY fail until remote sandbox) | `@opencode/sdk` | partial | — | Cloudflare | D:build/sdk/cloudflare/ | `server/src/workerd.ts:29,109` | E2,E3 | |
| A-HOST-11 | v1 compatibility: v1 config normalized; v1 run bridge; v1 plugin dual-support; v1 data migration | config / CLI | supported (bounded) | — | — | D:migrate-v1/, D:build/plugins/migrate-v1/ | `core/src/config/normalize.ts`; `cli/src/run/v1.ts:8-30` | E2,E3 | |
| A-HOST-12 | Simulation / recording modes (`OPENCODE_SIMULATE`, http-recorder) | env | supported (dev) | — | — | (undocumented) | `cli/src/server-process.ts:99`; `http-recorder/`, `simulation/` | E2 | Dev/test tooling. |

### CLIENT — native clients (TUI / mini / desktop / web / IDE / ACP)

| ID | capability | native surface | v2.0.22 | later | gates / prereqs | docs | source | evidence | notes |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| A-CLIENT-01 | Full-screen TUI: slash commands `/sessions` (`/resume`,`/continue`), `/new`, `/clear`, `/open` (`/projects`), `/models`, `/agents`, `/mcps`, `/variants` (`/thinking`,`/effort`), `/connect`, `/settings`, `/status`, `/update`, `/pair` (`/web`), `/restart`, `/reload`, `/debug`, `/themes`, `/help`, `/exit`, `/share` (unavailable), `/rename`, `/timeline`, `/fork`, `/undo`, `/redo`, `/btw`, `/stats`, `/plugins`, `/diff`, `/cd`, `/editor`, `/skills`, `/worktrees`, `/terminal`; command palette Ctrl+P | `opencode` | supported | 24: awaiting-input sessions surfaced first; read ranges shown | terminal | D:cli/tui/, D:cli/keybinds/ | `tui/src/app.tsx:711-1103`; `tui/src/routes/session/index.tsx:901-1020`; `tui/src/component/prompt/index.tsx:272-626`; `tui/src/component/session-frame.tsx:250` | E2,E3 | |
| A-CLIENT-02 | TUI tabs (per cwd/global, horizontal/vertical, status/number indicators; reopen closed tab) | cli.json `tabs.*` | supported | — | — | D:cli/config/, D:cli/tui/ | `cli/src/config/config.ts` | E3 | |
| A-CLIENT-03 | `cli.json` client settings (theme, animations, cursor, mouse, scroll, prompt.editor/paste/image_preview, session.sidebar/scrollbar/thinking/grouping/tps/markdown/new_location/permissions, diffs.*, attention notifications/sounds/sound packs, terminal title/copy, mini.*, keybinds + leader, plugins, debug.*); `OPENCODE_CLI_CONFIG_CONTENT`; auto-migration from v1 `tui.json` | `~/.config/opencode/cli.json` | supported | — | — | D:cli/config/, D:migrate-v1/ | `cli/src/config/config.ts:32`; `cli/src/config/migrate.ts`; `cli/src/config/schema.ts` | E2,E3 | Global-only (no project file). |
| A-CLIENT-04 | Themes (34+ built-ins, system theme from terminal palette, custom JSON themes global/project, hue/state token format) | cli.json `theme`; `/themes` | supported | — | truecolor recommended | D:themes/, D:cli/theme/ | `theme/` package; `tui/src/app.tsx:1054` | E2,E3 | |
| A-CLIENT-05 | Keybinds (leader key, per-command overrides, which-key, mini/diff-viewer scopes) | cli.json `keybinds` | supported | head: unused plugin install keybinds removed | — | D:cli/keybinds/ | `tui/` | E3 | |
| A-CLIENT-06 | `opencode mini` minimal interface (replay, spinners, mono mode) | CLI `mini` | supported | — | — | D:cli/, D:cli/config/ | `cli/src/mini.ts`; `cli/src/mini-host.ts:50,164`; `commands.ts:340-375` | E1,E2,E3 | |
| A-CLIENT-07 | Non-interactive `opencode run` (CI recipe with `--standalone`) | CLI | supported | — | provider key in env | D:cli/commands/ | `cli/src/run/run.ts`; `cli/src/run/noninteractive.ts` | E1,E2,E3 | |
| A-CLIENT-08 | Desktop app (Electron; macOS/Windows/Linux; bundled CLI/sidecar; deep links; onboarding; attachment picker 20 MiB; GUI extensions: browser, review, terminal, usage/context with system prompt & raw messages & export, summary panel with background tasks/MCP/plugins/skills, file artifact viewer, SSH, WSL, updater) | desktop | supported | 24/head: many desktop UX changes (session header running work, steer/queue labels, browser bar redesign, mobile drawers) | — | D: (Desktop), D:attachments/ | `desktop/src/main/*`; `gui-extensions/src/*` | E2,E3 | Most desktop features have no dedicated API beyond the shared server API. |
| A-CLIENT-09 | Web app (served by server; phone layouts; QR scanner) | `pair`/web UI | supported | 24: QR scanner worker fix; head: mobile navigation polish | pairing | D:cli/web/ | `app/`; `cli/src/services/web-ui.ts` | E2,E3 | |
| A-CLIENT-10 | ACP agent for editors (Zed etc.): sessions new/load/resume/fork/list/close/delete, config options model/effort/mode, prompt blocks (text/image/resource/resource_link incl. `zed://`), `available_commands_update`, client-supplied MCP (stdio/http), permissions once/always/reject with diff previews, elicitation-form questions, usage/usage_update, retry + compaction `_meta` markers | CLI `acp` | supported | 24: standard `compaction_update` reporting (`ebad4bc84c2`), permission spec alignment, remote image links; head: `acp --login` terminal auth, tool names + full-file diffs, cwd/cancel/delete tightening | ACP client | D:cli/acp/ | `cli/src/acp/service.ts:150-175`; `cli/src/acp/translate.ts:16-19,207-209,573-580`; `cli/src/acp/turn.ts:344,376` | E1,E2,E3 | Private server per ACP process (not the shared service). |
| A-CLIENT-11 | VS Code extension (open terminal, new terminal, add filepath to terminal) | `sdks/vscode` | supported (thin) | — | VS Code | (not in v2 docs) | `root:sdks/vscode/package.json` (v2.0.22; 3 commands) | E2 | Terminal launcher, not an embedded chat client. |
| A-CLIENT-12 | IDE selection/mention bridge for the TUI (Claude Code IDE-extension port, Zed terminal) | TUI | supported | — | IDE plugin exposing port | D:cli/config/ (`prompt.editor`) | `tui/src/context/editor.ts:117-121`; `tui/src/editor-zed.ts` | E2,E3 | See A-INP-14. |
| A-CLIENT-13 | Clients/SDKs: `@opencode/client` (Promise + Effect, OpenAPI-generated), `@opencode/sdk` (embed server; Cloudflare), `opencode api` raw caller | npm packages, CLI | supported | — | — | D:build/client/, D:build/sdk/, D:cli/commands/ | `client/src/promise/generated/types.ts`; `sdk/`; `cli/src/commands/handlers/api.ts` | E1,E2,E3 | |
| A-CLIENT-14 | Attention: system notifications + sounds (question, permission, error, done, subagent_done) | cli.json `attention.*` | supported (client) | — | — | D:cli/config/ | `tui/src/feature-plugins/system/notifications.ts` | E2,E3 | |
| A-CLIENT-15 | Diagnostics: SIGPROF CPU profile, SIGUSR1 heap snapshot, logs with run/role, `debug paths` | process signals, CLI | supported | — | macOS/Linux | D:troubleshooting/ | `cli/src/cpu-profile.ts`; `cli/src/heap.ts` | E2,E3 | |

## 4. Feature gates, env flags, telemetry, update channels

### 4.1 Experimental config gates (`experimental.*`, E2 `schema/src/config/experimental.ts:8-15`)

| Key | Default | Effect | Docs |
| --- | --- | --- | --- |
| `experimental.portable_shell_scanner` | false (v2.0.22); head: defaults to true on `local`/`dev` channels | Replace tree-sitter shell scanner | D:permissions/ |
| `experimental.subagent_depth` | 1 | Max nested subagent depth | D:tools/, D:migrate-v1/ |
| `experimental.policies` | [] | Hard-deny / provider.use policies | D:policies/ |
| Experimental API routes (`/api/experimental/...`) | — | session stats/import/export/wait/skill/log/instruction entries, config patch, fs write, generate, mcp add/remove/connect/disconnect, integration wellknown, migration status, persistent PTY | D:api |
| Experimental plugin hooks `experimental.ws.*`, `ctx.experimental.terminal` | — | WebSocket frame hooks; terminal read | D:build/plugins/ |

### 4.2 Hidden / undocumented CLI flags (E1/E2)

`--yolo`, `--dangerously-skip-permissions` (hidden aliases of auto-approve, `cli/src/commands/commands.ts:31-35`); `serve --service`, `serve --stdio` (no description, `commands.ts:548-549`); global `--wizard` (documented only in help).

### 4.3 Environment variables (user-relevant; E2, file:line @527f0b93)

| Variable | Effect | Source | Documented? |
| --- | --- | --- | --- |
| `OPENCODE_PASSWORD` (legacy `OPENCODE_SERVER_PASSWORD`) | Server password for `--server` clients / adopted by manual server | `cli/src/env.ts:10-13` | partially (D:cli/web/ uses `service set password`) |
| `OPENCODE_DB`, `OPENCODE_DISABLE_CHANNEL_DB` | DB file override; force `opencode.db` on non-release channels | `cli/src/database-path.ts:6-11` | `OPENCODE_DB` yes |
| `OPENCODE_CONFIG`, `OPENCODE_CONFIG_CONTENT`, `OPENCODE_CONFIG_DIR`, `OPENCODE_DISABLE_PROJECT_CONFIG` / `OPENCODE_CONFIG_PROJECT_DISABLE` | Explicit config file, inline config, global config dir, skip project config + project AGENTS.md | `cli/src/server-process.ts:36,109-114` | only `OPENCODE_DISABLE_PROJECT_CONFIG` (D:instructions/) |
| `OPENCODE_CLI_CONFIG_CONTENT` | Inline cli.json overlay | `cli/src/config/config.ts:32` | yes (D:cli/config/) |
| `OPENCODE_MODELS_URL`, `OPENCODE_MODELS_PATH`, `OPENCODE_DISABLE_MODELS_FETCH` | Model catalog source / offline | `cli/src/server-process.ts:104-106` | no |
| `OPENCODE_FILEWATCHER_DISABLE` / `OPENCODE_DISABLE_FILEWATCHER` | Disable file watcher | `cli/src/server-process.ts:120` | no |
| `OPENCODE_DISABLE_FFF` | Disable fff search backend | `cli/src/server-process.ts:122-124` | no |
| `OPENCODE_GIT_BASH_PATH` | Git Bash path (Windows shell) | `cli/src/server-process.ts:117` | no |
| `OPENCODE_SIMULATE` | Simulation mode | `cli/src/server-process.ts:99` | no |
| `OPENCODE_CLIENT` | Client identity in telemetry/server info (desktop sets `desktop`) | `cli/src/server-process.ts:90`; `desktop/src/main/lifecycle/environment.ts:31` | no |
| `OPENCODE_LOG_LEVEL`, `OPENCODE_PRINT_LOGS` | Log level; standalone stderr logs | `util/src/observability/logging.ts:165`; `cli/src/services/standalone.ts:30` | `OPENCODE_LOG_LEVEL` yes (D:migrate-v1/) |
| `OPENCODE_DISABLE_AUTOUPDATE` | Disable auto update | `cli/src/services/updater.ts:433` | no (config `update` documented) |
| `OPENCODE_DISABLE_EMBEDDED_WEB_UI` | Don't serve web UI (desktop sets it) | `desktop/src/main/lifecycle/configure.ts:19` | no |
| `OPENCODE_TUI_CHANNEL`, `OPENCODE_FAST_BOOT`, `OPENCODE_DRIVE`, `OPENCODE_SHOW_TTFD`, `OPENCODE_DIRECT_TRACE` | TUI channel override / boot / automation / timing / trace | `cli/src/commands/handlers/default.ts:100`; `tui/src/app.tsx:253,357`; `cli/src/mini-host.ts:50,164` | no |
| `OPENCODE_EDITOR_SSE_PORT` (also reads `CLAUDE_CODE_SSE_PORT`), `OPENCODE_ZED_DB` | IDE bridge | `tui/src/context/editor.ts:117`; `tui/src/editor-zed.ts:189` | no |
| `OPENCODE_API_KEY` | Zen/Console key | `ai/src/providers/opencode-zen.ts:20` | indirectly (D:console/*) |
| `OPENCODE_REPO_CLONE_GITHUB_BASE_URL` | Reference-repo clone base | `core/src/repository.ts:166` | no |
| `OPENCODE_GITLAB_AUTH_CLIENT_ID` | GitLab OAuth client | `core/src/plugin/provider/gitlab.ts:26` | no |
| `OPENCODE_SESSION_ID`, `OPENCODE_TERMINAL=1` | **Injected into** shell tool / terminals | `core/src/tool/plugin/shell.ts:213`; `core/src/shell.ts:271` | no |
| `OPENCODE_WORKTREE_BASE`, `OPENCODE_WORKTREE_PATH` | **Injected into** worktree startup script | `core/src/worktree.ts:244-245` | no (app i18n only) |
| `OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_EXPORTER_OTLP_HEADERS` | Opt-in OTLP telemetry | `cli/src/index.ts:127-128` | no |
| `OPENCODE_EXPERIMENTAL_ICON_DISCOVERY`, `OPENCODE_EXPERIMENTAL_FILEWATCHER` | Set by desktop; **no v2 reader found** (dead/legacy) | `desktop/src/main/lifecycle/environment.ts:29-30` | no |
| `HTTP(S)_PROXY`, `NO_PROXY`, `NODE_EXTRA_CA_CERTS` | Proxy / CA (persist via `service set env`) | D:network/ | yes |

Desktop note (E2): the desktop main process merges the user's login-shell environment into its own (`desktop/src/main/lifecycle/environment.ts:20-33`) before spawning the bundled CLI/sidecar — relevant context for workstream C's launchd-PATH question.

### 4.4 Telemetry and update channels

- Telemetry: OTLP only when `OTEL_EXPORTER_OTLP_*` is set (`cli/src/index.ts:125-133`); no analytics SDK found (E5). Config `username` schema text says "used for telemetry identity" (`schema/src/config.ts:51-53`) while docs say it is accepted but not displayed.
- Channels: build-time `OPENCODE_CHANNEL` (`cli/src/version.ts:1-10`); release channels `latest|dev|beta|next|prod` share `opencode.db`, other channels get `opencode-<channel>.db` (`cli/src/database-path.ts:7-11`); update API `https://opencode.ai/update/api/<channel>/<artifact>/<distribution>?current=…` (`cli/src/services/updater.ts:242`); Homebrew formula names `opencode-beta`/`opencode-v2` recognised (`updater.ts:183`); desktop updater offers "Move from Beta to Stable" (`gui-extensions/src/updater/i18n/en.ts`). Installed binary reports `opencode v2.0.22` (E1).

## 5. Version drift (v2.0.22 → v2.0.24 → v2-head)

Commands (read-only, E2): `git -C /Users/iulian/workspace/code/personal/opencode log --oneline 527f0b93..0fd7e282` (127 commits), `… 0fd7e282..e7a34f09` (55), `… e7a34f09..4a0f273f` (31); `git merge-base --is-ancestor` confirms 527f0b93 → 0fd7e282 → e7a34f09 → 4a0f273f is linear. Filtered to feature/behaviour commits; targeted `git diff` on protocol, schema, agent, ACP, CLI command and tool files. API-surface diff 22→24: **one new endpoint `vcs.init`** (`POST /api/vcs/init`, from `41516c78c83`), otherwise annotation-only; 24→head: no protocol change, `schema/src/agent.ts` default-permission change, `schema/src/model.ts` +1 line.

### 5.1 Released v2.0.22 (installed) — baseline for every table above.

### 5.2 v2.0.23 (`0fd7e282`, released, not installed)
- Core: compaction **honours the compaction agent's model** (`cb389aa679b`); provider-blocked compaction explained (`64744ce0340`); formatter config merged across files (`77eceb8c851`); transient MCP connect failures retried (`421a04f5fde`); legacy provider IDs renamed in top-level `model` (`d113a40f55a`); persistent-PTY handoff can't break restart (`5ad07865363`); first PTY output kept (`334f7a3510d`).
- Skills: `disable-model-invocation` frontmatter (`f3a23e52b33`).
- ACP: standard `compaction_update` reporting (`ebad4bc84c2`); permission requests aligned to tool-call spec (`134d0ede4df`); question-can't-show → model continues (`e2a700dc901`); permissions replied elsewhere settle (`ec90ad50d36`); remote image links as references (`4cb56a829e4`).
- AI/providers: native Venice, Cohere, Google Interactions protocol, Bedrock Mantle Anthropic Messages; Gemini/Mistral/Cohere system-part fixes; Anthropic system-update placement.
- Service/client: managed-service bind retries, wait for shutdown before restart, protocol-only mismatch keeps service, startup failures preserved; Homebrew Core upgrades.
- API: `vcs.init`. Desktop/app: running subagents & shells in session header, review controls in panels, app inbox/steer/queue/revert parity with TUI (`2db9c2e1b39`), Git init for non-Git projects.

### 5.3 v2.0.24 (`e7a34f09`, latest release, not installed)
- **`explore` subagent may run shell** (read-only prompt) (`7725ac81ebd`).
- Providers: native Vercel AI Gateway (`8186b7313a1`, `c2d4db698cc`); AI SDK v6 providers installed by default (`fa992020c54`); non-text models excluded from catalog/default (`944959566fb`); Azure completions-shaped models routed to chat; GitLab Duo variants; Vertex Claude effort updates; legacy OpenAI OAuth methods relabelled "Codex" (`d318cf41250`, `48b295aef4d`); ChatGPT sign-in `/models` sync temporarily disabled (`209346a0feb`).
- Permissions: portable bash/PowerShell scanners hardened (`25909277aa3`, `9d387f6b777`).
- MCP OAuth exchange failures shown on callback page; Console unread-config release; subagent text boundaries preserved; Windows instruction-walk fix.
- Clients: TUI surfaces sessions awaiting input first; desktop steer/queue labels and running-work headers; `/btw` per-answer tabs.

### 5.4 v2-head (`4a0f273f`, development, unreleased)
- **Default permissions: no more `external_directory: ask`, managed-dir allowances removed** (`36a38058fb6`) — changes approval volume for any client.
- Portable shell scanner default-on for `local`/`dev` channels (`81b92c043ff`).
- ACP: `opencode acp --login` + `clientCapabilities.auth.terminal` terminal auth (`f17b15eaa52`); tool names + full-file diffs (`f6ae9865493`); cold-location plugin wait (`fb72e25c060`); fork cwd mismatch rejected (`ed0e7acecdb`).
- Tool name/namespace length caps dropped (`c2f87853c6a`); server always reports `opencode` as MCP client name (`b78d10cd716`); v1 thinking block-binding opt-out migrated (`cabb5d035b2`); pairing QR includes every address (`4a0f273f`).
- Docs: Console workspace/providers/API docs added (`ba281e7f915`); Go "Space Bunny" plan documented (`89a8f1a3db4`).
- Still absent at head: LSP runtime (TODOs unchanged), session sharing (`/share` → unavailable), `instructions` config loading (E2 `git grep` @4a0f273f).

## 6. OpenCode-only capabilities

Relative to Codex CLI and Claude Code, these are distinctive (E5 comparison: based on this inventory plus general product knowledge; the Codex/Claude Code parity workstreams should confirm the comparison). Each is native in v2.0.22 unless marked.

1. **Shared multi-client background server** with remote attach (`--server`), device pairing links/QR, password-protected built-in web UI and phone layouts, plus desktop SSH remote-server bootstrap (A-WS-01..03, A-WS-13).
2. **Durable inbox with steer vs queue delivery** and an API to list, cancel and re-prioritise pending input. Synthetic-input and per-session environment APIs (A-INP-01/02/07/12).
3. **Mid-session move** of a session to another directory or worktree, available as an agent tool (`opencode_session_move`) (A-SES-07).
4. **Code Mode**: the `execute` tool runs JS that orchestrates catalog tools, and MCP tools are routed through Code Mode by default (A-TOOL-13).
5. **Desktop-attached browser namespace** with CDP automation, Lighthouse, CPU/heap/trace profiling and file transfer (A-TOOL-17).
6. **Native provider compaction** (encrypted OpenAI Responses checkpoints), selectable per provider or model, plus a plugin `compaction` hook that can supply the summary (A-MOD-09).
7. **Session warming** to keep provider prompt caches alive (A-MOD-10).
8. **Plugin hooks on raw provider traffic** (`http.request/response`, WebSocket handshake/frames), `retry` decisions, `permission.evaluate` (for example an LLM safety reviewer), and **plugin RPC** that external clients can call (A-EXT-08/09).
9. **Hard-deny policy layer** in which broader config and the Console org override narrower config (A-PERM-07).
10. **Per-integration multi-account** with switch, export and import of credentials, 30+ provider plugins, local model discovery (Ollama, LM Studio, vLLM) and WebSocket transport (A-MOD-05..08, A-MOD-11).
11. **References**: named external directories and auto-refreshed cached Git repos as attachable context (A-INP-06).
12. **Web-search provider choice** (5 providers plus Console-hosted) with random failover and cooldowns (A-EXT-19).
13. **Forms API**: external clients can create and answer structured questions, and MCP elicitation is mapped onto Forms (A-PERM-08/09).
14. **Persistent PTYs** that survive server restart, exposed over the API (A-WS-09).
15. **`session.background`**: moves running foreground tools to the background (A-TURN-03).
16. **Side questions `/btw`** through transient `session.generate` (A-OUT-06). (Verify against current Claude Code.)
17. **Native ACP agent server** with OpenCode `_meta` extensions for child sessions, retry and compaction (A-CLIENT-10).
18. **Mercurial VCS plugin**, pluggable worktree strategies and worktree startup scripts (A-WS-05/06).
19. **Hosted add-ons**: Zen/Go inference subscriptions, Console workspaces/policies/budgets and hosted web search (A-HOST-01..04; gated, no entitlement claim).
20. **Built-in formatter auto-run** after edits, with 27 definitions (A-EXT-17).
21. **Tool-reliability statistics** (`stats --tools`) (A-SES-10).

## 7. Native-client-only capabilities (no dedicated API surface)

Data for some of these is reachable through the API (noted in parentheses), but the feature itself exists only in a native client.

- TUI: tabs, themes, keybinds/which-key/leader, `cli.json` settings, attention notifications and sound packs, `/timeline`, `/status`, `/debug`, `/editor` ($EDITOR drafting), `/cd`, the command palette, mermaid/LaTeX rendering, the `/diff` viewer UI (data via `vcs.*` and `session.diff`), the `/stats` charts (data via `session.stats`), and TUI plugins (slots, routes, dialogs) (A-CLIENT-01..05, A-EXT-11, A-CLIENT-14).
- TUI IDE bridge: selection and mention context through the Claude Code IDE-extension port or the Zed DB (A-INP-14).
- `opencode mini` presentation options (A-CLIENT-06).
- Desktop GUI extensions: browser pane UI, Review panel, Context/usage panel (system prompt, raw messages, export), summary panel, file artifact viewer, SSH remote servers, WSL, updater (Beta→Stable), debug bar, attachment picker (20 MiB selection) (A-CLIENT-08, A-WS-13/14).
- Pairing QR display (the pairing code itself comes from `server.pair`) (A-WS-03).
- VS Code extension (opens or feeds a terminal running the TUI) (A-CLIENT-11).

## 8. Doc/source disagreements

Docs were retrieved 2026-10-06 at 20:31Z. Docs appear to track the **development line** (v2.0.23/24 and head features documented), and the Intro still links **2.0.6** binaries.

| # | Topic | Docs say | Source says | Classification |
| --- | --- | --- | --- | --- |
| 1 | ACP terminal auth | `clientCapabilities.auth.terminal` → `type: "terminal"`, `args: ["--login"]`; `opencode acp --login` (D:cli/acp/) | Absent in v2.0.22 and v2.0.24 (`acp` has no params, `cli/src/commands/commands.ts:99`); added at head (`4a0f273f:packages/cli/src/commands/commands.ts:102`, `acp/service.ts:112`) | documented-but-absent in releases (head-only) |
| 2 | ACP compaction updates | `session.compaction` client capability → `compaction_update`/`compaction_summary_chunk` | v2.0.22 sends only the `_meta["opencode/compaction"]` marker (`cli/src/acp/translate.ts:19,573-580`); standard updates arrive in v2.0.23 (`ebad4bc84c2`) | documented-but-absent in v2.0.22 |
| 3 | ACP child-session updates | Sent during a prompt, no capability mentioned | Gated on client `_meta["opencode/child-session-updates"] === true` (`cli/src/acp/translate.ts:16`, `agent.ts:118`, `turn.ts:376`); still gated at head (`acp/capabilities.ts:11`) | docs omit a prerequisite |
| 4 | Skill frontmatter | `disable-model-invocation: true` supported (D:skills/) | v2.0.22 reads only `metadata.opencode/autoinvoke` (`core/src/config/plugin/skill-file.ts:44-51`); added in v2.0.23 (`f3a23e52b33`) | documented-but-absent in v2.0.22 |
| 5 | OpenAI login method label | "Codex device code (legacy)" (D:cli/providers/) | v2.0.22 labels are "ChatGPT Pro/Plus (browser)", "ChatGPT Pro/Plus (headless)", "Sign in with ChatGPT" (`core/src/plugin/provider/openai.ts:58,177`; `chatgpt.ts:90`); relabelled in v2.0.24 (`d318cf41250`) | docs ahead of release |
| 6 | Explore agent permissions | Explore denies everything except read/glob/grep/webfetch/websearch (D:permissions/, D:agents/) | Matches v2.0.22 (`core/src/plugin/agent.ts:114-128`); **v2.0.24 also allows `shell`** (`7725ac81ebd`) | docs stale vs latest release |
| 7 | Default `external_directory: ask` | Part of every agent's base policy (D:permissions/) | True in v2.0.22/24; removed at head (`36a38058fb6`, same day) | docs stale vs head |
| 8 | Compaction model | "There is no separate compaction model" (D:compaction/) | v2.0.22 matches; v2.0.23+ honours the `compaction` agent's model (`cb389aa679b`) | docs stale vs latest release |
| 9 | `username` | "accepts this field but does not display it" (D:config/) | Schema: "Username displayed in conversations and used for telemetry identity" (`schema/src/config.ts:51-53`) | schema text vs docs |
| 10 | Subagent tool params | agent, description, prompt, sessionID, background (D:tools/) | Also `model` (`core/src/tool/plugin/subagent.ts:36-39`) | undocumented param |
| 11 | Code Mode `opencode` namespace | Lists `session_rename`, `session_move` (D:tools/) | Also `models` (`core/src/tool/plugin/opencode.ts:135`), plus `list_mcp_resources`/`read_mcp_resource` (`mcp-resource.ts:18,50`) | undocumented tools |
| 12 | MCP elicitation | Not mentioned (D:mcp-servers/) | Form and URL elicitation supported and mapped to Forms (`core/src/mcp/client.ts:165-190`; `mcp/index.ts:91-248`) | undocumented capability |
| 13 | GitHub agent | Not in v2 docs or CLI | Repo `github/action.yml` installs via `opencode.ai/install` and runs `opencode github run` (`root:github/action.yml:61-70`), a command the v2 CLI lacks (E1) | v1-only artifact in the v2 tree |
| 14 | LSP | Docs: accepted but not run (D:migrate-v1/, D:permissions/) | Matches (TODOs at `edit.ts:107`, `write.ts:44`, `file-mutation.ts:131`), but the desktop summary panel still has "Configured LSPs" UI strings (`gui-extensions/src/summary/i18n/en.ts`) | consistent docs; residual UI |
| 15 | Accepted-but-inactive config | `share`, `instructions`, `lsp`, agent `request` (docs acknowledge) | Confirmed: no consumer of `instructions` (`core/src/config/plugin/instruction.ts:37-71` only AGENTS.md); `/share` → `unavailable` | documented-absent (acknowledged) |
| 16 | Download links | Intro links point to `files/bin/2.0.6/…` | Installed and released v2.0.22–24 | stale docs |
| 17 | Undocumented source capabilities | — | Durable turn restart budget (`session/execution/restart.ts:33`); session-level permissions (`schema/src/session.ts:57`); `serve --stdio/--service`; hidden `--yolo`/`--dangerously-skip-permissions`; Mercurial plugin; SSH/WSL desktop extensions; IDE bridge via `CLAUDE_CODE_SSE_PORT`; `OPENCODE_SESSION_ID` injected into shell; OTLP telemetry env; workspace drivers; most env vars in §4.3; `vcs.init` (v2.0.23+) | undocumented |
| 18 | MCP sampling | Not claimed | No `sampling/createMessage` handler (`core/src/mcp/client.ts:165-190` declares elicitation, roots, listChanged only) | absent (not claimed) |
| 19 | Read tool vs attachments (PDF) | Attachments: PDF not sent; Tools: read passes PDF to model | Both documented. The asymmetry is real | note, not a contradiction |

## 9. Unresolved questions

1. Is the hidden `summary` agent (`core/src/plugin/agent.ts:147-154`) invoked anywhere in v2.0.22? No caller found in `core/src` (E5). It may be used by a client or a future feature.
2. Agent and command file discovery scope: the agents docs say "up to the project root" while the config docs say "to the filesystem root". `core/src/config/discovery.ts:33-36` walks `fs.up` without a project stop. Do agent and command files follow the same walk? (Likely yes, not proven.)
3. Does the desktop host load third-party GUI extensions, or only the built-ins? (`desktop/src/main/extension/{archive,manifest,manager}.ts` suggest packaging support; not verified.)
4. Remote workspaces (`core/src/workspace.ts`): which hosted product will register a driver, and when will a public API appear? Nothing user-facing exists in v2.0.22.
5. `opencode serve --stdio` and `--service` semantics are undocumented. Owner: workstream B or C, if LUNA could use stdio transport.
6. Exact browser-namespace operation list (docs list snapshot/click/input/screenshot/Lighthouse; `plugin-browser/src/rpc.ts` lists cpu/heap/files/network/tabs/trace/tunnel). The remaining operations probably live in `gui-extensions/src/browser/*`. Not enumerated here.
7. The models catalog URL in source is `models.opencode.ai/api.json` (`core/src/models-dev.ts:270`) while docs say "models.dev". Is it the same catalog behind a different host? (E5)
8. The docs build appears to come from the dev line. Is there a versioned docs snapshot for v2.0.22? None found in the nav.
9. Do Console-delivered MCP servers with `auth: "console"` appear in `mcp.list` like local ones? Workstream B should check payloads.
10. Was the formatter enabled by default in v1? Relevant to migration expectations (A-EXT-17). Not checked against v1 source.
