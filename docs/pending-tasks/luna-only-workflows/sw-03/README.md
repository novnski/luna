# SW-03: reviewed skill guidance

Baseline: latest main `944a6cbe`. Shared queue owns status.

## Failure contract before implementation

| Behavior | Credible failure | Owning proof |
| --- | --- | --- |
| Restricted selected skill | Picker excludes the skill or tries an unavailable slash command | Real signed app, selected project skill and independent output file |
| Reviewed identity/revision | Changed bytes or another project's same name silently replace reviewed content | Selected envelope with revision; real dispatch rejects before provider input |
| Scope precedence | Global entry masks the selected project's override | Existing file-backed lifecycle owner plus two native project workflows |
| Guidance authority | Guidance loads hooks or grants commands/permissions | Actual Ask/Auto launch and tool receipts; unrelated settings preserved |
| Fresh/resume | Guidance is omitted or cached stale after resume | Same selected envelope, native session readback, output files before/after app restart |
| Native-only command | Custom executable command is presented as available guidance | Restricted native command remains unavailable |

Authoring gate: the native workflow owns the actual delivery failure, which
filesystem lifecycle tests cannot reach. Reuse existing library, typed-input,
permission and extension suites. No test-only production seam or new unit tests.
The fixture instructions independently define the expected output; inspect the
real tool result and filesystem rather than accepting the assistant's claim.

## Delivery

The existing `$` picker exposes readable managed Claude skills as **Guidance
only** in restricted modes. Its anchored selection carries the absolute
owning-host path and reviewed tree revision. Discovery uses the existing
library reader and project-over-global precedence; native custom commands
remain unavailable in restricted mode. No settings, hooks, grants, account
selection or executable registrations are imported by guidance.

The engine resolves that exact selection again after account/policy admission,
rejects a changed revision or another project's path, and compiles the approved
instructions with identity/scope/revision provenance. The adapter sends those
instructions in the selected turn's native text blocks. Supporting-file paths
resolve from the quoted skill directory and keep normal tool permissions.
Changing guidance during an active turn requires the ordinary safe boundary.

Live QA caught a resume-specific defect in the first implementation: a revised
skill in `--append-system-prompt` was recorded by LUNA, but resumed Claude work
still used the old token. The final path delivers selected guidance with each
user turn and excludes it from the system-prompt append. The failing captures
remain under `result-0-resume-ask.json` and `result-2-resume-auto.json`; they are
not passing acceptance evidence. Final fresh/resumed cases use new bindings,
then an independently new skill token after an app restart.

Optional selection metadata keeps older native Codex/OpenCode/Claude envelopes
valid. Full's native commands are unchanged. This lane covers managed regular
skill files supported by the existing reader, with its size and symlink limits;
it does not enable native-only command, hook or plugin semantics.

## Repeating the native proof

From this checkout, after gracefully quitting the primary app:

```sh
qa=target/qa/luna-only-workflows/sw03-20261013
CARGO_INCREMENTAL=0 LUNA_DEV_APP_NAME='LUNA QA Skills' \
LUNA_DEV_BUNDLE_ID=com.novnski.luna.qa.skills LUNA_DEV_IPC_PORT=59847 \
LUNA_DEV_DATA_DIR="$PWD/target/luna-qa-data/sw03-skills" \
scripts/run-macos-dev.sh --refresh
agent-python "$qa/native_skills.py" catalog --index 0 --label first
agent-python "$qa/native_skills.py" queue --index 0 --label first
agent-python "$qa/native_skills.py" snapshot --index 0 --label first
```

The retained driver can seed a new disposable pair through `SaveSkillDocument`
and `InstallSkill`. Snapshot the real native Read/Write receipts and compare
result files to the independently prepared input/token. Edit the selected skill,
keep an old selection to test rejection, then select the new revision and
repeat after app restart. `negative_skills.py` reaches the actual engine command
plane with fresh binding generations to check stale and foreign selections.
