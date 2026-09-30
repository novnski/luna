# Installed native integration picker verification

The picker extends `@` file completion with distinct Plugin, App and Skill rows.
Selected native identities live in `inputItems`; raw links are editor projections.
A pasted link alone never grants a native selection. Names can collide; IDs cannot.

## Failure cases and artifact requirements

Verify disabled/uninstalled/admin-blocked plugins, non-callable apps, disabled
skills, unavailable/empty/policy-disabled inventories, account/cwd/thread changes,
late responses, duplicate labels, Unicode/parentheses, deletion/undo, draft
restart, queue editing, dispatch rejection and remote host routing. Preserve raw
RPC evidence and native UI screenshots outside the source checkout, with hashes.
Synthetic fixtures prove adapter contracts only. Real installed catalogs and
actual UI/draft/dispatch are separate required proof.

## Coordinator checks

Run these only from the coordinator-owned validation checkout after merging
native-inputs and integrations-picker. The worker intentionally cannot compile
this branch alone: native-inputs owns `ComposerInput` and the composer map.

```sh
scripts/cargo.sh check --locked -p luna-harness -p luna-engine -p luna-ui
LUNA_PICKER_EVIDENCE_DIR=/absolute/preserved/evidence/integrations-picker scripts/cargo.sh test --locked -p luna-harness --test composer_catalog -- --test-threads=1
scripts/cargo.sh clippy --locked -p luna-harness -p luna-engine -p luna-ui -- -D warnings
```

The subprocess scenario writes `adapter-catalog.json`. It uses the deterministic
`fake-composer-catalog.py` peer and never contacts a model or native provider.

Against the coordinator's already-running and verified LUNA Dev app, choose an
existing Codex task or project on the execution host:

```sh
LUNA_PICKER_EVIDENCE_DIR=/absolute/preserved/evidence/integrations-picker node scripts/qa-integrations-picker.mjs --chat TASK_ID --target HOST_DEVICE_ID
# Or use --space PROJECT_ID for a new-thread canvas, with optional --account ACCOUNT_ID.
```

This reads actual engine/adapter inventories twice and checks that file discovery
still uses its existing contract. It writes `live-engine-catalog.json` and its
SHA-256 manifest, including unavailable slice states without claiming them ready.
It creates no worktrees, does not launch a GUI or build a binary, and does not
change credentials or install integrations. An unloaded native thread reports
its plugin/app slices unavailable because thread policy cannot be proven by a
fresh app-server connection; inspect a connected loaded Codex thread to verify
those slices. Set `LUNA_PICKER_ALLOW_UNAVAILABLE=1` only for a negative scenario.

## Actual native composer/draft/dispatch scenario

1. Open a Codex canvas on the catalog's execution host/account. Type `@`; capture
   the rows and badges, then search one existing plugin, callable app and skill.
   Record the IDs/paths from the live catalog artifact, not their display labels.
2. Select each row using both keyboard and mouse. Add ordinary text and a file
   `@` reference. Capture the composer and draft RPC readback; `inputItems` must
   contain the exact two native Mention paths and absolute Skill path.
3. Delete one chip, undo, redo, and edit on either side of a Unicode name. Save
   readback after each action. Deletion removes its typed item; undo restores the
   exact previously selected identity. Paste an unselected raw integration link
   and prove that no typed item is created.
4. Navigate away and back, then let the exact app save and quit gracefully and
   restart through the coordinator's handoff workflow. Read the persisted draft
   and capture the restored chips and exact `inputItems` again.
5. Queue a harmless prompt that asks only to acknowledge the selected integration
   names, then edit the queue and remove/re-add a chip. Capture the queue payload
   and dispatch receipt/native adapter evidence. An actual selected-input turn
   uses native Mention/Skill items; plain labels or pasted links do not suffice.
6. Switch to another existing account or workspace while keeping a selected
   chip. Keep the draft and exact selection. Discovery/dispatch must reject an
   unavailable selection clearly; it must not swap an ID based on the same name.
   Restore the original context and verify that sending is possible again.
7. Repeat on a remote execution host. Client-local plugin/skill paths must not
   populate the host catalog or typed Skill identity. Record target device and
   opaque catalog scope alongside screenshots and dispatch evidence.
8. Disconnect/reconnect the selected host and retry discovery. Show unavailable
   state distinctly from an empty inventory. Same-scope last-good rows can remain
   visibly stale, but cannot authorize selection or dispatch. Explicit Retry
   forces a new native read; TTL is five minutes and stale retention is bounded
   to thirty minutes.

Do not mark UI/native proof complete until these artifacts exist and are reviewed.
