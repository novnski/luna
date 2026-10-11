# SW-05 native E06 failure cases

Written before the live runs. Each case names the observable it is checked
against. The deterministic owners (`timeline_order_e2e` failure cases 1–10,
`codex_subagents`, `claude_native_tasks`, `doc` parts, the `ui/transcript`
viewport tests) keep their contracts. These cases only add what a real provider
and the native app can break.

## Real-provider chronology (Codex and Claude, Full)

Each case compares two sources: the provider's own transcript (the Codex rollout
or Claude session JSONL) and LUNA's persisted document (`WatchDocMessages`).

| # | Failure | Observable |
| --- | --- | --- |
| N1 | A provider tool call is missing, duplicated or gets a second chip | Each native tool-call ID appears exactly once as a LUNA tool part |
| N2 | Text and tools reorder relative to the provider | The text/tool sequence of the root reply matches the provider's item order |
| N3 | Two provider messages glue into one paragraph | No LUNA text part joins the text of two provider messages |
| N4 | The helper's "created" card or late result lands above output produced after it | The notice entries sit between the provider items they occurred between |
| N5 | A chip stays running, or an empty or still-streaming entry remains after the turn settles | Every tool part has finished and no entry is `streaming` |
| N6 | Part IDs collide within the chat | All part IDs are unique |
| N7 | A Claude native subagent's activity leaks into the root transcript or is lost | Child events live under the owning Agent/Task chip, or in its child transcript |
| N8 | A restart changes stored content | After a graceful quit and relaunch, the document matches the pre-restart snapshot |

## Native viewport (pixels in the QA app)

| # | Failure | Observable |
| --- | --- | --- |
| V1 | Streaming, row inserts or tool updates below a mid-history reader move the visible rows | Transcript-region pixels stay unchanged across captures taken while the turn streams, apart from the jump-to-latest control |
| V2 | The reader gets pulled to the tail when the turn ends | The capture after settle still shows the same mid-history rows |
| V3 | Switching to another chat and back loses the reader's place | After a switch away and back, the same rows are visible |
| V4 | A window resize moves the reader away from their row | The first visible line text stays visible after the resize |

The viewport cache is held in memory per chat (`saved_viewports`), so the app
makes no promise to restore scroll position after a restart. N8 covers content
across a restart, and V3 covers position within one app session. A pinned
reader following the tail is already owned by
`pinned_viewports_follow_tail_and_the_cache_is_bounded`, so it is not
re-asserted here.
