# Native iOS parity: Zeron, T3 and LUNA

**Requested and refreshed:** 11 October 2026. **Status:** current-source audit
and pending implementation plan. No iOS build, provider run, physical-device
test, notification delivery or TestFlight upload is certified by this work.

The iOS app should become a complete native client for LUNA's owned engine:
reliable task control, a fast transcript, useful project/workbench screens and
the same authoritative capabilities as desktop. The approved direction remains
the shared Rust client/native iOS architecture and **direct paired-Mac V1**.
Zeron supplies implementation references; T3 supplies a broad mobile workflow
inventory. Their cloud services and runtime assumptions are not LUNA defaults.

## Fresh source cuts

| Repository | Inspected revision | Refresh |
| --- | --- | --- |
| LUNA | `71d5db137e5bbfda13893278e558a7b0ce34ea79` | Current main before this planning change. |
| Official Zeron | `40091d469aa8a3f1ab1ec7cb404957b742940278`, v0.2.111 | Fetched `zeronsh/zeron` over SSH; local main already matched. Its configured personal fork remote was preserved. |
| T3 Code | `477282263a9115f911380b5aec7d4973267e4edc` | Fast-forwarded local main by 29 commits; preserved the existing untracked research file byte-for-byte. This is 14 commits after our earlier T3 audit. |

These are repository-source versions. T3's app configuration says 2.0.0 and
its mobile README still contains an obsolete “not distributed” warning. This
pass does not map source features to a current installed App Store binary.

## Read order

1. [Research and fresh changes](research.md): actual architecture, current LUNA
   gaps and upstream changes worth adapting.
2. [Parity matrix](parity-matrix.md): feature status, evidence and one task owner.
3. [Implementation tickets](implementation.md): IOS-00–IOS-14, bounded scope,
   dependencies, failure cases and completion gates.
4. [Acceptance](verification.md): native, physical-device, lifecycle and release
   proof; source inspection is never a substitute for it.
5. [Ownership and sequencing](backlog-integration.md): how IOS, RM, CORE, IC, SW
   and ZI work together without implementing the same service twice.
6. [Sources and evidence](sources.md): immutable references, inventory, upstream
   freshness checks and preserved checkout state.

The [shared queue](../README.md) owns live status. New tasks use the `ios` group.
The core program still starts with **CORE-00**. iOS implementation begins with
**IOS-00** after the public SDK and remote-boundary prerequisites are available.

## What needs to change

- Preserve LUNA's bundle identity and current user data while adding a real
  Rust SDK/native facade and generated, versioned Swift bindings.
- Adapt Zeron's immutable projections and measured native transcript pipeline,
  validating font/layout agreement, streaming, gestures and resource use on LUNA.
- Finish the approved direct connection, host command admission and receipt
  paths. LUNA's current phone command-row writes do not prove host execution.
- Build a native shell, composer and interaction surfaces for real provider
  capabilities, approvals, queue control and correct project/worktree ownership.
- Expand useful phone workflows: Board/Side chats, files/diffs, search, terminal,
  Git, settings/usage/scheduling, share intake and readable rich content.
- Keep dictation, conversational voice, push and widgets explicitly scoped.
  They have different OS, provider and service dependencies. They are not hidden
  exceptions to direct-only V1.

## Release waves

| Wave | Owners | Exit |
| --- | --- | --- |
| Native foundation | IOS-00; CORE-05; RM-00–RM-03 | Owned SDK/FFI, preserved app/data identity, paired native client and authoritative read projections. |
| Complete direct V1 | IOS-01–IOS-04, IOS-09; RM-04–RM-07 | Real phone run/queue/steer/stop/questions/approvals, files/diffs and measured native UI, including actual cellular direct access. |
| Expanded daily workflow | IOS-05–IOS-08, IOS-10; RM-08/RM-09/RM-11 | Board/side work, rich content, search, attention, sharing, terminal, Git and ordinary phone operations. These RM tasks are scheduled after V1, not duplicated. |
| Optional services and audio | IOS-11–IOS-13, SW-16, RM-12 | Selected dictation/voice/awareness requirements with genuine device/provider/service proof. Initially Deferred. |
| Parity/release assessment | IOS-14 | All required rows pass; deferred/unsupported rows stay visible. No “full parity” label while required functionality is absent. |

Browser/device streaming and broad integration control remain under RM-10;
additional routes, Android and browser clients remain RM-13. Those optional
programs are not automatically selected by this iOS refresh. The iOS app must
surface host-installed provider plugins generically, while desktop plugin
binaries remain on their execution host.
