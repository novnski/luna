# Agent workspace controls acceptance — 2026-10-05

The engine, provider adapters, native UI and real Codex/Claude workflows passed
the implemented scope. New chat defaults are shared by the local composer and
agent-created fresh helpers. Tool discovery identifies provider/model/lineage;
background views preserve Main, and view closure remains independent from
conversation lifecycle.

## Approval audit

| Path | Implemented behavior | Evidence |
| --- | --- | --- |
| Built-in create/read/send/workspace tools | No extra LUNA permission card in Ask, Auto or Full; live-call and exact-target fences remain | Engine MCP integrations and both real providers |
| Child access escalation | An explicit request above the parent's ceiling still needs its existing grant | Child lifecycle and side-chat ceiling integrations |
| Persistent automation | Ask uses its card; Auto/Full execute through the same prepared-plan fences | Session policy review and engine regressions |
| Codex Auto | `on-request` plus native `auto_review`, workspace sandbox and native escalation | Permission wire integration; real Auto parent/child configuration |
| Claude Auto | Native `auto` plus restricted workspace controls and native escalation | Ten adapter scenarios and real Auto parent |
| Codex Full | Valid command/file/MCP/computer-use requests resolve once; additional permission profiles are scoped to the exact turn | Real adapter against deterministic peers, including three newly covered protocol paths |
| Claude Full | Valid tool permission requests resolve once, including a matched ask rule; no permanent rule is created | Adapter integration and real Full parent/helper |
| Full selection | Selecting Full does not require a second confirmation | Serial desktop regression suite |

User questions, generic forms and authentication interactions are not treated
as permission grants. OS-level consent is outside the adapter. Stale, duplicate,
canceled and foreign-turn requests retain their validation and refusal paths.

## Verification

All Cargo commands used `scripts/cargo.sh`, `--locked` and
`CARGO_INCREMENTAL=0`; UI tests ran serially.

| Suite | Result |
| --- | --- |
| Engine library regressions | 435 passed, 3 existing ignored |
| Workspace defaults/layout persistence E2E | 4 passed |
| Cross-task MCP integration | 5 passed |
| Claude child lifecycle integration | 4 passed |
| Claude LUNA tools integration | 1 passed |
| UI library regressions | 1,185 passed |
| Codex adapter integration | 76 passed, 6 existing ignored |
| Claude permission adapter integration | 10 passed |
| Permission-mode wire integration | 1 passed |
| Engine Claude permission + side-chat integrations | 18 passed |
| Clippy: engine, harness, UI libraries | Passed with existing warnings |
| Cargo formatting and whitespace checks | Passed |

The desktop tests exposed a fresh-draft option override regression: selecting
the catalog default could reapply a saved provider option. Explicit default
choices now override saved settings. Existing picker scenarios also verify
that explicit model switches retain that model's independent option choices.

## Native acceptance

The signed `LUNA QA Agent Workspace` app used bundle ID
`com.novnski.luna.qa.agent-workspace`, IPC 59804, and the separate data directory
`target/qa/agent-workspace-controls/native/data`. Its initial build took 91 s.
The script verifies the exact bundle executable, listener PID, signature and
data-directory identity before any RPC. Primary LUNA Dev was quit gracefully;
only one LUNA GUI ran at a time.

1. Codex Full created one Claude helper; Claude Auto created one Codex helper.
   Both omitted model/reasoning/options and received the saved provider choices:
   Codex `gpt-6-luna` / High / Standard and Claude `claude-sonnet-5` / High /
   200k context. The live catalog supplied these values.
2. Both helpers received exactly one user brief and returned
   `WORKSPACE_HELPER_OK`. Each parent kept Main selected, read the actual result
   and completed. Both parent transcripts recorded zero approval cards.
3. After a graceful app restart, settings, helper IDs and background layouts
   persisted. Codex switched to Auto and Claude to Full for resumed turns.
4. Each resumed parent opened its helper in a background split, closed only
   that view, read the preserved conversation, made one persistent portable
   context fork, messaged it and read `WORKSPACE_FORK_OK` from the actual fork.
5. Each parent archived and closed exactly its helper and fork. Both roots
   stayed active and selected. Final readback again found zero approval cards.
6. Native UI inspection verified the provider-default settings and linked
   helper card. A Medium reasoning selection persisted and restoring High
   persisted again. The card's Show in split action opened the saved helper;
   its Close tab control returned to Main while preserving its archive state.

Repeat with a fresh isolated directory under `target/`: set
`AGENT_WORKSPACE_NATIVE_DIR` to that absolute evidence path and
`LUNA_DEV_DATA_DIR` to its `data` child. Use the same QA name, bundle ID and
port above, following the four identity overrides in the
[QA guide](../reference/qa-infrastructure.md). The native runner
is `scripts/qa-agent-workspace-controls.mjs`: `seed`, `start`, `snapshot`,
`verify`, then after a graceful restart `resume`, `snapshot`, `verify`. Wait
until both provider turns are idle before each verification or restart. Never
rerun `seed` against a populated manifest. The runner does not approve prompts.

Local artifacts, transcripts, screenshots, logs and SHA-256 manifests are in
`target/qa/agent-workspace-controls/`. They are retained locally, not committed.
Deterministic engine evidence is under `engine/`; real-provider evidence is
under `native/`. The interactive walkthrough was checked at 320 and 1,024 px,
including split, close, archive and fork states.

## Scope and limits

Workspace tools are for the caller's local project and related conversation
family. Bulk archive uses exact IDs, protects Main/caller and refuses active
work. Across several conversations, archive receipts can report partial
completion; tab closure runs only after all requested archives succeed.

Codex and Claude have verified fresh/resumed LUNA MCP delivery. The existing
OpenCode adapter does not yet deliver these tools, and discovery reports that
limitation. Provider defaults are local to the owning profile; remote-device
defaults are not edited through this settings panel. Portable forks were tested
live; native-context forks remain subject to the existing idle/history checks.
The live scenarios prove app coordination and configuration; native automatic
review decisions for arbitrary external commands are covered by adapter
fixtures, not claimed from these harmless helper prompts.

## Developer app handoff

After hashing the evidence, the exact idle QA app was quit gracefully,
unregistered and removed. Its data and the shared build cache were retained.
Primary `LUNA Dev` was refreshed in 38 s, with its existing workspace restored.
The exact bundle process owned IPC 59777 and deep/strict code-signature
verification succeeded. `target/debug` was 15 GiB; no release cache existed.
