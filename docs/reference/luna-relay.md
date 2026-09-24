# Owner-run LUNA relay (L31 local slice)

`luna-relay` carries the existing L29 typed RPC between an outbound environment
and an authorized client. The environment remains authoritative. The relay
has no public signup, accounts, database, workspace copy, or general port proxy.
The `host` sidecar connects only to the explicitly selected **guarded remote
RPC** loopback port; it probes that socket and rejects a local-owner IPC port.
Every client bridge opens a fresh guarded socket, so each client must redeem a
one-time L29 pairing or authenticate its own revocable L29 device session.
The route credential alone grants no RPC scope. L25 approvals and L29's
remote-write restrictions remain local-only.

## Private local setup

These commands describe the setup; no owner endpoint is selected or deployed
by L31. Build with `cargo build --locked -p luna-relay`. Keep the relay files in
an owner-only directory (`0700`) and each config/secret/target file at `0600`.
Run `luna-relay generate-secret` separately for the host and **each** client,
then place each result in its private secret file. The tool prints a secret
once; it does not write one into a shell history argument. Do not reuse a host
secret for a client. The server needs the same values in its private config.

Create a config with one exact route (the values below are placeholders):

```json
{
  "version": 1,
  "routes": [{
    "id": "my-environment",
    "target": {
      "deviceId": "exact-device-id",
      "workspaceScope": "development",
      "environmentId": "exact-environment-id",
      "protocolVersion": 1,
      "requiredCapabilities": ["remote-auth-v1"]
    },
    "hostSecret": "HOST_64_HEX_CHARACTERS_FROM_GENERATE_SECRET",
    "clients": [{"id": "my-client", "secret": "CLIENT_64_HEX_CHARACTERS_FROM_GENERATE_SECRET"}]
  }]
}
```

The private target file contains only the `target` object above. Copy exact
identity and capability values from the environment's L29 pairing readback;
do not guess them. For a synthetic development environment, run
`luna-headless` with distinct `--ipc-port` and `--remote-port`. For an owned
real environment, start its normal headless engine with those same separate
ports. L29 pairing remains an explicit action on that environment; its
credential is handed to the client through an owner-approved channel and is
never stored at the relay.

On the relay machine, run `luna-relay serve --config private-relay.json`
(default bind `127.0.0.1:37670`). On the environment, run:

```text
luna-relay host --server wss://YOUR_OWNED_ENDPOINT --route my-environment --target-file private-target.json --host-secret-file private-host.key --guarded-rpc-port 37655
```

On the client, run:

```text
luna-relay client --server wss://YOUR_OWNED_ENDPOINT --route my-environment --target-file private-target.json --client-id my-client --client-secret-file private-client.key --bind 127.0.0.1:37671
```

Typed LUNA RPC clients can then connect to `ws://127.0.0.1:37671` and perform
normal L29 pairing/session authentication. Each WebSocket is independent. The
forwarder rejects browser `Origin` handshakes. No UI connection is implied by
starting the process alone.

## Operations and trust boundary

The relay and client forwarder bind loopback. `ws://` to a relay is accepted
only for a literal loopback IP in local tests. An external deployment needs an
owner-selected HTTPS endpoint with valid TLS and WebSocket upgrade forwarding
to the relay's loopback listener; `host` and `client` then use `wss://`.
TLS terminates at that VPS proxy: **the VPS operator and TLS terminator can
read RPC traffic**. This is not end-to-end encryption. Configure the proxy to
avoid body, URL, and credential logging, restrict its bind and firewall, and
protect its TLS keys. L31 does not configure or deploy that proxy.

The relay config is reread about once per second. Removing a client key,
rotating a secret, or removing a route closes all active bridges and host
tunnels on reload; still-authorized peers may reconnect. Revoke the L29
device session separately on the environment to end its RPC authority,
including if another route later appears. A malformed, missing, symlinked,
or non-private config fails closed until repaired. Restart the sidecars after
changing their private secret files. The host makes at most eight reconnect
attempts per outage, with capped backoff; a service manager can restart it
after operator review. A new host generation fences old pending bridges.

Limits are 32 configured routes, 32 client credentials per route, 16 active
bridges per route, 128 accepted relay sockets, 8 MiB per WebSocket message,
five-second stalled-send timeouts, an eight-second bridge setup timeout,
and heartbeat expiry. Large RPC payloads above the message limit fail closed.
No relay files contain provider credentials, task transcripts, or workspaces.

L31 was verified with local synthetic sockets and processes. There is no
chosen second host, real VPS deployment, external TLS test, or E2EE claim.
