# Stopped QA app launch: failure cases before helper change

Baseline: signed LUNA QA Pull Requests bundle built from runtime
`09aca3a1c2ec4eecc966c654217abaedbcdcbf98`; UI verification paused when
its old PID stopped. Existing prepare-github-pr-qa.mjs --restart-and-seed
requires a live PID before its reviewed fake-gh launch. This matrix is authored
before adding a stopped-app mode. No Rust/UI/backend code or existing fixture
logic changes are authorized here.

## Required fail-closed cases for --launch-and-seed

1. QA IPC59790 is occupied by any process, including a wrong executable: refuse
   before `open`; do not quit/replace that process.
2. The exact QA executable is unexpectedly already running without a listener,
   or a healthy QA process/listener is live: refuse the stopped-app mode and do
   not change its state. Existing --restart-and-seed remains the explicit live
   path with its graceful, verified-PID quit rule.
3. Bundle missing, wrong bundle ID, wrong embedded data/port or failed strict
   codesign, or a noncanonical/escaping QA data root: refuse before launch.
4. Main59777 changes during launch, including absence becoming presence:
   refuse before seed; read back again before mutation and after proof. Never
   launch, stop or otherwise mutate main as recovery.
5. Fake-gh executable or the explicit fixture PATH/LUNA_NO_LOGIN_SHELL/data/port
   launch environment is missing or wrong: refuse before launch if inspectable.
   If LaunchServices ignores the environment, fail the fresh fake-gh call proof
   before any PR Create. No real gh fallback or hosted write is acceptable.
6. Any fixture writes.jsonl POST predates initial Create in this stopped-launch
   path: refuse before launch. After launch, preview must append exact cwd/repo/
   head fake-gh reads and leave POST count at zero. Historical live-restart QA
   behavior is preserved for previously reviewed post-Create reconciliation.
7. Launch command fails, the QA process does not own59790 by the bounded
   deadline, PID/executable differs, or owner root/device claim fails: stop
   without force quit, retry loop, registration change or substitute app.

No PR-create RPC is present in the helper. A successful stopped launch must
record previousPid=null, one newly observed exact QA listener PID, matching
bundle/data/port/signature, fresh owner/device claim and fake-gh reads with zero
POSTs. A captured old proof or binary hash is never sufficient to enable Create.

## Source-only repair scope

Add only `--launch-and-seed` to prepare-github-pr-qa.mjs. Reuse its existing
launchEnv/open -g -n code and every existing post-launch identity/fixture gate.
Keep default preparation, --seed and --restart-and-seed behavior. The signed app
binary must not be rebuilt, edited or re-signed. The first positive test uses
only this helper and the disposable QA profile; record logs and all before/
after counts. Native CUA may resume only after that fresh proof and a separate
current QA PID check. A refusal or timeout is a setup limit, not a Luna PR
product failure.
