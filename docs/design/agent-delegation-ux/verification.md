# Delegation UX verification

Evidence root: `target/qa/agent-delegation-ux/`. The native runner is
`scripts/qa-agent-delegation-ux.mjs`; it refuses any app other than the signed
isolated QA bundle, validates its exact process/IPC/data identity, and checks
the per-process MCP isolation flag before creating or reading fixture chats.
All native prompts are scoped to disposable projects.

## Behavior verified

- The user's original Random number case contained one `create_thread` call
  and no `wait_threads`. Its raw message was a host-authored provenance
  envelope. Human transcript presentation now names the child and displays
  the result while leaving the stored/provider envelope intact. The separate
  provider continuation is not rendered as a human-authored message.
- A delayed-child engine E2E verifies immediate creation, independent parent
  work, completion queued behind an active parent turn, and exactly one
  continuation after restart. Native Codex and Claude flows also created one
  helper each, returned the independent answer, and received one follow-up.
  A captured native snapshot shows an idle Codex parent while its helper runs.
- Native `#Retry` results preserve related/project/other-project ordering.
  Selecting an other-project result inserts a canonical exact-ID reference
  without navigating away. Engine draft readback and navigation/restart
  confirm persistence. No attachment, appshot or execution grant is added.
  A separately submitted native prompt preserved the exact reference URL;
  clicking its rendered chip opened the intended other-project chat despite
  an identically titled same-project chat. The model used no tools for that
  reference-only prompt.
- The composer fixture covers false triggers, grouping, duplicate titles,
  Unicode, Tab/Escape, query replacement, rename identity, restored links,
  coexistence with file references, provider-envelope preservation, and
  notice invalidation after a child-title update.
  Origin badges distinguish fresh children, forks/side chats, and independent
  conversations. The fixture also covers a spaceless side chat inheriting its
  source project and origin metadata arriving after the chat row.
- Real Codex manual compaction completed with an Applied receipt in 24.242
  seconds. The following turn invoked `get_workspace`. Both providers resumed,
  created one context-snapshot fork each, and each fork invoked
  `get_workspace` under its own runtime identity. The native runner validates
  actual tool calls after the boundary, not just model-authored success text.
- Runtime diagnostics show one 626–631-character LUNA core bootstrap and 11
  attached tools. Optional guide topics carry the detailed usage instructions;
  a full manual is not prepended to ordinary user messages.
- Claude automatic compaction is covered by a subprocess protocol fixture:
  an observed automatic compact boundary is followed by real `tools/list`
  and `tools/call` against LUNA's local MCP endpoint. This is deterministic
  fixture evidence; real Claude manual out-of-turn compaction is unsupported.

## Compaction failures found during native QA

The first attempt could not resume the provider conversation because its idle
warm process still owned the writer. The implementation now reuses the exact
writer release used for native archive/rewind, requires confirmed EOF, and
holds dispatch through the maintenance operation and durable readback.

The second attempt exceeded the existing ten-second metadata deadline.
Compaction now has a separate three-minute inference deadline. The subprocess
regression deliberately completes after eleven seconds and verifies that
another task's writer, binding identity and transcript remain intact.

Both failed receipts remain in the evidence. They were not replayed. Explicit
recovery cleared only the disposable QA parent's provider context, preserving
its LUNA transcript and children, before starting a new binding. The successful
third operation preserved that binding's generation.

## Scale measurements

The debug benchmark uses a synthetic corpus and checks correctness as well as
latency. Results are machine-local measurements, not a production latency SLA.

| Corpus/case | Measured result |
| --- | --- |
| 10,000 titles, index construction | 105 ms, background work |
| 10,000 titles, slowest ordinary query | 3.51 ms |
| 50,000 titles, index construction | 535 ms, background work |
| 50,000 titles, slowest ordinary query | 17.67 ms |
| 50,000 matching common-trigram titles, old exact match retained | 43.23 ms |
| 10,007 chats through the actual composer, first open | 312 ms including catalog setup, debounce, indexing and render |
| Same composer catalog, next query | 72 ms including 45 ms debounce and render; index reused |

Complete postings and per-group top-K selection retain old/sparse matches.
Cancellation prevents obsolete query results from replacing current ones.
Search is case-insensitive exact/prefix/substring matching, without typo or
diacritic folding. At most eight results per group are rendered.

## Repeatable commands

Use the repository wrapper, `--locked`, and one build at a time:

```sh
CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-engine \
  --test agent_delegation_background --test agent_workspace_controls \
  --test idle_archive --test task_commands
CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-harness \
  --test mcp_startup_isolation --test claude_effective_config \
  --test claude_mcp --test claude_luna_tools --test codex
CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-ui --lib -- --test-threads=1
CARGO_INCREMENTAL=0 scripts/cargo.sh build --locked -p luna-ui \
  --features appshots-fixture --example chat-references-fixture \
  --example chat-reference-search-bench
```

Run both example executables with separate absolute output directories. They
write structured results, screenshots where applicable, and pass markers.
The native run requires the four QA identity overrides documented in the QA
guide, with name `LUNA QA Delegation UX`, bundle
`com.novnski.luna.qa.delegation-ux`, port `59805`, and this evidence root's
`native/data` directory. Gracefully close other LUNA apps first. Then use
`seed`, `start`, `snapshot` until complete, `compact`, `resume`, `snapshot`
until complete, and `verify`. Never re-run a submitted mutation phase to poll.
The explicit recovery mode exists only for the recorded pre-fix failures.

The final full UI suite passed 1,185 tests. The focused engine suites passed 48 tests;
the seven idle-writer tests were rerun after extending the deadline. The Codex
adapter suite passed 76 enabled tests with six explicitly ignored; the new MCP
isolation subprocess test and the focused Claude suites passed. Build/format/
lint logs and final native visual evidence accompany the local evidence bundle.

## Visual review

The final visual revision was implemented in the Claude desktop chat with
Opus 5.5 at Extra effort. It uses existing menu rows, heading/separator styles,
provider icons, and native subagent-card tokens. Chat labels use the app's
sans typeface in the composer and sent messages; file/skill labels retain
their existing presentation.

Native screenshots `05-final-picker-types.png`, `08-final-narrow-card.png`
and `09-final-wide-card.png` show all three type labels, the standard menu
selection and scrollbar, and the card at normal and narrow widths. Narrow
cards wrap the status/actions, preserving the model and reasoning. The
completed Claude helper still showed 49 seconds after several app restarts.
Both view actions and submitted-reference navigation were exercised in the
real signed app. The final native acceptance runner passed again after the
visual refresh. The evidence manifest records hashes of the retained artifacts.
