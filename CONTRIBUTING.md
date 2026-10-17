# Contributing to LUNA

Bug reports, documentation improvements, and focused code changes are welcome. Search [existing issues](https://github.com/novnski/luna/issues) before opening a new one. For architecture changes or a large feature, discuss the scope in an issue before implementing it.

## Reporting a bug

Use the [bug report form](https://github.com/novnski/luna/issues/new?template=bug_report.yml). Include:

- What happened, what you expected, and how it affects your work.
- Numbered reproduction steps, including what you did immediately before the problem.
- Whether it happens every time, intermittently, or only once; include any pattern or workaround.
- LUNA version or source commit, OS version, installation type, and relevant provider/version.
- Exact error text when available. Optional logs, screenshots, or a short video can help explain the failure.
- Optional repository-relative source paths and line numbers if you know the affected code. You do not need to locate the source to report a bug.

Explain what you tried if you cannot reproduce the problem reliably. Redact credentials, private conversation content, and sensitive workspace paths before posting public evidence. Feature requests have a [separate form](https://github.com/novnski/luna/issues/new?template=feature_request.yml).

## Development

1. Follow the [README](README.md) for setup and platform requirements. Read `AGENTS.md`, the relevant crate, and its CI workflow before changing code. Preserve unrelated changes and vendor-owned guidance.
2. Use a focused branch (for example, `codex/fix-composer-queue` for agent work). Keep changes tied to one issue or clear objective. Never commit credentials or `.env` files.
3. For queued repository work, read [the queue workflow](docs/pending-tasks/WORKFLOW.md) and claim the task before implementation. Respect reservations, dependencies, and existing work.
4. Verify the user-visible behavior at the strongest practical boundary, then open a pull request with the commands and evidence needed to repeat that verification.

Use `scripts/cargo.sh` and `--locked` on macOS/Linux. Before builds, inspect `python3 scripts/build-budget.py --status`. Keep builds focused on the changed packages and reuse the existing target directory.

```sh
scripts/cargo.sh check --locked -p <changed-package>
scripts/cargo.sh fmt --all -- --check
scripts/cargo.sh clippy --locked -p <changed-package> --all-targets -- -D warnings
git diff --check
```

Run the relevant integration/E2E suite and the required build for the changed feature. Consult [.github/workflows](.github/workflows) for the CI commands that own your area; the commands above are a starting point, not the full verification for every change. Documentation-only changes need link, syntax, and diff checks rather than a Rust rebuild.

Read the [test-audit skill](.codex/skills/test-audit/SKILL.md) before writing or changing tests. Define the behavior and credible failure cases first. Prefer an existing end-to-end or integration owner, avoid test-only production seams, and do not add unit tests after writing the implementation. Retain repeatable evidence such as a command transcript, structured result, screenshot, or recording.

Use [mutation testing](docs/reference/mutation-testing.md) to check whether those
tests catch injected bugs: `scripts/cargo.sh mutants --install`, then
`scripts/cargo.sh mutants`. Preview larger scopes with `--list`; the manual
Mutation testing workflow retains reports without extending required PR checks.

For meaningful macOS app code changes, switch to and refresh LUNA Dev with `python3 scripts/macos-app.py dev` after checks. Confirm the exact bundle process, IPC listener, and signature as described in `AGENTS.md`. Keep the daily Alpha snapshot unchanged unless its update is explicitly requested. Respect an explicit request to keep LUNA stopped.

## Pull requests

Before completing a behavior change, classify its product-version impact using [the versioning policy](docs/reference/local-release-workflow.md): compatible fix → patch, new feature or pre-1.0 breaking change → minor, documentation/test-only/refactor without behavior change → no bump. Use `python3 scripts/version.py bump patch` or `minor` once for the PR, then add a concise `CHANGELOG.md` entry. Internal SDK/API/schema versions are separate. A version bump never promotes the daily Alpha app or publishes a release.

The [pull request template](.github/PULL_REQUEST_TEMPLATE.md) appears automatically when opening a PR. Explain the problem and resulting behavior, link related issues, list affected areas, and include verification commands with their outcomes. For UI changes, attach before/after evidence where useful. State any migration, compatibility impact, or unverified platform.

Keep the diff focused and reviewable. Describe both human and agent contributions the same way: the author is responsible for reviewing the result and providing evidence. A linked issue should only use `Closes #123` when the PR fully resolves it.

## Labels and agent eligibility

[.github/labels.json](.github/labels.json) records the labels used by this workflow. GitHub labels are repository metadata and must exist separately from this file. Area dropdown answers in issue forms describe the report; GitHub does not automatically turn those answers into area labels. Maintainers apply the appropriate labels during triage.

| Label | Meaning |
| --- | --- |
| `bug`, `enhancement`, `documentation` | Type of work; existing GitHub labels are reused. |
| `area:ui` | Desktop layout, interaction, rendering, or accessibility. |
| `area:core` | Engine, storage, RPC, SDK, or extension runtime. |
| `area:providers` | Agent integrations, authentication, permissions, or provider protocols. |
| `area:sync` | Multi-device sync, remote access, SSH, or relay. |
| `area:mobile` | iOS app or mobile core integration. |
| `area:build` | Packaging, installation, updates, CI, or build tooling. |
| `needs-triage` | New report awaiting maintainer review. |
| `needs-info` | Missing reproduction details or other necessary evidence. |
| `ready-for-agent` | Reviewed, actionable issue eligible for an agent to investigate. |
| `agent-in-progress` | A specific agent has claimed the issue. |
| `blocked` | Work cannot proceed until a named dependency is resolved. |

Maintainers review scope, duplication, evidence, and a concrete completion criterion before adding `ready-for-agent`. A reliably reproduced bug is ideal; a credible intermittent bug can qualify when it has a bounded investigation and useful evidence. Reporters cannot make an issue eligible by writing a label name or selecting an area in the form.

Remove `needs-triage` and `needs-info` before marking an issue ready. Claiming removes `ready-for-agent` and adds `agent-in-progress`; record ownership so another worker does not duplicate it. Blocked work loses the ready/in-progress label and gains `blocked`, with its dependency recorded. A reviewed issue can return to ready when the blocker is resolved or a claim is released.

Future issue agents should use this initial filter and then read the issue before acting:

```text
repo:novnski/luna is:issue is:open label:ready-for-agent -label:needs-triage -label:needs-info -label:blocked -label:agent-in-progress
```

Add `label:area:ui` (or another area) to narrow the queue. This is an eligibility workflow, not an automatic review agent; no monitor or review automation is configured by these files. External issues created through an API may bypass the web form, so labels and a content review remain necessary.
