# Chat placement and child execution choices

Requested on 2026-10-03, following persistent Related chats (`8425272e`).

## Product contract

Iulian explicitly chose presentation changes, not reparenting. A child dragged
to the left sidebar keeps its parent and reporting ownership and need not be
pinned. An existing chat dragged right opens alongside the selected main chat;
it keeps its history, provider and real ancestry and does not become a child.
Placement persists on this device and is scoped to its engine/profile. Side
forks retain their dedicated lifecycle and cannot be promoted to ordinary chats.

Children in the left sidebar have an explicit child marker. Opening a child
shows a clickable parent link at the top. That link always follows the stored
parent ID, never a presentation/docking association.

Parents may create fresh children with another provider, model, reasoning level
and supported model options. Child creation remains a parent-agent tool with
the existing one-use approval; users do not create children directly. The
approval must show the execution selection. Omitted execution choices retain
legacy same-provider behavior; changing providers must not carry the parent's
provider-specific model, reasoning, options or account into the new provider.

## Implementation plan

Claude owns `crates/ui` placement gestures, persistence, identity/navigation and
native E2E fixtures. Codex owns the request/tool contracts, provider-aware model
discovery and validation, child startup, integration review and final QA.

The `create_thread` request gains optional `provider` and `modelOptions` fields
without changing old serialized request hashes. A bounded read-only model
catalog tool lets the parent discover valid model IDs, reasoning and option
choices before submitting a child request. Choices are validated before
provisioning a checkout or writing a child identity, and the resolved execution
configuration is persisted on the child for subsequent turns/restarts.

## Failure cases recorded before implementation

1. Dragging must never rewrite parent/source/lifecycle lineage, duplicate a
   conversation, copy its history, or implicitly pin/unpin it. Existing left
   sidebar ordering and pinned boundaries remain valid.
2. Reject/no-op self drops, duplicate docks, missing/deleted destinations,
   side-fork promotion and unsupported remote placement. Cross-project docking
   is presentation only and cannot move a checkout or change permissions.
3. Moving, switching, closing and reopening preserve the correct composer,
   draft and run; stale view releases cannot clear a newer pane. Restored
   placement is scoped to the same engine/profile and cleans missing IDs.
4. Every left-sidebar child (including pinned/promoted rows) identifies its
   role. Parent navigation follows actual ancestry after arbitrary docking,
   handles long titles and archived/missing parents, and is absent on ordinary
   alongside chats.
5. Cross-provider creation starts fresh with the supplied prompt only. It must
   not inherit a foreign model ID, account, provider options, native session,
   transcript or source lineage. Parent reporting remains linked to the parent.
6. Unsupported/disabled providers, unavailable models, unsupported explicit
   reasoning, unknown options, invalid choice IDs, internal runtime keys and
   oversized option payloads fail before child/worktree creation.
7. Permission ceilings remain enforced by the destination adapter; changing
   providers cannot widen access or silently lose a read-only sandbox.
8. Approval displays the chosen provider/model/reasoning/options. Cancellation
   or stale authority creates nothing. Same request/key is idempotent; changing
   provider or options under the same key conflicts. Older omitted fields keep
   their serialized hashes and behavior.
9. Model discovery is bounded, read-only, account-aware, available to the
   authorized parent, and exposes no credentials or internal model options.
10. The committed child configuration and fresh-context behavior survive a
    restart. Test both directions between Codex and Claude, different models
    within one provider, and result/report ownership independently of placement.

## Verification

The repeatable evidence root is `target/qa/chat-placement-models/`.

- `engine/`: all 17 tests across `cross_task_tools`, `pane_chat` and
  `pane_chat_mcp` passed. They exercise real engine/document boundaries and
  synthetic provider processes: both provider directions, model/options
  persistence, fresh history, idempotence, catalog pagination, and nine invalid
  execution requests rejected before a child or worktree exists. The separate
  `local_profiles` identity scenario passed: accounts sharing a device and scope
  have distinct opaque profile IDs, stable after restart.
- `placement-final/`: the production Shell pointer fixture passed promotion,
  pin boundaries, ordinary alongside chats and their own composers, cycle
  rejection, side-fork refusal, child badges, real-parent navigation (including
  archived/missing parents), disk reload, profile A/B/unknown isolation and
  reverse moves. It writes screenshots, `readback.json` and
  `chat-placement.pass`. Its initial reload setup lacked the startup selection
  event; the corrected fixture models actual boot and retains the assertions.
- The existing Related chats, child pane and side pane native fixtures passed
  with their readbacks and `.pass` artifacts. All 1,194 existing UI library
  tests passed with `--test-threads=1`. The default parallel invocation aborted;
  the isolated reported test and complete serialized rerun passed.
- Formatting, `git diff --check`, and focused engine/harness/proto/UI Clippy
  passed. Existing compiler/Clippy warnings remain.
- `native/`: the signed QA app (89-second build) created both real cross-provider
  children through their parent agents and native one-use approval cards.
  Codex selected `claude-sonnet-5`, High and `contextWindow=200k`; Claude selected
  `gpt-6.1-sol`, High and `serviceTier=default`. Each child received one fresh
  user prompt and returned its exact sentinel. Provider binding, execution
  configuration and agent-created ancestry were independently read back.
  Native pointer drags promoted the real Codex child, displayed its Child
  badge and working parent breadcrumb, and docked an ordinary Codex chat as
  Alongside. Both placements and transcripts survived a graceful app restart.
  Reverse drags restored the original listings; the saved placement map was
  empty and both ordinary parents still had no child ancestry.

The scoped QA bundle was gracefully stopped, unregistered and removed after
review; its profile data and hashed evidence remain. The primary LUNA Dev was
refreshed in 3 seconds, with its exact bundle process, IPC port 59777 and strict
deep code signature verified. The final debug cache was 49 GiB, with 356 GiB
of disk space available; no broad cache cleanup was needed.

Reproduction uses the repository wrapper and shared cache:

```sh
PANE_ENGINE_EVIDENCE_DIR="$PWD/target/qa/chat-placement-models/engine" \
  scripts/cargo.sh test --locked -p luna-engine \
  --test cross_task_tools --test pane_chat --test pane_chat_mcp
scripts/cargo.sh test --locked -p luna-engine --test local_profiles \
  resolved_profile_ids_separate_accounts_on_one_device_and_survive_restart
scripts/cargo.sh build --locked -p luna-ui --features appshots-fixture \
  --example chat-placement-fixture
target/debug/examples/chat-placement-fixture \
  "$PWD/target/qa/chat-placement-models/placement-rerun"
```

Run GUI fixtures with the primary app gracefully stopped. The live-provider
driver is `scripts/qa-child-models-native.mjs`; its `seed`, `start-codex`,
`start-claude`, `snapshot`, `verify`, `verify-placement` and `verify-reversed`
modes operate only the fixed signed QA
identity documented in the script. Parent agents call the actual LUNA tools;
creation approval is performed in the app, never via an approval bypass.

## Scope and compatibility

Placements are device-local presentation settings keyed by an opaque engine
profile ID. They do not sync as ancestry. Older engines without `profileId`
refuse placement changes and neither apply nor prune stored placements.
Ordinary chats may dock beside chats on the same device. Children return to
their actual parent's Related chats; side forks stay with their source chat.
Pins retain their meaning, so a pinned child can also appear in its parent's
Related chats. Provider availability and offered model choices are checked
against the selected execution account; approval freezes that selection and
rechecks account identity before creation. Unsupported explicit choices fail
before provisioning, and inherited access can narrow to a supported mode.
