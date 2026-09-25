# Sidebar scale, Settled, and Activity native QA — 2026-09-25

This exercise used the isolated **LUNA L12 QA** macOS bundle and a disposable
`/private/tmp/luna-l12-*` profile. The opt-in fixture has 60 recent tasks, a
read task last active 30 days ago, a recently *manually* settled task, another
task with activity newer than its manual settlement, one archived task, two
indexed message bodies, and a queued send that requires review.
`SIDEBAR-QA.json` records the exact task IDs and expected page size.

## Repeat

From the repository root:

```sh
qa_root=$(mktemp -d /tmp/luna-l12-sidebar-qa.XXXXXX)
LUNA_SIDEBAR_SCALE_QA=1 scripts/run-macos-l12-fixture.sh "$qa_root" build
LUNA_SIDEBAR_SCALE_QA=1 scripts/run-macos-l12-fixture.sh "$qa_root" run
cat "$qa_root/SIDEBAR-QA.json"
```

Open **LUNA L12 QA**. In the normal sidebar, confirm the organize control is
left of the Activity bell, the first 20 active rows are followed by **Show 20
more**, **Settled** contains the 30-day-old task and recent thread 60, and
**Archived** is separate. Recent thread 59 must be in Active because its latest
activity follows a manual settlement. Open search with Command-K. Search for
the settled titles `Scale keyword older thread` and `Scale keyword recent
thread 60`. Search content in all projects for `buriedmarker` and
`archiveonlymarker` to find the settled and archived message bodies.

Right-click an eligible recent row, or use its hover **Settle** control. It
should leave Active and appear in Settled. The row menu in Settled offers
**Return to Active**; Archive remains a separate action. The fixture's manual
settlement and later activity are written through the synced task record, so
restarting the QA app must preserve their resulting placement.

For Activity, click the bell, then **Scale keyword recent thread 01**. It must
remain selected in the Activity list while open. Its queued message should
still say that local review is needed in the task view. Click **Scale keyword
recent thread 02**: 01 should leave Activity, while 02 stays selected there.
Click 03: 02 should leave and 03 should stay selected. This checks both the
queued-send review case and ordinary unread completion rows.

Stop the QA app before removing the disposable root:

```sh
scripts/run-macos-l12-fixture.sh "$qa_root" stop
```

## Observed

The native UI showed the 20-row page and distinct Settled/Archived sections.
Recent thread 60 was manually settled on startup; recent thread 59 appeared in
Active after its later activity. Right-click **Settle** moved recent thread 02
into Settled, and **Return to Active** moved recent thread 60 out again. Search
found a manually settled title and the indexed body of the auto-settled task;
the archived content query also returned its task.
The Activity sequence retained the selected 01 and then removed it on leaving;
the review queue remained visible in its task view. A subsequent 03 → 04
sequence retained the newly selected 04 while removing 03. The read-only
fixture database still held the unresolved `queuedSendFailed` attention item
for 01 after it left the filter, so viewing did not erase the underlying
recovery state.

The search index intentionally stores bounded redacted summaries; this QA
proves discovery for indexed message content, including archived tasks, rather
than full-text coverage of every character in a long message.

## Pinned boundary and task-card controls

Repeat the fixture commands above with a fresh root. The two seeded pinned
tasks should appear under **Pinned**, each with a visible pin in the same
column. A hairline separates them from ordinary tasks. Task cards show the
project name without repeating the device. Hover an ordinary task to reveal
its pin action. At rest, a pinned task's pin sits beside its timestamp or
status. Hovering expands the action area to show **Settle** or **Archive** at a
consistent size and shifts the pin left. Scroll to the end of the first page:
**Show 20 more** starts on the same
text column as the task titles.

In the native QA app, I unpinned **Pinned design review** and saw it move below
the divider. I then pinned **Scale keyword recent thread 11** from its row and
saw it move into Pinned. After stopping and rebuilding the isolated QA bundle,
thread 11 remained pinned. The task card still selected normally when clicked
outside the pin control. The final native readback showed both pinned rows with
project-only first lines and the pagination control near the task-title column.

For the pin-spacing follow-up, I launched a fresh scale fixture after the
change. At rest, **Pinned design review** showed its pin beside `now`, and
**Pinned release checklist** showed its pin beside `Done`. Hovering the first
card revealed **Archive** and shifted its pin left into the adjacent control
position. The section header and divider remained visible. These states can be
reproduced with the fixture commands above without changing personal tasks.
