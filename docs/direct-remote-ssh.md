# Direct remote access over SSH (L30)

LUNA uses the system `ssh` command and the machine's existing `known_hosts` trust. The remote engine listens on `127.0.0.1` only. SSH forwards that guarded socket to a temporary local port; it never forwards LUNA's local-owner IPC port. Every forwarded RPC connection must redeem or authenticate an L29 device session, and the engine applies its existing method scopes. Remote task writes remain unavailable where local UI approval is required.

## Package the UI-free engine

Run `scripts/package-headless.sh` from this checkout. It builds `luna-headless` with `--locked` and writes a versioned platform binary and a SHA-256 manifest to `target/headless-artifacts/`. This is a local artifact, with no publication or update feed. Give `luna ssh probe` the binary path; it checks the manifest without executing the binary on the client, checks remote OS/architecture, and verifies checksum and executable version on the remote machine before installing. The remote installation is under `~/.luna/runtime/versions/<version>/`.

The remote host needs a compatible macOS or Linux architecture, system SSH, a POSIX `sh`, `ps`, and `sha256sum` or `shasum`. It needs no Rust compiler or desktop UI. Provider CLIs and credentials remain on that host. LUNA does not copy them.

## Connect an exact environment

The machine ID, environment ID, and workspace scope must come from the owning environment's L29 pairing readback. On a machine with the full LUNA CLI, `luna remote pair --label ... --scope environment-read --scope tasks-read` displays that target. The minimal binary can issue the same pairing from its own local-owner IPC with `luna-headless pair --ipc-port 37654 --label ... --scope environment-read --scope tasks-read` once its engine is running. Keep pairing credentials private.

The L29 environment ID identifies the authenticated profile. Task identity records retain the machine ID as their workspace environment ID; LUNA checks both at their respective boundaries.

After confirming the SSH host key through normal system setup, run:

```sh
luna ssh probe \
  --host my-ssh-alias \
  --machine MACHINE_ID --environment ENVIRONMENT_ID \
  --workspace-scope local \
  --artifact target/headless-artifacts/luna-headless-vVERSION-darwin-arm64 \
  --session-file /private/path/luna-remote-session \
  --pairing-file /private/path/luna-one-time-pairing
```

The pairing file contains the one-time credential from the owning environment's pairing readback and must be a regular mode `0600` file. LUNA redeems it through the forwarded authenticated RPC and creates the session file with mode `0600`. Omit `--pairing-file` on later probes. If you already know the exact target without an issued credential, `--enroll` instead creates a target-checked `environment-read` plus `tasks-read` pairing through remote local-owner IPC; a mismatch revokes the new grant without printing its credential. Add `--project PROJECT_ID --task TASK_ID` together to verify a specific task identity. The CLI fails if the authenticated engine or task does not match the supplied target. `--remote-port` and `--ipc-port` default to `37655` and `37654`; they must be distinct.

The probe reports whether the remote process is `owned` by this client or `reused`. It records an owned launch locally, so a later `luna ssh stop` with the same host, machine, environment, scope, and ports stops only that recorded process. A reused process is left running. If the SSH connection drops, rerun the probe: the new socket authenticates again. An expired or revoked session requires fresh enrollment. A failed stop retains the ownership record for readback rather than claiming completion.

To keep an exact task visible, use `luna ssh view` with the same flags as `probe` plus `--project PROJECT_ID --task TASK_ID`. This opens a small read-only LUNA window showing the target identity and bounded command snapshot. **Disconnect** closes the SSH tunnel without stopping an owned remote engine; **Reconnect** opens a new tunnel and authenticates again. A dropped tunnel changes the window to Disconnected and waits for an explicit reconnect. The full workspace conversation/watch UI and remote task writes are not enabled by this view.

Before a remote start, LUNA persists a private launch intent with the exact target and owner nonce. If SSH loses the start reply, retrying the same command reconciles that nonce with the remote process. A stop or cleanup failure retains the intent so the process is not silently treated as foreign. A different scope, port, version, or artifact for the same target is rejected until the existing ownership is resolved.

The remote endpoint and artifact must be selected by the owner. The local fake-SSH tests exercise forwarding, authentication, mismatches, reconnects, revocation, and process ownership; they are not evidence of a real second-host installation or reachability. Direct private-network use through an SSH host alias follows the same authenticated path. A public listener and the optional VPS relay are outside L30.
