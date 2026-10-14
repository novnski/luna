# Changelog

User-visible changes follow [Semantic Versioning](https://semver.org/) and the [LUNA release workflow](docs/reference/local-release-workflow.md). Product version bumps do not publish releases or update the installed Alpha app.

## 0.2.0 — 2026-10-14

- Open a project's local HTML file, such as a Playwright report, as a rendered page: HTML files in Files show **Preview**, which opens the saved file in a Browser tab beside the conversation while the source editor stays available. The preview loads the report's own assets, subpages and hash routes but cannot navigate to files outside that file's folder, including through symlinks; on macOS, WebKit can read only that folder. Choosing Preview again reloads the same tab from disk. Ordinary Browser tabs still refuse `file:` addresses and now say that local files open from Files. Rendered previews are available on macOS and Linux for projects on this device.

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
