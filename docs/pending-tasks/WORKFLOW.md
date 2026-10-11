# Working from the shared queue

Use this workflow for repository development tasks from Codex, Claude Code,
OpenCode or another agent with a shell. `scripts/pending_tasks.py` is the public
tool. It needs Python 3.10+ and Git, uses only the standard library, and returns
JSON with a nonzero exit on refusal. It does not start agents or change the
LUNA application.

## Pick up the next task

From the repository root:

```sh
python3 scripts/pending_tasks.py status
python3 scripts/pending_tasks.py claim --owner '<agent name>' --session '<actual chat or run ID>'
```

The claim response includes the ticket, all specifications, acceptance criteria,
partial evidence, resume note, previous worker, checkout and `claim_token`.
Keep that token for the current claim. Read the specifications and previous
work before editing. A session can hold one ticket at a time.

Selection is deterministic: eligible unfinished/paused work first, then P0 to
P3, catalog order, and ID. Dependencies must be Done. Deferred, blocked, owned
and conflicting work is excluded. An explicit requested ticket still passes
the same dependency and resource checks. `next` previews selection and reserves
nothing; `claim` performs selection and reservation in one transaction.

```sh
python3 scripts/pending_tasks.py next --group artifacts
python3 scripts/pending_tasks.py claim IC-0 --owner '<agent>' --session '<run ID>'
python3 scripts/pending_tasks.py show IC-0
python3 scripts/pending_tasks.py history IC-0
python3 scripts/pending_tasks.py list --status paused
```

`list` defaults to compact rows; `list --full` includes contracts. `show` returns the full ticket. `history`
returns its durable events. `status` shows counts, acceptance coverage, expired
claims, next eligible tickets and the remote milestones. Supply `--group` to
`list`, `next` or `claim` to scope selection; see the current group IDs in status.

The root [agent instructions](../../AGENTS.md) make this the default for
“pick up the next pending task.” A catalog assignment is development scope;
live production operations retain their own authorization requirements.

## Checkpoint, pause and finish

If the provider exposes no chat ID, generate and retain one `agent-run:<UUID>`
for this actual working session. Reuse it across checkpoints and continuations;
do not invent a different session identifier to bypass an existing claim.

Claims default to one hour. Refresh at least every 20 minutes and immediately
before a long check. For an unattended check longer than an hour, set a suitable
`--lease-seconds` on claim/heartbeat (maximum 86400) before starting it.
A heartbeat extends the current claim; it cannot revive
an expired one. Never heartbeat work you have stopped.

```sh
python3 scripts/pending_tasks.py heartbeat IC-0 --token '<claim token>'
python3 scripts/pending_tasks.py check IC-0 shared --token '<claim token>' \
  --note 'Reference/storage contract agreed; next prove native isolation' \
  --evidence 'docs/qa/ic0/shared-contract.json'
python3 scripts/pending_tasks.py pause IC-0 --token '<claim token>' \
  --note 'Shared contract accepted. Branch <name>, last commit <sha>. Native probe not run. Next: <exact command>. Evidence: <path>. Uncommitted files: <paths or none>.'
```

Use the actual criterion IDs from `show`; the example evidence file must exist.
`check` requires a useful note and an existing file or `commit:<hex SHA>`.
Local file evidence is hashed, and commit evidence resolves to an actual Git
commit. Multiple `--evidence` flags are supported. `uncheck` invalidates a
criterion with a reason when new evidence makes it unreliable.

Record only accepted, observable outcomes. Work in progress can be described
in the pause note; it earns no accepted coverage until the criterion passes.
Checks do not independently prove that the evidence supports the claim: the
worker and required reviewer must inspect it. The tool verifies references,
hashes, ownership and the completeness of the checklist.

```sh
python3 scripts/pending_tasks.py block IC-0 --token '<claim token>' \
  --reason 'Native isolation fails on the minimum supported macOS version' \
  --note '<reproduction, partial work, decision needed and exact resume step>'
python3 scripts/pending_tasks.py review IC-0 --token '<claim token>' \
  --note 'Current patch and acceptance evidence ready for the required review'
python3 scripts/pending_tasks.py finish IC-0 --token '<claim token>' \
  --note '<result, limits, checks and verified delivery>' --evidence 'commit:<actual SHA>'
python3 scripts/pending_tasks.py snapshot
```

`review` retains ownership. `finish` refuses unchecked criteria and requires
completion evidence. Every ticket's handoff criterion includes applicable
build, E2E, lint/format, reviewed evidence, LUNA Dev refresh and verified main
delivery. Verification-only tickets can close with passing evidence and no
runtime edits. Use repository [test-audit](../../.codex/skills/test-audit/SKILL.md)
and [commit-and-push](../../.codex/skills/commit-and-push/SKILL.md).

Pause/block removes the live claim and retains the previous worker, progress
and resume instructions. **Unfinished tickets keep their code-area reservation**,
including while blocked or deferred. This prevents another agent from editing
partially implemented functionality before it is resumed or explicitly
cancelled. Unrelated code areas remain available. Before cancelling partial
work, preserve or safely resolve its source changes and record the disposition.

To resolve an external blocker, first read fresh evidence, then:

```sh
python3 scripts/pending_tasks.py unblock UP-01 \
  --note '<what now satisfies the blocked condition>' --evidence '<fresh readback file>'
```

This makes attempted work resumable, or untouched work Ready. Completing an
ordinary dependency makes a Ready/Paused dependent eligible automatically;
it does not launch it or clear an unrelated explicit blocker.

## Expired or interrupted claims

An expired claim is visible in `status`, still reserves its task/code areas,
and refuses further token writes. **Expiry alone never authorizes takeover.**
Check the named chat/process, checkout, Git status and retained evidence to
confirm the old worker is stopped. Preserve incomplete files and identify the
next step. Retain that independent readback, then:

```sh
python3 scripts/pending_tasks.py recover '<ticket>' \
  --note '<prior worker confirmed stopped; partial files, branch and next command>' \
  --evidence '<stopped-worker readback file>'
python3 scripts/pending_tasks.py claim --owner '<agent>' --session '<new actual run ID>'
```

Recovery converts only an expired claim to Paused. A fresh claim has a new
token, so late writes from the former worker fail. An active claim can be
released only by its holder's pause/block/finish. The tool does not inspect
running chats itself; operators supply and review that evidence. Never infer
that a worker stopped from an idle sidebar entry alone.

## Add future work

1. Write a bounded specification and failure/acceptance contract in the
   appropriate existing feature folder, or create a new feature folder.
2. Read [ORGANIZATION.md](ORGANIZATION.md), search `list` and source mappings,
   and choose one existing implementation owner for overlaps. Add dependencies
   or integration links rather than duplicating a renderer, runner or feature.
3. Copy [templates/task.json](templates/task.json) to a temporary JSON file.
   Fill in a unique ID, outcome, group, priority, dependencies, code-area
   reservations, sources, resume guidance and measurable acceptance criteria.
4. Start concrete authorized work Ready, optional proposals Deferred, and
   externally unavailable work Blocked with a specific `blocker`.
5. Admit the task, save the portable checkpoint and review/commit the catalog,
   specification and checkpoint together:

```sh
python3 scripts/pending_tasks.py add --file /tmp/new-luna-task.json
python3 scripts/pending_tasks.py snapshot
python3 scripts/pending_tasks.py validate
git diff --check
```

`add` checks sources, IDs/aliases, dependency cycles, groups, criteria and
resource syntax, increments the catalog revision and adds the task without
resetting existing state. Concurrent writers are serialized; if a competing
update makes the input catalog stale, inspect the new catalog and retry.
Do not edit accepted criteria or active contracts in place. Add follow-up
criteria only before completion; completed work gets a new follow-up ticket.

For an intentional manual catalog edit, increment `revision`, then run `sync`
and `snapshot`. `sync` rejects older/conflicting revisions, deleting tickets,
rewriting live/accepted contracts and removing unfinished reservations. It
preserves actual statuses and evidence; editing the catalog's initial `status`
does not overwrite a live status. Use `activate`, `defer`, `cancel` or `unblock`
with a useful note for lifecycle changes. Do not activate deferred product or
architecture changes without the required scope decision.

## State and progress

- `catalog.json`: committed definitions, grouping, source ownership, acceptance
  criteria and initial states. One criterion weight is one acceptance unit in
  the initial inventory; weights never mean hours or an effort estimate.
- **Live coordination:** SQLite plus event history under
  `<git-common-dir>/luna-pending-tasks/`. Linked worktrees on this Mac share it.
  SQLite transactions make claim/check/recovery atomic across processes.
- `progress.json`: committed portable checkpoint generated by `snapshot`.
  It is a dated snapshot, not live ownership. In a fresh store it restores
  recorded progress/history; working/review tickets become Paused and all
  live claim tokens are discarded. Existing stores retain current live state.
- `progress.md`: readable report generated alongside the JSON checkpoint.
  Its owner/count fields describe the live state at export time; consult the
  CLI before making a claim.
- Coverage is accepted criterion weight divided by total criterion weight for
  active-scope tickets. Deferred/cancelled ideas and shipped reference tickets
  are excluded. A ticket can have 100% accepted criteria while still awaiting
  `finish`; Done counts are reported separately. Changing scope changes the
  denominator and must be visible in the catalog/checkpoint diff.

Save a checkpoint after a coherent milestone, pause or completion, and before
the task's final commit. Include it in the feature's reviewed delivery. After
delivery, record the final commit with `finish`, snapshot again, and commit/push
that small status update. This avoids claiming a commit contains its own SHA.
The live store remains authoritative while its checkout exists. If a checkout
catalog differs from shared state, reads report `catalog_stale`; mutations
refuse until the current catalog is synced. Export also refuses stale catalogs.

For recovery into a fresh explicit local store:

```sh
python3 scripts/pending_tasks.py --state-dir /absolute/local/shared-queue \
  restore --file docs/pending-tasks/progress.json
```

Restore refuses an already used store, a mismatched catalog, incomplete Done
criteria or invalid history. Keep checkpoint and catalog revisions together.
This is local, cooperative coordination. Independent clones/other machines do
not share `.git`; on one host they may explicitly use the same local
`--state-dir` and matching catalog. Do not put the SQLite store on a network
filesystem or assume multi-host claims are safe. Multi-host service integration
is a separately scoped extension. Existing LUNA Board tickets remain their own
runtime; repository queue status is not automatically mirrored into Board.

## Review and repeat the tooling verification

The [failure contract and authoring gate](../qa/pending-tasks.md) precede
implementation. Run the subprocess E2E with a fresh output directory:

```sh
python3 scripts/qa-pending-tasks.py --output /tmp/luna-pending-tasks-e2e-<unique-run>
```

It retains results, command/readback logs and SHA-256 hashes. The CI workflow
runs the same contracts and uploads evidence. This documentation/tooling change
requires no Rust build or GUI replacement; runtime features picked from the
queue still follow the normal build and LUNA Dev handoff rules.
