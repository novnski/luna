# Parallel backend audit — 26 September 2026

The four audit groups and the subsequent idle-archive correction are reviewed
in `codex/audit-integration`. The reviewed runtime is
`9bb805ac357bf71796e383daad37f5f5ede3e190`. Targeted real Codex idle-to-archive
verification passed. No push or deployment is part of this handoff.

## Corrections and evidence

| Area | Result | Verification |
| --- | --- | --- |
| Usage and history | Preserve unavailable legacy records with visible partial status; reparse available transcripts; deduplicate archive moves; fence results to the captured account. | History 35 focused checks; separate account-selection regression. Native cold, warm and restarted usage agree for the same closed reporting period. |
| Provider lifecycle | Stop/drop remains effective during blocked Codex operations and Claude steering writes; uncertain steering acknowledgments do not replay; malformed or cyclic catalogs fail clearly. | Provider audit 273 distinct passing checks. Real GPT-6 Luna normal reply, tool execution, direct steering and interruption verified. |
| Task durability | Claimed or uncertain queue rows resist stale edits/moves; draft idempotence preserves destination; receipt recovery uses exact task/correlation/intent; current clients route queue edits to the owning device. | 319 focused checks across task, document, synchronization and UI paths. Native draft, queue and submission state survived a graceful QA-app restart. |
| Workspace data | Upload publication cannot overwrite existing different content; checkpoints preserve the real index and force-added ignored files; failed worktree deletion preserves files and branch; diff capture avoids the default worker stack overflow. | 101 focused checks and four real native IPC scenarios. |
| Native archive | Close only the selected idle writer and await confirmed stream closure before archive. Hold its transition gate through provider mutation and local commit. Reject queued dispatch that waited across archive using a fresh row read. | Original writer-lock failure and separate automatic queue-drain race reproduced before correction. 47 focused checks pass, including deliberate send-after-archive. Real Codex target archive and warm-sibling continuation passed without app restart. |

Counts above are suite scopes, not an additive count of unique tests. The
combined audit runtime also passed 62 integration checks before the archive
follow-up. Compile, relevant Clippy, formatting and whitespace checks passed;
existing warnings and explicitly skipped live-provider opt-ins are retained in
the individual reports. The earlier main audit and infrastructure milestones
are documented separately and were not rerun as new work.

## Native review

History, Providers, Tasks and Workspace were built as distinct signed apps from
the combined audit runtime `8169b692`. The coordinator verified their exact
bundle processes, assigned IPC listeners and strict code signatures, and read
the corresponding evidence. The archive correction passed a targeted Providers
rebuild from `9bb805ac` in 59 seconds. The coordinator independently verified
PID 42775, port 59781 and the strict signature, then read the actual results.

The real idle target archived with applied receipt
`4f8ca575-cd8b-47e8-9107-544fcc540f9b`, unchanged native task/binding and both
provider and LUNA archived state. The app PID stayed unchanged. Warm sibling
provider PID 43254 survived and the sibling completed its second exact-marker
turn with the same native identity before its own successful archive. Both
histories remain; only the disposable workspace was removed. Evidence is in
the Providers worktree at `target/qa/provider-archive-followup.RgQsJd/`.

The first driver stopped on an overbroad process assertion: it included PID
43657, which the saved pre-archive snapshot proves had already exited. That
failure is retained. Fresh readback preceded the planned sibling continuation;
the continuation did not replay the target archive or earlier prompts. The
earlier app-restart workaround is separate from this successful source fix.

The merged audit changes preserve the existing UI styling. Queue UI changes
affect command routing and error recovery; the settings change supplies a
missing default in an existing test fixture. New sidebar and project features
remain in their own worktrees and are outside this backend handoff.

## Practical limits

- Real provider checks cover Codex GPT-6 Luna. Claude/OpenCode lifecycle fault
  injection remains fixture coverage in this audit.
- Direct native steering is verified. The attempted explicit queued-steer
  case used ordinary Queue intent, so conformant native queued-steer remains
  unverified. A later automatic delivery-unknown result is retained as an
  unconfirmed observation; it was not blindly replayed or claimed fixed.
- Owner routing protects current-client queue edits. Arbitrary old/raw CRDT
  display content is not a signed owner-only projection.
- Native workspace QA covers checkpoint reads/diffs and shared-checkout
  restore refusal. Provider-triggered checkpoint capture and isolated restore
  retain integration coverage.
- History immutability was checked on eight sampled transcripts, not every
  provider file.

## Repeatable records

- [History audit](history-audit-2026-09-26.md) and [native history](history-native-2026-09-26.md)
- [Provider audit](provider-audit-2026-09-26.md) and [native provider QA](provider-native-qa-2026-09-26.md)
- [Task durability](task-durability-audit-2026-09-26.md)
- [Workspace data](workspace-data-audit-2026-09-26.md)
- [Idle archive failure-first cases and commands](idle-archive-2026-09-26.md)

The coordinator tracker and detailed build/evidence paths are retained at
`target/qa/parallel-audit-2026-09-26/coordination.json` in the primary checkout.
All builds use `scripts/cargo.sh` and a serialized slot. At the archive native
grant, aggregate targets plus shared cache measured approximately 136.53 GiB,
below the 170 GiB operating ceiling and 200 GiB absolute policy maximum. No
in-use cache, app, user data or audit evidence was deleted.

## Main handoff

Pending fresh main-state review, local merge preserving the user's unrelated
edits, and a signed primary app refresh.
