# Workbench follow-up verification

Baseline `29da9bdd`; evidence root
`target/qa/workbench-followup-2026-10-04/`.

## Findings and behavior

- The unavailable-Main notice came from a layout read racing normal chat
  creation. The UI treated the initial refusal as permanent. It now retries
  quietly with bounded backoff, clears the notice after a successful read,
  and exposes prolonged failures with Retry. Missing/deleted chats remain
  invalid in the engine; nothing recreates a missing task to hide the error.
- A saved blank child was treated as a provider-bound conversation. Initial
  provider choice now waits for its catalog and uses the ordinary command with
  `initialOnly: true`. The engine commits only config while it is pristine:
  no execution, binding or history boundary. A stale initial choice is rejected
  if the task started or acquired queued work. Confirmed switches preserve
  the existing boundary behavior and serialized command payload.
- Native testing reproduced a separate catalog failure: the picker passed
  Claude's account ID when requesting Codex models. The foreign-account query
  correctly failed, while the target's default account returned eight models.
  Provider-scoped account selection and catalog cache tracking now prevent
  that leak and discard/reload stale account results.
- Split directly creates one blank child in a new pane. Child/fork metadata
  strips are removed from the workbench; state is on the tab and parent/source
  navigation remains in its context menu and tooltip.
- Context usage lives beside Goal in each composer's controls row. At narrow
  widths the percentage yields before model information; the ring and details
  card remain. The separate workbench composer footer is removed.

## Automated acceptance

20 engine integration tests pass: `pane_chat` (12), `chat_workbench` (7) and
`task_commands provider_switch` (1). The initial-choice test was written before
the implementation and failed because it added a System message and binding.
The stale-choice guard also failed before its implementation by resetting an
already-started child; both before traces are retained.

```sh
PANE_ENGINE_EVIDENCE_DIR="$PWD/target/qa/workbench-followup-2026-10-04/engine" \
WORKBENCH_EVIDENCE_DIR="$PWD/target/qa/workbench-followup-2026-10-04/engine" \
  scripts/cargo.sh test --locked -p luna-engine --test pane_chat --test chat_workbench
scripts/cargo.sh test --locked -p luna-engine --test task_commands provider_switch
scripts/cargo.sh test --locked -p luna-ui --lib -- --test-threads=1
```

All 1,185 existing UI tests pass. No new unit tests were added. Builds,
Clippy and formatting checks pass. Native GUI fixtures run one at a time:

| Fixture | Passing output | Checked behavior |
| --- | --- | --- |
| Workbench | `ui-attempt-3` | Early Main reads stay quiet; a prolonged refusal stays actionable and clears after the exact chat appears. Direct Split creates one blank child. Provider account scoping, tab status/unread, no metadata rows, layout persistence and all prior drag/resize/Balance scenarios. |
| Composer parity | `composer-attempt-2` | Same control row for Main/fork, in-row context chip, no overlaps at 320px group minimum, independent controls/drafts, dark/light. |
| Fork pane | `side-attempt-1` | Ordinary fork views, no metadata row, drafts, delivery, close/reopen, idle survival and archive. |
| Appearance | `appearance-attempt-1` | 810 theme/surface/accent combinations, including usage text/ring for ordinary, warning, danger, unknown and compaction states. |

The first workbench run found a fixture assertion comparing transient
Completed/Idle state as saved layout. Its corrected comparison preserves every
other field and also verifies focused pane persistence. The composer's wide
baseline grew to 1600px to accommodate the newly added control; its separate
320px minimum-width cases are unchanged.

```sh
scripts/cargo.sh build --locked -p luna-ui --features appshots-fixture \
  --example chat-workbench-fixture --example composer-parity-fixture \
  --example side-chat-pane-fixture --example appearance-audit
target/debug/examples/chat-workbench-fixture ABSOLUTE_EVIDENCE_DIR
target/debug/examples/composer-parity-fixture ABSOLUTE_EVIDENCE_DIR
target/debug/examples/side-chat-pane-fixture ABSOLUTE_EVIDENCE_DIR
target/debug/examples/appearance-audit ABSOLUTE_EVIDENCE_DIR
```

## Native acceptance

The guarded driver is `scripts/qa-workbench-followup.mjs`. Its QA identity is
`LUNA QA Workbench Followup`, bundle `com.novnski.luna.qa.workbench-followup`,
IPC 59806, with isolated data at the evidence root's `native/data`.
`seed-project` creates only a project: Main is created through its first native
composer Send, and the native Split icon creates its child.

The driver verifies PID ownership, bundle signature and data directory before
each operation. It checks the provider choice against the child's own registry,
managed identity, messages and draft, and compares parent data independently.
`checkpoint-restart`/`verify-restart` require different PIDs and unchanged
layout, provider config, lineage, messages, draft content and bindings.

Native acceptance passed against the same preserved child:

- Main was created by native Send and answered through Claude without a stale
  availability notice. One native Split click created exactly one blank child,
  with no chooser, messages or provider binding.
- The initial native provider attempt reproduced the foreign-account failure.
  After the scoped-account fix, the same child switched to Codex directly;
  its draft and lineage were unchanged, its transcript stayed empty and no
  binding was created. The parent remained on Claude.
- Restart replaced PID 50472 with 59576 and preserved the layout, provider
  choice, unsent draft, parent messages and binding state. Native Send then
  returned `FOLLOWUP_CHILD_OK` through the child's first Codex binding,
  generation 1. The parent's original messages and provider were unchanged.
- Native dark/light captures show the compact header and context meters inside
  both composers (10% for Main and 4% for the child). A child opened as its own
  Main also omits the duplicate inline parent row; titlebar navigation remains.

Readbacks: `native/verified-{split,choice,restart,delivery}.json` and
`native/verified-catalog-controls.json`. Visual results:
`native/n12-native-delivery-dark.png`, `native/n15-native-delivery-light.png`
and `native/n10-restarted-choice.png`.

## Handoff and cleanup

The last QA app rebuild took 14 seconds. The primary LUNA Dev refresh took
4 seconds and started PID 74700 on IPC 59777. Its exact bundle executable,
listener, bundle identity and deep/strict code signature were verified;
`primary-handoff.json` records the handoff.

Native evidence was reviewed and hashed before the QA app was quit gracefully,
unregistered and removed. Its data is preserved. Six scoped fixture runtimes
were archived and hashed, checked for open files, and removed. The shared debug
cache remains (63 GiB); 349 GiB of disk space remained at handoff. Cleanup
receipts are in `cleanup/`, and `SHA256SUMS` covers retained evidence and logs.

## Limits

Contrast measurements are scoped readability checks, not a full accessibility
certification. Main workbench restoration preserves saved layouts; LUNA's
existing most-recent-active-chat startup landing policy remains unchanged.
