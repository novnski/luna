# Luna

Control your coding agents (Claude Code, Codex, Cursor, Devin, Grok, Hermes, Pi, Antigravity) locally by default, with optional multi-device sync.

*English | [简体中文](README.zh-CN.md)*

![Luna driving a Claude Code session with a live branch diff sidebar](apps/landing/public/assets/app-screenshot.jpg)

Every device runs a small engine that stores sessions on that device. A new installation starts in local-only mode without an account or a network connection.

## Build and run locally (Linux)

```bash
cargo build --locked -p luna
./target/debug/luna daemon start
./target/debug/luna status
```

The engine starts in local-only mode. Install a release from this project's [GitHub releases](https://github.com/novnski/luna/releases) when one is available. The build uses the GPUI source checked into `vendor/`; it does not fetch an older app fork.

The desktop sidebar browser also needs the [Linux browser runtime](docs/reference/linux-browser.md).

Day-to-day:

```bash
luna status      # local/synced mode and engine status
luna daemon start|stop|restart|status
```

## Optional multi-device sync

Sync requires a service owned and configured for this project. Set `LUNA_EDGE_URL` and the service's `LUNA_WORKOS_CLIENT_ID` before signing in; a fresh clone has no hosted service configured.

Sign in only when you want to open your account's synced workspace. Authentication changes the profile selected by the next engine start, so stop the daemon before changing it:

```bash
luna daemon stop
luna login
luna daemon start
```

You can then start an agent on one synced device and follow or drive it from another. An always-on machine such as a VPS can keep those agents working after you close your laptop.

Devices signed in to the same synced account are trusted with remote workspace access. A device controlling a workspace on another device can list, read, and write its files; enabling `Show ignored files` also makes gitignored files such as `.env` available remotely. `.git` is always excluded. Only sign in devices you trust with the full contents of your workspaces.

Signing in does not upload, move, or import existing local sessions. Local sessions and their attachments remain under the local profile and reappear when you return to local-only mode:

```bash
luna daemon stop
luna logout
luna daemon start
```

`luna login` and `luna logout` refuse to modify credentials while an engine owns the data directory. The desktop app follows the same next-restart profile boundary.

On macOS: use a release from this project, or build `luna` from source and run `luna daemon install` to install the launchd service.

On Windows: extract the portable release ZIP and run `luna.exe`. Keep `luna-update.json` beside it for in-app updates. See the [development notes](docs/reference/windows-development.md) for source builds.

---

Developing or curious how it works? [![Ask DeepWiki](https://deepwiki.com/badge.svg)](https://deepwiki.com/novnski/luna) or check out [ARCHITECTURE.md](ARCHITECTURE.md).

Licensed under the [MIT License](LICENSE).
