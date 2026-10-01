# MCP forms and URL prompts

Codex's `mcpServer/elicitation/request` primitive form and URL modes use the existing blocking question panel. A form asks for its fields, then requires an explicit Accept, Decline or Cancel response. The footer can decline or cancel from any page. Picking an MCP option does not submit or advance automatically. Optional fields can be skipped; valid schema defaults are shown. Validation errors retain the form and the engine refuses invalid responses without consuming its resolver.

Supported fields are strings, finite numbers, integers, booleans, string enums (`enum`, legacy `enumNames`, or titled `oneOf`) and multiselect enums (`items.enum` or titled `items.anyOf`). String lengths, numeric bounds, item counts, defaults, required fields and the `email`, `uri`, `date`, and `date-time` formats are validated. Titles map back to the exact wire enum values. Email validation checks basic address/domain syntax; it does not verify an address exists. Optional blank fields are omitted unless a default exists; a required unconstrained string can explicitly advance with empty text. Nested objects, unknown constraints/modes, malformed schemas, and obvious credential/payment-secret fields are declined. OpenAI-specific form/verification modes remain unsupported. The existing computer-use server retains its separate allow-once approval path; it cannot gain permanent/session allowance through a generic form.

Requests require the active native Codex thread and turn and a concrete server name and opaque request ID. The adapter owns that binding, passes immutable questions to the engine's run-owned pending map, and cancels their receiver when the native turn ends. Client responses carry only the engine request ID and answers; client-supplied schemas or routing metadata never authorize a response. Retransmitted native IDs share the original pending callback. Different IDs can be pending concurrently. Resolving one preserves the other prompt and its awaiting-input status.

Limits are 64 KiB of incoming request JSON, 16 fields, 64 enum choices per field, 4 KiB of text/URL input, 16 KiB of submitted content, 32 generic requests per native turn, and a two-minute lifetime. Expiry or a closed provider channel resolves the visible pending part. Stop closes pending inputs. A recovered unresolved prompt can only be closed locally with decline/cancel; accepting it never becomes a resumed model turn.

A URL prompt displays its HTTP(S) origin and an **Open website** action. Opening is separate from responding. The full URL, including any authorization query/fragment, is only held in the adapter and engine's live resolver: it is serde-skipped, removed before emitting `InputRequested`, and excluded from `Debug`. A copy of that exact URL in the server's message is replaced with the origin. Embedded URL credentials and non-HTTP(S) schemes are rejected. Server messages remain untrusted display text, never executed instructions.

`McpElicitationUrl` accepts `chatId`, `requestId` and the existing optional `targetDeviceId`. It returns the URL for that exact live run/request only, checks cancellation, receiver liveness and expiry, and does not accept or consume it. Unknown/stale IDs return generic errors without the URL. Repeated fetches are harmless; the UI prevents concurrent duplicate fetches. The existing remote authorization policy keeps this credential-bearing endpoint owner-only: unauthenticated and authenticated remote read scopes are denied. Synced viewers may see the destination and submit ordinary forms through their existing command authority, but cannot retrieve the raw URL with read grants. A restart intentionally loses the URL.

## Repeatable coordinator validation

Run this only from the coordinator's primary checkout after integrating the branch and reviewing conflicts. It uses the existing build/cache wrapper and creates no worktrees or apps:

```sh
scripts/qa-mcp-prompts.sh /Users/iulian/workspace/code/personal/luna-feature-checkouts/2026-09-30-codex-parity/evidence/mcp-prompts/coordinator
```

The script runs the focused compile, adapter/engine/RPC integration checks, computer-use regressions and Clippy. It preserves every log's exit status, emits four assertion artifacts, and creates a SHA-256 manifest. These are deterministic fixtures through the real adapter and engine/document/RPC paths, not proof of a real MCP server, model, or native interaction. The stop/restart check seeds the last pre-stop projection as an aborted snapshot to deterministically exercise orphan recovery, and separately checks actual engine Stop invalidates the live URL.

Native and real-provider evidence remains required:

1. Refresh and verify the single primary LUNA Dev bundle using the repository's normal handoff instructions. Record the exact commit, bundle process, IPC listener, signature, and build time. Do not run a second GUI app.
2. Through an existing Executor integration that actually requests a primitive form, verify a numeric error remains editable, valid zero/false values arrive at the server, titled/multiselect choices map correctly, optional/default handling works, Back preserves data, and Accept/Decline/Cancel have distinct readback. Discover/describe the tool using Executor's documented workflow; do not add duplicate registrations. If no installed tool elicits a form/URL, report that specific native/provider gap rather than label a fixture as provider proof.
3. For a real URL prompt, verify only the origin is displayed. Click Open website twice while the first fetch is in flight; verify one launch and that no answer is submitted. Respond separately. Repeat after Stop, expiry, and restart: the old request must not open a website or accept. Keep screenshots on LUNA's prompt; authorization websites may display secrets and must not be captured.
4. With two concurrent requests, resolve one and verify the next appears. Check a request on a different chat/thread/turn cannot open or answer this one. Confirm computer-use still offers only its established one-request approval.
5. Save screenshots, targeted receipts, journal/document readback and assertion notes outside the checkout. Search the evidence for an agreed non-secret URL marker, ensuring only the deliberate live URL RPC return/browser location contains the full URL. Do not retain actual authorization URLs or real credentials in evidence. Hash the final evidence after the native steps.

Worker submission includes static checks only. Compile, integration and native verification are pending until the coordinator records them.
