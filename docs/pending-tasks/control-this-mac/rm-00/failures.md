# RM-00 (M0) failure cases

These cases were written before any prototype code. They cover only the M0
boundary:
- a harmless, pinned-TLS descriptor read from a physical iPhone on the LAN;
- a native transport callback that feeds the owned Rust client core.

Pairing, sessions and task operation belong to M1–M3. Each case names how it
is observed and which artifact proves it.

## Host identity and listener (Mac)

| ID | Credible failure | Expected behaviour | Observed by |
| --- | --- | --- | --- |
| H1 | The identity is regenerated on restart, so a phone's pin silently stops matching, or worse, every restart is accepted as "new". | First start creates the key, certificate and target once. Restarts reuse them: same pin, same target. | Probe host started twice on one identity dir prints the same pin and target; `openssl` recomputes the same pin. |
| H2 | A damaged identity is used anyway: key file readable by others, a symlink, corrupt PEM, a target file that doesn't parse, or an incomplete directory that would be silently regenerated. | The host refuses to start with a precise reason and opens no listener. No certificate is persisted: it is re-derived from the key at every start, so a key/certificate mismatch can't occur and renewal keeps the pin. | Probe host exit status and message for each damaged fixture; no port bound. |
| H3 | The host listens on a wildcard address, advertises loopback as a remote URL, or takes the local-owner IPC port. | Binds only an explicit non-loopback interface address. Refuses `0.0.0.0`, `::`, loopback and a port that is already in use (including LUNA Dev's IPC port), without stealing it. | Probe host refusal messages; LUNA Dev keeps its port. |
| H4 | The unauthenticated endpoint leaks data or reaches engine methods. | `GET /remote/v1/health` and `GET /remote/v1/descriptor` return only the protocol, target identity, catalog version and public pin. Other paths are 404, other methods 405, and any request body is refused. No local IPC method is reachable. | Host request log and recorded responses. |
| H5 | Plain HTTP or a non-TLS client reaches application data. | Handshake fails; no HTTP response is produced. | `curl http://` against the port fails; host log shows a failed handshake. |

## Phone trust and identity (native transport plus core)

| ID | Credible failure | Expected behaviour | Observed by |
| --- | --- | --- | --- |
| P1 | The ticket is malformed, oversized, of an unknown version, or has a missing or ill-formed pin. | The core refuses it before any network activity. | Phone result `rejected/ticket`; host sees no connection. |
| P2 | The endpoint is not HTTPS (`http`, `ws`, `file`), or it carries userinfo, a path, a query or a fragment. | The core refuses it before any network activity. | Phone result `rejected/endpoint`; host sees no connection. |
| P3 | The server presents a key other than the pinned one: an accept-all trust callback, or a pin compared incorrectly. | The trust challenge is cancelled. No HTTP request bytes are sent, and the core reports `trustRejected`. | Phone result; host log shows the handshake aborted by the peer and no request from the phone for that case. |
| P4 | The endpoint was swapped to another harmless host (different key) at a reachable address. | Same as P3. | Phone result; host B logs no request. |
| P5 | Same key, different target: for example a key reused across profiles, or a stale pin for another environment. | TLS succeeds, but the core refuses because the descriptor's target differs from the ticket's target. Nothing is shown as Verified. | Phone result `targetMismatch`. |
| P6 | The descriptor has an unsupported protocol or catalog version, or its pin field disagrees with the pin that was checked. | The core refuses with `unsupportedVersion` or `pinMismatch`. | Phone result. |
| P7 | The descriptor body is oversized or malformed, or the status isn't 200. | The core refuses with a bounded error; it never retains the large body. | Phone result. |
| P8 | The route is misreported: a relay, proxy, VPN or cellular path is counted as a LAN read. | The phone records the remote and local addresses, the interface class and the proxy flag. The host records the peer address. They must agree on a direct LAN route. | `URLSessionTaskMetrics` in the phone result plus the host log. |
| P9 | Trust is tied to an address, so a changed address breaks a valid host, or a name match is used as the trust anchor. | Reaching the same host through its mDNS name instead of its IP still verifies, because the pin, not the name, is the anchor. | Phone result for the `.local` endpoint. |

## Native bridge (callback feeds the owned core)

| ID | Credible failure | Expected behaviour | Observed by |
| --- | --- | --- | --- |
| B1 | The Rust core can't drive a Swift-owned transport, or a Swift completion can't re-enter the core. Typical causes: a binding or toolchain incompatibility, or a deadlock between the core's lock and the callback thread. | The core asks the foreign transport for the request; Swift's completion calls back into the core from the URLSession delegate queue; the state settles. | Successful verified case on device; Swift 6 strict-concurrency build. |
| B2 | A late reply from a cancelled request is applied, the classic stale-socket bug. | After `cancel()`, a reply that still arrives is ignored and the state stays `cancelled`. | Phone result for the delayed-descriptor case. |
| B3 | A reply that arrives after the core is released crashes the app or touches freed state. | The completion is dropped safely. | Phone result for the released-core case; the app keeps running. |

## Not exercised in M0 (owned later)

- Local-network permission **denial** recovery (RM-02). M0 records the prompt
  outcome only; it doesn't toggle Settings.
- Certificate renewal, expiry and deliberate key rotation (RM-07 acceptance).
  H1 only proves the identity is stable.
- Camera QR scanning. The M0 ticket is passed as a launch argument; scanning is
  RM-02.
- Any authenticated route, pairing credential or task data (M1+).
