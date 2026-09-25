# Sidebar hover and detail card QA — 2026-09-25

## Repeat

From the repository root, use a fresh disposable profile:

```sh
qa_root=$(mktemp -d /tmp/luna-l12-sidebar-hover.XXXXXX)
LUNA_SIDEBAR_SCALE_QA=1 scripts/run-macos-l12-fixture.sh "$qa_root" build
LUNA_SIDEBAR_SCALE_QA=1 scripts/run-macos-l12-fixture.sh "$qa_root" run
cat "$qa_root/SIDEBAR-QA.json"
```

Open **LUNA L12 QA**. The manifest identifies 60 recent tasks. In the normal sidebar, hover **Scale keyword recent thread 59** and compare its position with the pinned rows above and **Non-Git workspace task** below. The row should keep its height and neighbors' positions. Pin, Settle, and Archive should appear together at the right edge as small icon controls. A completed task should show a green dot in place of the check and Done label, with a brighter title.

Keep the pointer over the row until its detail card opens. The card should start just beyond the sidebar's right edge and show title, project, device, model if configured, branch if available, and status. Hover Pin, Settle, Archive, the sidebar view control, Activity, Settings, and Devices to read their action tooltips. Archive a disposable recent task and confirm it leaves the active list.

Stop the fixture when finished:

```sh
LUNA_SIDEBAR_SCALE_QA=1 scripts/run-macos-l12-fixture.sh "$qa_root" stop
```

## Native readback

The isolated `sidebar-scale-v5` fixture built and ran with a valid signed bundle. On the completed **Scale keyword recent thread 59** row, hovering revealed Pin, Settle, and Archive as one compact group at the right edge. The pinned rows above, divider, and **Non-Git workspace task** below stayed in place. The status appeared as a green dot with no Done label in the row. The detail card opened to the right of the sidebar, showing the task title, **Fieldnotes**, **Iulian’s MacBook Pro 16**, **Provider · fixture-only**, and **Done**. Clicking Archive removed thread 59 from the active list in the first disposable run. The grouped-control check used a fresh disposable profile; no personal task was modified.

## Anchored tooltip follow-up

Hover a task's text and compare the top edge of its detail card with the top edge of its row. The card should open after about 300 ms, start beyond the sidebar's right edge, and leave the row highlighted. Move across Pin, Settle, and Archive: only the hovered control's tooltip should appear; the detail card must stay hidden throughout the action strip. Scroll or click before the detail delay expires and confirm no late card appears. For a row near the bottom of the window, the card may move upward enough to stay within the viewport.

The native `sidebar-scale-v5` fixture showed the card's top aligned to **Scale keyword recent thread 59** and the hover controls in the same row. The **Scale keyword recent thread 07** card stayed within the bottom edge by moving upward. An unselected **Non-Git workspace task** showed its hover wash and detail card together. The cursor-anchored transparent spacer that previously crossed the row is gone; the action strip is excluded from the detail trigger in the row geometry.
