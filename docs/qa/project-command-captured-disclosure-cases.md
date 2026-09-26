# Captured command disclosure: failure-first matrix

The combined Projects native checklist asks the operator to inspect the
setup receipt's captured command before Retry after the saved action has been
edited. On combined integration `a801`, the run row shows its name, status,
attempt, Retry/Stop and Output, but no captured `run.action.command`. Settings
Edit shows the current mutable definition, which may differ from the receipt.

Use a held-RPC rendered Commands panel with a failed setup receipt for
`printf captured-setup-v1` in `/fixture/setup-checkout`. Its saved action has
the same ID but `printf edited-v2`. No shell or provider runs in this fixture.

| Case | Input | Required readback |
| --- | --- | --- |
| Closed receipt | Paint the failed setup row | A quiet disclosure control is present; captured command text is not exposed until opened |
| Captured content | Click the rendered disclosure | Read-only text identifies the receipt's captured command and checkout, associated with the same run and attempt; the edited saved command is not shown as the captured one |
| Toggle | Close the disclosure | Captured text disappears; failed status and Retry remain available |
| Navigation | Leave and return to the task, then repaint | Receipt is still inspectable; no Run, Retry, Stop, or Save command RPC is sent by painting, disclosure, or navigation |

The GPUI case writes its JSON trace to `LUNA_EPOCH_QA_ARTIFACTS` before
asserting. Native acceptance should photograph the failed setup row with the
expanded captured command and checkout visible immediately before Retry, then
verify Retry uses that same receipt despite a changed Settings definition.
