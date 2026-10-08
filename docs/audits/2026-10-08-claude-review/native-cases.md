# Coordinator native acceptance cases

## Codex nonblocking-question coverage: failure cases before runner change

The shared S9Q scenario already uses provider-neutral LUNA commands, but the
runner rejects Codex before reaching them. The live driver supports Codex.
Extend only S9Q real-provider execution; keep the established OpenCode/Claude
parity and fake-engine rehearsal provider set unchanged.

1. Codex S9Q with an explicit available model and T3 must exercise the same
   unanswered completion, late answer, amendment, duplicate and steering steps.
2. Codex with any other scenario must refuse before app discovery or mutation.
3. Codex S9Q with T2, a rehearsal, or no explicit model must refuse before app
   discovery; a mock run must never be mislabeled as paid-provider evidence.
4. Existing OpenCode/Claude runs and offline runner checks must remain valid.

## Hands-on question cases

Use the rebuilt QA app and a disposable conversation. Capture screenshots and
AX readback for each transition. Keep the prompt draft separate from answers.

- Select the second option, type Unicode details on page two, navigate to
  another chat and return: both selections and the typed draft must survive.
- Answer later, reopen, then amend an already answered card: preserve drafts;
  a different answer adds one amendment; repeating it adds no user turn.
- Click the question header/input without selecting an option: the hide or
  automatic-answer timer must stop. Waiting beyond its original deadline must
  not answer the held question. An untouched async picker hides without an answer.
- Stop a blocking question, then submit its stale request: it must not resume
  the stopped turn. Keep waiting failure must remain visible until confirmed
  or resolved; never hide an unconfirmed automatic deadline silently.

## Catalog refresh

In the isolated OpenCode profile, open Providers, Projects and title-model
menus to populate their retained lists. Add and then remove a synthetic model
in the mock provider config. Use the actual Refresh model cache button; verify
all retained menus show the new catalog and saved defaults are preserved.

## Native runner follow-up: question modes and completion wakeups

Observed before correction: real Claude S1 used LUNA's nonblocking cards for
three generically worded question prompts. That follows the current runtime
contract, while S1 expects the provider's blocking question path. S9U and
S4X use the same ambiguous wording. Their prompts must explicitly request the
native blocking tool; S9Q must continue to request LUNA ask_user.

Observed S9Q answer waits also consumed their full four-minute deadline even
though both the completed reply and Idle state arrived. The helper listened
only to transcript changes while testing session state too. A final Idle
notification after the final transcript update must wake that same predicate.
Use the existing combined-event waiter without weakening reply, completion,
marker or idle checks. Re-run live S1/S9U and S9Q after this runner change.

The General setting copy must distinguish questions that pause an agent from
nonblocking cards: the latter remain answerable rather than choosing a default.
# S7 runner admission correction

The October 8 live Claude run rejected the typed skill before provider acceptance:
`ListCommands` supplied a skill name without an absolute path, while the runner
sent an empty path. Preserve the rejected receipt and failed run. The rerun must
use the real `SKILL.md` created by this scenario, verify its path exists, and keep
the marker assertion. A truthfully rejected manual compaction has no lifecycle to
wait for; do not spend its lifecycle timeout or claim compaction occurred.

The first Claude S4 restart setup completed its harmless `echo` without asking,
so it could not establish the intended pending-approval precondition. Use the
same Python shell form already verified to request approval in S1; do not
change permissions or label the absent approval as an application defect.
