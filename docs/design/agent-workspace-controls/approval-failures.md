# Approval policy failure cases

Written before the approval-path corrections requested on 2026-10-05.

1. A valid Full Codex command, file, additional-permission profile, MCP tool or
   computer-use permission request reaches the UI and parks the running agent.
   The real adapter against a deterministic peer must return the exact
   once/turn-scoped native response, with zero permission questions recorded.
2. Full accidentally creates a persistent/session permission rule. Inspect
   peer responses: ordinary grants are once-only; permission profiles are
   limited to the current turn.
3. Auto bypasses Codex or Claude's native review. Inspect startup/resume/turn
   parameters: Codex uses on-request plus auto_review; Claude uses native auto.
   Native escalation requests continue through the provider's input bridge.
4. A stale, canceled or foreign-turn request is automatically granted. Existing
   exact-context/turn checks must run before any automatic Full response.
5. Claude Full forwards a matched user ask rule as another LUNA permission card.
   The peer must receive a one-call allow without updated permission rules and
   without a false claim that the user answered that specific question.
6. A genuine model question, OAuth form or arbitrary MCP form is mistaken for a
   permission approval and filled automatically. Keep typed input/elicitation
   scenarios, including dismissals and cancellation, separate from approvals.
7. Built-in chat creation/message/layout work asks again in Auto or Full. E2E
   transcripts must contain the tool result and no approval event. Child
   escalation from a lower parent ceiling still requires an explicit grant.
8. A Full agent parks on LUNA automation approval or a second Full-mode picker
   confirmation. Choosing Full is the mode selection; valid built-in operations
   use exact live-call and revision fences without a permission card.

Evidence must include fixture identity, resulting events, native peer response
checks, counts and a repeatable command. These fixtures prove adapter behavior;
native app/provider checks are recorded separately.
