# Global app homes and model catalogs

Observed on this Mac on 2026-09-24. The inventory below uses file names, JSON field names, SQLite schema, and local source code. Credential values and conversation contents were not read.

| App | Durable settings and state | Model discovery/cache |
| --- | --- | --- |
| Codex | `~/.codex/config.toml`, `AGENTS.md`, `skills/`, `plugins/`, `sessions/`, SQLite state and history | `~/.codex/models_cache.json` has `fetched_at`, `etag`, `client_version`, `identity`, and `models`. This is Codex-owned; LUNA asks the signed-in Codex app server for its account-visible `model/list` instead of parsing the private file. [Official configuration docs](https://learn.chatgpt.com/docs/config-file/config-basic). |
| BB | `~/.bb/bb.db` holds app settings, threads, plugins, and environment state; `plugins/`, `runtime/`, `thread-storage/`, and logs are separate. An optional `~/.bb/config.json` supports custom models. | `provider_model_catalogs` in `bb.db` is keyed by host, provider, and scope, with fingerprint, model JSON, and `fetched_at`. The installed BB CLI guidance says lists return the last stored catalog while a background refresh runs and keep that catalog after refresh failure. The `bb` executable was unavailable here, so this was checked through read-only schema inspection and its installed guidance. |
| T3 Code | `~/.t3/userdata/` holds settings JSON, keybindings, a model manifest, attachments, logs, and `state.sqlite`; `~/.t3/worktrees/` is separate. | `~/.t3/caches/<provider-instance>.json` stores provider status and models. `apps/server/src/provider/providerStatusCache.ts` validates instance identity and writes snapshots atomically; `ProviderRegistry.ts` hydrates them before background probes. Provider Settings offers manual refresh and a configurable health-check interval. The model manifest is a distinct file, not the per-provider cache. |
| LUNA before this change | `~/.luna/ui-settings.json`, `device-id`, `agent-accounts/`, `profiles/local/docs.sqlite3` and journals, plus logs. `LUNA_DATA_DIR` can relocate the app-owned root. | The picker asked providers on demand. Codex used live `model/list` but only had a compiled fallback; Claude's catalog was compiled. No durable LUNA model list existed. |

LUNA already has its own global home. The device root also holds harness preferences when saved; profile storage holds documents, journals, and LUNA instructions. The new cache belongs in that app-owned root. Provider authentication and private caches remain in the providers' homes.

## LUNA behavior added here

- The engine stores one JSON snapshot per harness and resolved provider authentication identity in `{data_dir}/model-cache/` (Codex and Claude identities when available). File names are hashes; cache files contain a schema version, refresh time, and picker models, but no provider credentials.
- Normal `ListModels` returns a valid cached list immediately. On a cache miss it queries the provider. Codex's bundled fallback remains available on a cold discovery failure, but is not saved as a successful live result.
- Once an RPC service starts, the engine waits two seconds, then refreshes installed providers in the background with at most two concurrent probes. A failed or empty probe leaves the last good file intact.
- Settings → Agents has **Refresh models**. It probes the selected device's installed harnesses with bounded concurrency, reports partial failures, and reloads open composer pickers after successful refreshes.
- Codex cache identity comes from LUNA's resolved execution account, including the native authentication identity. A switch to another Codex account does not reuse the previous account's catalog.
- Claude Code's read-only `initialize` response now supplies current model aliases and resolved IDs. LUNA enriches those rows with picker options and retains older curated IDs absent from the alias list. A failed probe does not replace a good cached result.

## Provider limits

Codex's `model/list` has no force-network option in its [request schema](https://github.com/openai/codex/blob/main/codex-rs/app-server-protocol/schema/json/v2/ModelListParams.json). Its own [model manager](https://github.com/openai/codex/blob/main/codex-rs/models-manager/src/manager.rs) uses a five-minute cache TTL. LUNA's manual refresh bypasses only LUNA's cache; Codex can still return its own recently cached catalog. Retry after a few minutes if a just-released model has not appeared. Other adapters that only return compiled catalogs still need a provider discovery route to reveal new models before a LUNA update.

Other useful patterns from the comparison are visible cache age/source in Settings (T3's `checkedAt`, BB's `fetched_at`) and optional scheduled refresh while LUNA remains open. BB also supports explicit custom model entries, which would need per-provider validation before LUNA could safely expose them. These are separate from the startup and manual refresh implemented here.

## Repeatable verification

Run `CARGO_INCREMENTAL=0 cargo test --locked -p luna-engine --test e2e model_catalog_cache_survives_refresh_failure_and_engine_reopen -- --exact`. The RPC-level scenario verifies disk creation, cached reads, forced refresh, retention after provider failure, and reuse after an engine reopen.

Run `CARGO_INCREMENTAL=0 cargo test --locked -p luna-harness --test claude models_come_from_the_initialize_control_request -- --exact` for the CLI-to-picker catalog path using the repeatable fake provider. On a machine with Claude Code installed, `CARGO_INCREMENTAL=0 cargo test --locked -p luna-harness --test claude live_models_discovery -- --exact --ignored` performs a read-only live catalog check without sending a model turn.
