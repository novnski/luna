# CORE-04: dynamic provider identity and discovery

Baseline: `71087aa0`. Live ownership and acceptance belong to the shared queue.
The user requested simple implementation, functional delivery first, Simulator
QA for iOS changes, and the test-audit authoring gate.

## Bounded implementation

1. Use CORE-01's existing provider-reference syntax and legacy aliases for
   runtime/persisted IDs. Keep the original wire spelling when serializing.
   Unknown namespaced IDs remain readable and explicitly unavailable. Built-in
   adapters retain a compatibility classification; adding a provider does not
   add a public enum variant. Do not introduce a package loader or worker here.
2. Extend the existing registry with collision-safe registration, removal and
   generation-tagged discovery. Preserve preferences for missing providers.
   Publish changes through the normal RPC subscription and update the existing
   picker. Catalog results retain host/account/executable/generation provenance;
   late results cannot change another target or account.
3. Verify migration/restart and live registry/picker behavior at real engine
   boundaries. Keep current adapter protocol keepers. Retain commands, exit
   codes and hashes, refresh LUNA Dev and deliver on verified main.

## Failure contract and test authoring gate

| Behavior / owner | Credible regression | Observable proof |
| --- | --- | --- |
| Persisted provider identity / real documents + engine restart | An unknown ID prevents history from decoding, or aliases change the stored task/session/account identity | Independently written old/new records remain readable through restart; original transcript/payload hashes and native/account bindings remain |
| Registry / real RPC | A collision replaces a live provider, removal silently selects a different provider, or missing preferences are erased | Refuse collision, publish the exact changed registry, preserve original saved selection and return unavailable for a missing binding |
| Catalog provenance / real discovery and picker | A delayed result from another host/account/executable/generation is applied | Accept only the matching current provenance; existing task bindings and permission mode remain |
| Live picker / native GUI + registry watch | Enable/remove requires restart or a local-only notification never reaches another client | Existing picker reflects the authoritative registry generation and maintains its saved selection |

Existing native-provider conformance and restart owners keep their coverage.
New integration scenarios cover only dynamic identity, registry mutation and
catalog provenance risks those owners cannot exercise. No test-only production
exports, golden admission receipts or duplicated private predicates are added.

## Delivered

`HarnessId` is now an opaque, bounded provider reference using CORE-01's
validated syntax. Legacy and namespaced spellings compare by canonical identity
while serialization preserves the received spelling. Built-in implementation
branches use a compatibility classification; new IDs need no public enum case.
Account directory keys also distinguish namespaces. The inline value keeps
existing immutable bindings copyable; large error payloads remain boxed.

The existing registry now offers collision-safe registration, removal, typed
capabilities, installed/enabled/activated/available projections and a unique
registry lifetime plus monotonic generation. Factories must return the requested
identity. Missing and retired preferences remain saved. Automatic retired
conversation deletion was removed: this intentionally replaces the cleanup
policy introduced by `1d635117`, rather than weakening that policy's test.

`GetProviderCatalog`, `WatchProviderCatalog` and `GetProviderModels` use the
existing reviewed RPC operation catalog and environment-read authority. The
watch acknowledges readiness, reports registration/enablement/removal, and
probes installation/source changes without launching providers. Native source
identity comes from the actual launch resolver's canonical path and file
metadata; compiled adapters use their implementation/version identity.

Model discovery carries host, registry lifetime/generation, provider, execution
account, authentication scope and executable provenance. Discovery rejects
changed sources/accounts/registries before publishing; cache v3 records the
source for every provider and uses canonical ID/account keys. The old derived
cache is rebuilt on demand. Conversation and session data are not rewritten.

The existing picker subscribes to its execution host, retains its saved choice,
and rejects late results after host, registry, executable or account refreshes.
Legacy catalog/model RPC shapes remain available; older hosts can use the
picker's unary fallback. Models keep their existing typed reasoning/options
shape. No password, Keychain, package loader or provider worker was added.
Separately installed provider execution remains CORE-08's responsibility.

## Verification and evidence

Evidence directory: `target/qa/core04/20261012T210735Z/`.
`SHA256SUMS` covers retained logs, JSON reports, native captures and the scoped
RPC reproduction script. `migration-final.json` includes transcript/config
hashes and preserved native session/account/permission bindings.

| Check | Result |
| --- | --- |
| `CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-engine --test provider_scope_e2e --test model_cache_lifecycle --test task_identity --test usage_account_selection -- --nocapture` | 10 passed; zero failures/ignores |
| `scripts/cargo.sh clippy --locked -p luna-proto -p luna-harness -p luna-engine -p luna-ui --all-targets` | Exit 0; existing warnings retained |
| `scripts/cargo.sh run --locked -p luna-rpc --features schema --example contract` then isolated `scripts/cargo.sh test --locked -p luna-rpc --features schema --test contract` | Published catalog regenerated; 3 passed |
| `scripts/cargo.sh check --locked -p luna-ui --features appshots-fixture --examples` | Exit 0 |
| `scripts/cargo.sh fmt --all -- --check` and `git diff --check` | Exit 0 |
| Native QA | Missing provider readable through restart; original model/access preserved; external OpenCode disable/re-enable changed the same open picker, with Claude model/reasoning/options preserved |

E06's integration owner replaces the old deletion expectation and adds distinct
real RPC subscription/collision/removal and delayed-discovery rejection cases.
The discovery-only adapter cannot execute tasks and makes no native-execution
claim. Existing fixture consumers were adapted mechanically to classification;
two stale fixture omissions from the previous Connections feature were fixed
so all-target and configured example compilation succeeds.

Native QA used `LUNA QA Discovery`, bundle `com.novnski.luna.qa.discovery`,
IPC `59843`, and isolated data `target/luna-qa-data/core04`. Build took 90s.
The captured watch generations were 10 → 12 → 14; the registry lifetime changed
after restart. Its exact process/port/signature were verified, then its evidence
and bundle hashes were retained, it quit gracefully, and only its disposable
bundle and empty synthetic Git fixture were removed. QA data/cache were retained.

The primary **LUNA Dev** refresh took 45s; PID `79283`, IPC `59777`, exact bundle
path and strict code signature were verified. No iOS source changed and no
physical phone was used. These results certify discovery/migration/picker
behavior; they do not certify a separately installed provider's execution.
