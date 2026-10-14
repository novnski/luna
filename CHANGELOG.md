# Changelog

User-visible changes follow [Semantic Versioning](https://semver.org/) and the [LUNA release workflow](docs/reference/local-release-workflow.md). Product version bumps do not publish releases or update the installed Alpha app.

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
