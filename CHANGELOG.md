# Changelog

User-visible changes follow [Semantic Versioning](https://semver.org/) and the [LUNA release workflow](docs/reference/local-release-workflow.md). Product version bumps do not publish releases or update the installed Alpha app.

## 0.1.0 — 2026-10-14

- Open a project's local HTML file, such as a Playwright report, as a rendered page: HTML files in Files show **Preview**, which opens the saved file in a Browser tab beside the conversation while the source editor stays available. The preview can load the report's own assets, subpages and hash routes but cannot read or navigate to files outside that file's folder. Choosing Preview again reloads the same tab from disk. Ordinary Browser tabs still refuse `file:` addresses and now say that local files open from Files. Rendered previews are available on macOS and Linux for projects on this device.

## 0.0.2 — 2026-10-14

- Keep a long working-line warning, such as "No activity for 3m 39s · may be stuck", clear of the elapsed time in narrow chat panes at large Interface sizes. The warning now wraps beside the timer instead of painting over it.

## 0.0.1 — 2026-10-14

- Start LUNA's own initial-development version line instead of the inherited `0.2.73` numbering. Preserve independently versioned SDK/API/schema contracts and existing user data.
- Add an explicitly promoted LUNA Alpha snapshot for daily use, isolated from development app refreshes and data.
- Wait for engine readiness when switching apps, so the first CLI/RPC read after a successful switch can connect.
- Add consistent version-bump tooling, agent guidance, and checks before release builds. Initial `0.x` GitHub releases are prereleases; public publishing remains an explicit action.
- Bundle the extension host and OpenCode provider worker with desktop release packages.
