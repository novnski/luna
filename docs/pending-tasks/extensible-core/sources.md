# Sources and evidence

## Repository and live boundary

- Repository cut: `2838125e42646b5d5ba86a3f8dfbdd93922ec60d`.
- [Inventory summary](evidence/inventory.json): tracked owned source scope,
  file/line counts, largest files, direct workspace dependencies and provider-ID
  impact index. Physical lines include comments and tests.
- [Per-file inventory](evidence/files.jsonl): path, source hash, physical lines,
  bytes, textual test-attribute count and `HarnessId::` occurrences. These are
  measurements, not an exhaustive semantic audit or executed-test count.
- [Read-only Dev CLI result](evidence/live-inspect.json):
  `target/luna-macos-dev/LUNA Dev.app/Contents/MacOS/luna inspect --json --port 59777 status`
  returned exit 3 / connection refused. No native app, provider run or new plugin
  was exercised. No performance baseline is claimed from this planning pass.
- Current source findings and exact entry-point references are in
  [source-audit.md](source-audit.md).
- [Planning verification](evidence/planning-verification.json) records link/hash
  checks, unchanged captured runtime source, queue admission and fresh-store
  restore/selection. [Queue update](evidence/queue-update.json) records preserved
  state for all 66 original tickets; [hash manifest](evidence/sha256.json) covers
  the evidence files. These checks validate the plan and registry, not future
  CORE runtime acceptance.
- [Recent test audit](../../audits/2026-10-09-test-audit/README.md),
  [tool-output contract](../../reference/tool-output.md),
  [build-cache policy](../../reference/build-cache.md),
  [native QA guide](../../reference/qa-infrastructure.md),
  [macOS performance](../../performance-macos.md) and
  [idle/graphics attribution](../../performance-idle-presence.md) preserve useful
  existing behavior and evidence. Historical totals are labeled as historical.

## External primary references

Read on 11 October 2026. These establish implementation patterns and limitations;
they do not prove a LUNA runtime or choose a library version automatically.

| Source | Relevant fact | LUNA interpretation |
| --- | --- | --- |
| [Rust Reference: ABI](https://doc.rust-lang.org/reference/items/external-blocks.html#abi) | Rust's native ABI has no stability guarantee. | Independent plugins use versioned serialized/component contracts; private Rust trait objects/GPUI memory are not the public binary interface. |
| [VS Code extension host](https://code.visualstudio.com/api/advanced-topics/extension-host) | Extension placement distinguishes local, web and remote hosts. | Model execution-host and viewing-client contributions separately, with explicit package/platform capability. |
| [VS Code contribution capabilities](https://code.visualstudio.com/api/extension-capabilities/overview) | Extensions use commands/configuration/views and custom webviews through supported APIs; they do not access the workbench DOM directly. | Publish contribution points and a supported native view model; preserve shell ownership. |
| [VS Code manifest](https://code.visualstudio.com/api/references/extension-manifest) | Manifests describe engine compatibility, entry points, contributions and dependencies. | Version and validate installation metadata independently of activation and execution grants. |
| [Zed extension development](https://zed.dev/docs/extensions/developing-extensions) | Zed supports declarative files plus Rust code compiled to Wasm and local dev extension installation. | Data-only and executable plugins can share one package workflow; prototype a real local authoring path. This does not imply Zed supports LUNA's proposed arbitrary sidebar/media extensions. |
| [Zed capabilities](https://zed.dev/docs/extensions/capabilities) | Extension host operations such as process/download access can be capability-restricted. | Explicit brokered operations need actual enforcement and useful denied-capability errors. |
| [Wasmtime security](https://docs.wasmtime.dev/security.html) | Wasm isolation and WASI filesystem capabilities are distinct host/runtime mechanisms. | Keep imports narrow and audit host services; Wasm cannot make an overpowered import safe. |
| [Wasmtime runtime configuration](https://docs.wasmtime.dev/api/wasmtime/struct.Config.html) | Runtime configuration exposes execution controls including fuel/epoch mechanisms. | Prototype cancellation/metering and select limits from measurements; no automatic performance/sandbox claim. |
| [WASI security](https://wasi.dev/security) | Capability-oriented APIs differ from native processes' ambient OS authority. | Separate portable sandboxed logic from explicitly trusted native/provider workers; test any claimed platform confinement. |

The architecture, task decomposition and dependency decisions are original LUNA
recommendations based on these patterns and the inspected source. They are not
upstream guarantees, copied plugin compatibility, or a requirement to import
another editor's runtime.
