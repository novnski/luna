# SW-02: scoped Claude MCP delivery

Baseline: `bb3cf3ed`. Shared queue owns live status.

## Failure contract and authoring gate

| Behavior | Credible regression | Primary boundary and oracle |
| --- | --- | --- |
| Restricted native MCP delivery | Saved selected connection is absent; unrelated settings/hooks load instead | Installed CLI scope probe, then real Ask session with independent HTTP service log |
| Permission preservation | Read/write gets bypass, declined or stale grant reaches service | Existing native approval owner; real root `tool_use` and exact `RespondApproval`, independent mutation count |
| Advisory annotations | A misleading read-only hint authorizes a real fixture write | Native callback evidence plus independent service effect count; do not infer authority from metadata |
| Ownership/configuration | Other account/project/session or disabled/unapproved connection supplies authority | Existing runtime/permission fences and scoped provider config resolution; no foreign downstream effect |
| Uncertain delivery/resume | A committed write is silently reissued after lost acknowledgement/restart | Real HTTP request log and retained session/document readback, no test-provided idempotency receipt |
| Existing gateway | New registrations, credentials or policy changes replace saved ownership | Existing Executor connection read and real native gateway call; compare sanitized source fingerprints |

Prefer a verified native scoped configuration/control route. Keep `--restricted`,
native permission mode and the stdio callback. Administrative inspection never
executes chat tool calls. No new Keychain/auth storage, copied credential files,
blanket external-tool allowlist, marketplace or settings database. Auto requires
its own eligible-account proof; an unavailable variant stays explicitly limited.

Extend existing permission/MCP/bridge owners only where they cannot reach the
failure. No post-implementation unit tests or test-only production exports.

## Native route

Installed Claude 2.1.293 accepts the SDK's `mcp_set_servers` control on the
conversation process before its first user input. A credential-free probe
keeps the argv-attached LUNA endpoint, adds only the selected HTTP fixture,
and leaves user/project hooks and an unselected executable server inactive.
Real native Ask calls independently showed exact allow/deny callbacks, even
when a writing fixture advertised a misleading read-only annotation.

LUNA now stamps the host's provider configuration path and an opaque revision
after account/policy admission. The adapter selects enabled, already saved
HTTP/SSE definitions and explicitly consented project definitions. It sends
them over the private control pipe, waits for the correlated acknowledgement,
then sends the conversation input. Credentials are not put in argv, copied to
a new credential file or included in the journal. No connection registration
or saved permission rule is rewritten by this launch path.

Ask/Auto retain restricted mode, built-in tool selection, stdio permissions
and parent/machine/environment ceilings. Pending human approvals compare the
same selected connection definitions again before honouring a response. A
configuration change denies the old grant; the next safe-boundary launch
revalidates configuration. Metadata inspection uses the same scoped native
registration and disables hooks; it never sends a conversation prompt.

Full keeps Claude's ordinary native connection loading. Review-constrained
and no-file-edit runs receive no new external connection delivery. Stdio
servers are outside this scoped HTTP/SSE lane and remain unverified in
restricted modes. Auto uses the eligible native reviewer and may approve
without an SDK human callback; this is not an Ask approval result.

## Native QA scope

Work is isolated on `codex/sw02-scoped-mcp` in the existing completed RM-02
worktree, preserving its old cache/data and unrelated UI work in main. The
signed **LUNA QA Scoped MCP** uses IPC 59846 and its own data directory.
`LUNA_DEV_QA_MCP_SERVERS=executor` opts that QA bundle into exactly the existing
saved gateway. Default QA still suppresses ambient MCP connections; primary
LUNA Dev never inherits a QA allowlist.

An independently logged, disposable HTTP service is temporarily registered
in Executor's user scope as `luna_sw02_fixture_20261013.user.qa`. Its exact
namespace alone has a temporary Always-run policy; existing policies and
credentials remain owned by Executor. The fixture and its policy are removed
after evidence is preserved. Raw artifacts live under the worktree and main
`target/qa/luna-only-workflows/sw02-20261013/` directories.

The live cases use real provider sessions, actual command/approval IDs and
native transcripts; the service's nonce/request/effect log is the oracle.
`native_cases.py` queues one exact gateway call, takes milestone readbacks and
answers native approvals through `ExecuteTaskCommand` with fresh binding
generation. `toggle_qa_project.py` exercises only the disposable project's
native connection toggle, with no user message, bypass flag or hooks. It
restores the original enabled state after the stale-grant proof.

Haiku Auto is rejected by LUNA's model eligibility contract without a hidden
Ask fallback. The eligible Auto case uses the advertised Opus 5.5 model.
