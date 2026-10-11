# CORE-01 failure cases

Written before code. Each case names its owner and the independent evidence
that observes it. Owners: the operation catalog in `luna-rpc`, the
engine/remote policy that consumes it, the contract types in `luna-proto`, and
the generated contract artifacts.

| # | Failure | Observable (independent of the code under test) |
| --- | --- | --- |
| C1 | A method is dispatched by the engine but missing from the catalog, so it has no reviewed authority | The engine refuses an uncatalogued method before dispatch: a real RPC call returns `unknown method`, while a catalogued method reaches its handler |
| C2 | A newly declared method becomes remotely callable by default | `method_policy` gives a new catalog entry no remote scope unless one is written; a real remote session gets `method is local only` |
| C3 | Moving the remote scope, forwarding, stream and deadline lists into the catalog changes any existing decision | One-time equivalence of old and new predicates for every method, retained as evidence; existing `remote_auth` and forwarding tests stay green |
| C4 | The catalog and `methods::ALL` drift (the automation drift the audit found) | A generated-artifact check: catalog methods == declared constants == `ALL`, with no duplicate IDs or spellings |
| C5 | The contract's JSON Schema disagrees with the real wire: a wrong field name, a required field that is really optional, or wrong null or default semantics | Payloads recorded from a real engine **before** the change validate against the generated schema in an independent Node validator |
| C6 | An existing receipt or payload changes shape, or its historical `payloadHash` changes | The same scripted engine session recorded before and after, with identical receipt fields and hashes apart from volatile ids and times |
| C7 | Reordering JSON keys changes command identity, so an identical retry becomes a conflict or a second command | A real `ExecuteTaskCommand` sent again with reordered keys returns the original receipt, not `commandIdConflict` |
| C8 | An unknown provider, extension or content ID breaks an old reader, or is silently rewritten | The independent reader keeps unknown namespaced IDs verbatim. Legacy provider spellings round-trip byte-identically and map to their canonical IDs |
| C9 | A success reply is decoded as an error, or the reverse (an SDK decoding hazard) | The Node reader classifies recorded `ok`/`item`/`err` frames using only the generated frame schema |
| C10 | The generated artifacts go stale against the code | A drift check regenerates them and compares with the committed files |

Non-goals, owned elsewhere:
- The provider ID migration is CORE-04.
- Typed SDK clients and a typed error envelope are CORE-05.
- Extension grants are CORE-06.
- CORE-01 adds no new permission. A catalog entry is metadata plus a fail-closed
  gate; it never grants anything.
