# 00 — Coordinator baseline and shared research conventions

Owner: coordinator. Research date: **2026-10-06** (all observations below are fresh, E1, unless marked).

## Repository pins

| Repo | Path | Branch | HEAD | Dirty state at inspection |
| --- | --- | --- | --- | --- |
| LUNA | `/Users/iulian/workspace/code/personal/luna` | `main` | `9b3e9a2e3551d5b07a87847f79f911e3b75546a0` (2026-10-06 22:35 +0300) | Another session's uncommitted UI edits (`crates/ui/Cargo.toml`, `crates/ui/src/shell.rs`, `shell/activity_center.rs`, `shell/spaces.rs`, `shell/usage.rs`, `state.rs`, untracked `crates/ui/examples/sidebar-polish-fixture.rs`) and a running `mbx test -p luna-ui` from that session. Untracked research dirs under `docs/pending-tasks/` (`claude-code-feature-parity/`, `agent-workspace-app-research/`, `orchestrator-v2-and-bb-2026-10-04/`) and modified `docs/pending-tasks/README.md`. **Do not touch.** |
| OpenCode (local checkout) | `/Users/iulian/workspace/code/personal/opencode` | `dev` | `4ac0d9c3d169bbe81d9570013effdda3fe24d36e` (2026-10-06 16:28 UTC) | clean. **This is the v1 line** (`packages/opencode/package.json` = 1.18.34). It is NOT the v2 source. `origin` = fork `novnski/opencode` (444 behind upstream/dev); `upstream` = `anomalyco/opencode`. |
| T3 Code | `/Users/iulian/workspace/code/personal/t3code` | `main` | `4ee6bfd50ef4a089440d5c3662db2298da9cc50e` (2026-10-04) | untracked `docs/research/` |
| BB | `/Users/iulian/workspace/code/personal/bb` | `main` | `a2bbfaadaa91084cde68caa06954c8927f0015a4` (2026-10-04) | clean |

## OpenCode v2 source pins (immutable)

Located via `git ls-remote upstream` (read-only, 2026-10-06); all commit objects already present locally, no fetch performed.

| Label | Commit | Notes |
| --- | --- | --- |
| **v2.0.22 (installed + released)** | `527f0b931d1f9b3ebd34e106c51b31ce5db5b075` | lightweight tag → commit "release: v2.0.22", 2026-10-02 04:22 UTC. Parent `05018b8862`. 76 commits after v2.0.21, 769 after v2.0.0. |
| v2.0.21 | `8a8bd622a3d7dc29ccf30ec17f84e363ed95ed72` | |
| v2.0.23 | `0fd7e2829449b052abf0078666669302923d77af` | released, NOT installed |
| **v2.0.24 (latest release at check)** | `e7a34f09bfd9134dfade5a8ddb843f7030bc9a69` | released, NOT installed |
| v2.0.0 | `63f7ceecbed2d7d9a627518d935dd963b9d4ac9f` | 2026-09-11; diverged from `dev` at `0e2dd4ad15` (2026-06-26) |
| upstream `v2` branch head | `4a0f273fa741047ce694b028ed1418fee406998d` | development line for v2 (ls-remote 2026-10-06); present locally |
| upstream `dev` head | `63be8f95d8a32e957701cdf3fb5c7613742dc202` | v1 line; local dev is behind |

Read-only extracted snapshot of v2.0.22 (temporary, session scratchpad; pin is the commit, not the folder):
`/private/tmp/claude-501/-Users-iulian-workspace-code-personal-luna/53e6332d-a23d-4d6c-9711-a15335a5966b/scratchpad/opencode-v2.0.22/`
(8,559 files). Top-level packages at v2.0.22: `ai app cli client codemode console containers core desktop effect-drizzle-sqlite enterprise function gui-extensions http-recorder httpapi-codegen identity latex merman plugin-browser plugin posts protocol schema script sdk server session-ui simulation stats storybook theme tui ui util web`. Note: v2 has **no `packages/opencode/`**; the root also has `V2_HTTP_API_AUDIT.md` and `specs/`.

How to read other versions without touching the checkout: `git -C /Users/iulian/workspace/code/personal/opencode show <sha>:<path>`, `git -C … grep -n <pat> <sha> -- <path>`, `git -C … diff 527f0b93..e7a34f09 -- <path>`. **Never** checkout/switch/fetch/stash in that repo.

## Installed executables (E1, 2026-10-06)

| Path | Resolves to | `--version` |
| --- | --- | --- |
| `~/.opencode/bin/opencode` | Mach-O arm64, 180,293,072 B, sha256 `af29b0b1b0291dbf2471c66c89d2e055b6afa9c41a950e052fa609c3295e2a75` | `opencode v2.0.22` |
| `~/.opencode/bin/opencode2` | `#!/bin/sh exec "$(dirname "$0")/opencode" "$@"` | `opencode v2.0.22` |
| `~/.opencode/bin/opencode1` | sh wrapper: isolated XDG under `~/.local/share/opencode-v1/`, then `exec /opt/homebrew/bin/opencode` | `1.18.34` |
| `/opt/homebrew/bin/opencode` | `/opt/homebrew/Cellar/opencode/1.18.34/bin/opencode` | `1.18.34` |

Environment-dependent resolution of plain `opencode`:

| Environment | PATH source | Result |
| --- | --- | --- |
| Interactive zsh (`zsh -i`) | `~/.zshrc:5` prepends `~/.opencode/bin` | v2.0.22 |
| Claude Code Bash tool shell (profile-initialized) | inherited, `~/.opencode/bin` first | v2.0.22 |
| Login non-interactive (`env -i … zsh -lc`) | `~/.zprofile` (brew shellenv, `~/.local/bin`); `.zshrc` not read | **v1.18.34** (`/opt/homebrew/bin/opencode`) |
| Minimal `env -i … /bin/sh` with `/opt/homebrew/bin` | | **v1.18.34** |
| launchd GUI (`launchctl getenv PATH` empty → default `/usr/bin:/bin:/usr/sbin:/sbin`) | no opencode on PATH | falls through to LUNA's resolver chain |

The prompt's earlier claim "a non-interactive shell resolved plain `opencode` to v1" is refined: login-non-interactive and minimal shells resolve v1; the profile-initialized tool shell resolves v2.

## LUNA executable resolver (E2, LUNA `9b3e9a2e`)

- `crates/harness/src/opencode/mod.rs:146-156` `resolve_opencode_executable()`: `OPENCODE_EXECUTABLE` if it is an existing file, else `crate::executable::find_on_paths("opencode", opencode_install_paths())`.
- `crates/harness/src/opencode/mod.rs:118-132` install-path list order: `~/.opencode/bin/opencode`, `~/.local/bin/opencode`, `~/.bun/bin/opencode`, `~/.npm-global/bin/opencode`, `/opt/homebrew/bin/opencode`, `/usr/local/bin/opencode`.
- `crates/harness/src/executable.rs:296-330` candidate order: **process PATH → login-shell PATH → install paths → node version manager bins**.
- `crates/harness/src/shell_env.rs:133-134` login-shell PATH probe: zsh/bash try `-l -i -c` first, then `-l -c` fallback.
- Consequence (E5 inference, to verify): a launchd-started LUNA Dev gets no opencode from process PATH; if the `-l -i -c` probe succeeds it picks v2 (`~/.opencode/bin` first via `.zshrc`); if that probe fails/times out and `-l -c` is used, it picks **v1** from `/opt/homebrew/bin` before ever reaching the `~/.opencode/bin` install-path entry. Terminal-launched processes inherit the terminal PATH. The daemon/remote-device environment is a separate question for workstream C.
- `crates/engine/src/provider_updates.rs:33,155-156` uses the same resolver for update checks and special-cases `~/.opencode/bin/opencode`.
- Adapter header (`mod.rs:1-37`) claims: native HTTP+SSE (not ACP); two generations detected at boot via `GET /api/health` (2.x) vs `GET /global/health` (1.x); 2.x verified against **2.0.3**; 1.x against 1.18.31; global SSE `GET /api/event`; 2.x frames rewritten into 1.x shapes by `normalize_v2_frame`; 2.x turn = `POST /api/session/{id}/prompt`.

Running processes at inspection: `LUNA Dev.app` pid 95440 (started 22:34:50, primary bundle); no `opencode serve` child running. Another session is running `mbx test -p luna-ui`. Do not start/stop/refresh any app.

## Official documentation

Root: https://opencode.ai/v2/docs/ — retrieval and navigation owned by workstream A (record retrieval timestamps per page).

## Shared conventions for all workstreams

**Status vocabulary** (per dimension): `supported`, `partial`, `missing`, `unverified`, `gated`, `unavailable`, `excluded-by-product-decision`, `n/a` (native-client-only or not applicable). Always give a reason.

**Four dimensions:** Execution (does native v2 do it), Data fidelity (does it survive normalization/storage intact), Product connection (can a LUNA user invoke/observe/answer/stop it), Verification (real production path exercised with retained evidence).

**Evidence levels:**
- `E1 fresh-runtime` — observed in this research via read-only command (version/help/metadata).
- `E2 pinned-source` — read at a pinned commit, with `file:line`.
- `E3 official-docs` — official docs page, URL + retrieval date.
- `E4 historical` — earlier LUNA QA/doc/parity report, not re-run (cite path + date).
- `E5 inference` — reasoned conclusion not directly observed.

**Version tags:** `v2.0.22` (installed/released), `v2.0.24` (latest release, not installed), `v2-head` (`4a0f273f`), `v1.18.34` (Homebrew).

**Category codes** (tag every finding; coordinator assigns final feature IDs):
`SES` sessions/persistence · `INP` inputs/attachments/queue/draft · `TURN` steering/interrupt/cancel/completion/retry/stall/recovery · `OUT` text/reasoning/structured/tool-results/stdout/rich/images/diffs/full-output · `TOOL` native tool families · `MOD` models/providers/accounts/variants/usage/cost/context/compaction/effective-config · `PERM` permissions/sandbox/approvals/questions/elicitation/stale/child-inheritance · `EXT` MCP/plugins/skills/instructions/memory/hooks/discovery/LUNA-callable-tools · `AGT` native subagents/tasks/teams/parent-child · `ADV` review/goals/continuations/schedules/monitors/background · `WS` workspace/project/worktree/remote/multi-device/credentials/data-ownership · `HOST` hosted/cloud/share/device/browser/updates/compat · `CLIENT` native-client-only (TUI/desktop/web/IDE) · `LIFE` transport/lifecycle.

**Product decisions to respect:** dedicated Plan/Build UI and replacement `/plan` workflow are cancelled — inventory native plan agents/modes and imported records, but classify any conflicting proposed LUNA surface as `excluded-by-product-decision`. Reuse shared LUNA surfaces (composer, queue, output viewer, approval/form wizard, child-chat system); do not propose duplicates. Native subagents ≠ LUNA-created child chats.

**Hard limits for every agent:** research only. No code/runtime/UI changes, no tests, no builds (`cargo`/`scripts/cargo.sh`/`mbx`), no app launch/refresh, no model calls/prompts, no installs/upgrades, no credential/config changes, no worktrees, no git state changes in any repo (no checkout/fetch/stash/commit). `--version`/`--help`, `git show/grep/log/diff/ls-remote`, file reads and official-docs fetches are allowed. Write only to your own notes file.
