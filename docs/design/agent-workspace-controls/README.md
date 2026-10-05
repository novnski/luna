# Agent workspace controls

Status: implemented and verified on 2026-10-05. See
[the acceptance report](../../qa/agent-workspace-controls-2026-10-05.md).

The existing child-chat engine and durable workbench remain the owners of
conversations and views. This change makes their capabilities discoverable to
provider agents and gives fresh conversations explicit provider defaults.
Codex and Claude use their existing LUNA MCP delivery, including resumed runs.
Other providers advertise these operations only when their adapter actually
delivers the tools.

`get_workspace` discovers the caller's workspace and defaults.
`manage_workspace` opens background tabs/splits, closes exact views, archives
exact related conversations, or creates a persistent context fork. It uses
canonical task IDs and a current layout revision. `create_thread` continues
to create fresh helpers and now reports background placement separately.

Settings → Providers → Defaults for new chats stores model, reasoning and
model-specific options on the owning local device. Explicit draft choices and
project model selections take precedence. Defaults do not change existing
conversations or forks. An unavailable saved model blocks sending until it is
updated; no alternative model is silently selected.

## Behavior

1. A capability read identifies the caller, related conversations, supported
   providers, saved defaults, context modes, and available desktop operations.
   Thread summaries include provider, execution selection and relationship so
   a model can resolve “the Claude chat.” Multiple plausible targets require
   clarification; a provider name alone describes a fresh helper unless the
   user names an existing conversation.
2. Fresh helper chats receive only their brief. Explicit model/reasoning/model
   options override saved defaults for the chosen provider. Defaults are
   validated against that provider's live model catalog. Existing chats keep
   their execution configuration. Forks preserve an explicit source boundary
   and report whether context is native or a portable transcript snapshot.
3. Creation and layout are distinct durable operations. A background tab must
   preserve selected tabs, focused pane and composer draft. A requested split
   uses the existing workbench destination model. Tool results report actual
   placement; failed placement never causes another conversation to be created.
4. Closing a tab removes a view. Archiving a conversation changes its lifecycle.
   Close-and-archive reports both outcomes. Bulk archive resolves and freezes
   only the caller's related descendants, excludes the root and unrelated
   conversations, and refuses stale targets instead of silently widening scope.
5. The main timeline exposes a linked agent action and result. Runtime status
   comes from engine-owned child metadata, never parsed model output. A child
   completing a turn is distinct from completing its overall assigned work.
   Existing reportResults and continueParent semantics remain explicit.
6. Ordinary in-app coordination has no separate approval card, including
   creation, reading, messaging and workspace changes. Actual provider work
   still follows Ask / Auto / Full. Children remain capped at parent access;
   only an explicit escalation uses the existing grant card. Model text, copied
   context and tool results cannot grant access. Every mutation retains its
   live provider-call, exact-target and commit-time fences. Persistent
   automations ask only in Ask mode; Auto and Full do not add a LUNA card.

## Failure-first verification matrix

Write end-to-end scenarios before production changes. Each run records fixture
identity, commands, snapshots, expected/actual outcomes, exit status and hashes.

| Area | Failure to prevent | Required observation |
| --- | --- | --- |
| Discovery | Provider claims tools it cannot invoke | Actual fresh and resumed Codex/Claude catalogs match advertised tools; unsupported hosts report unavailable |
| Target selection | “Other agent” selects the wrong chat | Canonical LUNA IDs, provider/model and parent/source metadata appear in bounded reads; duplicate labels remain distinguishable |
| Defaults | Parent Codex settings leak into Claude | Per-provider model, reasoning and option choices survive settings save/reload and appear in created-chat configuration |
| Overrides | Omitted and explicit choices are confused | Explicit values win; unsupported model/options fail before creation; existing target settings remain intact |
| Context | Fresh chat silently receives history | Child initial context contains only the brief; a fork records its source and actual context mode |
| Background | Child steals selected tab or focus | Before/after workbench snapshots retain focused pane and active tabs, with one new inactive tab |
| Split | Stale destination retargets another pane | Exact destination/revision is checked and failures are returned without layout or child duplication |
| Close | Closing a tab destroys or stops the conversation | Conversation, draft and active work survive and the view can reopen |
| Archive | Parent or unrelated chat is archived | Exact descendant set is returned; root and unrelated rows remain unchanged |
| Combined action | Partial close/archive is hidden | Result identifies each applied, unchanged or failed operation |
| Retry | Retried create starts two providers | Stable idempotency key yields one persistent child and the original receipt |
| Authorization | Routine coordination prompts unnecessarily or bypasses live-call checks | Ask/Auto/Full runs create/read/message/manage without extra cards; child access escalation still prompts; canceled or stale calls cannot commit |
| Timeline | Result routed to the wrong parent or duplicated | Linked action and outcome appear once in the correct parent, including after view closure and restart |
| Lifecycle | Background completion starts an unwanted parent turn | Result display and explicit continuation are independently verified |
| Persistence | Restart loses layout or saved defaults | Restart reads the same provider defaults, child identity and durable workbench layout |

## Delivery checks

Use the existing engine integration and native QA infrastructure, with an
isolated profile and all four app identity overrides. Preserve original user
data and other tasks' files. Run focused locked compilation and configured
format/lint checks, inspect the native settings and timeline, refresh the
primary LUNA Dev app, verify its process/IPC/signature, and follow the repository
commit-and-push workflow only after the implemented scope passes verification.
