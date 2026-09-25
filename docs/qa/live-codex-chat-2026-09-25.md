# Live Codex chat QA — 2026-09-25

This run exercised LUNA Dev against the installed Codex app server and GPT-6-Luna (High, Auto) in a local checkout. The starting revision was `f2346981`. The baseline contained 285 `noviulian`-authored commits; this review concentrated on their chat-facing paths. All task mutations below were made in disposable LUNA QA tasks.

## Repeatable live run

Use a signed LUNA Dev build and a Codex account with GPT-6-Luna access. Start with a new local-checkout task and select GPT-6-Luna, High, Auto. Send these prompts in order, inspecting the transcript and composer after each step:

1. `Reply with exactly READY. Do not use tools.` Confirm one user entry, one completed reply, and an empty composer after the delivery receipt settles.
2. `Run the read-only terminal command pwd, then report the path.` Expand the tool group. Confirm reasoning, commentary, command invocation, completion state, and final answer survive reopening the task.
3. Ask for a Markdown heading, a two-column table, a fenced Rust code block, and 90 numbered lines. Scroll away from the bottom, then use **Scroll to bottom**. Confirm the content renders and the button returns to the tail. Type an unsent sentinel, navigate away and back, and confirm the per-task draft survives.
4. `Generate an image of a dog using image generation and show it inline.` Confirm the generated image appears in the assistant turn, opens in the preview, and remains available after a LUNA Dev refresh.
5. `Use request_user_input_async to ask whether I prefer Red or Blue, with those two options. Do not answer it yourself.` Confirm a typed async question appears. Choose Red, confirm the card resolves and the provider responds in a following turn.
6. Ask Codex to run `sleep 5` and reply `FIRST COMPLETE`. While the turn is active, queue `Reply with exactly SECOND COMPLETE`. Confirm the queue changes from held to claimed to empty, with the two assistant replies in order.
7. Ask Codex to run `sleep 20`, then interrupt after the task snapshot reports an active turn. Confirm the interrupt receipt applies, the active turn clears, and the assistant entry is marked aborted.
8. Fork the completed task at the tip with a new title and first prompt. Confirm a distinct provider session, matching native/LUNA title, current reconciliation, and an accepted first prompt. On a disposable fork, edit an earlier user message and then restore its recovery. Confirm the binding generation advances and the original task history is unchanged.
9. Search session history globally for `dog`. Confirm prompt and answer events from the real task appear.

For draft recovery, use an isolated QA task with the local IPC `BeginDraftSubmission` and `SettleDraftSubmission` calls to stage a recoverable copy. Verify that a pending copy cannot be discarded, a matching applied retry removes only the matching recoverable copy, and explicit discard clears a matching saved draft with a higher revision. Reopen the task to confirm a recoverable submission is offered as an explicit action rather than inserted into the input.

## Observed results

| Area | Live readback |
| --- | --- |
| Send, streaming, reasoning, tool call | Codex returned READY; `pwd` produced reasoning, command invocation, completed tool chip, and final text. |
| Markdown and scroll | Heading, table, code, 90 lines, tail follow, manual scroll, and per-task unsent draft worked in the native window. |
| Generated image | Codex image-generation output became a `MessagePart::Image`; the image rendered inline and its native preview opened. It survived refresh. |
| Async question | Codex emitted `asyncQuestion`; a later answer applied, resolved the card, and produced a provider reply. |
| Queue and interrupt | Queued follow-up moved held → claimed → empty with ordered replies. Interrupt applied and left an aborted assistant entry. |
| Fork, edit, recovery | Native fork produced a distinct session with current reconciliation. Earlier-message edit advanced the binding and returned the edited reply; restore advanced it again and reinstated the prior history. |
| Search | Global `dog` search returned nine activity events, including prompt and answer events from the QA task. |
| Draft settlement | Live IPC readback proved pending protection, matching retry supersedence, unrelated-copy retention, explicit discard, and a higher blank-draft revision. |

After the final 51-second LUNA Dev refresh, a new GPT-6-Luna prompt received an applied delivery receipt and a completed `LUNA FINAL READY` assistant reply. The earlier generated image was still present in the reopened transcript.

## Defects found and corrected

- A normal pending send showed a red delivery error before the provider receipt had settled. The composer now waits for the authoritative terminal receipt.
- A rejected submission could be loaded into the input automatically even after a later successful send of the same content. Recovery is explicit; an applied retry supersedes the matching saved copy; discard clears only a matching saved draft and advances its revision.
- Warm idle provider handles were reported as active turns, blocking history forks. Idle handles no longer satisfy the active-turn fence.
- Native Codex forks inherited the source title while LUNA registered a new title, causing `ExternalChange` on the first prompt. The fork sets and verifies the native title before registration.
- An async question answer sent between turns attempted native in-turn steering and was rejected. Idle delivery now starts a follow-up turn. A definitively rejected native steer also removes its unaccepted user echo.
- Pressing `0` while a question card was open could panic before the shortcut range check. The shortcut now evaluates its index only for digits `1` through `9`.
- Several native QA examples no longer compiled after the local-owner IPC signature and queue schema changed. Their engine data-root arguments and queue fields were updated; all UI examples compile with the fixture feature. A Codex fake-server assertion also assumed JSON field order and was corrected; the full adapter suite then passed.
- Composer recovery and sidebar errors now have visible dismissal actions. Pending delivery remains protected from discard while its provider result is unknown.

## Limits observed

- Tool groups expose invocation and completion state, but full stdout is intentionally retained only in the host run journal. `crates/doc/src/parts.rs` and `crates/engine/src/sessions.rs` document this product policy. This run did not change that privacy boundary.
- Native image-generation results render inline. A plain Markdown image link to a local file outside the workspace rendered as a link; this path is separate from the provider image result.
- This live run used Codex. Other provider adapters, remote devices, and multi-device sync were not exercised with real accounts in this run.
- Draft discard and retry were verified through the running local IPC. The new dismissal controls were compiled and checked in source; this run did not obtain a reliable full-size native screenshot of those controls after the app moved into the macOS window thumbnail.

## Verification commands

Run `cargo fmt --all --check`, `cargo check --locked -p luna-ui -p luna-engine -p luna-harness`, `cargo check --locked -p luna-ui --examples --features appshots-fixture`, the focused `luna-sync` draft test, the `luna-engine` task-command suite, the `luna-harness` Codex adapter suite, and `cargo test --locked -p luna-ui --lib -- --test-threads=1`. Finish with `scripts/run-macos-dev.sh --refresh`, then verify the exact LUNA Dev bundle process, IPC listener, and `codesign --verify --deep --strict`.
