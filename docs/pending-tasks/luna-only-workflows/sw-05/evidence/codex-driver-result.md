# OpenCode QA live run: sw05-codex (run)

- Provider: codex; model gpt-6.1-sol; variant (none); permission full.
- Evidence tier: **T3** through the signed QA app's RPC surface.
- App: LUNA QA Transcript pid 6540; LUNA 4128f083de0ece28cea4727f382fe696706ccfd8 (dirty=true).
- Result: **PASS**

| Criterion | Expected | Actual | Pass |
| --- | --- | --- | --- |
| model-in-catalog | requested or tier default model listed by ListModels | gpt-6.1-sol | yes |
| tier-label | T3 uses a real (non-mock) model | gpt-6.1-sol | yes |
| chat-created | createChat applied | applied | yes |
| turn-0: turn-0-settled | session reached idle within the turn timeout | settled:idle via working>idle | yes |
| turn-1: turn-1-settled | session reached idle within the turn timeout | settled:idle via working>idle | yes |
| send-1-once | the user message appears exactly once in the transcript | 1 | yes |
| send-2-once | the user message appears exactly once in the transcript | 1 | yes |
| secret-scan | no credentials in evidence (OCF-142) | {"passed": true, "findings": 0, "filesScanned": 15} | yes |

## Unverified by this run

- Native server readback (session record, message pages, pending asks) beyond what the wire log captured: the server password stays private to LUNA.
- Model consumption claims; a model statement is not verification (validation-plan §1 item 7).
- Screenshots prove presentation only; RPC admission proves admission, not consumption.

## Replay

```
node scripts/qa-opencode-live.mjs run --tier T3 --provider codex --scenario sw05-codex --permission-mode full --turn-timeout 420 --prompt-file target/qa/luna-only-workflows/sw05-20261011T160312Z/prompts/order-codex.txt --prompt-file target/qa/luna-only-workflows/sw05-20261011T160312Z/prompts/viewport.txt --evidence-root target/qa/luna-only-workflows/sw05-20261011T160312Z/live
```
