# Contained UI alignment

Requested 2026-10-04; source baseline `49bd7f5f`.

The workbench established a contained, rounded content surface within quieter
app chrome. Audit the rest of the UI against that treatment, especially
Settings, and adapt surfaces that still assume the removed sidebar dividers.
Navigation, content and floating help should remain visually distinct and
readable without forming a grid of unnecessary separators.

## Direction and ownership

Claude owns the UI audit proposal and implementation: shared content framing,
settings/other affected surfaces, creation chooser, terminology, tooltip
styling and corresponding UI fixtures. Codex independently inspects the live
reference and LUNA surfaces, traces creation contracts, supplies audit findings,
handles necessary backend changes, and owns build/native QA and delivery.
Both preserve unrelated pending-task work. The user subsequently authorized a
read-only shortcut subagent; its audit supplies the keymap integration plan.

1. Reuse a consistent contained content surface where the route needs a main
   content plane. Keep navigation in the app chrome; avoid nested frames and
   decorative rules. Audit right panes, dialogs and empty states as well as
   Settings before deciding which need changes.
2. Use **Side chat** for a conversation created with parent context and **New
   thread** for a fresh conversation created from a parent. Internal wire and
   storage identities remain unchanged.
3. Side-chat tab help names the parent and explains that context is captured
   when it is created, not continuously synchronized. New-thread help names
   the parent and explicitly says its context is not carried over.
4. `+` opens a deliberate creation chooser with the source, context choice,
   eligible provider choices and an explicit action naming the result. The
   earlier direct Split behavior stays a fresh-thread shortcut. Provider/native
   context limitations must be clear before creation; no silent downgrade to
   a different kind of copied context, provider or account.
5. Tooltip reading surfaces must be opaque in every theme/surface preference.
   Keep text/icon contrast and keyboard/hover behavior; blur alone does not
   prevent underlying text from showing through.

Super's supplied screenshot and live app show the useful reference pattern:
an anchored provider list with names/icons and clear selection, inside a solid
reading surface. Reuse that clarity with LUNA's available providers rather
than adding unsupported providers from the reference.

## Failure cases before implementation

- A route gains double frames, clipped rounded corners, mismatched gutters or
  a visually detached sidebar; Settings loses scroll/selection/save behavior.
- Floating text remains translucent over busy underlying content, or an opaque
  background is defeated by parent opacity. Light/dark/custom surface choices
  lose readable foregrounds or keyboard focus visibility.
- UI copy calls context-carrying conversations forks in one place and side
  chats elsewhere, or fresh threads imply that parent messages were copied.
- A tooltip names the wrong source, follows tab placement rather than lineage,
  promises future context synchronization, or hides a deleted-source condition.
- Opening `+`, choosing a kind or browsing providers creates a conversation.
  Double clicks/retries create duplicates, or a stale popup targets another
  source/project/profile after navigation.
- A provider removed from the offered catalog remains a hidden creation choice,
  the chooser survives navigation into Settings, or a native refusal changes
  the action to a recent-message copy while the explanatory text still promises
  native provider history. The visible action and its explanation must agree.
- New-thread provider choice reuses a foreign account; a side-chat choice
  silently loses provider context or replays an uncertain native creation.
- A side chat explicitly created from recent visible messages later has a
  tooltip that implies complete provider history. Publish its existing
  `sideChatContextMode` for precise help; absent metadata stays unspecified.
- The chooser breaks direct Split, minimum pane sizes, drafts, existing
  histories, keyboard navigation, command-palette alternatives or reopen flows.
- A native history-created side chat loses its source label because only the
  older side-chat service publishes provenance. Publish optional
  `forkSourceTaskId` from managed task identity without converting its storage,
  binding or lifecycle. Keep it after source deletion and engine restart; never
  infer it from a title or reuse the service-only layout identity.
- A fresh thread created from a non-Main active tab is accidentally parented to
  Main, inherits source conversation history, starts a provider while browsing,
  or duplicates on retry. Its host layout and parent lineage are independent.
- New shortcut defaults override existing custom bindings, a fixed shortcut
  is assigned twice, or a chooser key leaks into send/stop/window-close handling.
  Route actions through the focused group, guard overlays, and show the effective
  binding in tooltips, the palette and Settings.
- macOS folds Shift+[ and Shift+] into `{` and `}` events without a Shift flag.
  Binding literal `cmd-shift-[` can pass a synthetic fixture while failing on
  the native keyboard. Bind the actual native event, preserve readable labels,
  and recognize equivalent recorded/custom chords during conflict checks.

New evidence: `target/qa/contained-ui-alignment/`. Prior completed milestones
and their hashes remain unchanged. Use E2E and native readback; no new unit tests.

## Shortcut direction

The existing configurable keymap remains authoritative. `Mod` means Command on
macOS and Control on Windows/Linux. Existing overrides survive upgrades; a new
default that conflicts with a saved binding is left unbound rather than taking
over that binding. Tooltips and palette entries display effective bindings.

| Action | Default |
| --- | --- |
| Open creation chooser for focused group | Mod+Alt+N |
| Create fresh thread in split | Mod+Alt+Shift+N |
| Previous / next tab in focused group | Mod+Shift+[ / ] |
| Previous / next group | Mod+Alt+[ / ] |
| Reopen closed tab | Mod+Shift+T, now configurable |

Keep existing full-conversation creation, sidebars, terminal, model picker,
sidebar-conversation cycling and platform conventions. The palette exposes
the chooser, side-chat choice, fresh split, group focus, close group, moving
tabs and balancing panes. Its existing search remains the conversation/project
search entry point; browser/editor find keys retain their own focus behavior.

The chooser must consume Escape before stop handling, and Enter creates only
the visibly selected kind/provider. Navigation, focused pane routing, custom
binding migration and conflicts require interaction-level acceptance evidence.
