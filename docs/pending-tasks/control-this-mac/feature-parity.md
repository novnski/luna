# Native remote features and browser follow up

This is a proposed product inventory, not a report of implemented features. The user's target is the strongest useful remote client assembled from LUNA's native lineage, Zeron's newer mobile implementation, T3's mobile workflows, and BB's browser experience.

The table seeds the inventory. Before M0 completes, inspect every screen, route, composer action, settings entry, and supported remote operation in the pinned reference apps. Add missing rows; record the source symbol and engine capability. This seed is not an exhaustive claim about what those apps ship.

## Architecture references

| Reference | Reuse approach | Boundary |
| --- | --- | --- |
| [Existing LUNA iOS client](../../../apps/ios/README.md) | Keep app ownership, relevant interactions and data migration knowledge. | Its current edge-room command writer is not the new direct host API. |
| [Newer Zeron UIKit and Rust core](https://github.com/novnski/zeron/blob/b42fc2b8fbf247dd92796c2917f277535cea91ac/docs/mobile-rewrite.md) | Port native shell, rendering and projection architecture into owned modules. | Replace cloud identity, room connections, relay RPC and upstream service defaults. |
| [T3 mobile package](https://github.com/pingdotgg/t3code/blob/5cc99e1c23980d7995a13c47f969b47cb68ed1be/apps/mobile/package.json) and [shared RPC client](https://github.com/pingdotgg/t3code/blob/5cc99e1c23980d7995a13c47f969b47cb68ed1be/packages/client-runtime/src/rpc/client.ts) | Inventory workflows and their dependency on host operations; port behavior to native LUNA screens. | React Native UI and T3's Effect RPC product contracts are not drop-in LUNA components. |
| [T3 local sessions](https://github.com/pingdotgg/t3code/blob/5cc99e1c23980d7995a13c47f969b47cb68ed1be/docs/internals/environment-auth.md) | Use scoped host authority, one-time pairing and platform-specific session adapters as references. | Its managed Connect service is separate and excluded from V1. |
| [BB browser model](https://github.com/get-bb/bb/blob/2573ad5e85835dc1c90aa33795e6dba52ef5aa92/docs/multiple-devices.md) | Use persistent work, streaming and reconnection as browser behavior references. | BB Connect's cloud forwarding is excluded from V1. |

## First native release and later parity

M0–M4 define the first release. Later rows remain part of the desired product backlog; their deferral must stay visible. A release claiming full reference-app parity requires the completed inventory and acceptance for every applicable row, not only the rows below.

| ID | Feature | Required host contract or policy | Target |
| --- | --- | --- | --- |
| RC01 | Control this Mac settings | Local authority roster, enable gate, Add, refresh, revoke and power state. | M1 |
| RC02 | QR pairing and saved Mac | Exact target and TLS trust, single-use credential, device session in Keychain. | M1 |
| RC03 | Honest connectivity | Authenticated readback, stale cache marking, reconnect and session expiry/revocation. | M1–M2 |
| RC04 | Project list and existing tasks | Host-owned project/thread projections, sorting, archive state, current profile. | M2 |
| RC05 | Streaming transcript | Incremental messages, rich tool summaries, lazy bodies, status and stable ordering. | M2–M4 |
| RC06 | Start a project-bound task | Typed target/config and durable creation receipt; no phone-local agent. | M3 |
| RC07 | Send follow-up messages | Idempotent admission, optimistic echo settled by exact host receipt. | M3 |
| RC08 | Queue, edit, remove and reorder | Host leases/revisions and acknowledgement; preserve attachment ownership and blocked-head policy. | M3 |
| RC09 | Steer or interrupt/stop | Provider capability and active-turn checks; explicit stale-turn behavior. | M3 |
| RC10 | Answer questions and approvals | Exact outstanding request/revision and host permission policy; reject replay. | M3 |
| RC11 | Model, effort and permission choices | Host-advertised provider catalogs and allowed task config; preserve account-specific model identity. | M3–M4 |
| RC12 | Photos and file attachments | Native picker, bounded task-bound upload, hashes, retry and cancellation. | M3–M4 |
| RC13 | Files, images and artifacts | Workspace/explicit-artifact grants, path checks, lazy download and native preview. | M4 |
| RC14 | Changed files and diffs | Exact checkout/task association and revision-aware read projections. | M4 |
| RC15 | Search, pinned/history/archive views | Host query contract and local preferences; do not derive ownership from sidebar position. | M4/basic; broader M5 |
| RC16 | Keep host available | Shared power reasons, external-power observation, accurate failure and quit behavior. | M4 |
| RC17 | Direct public-domain setup | Explicit endpoint and routing, TLS target verification, actual off-network readback. | M4 |
| RC18 | Mobile transcript polish | Virtualization, gesture anchoring, streaming/coalescing, keyboard and accessibility. | M4 |
| RC19 | Terminals | Separate terminal grant and explicit session ownership/lifecycle. | M5 unless separately included in release scope |
| RC20 | Rich source-control operations | Branch/worktree/commit/PR actions with owned backend contracts and separate destructive-action policy. | M5 |
| RC21 | Fork, revert, review and imports | Current LUNA capabilities, irreversible-action semantics, exact history identity. | M5; reuse the host's existing fork, rewind and review operations |
| RC22 | Full provider/settings/usage workflows | Capability-specific metadata and host mutations; secrets and account administration stay local unless deliberately designed. | M5 |
| RC23 | Audio/voice and additional media | Platform capture and format support, bounded upload, provider support. | M5 |
| RC24 | Device preview, Computer Use or browser control | Explicit separate grants, host OS permission and visible ownership/readback. | M5, independently reviewed |
| RC25 | Notifications | Foreground updates in V1; APNs/other external push only after an explicit product decision. | Foreground M4; push M5 |
| RC26 | Browser companion | Responsive UI, trusted HTTPS, local browser sessions, CSRF/origin policy and recovery. | M5 |
| RC27 | Native Android | Platform shell and transport integration against the shared client; verify actual core portability. | M5 |
| RC28 | Optional Tailscale or relay | Separate owner-selected route, transparent connection mode, unchanged engine authority. | Future; absent from V1 |

## Per feature record

For each row, store the reference repository and commit, exact user trigger, screen/entry points, source symbols, required host method or projection, scope, capability/version, target identity, offline behavior, idempotency/revision behavior, native implementation, browser equivalent, and linked E2E evidence.

Record whether behavior exists in LUNA today, is restricted by the remote policy, or needs a new backend operation. Separate source evidence, observed UI behavior, and proposed work. Do not mark a feature complete from a visible control, a successful socket, a static fixture, or a shared TypeScript/Rust schema alone.

Codex and Claude parity and persistent child chats are merged; expose their existing host operations instead of building remote-only versions. Link overlapping work to [OpenCode parity](../opencode-feature-parity/README.md) while it remains open. This remote plan does not authorize implementing unrelated provider gaps. When a required operation is unavailable, show its absence through capability negotiation and preserve the backlog entry.

## Native and browser compatibility

Both clients use the same Mac host identity, task store, admission policy, and outcomes. Their credentials and presentation remain platform-specific. The phone uses Keychain sessions and native trust established by pairing. The browser uses trusted HTTPS and host-issued HTTP-only sessions; it receives a separate grant.

Browser access is especially useful from another device after a phone is lost. It still requires a reachable host and an independent enrollment or recovery credential. Provide locally issued recovery material only after defining its scope, expiry, revocation, and rate limiting; do not put a universal administrator token in a public URL.

Keep the domain optional. A nearby native phone should not need the domain, certificate authority account, cloud sign-in, or Tailscale installation. A public browser URL must never serve an unauthenticated engine API.

## Platform and lifecycle limits

The native app improves input, media handling, secure credential storage and UI performance. It cannot keep the Mac alive after app quit, promise persistent background socket execution on iOS, or remove an ISP routing restriction.

The Zeron Rust core prepares an Android path but does not prove Android parity in this inspected source. T3 already has a React Native cross-platform structure; importing it wholesale is a different architecture decision from porting its feature set. If Android becomes a first-release requirement, reassess that tradeoff before committing the complete UIKit migration.

Push notifications require a separate route through Apple's service on iOS. They are not included as a hidden exception to V1's direct-only remote-control design. Likewise, generic desktop automation carries a different permission boundary from steering an existing coding task.

## Parity completion gate

Full parity is achieved only after every applicable reference feature is mapped, implemented or explicitly accepted as unsupported, and verified on the relevant live client/host path. Keep evidence for negative states as well as successful actions. The initial first release deliberately delivers the complete core task-control journey; later milestones deliver the broader remote application.
