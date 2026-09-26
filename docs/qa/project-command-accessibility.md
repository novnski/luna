# Project Commands accessibility acceptance

This is a failure-first acceptance matrix for the rendered Settings → Projects
command card. It exercises synthetic project actions through held RPC replies;
no shell command, provider, or saved user project runs.

## Observed red state

The native `6fce1828` Projects screenshot shows Edit and Remove beside four
saved commands. Its accessibility readback lists the project choice controls
and Add command, but no Edit or Remove buttons. Opening an unsaved command
editor exposes the Name, Command, and Preview URL text fields; setup, wait,
Save, and Cancel are absent from the accessibility tree. The readback and
hashes are in `target/qa/ui-theme-visual-6fce/REPORT.md` in the integration
checkout. The later Stage Manager thumbnail invalidated further native input,
so this matrix does not claim a completed light-mode or keyboard pass.

## Failure cases before the fix

1. A visible Edit or Remove action has no button role or contextual name, so
   a screen reader cannot identify which saved command it changes.
2. A keyboard user cannot Tab to Edit/Remove or activate them with Enter/Space.
   Held key repeats must not create multiple edits or remove RPCs.
3. The editor's setup and wait choices have no announced on/off state. Their
   accessible names must stay meaningful when the checkmark text changes.
4. Setup/wait keyboard activation must toggle only the targeted draft value;
   neither action may save or run a command.
5. Save and Cancel have no accessible button names or keyboard activation.
   Cancel must leave saved commands intact; Save must issue at most one save
   request and stay inactive while a save is pending.
6. Disabled Edit/Remove/Save/Reload controls must not accept pointer or
   keyboard activation during an RPC wait. Existing Stop-after-Retry click
   admission in the Terminal Commands panel must remain unchanged.

## Repeatable verification

The focused rendered GPUI/RPC case in
`crates/ui/src/project_commands_accessibility_tests.rs` paints the real
`ProjectCommandSettings` child, locates the actual controls by QA selectors,
dispatches keyboard and mouse events through the window, and inspects the
resulting draft state and held RPC trace. It writes selector presence/bounds,
focus, RPC counts, and source to a bounded JSON trace before the first
missing-control assertion, so the failing baseline retains its evidence.
The GPUI headless `TestWindow`
does not initialize an accessibility adapter, so it cannot produce a genuine
AccessKit tree. After the source patch is composed into a signed QA app, open
Settings → Projects with a synthetic saved command and repeat native AX
readback in both appearances: Edit/Remove, setup/wait, Save/Cancel and Reload
must be named buttons; setup/wait must announce their current toggle values.
Capture the full-resolution screen, AX text, source commit, PID, port,
signature, settings readback and SHA-256 values under a new `target/qa/` run
directory. Restore the original appearance and task selection.
