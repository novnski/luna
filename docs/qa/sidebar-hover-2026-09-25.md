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
