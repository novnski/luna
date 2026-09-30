# Mermaid diagrams in chat

Assistant Markdown fences labelled `mermaid` (case insensitive), including fences nested in quotes or lists, use the same local native adapter and SVG preparation as [Files preview](markdown-preview.md). Rendering runs on the background executor, with no external diagram service or executable HTML. Other Markdown, images, links and code languages keep their existing chat behavior.

A streaming fence stays as copyable source until its closing delimiter arrives. Completed messages and reopened messages render eligible diagrams automatically. A closed diagram can render while the remaining reply still streams. A mutable quote/list container larger than 128 KiB stays as source until the parser considers it stable or the message completes, avoiding a second unbounded parse of that container.

Rendered diagrams use the shared fence frame and Copy action. Show source reveals the original fence with the existing Copy, Fit content and horizontal scroll controls. The choice follows the stable fence identity through streaming completion and is cleared on chat attachment. Pending, failed and limited diagrams show their source; failed or limited rendering includes a diagnostic. The diagram toggle becomes available after rendering succeeds.

The adapter uses the current theme and font. An appearance or font change discards old media and rejects old render results. Changing chats, removing a source, or disposing the transcript releases retained media and rejects results that no longer belong to the view. A single background task per transcript requests only laid-out diagrams; the shared adapter serializes layout across all views.

A transcript admits its first 32 distinct eligible sources and retains up to 64 MiB of media, including estimated texture memory. Sources over 16 KiB, 256 lines or 2048 lexical segments and generated SVG over 2 MiB use source fallback. Images fit the chat column, stay at or below their natural size and 480 px height, and use the existing bounded SVG raster preparation. Memory admission is checked again after DPI/viewport changes. Failures are cached until the source, chat or style changes.

The embedded `mermaid-rs-renderer` supports the existing Files corpus but does not promise complete Mermaid.js syntax or pixel-identical layout. It has no cooperative cancellation or hard execution deadline; input limits and background serialization do not guarantee a fixed execution time.

## Repeatable validation

The failure scenarios for this feature were recorded before implementation. The native fixture exercises production transcript parsing/rendering and the local adapter with synthetic documents, no engine/provider/network/user profile, and no Git worktrees. This does not prove real-provider delivery or full Shell interaction.

The coordinator runs these commands from the validated checkout, with its build guard and one-GUI-app policy:

```bash
scripts/cargo.sh check --locked -p luna-ui
scripts/cargo.sh build --locked -p luna-ui --example chat-mermaid-fixture --features appshots-fixture
scripts/qa-chat-mermaid.sh /absolute/path/to/evidence/native-fixture
```

The fixture saves PNG and JSON for partial/closed fences, longer fences with CRLF, streaming completion, light/dark invalidation, nested list/quote diagrams, malformed/oversized source, 33 distinct sources and chat switching. A second process reads the persisted synthetic document and renders it again. Pass receipts and SHA256SUMS make the evidence repeatable. A failed assertion keeps the failing screenshot/readback and returns a nonzero exit status. Inspect PNGs for readable labels, sizing, visible diagnostics and preserved ordinary Markdown/image fallback; cache counters alone cannot prove visual correctness.

For real-provider/native acceptance, use a disposable authorized chat in the coordinator's refreshed LUNA Dev app. Ask Codex to return the six diagrams in `scripts/fixtures/markdown-preview/`, with prose, ordinary Rust code and an image reference between them. Observe a fence close while the reply continues. Confirm all supported diagrams, Show source/Show diagram, Copy's exact source, Fit content, links and image fallback. Switch light/dark and resize to a narrow column. Navigate away during rendering, return, then gracefully restart the app and reopen the chat. Confirm persisted diagrams and source again. Repeat malformed and oversized fences and more than 32 distinct diagrams in a disposable fixture chat. Retain source SHA, prompt, original document/protocol readback, screenshots and hashes. Record native/provider acceptance separately from the synthetic fixture.
