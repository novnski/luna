# RM-01 milestone C verification

The failure contract in [failures.md](failures.md) precedes the implementation.
This campaign extends the existing engine integration owners; it adds no unit
tests or test-only production seams.

## Authoring gate

| Owner | Observable contract and credible regression | Existing coverage gap |
| --- | --- | --- |
| `remote_access::gateway::remote_access_is_off_until_enabled_then_admits_only_its_paired_session` | A valid local task command and workspace mutation are refused over pinned TLS/WebSocket, with no durable receipt, executable admission or title change. The same command works locally. Dispatch before the guard would violate this. | The existing refusal used an empty command body and never inspected durable state; it could pass because decoding failed. |
| Same gateway owner | Wrong host/profile/protocol, malformed body, cancelled and expired one-time pairing credentials fail with their actual HTTP reason; failed target attempts do not consume the valid credential, and fresh pairing recovers. | The existing transport campaign checked only reuse and wrong TLS pin. The memory-client authority suite cannot guard HTTP decoding or target propagation. |
| Same gateway owner | A changed HMAC signature is refused at the HTTP upgrade without consuming the original ticket; the original still upgrades once. | Review found milestone B used only opaque random tickets, contrary to the owner's selected short signed capabilities and failure B7. The repair signs the nonce, exact target, session and expiry with an engine-epoch key owned by that profile's authority store. The key and outstanding ticket inventory stay in memory, so restart invalidates outstanding tickets as before; paired device sessions retain their existing durable lifecycle. |
| `remote_access::gateway::revoke_closes_only_its_session_and_disable_closes_everything` | A ticket minted before revocation still upgrades once during its bounded lifetime, then the revoked session cannot call. The other session and local engine remain usable. | The existing campaign revoked an already-open socket without redeeming an outstanding ticket. A change to ticket redemption or socket admission could violate the agreed capability policy. |
| `remote_access::gateway::remote_access_resumes_after_restart_with_the_same_pin` | Restart retains the exact target/pin and admits only the still-valid session; a one-second expired session stays refused. | The existing restart owner covered live and revoked sessions only. The memory-client expiry test cannot catch a restart that resurrects persisted expired sessions. |
| `backup_restore::managed_offline_restore_preserves_excluded_files_and_keeps_rollback` | Authority, listener settings and TLS identity are absent from the archive, and newer excluded bytes survive restoration rather than resurrecting old grants/keys/config. | The owner covered only unchanged `remote-auth.json`; it did not exercise the two newly introduced paths or newer post-backup authority. |

Expiry uses the public one-second lifetime and its issued wall-clock deadline;
the timer is the contract under test, not a substitute for a lifecycle event.
Gateway assertions use a real LAN interface, pinned TLS, the engine service and
its actual store. `LUNA_REMOTE_TEST_IP` optionally selects the actual LAN
interface when a VPN owns the default route; production bind rules still
reject loopback and wildcard addresses. A firewall skip is recorded separately and earns no gateway
acceptance. Linux CI supplies executable LAN evidence when the Mac blocks an
unapproved integration-test binary. Native LUNA Dev readback verifies the
headed owner, process, port, signature and enable/disable lifecycle separately.

## Evidence

The committed [manifest](evidence/c-manifest.json) records commands, exit codes,
counts, hashes and the app handoff. Verbose logs remain in
`target/qa/control-this-mac/rm01-c-01a130a0/`; hashes cover the retained raw files.
No pairing credentials, bearers, socket tickets or private TLS material appear
in the published evidence. The unchanged v2 on-disk store needs no migration
for signing; `hmac 0.12.1` was already resolved and its direct dependency is the
only lockfile change.

Repeat the real-interface engine campaign, replacing the address with the
current LAN interface if needed:

```sh
CARGO_INCREMENTAL=0 LUNA_REMOTE_TEST_IP=192.168.18.2 \
  scripts/cargo.sh test --locked -p luna-engine \
    --test remote_access --test remote_auth --test backup_restore \
    --test local_profiles -- --nocapture
scripts/cargo.sh test --locked -p luna-rpc --features schema
```

The native owner has a separate repeatable command:

```sh
agent-python scripts/qa-rm01-headed.py --bind 192.168.18.2:7744 \
  --ipc-port 59777 --output target/qa/control-this-mac/rm01-headed-repeat
```

It requires a running LUNA Dev with remote access disabled and a free chosen
LAN port. It waits for IPC startup, pins TLS before transmitting a credential,
creates two short QA sessions, revokes them and disables access in cleanup.
It emits sanitized `native-readback.json`; no credentials are printed. It
preserves existing paired sessions and task data. macOS may require a firewall
permission for the bundle on another machine; a refused route is a failed
readback, never phone/network acceptance.

The signed-ticket regression was red before the runtime repair: the gateway
returned `socket ticket unknown or already used` instead of verifying a
signature. The old headed app also failed the signed-capability readback, with
cleanup leaving access disabled. Both are retained separately from passing
acceptance. The early default-route run skipped three gateway tests and is
excluded from accepted evidence. Current explicit-LAN runs have zero skips.

Review inspected the final diff against the authoring gate: one integration
owner per contract, independently valid negative inputs, real store/transport
and restore paths, no new unit tests, no production test seams and no secrets.
The native owner independently proves the bundled executable and headed IPC
path. No physical-phone, Keychain, upload/asset or outside-network acceptance
is claimed; RP-04's phone half and RM-06's capabilities retain their owners.
