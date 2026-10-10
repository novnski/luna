# Research verification and remaining limits

Prepared 11 October 2026, Europe/Bucharest. This verifies a source research package and its evidence, not the remote feature.

## What was checked

Final candidate validation initially resolved 676 pinned source-link occurrences across 219 unique source files and 46 local links, with zero missing files or invalid line bounds. The finalized manifest records the current counts. This is mechanical evidence QA; semantic review is separately recorded below.

Exactly ten research agents covered architecture, auth, transport, mobile runtime, workflows, developer tools, native platform, LUNA backend, LUNA iOS and independent skeptical review. Each report cross-checks important claims against implementation and a separate contract/caller/docs/test counterpart where available. Existing tests were not executed.

The parent fetched T3 origin/main and created a Git archive of a8bdfdb54181fb5c9d43f2747112c137a804d016 outside both repositories. The original checkout remains at its prior HEAD; existing untracked T3 research was preserved. LUNA comparison was read-only at4f3a6b0cbe2603db7b18f7d452d3a5dfdecb183c before the documentation change.

The independent reviewer checked source reports and the combined candidate package before publication. The parent also checked pinned source file/line targets, relative document links, and source/document hashes. See evidence-manifest.json for machine-readable counts and checks. All sixteen Markdown documents and the read-only verifier are hashed. Only the self-referential manifest is excluded from its own hash.

Public pages opened: [official download](https://t3.codes/download), [App Store](https://apps.apple.com/us/app/t3-code-remote-claude-more/id6787819824), [Google Play](https://play.google.com/store/apps/details?id=com.t3tools.t3code). Availability and iOS1.4.1 were observed; installed-binary source parity was not.

## Corrections made before publication

1. Stale mobile README distribution warning was qualified by actual public listings and current install guide.
2. Upstream mobile2.0.0 was separated from stable iOS1.4.1; Nightly requires mobile beta.
3. Tailscale Serve was kept outside LUNA's existing no-relay V1; HTTPS proxy does not prove relay-free underlay.
4. Connect relay Worker was separated from Cloudflare tunnel data plane; no unsupported application-level E2E encryption claim.
5. Public descriptor checks were qualified by the ten-second authorization cache and possible address-reassignment race.
6. Proactive return-to-preferred-route was limited to direct routes; SSH/Connect use ordinary failover/reconnect.
7. Mobile Remove was separated from server revoke; signed asset URLs retain their own expiry after session revocation.
8. Mobile file view was recorded read-only; Git local diff/PR creation was separated from full hosted PR review.
9. Browser/simulator session control was separated from generic Mac desktop sharing; mobile SSH provisioning was not claimed.
10. LUNA phone UI/synced rows were separated from admitted task execution. Authenticated actor binding, local executable-grant protection and terminal ownership remain implementation requirements.

## Not verified and not represented as complete

- Any installed T3/LUNA iOS/Android build, user account, relay/tunnel deployment, provider execution or live phone UI.
- Feature-level parity between stable/mobile beta/source, reconnect latency, resource use or real network route choice.
- Physical TLS/cellular/LAN/revocation/approval/queue/attachment journeys or deployed APNs/FCM delivery.
- Cloud tunnel pricing/operational capacity, platform availability by country/account, full accessibility or Android voice support.
- Completion of Control this Mac M0–M4 or any RM task; no production source/configuration was changed.

The audit helper passed against both real Git repositories: 219 source hashes, 676 citation ranges, sixteen Markdown hashes and all local links; zero errors. [Retained result](evidence-check.json). No Rust/app build or LUNA Dev refresh is needed for documentation-only planning, as the existing Control this Mac implementation guide explicitly states. git diff --check and evidence/link validation are the applicable checks. Native E2E/build work is reserved for implementation.

## Rechecking

Run the saved [read-only evidence verifier](verify-evidence.py) from the LUNA root with agent-python docs/pending-tasks/t3-remote-mobile/verify-evidence.py. It uses Git objects already present in the LUNA and sibling t3code checkouts; --luna-root and --t3-root override those paths. It performs no network request or mutation. It verifies source/document hashes, citation bounds and relative links. Resolve each GitHub blob permalink against its pinned tree when reviewing the surrounding source. Resolve relative links from this package in the LUNA checkout. Re-run git diff --check for the changed docs. A fresh semantic audit is needed when updating claims; link/hash validation alone does not prove the sentence accurately describes its source.
