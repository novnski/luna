# Extensible core, SDK and plugins

**Requested:** 11 October 2026. **Status:** architecture/source audit and pending
implementation contracts. No core rebuild, SDK, plugin runtime or extracted
feature is delivered by this package.

The goal is a native LUNA whose engine, CLI, SDK and application share one
coherent contract. A user can ask an agent to create a provider or UI extension,
inspect it, enable it, and use it without rebuilding or updating the installed
application when the installed extension API already supports its needs.
Existing providers, browser features and the diff workbench should become
ordinary first-party consumers of those public contracts.

The [native iOS parity program](../ios-parity/README.md) consumes CORE-05 through
IOS-00/RM-03 and CORE-04 through IOS-04. It owns full phone presentation and
physical acceptance; CORE-21 still owns shared host/client compatibility rather
than duplicating a phone runtime or claiming a complete mobile migration.

The recommended approach is an incremental core reconstruction: preserve the
existing durable behavior, extract its authority and lifecycle boundaries,
publish the contracts, prove one independently installed provider, then move
existing features behind the same interfaces. Each slice must work before the
next extraction removes its predecessor. A repository-wide replacement would
make it much harder to preserve the existing recovery and provider semantics.

## Read order

1. [Source audit](source-audit.md): measured file/dependency inventory, concrete
   coupling, useful foundations and current verification limits.
2. [Target architecture](architecture.md): core/SDK/extension ownership,
   execution and UI contribution contracts, security, state and versioning.
3. [Implementation tickets](implementation.md): CORE-00–CORE-23, deliverables,
   owners, dependencies and acceptance gates.
4. [Verification and performance](verification.md): failure cases, real E2E
   ownership, migration proofs and measurable optimization gates.
5. [Existing backlog integration](backlog-integration.md): which current tasks
   wait for a shared contract, remain independent, or supply acceptance evidence.
6. [Sources and evidence](sources.md): pinned repository evidence and primary
   external architecture references.

Use the [shared task queue](../README.md) and [agent workflow](../WORKFLOW.md)
for actual status, claims, progress and resume notes. CORE tickets are one
workstream, `core-platform`. The first eligible ticket is **CORE-00**: establish
execution/performance baselines and test the narrow runtime choices. This
source audit is its starting evidence, not a reason to repeat the inventory.

## The product acceptance scenarios

- **New provider:** from a LUNA chat, create an extension for a real CLI, validate
  its adapter, preview its manifest/capabilities, enable it, see its models in
  the existing picker, and finish a real task. The same installed app executable
  and bundle hash remain unchanged. Unsupported provider operations are explicit.
- **Custom sidebar:** enable a layout extension that contributes or replaces
  the selected sidebar presentation using host-owned task/project data and
  actions. Selection, drafts, accessibility and permission prompts still work.
  Disable it to return to the known-good layout.
- **Video in chat:** install a renderer for a typed video attachment. Play and
  seek it, scroll it out of view, restart, reopen, then disable the extension.
  The conversation retains a usable fallback and authorized original media.
- **Built-in features:** Codex, Claude and OpenCode run through independently
  versioned provider packages; browser and diff features use the same contribution
  contracts available to an external author. A clean installation still has a
  complete default experience.
- **User control:** a faulty extension can be disabled or rolled back without
  losing its chats, replacing the engine, interrupting unrelated work, or making
  the Extensions screen unreachable. Generated code never enables itself.

“Anything in the app” becomes a growing set of explicit extension points.
Arbitrary new OS capabilities or widget primitives can require a future core
API version. Existing extension APIs must support new provider implementations,
commands, themes, layouts and renderer contributions without app recompilation.
This distinction makes the independent-update promise testable.

## Delivery milestones

| Milestone | Tickets | User-visible result |
| --- | --- | --- |
| Foundation | CORE-00–CORE-05 | Measured, versioned, client-neutral core and typed SDK/CLI; current behavior preserved. |
| First provider extension | CORE-06–CORE-08 | A locally created provider package appears and runs through the normal picker without an app update. |
| Existing providers | CORE-09–CORE-11 | Current three adapters use that public boundary with their real semantics retained. |
| Custom workbench and content | CORE-12–CORE-15 | Dynamic shell contributions, custom sidebar/theme and a durable video renderer. |
| Feature packages | CORE-16–CORE-17 | Browser and diff workbench extracted using public services and presentation contracts. |
| Authoring and lifecycle | CORE-18–CORE-19 | Create/test/preview/enable from inside LUNA; bounded updates, rollback and safe mode. |
| Certification and cleanup | CORE-20–CORE-23 | Performance, host/platform compatibility and integrated no-app-update proof; retired internal paths removed. |

These milestones are not time estimates. Default-provider migration can be
certified one provider at a time after the first external-provider proof. UI
extensions use a separate contribution boundary and do not wait for every
provider extraction. See the full graph in the catalog.

## Implementation rules

Keep native GPUI, local-first behavior, optional sync, direct-Mac constraints,
provider-native permissions, credentials and existing user data. Reuse the
current command receipts, task identity, journals and strongest-boundary E2Es.
The recent test audit is a baseline, not permission to remove tests by count.
No blanket file-size or “everything is a plugin” target overrides a behavior
contract. Follow root AGENTS.md, test-audit, the build/disk guard, native QA
isolation, LUNA Dev handoff and commit-and-push for future code changes.
