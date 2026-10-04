# LUNA chat workbench

Requested by Iulian on 2026-10-04. This directory is the implementation brief
and visual evidence for the inset chat frame, central tabs and resizable splits.
Reference observations are separate from the LUNA design and validation results.

## Confirmed product decisions

- Keep LUNA's left navigation and right tools/Related chats functionality familiar.
  Iulian's follow-up explicitly expands the visual redesign to both sidebars:
  the left/right/header/footer must form one continuous frame, with the chat as
  the distinct inset surface. Remove mismatched sidebar colors and unnecessary
  hard seams, and simplify the right sidebar's stacked top separators.
- Put main, child, side and alongside conversations in the central workbench.
  The right Related chats surface is a persistent library, not another place
  where a conversation must remain rendered.
- The `+` button creates a **fresh child chat linked to the main chat**. Iulian
  explicitly chose this over an unrelated ordinary chat. This supersedes the
  previous agent-only child-creation UI policy for this specific user action.
- The original main chat has a visible **Main** badge and cannot be closed.
  Focus, ordering and dragging do not change which chat is main.
- Closing a tab hides a view. It never ends, stops, archives, deletes, reparents
  or forks the conversation. Reopen with one click, a row action, a keyboard
  command, or a drag from Related chats.
- Existing panes retain their minimum dimensions when the window or sidebars
  reduce available space. The inset canvas scrolls; saved axes, ratios and
  groups stay unchanged, and focusing a pane reveals it.
- Opening the right sidebar keeps the canvas's left viewport anchor and
  covers the rightmost panes. Resizing alone does not scroll to a focused
  right-hand pane. Balance panes gives each cell equal area within layout
  limits while preserving the arrangement and every tab's identity.
- New child tabs start empty. No fake prompt or provider call is submitted by
  pressing `+`. The first normal Send starts that child. User-created blank
  children do not automatically report results or wake the main chat.
- Before final delivery, Claude owns a deep review of appearance settings and
  app-wide readability after the new surfaces land. Review every UI family and
  supported appearance mode, measure composited text/icon/control contrast,
  fix findings, and preserve a settings matrix and visual evidence in
  `appearance-audit.md`. This is an explicit user requirement, not an optional
  polish item; contrast coverage alone is not full accessibility conformance.

## Design direction

**User and task:** an engineer supervising several agent conversations, comparing
answers, directing work and reviewing a checkout without losing the main task.
**Feel:** calm, dense and legible; a native engineering desk with conversations
inside a continuous frame. The inset canvas is the memorable element.

Domain vocabulary: main task, child, fork, checkout, branch, review, running,
approval, history, pane and focus. The color world is graphite, charcoal,
smoked glass, chalk, muted steel and LUNA's existing accent. Use the active
theme's tokens and font preferences; these are relative surfaces, not fixed
dark-only colors. The signature is a protected Main tab plus related
conversations arranged inside one reading canvas, with checkout context around
its perimeter. Avoid independent dashboard cards, duplicate toolbars inside
every transcript, and decorative gradients or large shadows. The depth strategy
is subtle surface separation with low-contrast borders.

## Reference evidence

See [the illustrated interaction review](reference-review.md),
[the UI implementation handoff](ui-handoff.md),
[the backend contract](api-contract.md), and
[failure cases written before implementation](failure-cases.md). The running
[verification record](verification.md) distinguishes passing checks from pending
native acceptance.

The original recordings remain at the user-supplied paths in
`evidence/recording-index.json`. The stills and short MP4 are timestamped extracts
with scaling and a documented top crop that excludes the unrelated usage bar.
The conversation frame, branch controls, tabs and split interactions remain.
Native Super inspection additionally verified nested pane creation, moving a
pane back to the tab strip, tab reorder, and finding a closed conversation in
history. No prompt, Git action, or permission grant was submitted to Super.

## Ownership and delivery

Codex owns shared protocol/layout invariants, profile-local persistence, the
manual blank-child operation, engine integration scenarios, review, native QA,
the final LUNA Dev refresh, and commit/push. Claude Code owns `crates/ui`, the
central frame, rendering, drag hit testing and motion, keyboard/accessibility,
Related chats reopening, and native UI E2E fixtures. Both work in the same
checkout and must preserve each other's work. No subagents or worktrees.

Existing unrelated work is under `docs/pending-tasks/README.md`,
`docs/pending-tasks/agent-workspace-app-research/`, and
`docs/pending-tasks/claude-code-feature-parity/`; do not modify or stage it.

## Validation status

The shared model, profile-local atomic store, revision/command guards, desktop
RPCs, blank child creation and backup inclusion are implemented. Backend
verification currently passes: six workbench RPC/persistence scenarios,
eleven pane/child/side lifecycle scenarios, 26 encrypted backup/restore cases,
five cross-task cases and two real bridge/approval-boundary cases. The new blank
child scenario proves zero startup before first Send, draft/restart retention,
running-turn survival through view close, and persistent side-fork identity.
The backend check and Clippy pass with existing repository warnings.

The complete workbench UI fixture, 1,195 existing UI regressions, lint and
formatting pass. Native checks verify balancing, drag gestures, right-side
occlusion, checkout Review, provider replies, drafts and real process restart.
The appearance audit passes 810 built-in combinations and 144 rendered sample
pairs. Evidence and exact limits are in [verification](verification.md) and
[the appearance audit](appearance-audit.md). The requested simplification of
side-thread lifecycle follows as a separate milestone.
