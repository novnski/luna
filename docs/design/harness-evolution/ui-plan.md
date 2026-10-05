# Harness evolution: UI plan (slice 1)

UI owner: the established Claude conversation (crates/ui only). Contract
authority: `crates/proto/src/execution.rs` (`TaskExecutionSummary`, schema 1),
published as `WatchChats` row `execution` for locally owned tasks and as
`RuntimeContractView.execution`. Root owns proto/engine/harness, builds, E2E
runs and delivery; see [README](README.md).

## Surface

- **Composer chips** (model, access). While a turn is starting or running and
  the engine reports pending model/provider/account/reasoning/option or
  permission fields, the chip shows the **running** settings, a corner dot,
  and a tooltip with Running / Requested and how the change applies. Idle or
  warm chats show the requested settings; a pending change still gets the dot
  and the explanation (a warm runtime's settings read as "Loaded").
- **Queue Steer** follows `steer.kind` when the summary is present: Native is
  the only non-interrupting Steer; After turn and Unavailable never offer it,
  and the row's tooltip names the engine's reason. Without a summary the
  existing catalog check stays (fail closed while loading).
- **Settings → Providers → Advanced → Execution** (the former Effective
  session card) follows the focused workbench chat, not Main. It shows the
  chat title, phase, a Setting / Running (or Loaded) / Requested table with
  pending rows marked, how pending changes apply, steering, and adapter
  declarations labelled as declarations. Execution renders **only** from the
  live `WatchChats` accessor; the one-shot `GetRuntimeContract` response is
  never an execution fallback and feeds only the muted "Technical · last read
  from the engine, not live" section (contract hash, delivery, tools).
- Execution observations are dropped when the chats watch ends and whenever
  the engine connection changes (attach, reconnect, profile replacement), and
  each WatchChats frame replaces them, so an absent field claims nothing.
  Contract responses install only for the focused chat on the engine
  connection generation they were requested on.
- The UI owns all wording. The engine's `transition.reason`, `steer.reason`
  and `observationNote` are shown verbatim as an explanatory line.

## Failure cases (written before implementation)

1. A requested model, reasoning, option, provider or account replaces the
   chip's label while the running turn still uses the old settings.
2. The pending dot persists after the engine reports no pending fields, or a
   finished turn keeps showing the old running settings.
3. A parked warm runtime's settings are labelled "Running", or a cold
   restart/stream end leaves an old observation displayed as live.
4. A raised or lowered access level is shown as applied during the old turn,
   or a ceiling/degradation explanation disappears when a summary is present.
5. `newBinding` copy implies the previous conversation's context is carried
   over automatically.
6. Native Steer is offered when the engine says After turn or Unavailable, or
   the unavailable reason is not visible on the row.
7. A missing summary (remote host, older engine, unavailable), an unknown
   schema version or a summary for another task is replaced by a local
   capability or effective-state claim; or its absence breaks the legacy
   chips, access preview or steer check.
8. Adapter capabilities are presented as verified/negotiated support.
9. The Execution card follows Main (or `selected_chat`) instead of the focused
   workbench tab, or a late response for the previous focus overwrites the
   current one.
10. Long model names, reasons or notes overflow the opaque tooltip/card or
    become translucent over content.
11. Any provider-native id, the contract's provider session id, credentials or
    the raw task id is rendered.
12. A diagnostic read starts a run, changes configuration, or adds a request.
13. After the watch ends, a reconnect or a frame without `execution`, a
    cached contract snapshot revives an old Running state.
14. A same-task contract response requested on an earlier engine connection
    or profile installs after a reconnect.

## Fixture

`composer-parity-fixture` (extended; failure cases 12-25 in its header)
drives the production Shell on the in-process engine, after its existing
c01-c10 steps:

- e01: the engine's own `WatchChats` summary for Main decodes (schema 1,
  Main's task, requested provider), and the chips' dots match the engine's
  pending list. A legacy baseline is then recorded with Main's summary
  hidden (as a remote or older host).
- e02: a running turn on Opus / Low / Standard at Approve for me while Sonnet
  / High / Fast at Full access is requested: running labels and access icon,
  dots, Running / Requested / how it applies / a long engine reason in the
  tooltips (captured hovered), no task id, and the fork's composer unchanged.
- e03: Steer through the queue's own resolver: unavailable → Send now with
  the reason; native → Steer now; after turn → Send now with the reason; the
  fork follows its own decision.
- e04: next-turn clearing (idle, nothing pending) equals the legacy baseline.
- e05: warm reads Loaded and keeps requested labels; a new binding says
  context is not carried over automatically.
- e06: missing, old-schema and other-task summaries equal the legacy chips
  and Steer.
- e07: the Execution card follows the focused fork tab (title, rows, pending
  marks, long note, declarations labelled unverified, steering); a late
  contract response for the fork is refused after focus returns to Main.
  The Providers page's real scroll container is scrolled to the card; its
  painted bounds must be inside the viewport before the capture (a second
  capture shows the summary's end if the card is taller than the viewport).
- e08: a contract carrying a Running snapshot is installed, then the watch is
  invalidated: the card and chips show no Running state. After a reconnect, a
  same-ID response from the earlier connection is refused and one from the
  current connection installs.

QA-only hooks: `Shell::fixture_seed_execution` / `fixture_clear_execution`
(override the same `AppState::execution_for` every view reads),
`fixture_watch_execution`, `fixture_execution_focus`,
`fixture_steer_resolutions`, `fixture_open_execution_card`,
`fixture_execution_card`, `fixture_deliver_execution_contract` (the card's
real completion guard, with the requesting connection generation),
`fixture_engine_generation`, `fixture_reconnect_engine`,
`fixture_end_chats_watch` (the watch's own invalidation).

Evidence: `target/qa/harness-evolution/` (root's run). No unit tests.
