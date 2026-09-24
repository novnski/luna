# LUNA remote authority (L29)

LUNA's existing device-room RPC dispatch now requires a paired device session for each remote connection. The edge token and relay route carry frames but do not grant RPC authority. Local in-process clients and the loopback IPC port remain the trusted local-user boundary; SSH forwarding of that IPC port is **not** a supported remote route. A future direct transport must construct `RemoteRpcService` for each connection, as the device-room host does.

## Pair and revoke

Run these against the **local running engine**:

```text
luna remote pair --label "My phone" --scope environment-read --scope tasks-read
luna remote pairings
luna remote sessions
luna remote revoke-pairing <pairing-id>
luna remote revoke-session <session-id>
```

`pair` prints a random credential once, the target machine ID, profile-specific environment ID, workspace scope, protocol version, and capability. Treat it as a secret and transfer it only through a channel you trust. The pairing expires after five minutes by default and can be redeemed once. A successful redemption returns a distinct device-session token. The client must authenticate every new connection with that token and the exact target identity, protocol version, and `remote-auth-v1` capability. Authentication does not survive reconnect by inference. Revocation and expiry are checked on every call and on active streams; revoking a session ends its streams and denies later calls and reconnects. The host persists only credential hashes under its selected profile root in a private `remote-auth.json` file.

The CLI accepts `environment-read`, `tasks-read`, `files-read`, and `terminal-operate`. The guard explicitly maps shipped RPC methods to these scopes. Task mutations, account/credential operations, backup control, updates, local draft/attention actions, and pairing/session administration remain local only; a new method is denied until added to the inventory and policy. No `desktop.control` or `automations.manage` grant exists. Remote L25 cross-task writes remain unavailable.

The existing relay client cache needs an explicit target-bound `remote_session` source before it will dial a peer. L29 provides the typed `RpcClient::redeem_remote_pairing` and `RpcClient::authenticate_remote` calls but does not persist outbound client tokens or provide the L30 SSH route or L31 VPS relay. The legacy relay nudge is ignored because it carries no device-session proof. Pairing, revocation, and CLI management are available locally; remote transport setup and deployment remain later steps.

If the authority file is damaged or has unsafe permissions, remote hosting and pairing are disabled for that profile; local in-process and IPC work remains available. The current offline backup excludes the authority file, so restoring task data does not restore device sessions. Historical unscoped relay task/file writes and command nudges are unavailable under this policy.
