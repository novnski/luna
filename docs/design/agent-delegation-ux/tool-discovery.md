# Tool discovery and compaction

Source comparison made against local T3 Code commit `4ee6bfd50e` and LUNA
`a08e9344` before this follow-up. T3 files were read only.

## What T3 actually does

- `apps/server/src/provider/CodexDeveloperInstructions.ts:197–224` builds
  named `turn/start.additionalContext` entries of kind `application` for
  orchestration, runtime identity and available tools. Its comment explains
  that Codex resends a value only when it changes. These instructions stay
  outside collaboration-mode instructions, which a model's own mode definition
  can replace. `Adapters/CodexAdapterV2.ts:727–751` attaches this context only
  when the actual provider session has the T3 MCP connection.
- `Adapters/ClaudeAdapterV2.ts:907–914` appends runtime and orchestration text to
  Claude's system-prompt preset and passes MCP servers as session options.
  This is provider configuration, not an ordinary user-message prefix.
- `provider/T3OrchestrationInstructions.ts:90–105` supplies a first-run-only
  prompt wrapper for adapters without a system/developer channel. That fallback
  is distinct from the Codex and Claude paths.
- `mcp/McpProviderSession.ts` and `ProviderSessionManager.ts` manage the
  per-thread/provider-session MCP endpoint and credential lifecycle.
  Registration exists independently from conversation summaries.
- `mcp/toolkits/orchestrator/tools.ts` explicitly describes asynchronous
  delegation, completion notifications and bounded blocking waits. It tells
  agents to continue/end their turn instead of polling for independent work.

This is a source-backed architecture comparison, not a claim that every T3
adapter or account was exercised live in this task.

## LUNA's approach

LUNA already attaches its MCP endpoint on supported Codex/Claude process starts,
including resumes. Codex receives the runtime contract through the thread's
`developerInstructions`; Claude receives it through `--append-system-prompt`.
These are separate from ordinary user input. Forks receive their own runtime
identity when started. Native compaction does not require copying a tool manual
into a user message.

This follow-up keeps that existing transport and reduces the persistent LUNA
bootstrap to a short entry point and trust rules. `get_workspace` returns a
small guide index by default. Optional `topic` selects focused guidance for
delegation, workspace actions, defaults or references. Agents can rediscover
usage after compaction without relying on a summary to retain every parameter.
The actual attached catalog remains authoritative; unsupported adapters are not
described as having tools they cannot call.

The asynchronous path is explicit: `create_thread` returns after durable
creation; a helper runs independently. `reportResults:true` displays its result.
`continueParent:true` additionally queues a result-triggered parent turn after
active work. The default leaves the parent idle. `wait_threads` blocks only its
bounded call and is for work that actually depends on the answer. A timer or
recurring reminder is unnecessary for completion delivery.

Native Codex manual compaction retires the exact idle provider writer through
the existing graceful EOF path, preserving its binding and excluding a new
dispatch until the maintenance operation and durable readback finish. It uses
a three-minute inference deadline rather than the ten-second metadata limit.
Completion still requires the matching provider event; timeout remains an
unknown, non-replayed outcome. This handoff prevents a subsequent warm turn
from retaining stale pre-compaction context.

## Verification contract

- The default guide stays below 800 serialized bytes in the engine E2E; topic
  help is opt-in and rejects unknown topic names.
- The delayed-child engine E2E completes independent parent work, queues a
  continuation behind another active parent turn, and verifies exactly one
  continuation after restart. Provider-facing provenance text is preserved.
- Native adapter/session tests must distinguish fresh start, warm reuse,
  resumed/forked startup and compaction. Assert the actual catalog and a
  successful post-boundary discovery call, not merely a configured MCP name.
- Record instruction size and ensure ordinary user prompts do not receive a
  repeated manual. Static provider instructions still occupy context; this is
  bounded reuse, not a claim of zero token cost or guaranteed cache billing.
- Claude's current adapter does not support out-of-turn manual compaction.
  Exercise its automatic compact-boundary path in a deterministic provider
  fixture and report the real-provider limitation explicitly.
- Keep metadata probes and disposable QA isolated from unrelated user MCP
  servers while preserving normal user session connections. The accompanying
  MCP-startup investigation owns that launch-policy verification.
