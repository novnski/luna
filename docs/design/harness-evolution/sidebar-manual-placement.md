# Sidebar placement failure cases

Recorded before implementation for the chat placement E2E fixture.

1. A new child appears in the main sidebar automatically because it is selected, pinned, active, settled, archived, orphaned, or under an archived parent.
2. A fork or side thread appears there automatically because it has an ordinary chat row, including after its source is closed or removed.
3. A child or fork disappears from its parent's Related chats or workbench when excluded from the main sidebar.
4. Dragging a child or fork from Related chats into the sidebar fails to save explicit placement, creates a duplicate child row, changes lineage, pins it, or leaves an indented row.
5. A saved explicit placement is lost after a settings reload, leaks between profiles, or applies when the engine has no profile ID.
6. Dragging a placed child back to its parent's Related chats leaves it in the main sidebar; docking and returning a fork loses its explicit sidebar placement.
7. Sidebar keyboard order, active rows, Activity, Settled, and Archived disagree about which parent-origin chats are visible.

The fixture should exercise real GPUI drag and click paths and retain screenshots, a JSON readback, and a pass/failure artifact.
