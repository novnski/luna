# Implementation tasks

All tasks are **not started**. Dependencies/priorities are authoritative in [README](README.md#priorities-dependencies-and-task-ledger). Production owners below are existing integration points; inspect their crate manifests and current code before editing. Do not add new modules solely because the list spans several owners.

Each task closes with its named [acceptance scenarios](verification.md), a retained artifact manifest and an updated ledger. If baseline behavior passes, record verification and avoid a needless implementation change. A failing regression must first demonstrate the intended failure at its strongest boundary. The four-field test-audit authoring gate applies before any new/changed test; no post-implementation unit tests or test-only production seams.

## SW-00 — establish the current migration contract

**P0; no dependencies. Owners:** coordinator across engine/harness/UI; documentation and QA manifest.

1. Capture current repo SHA, installed CLI/schema versions, chosen accounts, platform, provider/mode capability facts and source owners. Credential values never enter the manifest. Include existing, resumed and fresh chats; list which tools are actually delivered in each.
2. Read current canonical suites/reference docs and map each acceptance contract to one existing owner. Identify missing contracts, not just missing filenames. Reuse the Board/automation fix evidence and IC ownership; record any source drift from the audit.
3. Choose bounded representative cases: a support/account investigation, infrastructure/source investigation, requested parallel coding task, interactive visual iteration, past-chat handoff and local follow-up. Use sanitized independent fixtures for deterministic checks, then approved read-only live targets for current data.
4. Define expected failures before code: wrong identity/scope, unavailable native capability, denied action, unknown delivery, stale result, restart, incomplete history and inaccessible media. Record a gap as implementation, verification, external dependency or optional requirement.
5. Decide the shared image/artifact reference boundary with IC-0; reuse existing host assets/uploads/journal paths rather than create competing blob stores. Select exact scoped helper-control naming/receipts using the existing backend contract.

**Deliverable:** baseline manifest, effective provider/mode matrix, contract-owner map and required/optional workflow set. **Gate:** E01 baseline and complete provenance; unknown/unsupported capabilities remain explicit. This task's discovery is not delivery of later features.

## SW-01 — show effective tools and skills for the actual chat

**P0; SW-00. Owners:** `integration_settings.rs`, `claude_mcp.rs`, `rpc/native_extensions.rs`, provider catalogs; UI `settings/harnesses.rs`, `settings/native_extensions.rs`, `composer/integrations.rs`.

1. Extend existing inventory with actual provider/account/host/project/mode ownership, discovery age/generation, native versus guidance-only skill delivery, and configured/available/attached/callable/verified distinctions.
2. Preserve existing provider IDs and picker provenance. A pasted `plugin://`/`app://` link never creates a selected identity; a catalog row or administrative resource read never claims successful active-session access.
3. Display a precise unavailable reason and supported remediation in existing picker/settings surfaces. Do not add another settings database or marketplace. Retain bounded last-call evidence without secrets; a prior successful call becomes stale when its relevant session/configuration changes.
4. Revalidate before submission and after safe-boundary mode/account changes, reconnect or restart. Focus/tab changes cannot change a call's owning chat. Preserve draft and selected-item identity on rejection.

**Gate:** E01; active-session inventory agrees with actual allowed/denied calls and survives a mode/configuration transition. Extend current inventory/picker owners, not a duplicate catalog test.

## SW-02 — preserve scoped MCP use in Claude Ask/Auto

**P0; SW-00/SW-01. Owners:** `crates/harness/src/claude/{extensions,mcp,luna_tools}.rs`, engine `claude_mcp.rs`, `permissions.rs`, bridge admission/runtime contract.

1. Establish which native restricted-launch controls can load specifically selected existing MCP connections without loading user/project/local permission overrides, executable hooks or unrelated settings. The current restriction is intentional confinement; deleting the restriction is not the solution.
2. Implement the smallest verified active-session route: explicitly scoped native MCP configuration when the CLI enforces the required callbacks/permissions, otherwise the existing LUNA tool bridge with a narrowly admitted connection/tool route. Settle this branch using SW-00 wire evidence before code. Never use the unrestricted administrative inspection process to execute conversation tool calls.
3. Resolve connections through their current provider/Executor owner. Preserve saved credentials, connections and user-selected Executor policies; do not copy tokens, infer homes from credential stores, or add duplicate direct registrations. A read-only annotation is advisory and cannot authorize a write.
4. Keep mode, parent, machine and environment ceilings. Ask must surface required grants; Auto follows its eligible reviewer contract; unavailable enforcement fails closed with a specific reason. Never silently switch to Full or advertise a connected-but-unusable server as delivered.
5. Correlate call/result/approval/cancellation to the exact root chat, run and connection. Validate continued and resumed sessions; a rejected/stale call must not reach upstream.

**Gate:** E02/E05 and the real read-only call in E04 pass in selected Codex/Claude modes. At minimum the intended Ask workflow must work. Auto requires separate live proof on an eligible account; ineligibility is a recorded blocker/limitation, not a passing Auto result.

## SW-03 — deliver approved recurring skills without weakening modes

**P0; SW-00/SW-01. Owners:** `integration_settings.rs`, Claude extension/composer input paths, `runtime_contract.rs`, existing managed-skill UI.

1. Reuse inventory, copy/install, revision checks and scope precedence. Select the actually needed skills first: computer-use-readback, docs/research, Git, clipboard, support, Infra Monitor, design and delivery conventions.
2. Preserve reviewed skill identity, scope and content revision in launch/input provenance. Use native skill delivery where verified; otherwise provide approved instruction content through the existing root instruction contribution path and label it guidance-only. This does not grant executable hooks/custom commands or native app access.
3. Ensure a required unavailable skill is visibly unavailable before sending. Revalidate after edits, account/mode/project changes and resume. Keep independent projects' overrides and preserve existing new-chat defaults; do not replace existing conversations' execution selection.
4. Establish that real work can follow the guidance through allowed tools. A body in a catalog or prompt is insufficient to certify the workflow; unavailable native semantics remain explicitly unsupported.

**Gate:** E03 proves skill resolution, observable work and permission preservation in the selected mode; current managed-skill mutation coverage stays its primary owner.

## SW-04 — finish the Moralis evidence-to-draft/action journey

**P0; SW-02/SW-03. Owners:** active integration/permission paths and existing browser/terminal/source tools; workflow acceptance runner.

1. Discover through Executor, inspect the exact tool contract, then read one approved support conversation and relevant account/usage or infrastructure data. Add Slack/source/Git evidence when the selected case requires it. Preserve query time, exact target and origin links.
2. Produce a concise customer draft or technical handoff grounded in those responses. Retain uncertainty and avoid attributing stale endpoint/source data to the UI. The same task must remain readable and resumable in LUNA.
3. Exercise a reviewed mutation only against a disposable service fixture or explicitly approved staging target. Show the exact action, obtain the proper grant, perform it once and verify an independent readback. Stop/restart around uncertain delivery and prove no silent resend.
4. Include an authentication/unavailable-tool failure: show its actual reason and supported recovery path, while preserving current connection ownership and request state.

**Gate:** E04/E05 on both primary providers. Production ticket sends, snoozes and database mutations are outside automatic QA authorization; drafts and live reads are the required real-service evidence.

## SW-05 — close uncovered transcript/identity contracts

**P0; SW-00. Owners:** native provider normalizers, engine session fold/journal/native-subagent ownership, doc parts/deltas, UI transcript row/anchor projection.

1. Inspect current `timeline_order_e2e`, provider tool-result suites and recent fixes. Reuse their chronological/part-identity cases; add a case only when the existing owner cannot reach the credible failure.
2. Trace text/tool/result boundaries and terminal ownership through the real adapter → journal → document → rows path. Cover independently arriving child/late results, mixed update/insert deltas and duplicate native IDs only where not already owned.
3. Repair the owner that loses information; do not sort the final UI to mask wire/persistence defects or deduplicate two genuinely distinct provider messages. Preserve private/full-result retention limits.
4. Extend native row/viewport proof for scroll position, off-screen updates and restart. Reuse IC H09 for interactive-view lifecycle once those views exist; this task owns ordinary long transcripts.

**Gate:** E06. Existing historical reproductions must first fail on the current baseline to justify a fix; a passing baseline is recorded as already covered.

## SW-06 — certify long-session human steering and recovery

**P0; SW-05. Owners:** `sessions.rs`, queue/provider inputs, context/history operations, `recovery.rs`; native QA over existing production paths.

1. Exercise ordinary follow-up versus live steer, revised/late questions, image inputs, a completion race, stop/restart, fork and rewind. Preserve exact task/draft/run/part identities and model/account/mode choices.
2. Compare accepted provider inputs with authoritative command receipts, journal/document and visible rows. Distinguish sent, accepted, completed and unknown states; do not turn a queued receipt into success.
3. Resume the same task after restart, then create a deliberate fresh-chat handoff with exact source/branch/next-step context. Native context and portable snapshot context remain labelled. Claude's unverified out-of-turn compact operation stays unavailable unless a separate real contract is established.
4. Fix only failures reproduced through the strongest existing boundary; no new context manager or general replay framework.

**Gate:** E07/E08 plus native E06. Current `message_queue`, `native_inputs`, `queued_attachments`, `restart_resume` and history suites retain distinct contract ownership.

## SW-07 — integrate the existing visual artifact delivery

**P1; SW-01 and IC-0–IC-3. Owner:** [interactive chat surfaces](../interactive-chat-surfaces/implementation.md).

1. Deliver HTML storage, isolated renderer, callable preview/publish and native inline lifecycle through the IC phases. Do not introduce another preview server, renderer, artifact store or publication API in this package.
2. Add the audit's representative mock/chart to that plan's native campaign: preview, inspect diagnostics/pixels, publish, interact, revise and republish, restart and reopen/export. Publications remain immutable; a revision is a new reference/card.
3. Keep main/side/Board ownership and graceful unsupported-platform/provider states. Original all-provider HTML scope remains required by IC; this journey specifically records real Codex and Claude use.

**Gate:** E11 consumes IC H01–H09 evidence and adds the end-to-end user iteration outcome. It does not duplicate their renderer/storage/isolation assertions or declare IC-6 complete early.

## SW-08 — bind computer use to an exact target and retain evidence

**P1; SW-01/SW-02/SW-09. Owners:** existing browser/Appshots/attachment UI, connection/tool delivery; external `luna-computer-use` through its existing Executor owner.

1. Reuse the configured external browser/native MCP tools. Return target/session/window identity and supported operations, request required OS grants through their existing flow and verify a harmless read. Preserve existing connection policies.
2. Bind every action and capture to that identity. User tab/focus changes cannot redirect an action to another target. Appshots remain explicit user captures rather than proof of autonomous control.
3. Show the returned DOM/AX/pixel evidence in the owning LUNA conversation with bounded source/target facts and a usable follow-up. Validate denied/revoked OS access and target closure without fabricated screenshots or silent retargeting.
4. Start with the configured browser that actually supports control. Do not assume LUNA's embedded WKWebView is a Chromium/CDP tab; selected embedded-tab automation needs a separately proved interface. IC's isolated HTML renderer retains its own preview path and no general host bridge.

**Gate:** E10/E11; one actual browser interaction and one required native-UI inspection have independent readback. A browser display tab or a connected MCP entry alone does not pass.

## SW-09 — present authorized visual tool results

**P1; SW-00 and IC-0 shared contract. Owners:** provider result normalization, existing journal/assets/uploads/RPC authorization, transcript/tool detail/image previews; align with IC presentation references.

1. Capture actual screenshot/image/resource result shapes from the selected tools/providers. Inventory where bytes are intentionally omitted, retained only for Copy, already exposed as files or genuinely unavailable.
2. Add typed lazy visual references for supported returned content, using existing owning-host storage/transport. Preserve bounded metadata in documents/sync; never push screenshot base64, credential-bearing URLs or full private tool output into synced text.
3. Make origin, capture/verification status and unavailable/stale state readable. Reuse normal image previews for image content; interactive app resources belong to IC-4. Do not create an audio player or library as incidental work.
4. Respect the existing computer-use journal omission policy. Any required retained capture must use an explicit authorized host-owned asset path with documented retention/deletion. If the native wire has no recoverable bytes, show that limitation and use a verified supported capture route rather than claiming an image from metadata.
5. Prove allowed reads, cross-task refusal, restart and owner removal. Do not let arbitrary paths in tool prose become authorized media.

**Gate:** E09. The user's actual visual-inspection path must produce readable evidence; a truthful unavailable fallback is only the fallback contract, not certification of the missing visual feature.

## SW-10 — complete scoped helper supervision

**P1; SW-01/SW-06. Owners:** `cross_task_tools.rs`, `child_threads.rs`, existing control grants/receipts, native-descendant/session tracking and Activity/workspace UI.

1. Reuse create/read/send/wait and model/default/context selection. Expose only the needed missing stop/interrupt operation through the existing control authority path. Do not re-enable legacy generic destructive controls wholesale.
2. Address exact task/run/host identity and, where relevant, expected state/revision. Enforce trusted user authorization, ancestry and permission ceilings; a child's report is untrusted data and cannot grant control of a sibling/unrelated task.
3. Report accepted-versus-confirmed stop, race-with-completion, offline/unknown and remaining owned work. Closing a view is not stopping a worker; a finished parent turn is not completion of all descendants.
4. Keep transient provider agents separate from persistent LUNA chats. Results arriving out of order retain origin and source context without stealing selected tab, focus or draft. Reuse Board reviewer/lifecycle rules.

**Gate:** E12/E13. A stopped helper has an authoritative terminal readback; unrelated running work and views remain intact.

## SW-11 — migrate skills, history and general-work context

**P1; SW-03; IC-5 for Find. Owners:** existing managed skills, local import/activity search/session history, provider defaults and space picker; no new persistence layer.

1. Reuse reviewed copy/install to migrate the high-use skills, preserving revision/scope precedence and sibling-provider settings. Credentials/connections stay with their owners. Validate startup and resume behavior through SW-03.
2. Import representative sanitized Codex and Claude histories into an existing project; preserve provider/source attribution, timestamps, archive/search state and source boundaries. Missing older/cloud-only history remains explicitly unavailable.
3. Find a prior task through current global search, open the correct branch/context, then find an off-screen body passage through IC-5 and prepare a fresh handoff. Historical text or pasted native links cannot create new tool grants.
4. Provide a predictable general-work setup using the current device+folder space model, with clear project/host/repository scope. Preserve cross-repo access limits; do not infer that a generic workspace is permission to mutate every repository.

**Gate:** E03/E14 and IC S01–S06 for Find. Broader settings registry, cross-repository feature bundles and new projectless schema remain outside this task.

## SW-12 — verify the already shipped Board/automation fixes

**P1; SW-00. Owners:** [existing fix package](../../design/board-automation-ticket-fixes/verification.md), Board store/tools/UI and automation watch/list/editor.

1. Reuse LUN-1/2/11/12/13 and their failure/E2E/native evidence. Check the current build before reopening an issue. No duplicate tickets are created by this plan.
2. Verify editor layout, deletion/status freshness on reveal, correct Board banners/counts, settled deletion refusal for active/queued/uncertain work, and original same-chat structured-image reuse into a ticket/worker.
3. If the UI image exists but the agent cannot resolve its legitimate reference, diagnose delivered attachment identity/catalog/ledger evidence first. Own-chat uploaded references are supported; foreign-chat/prose-only paths are not a reason to weaken authorization.
4. Only a current failure opens a scoped fix under the existing contract owner. Preserve the original ticket/chat history and data.

**Gate:** E15. A passing replay closes verification without rebuilding Board, image attachment or the scheduler.

## SW-13 — certify local recurrence and host lifecycle

**P1; SW-01/SW-04. Owners:** existing automation scheduler/RPC/store/UI and `qa-automations-live.mjs`.

1. Run one current-chat heartbeat and one new-chat/project follow-up with the actual selected providers. Record two real occurrences, destination, mode/skills/tools, notifications and result receipt.
2. Exercise update/delete and host sleep/restart across a due slot. Document existing missed-slot/lease/unknown-delivery policy; no silent catch-up promise or duplicate external write. Distinguish an already admitted in-flight run from a new post-cancellation occurrence.
3. Retain run history and accurate Needs attention/unknown/failed states after a tool/auth failure. An owning host must run for local schedules; a saved RRULE is not execution evidence.

**Gate:** E16. Hosted/offline execution is deferred to the explicit SW-16 requirement decision, not added to this scheduler task.

## SW-14 — certify additional connected-app workflows

**P2; SW-02/SW-09/IC-4. Owners:** IC app bridge/renderer and native provider-owned app catalog/capabilities.

Choose one actually needed document/sheet/slide/app operation. Reuse IC A01–A07 for real resource render, live reads, confirmed/declined writes, stale-session rejection and original-connection ownership. Record which native app selections and operations are available per provider; unsupported native-only apps keep a precise fallback. Any new integration follows normal Executor discovery/reuse/setup without copying credentials. No plugin marketplace, duplicate bridge or Office UI clone.

**Gate:** E17 and the existing IC contracts. Promote this task to the required switching set only when the selected real workflow requires it.

## SW-15 — review the desktop switching evidence

**P1 release gate; SW-04 and SW-06–SW-13. Owner:** coordinator; no new runtime feature.

Assemble independent results for the required workflows selected in SW-00. Run the actual journeys using LUNA, intended accounts/modes, core skills and connections. Record every escape to another agent app and its reason, distinguishing normal provider authentication/setup from inability to finish work. Re-run each required journey after engine/app restart. Report pass/fail/blocked per provider/platform and the precise remaining gap. Optional SW-14/SW-16 limitations stay visible; if made essential, they become hard dependencies before a complete-switch claim.

**Gate:** E19. Do not infer parity from source counts or a percentage of checked boxes.

## SW-16 — decide conditional voice, phone and offline needs

**P2 conditional; SW-00. Owners:** product requirement record; existing Control-this-Mac package for paired remote work.

Identify the actual required journey: dictating a prompt, live voice conversation, remotely steering this Mac, or execution while every local host is offline. Record whether it is optional or a switching blocker. Consume M0–M4/its validation plan for phone control; do not duplicate its transport. Voice or hosted scheduling needs a separately scoped design/failure contract before implementation, including actual input/uptime requirements. Prefer an already available always-on LUNA host when that satisfies the requirement. This task authorizes discovery and dependency selection, not an automatic cloud/voice build.

**Gate:** E18 requirement/owner decision; any selected implementation must pass its own native E2E before promotion into SW-15.
