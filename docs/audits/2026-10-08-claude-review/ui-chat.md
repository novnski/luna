# Chat UI review — 2026-10-08

Scope: changes since 2026-10-03 through `d3819203`, covering the composer,
questions and approvals, transcript, persistent related chats, workbench,
sidebar, focus and sizing. Settings, Usage and provider catalogs belong to
other review workers. Source review is performed in the shared checkout;
the coordinator owns builds, test execution and native UI verification.

## Failure cases recorded before implementation

1. An unanswered async card loses selected options, its current page and
   typed text when the main chat changes, because navigation drops the
   wizard without stashing it. Returning must restore all three and the
   independent prompt draft.
2. The same loss affects a reopened answered card while composing an
   amendment. Navigating away and back must preserve the unsent amendment
   without sending it or changing the recorded answer.
3. Navigating to another chat must not display or store the first chat's
   question draft under the second chat's identity.
4. A rejected or still-pending `holdInput` receipt must not permanently hide
   the unattended-answer countdown. The user needs a visible failure and an
   opportunity to retry; only an applied receipt confirms the hold.
5. Two chats can carry the same provider request ID. Holding one question
   must not hide another chat's countdown.
6. Reopening an amendment after visiting another chat with the same async
   card ID must restore the original chat's draft, not reuse the other
   chat's current wizard.
7. Historical async cards saved before answer history was introduced have
   `resolved: true` and no `answers`. They must say "Answered", rather than
   present themselves as open questions. Source history confirms this older
   schema in the parent of `4b21e811`.
8. A late failed hold must not replace a newer unrelated composer error or
   appear in a different chat. Keep its owning chat/request and surface it
   only when that chat is visible with no newer failure; clear its own banner
   when the request resolves.
9. Clicking into the answer field or elsewhere in a question panel is user
   interaction even before a keypress or option selection. Stop the async
   hide timer, or request the blocking hold, on that bounded pointer press.

The focused GPUI regression is written before the implementation. It drives
the production composer through both unanswered and answered-card routes.
This is component integration coverage. The separate native outcomes are
recorded in Coordinator validation below.

## Findings and fixes

| ID | Severity | Trigger and previous behavior | Change |
| --- | --- | --- | --- |
| UI-01 | P2 | Leave a main chat with an unsent async answer or amendment; navigation cleared the answer editor and wizard. An overlapping card ID in another chat could also be reused when reopening. | `composer.rs`: stash before changing the chat key; reuse a live reopened wizard only when its chat matches. |
| UI-02 | P2 | Keep waiting received a failed or pending command receipt; the UI still permanently hid the countdown. Request IDs were also held globally across chats. | `composer.rs`: only Applied confirms the hold; restore the timer and show a retryable error otherwise. Key holds by chat/request. Retain late errors under their owner without overwriting a newer error; retry/resolution clears only the exact owned banner. |
| UI-03 | P3 | Open an answered async card saved before `4b21e811`, when cards had `resolved` but no answer history. Its status said Open. | `transcript.rs`: a resolved card without recorded labels says Answered and still offers change. No history is invented. |
| UI-04 | P2 | Click into the answer field or elsewhere in the question panel without a keypress or option selection. The hide/automatic-answer timer kept running. | `composer.rs`: a hitbox-bounded capture of mouse down invokes the existing interaction handler. This also precedes button handlers that consume the event. |
| UI-05 | P2 | Visible task/search rows and the project selector were missing from AX. | `shell.rs` and `shell/spaces.rs`: expose names, Button roles and selection/expanded state while preserving prior keyboard traversal and mouse handlers. |
| UI-06 | P2 | Once the async picker hid, its visible transcript card could not be reopened through AX. | `transcript.rs`: expose the existing reopen action as a named keyboard-accessible Button. |
| UI-07 | P2 | Sidebar and Settings navigation disappeared from AX after sibling-only frames although their pixels remained. | `shell.rs`: bypass only SidebarPane's scene cache while AX is active, preserving dimensions and ordinary inactive-AX caching. |
| UI-08 | P2 | A multiline answer painted outside its fixed one-line field; Back/Next retained option wording on a text-only question. | `composer.rs`: grow to six scaled lines, scroll and clip longer answers, and derive hints from the current question on every page transition. |

No new engine/protocol/storage format is introduced. The two GPUI regressions
were authored before the production changes they exercise:

- `async_question_draft_survives_navigation_and_amendment`: selected page,
  typed detail, ordinary prompt, unanswered cards and reopened amendments.
- `unattended_answer_hold_requires_applied_receipt`: rejected, pending and
  applied receipts, retry, and identical request IDs in different chats.

## Source coverage

These are source-review conclusions, not claims of native or live-provider
validation. The listed entry points make the review repeatable.

| Feature | Entry points reviewed | Edge cases and conclusion |
| --- | --- | --- |
| Persistent side chats and children | `shell/side_chats.rs::create`, `shell/child_chats.rs`, `shell/pane_chat.rs::PaneComposers` | Unknown creation outcomes reuse the request ID; stale engine panels ignore completion; composers remain cached across pane closure; child completion labels refer to a turn. Native close/reopen and handoff still require verification. |
| Sidebar placement and related chats | `shell/placement.rs::effective`, `shell/related_chats.rs`, `shell/spaces.rs::sidebar_row_visible` | Self-docks, archived hosts and cycles are filtered; promoted children use saved placement; ordinary sidebars and keyboard order share the visibility predicate. No additional concrete defect found in the reviewed paths. |
| Folded startup | `shell/tabs.rs::boot_select_chat` | Startup waits for chats/spaces/sessions, honors explicit selection, and chooses only independently visible sidebar rows. An archived parent's unplaced child cannot become the boot landing. |
| Split restoration and mutation races | `shell/workbench.rs::reconcile_workbench`, `finish_workbench_mutation`, `sync_workbench_views`; `workbench/model.rs` | Layouts are scoped to main/profile/generation; exact in-flight request identity fences late completions; conflict discards queued gestures; transport retries preserve the exact command; hidden archived composers dismiss popovers. Restart and multi-pane native QA remain necessary. |
| Commands, skills and goals | `composer.rs::slash_token`, `skill_token`, `skill_wire_text`; `composer/goals.rs::goal_send` | Slash prefixes reject paths; dollar skill names reject money and shell substitutions; provider-specific wire conversion is explicit. Goal activation remains explicit; attachments/length/pending-change guards prevent silent conversion of ordinary messages. |
| Blocking questions and approvals | `composer.rs::pending_input_request`, `auto_answer_plan`, `hold_auto_answer`, `wizard_finish` | Approvals, MCP and secret routes remain distinct. Recommended-answer timers are host plans, not UI inference. Failed/pending holds and pointer engagement were the defects fixed above. |
| Nonblocking questions and amendments | `composer.rs::pending_async_question`, `present_async_candidate`, `stash_async_draft`, `dispatch_async_submission` | Later assistant speech does not supersede a question; a later user message stops auto-surfacing; manual reopening remains available; retries preserve command identity. Navigation and cross-chat wizard reuse were fixed. |
| Transcript chronology and child notices | `transcript.rs::present_timeline`, `hide_created_thread_chips`; `transcript/child_notices.rs` | Turn tails collect copy text; non-tail lanes are hidden; create chips are only hidden when a same-turn child notice can claim a non-error create. History actions use checkpoint eligibility. Native interleaving needs observation. |
| Tool results and thought reflow | `transcript.rs::rich_output_display`, `thought_cols_for_width`, `thought_detail_lines`, tool group rendering | Rich output has readable media fallbacks rather than inline base64; full output remains bounded; thought wrapping uses the live width and interface scale with cache keys including width. Verify actual image/diff/long-output geometry in the fixture. |
| Stop, stalled and owned work | `composer.rs::interrupt_chat`; `transcript.rs::provider_notice_label`, working trailer; `task_work.rs` | Stop tracks the addressed chat; stalled text counts from last activity without ending the run; unknown background work is not reported as current. Owned-work stop eligibility excludes services and terminal/nonowned rows. Provider-neutral terminal status is engine-owned and belongs to the coordinator's end-to-end checks. |
| Focus, keyboard and scaling | `shell.rs::restore_mounted_focus`; `shell/tabs.rs::cycle_session`; composer question keys and compact controls | Focus restoration checks the mounted tree, overlays guard session cycling, number shortcuts distinguish the answer field, and shared UI sizing drives controls. Pointer engagement was fixed; actual keyboard traversal and narrow-width geometry remain native QA. |

## Coordinator validation

The coordinator completed the checks below; saved images, logs and readbacks
were inspected when updating this report. Evidence paths in this section are
relative to `target/qa/claude-review-20261008/`.

| Validation | Result and evidence |
| --- | --- |
| UI library regressions | **PASS — 1,188 passed, zero failed** in `logs/ui-lib-r3.log`, including both `async_question_draft_survives_navigation_and_amendment` and `unattended_answer_hold_requires_applied_receipt`. |
| Clippy and formatting | **PASS** — coordinator-recorded exit 0 for `logs/clippy-r3.log` and `logs/fmt-r4.log`; Clippy retains existing warnings. |
| Refreshed QA app | **PASS** — 44-second coordinator refresh, with a 43-second build in `logs/qa-app-build-r3.log`. `qa-identity-r3.json` records LUNA QA Review, bundle `com.novnski.luna.qa.review`, exact bundle executable PID 17694, IPC 59841 and isolated QA data. The coordinator verified strict identity/signature. |
| Rendered AX subtree persistence | **PASS** — `sidebar-ax-native/sidebar-polish.pass` and `readback.json`; eight tree snapshots and four PNGs. All expected chat/project Buttons remain in sidebar frames 2, 5, 7 and 9; Settings Buttons remain in frames 13, 15, 16 and 17. |
| Native sidebar/question accessibility | **PASS** — subsequent `ax/question-navigation-final.txt`, `ax/question-amendment-restored.txt` and `ax/question-deferred-reopened.txt` retain sidebar controls/tasks, the question action and the correct answer-field name after composer edits and reopening. |
| Three-line answer layout | **PASS** — `screenshots/question-multiline-fixed.jpg` keeps the exact Unicode/markup/punctuation draft inside the answer border, with footer and independent prompt separate; corresponding AX names the field Type your answer. |
| Bounded long-answer scrolling and split | **PASS** — `screenshots/question-long-answer-bottom.jpg` shows lines 11–16, `screenshots/question-long-answer-top.jpg` shows lines 1–6 after caret navigation, and `screenshots/question-long-answer-narrow.jpg` keeps the six-line editor and footer inside a split. Matching AX text is saved under `ax/`. |
| Navigation and deferred drafts | **PASS** — `screenshots/question-navigation-final.jpg` retains page 2/2, the exact three-line answer and Independent prompt draft after the QA beta roundtrip; `screenshots/question-deferred-reopened.jpg` restores the longer draft after Answer later/reopen. |
| First answer and amendment delivery | **PASS** — correct-chat submit readback and `question-native-amendment.json` retain two ordered answers and two tagged user deliveries. Details below. |
| Workbench and goals | **PASS for observed paths** — `screenshots/workbench-native-split.jpg` confirms split creation; use the newer `screenshots/question-long-answer-narrow.jpg` for corrected answer geometry. `screenshots/codex-goal-completed.jpg` and AX readback show Goal achieved in 7s. |
| Corrected performance fixture | **PASS at both sizes** — `performance-1200-r2/acceptance.json` and `performance-10000-r2/acceptance.json` both have `fixtureExit: 0`, `effectiveCode: 0`, `checksFailed: false` and no failed checks. |

The native question owner is **QA alpha,
`eeca8a13-1048-433e-b896-a142c32f2bcc`**. Use
`question-native-submit-eeca8a13-1048-433e-b896-a142c32f2bcc.json` for the first
answer: Markdown plus the exact three-line draft below. Exclude the earlier
unsuffixed `question-native-submit.json`, which addressed an older chat.
`question-native-amendment.json` confirms the same owner through its tool
output reference and card `aq-luna-ask-b06ab31a9078`: first Markdown plus the
three-line text, then JSON (Recommended) plus
`Amendment Ω — retain this draft across navigation.` The second tagged user
message explicitly changes the earlier answer. The earlier answer remains
unchanged. `screenshots/question-amendment-restored.jpg` separately proves the unsent
amendment survived navigation before submission.

The coordinator also observed the Settings model cache refresh without
remounting. `screenshots/model-cache-refresh.jpg` shows Updated 3 model catalogs and its
AX capture retains all Settings navigation; `screenshots/provider-catalog-refreshed.jpg`
shows the refreshed picker. The coordinator additionally confirmed Small
Context appeared after using Refresh model cache, without remounting.
Provider catalog details remain in that owner's report.

These native draft checks prove in-process navigation and reopening.
Persistence of unsent async answer drafts across app restart is outside the
implemented contract and is not claimed. The multiline screenshots prove the
recorded full-width and split layouts; they do not establish a separate
native pass at every Interface size. Source-only review findings and broader
reproduction cases retain their original scope below. This worker performed
no build or GUI control; the coordinator produced the execution evidence.

### Reproduction checklist for later reruns

1. **Async draft and amendment:** request two optional questions; select an
   answer on page one and type on page two. Also type an independent prompt.
   Navigate away and back, then use Answer later and reopen. All values must
   remain. Answer once, reopen to change it, repeat navigation, and send: one
   amendment must appear in history. Test two chats carrying the same card
   ID in a synthetic fixture; their drafts must stay separate.
2. **Timer engagement and failure:** show a blocking recommended question
   and an async question. Click the answer field/header and wait beyond the
   original deadline: the former must remain unanswered; the latter must
   stay open. Inject rejected and pending hold receipts, verify a visible
   failure plus restored countdown, then retry with Applied. Navigate during
   the failed hold and produce another failure: the late hold must not
   replace the other chat's error. Revisit the original chat for its result.
3. **Legacy cards:** load a saved resolved async card with no `answers`
   field; it must read Answered and remain clickable. Verify an unresolved
   card remains Open and newer cards still display latest answer/history.
4. **Related chats and layouts:** use `chat-placement-fixture`,
   `chat-workbench-fixture`, `side-chat-pane-fixture` and
   `child-chats-pane-fixture`. Open/close/reopen children, move a chat between
   sidebar and related lists, split and resize, navigate Main away, then
   restart and compare durable layout/draft state. Include the startup
   fixture's archived-parent-child and active-parent-child cases.
5. **Transcript and composer geometry:** use `timeline-fixture`,
   `composer-parity-fixture`, `goal-card-fixture`, and
   `claude-tool-results-fixture`; check narrow and wide layouts at each
   supported Interface size, keyboard focus/Tab/Enter/Escape, IME/paste,
   command/skill completion, goal controls, long tool output and thought
   reflow. Record screenshots with the test case and viewport size.
6. **Live end-to-end:** retain the existing S9Q (late answer/amendment/steer),
   S9U (recommended answer/hold), S4S (stalled recovery) and S1 (cancel)
   artifacts from `docs/reference/qa-infrastructure.md`. Observe native
   controls as well as provider/engine readback; direct RPC success alone
   cannot verify the timer interaction fixes.

Use the coordinator-selected app identity and preserve evidence hashes,
process/port/signature readback and repeatable commands per the QA guide.
The completed outcomes above are grounded in the saved execution evidence.

## Follow-up: performance fixture scale assumptions

The coordinator's native 1,200-entry candidate run passed. The 10,000-entry
run completed its phases but failed two acceptance checks in
`target/qa/claude-review-20261008/performance-10000/acceptance.json`.
Its `results.json` identifies a fixture assumption, not a demonstrated
production cache defect: FORK has 10,003 entries, so both the test's 10,000
entry byte-probe ceiling and the production row-cache 10,000 entry ceiling
correctly reject it by `entries`. The recent-transcript cache independently
permits 20,000 entries and retains the approximately 12.7 MB transcript.
The 32 MiB byte ceilings remain relevant, but are not the recorded first
rejection reason for this 10,003-entry row-cache case.

Failure cases recorded before correcting the fixture:

1. The byte-only rejection probe must not hit its entry/part/row ceilings
   first when ENTRIES grows from 1,200 to 10,000. It must still require a
   `bytes` rejection and no retained cache, including the one huge entry.
2. Default full-workload caches may legitimately reject either by count or
   bytes. Any retained cache must satisfy every production count/byte bound;
   the check must never raise production limits to make retention possible.
3. Positive default-cache retention/replacement/reuse needs an explicitly
   bounded compact dataset, with its own actual expected entry count. It
   must not claim that the full benchmark history was retained or reused.
4. The compact probe runs only after the measured phases and must restore
   both original documents exactly before subsequent phases. Restoration
   needs document and visible-state readback, so later geometry/resize work
   cannot silently run on the compact history.
5. Preserve the original advertised ENTRIES value, 200 large outputs,
   measured phases, byte-only/count-only rejection checks, and native paint
   evidence. Add explicit compact-probe metadata to distinguish its results.

Correction is confined to `crates/ui/examples/workbench-performance-fixture.rs`:

- Derive generous byte-probe count ceilings from ENTRIES while keeping the
  1 MiB byte ceiling and requiring the actual `bytes` rejection reason.
- Retain the default-limit transitions on full D/FORK histories and assert
  all six production bounds (recent entries/parts/bytes, row-cache
  entries/rows/bytes). A refused cache is valid here and is recorded.
- Temporarily replace D/FORK with at most 256 history entries plus three
  marked tail entries. Wait for each compact tail to be painted, require
  exact retained entry counts, both cache identities, every default bound,
  and at least 90% row reuse using this probe's own entry count.
- Restore both saved full histories and require exact document equality,
  the full visible entry count and the original painted tail before later
  phases. `probeHistoryEntries`, `probeTotalEntries`, full-workload counters
  and restoration status are explicit in the result artifact.

The measured workload setup, 200 large outputs, 11 phase groups and
production library sources/limits are unchanged. Static verification:
`git diff --check` passed; standalone rustfmt parsed the edited fixture via
`--emit stdout` without writing it. This worker ran no Cargo/build/native/app
command. The coordinator rebuilt and repeated both candidate sizes:
`performance-1200-r2/acceptance.json` and
`performance-10000-r2/acceptance.json` both report effective exit 0 and no
failed checks. The original failing 10,000-entry artifacts remain as the
before evidence.

## Follow-up: sidebar task rows absent from accessibility

The coordinator observed visible QA task rows absent from the native AX
tree, although nearby sidebar controls were exposed. Source tracing found
that `Shell::render_chat_row` applied its Button role, label, keyboard focus
and activation handling only to related-chat pane rows. Sidebar, Activity
and Palette rows skipped that block entirely. The coordinator also visually
confirmed a Cmd-K search result missing from AX and the project selector's
current label (All projects) missing from AX.

Failure cases recorded before the fix:

1. Each visible Sidebar/Activity/Palette task row must expose its full chat title
   and selected state as an actionable native accessibility node.
2. Native AX press must use the existing chat-row activation route. Preserve
   tab order and keyboard handlers exactly; adding AX semantics must not
   enroll every sidebar/search row in Tab traversal.
3. Mouse selection, right-click context menus, drag/drop and nested row
   controls must keep their existing hitboxes and handlers. The command
   palette's separate keyboard navigation must remain unchanged.
4. The project selector must expose its current label as a Button and its
   expanded state; mouse-down/click menu handling must remain unchanged.

The narrow fix makes Button/name/selected semantics unconditional for chat
rows, while keeping `tab_index(0)` and activation-key handling gated by
`surface.in_pane()` exactly as before. It changes no layout, dimensions,
mouse listeners, selection logic or context-menu code. The project selector
in `shell/spaces.rs::render_spaces_filter` receives only Button role,
its existing current label, and expanded-state semantics.
No new unit test was added. Native AX task/project presence and continued
navigation are verified by the coordinator's saved question/navigation AX
captures and the dedicated native sidebar fixture. Cmd-K and row activation
were exercised during the coordinator's manual navigation. The original
Tab/arrow/Enter/context-menu requirements remain reproduction criteria;
the acceptance table states the saved evidence's exact scope.

## Follow-up: async question transcript card absent from AX

After the picker auto-hid, the coordinator observed the visible Format
question card missing from the AX tree. Its transcript renderer installed
only a mouse click handler, without a semantic role, name or keyboard focus.

Failure cases recorded before the fix:

1. A collapsed async question remains discoverable by its complete title and
   status; both unanswered cards and answered cards with amendments qualify.
2. AX press and keyboard activation must invoke the existing
   `OpenAsyncQuestion` event for the same chat/message, without answering it.
3. A card without an actionable chat identity must not claim an actionable
   Button. Card geometry and its current mouse click behavior stay unchanged.

The fix adds Button/name/Tab/activation-key semantics beside the existing
click handler, inside its existing chat-identity guard. It does not expand
accessibility work to other transcript rows. Native acceptance **passed**:
`screenshots/question-deferred-reopened.jpg` plus its AX capture show the open card and
restored long draft; `screenshots/question-amendment-restored.jpg` plus AX show an
answered card reopened for amendment with its draft intact. No new unit test
or native execution was performed by this worker.

## Follow-up: cached sidebar disappears from subsequent AX frames

The coordinator observed sidebar and Settings navigation present initially
but missing from later full AX reads while the same pixels remained visible.
Read-only tracing established the mechanism: GPUI clears its AX builder and
bounds/action maps each frame, but a cached `ViewElement` skips descendant
prepaint and replays only scene/input/tab state. `SidebarPane` uses this
cache for both chat and Settings navigation. The question wizard is an
ordinary composer child and does not mark surrounding UI modal or inert.

Failure cases recorded before the mitigation and native fixture:

1. With native accessibility enabled, an initial full render exposes the
   sidebar task names and project selector; a composer-only redraw and later
   unchanged frames must retain those Button nodes in the app's own
   `debug_a11y_tree_json()` output.
2. Settings navigation names must survive a subsequent frame without a
   Shell notification or full-window refresh. This must be checked from the
   rendered AX tree, not inferred from visible pixels or CUA extraction.
3. Each captured frame must advance the AX frame number; stale snapshots
   cannot satisfy the assertion. Preserve before/after JSON and screenshots.
4. Accessibility-inactive rendering retains the existing cached path and
   fixed sidebar geometry. No vendor patch, global cached-view change or
   provider/model behavior change is part of this mitigation.

The native acceptance mode is added to the existing sidebar-polish fixture
before changing production rendering. It uses only synthetic AppState rows
and no engine/provider connection.

Mitigation: `Shell::render_sidebar` receives the current window and uses an
uncached `SidebarPane` only while `window.is_a11y_active()`. Its fixed width,
full height and nonshrinking layout are preserved; the old cached branch is
unchanged for accessibility-inactive rendering. Both chat and Settings
navigation share this one boundary. No vendor or other cached-view changes.

Coordinator commands, with the normal single-GUI-slot discipline:

```sh
scripts/cargo.sh build --locked -p luna-ui --features appshots-fixture --example sidebar-polish-fixture
LUNA_SIDEBAR_AX_NATIVE=1 target/debug/examples/sidebar-polish-fixture "$PWD/target/qa/claude-review-20261008/sidebar-ax-native"
```

Without the environment flag the original headless geometry/Activity fixture
still runs unchanged. Native mode writes eight `ax-*.json` rendered-tree
snapshots, four PNGs, `readback.json` and the existing pass/failure sentinel.
It checks exact Button labels for task names, the project selector and
Settings navigation, plus increasing AX frame numbers. The three sidebar
follow-ups dirty only the composer; the three Settings follow-ups draw new
frames without notifying Shell or refreshing the window. This avoids the
CUA extraction layer when proving subtree persistence.

Worker validation was source-only. The coordinator subsequently compiled the
fixture (correcting an invalid `?` on its infallible composer update) and
completed native acceptance: `sidebar-ax-native/sidebar-polish.pass`,
`readback.json`, eight fresh AX trees and four PNGs. Every expected Button
remained through all three sibling frames and all three Settings frames.
Later real-app question-edit AX captures also retain the sidebar. The
mitigation changes no model/provider behavior.

## Follow-up: multiline question answer overflows its field

The coordinator's native evidence is
`target/qa/claude-review-20261008/screenshots/question-multiline-layout.jpg`
and `ax/question-multiline-layout.txt`. The screenshot shows three answer
lines above/below the one-line pill, with the last line reaching the footer.
The AX name still refers to picking an option although Details has none.
Source review found a fixed 49/47 px outer/inner field around a multiline
`ComposerInput` whose scroll viewport was never configured for that field.
Hints were refreshed on opening/selection, but not on Back/Next.

Failure cases recorded before the fix:

1. This exact three-line draft must stay inside an auto-growing answer field
   without touching its header, footer, send controls or ordinary composer:

   ```text
   Draft Ω — line one
   Line two: keep <tags> & punctuation.
   Line three: multiline must remain editable.
   ```

2. More than six wrapped lines must stay in a bounded editor and scroll
   internally. Wheel/caret movement must reveal the first and last lines;
   no text may paint outside the field's border or overlap the footer.
3. Repeat at Interface sizes 12, 16 and 20 and in a narrow split. Wrapping,
   cap and padding must use the same interface scale as the text.
4. Choose an option on page one, advance to a text-only Details page, and go
   Back/Next. The hint must describe the current page: plain answer when
   there are no options, or selected-option override when one is selected.
5. Answer later/reopen and chat navigation must retain all Unicode,
   punctuation, line breaks and the independent prompt draft. Rendering or
   resizing must not send the answer or change Enter/Shift-Enter behavior.

This narrow layout correction was validated through the coordinator's
existing real-provider native question flow. No post-implementation unit
test or provider call was introduced by this worker.

Implemented only in the shared wizard answer code in `composer.rs`: remove
the fixed 49/47 px field height, use natural content height with scaled
vertical padding, cap the existing editor viewport at six scaled line
heights, and clip at the answer surface. Existing caret-follow, wheel-scroll
and edge-fade behavior now operates against that explicit cap. A single
per-page placeholder helper is used when opening, selecting, advancing and
going back; a question without options says "Type your answer".

The prior draft/hold/AX fixes and unrelated composer edits are preserved.
Standalone parsing/diff checks passed, followed by the coordinator's 1,188
passing UI checks, Clippy and format checks. Native acceptance **passed** in
`screenshots/question-multiline-fixed.jpg` and
`screenshots/question-long-answer-{bottom,top,narrow}.jpg`: three lines remain bounded,
a sixteen-line draft scrolls in the six-line viewport, and the split footer
fits. `screenshots/question-navigation-final.jpg` and the correct-chat submission
readback preserve the exact three-line text, page and independent prompt;
the AX field name is Type your answer. These saved images were inspected for
this report. Separate native size-12/20 runs and restart persistence of
unsent answer drafts are not claimed.
