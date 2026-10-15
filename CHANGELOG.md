# Changelog

User-visible changes follow [Semantic Versioning](https://semver.org/) and the [LUNA release workflow](docs/reference/local-release-workflow.md). Product version bumps do not publish releases or update the installed Alpha app.

## 0.4.2 — 2026-10-15

- A file open in a task's sidebar that is renamed on disk (for example, by an agent) while you look at another task now reopens under its new name after a restart, instead of showing that the old file couldn't open. Restored file tabs also start following such renames right away, before you show them again.

## 0.4.1 — 2026-10-15

- A server a Codex agent leaves running, such as `npm run preview`, now keeps running while LUNA is open. Previously, LUNA ended the agent's Codex process after a chat sat idle for 30 minutes, and that also stopped every command the agent had left running, so a preview link the agent reported as running stopped loading with no explanation. The command now appears in the chat's **Background work** as a running background shell, and the chat stays connected while it runs. When the command stops, its shell and its command show the exit code (for example "Exited with code 143"). Quitting LUNA or stopping the chat still ends it, and it is then shown as stopped.

## 0.4.0 — 2026-10-15

- Reopen each task's right sidebar where you left it after quitting and relaunching LUNA. The sidebar comes back open or closed as it was, on the same place (Overview, Files, Changes, …), and Files reopens its open file and its file tabs in the same order, read fresh from the task's checkout. Terminals, diffs and web pages are not restarted on their own; those places reopen on their own page. The last 64 tasks you changed are remembered, on this device only.
- When a file in Files can't be opened — for example, it was deleted while LUNA was closed — the editor now names the file, shows why, and offers **Retry** and **Show files** instead of only an error message.

## 0.3.1 — 2026-10-14

- Make the warnings in **Board → From GitHub issue** readable. A message such as "This project's checkout has no GitHub remote." was printed in near-white text on a bright yellow bar in dark appearance, and in dark text on dark amber in light appearance. It now shows amber text on a faint amber background that stays at least 4.5:1 in every built-in theme. The issue-update dialog's warnings use the same style.

## 0.3.0 — 2026-10-14

- LUNA no longer supports Windows. Releases ship for macOS and Linux only, and the Windows build, portable package, CI job and Windows-specific code paths are removed. Existing Windows installs stop receiving updates; their data in `%LOCALAPPDATA%\LUNA` is left untouched.

## 0.2.2 — 2026-10-14

- Pair remote devices with `luna remote pair` on Windows. It previously stopped with "remote authority was published but durability is uncertain". Saved SSH launches, the local credential vault and sign-in session details no longer fail the same way on Windows.

## 0.2.1 — 2026-10-14

- Expose the model picker to assistive technology. VoiceOver and other native accessibility clients now see the provider tabs, each model with its selected state, the favorite toggles, the Reasoning and Speed settings with their current values and choices, the Codex account choice and the Compact and Clear buttons, and can press each one by name. Provider tabs are also keyboard tab stops, and a screen reader can move through a long model list past the rows in view.

## 0.2.0 — 2026-10-14

- Open a project's local HTML file, such as a Playwright report, as a rendered page: HTML files in Files show **Preview**, which opens the saved file in a Browser tab beside the conversation while the source editor stays available. The preview loads the report's own assets, subpages and hash routes but cannot navigate to files outside that file's folder, including through symlinks; on macOS, WebKit can read only that folder. Choosing Preview again reloads the same tab from disk. Ordinary Browser tabs still refuse `file:` addresses and now say that local files open from Files. Rendered previews are available on macOS and Linux for projects on this device.

## 0.1.2 — 2026-10-14

- Searching Git history for a commit hash prefix now shows only that commit. Previously a short hash could also match unrelated commits whose hashes happened to contain the same characters in order.
- Restore local data backups on Windows. Restore previously stopped with "free-space preflight is unavailable on this platform"; it now checks the disk's free space as it does on macOS and Linux.

## 0.1.1 — 2026-10-14

- Git History finishes loading in a repository with no commits yet: it shows "No commits yet" instead of staying on "Loading history…", and the first commit appears on its own once it lands.
- Changes picks up a repository created with `git init` during a task right away, instead of staying on "Preparing diff…" until the task's next turn.

## 0.1.0 — 2026-10-14

- Save downloads from the built-in browser to your Downloads folder instead of silently dropping them. Blob and `data:` exports from a web page (such as an "Export backup" button), attachment links, and files the browser can't display are all saved, the page stays where it was, and an existing file is never overwritten (`backup.json`, then `backup (1).json`).
- Show each download's outcome under the browser toolbar: "Saved backup.json to Downloads" with Show in Finder (macOS) or Show in Folder (Linux), or why it failed with Open in Browser to retry in your default browser. On Linux this replaces the "Frame load interrupted by policy change" error.
- Mark files saved on macOS as web downloads, so Gatekeeper checks them like files from any other browser.

## 0.0.2 — 2026-10-14

- Keep a long working-line warning, such as "No activity for 3m 39s · may be stuck", clear of the elapsed time in narrow chat panes at large Interface sizes. The warning now wraps beside the timer instead of painting over it.

## 0.0.1 — 2026-10-14

- Start LUNA's own initial-development version line instead of the inherited `0.2.73` numbering. Preserve independently versioned SDK/API/schema contracts and existing user data.
- Add an explicitly promoted LUNA Alpha snapshot for daily use, isolated from development app refreshes and data.
- Wait for engine readiness when switching apps, so the first CLI/RPC read after a successful switch can connect.
- Add consistent version-bump tooling, agent guidance, and checks before release builds. Initial `0.x` GitHub releases are prereleases; public publishing remains an explicit action.
- Bundle the extension host and OpenCode provider worker with desktop release packages.
