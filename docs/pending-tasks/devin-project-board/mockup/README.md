# Saved Board mock

**Current design:** Backlog / Ready / Done, temporary reviewer side chats, and separate manual versus conversation-only ticket authoring. This is an interactive local simulation with sample data, not an implemented LUNA feature.

- [Open the interactive mock](board.html): standalone export for browser viewing.
- [Editable source](board.fragment.html): the HTML fragment used for the in-conversation preview and this export.
- [Verification manifest](verification.json): hashes, repeatable interaction steps, observed results and limits.
- [Execution contract](../board-lifecycle.md): real-engine start/stop/resume requirements.
- [Reviewer contract](../automatic-review.md): placement, ownership, successful close/archive and retained evidence.

Open `board.html` in a browser. If the viewer only shows source, serve this directory with a local static server and open `board.html` from it. Typography uses a static font CDN and has local fallbacks; no real provider, GitHub, LUNA RPC, or production service is called. Model names are sample selections, not a live account-availability claim.

## Explore the interaction

1. Drag **LUN-42** from Backlog to Ready. It creates one main chat and records a first-start instruction. Start buttons are the keyboard-accessible equivalent of this move.
2. Drag it back to Backlog. Watch **Stopping → Stopped**. The mock uses a short simulated delay; the real app must wait for actual termination evidence.
3. Drag it to Ready again. It reuses the chat and records a **Continue** instruction. Open the ticket's Dispatch history to see first-start and continuation counts/text.
4. Use **Simulate review** in the preview controls below the app. This advances a simulated agent phase; it is not a required user step in the planned product. For this main ticket, the reviewer is a side chat under the ticket.
5. Use **Simulate success & cleanup**. The ticket moves to Done, the reviewer disappears from active tabs/listing, and its archived history remains accessible through Review details. The primary chat stays.
6. Open **LUN-41**, initially reviewing in a side chat. Its reviewer is a sibling under **Release planning** and reports to that ticket. Stop/continue and successful cleanup must leave unrelated siblings/main chats alone.
7. In Write manually, enter title/description and choose implementation harness/model, reasoning, access and reviewer. Save to Backlog creates no chat; open the ticket to inspect its saved settings.
8. In Discuss with AI, use the temporary conversation. There are no title/description inputs. Add multiple messages, answer its execution-settings question, optionally change the discussion assistant independently, and create the ticket from its read-only proposal.
9. Closing an unfinished discussion keeps only a resumable Board draft, as confirmed by the user. It adds no normal chat. Saving then closing ends the authoring session; Discard removes the draft.
10. Open Release planning and use Create ticket from this chat to demonstrate a board action inside an ordinary conversation. It returns a ticket receipt without starting another chat. This is a local stand-in for the planned internal MCP operation, not a live MCP call.

The authoring assistant uses local demonstration responses, not a live AI model, and does not actually inspect project files. Project-aware context/tool delivery and real account-visible model catalogs require the planned engine implementation. Discussion and implementation profiles are displayed separately to make that design distinction testable. Very large interactive preview state remains local to the open preview rather than exceeding the host's state-size limit.

The demo has an explicit sample Opus 5.5 review selection. In the actual product, when no project/ticket review selection is configured, the confirmed fallback is the implementation model in a separate reviewer chat. Automatic repairs stop after two repair/re-review rounds if blockers remain. Existing ticket chats always keep their original parent; the mock does not expose reattachment controls.

Reordering a ticket within its current column does not send or stop anything. Dropping on Done does not manufacture a successful review. Changes are local to the preview. The saved source starts with five sample tickets; use a fresh preview state when repeating the scenario counts.

## What was checked

Browser actions and visible DOM/accessibility readback verified real drag-to-start and drag-to-stop in the rendered document, same-chat continuation, the stop barrier during rapid toggles, both reviewer placements, successful archive/view closure, preserved primary chats, and read-only archived review access. The IAB's native coordinate driver rejected fractional iframe coordinates in the standard export, so drag gestures were exercised against the renderer's exact inner document; ordinary rendering/navigation was also inspected in its standard iframe wrapper. This is a test-harness accommodation, not a claim of LUNA runtime execution.

The fragment and standalone export are hashed in `verification.json`. JavaScript syntax, document links and whitespace were checked. Required real provider/engine E2E remains in the main plan.

## Regenerate the export

Use the installed visualize skill's `scripts/render.py` with `board.fragment.html` as source, `board.html` as destination, and `--force` when replacing a reviewed export. The export preserves only the local `widgetState` / `setWidgetState` host API usage. Re-run the interaction checks and refresh the manifest hashes after changing the source.
