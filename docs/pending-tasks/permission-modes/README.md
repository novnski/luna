# Permission modes: Auto and Full for every provider and tool call

Ticket: **PM-01** (proposed in [new-task.json](new-task.json)). Ticket wording
review: [ticket-review.md](ticket-review.md). Baseline: LUNA `3c959fb3`,
Codex source `ceea67163f`, T3 Code `477282263a`, BB `a2bbfaad`, Zeron
`40091d46`, OpenCode `4ac0d9c3d16`, Claude Code 2.1.293 (installed binary).

## Product rule

LUNA users choose between two modes. Ask exists in code only as the legacy
`PermissionChoiceState::LegacyAsk` state for old chats
(`crates/ui/src/pickers.rs:332`); it is not a user mode.

- **Full access:** no approval questions, ever. Every tool call (shell, file
  edits, network, MCP and app tools, computer use) runs without a human prompt,
  inside the existing machine, parent and environment ceilings.
- **Auto ("Approve for me"):** the provider harness's own reviewer decides, as
  it already does for shell commands and file edits. LUNA never turns an Auto
  decision into a human approval question.

The user, verbatim: "if it's set to auto the harness itself should decide if
the MCP call is safe to call", "if it's on full access, I should not get any
questions", "I don't get why we have to approve when we are set to auto".

A *question* here is a permission approval. Content requests — Claude's
`AskUserQuestion`, Codex `item/tool/requestUserInput`, an MCP form that asks
for values, a URL sign-in — still reach the user in every mode, because no
reviewer can answer them for the user (confirmed by the user; see
[decisions](#decisions-from-the-user)).

## Evidence: how the harnesses and other apps behave

All sources were read on disk, never run. "Escalation" means the reviewer
handed the decision back to a person instead of deciding.

### The harnesses themselves

| Harness | Auto mechanism | Reviewer denies | Reviewer errors or times out | Reviewer escalates to a person | Full |
| --- | --- | --- | --- | --- | --- |
| Codex (commands, files, network, extra permissions) | `approvalPolicy: on-request` + `approvalsReviewer: auto_review`. Hooks, then the guardian, then the user (`core/src/tools/approvals.rs:479-525`, precedence comment `:505`; `request_reviewer_approval` `:554`). Guardian routing: `ext/guardian-reviewer/src/routing.rs:54-63,65-140`. | `Denied{rationale}`; the action is skipped and the model receives the rationale (`ext/guardian-reviewer/src/completion.rs` assessment branch; `core/src/tools/approvals.rs:440-475`). | Timeout → `TimedOut`, a rejection with timeout instructions (`completion.rs:85-97`, `approvals.rs:466`). Session, parse and prompt errors → denied "Automatic approval review failed" (fail closed, `completion.rs:110-139`). Cancel → `Abort` (`:99-108`). | Only when the guardian returns `None`: input budget exceeded while review is not required (`completion.rs:70-82`) or an extension contributor answers `AskUser` (`routing.rs:121-130`). Then the ordinary user request is sent to the client. | `approvalPolicy: never` + danger-full-access: no requests reach the client. |
| Codex (MCP and app tool calls) | Per-server `default_tools_approval_mode` / per-tool `approval_mode`. `approve` skips review entirely, in every mode (`codex-mcp/src/mcp/mod.rs:91-98`, `core/src/mcp_tool_call.rs:2485-2497`). `auto`/`prompt`/`writes` go to the reviewer chosen by `mcp_approvals_reviewer_from_layers` (`core/src/connectors.rs:514-551`, called at `mcp_tool_call.rs:1504`): the thread's reviewer, unless a `codex_apps` connector's own config says `approvals_reviewer = "user"`. | Tool call skipped with the rejection (`mcp_tool_call.rs:311-321`). | Timeout skipped with timeout instructions (`:323-336`); abort skipped (`:338-349`). | Guardian `None` (as above) or a connector configured for user review → an empty-schema elicitation `_meta.codex_approval_kind: "mcp_tool_call"` (`mcp_tool_call.rs:1593-1700`; `request_user_input` fallback only when `features.tool_call_mcp_elicitation` is off, `:1707`, default on at `features/src/lib.rs:1752`). Client `decline` → denied "user rejected MCP tool call"; `cancel` → abort (`:2131-2168`). | Auto-approved (`mcp/mod.rs:100-109`); `request_mcp_tool_user_approval` refuses to ask under `never` (`mcp_tool_call.rs:1620`). |
| Codex (server-initiated MCP elicitations) | Elicitation reviewer when configured (`codex-mcp/src/elicitation.rs:460-468`), else the user. | — | — | Sent to the client. | Message-only forms auto-accepted with `{}` (`elicitation.rs:420`, `can_auto_accept_elicitation` `:563-577`); forms with fields declined under `never` unless a full-access-form feature is enabled (`:428-458`, `:552-559`). |
| Claude Code 2.1.293 | `--permission-mode auto`: the auto-mode classifier reviews each call. The SDK control schema documents `decision_reason_type` as the "structured discriminator for why auto-mode escalated", meant to let hosts "make policy (e.g. auto-deny safetyCheck)"; values `rule, mode, subcommandResults, permissionPromptTool, hook, asyncAgent, sandboxOverride, workingDir, safetyCheck, classifier, other`; `classifier_approvable: false` = "at least one safety check requires manual approval". | Classifier block → `deny` returned to the model, counted toward a denial limit (binary string "Auto mode classifier blocked action"). | Classifier unavailable → "denying with retry guidance (fail closed)"; safeguard refusal → deny. | Safety checks, ask rules, paths outside the workspace, "transcript too long" ("falling back to normal permission handling") and the consecutive/total denial limit → a host `can_use_tool` request. With no prompt available (`shouldAvoidPermissionPrompts`, headless) Claude instead aborts ("Agent aborted: … in headless mode") or denies. | `bypassPermissions`: allowed. |
| OpenCode | No reviewer. MCP tools ask under their `{server}_{tool}` permission key (`packages/opencode/src/session/tools.ts:408`). | — | — | — | `--auto` approves `once` every request not explicitly denied (`cli/cmd/run.ts:242-246,801-809`). Without it, headless `run` auto-rejects with a visible "permission requested: …; auto-rejecting" (`run.ts:810-819`). |

### Other apps driving those harnesses

| App | Codex | Claude Code | OpenCode | Escalation in Auto |
| --- | --- | --- | --- | --- |
| T3 Code | `auto` → `on-request` + `auto_review`; `full-access` → `never` + `dangerFullAccess` (`apps/server/src/orchestration-v2/Adapters/CodexAdapterV2.ts:661-698`). Every request that still arrives — command (`:5049`), file, permissions, MCP elicitation (`:5234`; unsupported shapes declined `:5236-5245`) — is shown to the user, with no mode gate. | `auto` → `permissionMode: "auto"`; `full-access` → `bypassPermissions` + `allowDangerouslySkipPermissions` (`ClaudeAdapterV2.ts:1594-1640,1684`). The approval callback is not required for auto (`:1665-1669`), so `canUseTool` returns **allow** for every escalation (`:7364-7370`). | `auto` is not `full-access`, so every rule asks the user (`packages/provider-opencode/src/server/adapter.ts:618-660`). | Codex: user. Claude: **silently allowed**, including calls Claude marked as needing manual approval. |
| BB | `auto` → `on-request` + `auto_review`; `full` → `never` + danger-full-access (`plugins/provider-codex/src/session-params.ts:168-182,502-526`). MCP elicitations are not handled: `METHOD_NOT_FOUND`, which Codex treats as no approval (`bridge/bridge.ts:855-861`). | `auto` → `auto`, `full` → `bypassPermissions` (`plugins/provider-claude-code/src/interactive-contract.ts:32-43`). Policy carries `permissionEscalation: "ask" | "deny"` (`packages/domain/src/shared-types.ts:69,502-523`). `ask` forwards escalations to the user; `deny` denies with Claude's own `decisionReason` (`bridge/bridge.ts:2063-2090`). | ACP providers offer only `accept-edits`/`full`, Pi only `full` (`plugins/provider-acp/src/declaration.ts:15`, `plugins/provider-pi/src/declaration.ts:24`): no Auto without a native reviewer. | `ask` for user-initiated turns, **deny with the provider's reason** whenever no user started the turn (`apps/server/src/services/threads/thread-runtime-config.ts:109-117`). |
| Zeron | No Auto: `approvalPolicy` is always `never` (`crates/harness/src/codex/mod.rs:1072-1079`); stray approvals accepted; unknown requests including MCP elicitations get `-32601` (`:2055-2110`). | No Auto: every `can_use_tool` allowed (`crates/harness/src/claude/mod.rs:1326`), `bypassPermissions` when auto-approving (`:242-252`). | — | n/a: Full only. |

### What the evidence decides

- **Auto escalations:** the user rule removes the human branch, so the choice
  is allow or deny. Every source that has no person to ask fails closed:
  Codex's reviewer on error or timeout, Claude in headless mode, OpenCode's
  headless `run`, BB whenever no user started the turn (with the provider's
  reason). T3's allow is the only allow, and it ignores Claude's own
  "requires manual approval" signal. **LUNA denies, with the provider's
  specific reason visible.**
- **Auto denials, errors and timeouts by the reviewer itself:** the harness
  already fails closed and tells the model. LUNA does not override them; it
  makes Codex's `item/autoApprovalReview/completed` denials and timeouts
  visible (LUNA dropped them before).
- **Full:** every app maps Full to the harness's no-approval mode; stray
  requests are allowed once (Zeron, OpenCode `--auto`, Codex's own
  message-only auto-accept). **LUNA allows each once and saves nothing.**
- **No reviewer, no Auto:** BB and LUNA's engine both refuse Auto for OpenCode;
  T3's OpenCode "auto" is human approval under another name.

## LUNA before this change

Live QA at `3c959fb3` (orchestrator, disposable fixture only): Codex and Claude
Full each ran one fixture write with no prompt. Codex Auto and Ask never asked
for Executor calls, because the user's own `~/.codex/config.toml` sets
`[mcp_servers.executor] default_tools_approval_mode = "approve"` (Codex skips
review for that server, `mcp/mod.rs:96`); Codex Ask even wrote without a
prompt for that reason. Claude legacy Ask asked Deny/Allow once/Allow for
session/Cancel turn for every Executor call. Claude Auto (Opus 5.5) ran a read
with no prompt.

| Gap | Path (before) | Effect |
| --- | --- | --- |
| G1 | Codex `approvals::handle` (`crates/harness/src/codex/approvals.rs`, command/file/permissions) auto-answered only `auto_approve`; Auto parked a card. | Auto asked a person whenever Codex's reviewer handed back. |
| G2 | Codex `approvals::handle_tool_approval` answered Full only; Auto parked the "Approve MCP tool" card. | Auto MCP tool approvals became human questions. |
| G3 | Codex computer-use elicitation (`crates/harness/src/codex/mod.rs`, `handle_server_request`) answered Full only. | Auto asked "Approve computer use". |
| G4 | Codex `handle_elicitation` and Claude `controls::elicitation` turned a message-only MCP form (Executor's `require_approval`: `Approve <tool>?`, empty schema) into an Accept/Decline card in every mode. | Full and Auto both asked. |
| G5 | Claude `controls::can_use_tool` (`crates/harness/src/claude/controls.rs`) allowed Full and parked a card for everything else. | Every Auto escalation (safety check, denial limit, ask rule, path outside the workspace, transcript too long), MCP calls included, became a human question. |
| G6 | Codex `item/autoApprovalReview/completed` fell into the unknown-notification arm. | A reviewer denial or timeout was invisible in LUNA. |
| G7 | Codex command/file Full keyed on the legacy `auto_approve` bit only. | A Full policy without the bit would have asked. |
| G8 | Copy: picker "it may still ask you" (`crates/ui/src/pickers.rs:318`); engine limitation "Escalations returned by native auto review still require the user." and mapping "… + user escalation" (`crates/engine/src/permissions.rs:42,223`); `PermissionEscalation::Ask` for Auto (`:297-301`). | The product told users Auto asks. |

Not gaps: OpenCode Full already allows every rule except configured denies and
answers stray asks `once` (`crates/harness/src/opencode/permissions.rs:237-252,1336-1347`).
OpenCode Auto never runs: the engine has no Auto capability for it
(`crates/engine/src/permissions.rs:52-73`), resolves it to Ask, and refuses the
run as a hidden Ask (`crates/engine/src/sessions.rs:2780`,
`crates/engine/tests/e2e.rs:2295`). LUNA's own cross-task cards ask only in
legacy Ask or for an explicit ceiling increase (a child above its parent's
access, an automation outside the visible projects;
`crates/engine/src/sessions.rs:1995-2001`), which is a grant beyond the
ceiling, not a tool approval.

## Design

The harness settles every request that reaches LUNA by the engine-resolved
`PermissionPolicy.effective_mode`. Request validation (live thread/turn,
duplicate IDs, size, stale routing) still runs first and still declines bad
requests in every mode.

| Request (provider) | Full | Auto | Legacy Ask |
| --- | --- | --- | --- |
| Command / terminal input (Codex) | `{"decision":"accept"}` once; never `acceptForSession` or a policy amendment | `{"decision":"decline"}` + visible reason | Card |
| File change (Codex) | `accept` once | `decline` + reason | Card |
| Additional permissions (Codex) | Exactly the requested profile, `scope: "turn"` | `{"permissions":{},"scope":"turn"}` + reason | Card |
| MCP tool approval, `codex_approval_kind: mcp_tool_call` (Codex) | `{"action":"accept","content":null}`, no `_meta.persist` | `{"action":"decline"}` + reason | Card |
| Computer use (Codex) | `accept` with the one-request choice | `decline` + reason | Card |
| Message-only MCP form, a server's own approval (Codex, Claude) | `{"action":"accept","content":{}}` (Codex's own full-access rule) | `{"action":"decline"}` + reason | MCP card |
| MCP form asking for values, URL sign-in (Codex, Claude) | User | User | User |
| `can_use_tool` (Claude) | `allow` once, no `updatedPermissions`, no user classification | `deny` with Claude's `decision_reason_type`/`decision_reason`/safety-check/path/ask-rule reason; no `decisionClassification`, no `interrupt` | Card |
| `AskUserQuestion`, `request_user_input` | User | User | User |
| Reviewer outcome `item/autoApprovalReview/completed` (Codex) | n/a | `denied`/`timedOut` → visible error with the action and rationale; `approved`/`aborted` → nothing | n/a |
| OpenCode `permission.asked` | `once` (unchanged) | Never reached (engine refuses OpenCode Auto) | Card (unchanged) |

Why decline rather than cancel for Codex: `decline` maps to a denial the agent
works around, `cancel` aborts the call (`mcp_tool_call.rs:2164-2165`). Why
Claude's deny has no classification: it is not a user decision, and an
`interrupt` would stop the turn the reviewer let continue.

The visible reason is an `AgentEvent::Error` transcript part (the same channel
the Claude mode check and OpenCode reply failures use). Codex Auto declines:
"Auto access declined {subject}. It needed a person's approval, and Auto never
asks, so it did not run. Switch this chat to Full access to allow it." Codex
reviewer outcomes: "Codex's automatic reviewer denied {subject}: {rationale}.
It did not run." Claude escalations carry the reason in the deny message, which
Claude records as the call's error result on its tool card and gives the model;
a declined Claude server approval form also adds the error part.

The user's saved provider configuration is respected and never rewritten:
Codex `default_tools_approval_mode = "approve"` keeps skipping review (Codex
decides that before LUNA is involved); LUNA never sends `acceptForSession`,
execpolicy/network amendments, `_meta.persist`, Claude `updatedPermissions` or
OpenCode `always` in Full or Auto. To let an escalated action run, the user
switches the chat to Full access or changes that provider's own setting.

### Changed files

Owned (`harness/permissions`):

- `crates/harness/src/codex/approvals.rs` — mode routing for command, file,
  permissions and MCP tool approvals; `decline_without_review`,
  `auto_review_outcome`.
- `crates/harness/src/codex/mod.rs` — `ServerRequestContext.events`,
  computer-use Auto, message-only form Full/Auto, the
  `item/autoApprovalReview/completed` arm, module docs.
- `crates/harness/src/codex/elicitation.rs` — `message_only_form`.
- `crates/harness/src/claude/controls.rs` — Auto escalation denial,
  message-only form Full/Auto, `with_events`/`notify`.
- `crates/harness/src/claude/mod.rs` (wiring + docs),
  `crates/harness/src/claude/profile.rs` (comment).
- Tests: `crates/harness/tests/permission_modes.rs`,
  `crates/harness/tests/fixtures/permission-modes.py`,
  `crates/harness/tests/claude_permissions.rs`,
  `crates/harness/tests/fixtures/claude-permissions-peer.py`.

Shared hunk: `crates/ui/src/pickers.rs:318` Auto description.

Engine hunk, applied after CORE-06 released `crates/engine/src/permissions.rs`
(no engine, UI, iOS or edge code or test reads these strings or the
`escalation` field):

```diff
@@ provider_capabilities, Codex known_limitations
+                "Auto never asks: a request Codex's automatic review hands back to a person is declined and its reason shown; Full access runs it."
+                    .into(),
@@ provider_capabilities, ClaudeCode known_limitations (line 42)
-                "Escalations returned by native auto review still require the user.".into(),
+                "Auto never asks: a call Claude's auto review escalates instead of deciding is denied with Claude's reason; Full access runs it.".into(),
@@ mapping (line 223)
-            "auto + restricted workspace + named command/web tools + user escalation".into()
+            "auto + restricted workspace + named command/web tools + escalations denied".into()
@@ resolve_run_policy (lines 297-301)
         PermissionMode::Auto => (
             PermissionScope::Workspace,
             ApprovalReviewer::Automatic,
-            PermissionEscalation::Ask,
+            PermissionEscalation::Deny,
         ),
```

The harness change does not depend on this hunk; it corrects what the engine
reports.

## Failure contract

Written before the code. Each credible regression has one owner at the
strongest boundary: the real adapter driven by a scripted peer that speaks the
provider's own frame shapes and checks every native response.

| ID | Credible regression | Owner test | Assertion |
| --- | --- | --- | --- |
| F1 | Codex Auto parks a command, file or permissions card when the reviewer hands back | `permission_modes::codex_auto_and_full_settle_every_approval_without_a_person_and_ask_still_asks` (Auto row) | zero approval cards; peer receives `decline` / empty permissions |
| F2 | Codex Auto parks the MCP tool-call approval card | same | zero cards; peer receives `{"action":"decline"}` |
| F3 | Codex Auto parks computer use | same | zero cards; `decline` |
| F4 | Full or Auto turns a server's message-only approval form into a card | same (Codex); `claude_permissions::full_mode_allows_once_but_never_plan_or_interactive_tools` and `::auto_mode_denies_escalations_with_claudes_reason_and_never_asks_for_approval` (Claude) | Full `accept {}`, Auto `decline`, no question |
| F5 | An Auto decline is silent | Codex matrix (six reasons, each naming its subject and "Full access"); Claude auto test (server approval error part) | visible `Error` per decline |
| F6 | Auto silently executes an escalation (T3-style allow, or Auto treated as `auto_approve`) | Codex matrix exact responses; Claude peer checks `behavior: deny` | peer fails the turn otherwise |
| F7 | Full prompts for any kind, or the legacy bit gates Full | Codex matrix (Full row); Claude `full-mode` peer adds an MCP `can_use_tool` | zero cards; exact allow-once responses |
| F8 | Full or Auto saves a broader grant (session rule, `persist`, `always`, `updatedPermissions`) | Codex matrix exact responses (no `_meta`, scope `turn`); Claude peer checks no `updatedPermissions` | exact equality |
| F9 | Legacy Ask stops asking | Codex matrix (Ask row: five cards, two MCP question sets); existing `permission_detail`, `codex::mcp_tool_call_approval_renders_tool_context_and_sends_persist`, `claude_permissions::approval_projects_native_context_and_exact_session_grant` | cards present, Deny maps to the native decline |
| F10 | A Codex reviewer denial or timeout stays invisible, or approvals/aborts spam the transcript | Codex matrix (Auto row) | exactly two reviewer errors with action and rationale |
| F11 | A Claude Auto denial loses Claude's reason, claims a user decision or interrupts the turn | Claude auto test; peer checks each message has the escalation type and reason, no `decisionClassification`, no `interrupt`, no `updatedInput` | peer verification |
| F12 | Content questions blocked in Auto/Full | Codex matrix (data form reaches the user in all three rows); Claude auto test (AskUserQuestion parks) | question present |

Not covered by automated tests: the engine copy hunk (no test reads it), the
picker string, and a live provider actually escalating (native QA below).

## Commands for the orchestrator

```sh
scripts/cargo.sh check --locked -p luna-harness --tests
scripts/cargo.sh test --locked -p luna-harness --test permission_modes
scripts/cargo.sh test --locked -p luna-harness --test claude_permissions
# Siblings that own Ask details and must stay green:
scripts/cargo.sh test --locked -p luna-harness --test permission_detail
scripts/cargo.sh test --locked -p luna-harness --test codex -- approval computer_use provider_permission_event generic_mcp_elicitation
scripts/cargo.sh test --locked -p luna-harness --lib codex::
scripts/cargo.sh check --locked -p luna-ui
# Retained evidence:
LUNA_PERMISSION_MODES_RECEIPT=target/qa/permission-modes/codex-approvals.jsonl \
  scripts/cargo.sh test --locked -p luna-harness --test permission_modes
LUNA_CC03_EVIDENCE_DIR=target/qa/permission-modes/claude \
  scripts/cargo.sh test --locked -p luna-harness --test claude_permissions -- auto_mode full_mode
```

## Native QA (QA app, disposable fixture only)

1. **Codex Full, Claude Full (Sonnet 5.5), OpenCode Full:** one fixture shell
   write and one Executor MCP write each. Expect no card and exactly one effect.
2. **Codex Auto:** an in-workspace fixture write runs. Ask for a clearly
   destructive action outside the workspace on a disposable path. Expect no
   card; either Codex's reviewer allows it (one effect) or the transcript shows
   "Codex's automatic reviewer denied …" and nothing changed. Executor calls
   keep running without review because of the user's `approve` setting.
3. **Claude Auto (Opus 5.5):** repeat the read and the fixture write; then a
   destructive out-of-workspace action. Expect no card ever. A blocked or
   escalated call shows its reason on the tool card ("LUNA Auto access denied
   this … call: …") and has no effect.
4. **Server approval form (optional, needs the user's approval):** only with a
   disposable Executor scope or fixture server that sends a `require_approval`
   form. Full accepts it once; Auto shows "Auto access declined an approval
   request from …" and the tool does not run. Do not change the user's shared
   Executor policies.
5. **Legacy Ask:** an old Ask chat still shows the Deny/Allow once card.
6. **OpenCode Auto:** not selectable; a forced run is refused with the hidden
   Ask reason.

Record per step: provider and model, mode, prompt, card count, transcript
reason text, effect count from the fixture readback.

## Residual risks

- Codex's `request_user_input` fallback for MCP approvals (only when the user
  disables `features.tool_call_mcp_elicitation`) arrives as a content question
  with options Allow / Allow for this session / Cancel and is still shown in
  every mode. Default-on feature; recorded rather than special-cased.
- In Full, a message-only form is accepted even when a server used it as a
  "visit this URL, then accept" fallback for a client without URL support.
  Codex behaves the same. Claude and Codex both advertise URL elicitations, so
  servers should send the URL form instead.
- `item/autoApprovalReview/completed` is marked UNSTABLE in Codex's protocol.
  If the shape changes the visible reason disappears; the decline itself is
  Codex's and is unaffected.
- A Codex request routed to a person by the user's own connector setting
  (`approvals_reviewer = "user"`) is declined in Auto. That honors "Auto never
  asks" over the provider setting; Full still runs it.

## Decisions from the user

- Full asks no approval questions. Content and data forms that need the user's
  actual input stay questions (the behavior above).
- An MCP app's proposed user message (IC A04) and Board/GitHub writebacks keep
  their existing product confirmations; they are outside this permission work.
