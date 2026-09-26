# Commands Stop after Retry: failure-first matrix

Native integration on combined commit `58c084c7` observed one run at attempt 2
with a new terminal and a listening preview process. Two visible Stop clicks left
that same attempt Running. After navigating A2 → A1, Stop canceled that same
attempt and released its port. This confirms the backend can stop attempt 2;
it does not prove which UI state suppressed the first two clicks.

The suspected UI path is the Commands panel's one-second status poll: `load`
sets `busy` while its RPC is pending, and rendering leaves the Stop label on
screen but omits its click handler. The following GPUI/RPC cases use actual
rendered button mouse input and held status responses to make that path
repeatable before changing production code.

| Case | Setup and input | Required result |
| --- | --- | --- |
| Retry and current Stop during status read | Start from a canceled attempt 1; click rendered Retry; receive Running attempt 2 with the same run ID and a new terminal ID; hold the next status poll; click rendered Stop | Exactly one Stop RPC for the current run, despite the pending read |
| Duplicate Stop while write pending | Click Stop again while its first RPC is held | No second Stop RPC; first write keeps its waiter |
| Stale scope | Change engine, selected task, cwd, or checkout after painting a control, then deliver that old frame's input | No command RPC against the new scope; existing stale-button/scope cases remain green |
| Retry result ownership | Complete Retry and its status read without clicking Run | No Run RPC or second Retry RPC; attempt 2 keeps its run ID and new terminal |
| Refreshed-frame control | Complete Stop and refresh status | Current run is Canceled; the test records the full RPC and rendered-state trace |

The test writes a JSON trace through `LUNA_EPOCH_QA_ARTIFACTS` before its
assertions. The original native report remains under the integration worktree's
`target/qa/projects-native/58c084c7-integration/` directory.
