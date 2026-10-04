# Workbench interaction follow-up

Baseline: `29da9bdd`, 2026-10-04. User recordings show a newly created Main
chat answering while a persistent notice says its layout Main is unavailable.
The user also reports difficult provider switching in a blank child and asks
for direct child creation from Split, tab-level status, and context usage
inside the composer.

## Scope and ownership

Codex traces backend readiness, task identity and provider changes, owns engine
integration checks and native QA, and delivers the refreshed app and commit.
Claude owns the frontend, existing UI fixtures and their design notes. Both
work in the current checkout and preserve unrelated pending-task documents.

1. Reproduce the unavailable notice at normal new-chat creation, preserving
   real missing/deleted-chat validation. Avoid a warning latched from an
   optimistic UI chat before the durable registry has accepted its creation.
2. Verify independent provider choice before a child's first send, then its
   own fresh provider binding, permissions, draft and lineage after restart.
   A started thread retains explicit provider-switch boundaries.
3. Split creates one blank child directly in the new pane. No chooser, no
   agent execution, no duplicate child on retries or repeated clicks.
4. Remove the child/fork status/source/model strip. Put ordinary activity,
   unread/attention and meaningful failure/approval state on the tab using
   the existing sidebar semantics. Preserve identity and source information
   through icons, tooltips and contextual navigation.
5. Put context usage inside the shared composer control row near Goal. Keep
   its details and keyboard/accessibility behavior; reclaim the external
   footer space while preserving useful controls at minimum pane widths.

## Failure cases recorded before backend implementation

- A workbench watch starts before a new chat's create RPC completes and latches
  an unavailable notice even after that same chat is persisted and running.
- A stale unknown or deleted task is silently recreated, or hiding a transient
  warning also hides genuine host/profile/deletion errors.
- Provider choice before first Send creates or resumes a provider session,
  silently changes the parent, reuses its native identity, or loses the child
  draft, lineage, workspace, permission ceiling or command idempotency.
- A child provider change is accepted then overwritten by parent/provisioning
  metadata at dispatch or restart. In-flight work bypasses the ordinary command
  guards or unverified provider/account selections bypass validation.
- A target provider's model query receives the source provider's account ID.
  Native QA reproduced Claude's `claude-native` account being passed to Codex;
  the foreign-account query correctly failed while the target's default-account
  query returned models. Catalog requests and stale-result checks must use
  provider-scoped account selection.
- A direct initial choice races with execution in another window and silently
  becomes a destructive context reset. The direct command must require the
  task to remain pristine at admission/commit; a stale choice asks for the
  ordinary explicit switch instead.
- Clicking Split leaves an empty pane/chooser, creates more than one child,
  starts an agent automatically, or damages the prior layout on failure.
- Removing the metadata strip hides actionable approval/recovery controls,
  moves keyboard ownership, or loses distinguishable busy/unread/error states.
- Moving context usage duplicates it, leaves an empty footer, overlaps Send,
  Goal or access controls, or binds a pane's meter/popover to another chat.

Evidence belongs under `target/qa/workbench-followup-2026-10-04/`. Preserve
the completed prior workbench and persistent-fork milestone evidence.
