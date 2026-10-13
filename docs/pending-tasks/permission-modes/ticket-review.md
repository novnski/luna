# Ticket review under the Auto/Full model

Contract: [README.md](README.md). Queue read with
`python3 scripts/pending_tasks.py list --full` on 13 October 2026: 86 non-Done
tickets (54 ready, 25 deferred, 1 blocked, 6 in progress). Every non-Done
outcome, criterion and linked specification was searched for approve, deny,
decline, confirm, consent, Ask, Auto and Full wording.

The model every replacement follows:

- **Full:** no approval question for any tool call; each runs once and no rule
  is saved.
- **Auto:** the provider's own reviewer decides; nothing reaches a LUNA
  approval card; anything the reviewer declines or hands back to a person has
  no effect and shows its reason.
- **Approval cards** remain only for legacy Ask chats and for LUNA's own
  ceiling grants (a child asking for more than its parent's access, an
  automation targeting a project outside the visible set).
- **Content requests** (questions, forms asking for values, sign-in URLs, an
  app-proposed user message) still reach the user.

Live tickets (CORE-05, SW-10, ZI-01, CORE-06, IC-0, SW-04): text is proposed
here only; their sections were not edited. Catalog edits are proposals for the
orchestrator; `catalog.json`, `progress.json` and `progress.md` were not
touched.

## Affected tickets

### SW-04 — live: proposal only

**Catalog `guard` criterion.**

- Current: "E05 proves consent, one fixture mutation and independent receipt/readback around restart and unknown delivery; no production send is implied."
- Replacement: "E05 proves the mode-settled fixture mutation — Full performs it once with no prompt; Auto leaves it to the provider's reviewer with no LUNA card, and a declined or escalated call leaves zero effects with its visible reason — and independent receipt/readback around restart and unknown delivery; no production send is implied."

**`luna-only-workflows/verification.md`, E05.**

- Current: "Drive the production bridge against a disposable independently logged service. Review/decline one action, approve another, then break transport after service commit but before acknowledgement and restart. Assert zero effects for decline, one committed approved effect, an unknown/reconciliation state where appropriate, and no second request silently issued."
- Replacement: "Drive the production bridge against a disposable independently logged service. In Full, perform one action with no prompt; in Auto, let the provider's reviewer decide one action and have it decline or escalate another (no LUNA approval card in either mode; a legacy Ask chat may still decline one and approve another). Then break transport after service commit but before acknowledgement and restart. Assert zero effects for every declined or escalated action and a visible reason for each, one committed effect for the performed action, an unknown/reconciliation state where appropriate, and no second request silently issued."

**`luna-only-workflows/implementation.md`, SW-04 step 3.**

- Current: "Exercise a reviewed mutation only against a disposable service fixture or explicitly approved staging target. Show the exact action, obtain the proper grant, perform it once and verify an independent readback."
- Replacement: "Exercise a mutation only against a disposable service fixture or explicitly approved staging target. Show the exact action and run it under the chat's mode — Full performs it once without a prompt; Auto lets the provider's reviewer decide, and a declined or escalated call leaves zero effects with its reason visible — then verify an independent readback."

Live QA note for E05: with the user's saved
`[mcp_servers.executor] default_tools_approval_mode = "approve"`, Codex runs
Executor calls without any review in every mode. A Codex Auto run therefore
proves "no card, one effect", not a reviewer decline; the decline branch needs
an action Codex's reviewer actually reviews, or Claude Auto.

### SW-02 — Done; spec wording clarified (edited)

Its accepted criteria are unchanged. E02 is the reusable permission scenario
later work reruns, so its Auto/Full sentence was made explicit.

- `luna-only-workflows/verification.md` E02. Current: "In Claude Ask, execute a permitted fixture read, decline a write, then allow one exact staged write; expect read data, zero declined mutations and one approved mutation. Repeat on eligible Auto and the selected Codex mode with their actual reviewer/grant semantics." Now: "In a legacy Claude Ask chat, execute a permitted fixture read, decline a write, then allow one exact staged write; expect read data, zero declined mutations and one approved mutation. Repeat on eligible Auto and the selected Codex mode under the permission-modes contract: the provider's own reviewer decides, no LUNA approval card appears, and a write it declines or escalates leaves zero mutations with its reason visible; in Full the staged write runs once without a prompt."
- Same file, risk table row "Restricted-mode permission/config escape". Current proof column: "Selected external tool call through actual CLI with required approval and denied upstream counter". Now: "Selected external tool call through actual CLI with the mode's decision (legacy Ask approval; Auto native reviewer, escalations declined; Full once) and denied upstream counter".

### SW-14 — deferred (docs edited; catalog proposed)

- Catalog `verification`. Current: "E17 consumes IC A01-A07 read/render, confirmed and declined write, stale-session refusal and original ownership evidence." Replacement: "E17 consumes IC A01-A07 read/render, a mode-settled write (Full once; Auto by the provider's reviewer, with declines visible and effect-free), stale-session refusal and original ownership evidence."
- `luna-only-workflows/verification.md` E17. Current: "one confirmed fixture mutation, declined mutation with no effect". Now: "one fixture mutation settled by the chat's mode (Full once without a prompt; Auto by the provider's reviewer), a declined or escalated mutation with no effect and a visible reason".
- `luna-only-workflows/implementation.md` SW-14. Current: "live reads, confirmed/declined writes, stale-session rejection". Now: "live reads, writes settled by the chat's permission mode (Full once, Auto by the provider's reviewer with visible declines; no LUNA approval card), stale-session rejection".

### IC-4 — ready (docs edited; catalog proposed)

MCP App tool calls are tool calls, so they follow the mode. An app-proposed
*user message* (A04) is content and keeps its explicit confirmation (the user's decision).

- Catalog `live`. Current: "A01-A07 real Codex/server read, confirmed/declined mutation and exactly-once explicit message round trips pass; unsupported providers are accurately labeled." Replacement: "A01-A07 real Codex/server read, mode-settled mutation (Full once without a prompt; Auto declined or escalated with no effect and a visible reason) and exactly-once explicit message round trips pass; unsupported providers are accurately labeled."
- `interactive-chat-surfaces/implementation.md` IC-4 step 4. Current: "Implement confirmation, denial, stale-session rejection, cancellation, bounded results and duplicate/uncertain-outcome receipts." Now: "Settle app tool calls by the chat's mode (permission modes: Full runs it once without a prompt; Auto leaves it to the provider's reviewer through the live client or declines it with a visible reason; only legacy Ask confirms), and implement denial, stale-session rejection, cancellation, bounded results and duplicate/uncertain-outcome receipts."
- Same file, Gate. Current: "performs a read-only call, confirms and completes one real mutation, refuses a declined mutation, and accepts one explicit app message exactly once." Now: "performs a read-only call, completes one real mutation in Full with no prompt, leaves an Auto mutation to the provider's reviewer (one it declines or escalates changes nothing and shows why), and accepts one explicit app message exactly once."
- `interactive-chat-surfaces/verification.md` risk row. Current: "Confirmation | Decline still calls the server; repeated requests duplicate mutations/messages; read-only metadata bypasses LUNA access." Now: "Mode settlement | An Auto decline or escalation still calls the server; a Full or Auto call parks a LUNA approval card; repeated requests duplicate mutations/messages; read-only metadata bypasses LUNA access."
- Same file, A02. Current: "allowed mutation changes the server once; declined mutation changes nothing; … confirmations and results." Now: "a Full mutation changes the server once with no prompt; an Auto mutation the provider's reviewer declines or escalates changes nothing and shows its reason; … per-mode decisions and results."
- Same file, live scenario 4. Current: "perform read-only and confirmed/declined mutations". Now: "perform read-only and mode-settled mutations (Full once; Auto by the provider's reviewer)".
- `interactive-chat-surfaces/design.md`. Current: "Declining a confirmation performs no upstream call." Now: "App tool calls follow the chat's permission mode: Full runs them once without a prompt, Auto leaves them to the provider's reviewer, and a call declined in Auto (or in a legacy Ask confirmation) performs no upstream call."

### IOS-04 — ready (docs edited; catalog proposed)

- Catalog `delivery`. Current: "Build generic native picker/questions/approvals with stale/offline/missing capabilities, secret handling and host-side settlement; keep provider binaries and credentials on the host." Replacement: "Build generic native picker/questions, and approvals for legacy Ask chats and LUNA ceiling grants (Auto/Full raise none), with stale/offline/missing capabilities, secret handling and host-side settlement; keep provider binaries and credentials on the host."
- Catalog `verification`. Current: "P06 real allowed/denied/changed/revoked/replayed request and new/missing plugin cases retain native UI and authoritative provider decision evidence." Replacement: "P06 real allowed/denied/changed/revoked/replayed request (legacy Ask chat or ceiling grant), an Auto/Full task with no approval card, and new/missing plugin cases retain native UI and authoritative provider decision evidence."
- `ios-parity/implementation.md` IOS-04. Current: "Map canonical permissions and exact request/revision/decision IDs into native question and approve/deny flows." Now: "Map canonical permissions and exact request/revision/decision IDs into native question flows, and into approve/deny flows for the requests that still reach a person: legacy Ask chats and LUNA ceiling grants. Auto and Full chats never show an approval card; an Auto decline appears as its transcript reason." Gate: "P06 actual allowed/denied provider-facing flows" → "P06 actual allowed/denied provider-facing flows (from a legacy Ask chat or a LUNA ceiling grant) plus an Auto/Full task that shows no approval card".
- `ios-parity/verification.md` P06: adds the failure "an Auto/Full task shows an approval card" and the owner proof "(approvals from a legacy Ask chat or LUNA ceiling grant), an Auto/Full task with no approval card".

### IOS-06 — ready (docs edited; catalog proposed)

- Catalog `verification`. Current: "P08 actual native media/app interactions, restart/scroll/disable and allowed/declined/stale/cross-task tests retain asset/frame/authority hashes and truthful unavailable states." Replacement: "P08 actual native media/app interactions, restart/scroll/disable and mode-settled (Full once, Auto declined with its reason)/stale/cross-task tests retain asset/frame/authority hashes and truthful unavailable states."
- `ios-parity/implementation.md` IOS-06 Gate. Current: "declined/allowed fixture app actions". Now: "mode-settled fixture app actions (Full once; Auto declined with its reason)".

### RM-05 — ready (docs edited; catalog proposed)

- Catalog `interactions`. Current: "Question/approval IDs/revisions/command/grants reject changed/stale/revoked/replayed responses through existing provider boundary." Replacement: "Question/approval IDs/revisions/command/grants reject changed/stale/revoked/replayed responses through existing provider boundary; approvals arise only from legacy Ask chats and LUNA ceiling grants, never from Auto/Full tasks."
- Catalog `verification`. Current: "RP-16-RP-18 prove actual provider request/decision correlation and refusal through the native/SDK contract slice; IOS-04 supplies complete native UI acceptance before RM-07 closes." Replacement: "RP-16-RP-18 prove actual provider request/decision correlation and refusal (approvals from a legacy Ask chat or ceiling grant; an Auto/Full task shows none) through the native/SDK contract slice; IOS-04 supplies complete native UI acceptance before RM-07 closes."
- `t3-remote-mobile/implementation-plan.md` RM-05. Current: "Add approval projection and native approve/deny/action UI; current iOS question controls do not supply approval parity." Now: "Add approval projection and native approve/deny/action UI for the requests that still reach a person — legacy Ask chats and LUNA ceiling grants; Auto and Full never produce one. Current iOS question controls do not supply approval parity." Completion line now names the approval source and adds "plus an Auto/Full task whose phone view shows no approval card".
- `t3-remote-mobile/validation-plan.md` RP-16. Current: "Actual provider requests approval. … permitted approve and deny each produce host readback." Now: "In a legacy Ask chat (or for a LUNA ceiling grant) the actual provider requests approval. … permitted approve and deny each produce host readback. An Auto/Full task produces no approval card on the phone."
- `control-this-mac/implementation-plan.md` (M3, RM-05). Current: "Preserve existing sandbox and provider permission prompts." Now: "Preserve existing sandbox and provider permission behavior: Full and Auto never prompt; only legacy Ask chats and LUNA ceiling grants ask."
- `control-this-mac/feature-parity.md` RC10: appends "Approvals exist only for legacy Ask chats and LUNA ceiling grants; Auto/Full tasks never raise one."

### CORE-08, CORE-09, CORE-10, CORE-11 — ready (docs edited; catalog proposed)

Provider extraction must carry this contract and its E2Es as a conformance
gate, or a packaged adapter silently returns to human approvals.

- CORE-08 catalog `contract`. Current: "Implement mandatory/optional provider conformance profiles with exact run/input/approval/Stop/continuity and unsupported-capability semantics." Replacement: "Implement mandatory/optional provider conformance profiles with exact run/input/approval/Stop/continuity and unsupported-capability semantics; approval conformance is per mode (Full never asks, Auto routes to the provider's reviewer and fails closed visibly, legacy Ask parks)."
- CORE-09 catalog `delivery`. Current: "Package the real adapter; preserve tool/media/child identity, steering, native history, usage, credentials and permissions without core/picker name shortcuts." Replacement: "Package the real adapter; preserve tool/media/child identity, steering, native history, usage, credentials, permissions and the PM-01 per-mode approval settlement (its `permission_modes` E2E) without core/picker name shortcuts."
- CORE-10 catalog `delivery`. Current: "Package the adapter and provider-owned settings/account operations; preserve Ask/Auto/Full ceilings, questions/goals/history/images and native ownership." Replacement: "Package the adapter and provider-owned settings/account operations; preserve Ask/Auto/Full ceilings and the PM-01 settlement (Auto escalations denied with Claude's reason, Full never asks; `claude_permissions` E2E), questions/goals/history/images and native ownership."
- CORE-11 catalog `delivery`. Current: "Package the existing adapter under supervised lifecycle with core-owned admission; retain exact SSE/event/session/permission and recovery behavior." Replacement: "Package the existing adapter under supervised lifecycle with core-owned admission; retain exact SSE/event/session/permission (Full answers stray asks once; no Auto without a native reviewer) and recovery behavior."
- `extensible-core/implementation.md`: CORE-08 failure focus adds "an Auto or Full run parks a human approval card instead of following the permission-modes contract"; CORE-09 "Preserve app-server wire behavior, approvals," now names the per-mode settlement; CORE-10 adds the settlement after "Ask/Auto/Full ceilings"; CORE-11 qualifies "session permission readback".

### Board (PB-4A–PB-4D, deferred) — doc edited, no catalog change

- `devin-project-board/proposal.md` QA step 2. Current: "Exercise a real question/approval, a provider failure, disconnected host, and active child work." Now: "Exercise a real question, an Auto reviewer decline with its visible reason (only legacy Ask chats still raise approval attention), a provider failure, disconnected host, and active child work."

## Reviewed and unchanged

| Ticket | Text | Why it stays |
| --- | --- | --- |
| CORE-05 (live) | "E03/E07 independent clients exercise the same allowed/denied operation" | SDK client authorization of a LUNA operation, not a provider tool approval. |
| CORE-06 (live) | "enable only the exact compatible package with explicit grants" | Installing and enabling an extension is a product consent, not a tool call. |
| CORE-18 | "declined or changed candidates cannot self-activate" | Same: extension activation consent. |
| SW-10 (live), PB-4D | "permission ceilings", "permission inheritance" | Ceilings are unchanged by this contract. |
| AW-03 and `agent-workspace-app-research/luna-opportunities.md` | "a notification dismissal cannot approve/deny a tool action" | Still true for legacy Ask chats and ceiling grants. |
| IOS-05 | "Keep GitHub/external writes on their existing explicit confirmation boundary" | LUNA's own Board writeback, started by the user, not an agent tool call; the user confirmed it keeps its product confirmation. |
| PB-4B | "confirmed writeback receipts" | Same as IOS-05. |
| IOS-10 | "explicit send/decline" of a share-extension payload | A user-initiated share, not a tool approval. |
| IC A04 | "App proposes a message; decline adds nothing; confirm queues one attributed user message" | The message enters the chat as the user's own words: content; the user confirmed it keeps its confirmation. |
| SW-02 implementation step 4 (Done) | "Ask must surface required grants; Auto follows its eligible reviewer contract; unavailable enforcement fails closed with a specific reason." | Consistent with this contract. |
| ZI-01 (live), ZI-05, IC-0 (live), IC-1, IC-2, ART-01, CORE-01/04/07/12/14/16, RM-11, IOS-11–IOS-13 | "denied" cases | File/asset/method access denials, not approvals. |

## New ticket

[new-task.json](new-task.json) admits this implementation as **PM-01** in the
`integration` group, Ready, priority 0, reserving `harness/permissions`,
`engine/permissions` and `ui/pickers`. The engine copy hunk (README, "Engine
hunk") was applied after CORE-06 released `engine/permissions`.

```sh
python3 scripts/pending_tasks.py add --file docs/pending-tasks/permission-modes/new-task.json
python3 scripts/pending_tasks.py snapshot
python3 scripts/pending_tasks.py validate
git diff --check
```
