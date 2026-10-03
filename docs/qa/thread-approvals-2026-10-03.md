# Thread approval audit — 2026-10-03

## Conclusion and scope

LUNA's primary Auto/Full provider mappings agree with BB and T3. The defects
were in inherited policy publication, legacy sandbox inheritance, inconsistent
Codex network defaults, and picker failure/ceiling feedback. Those paths are
corrected in this change. Auto uses a native reviewer; it does not mean approve
every request. Full bypasses the execution provider's ordinary prompts; it does
not bypass LUNA's cross-chat consent, OS consent, or external tool controls.

Source baselines inspected locally:

- LUNA: `6ef3dab7` before this change.
- BB: `2573ad5e85835dc1c90aa33795e6dba52ef5aa92`.
- T3: `5cc99e1c23980d7995a13c47f969b47cb68ed1be`.
- Installed CLIs: Codex `0.159.0`, Claude Code `2.1.288`.

This audit follows the picker and stored `ChatConfig`, host dispatch, warm
runtime replacement/resume, native adapter settings, approval decision routing,
and ordinary/child/side-chat ceilings. BB and T3 were reference reads only.

## Provider comparison

| Path | LUNA | BB | T3 |
| --- | --- | --- | --- |
| Codex Auto | `workspace-write`, `on-request`, `auto_review` | Same mapping | Same mapping |
| Codex Full | `danger-full-access`, `never`, explicit `user` reviewer | Full sandbox and `never`; reviewer set from policy | Same explicit mapping as LUNA |
| Codex Auto networking | Enabled on thread start/resume and ordinary turns after this fix | Enabled on workspace turn policy | Workspace turn policy leaves network to provider defaults |
| Claude Auto | Native `auto` plus `--restricted`, with named command/web tools | Native `auto` plus workspace sandbox configuration | Native `auto`; launch-argument overrides also supported |
| Claude Full | `bypassPermissions` and skip-permissions flag | `bypassPermissions` plus SDK skip capability | `bypassPermissions` plus SDK skip capability |
| OpenCode Auto | Unavailable: no verified native automatic reviewer | Not used as a parity target here | Not used as a parity target here |

Reference implementations:

- BB: `plugins/provider-codex/src/session-params.ts`, especially
  `toCodexThreadPermissionSettings`, `toCodexPermissionSettings`, and
  `toWorkspaceWriteCodexSandboxPolicy`.
- BB: `plugins/provider-claude-code/src/interactive-contract.ts` and
  `bridge/session-options.ts` (`toClaudePermissionMode`, workspace sandbox).
- T3: `apps/server/src/provider/Layers/CodexSessionRuntime.ts`
  (`runtimeModeToThreadConfig`, `runtimeModeToTurnSandboxPolicy`). T3 explicitly
  resets the reviewer so Auto does not remain sticky after a mode switch.
- T3: `apps/server/src/provider/Layers/ClaudeAdapter.ts`, native permission map
  and `allowDangerouslySkipPermissions` in query setup.

LUNA's Claude restricted profile deliberately ignores user/project/local
settings and explicitly names Bash/WebFetch. That differs from the BB sandbox
and T3 settings-source strategies. Changing it would alter the workspace trust
boundary and custom-hook behavior; the current behavior is already disclosed
in provider limitations. This audit does not replace it with a broader policy.

## Defects and corrections

### Child and side-chat mode publication ignored inheritance

`SessionsEngine::prepare_permission_mode` resolved only provider/model/machine
constraints. Dispatch later intersected the child's creation ceiling and its
parent's current effective mode. An idle Auto child could therefore show Full
and record Full in its audit, then execute as Auto on the next turn.

Picker publication and dispatch now share `child_permission_ceiling`. It
combines the durable creation ceiling, the current parent's effective access,
and any saved stricter sandbox. An explicit user grant retains its existing
exception to the current parent's ceiling. An Ask-only ceiling rejects a
product-mode request instead of publishing a misleading Full/Auto value.

Persistent side chats had the same mismatch through the ordinary permission
command. Permission previews now reuse their existing run-admission checks,
which apply both their frozen creation policy and the current parent's limits,
without starting a provider. The original requested mode is retained for honest
audit/ceiling reporting and for rejecting an Ask-only fallback.

### Full parents passed a stale sandbox to children

The product permission picker does not rewrite every legacy `ChatConfig.sandbox`
value. A Full parent can legitimately retain `workspace-write` in that field.
Child provisioning copied it into `sandbox_ceiling`. Codex then received a
workspace sandbox alongside a Full/never policy; Claude uses the effective
native mode, making the shared policy inconsistent between providers.

Child provisioning now derives the native parent's effective sandbox through
the same helper used by side chats. New Full children inherit unrestricted
access. Existing saved sandbox ceilings remain restrictive and are now included
in effective-mode resolution; this change does not silently widen old grants.

### Codex network policy differed between startup and turns

Ordinary `turn/start` already sent `networkAccess: true`, matching BB's workspace
policy. LUNA's policy metadata incorrectly said Restricted, and thread startup
left networking to configuration defaults. Provider-started goal/review turns
could consequently receive different networking from ordinary turns.

Codex workspace metadata now says Unrestricted networking. Thread start/resume
explicitly set `sandbox_workspace_write.network_access=true`; normal turns
retain their existing explicit value. The filesystem sandbox and native
approval reviewer remain in force. Title-only runs retain their restricted,
tool-disabled configuration. A preserved legacy read-only child/side-chat
sandbox continues to report restricted networking.

### Picker errors and ceilings were misleading

The picker staged a requested mode locally before the host accepted it and
only logged errors. A rejected/offline change could remain visible as a pending
request. Additionally, every requested/effective mismatch promised application
at the next safe point, even when an authoritative ceiling prevented it.

The picker now waits for host-synced requested/effective state, shows failures
in the access chip and menu, and names parent/machine/environment/provider
ceilings. Real active-turn transitions retain the existing pending explanation.

## Existing behavior retained and checked

- Auto never sets blanket `auto_approve`; only effective Full sets that legacy
  bit. Codex sets approval policy and reviewer on both thread setup and turns.
- Mode changes during active work preserve the current effective mode. A
  replacement provider runtime applies the requested mode at a safe boundary.
- Native approval choices are mapped to exact advertised responses. Stale,
  forged, cancelled, and already-resolved requests cannot manufacture grants.
  Full's stray ordinary requests use one-request responses, not persistent rules.
- Claude verifies reported native permission modes and stops on a weaker or
  unknown mode. Native Auto escalation can still require a user decision.
- Claude Auto is model-gated; unsupported Auto never silently becomes Full.
  OpenCode's blanket approval flag is not represented as a native reviewer.
- Side chats retain their creation/parent and legacy read-only restrictions.
- Approval for creating/messaging another LUNA chat remains independent from
  provider Full Access.

## Failure cases written before implementation

1. An idle child inherited Auto, but selecting Full publishes Full in its
   effective configuration and audit despite the creation/parent ceiling.
2. A Full parent retains a legacy `workspace-write` field. A new child copies
   it as a sandbox ceiling even though the parent's execution is unrestricted.
3. A parent is subsequently restricted. Its ungranted child's permission
   picker must honor the current parent as well as the creation ceiling.
4. A child explicitly granted Full by the user must retain that grant when
   the parent is Auto; a fix must not erase this exception.
5. A parent ceiling of Ask must reject a requested product mode that would
   silently become Ask, leaving the existing configuration unchanged.
6. Codex Ask/Auto metadata reports restricted networking while the adapter
   passes `networkAccess: true`. Describe the actual policy, including on
   native thread startup and resumed/automatic turns.
7. Changing modes during an active turn must retain the running effective
   policy until its replacement starts. Denied/cancelled/stale approvals must
   never turn into grants; Auto must never blanket-approve requests.
8. A disconnected or rejected permission picker change must not leave a
   fictional requested mode in local state. Show the failure to the user.
9. A confirmed ceiling is a restriction, not a promise that broader access
   will become available at the next safe turn boundary.
10. Side-chat permission commands must apply the same frozen/current-parent
    ceilings as their run admission. Ask-only side chats refuse Full; workspace
    side chats publish Auto; unrestricted side chats can publish Full.

Validation uses production RPC/engine/document paths and real adapters with
deterministic subprocess peers. These tests do not establish live model/account
eligibility or exercise a provider's OS sandbox. Retain command logs, native
wire receipts and hashes separately from this report.

## Verification

Repeat the offline suite from the repository root:

```sh
scripts/qa-thread-approvals.sh /absolute/new/evidence-directory
```

The runner preserves logs, start/resume/turn wire receipts, source revision,
working-diff summary, exit status and SHA-256 manifests. It uses isolated
provider fixtures and the repository build wrapper with `--locked`.

The recorded suite passed **49 checks**, including six start/resume mode
combinations in the Codex wire test, all ten Claude adapter approval scenarios,
the child lifecycle/ceiling cases, engine mode replacement, side-chat boundaries,
and five picker/provider-capability checks. Workspace formatting passed.
Clippy passed for the affected harness/engine/UI libraries with existing
repository warnings; the final engine change also passed its focused rerun.

Canonical local evidence is under
`target/qa/thread-approvals-2026-10-03/verified-suite/`; both its manifest and
the nested `detail/SHA256SUMS` were checked successfully. The parent directory
retains before-fix failures, the installed-provider `live-startup.json`, and
build/lint logs. An earlier `final-suite` run exited 127 because the reporting
script was edited while it was executing; `verified-suite` is the complete
successful run of the frozen runner.

Original failing assertions were captured before the fixes:

- Child effective mode was Full where its durable ceiling required Auto.
- The child picker accepted Full above an Ask ceiling.
- The side-chat picker also accepted Full above an Ask ceiling; its new RPC
  assertion reproduced an `applied` receipt before the fix.
- Codex thread config omitted the networking value present on ordinary turns.

An additional isolated probe of installed Codex `0.159.0` started ephemeral
Auto and Full threads and checked its returned approval policy, reviewer and
sandbox. Both matched the requested policy, including Auto networking. It used
a disposable provider home, invoked no model turns and removed that home.
This confirms configuration acceptance, not live automatic-review decisions.

Live Claude model/account eligibility and execution of actual OS-sandbox escapes
were not exercised in this audit. Claude protocol behavior is covered by the
existing deterministic subprocess approval suite; native Mac handoff checks
verify the rebuilt application separately.

Final macOS handoff: `scripts/run-macos-dev.sh --refresh` built in **58 seconds**.
The exact `target/luna-macos-dev/LUNA Dev.app/Contents/MacOS/luna` process owned
IPC port **59777**, and `codesign --verify --deep --strict` succeeded. The local
`handoff.json` records its PID and executable SHA-256. The primary data directory
and build cache were preserved; no disposable GUI app or worktree was created.
