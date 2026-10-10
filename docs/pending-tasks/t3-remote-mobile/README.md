# T3 remote and mobile research for LUNA

**Shared tracking:** [queue and current status](../README.md) · [agent workflow](../WORKFLOW.md).
Queue ownership: RM-00–RM-13. This package supplements Control this Mac; the catalog derives M0–M4 from the same RM tickets. Status statements below describe the planning/delivery baseline; use the CLI for live claims and progress.

**Status:** source research complete; implementation pending. Prepared on 11 October 2026, Europe/Bucharest. This package implements no remote feature and certifies no running mobile or host deployment.

T3's main advantage is an environment-centered remote product: the machine runs agents and owns files, terminals and durable work; native mobile, browser and desktop clients send scoped commands and follow authoritative state through a common client runtime. LUNA already has many of the backend pieces and an iOS transcript client. Its main missing connection is a complete direct, paired-Mac product with host-admitted task operations and the phone screens that expose existing host capabilities.

## Reading order

1. Read [the gap matrix](gap-matrix.md) for what exists, what is partial, and what needs work.
2. Read [implementation tasks](implementation-plan.md) for dependencies, source ownership and completion gates.
3. Read [E2E acceptance](validation-plan.md) before implementing any milestone.
4. Use the detailed source reports below when implementing a capability. They preserve mechanisms, restrictions, references and failure cases.
5. Read [verification and limitations](verification.md), [independent review](10-independent-review.md), and [the implementation handover](HANDOVER.md).

## Baseline and release boundaries

| Item | Inspected evidence | Interpretation |
| --- | --- | --- |
| T3 source | pingdotgg/t3code at a8bdfdb54181fb5c9d43f2747112c137a804d016; fetched origin/main, then inspected an isolated Git archive | Fresh upstream source at the research boundary; existing local T3 checkout and its untracked research were preserved. |
| LUNA source | novnski/luna at 4f3a6b0cbe2603db7b18f7d452d3a5dfdecb183c | Current main before saving this package; existing pending docs are planning evidence, not delivered features. |
| Public iOS listing | [App Store](https://apps.apple.com/us/app/t3-code-remote-claude-more/id6787819824) showed 1.4.1 and iOS/iPadOS 18 minimum on the research date | Confirms public listing availability, not a feature-by-feature installed-binary test. |
| Public Android listing | [Google Play](https://play.google.com/store/apps/details?id=com.t3tools.t3code), linked by [official download page](https://t3.codes/download) | Confirms the listing exists; availability by account, region and beta enrollment was not tested. |
| T3 mobile source | app.config.ts declares 2.0.0; install guide says store clients cannot connect to Nightly and require TestFlight/Android beta | All detailed feature claims describe the pinned upstream source. Do not attribute the entire inventory to stable 1.4.1. See [platform/release evidence](07-mobile-platform.md). |
| Runtime proof | No host, mobile app, simulator, provider turn, account, packet capture or notification service was exercised | Live usability, delivery latency, production permissions and store-build parity remain unverified. |

The old mobile README warning that the app is not distributed is stale relative to the store listings and current install guide. The source's individual Apple voice transcription feature requires a supported iOS 26+ device and locale; that is separate from the public app's iOS 18 minimum.

## What T3 actually does

A phone saves an environment identity, routes and credentials. Direct pairing exchanges a short-lived one-use code for a separate scoped session. Native credentials live in secure storage. HTTP handles bootstrap, descriptors, snapshots and assets; authenticated WebSocket RPC carries operations and streams. Server authorization precedes command admission. Durable command receipts, events, projections and effect outbox entries commit before the host starts provider work. The phone can cache state and queue selected intents, but a successful send/receipt is distinct from completion.

T3 supports direct LAN/public addresses, Tailscale IP/Serve, desktop-managed SSH and T3 Connect. T3 Connect brokers identity and tunnel allocations through a relay Worker; ordinary application traffic traverses the environment's Cloudflare tunnel hostname. “The Worker is outside the ordinary data path” does not mean that Connect avoids cloud infrastructure or has application-level end-to-end encryption.

Its native mobile source exposes task creation/control, provider options, approvals/questions, attachments/context, thread lifecycle, terminal sessions, read-only workspace files, local Git diff/review, commit/push/PR creation, browser and simulator sessions, usage, scheduled tasks and host settings/updates. These features are subject to host capabilities and scopes; the reports identify concrete exceptions.

## What LUNA needs first

1. A headed Mac host gateway with pinned TLS, pairing, scoped principals, host identity, device/session revocation and accurate availability.
2. A direct iOS connection path with a saved host, secure credentials, snapshots/streams, stale-cache presentation and lifecycle recovery.
3. Authorized project/task operations through engine admission and durable receipts. Synced command rows and a visible Send button cannot substitute for this.
4. Approval UI, effective provider/permission readback, task-bound attachment transfer and file/diff screens.
5. Physical-device proof of the complete journey, including lost replies, stale approvals, revocation, backgrounding and a real off-network direct route.

The [existing Control this Mac plan](../control-this-mac/README.md) owns M0–M4 and its direct-only V1 boundary. This research expands its inventory and acceptance detail. It does not silently replace that architecture or mark its milestones complete. Tailscale, Cloudflare tunnels, hosted account discovery, push services, Android and browser companions remain separately scoped later options. V1 must work with a local-only LUNA profile and no undeclared relay or hosted sync.

## Detailed research reports

| Report | Scope |
| --- | --- |
| [01 — T3 architecture](01-architecture.md) | Command-to-provider-to-render flow, execution ownership, server/desktop/service lifecycle, protocols and subscriptions. |
| [02 — Authentication](02-auth.md) | One-use pairing, bearer/DPoP/cookies, scopes, administration, revocation and asset URL exceptions. |
| [03 — Transports](03-transports.md) | LAN, public HTTPS, Tailscale, SSH, Connect control/data planes, route selection and prerequisites. |
| [04 — Mobile connection runtime](04-mobile-runtime.md) | Onboarding, identity, routes, secure storage, SQLite caches, reconnect and compatibility. |
| [05 — Mobile workflows](05-mobile-workflows.md) | Reachable navigation, task/project/thread actions, approvals, composer, settings, usage and scheduling. |
| [06 — Mobile developer tools](06-mobile-tools.md) | Terminal, files, Git/diffs, browser streaming, device sessions and explicit mobile limitations. |
| [07 — Mobile platform](07-mobile-platform.md) | Push route, Live Activities/widgets, Android native notifications, voice/share/extensions, updates and shipping requirements. |
| [08 — LUNA backend](08-luna-backend.md) | Existing RPC/authority/sync/SSH foundations, admission restrictions and direct-host gaps. |
| [09 — LUNA iOS](09-luna-mobile.md) | Actual SwiftUI routes, edge topology, rendering, controls, missing screens and migration needs. |
| [10 — Independent review](10-independent-review.md) | Counterpart checks, corrections, contradictions and outstanding runtime questions. |

## Important limits discovered during cross-checking

- Pairing establishes authority; it creates no network route and cannot wake a stopped host.
- T3 mobile production incoming-link pairing prefill/auto-connect is disabled; QR/manual submission is a separate supported path.
- Mobile cannot establish T3's desktop-managed SSH tunnel itself.
- Descriptor checks reduce wrong-host credential exposure, but T3's ten-second same-URL identity cache prevents an absolute guarantee for an address reassigned inside that window.
- Removing an environment on mobile forgets local state; server session revocation is a separate operation.
- T3 session revocation ends session-authenticated access and sockets; already minted signed asset URLs can survive until their own expiry.
- T3 mobile file browsing is read-only. Shared write RPCs do not prove a phone editor.
- Mobile local Git diff review is distinct from a hosted PR Code tab; general host desktop sharing was not found. Browser/device sessions have their own authority.
- LUNA's iOS edge command-writing paths are source-visible, but exact host admission and provider execution were not demonstrated. Direct task mutation is currently denied by the paired remote method policy.

## Evidence and maintenance

The [repeatable read-only verifier](verify-evidence.py) checks the package against the pinned Git objects. [evidence-manifest.json](evidence-manifest.json) records source-file hashes, document hashes, public checks and the ten research lanes. [Verification](verification.md) records actual checks and explicitly separates them from future E2E. Re-run baseline inspection before implementation: new source may close gaps or change call sites. Preserve the existing feature owners and extend their tests at the strongest real boundary instead of building parallel implementations.
