# LUNA

A native workspace for coding agents. Run Claude Code, Codex, and OpenCode from one desktop app, with your projects, conversations, files, terminals, and changes together.

LUNA is built in Rust with GPUI. It starts in local-only mode: no LUNA account or sync service is required. Agents use their own installations and authentication, and may need an internet connection.

## What you can do

- Organize projects and agent conversations, including tabs and split views.
- Follow agent output, inspect tool calls, handle permission requests, and queue messages.
- Browse workspace files, review diffs and history, and use terminals and browser panels alongside a conversation.
- Configure providers, skills, MCP integrations, and installed extension packages.
- Use LUNA Board to organize work and link GitHub issues.
- Run the engine without the desktop UI, or use the CLI and SDK to interact with it.

Provider capabilities vary; LUNA exposes the controls supported by the selected provider. Additional provider integrations use installed packages rather than requiring every agent to be part of the desktop binary.

## Get started

Download an available build for your platform from [GitHub Releases](https://github.com/novnski/luna/releases), or build from source below. Install and authenticate the coding agent you want to use, then open LUNA, add a project folder, and start a conversation with that provider.

The repository contains desktop code for macOS, Linux, and Windows, plus an iOS companion. Packaging, system dependencies, and feature support differ by platform. The [iOS guide](apps/ios/README.md) describes its current capabilities and migration limits.

## Build from source

Clone the repository and install stable Rust; [rust-toolchain.toml](rust-toolchain.toml) declares the toolchain and components. GPUI sources are included under `vendor/`.

```sh
git clone git@github.com:novnski/luna.git
cd luna
```

### macOS

Install Xcode Command Line Tools. Build and open the isolated development app:

```sh
python3 scripts/macos-app.py dev
```

This creates **LUNA Dev** at `target/luna-macos-dev/LUNA Dev.app`, with separate data under `target/luna-macos-dev/data/`. It gracefully leaves Alpha, then uses the guarded development build/refresh. Any other LUNA GUI must save and quit first. For packaging without launching, use `scripts/run-macos-dev.sh --build-only` directly.

For day-to-day use, install a frozen **LUNA Alpha** from committed, clean `main`:

```sh
python3 scripts/macos-app.py install-alpha   # first installation only
python3 scripts/macos-app.py alpha           # open the existing snapshot
python3 scripts/macos-app.py dev             # switch to development and refresh Dev
python3 scripts/macos-app.py update-alpha    # replace Alpha only when explicitly requested
```

Alpha lives at `~/Applications/LUNA Alpha.app`, uses `~/.luna-alpha` for its own data/worktrees, and does not update itself. Dev refreshes and source version bumps do not replace Alpha. Keep one LUNA GUI running at a time; switching saves and quits gracefully. Existing Dev conversations stay in Dev; Alpha starts with its own profile. See [app channels, versioning, and releases](docs/reference/local-release-workflow.md).

### Linux

Install the system dependencies listed in the [Linux packaging script](scripts/package-linux.sh) and [browser runtime guide](docs/reference/linux-browser.md), then:

```sh
scripts/cargo.sh build --locked -p luna -p luna-extension-host -p luna-engine
./target/debug/luna
```

### Windows

Follow [Windows development](docs/reference/windows-development.md) for the MSVC toolchain, Windows SDK, native dependencies, build commands, and portable release layout.

On macOS and Linux, use `scripts/cargo.sh` for Rust commands. It applies the repository's build guard and uses the optional Mr Boxington cache when installed. See [build cache and disk limits](docs/reference/build-cache.md).

## Local engine and data

Run a foreground engine without the UI, or inspect a running instance:

```sh
./target/debug/luna headless
# In another terminal, using the same data directory:
./target/debug/luna status
./target/debug/luna --help
```

`luna daemon install` installs a background service on supported macOS/Linux systems; `luna daemon start`, `stop`, and `status` manage an installed service. Do not run two engines against the same data directory.

Default application data lives in `~/.luna` on macOS/Linux and `%LOCALAPPDATA%\LUNA` on Windows. `LUNA_DATA_DIR` selects a different root. Provider credentials remain in their provider-owned locations. The development app's separate data directory is intentional: a CLI command aimed at it must use that same `LUNA_DATA_DIR` and `LUNA_IPC_PORT=59777`.

## Optional sync and remote access

Local use works without a hosted backend. Account sync requires a configured LUNA-owned service, including `LUNA_EDGE_URL` and `LUNA_WORKOS_CLIENT_ID`. A fresh clone does not come with a hosted service configured.

Signing in selects the synced profile for the next engine start; signing out returns to the local profile. Existing local conversations are not automatically uploaded or imported. Stop the app or daemon before changing authentication, then restart it.

Synced devices are trusted with remote workspace access, including file reads and writes. Only enroll devices you trust with your workspace contents. Pairing, SSH, and relay support have their own access boundaries; see `luna remote --help`, `luna ssh --help`, and the [relay guide](docs/reference/luna-relay.md).

## Contribute and report problems

- [Contribution guide](CONTRIBUTING.md): development workflow, verification, and issue triage.
- [Report a bug](https://github.com/novnski/luna/issues/new?template=bug_report.yml): behavior, reproduction steps, environment, and optional logs, screenshots, videos, or source paths.
- [Request a feature](https://github.com/novnski/luna/issues/new?template=feature_request.yml).
- [Pull request template](.github/PULL_REQUEST_TEMPLATE.md): scope, linked issues, and verification evidence.

New issues start with `needs-triage`. Maintainers add area labels and mark actionable work `ready-for-agent` after review. See the contribution guide for the filtering contract.

The [task queue](docs/pending-tasks/README.md) tracks repository implementation work. Its status is separate from LUNA Board's runtime state. [ARCHITECTURE.md](ARCHITECTURE.md) contains historical context and links to the current core work; it should not be read as a complete description of the current app.

## License

[MIT](LICENSE).
