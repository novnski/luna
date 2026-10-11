# Shared pending task queue

Ask an agent: **“Pick up the next pending task and work on it.”** The root
[AGENTS.md](../../AGENTS.md) routes that request through the atomic queue.

```sh
python3 scripts/pending_tasks.py status
python3 scripts/pending_tasks.py claim --owner '<agent name>' --session '<actual chat or run ID>'
```

The agent receives one eligible ticket with its full specifications, previous
work, acceptance criteria and claim token. Unfinished eligible work comes
first. Owned/conflicting tasks, unmet dependencies and explicit blockers are
skipped. `next` previews; `claim` reserves. Future agents must register their
claims for the coordination tools to see them.

Start with [the agent workflow](WORKFLOW.md), [how the work fits together](ORGANIZATION.md),
and [the saved progress report](progress.md). [catalog.json](catalog.json)
contains the contracts; `status` reads live ownership across linked worktrees.
The report is a dated checkpoint. [templates/task.json](templates/task.json)
and `add` admit new tasks without losing progress.

## Planned scope, updated 11 October 2026

| State | Entries | Meaning |
| --- | ---: | --- |
| Ready | 59 | Concrete specified work; dependencies/resources determine eligibility. |
| Blocked | 1 | Real updater trial awaiting its recorded external prerequisites. |
| Deferred | 25 | Later client extensions, optional journeys and research ideas awaiting bounded scope decisions. |
| Done references | 5 | Shipped PB-0/1/1R/2/3; excluded from active progress. |

**90 entries across 10 workstreams; 60 active-scope tasks, 0% accepted completion at this planning update.** Progress measures
evidenced acceptance criteria, not estimated time. The default first ticket is
**CORE-00**, the measured core/extension baseline. The new
[core, SDK and plugin program](extensible-core/README.md) adds 24 tasks and
coordinates prerequisite contracts with existing features. Later status comes
from the live CLI and generated checkpoint, not these planning counts.

## Detailed specifications

| Source package | Queue ownership |
| --- | --- |
| [Extensible core, SDK and plugins](extensible-core/README.md) | CORE-00–CORE-23: stable core/client contracts, independent providers, UI/content extensions, feature extraction, lifecycle and measured acceptance. |
| [LUNA-only workflows](luna-only-workflows/README.md) | SW-00–SW-16: integration, operations, transcript/recovery, helper and switching journeys; consumes shared feature owners. |
| [Interactive surfaces](interactive-chat-surfaces/README.md) | IC-0–IC-6: shared storage/renderer, all-provider HTML, MCP Apps, conversation Find and integrated acceptance. |
| [Useful Zeron changes](zeron-upstream-improvements/README.md) | ZI-01–ZI-05: bounded snapshots/registry, current checklist, typed file navigation and workspace text search. |
| [Control this Mac](control-this-mac/README.md) + [T3 remote/mobile audit](t3-remote-mobile/README.md) | One RM-00–RM-13 chain. M0–M4 derive from RM-00–RM-07; later extensions remain deferred. |
| [Agent workspace research](agent-workspace-app-research/README.md) | Fifteen audit-first AW opportunities; AW-13 maps to PB-4A and AW-14 to PB-4D. |
| [Project Board](devin-project-board/README.md) | Five shipped references; PB-4A/B/C/D deferred for grouping, trackers, templates and bounded workflows. |
| [Actual updater trial](github-release-updater-two-version-trial.md) | UP-01, blocked until fresh evidence satisfies access/release prerequisites. |

Specifications retain their stable folders, research and evidence. Resource
reservations prevent overlapping implementation; the graph avoids duplicate
work across packages. The queue is local repository tooling and has separate
ownership from LUNA's runtime Board. See the workflow for portable checkpoints,
safe recovery and the local/multi-host coordination boundary.

## Historical work

Completed parity/orchestrator specifications were removed on 2026-10-08;
recover them with `git show be17717e:docs/pending-tasks/<path>`. OpenCode parity
was removed on 2026-10-09; its adapter/live E2Es are merged, OC-18 is not pursued,
and remote OpenCode work belongs to the remote chain
(`git show 2abe004b:docs/pending-tasks/opencode-feature-parity/<path>`).
Claude's [two-writer protection](../reference/claude-code-advanced-certification.md)
has retained live verification. Native Claude workflow lanes remain deferred,
and the cancelled Plan/Build workflow is not revived by this catalog.
