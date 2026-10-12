# RM-01: what counts as a failure

Written before any RM-01 code, on `f8d31c9b` (12 October 2026). These rows
expand RP-02 to RP-06 and RP-19 of the
[remote validation plan](../../t3-remote-mobile/validation-plan.md) for the
engine-owned direct gateway. Owner decisions taken before the code:
- **Order:** authority (A), then the gateway (B), then the RP campaign (C).
- **Issued capabilities are short signed tokens.** They stay valid until
  expiry (60 s at most) even after a revoke. Every RPC call over a socket is
  still checked against the live session.
- **Remote access is off by default,** persisted per profile, and enabled
  through a local-only engine operation and `luna remote enable`.

## A. Authority (store and guard)

| ID | Failure | Required behaviour |
| --- | --- | --- |
| A1 | An upgrade loses paired sessions, half-writes the store, or an older engine misreads a newer store | v1 migrates to v2 in one atomic, private publish, and existing sessions survive. A failed migration leaves v1 intact. An engine that doesn't know the version refuses the store; it never guesses. |
| A2 | A corrupt or foreign store silently turns into an empty one (losing revocations) or keeps remote access dead with no way out | A corrupt store disables remote access with an explicit reason. Only a local-only reset quarantines it (keeping the bytes) and starts empty, which requires re-pairing. Nothing regenerates it implicitly. |
| A3 | A remote caller reaches a local-only method, or a local-only nested intent through a remote-scoped method | Policy is per method, from the CORE-01 catalog. The catalog at `f8d31c9b` gives no remote method nested intents. Its remote commands are only pairing bootstrap and the four `terminal-operate` operations. `ExecuteTaskCommand` and `Mutate` stay entirely local-only, and RP-02 proves the refusal leaves no record. A per-intent allowlist arrives with RM-04, which owns opening task commands. |
| A4 | A remote caller's own `actorId`/`clientId`/`clientSessionId` is trusted | No remote-scoped operation carries actor fields today. RM-04 binds the session principal into task-command admission when it opens those commands, and must overwrite caller-supplied actor fields there. RM-01 records the rule and adds no unused plumbing. |
| A5 | A call targets another device or profile through an authenticated host | Refused before dispatch (exists for `targetDeviceId`; kept). |

## B. Gateway (engine-owned listener)

| ID | Failure | Required behaviour |
| --- | --- | --- |
| B1 | The listener runs when remote access is off, binds every interface or loopback, or shares a port with local IPC | It listens only while enabled, on one explicit non-loopback interface, on a port that is also free on loopback (the RM-00 bind rules). |
| B2 | The TLS identity is regenerated silently, so the pin changes and paired phones trust a new key, or a damaged identity is used | One identity per profile, beside `remote-auth.json`, created once. Damaged, permissive or partial identity files are refused and the gateway reports why. The pin is stable across restarts. |
| B3 | A route accepts a secret in the URL, an oversized body, a browser origin, or proxies to local IPC | Pairing and tickets travel in bodies or headers, never in URLs. Bodies over 16 KiB are refused before they are read. A WebSocket upgrade carrying `Origin` is refused. No route reaches owner IPC. |
| B4 | One remote socket floods the engine | Bounded: 32 connections, header and body limits, a 1 MiB WebSocket message cap, idle timeouts, and at most 32 requests in flight per socket. |
| B5 | Disabling remote access tears sockets down while calls are still being admitted, or leaves sockets open | Disable first closes admission (every guard call denied, no new pair, check or ticket), then closes every socket, then stops the listener. Engine shutdown does the same. |
| B6 | Revoking one session closes the wrong sockets, or leaves that session's socket usable | The revoked session's calls are denied at once, and its sockets close. Other sessions keep working. |
| B7 | A socket ticket is replayed, or used after expiry, or for another session or host | Single use, 30 s or less, HMAC-signed with the profile's key, and bound to the session and the target. Valid until expiry even after a revoke, per the owner decision. The socket it opens still refuses every call of a revoked session. |
| B8 | Local IPC loses functionality, or the headed app's local work is interrupted | Local IPC is untouched, and enable, disable and revoke never stop local tasks. |

## C. RP campaign (verification)

| RP | Journey (deterministic, at the real TLS/WebSocket boundary) |
| --- | --- |
| RP-02 | A paired remote session tries local-only methods and a local-only nested intent: each is refused with no receipt, admission record or side effect. |
| RP-03 | Pair once; replaying the same credential, an expired or cancelled code, a wrong pin, host or profile, or a malformed ticket all fail precisely. A fresh grant recovers. |
| RP-05 | Two sessions are connected. Disable denies both before teardown. Re-enable, then revoke one: only that one is denied and closed, and the other keeps working. |
| RP-06 | Mint a socket ticket before a revoke and use it after: it opens until expiry (policy), and every call is refused. Upload grants and artifact URLs don't exist until RM-06, which inherits this signed-capability policy. |
| RP-19 | Restart: same pin and target, sessions survive, revoked or expired ones stay invalid. Corrupt store: refused, then an explicit reset. A backup restore never revives remote grants (they are excluded from archives). A second engine never owns the profile. |

RP-04 (a lost redemption reply, a Keychain write failure) belongs to RM-02's
phone flow. RM-01 provides the host half: a consumed code stays consumed,
and the minted session is listed so it can be revoked.

**Proof style:** each row has one owner at the strongest boundary (a real TLS
client against the engine's gateway). Where an absence can only be shown with
a sleep, the row says "by construction" instead.
