# Complete a two-version macOS GitHub Release updater trial

**Shared tracking:** UP-01 in [the queue](README.md), initially Blocked.
Use [the agent workflow](WORKFLOW.md) and fresh prerequisite readback before unblocking; the status below is the retained planning baseline.

**Status:** Blocked; rechecked on 2026-10-11 using the repository and Releases GitHub API through `gh`. The repository is private and the Releases list is empty, so the app cannot yet fetch a release anonymously. The readback is recorded in [queue verification](../qa/pending-tasks-verification.json).

The [local updater QA](../qa/update-e2e/README.md) passed signed-manifest checks, bundle replacement, and a Settings > Updates restart. Its fixtures changed the plist version and marker while reusing one compiled executable. They do not establish that a newly compiled version reports its new version after restart or that the GitHub Release download path works end to end.

When the repository and its Releases are accessible to the intended updater clients, install one actual macOS release and publish a newer release built from a distinct versioned source commit. From the older installed `LUNA.app`, use **Settings > Updates** to check, download, and restart. Verify the offered version, old and new exact-bundle PIDs, the new executable's reported version, plist version and bundle identifier, strict code signature, and the relaunched IPC listener. Retain the release asset hashes and readback as evidence.

Keep the isolated QA app and data separate from the primary LUNA Dev install. Follow the [release guide](../reference/github-release-updates.md) and the one-app rule in [AGENTS.md](../../AGENTS.md) during the trial.
