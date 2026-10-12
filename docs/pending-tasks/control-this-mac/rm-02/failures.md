# RM-02 failure contract and authoring gate

Written on baseline `a31db907` before code. Existing engine RPC authority and
RM-01 gateway tests remain their contract owners; this campaign adds transport,
platform persistence and UI risks that those tests cannot reach.

| ID / owner | Credible failure | Required observable behavior |
| --- | --- | --- |
| D1 / desktop native UI | Registry online status or a configured listener is mistaken for remote authority/reachability | Connections reads the active engine authority, displays actual host readiness, granted sessions/scopes and last authenticated use; phone route/authentication remains separately observed. Devices keeps registry semantics. |
| D2 / desktop → real engine | Repeated Add, stale engine replies, cancellation, expiry or host disable leaks a usable bootstrap | One current sheet and bounded one-use grant; cancellation/disable revoke the sheet's grant, expiry is explicit, stale replies cannot reopen it. Secrets stay out of logs, diagnostics and screenshots. |
| D3 / desktop → real engine | Revoke affects another session or local task; errors are hidden behind optimistic controls | Await the exact host result, read back authority, surface failure; only selected authority is revoked, accepted local work/history remain. Refresh is read-only. |
| D4 / desktop power owner | Remote preference replaces another task's power reason or claims sleep protection on battery/failure | Persist the separate opt-in, combine reasons through the existing coordinator, require ready hosting and external power, release its reason on disable/quit, expose actual unavailable/failure state. Full availability acceptance stays RM-07. |
| P1 / native transport + shared core | Unsafe/oversized/malformed/unsupported payload, endpoint credentials/query/fragment, wrong pin/host/profile/capability spends a credential | QR and manual entry use one bounded validator; pin and descriptor/target are verified before redemption. Invalid input consumes no grant and produces a precise safe recovery state. |
| P2 / native transport + host store | Expired/cancelled/reused pairing or two concurrent redemptions creates false/double authority | Exact host refusal, no false paired/connected state, recovery requires a fresh authorized grant. The current operation owns cancellation; late replies cannot mutate a later operation. |
| P3 / native Keychain + lifecycle | Plain preferences store a bearer, Keychain fails, or crash/restart loses identity or resurrects trust | Bearer only in app/host/profile scoped Keychain, device-only accessibility and no synchronization. Metadata contains no secret. Pairing is committed only after successful storage. Restart reauthenticates exact target; expired/revoked/missing credentials require explicit re-pair. Preserve existing edge account and user data. |
| P4 / pinned physical transport | Redemption reply is lost after consumption, cancellation wins, or storage fails after minting | Uncertain/re-pair result, no silent second redemption or success claim. Host inventory explains an unused session, which can be revoked; a fresh grant recovers. Distinguish scripted storage error from actual OS refusal. |
| P5 / physical UI | Camera unsupported/denied, LAN permission refused or incoming link triggers automatic pairing | Manual fallback and actionable refusal; explicit confirmation before contacting the host; no automatic incoming-link connection. Do not claim a camera capture from a manual-entry run. |
| P6 / physical restart + host authority | Forget is described as server revoke or removes another host's secrets/data | Forget clears only the selected local host/profile credential/cache. Separate host revoke denies future authentication and closes its socket. Other grants and legacy edge data survive. |
| P7 / native TLS/WebSocket | TLS-only or open socket is shown Connected, stale late response targets a new host | Connected requires successful host-authenticated readback for the exact selected host/session; generation fencing ignores old callbacks. Record actual route metrics, never claim Internet/phone reachability from Mac readiness. |

## Why these are the strongest owners

- D1–D4 require the shipped settings controls, real engine results and native
  power state; a table of UI labels or private predicates is insufficient.
- P1/P2/P4/P7 require real pinned HTTP/WebSocket exchanges and host consumption;
  a mock returning the expected receipt is insufficient. Reuse the M0 transport
  validator and RM-01 host service rather than replaying their private logic.
- P3/P5/P6 require the platform's Keychain, OS/UI and actual app lifecycle. Shared
  core callback failure coverage cannot certify physical Keychain or permission
  behavior. No test-only provider, entitlement, persistence or network bypass is
  added to production.

Each accepted run retains command/exit code, source/app identity, route class,
observed outcomes and hashes. Raw QR, bootstrap, bearer and socket-ticket values
are private transient input and never published in Git, logs, screenshots or
clipboard history. HTTP URLs carry no credential. Physical camera, manual
pairing, simulator and controlled network faults are labeled separately.
