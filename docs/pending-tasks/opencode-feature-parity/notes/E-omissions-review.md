# E: Independent omissions review

Workstream E, reviewer only. Status: **complete** (2026-10-07 ~00:45 EEST). Read-only: file reads, `rg`, `git grep`/`git show` on the OpenCode repo, and Python over the Markdown tables and the committed `openapi.json`. No deliverable was edited.

Inputs reviewed: `notes/00-baseline.md`; notes A, B, C, D; `capability-inventory.md`, `tool-and-event-matrix.md`, `luna-coverage-audit.md`, `implementation-plan.md`, `validation-plan.md`, `coverage-review.md`, `research-baseline.json`, `README.md`, `HANDOVER.md` (as of 00:40). Source checks against the v2.0.22 snapshot (`527f0b93`) and LUNA's working tree (the adapter file is unchanged from the pin).

## §1 Summary

- **Prior-parity rows (task 1):** all 524 D rows mapped individually (§3): **342 mapped** (319 with a scenario step, 23 where no step actually exercises the row), **181 excluded with a reason** (175 stated in the deliverables, 6 where this review supplies the reason), **1 omitted** (RS-1, shared approval-card AX defect).
- **Coverage tables (task 3):** 63/63 docs pages, 29/29 API families, 94/94 events and 183/183 A rows are covered. The §4 per-family operation counts are docs-sidebar numbers, not the "136-operation OpenAPI" the heading claims (server/credential/vcs differ).
- **ID consistency (task 4):** no dangling IDs. OCE-00 and OCE-02 are owned by no package acceptance. 8 OCE "WPs" cells disagree with the acceptance text. OCF-15 and OCF-165 are orphaned from scenarios. F-INP-13 and F-AGT-01 have inconsistent WP/E2E cells.
- **Spot checks (task 6):** 17 claims checked. 15 confirmed, 1 confirmed with a contradiction (fork `before`), 1 partly refuted (OpenAPI op counts), plus the F-SES-06 "OpenAPI requires `before`" sub-claim refuted. **None of the P0/P1 drivers was refuted:** G-0, the `decision` and `name` bodies, the `form.created` shape, `shell`/`subagent`, the 4 KiB cap, the `RuntimeDelivery` default, the managed-only claim sweep, the probe order, cumulative usage, and `--stdio` deleting the password all hold.
- **Findings:** 1 high, 6 medium, 14 low, 1 info (§2). The high item is the contradictory fork-boundary guidance in the matrix's endpoint table.

## §2 Findings

| # | severity | location | finding | suggested fix |
|---|---|---|---|---|
| 1 | **high** | `tool-and-event-matrix.md` §2 row E15 vs `implementation-plan.md` OC-12 step 1 and §2 payload example | Contradictory fork-boundary guidance. E15 says "use a real **assistant** message id as the checkpoint"; OC-12 and the payload example say `{before: <user msg id>}`. `before` is **exclusive**: `oc2:core/src/session/projector.ts:136-138` copies `seq < boundary.seq` (`lte` only when `before` is omitted). An assistant id would drop the very reply the user forked at. HANDOVER step 5 points implementers at §2 as "exact endpoint and body fixes". | Change E15 to "`{before}` = the first message **after** the kept prefix (the next user message); omit `before` to fork through the newest message", and cite `projector.ts:136-138`. |
| 2 | med | `coverage-review.md` §7 last row; §6; §8 | §7 labels all Codex supporting audits "reference only". D §1.E counts only BB/T3/competitor (45) and Claude reference integrations (14) as reference rows. CXA (19), CXG (12), CXTS (11), CXNS (6), CXSS (16) and CXGV (8) are 72 requirement-bearing rows. §6/§7 also don't map CCI, CCT, CCM, CCC, CCH, CCL, CC-G, CCR, RS, COR, the 6 contracts or PAR rows individually; §1 defers that to §8. | Relabel the §7 row: "BB/T3/competitor rows reference only; CXA/CXG/CXTS/CXNS/CXSS/CXGV mapped row-by-row in E §3". In §8, cite E §3's counts (524 = 342 mapped, 181 excluded, 1 omitted). |
| 3 | med | `validation-plan.md` OCE-17; `implementation-plan.md` OC-07 step 3 | OC-07 adopts native steering only after T2 proves acceptance vs incorporation (PD-2). OCE-17 doesn't contain the canonical steering fixtures from the prior inventories: held long command + steer (CXSS-R1, CXNS-1), two parallel tools + two steers (CXE-09, CXSS-R2, CXNS-2), steer after the turn ended or with a stale id (CXNS-4), and Stop while a steer is pending, with and without `?resume=true` (CXNS-6, CXT-12, F-TURN-03). F-TURN-03 points at OCE-17, which only cancels a pending steer. | Add OCE-17 steps (f) held 20 s marked command + steer, (g) two parallel tools + two steers (order kept), (h) steer after `execution.succeeded`, (i) Stop with a pending steer, then the `resume` variant. Add pass criteria tied to `session.inbox.delivered`/`cancelled`. |
| 4 | med | `implementation-plan.md` (all packages) vs `validation-plan.md` §4 WPs column | **OCE-02** (duplicate and out-of-order events, a brief-required category) lists OC-06 and OC-08 as owners, but no package's acceptance names it. **OCE-00** (baseline blocker reproduction) is described in OC-01's acceptance but never named. Smaller WP-column mismatches: OCE-03 lists OC-07 (OC-07 acceptance omits OCE-03); OCE-05 lists OC-03; OCE-14 lists OC-13; OCE-19 lists OC-12; OCE-20 lists OC-09; OCE-21 lists OC-11. None of these packages name the scenario. | Add "OCE-02 (replay: duplicated, late-delta, early-terminal, re-emitted ask, unknown type)" to the OC-06 and OC-08 acceptance, "OCE-00" to OC-00/OC-01, and reconcile the other six WP columns with the acceptance text (one direction or the other). |
| 5 | med | `capability-inventory.md` F-TOOL-08/09 (E2E OCE-06/OCE-16); `validation-plan.md` OCE-06, OCE-16 | No scenario renders a **web result**. OCE-06 covers shell/edit/patch/read-image; OCE-16 covers only the webfetch *ask* and the websearch provider *form*. Prior rows CXM-08, CXL-6 (source cards), CXE-05 (labels and sources), CXTS-4 and CCT-06 therefore lack evidence. The matrix target ("keep `format`") is otherwise untested. | Add an OCE-06 (or OCE-22) step: a webfetch of a fixture URL plus a websearch through a mock provider. Assert the typed card, query/URL, result body via full-output fetch, and failure display (provider 429 → failover, F-TOOL-09). |
| 6 | med | A §8 (19 doc/source disagreements) vs all deliverables | The brief asks for doc/source disagreements, but the deliverables only cite A §8 #16 (stale 2.0.6 links, coverage-review §3). Items like #4 (`disable-model-invocation` documented but absent in 2.0.22), #6–#8 (explore/`external_directory`/compaction model drift), #10–#12 (undocumented `subagent.model`, Code Mode tools, MCP elicitation), #3 (ACP child updates gated) and #9 (`username`) appear only partly, inline, without the disagreement framing. | Add a short "Doc/source disagreements" table to `coverage-review.md` (or a §3 footnote) listing A §8 #1–19 with the affected F-row and the version where it resolves. |
| 7 | med | `implementation-plan.md` OC-13; `validation-plan.md` OCE-18; `capability-inventory.md` F-PERM-12, F-EXT-12 | Brief item "context inheritance": what a **native subagent** inherits from the LUNA runtime contract is not addressed. `mcp.add` is a per-location in-memory override (`oc2:core/src/mcp/index.ts:635-639`), so a `luna` MCP attached for the parent is presumably visible to child sessions in the same location (E5). Children run under their own agent rules (F-PERM-12), and instruction entries are per-session, so a child may call `luna` tools without the parent's runtime contract or authority attribution. OCE-18 tests four start kinds but not a child session. | Add to OC-13: decide whether `luna` tools are visible to native children, how calls from a child session are attributed and authorized, and whether instruction entries must be copied to children. Add an OCE-18 step: a native subagent lists tools and attempts one `luna` call. Add a Q-ledger entry until it's decided. |
| 8 | low | `capability-inventory.md` F-SES-06 evidence cell | "OpenAPI requires `before` with `additionalProperties:false`" is refuted. OpenAPI `session.fork` requires a request **body**, but `before` is optional (no `required` array; source `Schema.optional`, `oc2:protocol/src/groups/session.ts:314`). | Reword: "body object required; `before` optional; `additionalProperties:false`". |
| 9 | low | `coverage-review.md` §4 heading and op counts; `research-baseline.json` `opencode_entry_points_v2.0.22.server`; B §3.5 | "136 operations" is the committed `packages/protocol/openapi.json`, but the per-family counts are docs-sidebar numbers (sum 141): server 3 vs 1, credential 5 vs 3, vcs 6 vs 5. The 2.0.22 **source** groups already define `server.pair`, `server.connect`, `credential.list` and `credential.create`, which the committed JSON lacks. So B §3.5's "pairing/credential routes added in 2.0.23–2.0.24" describes the JSON artifact, not the code. The boot fingerprint (OC-02) reads the **live** `/openapi.json`. | State the three numbers and their sources (committed JSON 136, source groups 140, docs sidebar 141). Note that the coordinator's "OpenAPI per tag" checks used the committed artifact, which may lag the source. Have OC-00 capture the live `/openapi.json` once as the fingerprint baseline. |
| 10 | low | `coverage-review.md` §9 | Notes questions not carried into the ledger: null-body handling for `compact` and `interrupt` (B §6 Q2, C §8 Q2; drives G-13); v2 message-list item shape used by fork readback (C §8 Q6); hidden `summary` agent callers (A §9 #1); `models.opencode.ai` vs models.dev catalog host (A §9 #7); whether Console-delivered MCP servers appear in `mcp.list` (A §9 #9). | Add them as Q-23..Q-27, or mark them closed or out of scope with a reason. |
| 11 | low | `capability-inventory.md` (version tags), A §5.4 | Head-only drift relevant to OC-13/OC-06 is missing: tool name/namespace length caps dropped (`c2f87853c6a`), and the server always reports `opencode` as the MCP client name (`b78d10cd716`). Both affect `luna` MCP tool naming and `<server>_<tool>` parsing on upgrade. | Add `head:` notes to F-TOOL-17 and F-EXT-12. |
| 12 | low | `capability-inventory.md` "OpenCode-only capabilities" table | Omits 4 of A §6's 21 items: #17 native ACP server (F-CLIENT-05), #18 Mercurial, worktree strategies and startup scripts (F-WS-05/06), #19 hosted add-ons (F-HOST-01..04), #20 formatter auto-run (F-EXT-13). | Add one row: "`n/a` or `gated`, reason recorded". |
| 13 | low | `validation-plan.md` §3 | **OCF-15** (TUI or another client writes the same session concurrently) and **OCF-165** (embedded web UI exposed on LUNA-spawned servers) appear in no OCE failure-refs column and in no package text, only under group headers. CERT13-12 (two-writer protection, LA in D) depends on OCF-15. OCE-19's "another client renames/deletes" is the nearest step. | Reference OCF-15 from OCE-19 (add "another client prompts the same session while LUNA's turn runs") and OCF-165 from OC-02 step 5 and OCE-12. |
| 14 | low | `coverage-review.md` §6 rows G8 and CERT13 | G8 "History, **reset**, context lifecycle → OC-12, OC-10 applies", but v2.0.22 has no reset/clear operation (none among the 136 ops) and no reset or prompt-suggestion event (none among the 94). So CCI-42, CCM-17 and CCM-18 have no real home. The CERT13 row says "n/a except where noted at CC-13/16", but only CERT13-11 is noted. CERT13-03 (sticky terminal), -07 (parked-gate auto-decline) and -12 (two-writer) are LA and have homes (OCE-02 terminal unchanged, OCE-05 (c), OCF-15). | Mark reset and prompt suggestion `unavailable` (no native op or event). List CERT13-03/07/12 with their OCE/OCF homes. |
| 15 | low | `capability-inventory.md` F-INP-13, F-AGT-01 | F-INP-13 has Product `missing` and WP `OC-13 (P3)` but no E2E, while the route says "Record only". F-AGT-01 has Product `supported` but WP "—". | F-INP-13: Product `n/a` ("record only"), WP "—"; or keep OC-13 and add an E2E. F-AGT-01: WP OC-14 (agent selection) or "—" with reason. |
| 16 | low | `luna-coverage-audit.md` §4 vs C §2 | C §2 has 35 provider-gate rows; the audit has 23 (two are merges), so about 10 are absent. Two have no F-row anywhere: conversation deep link (Codex-only, `crates/ui/src/links.rs:75`) and historical usage scan (unsupported, `crates/engine/src/usage.rs:421-424,1632-1640`). Others are minor: single-flight pending steers `sessions.rs:5172-5174`, 60 s harness-settings probe timeout, skill-library install, accounts card, model-picker cold-boot retry, provider updates. | Add the two rows (or one "Other OpenCode gates" row citing C §2), with WP or `n/a`. |
| 17 | low | `crates/engine/src/permissions.rs:51-60` (not in deliverables) | The LUNA capability descriptor for OpenCode advertises `supports_workspace_sandbox: true` and `network_control: "session-scoped OpenCode permission rules"`, but on v2 LUNA's session rules are never applied (G-3, F-PERM-01). No consumer of the flag outside a UI fixture was found, so this is latent. | Note under F-PERM-01/F-PERM-11 and OC-04: make the descriptor truthful per generation once rules are compiled to `permissions[{action,resource,effect}]`. |
| 18 | low | `README.md` "Preservation status" | Claims "`SHA256SUMS` covers the deliverables and notes"; no `SHA256SUMS` file exists in the folder (checked 00:40). | Generate it last, after E and any coordinator edits. |
| 19 | low | RS-1 (D §1.B.2) | The only true omission in the 524 rows: shared approval choices use a radio role while acting immediately. OpenCode reuses the shared approval card, so it inherits the defect. | Add an approval-card AX/role assertion to OCE-16, or record it as a shared-surface exclusion with an owner. |
| 20 | low | `luna-coverage-audit.md` G-16; `capability-inventory.md` F-EXT-12 (`hl:228-230`) | Line drift: `luna_runtime_delivery` default is at `crates/harness/src/lib.rs:230-234` (228 is `pub trait Harness`). | Update to `hl:230-234`. |
| 21 | low | CXE-04 (`cx/README.md:240`) | The stdin half of "live command and stdin progress" is unaddressed. Nothing states whether the v2 `shell` tool supports interactive stdin. | State `unavailable` (with source) or add it to F-TOOL-07. |
| 22 | info | C §2 vs deliverables | C §2 classifies Auto as `excluded-by-product-decision`. The deliverables and HANDOVER say "unavailable (no native automatic reviewer)", which matches the code's limitation text (`permissions.rs:61-62`). Deliverables are right; no fix. | — |

## §3 Row-level mapping appendix (D §1 prior-parity rows)

Status legend:
- **M**: mapped to F-row(s), a package and a scenario.
- **M\***: mapped, but no scenario step actually exercises it; see the finding cited.
- **X**: excluded, with the reason stated in the deliverables (coverage-review §5–§7, matrix `n/a`/`unavailable`/`gated`, or PD).
- **X(E)**: excluded with a reason this reviewer supplies; the deliverables are silent, so the coordinator should add it.
- **O**: omitted (no reasonable home).

IDs for unnamed rows follow D's convention: PAR-§.n in table order, PAR-D1..D4 for "Deferred". The six Claude contracts are named by their noun.

### 3.A Codex parity (208)

| IDs | n | St | Home / reason |
|---|---:|---|---|
| CXM-01 | 1 | M | F-SES-01/02/04/06 → OC-03, OC-12 → OCE-10, OCE-12 |
| CXM-02 | 1 | M | F-MOD-01..03 → OC-10 → OCE-13 (service tier = provider option, coverage-review §7) |
| CXM-03, CXM-04 | 2 | M | F-INP-02/03, F-INP-15 → OC-07 → OCE-17 |
| CXM-05, CXM-06, CXM-07 | 3 | M | F-TOOL-01..07, F-OUT-02/03/04 → OC-06 → OCE-06 |
| CXM-08 | 1 | M\* | F-TOOL-08/09 → OC-06; no web-result step (#5) |
| CXM-09 | 1 | M | F-OUT-05 → OC-06 → OCE-06 |
| CXM-10 | 1 | M | F-INP-05/06 → OC-09 → OCE-08 |
| CXM-11 | 1 | M | F-TOOL-17 → OC-06 → OCE-22 |
| CXM-12 | 1 | M | F-PERM-08 → OC-05 → OCE-16 |
| CXM-13 | 1 | M | F-EXT-12 → OC-13 → OCE-18 |
| CXM-14 | 1 | X | Codex plugin/app identities; nearest F-INP-07 (coverage-review §7) |
| CXM-15 | 1 | M | F-INP-07, F-EXT-08 → OC-09, OC-14 → OCE-20 |
| CXM-16 | 1 | X | PD-1; F-AGT-02 inventoried; OCF-94 in OCE-10 |
| CXM-17 | 1 | M | F-ADV-01 → OC-15 → OCE-23 |
| CXM-18 | 1 | M | F-SES-16 → OC-12 → OCE-10 |
| CXM-19 | 1 | X | F-ADV-04 unavailable; OCE-23 asserts truthful "unavailable" |
| CXM-20 | 1 | M | F-PERM-02/03/15 → OC-04 → OCE-05, OCE-16 |
| CXM-21 | 1 | X | F-OUT-09 n/a (LUNA-owned renderer) |
| CXS-1 | 1 | M | F-INP-05/06/07 → OC-09, OC-14 → OCE-08, OCE-20 (plugin/app part X per CXM-14) |
| CXS-2 | 1 | M | F-OUT-02/03, F-TOOL-17, F-EXT-12 → OC-06, OC-13 → OCE-06/18/22 |
| CXS-3 | 1 | M | F-INP-02/03 → OC-07 → OCE-17 |
| CXS-4 | 1 | M | F-ADV-01, F-EXT-02, F-SES-16 → OC-15/14/12 → OCE-23/20/10 (Mermaid n/a, goals unavailable) |
| CXS-5 | 1 | M | F-PERM-08 → OC-05 → OCE-16 |
| CXP-1, CXP-2, CXP-3, CXP-5, CXP-6 | 5 | M | F-TOOL-01..07, F-OUT-04/05, F-SES-12 → OC-06, OC-12 → OCE-06, OCE-10 (OCF-67) |
| CXP-4 | 1 | M\* | F-OUT-02 + F-INP-03 → OC-06/07; no step asserts tool completion across a steer (#3) |
| CXL-1, CXL-2, CXL-3 | 3 | M | F-OUT-02/03, F-TOOL-15/17 → OC-06 → OCE-06, OCE-22 |
| CXL-4 | 1 | M | F-EXT-12 → OC-13 → OCE-18 |
| CXL-5 | 1 | M | F-OUT-10 → OC-06 → OCE-02 |
| CXL-6 | 1 | M\* | F-TOOL-08/09 → OC-06; source cards untested (#5) |
| CXT-1 | 1 | X | No expected-turn fence; correlation by inbox id (coverage-review §7) |
| CXT-2..CXT-11, CXT-14 | 11 | M | F-INP-02/03/04/06/15, F-TURN-01/02/09 → OC-07 (CXT-9 also OC-09, CXT-10 also OC-08) → OCE-17, OCE-03, OCE-09, OCE-08 (path 4) |
| CXT-12 | 1 | M\* | F-TURN-02/03 → OC-07 → OCE-03; no "Stop with pending steer" step (#3) |
| CXT-13 | 1 | X | Codex `instant_interrupt` flag (coverage-review §7) |
| CXI-1 | 1 | X | Codex catalog concepts (coverage-review §7) |
| CXI-2..CXI-6, CXI-8, CXI-9, CXI-11 | 8 | M | F-INP-07/11/15, F-EXT-08, F-AGT-05, F-WS-04/11, F-LIFE-06 → OC-09/14/16/02 → OCE-20, OCE-08, OCE-11, OCE-12 |
| CXI-7 | 1 | M\* | F-INP-07 → OC-09; "unavailable selection preserved with explanation" isn't an OCE-20 step |
| CXI-10 | 1 | X(E) | Codex app/plugin icons; the only OpenCode analogue is optional agent `color` (`oc2:schema/src/agent.ts:32`), which is cosmetic |
| CXW-1 | 1 | X | PD-1 |
| CXW-2, CXW-3, CXW-4, CXW-5, CXW-8 | 5 | M | F-ADV-01, F-PERM-07/08, F-PERM-01..03/09, F-SES-16, F-ADV-03 + F-MOD-02 → OC-15/05/04/12/10 → OCE-23/16/05/10/21/13 |
| CXW-6, CXW-9 | 2 | X | goals and memory unavailable (F-ADV-04, F-EXT-11) |
| CXW-7, CXW-10 | 2 | X | n/a: Mermaid (LUNA renderer); Codex client affordances (CLIENT) |
| CXE-01, -02, -03, -06, -07, -08, -10, -11, -12, -13, -14, -17 | 12 | M | per coverage-review §7: OCE-06 (01–03), OCE-08 (06), OCE-22 + OCE-02 (07), OCE-18 (08), OCE-17 + OCE-03/07/09 (10–12), OCE-03 (13), OCE-20 (14), OCE-10/23 (17) |
| CXE-04 | 1 | M\* | live progress → OCE-06 (`tool.progress`); stdin half unaddressed (#21) |
| CXE-05 | 1 | M\* | web labels/sources: no step (#5) |
| CXE-09 | 1 | M\* | held sequential/parallel tools + two steers missing from OCE-17 (#3) |
| CXE-15 | 1 | M\* | selections' draft/queue/restart/remote scope: OCE-08/11 test images, not composer selections |
| CXE-16 | 1 | M\* | catalog refresh in OCE-20; same-name collisions not a step |
| CXE-18 | 1 | X | goals unavailable (F-ADV-04, OCE-23) |
| CXA-01..CXA-09, CXA-11, CXA-12 | 11 | M | F-SES-02/05/12/16, F-MOD-01..06/11/12, F-INP-03..08, F-TURN-02, F-ADV-01/03, F-PERM-02/03, F-OUT-01..05/10, F-EXT-01/03/05/08, F-TOOL-20 → OC-06/07/09/10/12/14/15/04/16 → matching OCE (realtime voice, apps and audio are X: no native) |
| CXA-10 | 1 | X | goals and memory unavailable (F-ADV-04, F-EXT-11) |
| CXA-P1..CXA-P7 | 7 | M | OC-15, OC-12, OC-06, OC-14/10, OC-04, OC-07 (goals unavailable), OC-10 (live settings; realtime X) |
| CXG-1, -2, -3, -6, -7, -8, -9, -10, -12 | 9 | M | F-INP-05/06, F-PERM-08, F-ADV-01, F-EXT-12, F-PERM-02/03, F-SES-16, F-EXT-01/05, F-OUT-10, (F-INP-04, F-MOD-02, F-ADV-03) → OC-09/05/15/13/04/12/14/06/07/10 → OCE-08/16/23/18/10/22/02/17/21 |
| CXG-4, CXG-5, CXG-11 | 3 | X | goals unavailable; PD-1; Mermaid n/a |
| CXTS-1, -2, -3, -5, -6, -7, -8, CXTS-E1, -E2, -E3 | 10 | M | F-TOOL-01..06, F-OUT-02/05, F-TOOL-17, F-EXT-12, F-AGT-06/F-TOOL-12, F-INP-02/03 → OC-06/13/11/07 → OCE-06/22/18/01/17 |
| CXTS-4 | 1 | M\* | web results (#5) |
| CXNS-3, CXNS-5 | 2 | M | F-INP-03, F-TURN-12 → OC-07 → OCE-17, OCE-07 |
| CXNS-1, CXNS-2, CXNS-4, CXNS-6 | 4 | M\* | F-INP-03/F-TURN-03 → OC-07; fixtures missing from OCE-17 (#3) |
| CXSS-F1..F8, CXSS-F10, CXSS-R3..R6 | 13 | M | OCF-12/24/26/30/31/35 → OC-07/08 → OCE-17, OCE-02, OCE-04, OCE-03, OCE-09, OCE-10 |
| CXSS-F9, CXSS-R1, CXSS-R2 | 3 | M\* | evidence races; held 20 s command + steer; two steers/two tools (#3) |
| CXGV-1..CXGV-7 | 7 | X | native goals unavailable (F-ADV-04); the LUNA goal gate stays (luna-coverage-audit §4 "correct") |
| CXGV-8 | 1 | M | account isolation → F-MOD-05 → OC-16 → OCE-13 accounts section |
| CXBB-01..13, CXT3-01..17, CXC-1..15 | 45 | X | reference implementations, not LUNA requirements (D §1.E caveat; coverage-review §7). Adoptable patterns (CXC-5/6) land in OC-06/F-OUT-10. |

Codex subtotal: M 120, M\* 18, X 69, X(E) 1 = 208.

### 3.B Claude Code parity (316)

| IDs | n | St | Home / reason |
|---|---:|---|---|
| CC-00..CC-13, CC-15..CC-18 | 18 | M | coverage-review §6 row by row (OC-00..OC-18). CC-09 peer messaging, CC-11 apps/widgets, CC-12 output styles and CC-13 teams/channels are `unavailable`/`n/a` sub-parts |
| CC-14 | 1 | X | goals and scheduling unavailable (F-ADV-04) |
| Contracts: capability snapshot, native event envelope, content reference, input delivery receipt, pending interaction, native task binding | 6 | M | F-LIFE-06 → OC-02 → OCE-12; F-LIFE-04 → OC-08 → OCE-02/04; F-OUT-02/05 → OC-06 → OCE-06; F-INP-02 → OC-07 → OCE-17; F-PERM-07/13 → OC-04/05 → OCE-05/16; F-AGT-06/F-TOOL-12 → OC-11 → OCE-01 |
| G1..G12 | 12 | M | coverage-review §6 (G9 partial: F-ADV-01/04 → OC-15 → OCE-23) |
| RS-1 | 1 | **O** | shared approval-card radio-role defect; no OCE or AX assertion (#19) |
| RS-2 | 1 | M\* | F-SES-04, F-INP-02 → OC-07/08 → OCE-09/17; no step for "row written before resume reconciliation" |
| RS-3 | 1 | M\* | F-AGT-12/13 → OC-13 → OCE-15 (b); failed child-creation chip not asserted |
| RS-4 | 1 | X(E) | meta: prior Claude-era suite failures, not a capability |
| COR-1 | 1 | M | F-INP-06 (warm routing `ses:972-977`) → OC-09 → OCE-08 |
| COR-3 | 1 | M | PD-6/PD-7, F-AGT-06 vs F-AGT-12/13, Q-21 → OCE-15 |
| COR-4 | 1 | M | OC-18; validation-plan §1 rule 7 |
| COR-2 | 1 | X(E) | Claude-specific correction; OpenCode analogue is F-MOD-14 |
| COR-5 | 1 | X | Claude CLI flag spelling (D: NA) |
| CCI-01..09, 11..22, 24, 25, 27, 29..32, 36, 40, 41, 45, 51, 52, 53 (except 07) | 34 | M | F-WS-12, F-EXT-09/12, F-INP-12, F-TURN-01, F-OUT-01/02/05/07, F-INP-04..08/11, F-MOD-02/03/04/05/07/11/12/14, F-SES-06/08/09/16, F-TOOL-*, F-PERM-01/02/03/05/06/07/15, F-TOOL-10, F-EXT-01/03/05/08, F-TOOL-15, F-AGT-05/06/12/13, F-ADV-01/03, F-SES-07, F-WS-05/11, F-HOST-05, F-MOD-13 → OC-00/04/05/06/09/10/11/12/13/14/15/16/17 → matching OCE |
| CCI-07 | 1 | M\* | F-OUT-03 (streamed tool input) → OC-06; OCE-06 asserts `tool.progress`, not `tool.input.delta` |
| CCI-X1, CCI-X2, CCI-X3 | 3 | M | F-AGT-02/F-TOOL-20 + OCF-94/105 → OC-12/14 → OCE-10; OC-03 (adapter-local request names); luna-coverage-audit §4 + validation-plan §5 |
| CCI-10, 23, 26, 28, 33, 34, 35, 37, 38, 39, 43, 44, 46, 47, 48, 49, 50, 54 | 18 | X | structured output unavailable (F-OUT-08); MCP apps unavailable; hooks native-only (F-EXT-05/F-PERM-10); output styles n/a; workflows/teams/peer unavailable (F-AGT-11); goals/schedules unavailable (F-ADV-04); channels n/a; hosted/cloud/browser/computer lanes per coverage-review §6 CC-16 (F-TOOL-16 gated, F-SES-17/F-HOST-04 unavailable, F-WS-07 unavailable); CI/editor/desktop CLIENT n/a |
| CCI-42 | 1 | X(E) | end/reset conversation: no native op among 136 or event among 94 (#14) |
| CCT-01, 02, 03, 04, 08, 11, 12, 15, 16, 17, 18, 20, 21, 26, CCT-ENV | 15 | M | F-TOOL-07/01/04/05/20/02/03/12, F-ADV-03, F-TOOL-10/20 + F-AGT-02, F-SES-07/F-TOOL-14/F-WS-05, F-TOOL-11/15/17/18, F-OUT-02/10 → OC-06/11/15/05/12/14 → OCE-06/01/21/16/10/19/20/22/02 |
| CCT-06 | 1 | M\* | web tools (#5) |
| CCT-05, 07, 09, 10, 13, 14, 19, 22, 23, 24, 25 | 11 | X | no notebook/LSP natively (coverage-review §6 E09; F-EXT-13/F-TOOL-20); F-AGT-11; F-ADV-04; Claude-only tools n/a; findings unavailable (F-ADV-01 text review) |
| CCM-01..08, 10..14, 16, 19 | 15 | M | F-SES-12, F-OUT-01/03/07, F-TURN-01/06/12, F-LIFE-06/07, F-MOD-06/11, F-AGT-06, F-INP-10/11, F-PERM-08/13, F-OUT-10 → OC-06/07/08/10/11/14/15/04/05 → OCE-02/03/07/10/12/13/14/01/06/20/21/16/04/09 |
| CCM-09, CCM-15 | 2 | X | hook events native-only (F-EXT-05); `filesystem.changed` ignored (matrix §5) |
| CCM-17, CCM-18 | 2 | X(E) | prompt suggestion and conversation reset: no v2 event (#14) |
| CCC-1..5, CCC-7 | 6 | M | F-INP-01/F-TURN-02/F-LIFE-07, F-MOD-02/03/F-AGT-05, F-MOD-01/F-INP-11, F-EXT-01, F-SES-02/03/06/08/12/16, F-PERM-07/13/15 → OC-07/10/14/12/04/05 → OCE-03/13/20/22/10/16 |
| CCC-6 | 1 | X | persistent settings writes: config is read-only for LUNA (OC-14; PD-3/PD-12) |
| CCH-1..CCH-5 | 5 | X | plugin hooks are native-only; LUNA ships no OpenCode plugin (F-EXT-05, F-PERM-10 n/a) |
| CCL-01..CCL-13 | 13 | M | F-LIFE-01/02, F-SES-04/12, F-MOD-01, F-INP-06/11, F-OUT-01/02/07/10, F-TOOL-*, F-AGT-06, F-PERM-02/03, F-TOOL-10, F-EXT-01/05/12, F-MOD-14 → OC-02/06/09/10/11/04/05/13/12/14/16/08 → OCE-12/09/13/20/02/06/08/01/16/18/10/22 |
| CC-G01..CC-G08 | 8 | M | same homes as G2/G6/G4/G5/G7/G3/G8/G11 (D §1.B.5) |
| CCR-T1..T5, CCR-B1..B4 | 9 | X | reference implementations (D §1.E caveat) |
| CCR-N1..N5 | 5 | M | "do not infer" rules: README objective + F-CLIENT-07; PD-6; F-SES-06/16 + OCF-80; validation-plan §1 rule 7; PD-1 + F-PERM-11 |
| CCF-01..CCF-15 | 15 | M | OCF superset (coverage-review §6): 01→OCF-01..09; 02→20–22/60/63; 03→24/25/66/162; 04→80–87; 05→41/50/54/57/58; 06→53/153; 07→30/31/35; 08→121/123; 09→20/23/124/167; 10→120/122 (peer n/a); 11→90/92/93; 12→04/102/111; 13→40/125 (goals unavailable); 14→PD-13, F-HOST-*; 15→140–143 |
| E01..E17, E19, E20, E22..E28 | 26 | M | coverage-review §6 table (E19 → OCE-23 asserts unavailable; E09 → OCE-06/22 without notebook/LSP) |
| E18, E21 | 2 | X | workflows/teams n/a; hosted lanes n/a or gated |
| CERT13-03, CERT13-07, CERT13-11 | 3 | M | sticky terminal → F-TURN-01/09, OCE-02 "terminal unchanged", OCF-41; parked-gate auto-decline → F-PERM-12/13, OCF-51, OCE-05 (c); external sessions → F-SES-02 → OC-12 → OCE-10 |
| CERT13-12 | 1 | M\* | two-writer protection → OCF-15, Q-6, OCE-19 (other-client metadata changes only) (#13) |
| CERT13-01, 02, 04, 05, 06, 08, 09, 10, 13 | 9 | X | Claude workflow/team/channel/peer/remote-agent lanes (coverage-review §6; F-AGT-11, F-WS-07) |
| CERT16-01..CERT16-10 | 10 | X | Claude hosted services; the ledger format is reused by OC-18 (coverage-review §6) |
| PAR-1.7, 1.8, 1.9, 1.11, 1.13 | 5 | M | shared composer F-INP-15; transcript F-OUT-01; accounts F-MOD-05/14 → OC-16 → OCE-13; changes F-OUT-04 → OC-06 → OCE-06; connection F-LIFE-05 → OC-08 → OCE-04 |
| PAR-1.1..1.6, 1.10, 1.12 | 8 | X | provider-agnostic shell UI (PD-4); terminal F-WS-08 n/a |
| PAR-2.1, 2.2, 2.4, 2.6, 2.8, 2.11 | 6 | M | ListModels F-MOD-01; Run/Steer/Interrupt/RespondInput F-TURN-02/F-INP-03/F-PERM-03; uploads F-INP-05/06; agent accounts F-MOD-05; QueueCommand F-INP-15; wire types (matrix §4/§8) → OC-10/07/04/09/16/06 |
| PAR-2.3, 2.5, 2.7, 2.9, 2.10 | 5 | X | repos/worktrees (PD-5), terminals, LocalDevice, chat mutate ops, AuthRpc: provider-agnostic |
| PAR-3.2, 3.3, 3.7 | 3 | M | run journal/steering mailbox/stall F-OUT-02/F-INP-03/F-TURN-10; doc previews F-OUT-02; uploads/accounts F-INP-05/F-MOD-05 → OC-06/07/09/16 |
| PAR-3.1, 3.4, 3.5 | 3 | X | device lifecycle, terminals, repos/diffs: provider-agnostic |
| PAR-4.1..4.8 | 8 | X | other harnesses; the missing OpenCode row is Q-4 → OC-18 |
| PAR-5.3, 5.4, 5.5 | 3 | M | continuation splitting/large output F-OUT-02; render-parts privacy (`sanitize_tool_call`) F-TOOL-07/17; sidecars F-OUT-02/04 → OC-06 → OCE-06 |
| PAR-5.1, 5.2 | 2 | X | doc containers and command rules: provider-agnostic |
| PAR-6.1..6.4, 7.1, 7.2, 8.1, PAR-D1..D4 | 11 | X | edge/server/auth infrastructure; 8.1 = PD-15 stale (coverage-review §7); deferred mobile/E2EE/packaging/engine hardening |

Claude subtotal: M 199, M\* 5, X 106, X(E) 5, O 1 = 316.

### 3.C Counts

| Status | Codex | Claude | Total |
|---|---:|---:|---:|
| M (mapped) | 120 | 199 | 319 |
| M\* (mapped, scenario gap) | 18 | 5 | 23 |
| X (excluded, reason in deliverables) | 69 | 106 | 175 |
| X(E) (excluded, reason supplied here) | 1 | 5 | 6 |
| O (omitted) | 0 | 1 | 1 |
| **Total** | **208** | **316** | **524** |

**Mapped 342 · excluded with reason 181 · omitted 1.** The 6 X(E) and 23 M\* rows are what the coordinator should fix: findings #3, #5, #13, #14, #19, #21, and the minor M\* notes above.

## §4 Coverage-table checks

All checks script-assisted (Python over the Markdown tables), then eyeballed.

| Check | Result | Detail |
|---|---|---|
| `coverage-review.md` §3 lists all 63 docs pages (vs A §1) | **pass** | A §1 tree: Docs 25 (4 + 21 Configure), CLI 10, Build 12, API 1, Console 15 (5 + Workspace 6 + API 4) = 63. §3 rows cover the same 25 + 10 + 12 (5 + 1 + 2 + 4 grouped) + 1 + 15 (5 + 6 + 4 grouped) = 63. Every mapped F-row exists. |
| §4 covers all 29 API families | **pass (families)** | All 29 families from A §1 present (some grouped: integration+credential, pty+persistentPty+shell, worktree+vcs). |
| §4 op counts correct | **fail (minor)** | §4 heading says "v2.0.22 OpenAPI, 136 operations", but the per-family counts are the docs-sidebar counts (A §1), which sum to **141**. Committed `packages/protocol/openapi.json` @527f0b93 counts (script): server **1** (§4 says 3), credential **3** (§4 says 5), vcs **5** (§4 says 6); all other 26 families match; total 136. The v2.0.22 source groups have server 3 (`server.info`, `server.pair`, `server.connect`: `protocol/src/groups/server.ts:36,47,58`) and credential 5 (`credential.ts:8-67`, adds `list`/`create`), so the committed `openapi.json` omits 4 source endpoints, and the docs sidebar (built from 2.0.6-era docs, A §1 note) shows vcs 6. See finding #9. |
| `tool-and-event-matrix.md` §5 has all 94 events (vs B §3.2) | **pass** | 94 numbered rows; set-equal to B §3.2's 94 rows (93 named + `rpc.*`); no duplicates; D/E flag, "LUNA today" text and `mod.rs` line identical for all 94; consumed count 19 matches the §5 intro ("19 consumed, 75 dropped"). |
| `capability-inventory.md` covers all 183 A rows | **pass** | A §3 has 183 rows (SES 18, INP 14, TURN 10, OUT 9, TOOL 21, MOD 16, PERM 13, EXT 20, AGT 13, ADV 8, WS 14, HOST 12, CLIENT 15). `coverage-review.md` §2 maps all 183, every target F-row exists (176 defined), and every A row is also cited in some F-row's "(A-…)" refs. |

## §5 ID-consistency report

Script over all 8 deliverables (`README.md`, `HANDOVER.md`, the six documents, `research-baseline.json`), files as of 2026-10-07 00:40.

| Check | Result |
|---|---|
| Definitions | F-rows 176 (`capability-inventory.md`), OC-00..OC-18 = 19 (`implementation-plan.md`), OCE-00..OCE-23 = 24, OCF = 117 (numbered 01–168 with deliberate gaps per group), Q-1..Q-22 = 22 (`coverage-review.md` §9). |
| Dangling references (`F-…`, `OC-…`, `OCE-…`, `OCF-…`, `Q-…` used but undefined) | **none**. The only "undefined" hits are range shorthands spanning unassigned group gaps (`OCF-01..168` in README/validation intro, `OCF-100..113` in OC-10). Harmless; optionally write `OCF-100–105, 110–113`. |
| Every OCF referenced somewhere | yes (by group header or package/scenario). **But** OCF-15 and OCF-165 appear in no OCE failure-refs column and no package text (finding #13). OCF-10/11/68/140–143 are in no OCE column but are named in OC-02/OC-01 acceptance or are v1-only (OCF-68). |
| Every OCE referenced by at least one package | **no**: OCE-00 and OCE-02 are named by no package (finding #4). The other 22 are named. |
| Every OC package has an acceptance scenario | yes for OC-00, OC-02..OC-18 (OC-18 = all OCE). OC-01's acceptance describes OCE-00 ("dry T2 run … expected to fail at G-0") without naming it. |
| OCE "WPs" column vs package acceptance text | 8 mismatches: OCE-00 (OC-00/01 don't name it), OCE-02 (OC-06/08), OCE-03 (OC-07), OCE-05 (OC-03), OCE-14 (OC-13), OCE-19 (OC-12), OCE-20 (OC-09), OCE-21 (OC-11). Finding #4. |
| Every matrix row has WP + E2E, or an `n/a`/`unavailable`/`excluded` reason | 131 rows have both. 45 lack one or both (38 twelve-column rows plus the 7 F-CLIENT rows); all but two carry `n/a`, `unavailable`, `gated` or "record only" with a stated reason. Exceptions: **F-INP-13** (Product `missing`, WP `OC-13 (P3)`, no E2E, route "Record only") and **F-AGT-01** (Product `supported`, WP "—", E2E OCE-13). Finding #15. The F-CLIENT table has no "LUNA files"/"E2E" columns; acceptable because every row is `n/a`. |
| Sequence "Rows closed" vs row WP column | Consistent except the `F-TOOL-*` wildcard in OC-06, which also sweeps in F-TOOL-10 (WP OC-05), F-TOOL-12 (OC-11), F-TOOL-16 (gated, "—") and F-TOOL-19 (internal, "—"). Cosmetic: write `F-TOOL-01..09, 11, 13..15, 17, 18, 20`. |
| A-row → F-row map (coverage-review §2) | 183/183, all targets defined (see §4). |
| Q-ledger completeness | 22 open plus 10 closed. Missing items are listed in finding #10, plus the context-inheritance decision in finding #7. |
| `research-baseline.json` lists | `work_packages` 19 and `planned_e2e_scenarios` 24, both matching the definitions. |

## §6 Brief-compliance checklist

✓ = explicit and adequate; ◐ = present but weak (finding); ✗ = missing.

| Brief requirement | Status | Where / gap |
|---|---|---|
| Session create, resume, import/export, sharing, fork, rewind/revert, file checkpoints, persistence | ✓ | F-SES-01..18; sharing `unavailable` (F-SES-17); checkpoints Q-13; OCE-09/10. Fork wording conflict (#1). |
| Initial, follow-up, queued, steered inputs; text, images, files, references, skills, commands; attachment validation; draft/retry/restart fidelity | ✓ | F-INP-01..15; OC-09 admission policy; OCE-08 six paths + invalid/oversized/SVG; OCE-20; F-INP-15. |
| Native steering vs queued delivery; acceptance vs incorporation; interruption, cancellation, completion, retries, stalls, recovery | ◐ | F-INP-02/03, F-TURN-01..12, OC-07/08, OCE-03/07/09/17. Canonical steering fixtures missing from OCE-17 (#3). |
| Text, reasoning, structured output; tool args/results/errors; stdout/stderr; rich content; images/artifacts; diffs; full-output retrieval | ✓ | F-OUT-01..10 (structured output `unavailable`), OCE-06. Streamed tool input not asserted (CCI-07, M\*). |
| File, search, edit, patch, run, web, browser and other native tools, incl. failure and approval paths | ◐ | F-TOOL-01..20, matrix §4, OCE-06/16/22. Web result rendering untested (#5). Browser `gated` (desktop-only). |
| Models, providers, accounts; variants/reasoning; settings; discovery; usage, cost, context; compaction; effective config | ✓ | F-MOD-01..14, F-OUT-07, OC-10/14/16, OCE-13/14. |
| Permissions, sandbox, security; approval choices; questions; MCP elicitation; denied/cancelled/stale; child inheritance | ✓ | F-PERM-01..15, OC-04/05, OCE-05/16. Descriptor truthfulness note (#17); RS-1 AX (#19). |
| MCP servers/tools/resources/prompts; plugins; skills; instructions/memory; hooks; discovery; callable LUNA tools via runtime contract and Executor conventions | ✓ | F-EXT-01..15, F-TOOL-13/15/17/18, OC-13/14, OCE-18/20/22; Executor convention stated at F-EXT-01. |
| Native subagents/tasks/teams; parent-child identities; nested transcripts; progress/results; concurrent work; independent LUNA chats; context inheritance; routing; ownership; stop propagation | ◐ | F-AGT-01..14, OC-11/13, OCE-01/15/21. Context inheritance of the LUNA contract and tools by native children not addressed (#7). |
| Review and findings; goals/continuations; schedules, monitors, background; unsupported counterparts recorded | ✓ | F-ADV-01..05, F-OUT-06, OC-15, OCE-21/23 (goals/schedules `unavailable`). |
| Workspace, project, worktree; remote and multi-device; credentials and data ownership; hosted/cloud/device/browser; updates and compatibility | ✓ | F-WS-01..12, F-HOST-01..09, OC-16/17, OCE-11/12; Q-15/16. |
| Four dimensions per feature with precise statuses and reasons | ✓ | All 176 rows; statuses per baseline vocabulary; reasons in the route column. |
| Native-client-only and non-applicable documented | ✓ | F-CLIENT-01..07; A §7 reflected; `n/a` rows have reasons. |
| Matrix columns (ID, native v2 support + version, evidence, 4 dims, LUNA route/gap, files, WP, E2E) | ✓ | Present in 13 category tables; F-CLIENT omits files and E2E (all `n/a`). |
| Every material finding pinned (source/local `file:line`, docs URL, version, evidence level) | ✓ | `oc2:`/`oc:` refs and `D:` URL abbreviations per row; one line drift (#20); doc/source disagreements not consolidated (#6). |
| Baseline record (commits, branches, dirty state, docs dates, executables, versions, entry points; released vs installed vs dev separate) | ✓ | `research-baseline.json` covers every item; head drift recorded. `SHA256SUMS` claimed in README but absent (#18). OpenAPI count provenance (#9). |
| E2E: root/child, duplicates/out-of-order, cancellation, reconnects, stale approvals, partial output, provider failure, images on every path, restart recovery | ◐ | OCE-01..09 cover all nine categories (validation-plan §5). OCE-02 isn't owned by any package acceptance (#4). |
| Unresolved-questions ledger | ◐ | 22 open + 10 closed; five notes questions not carried (#10). |
| (Task 2) Version drift and OpenCode-only capabilities | ◐ | Drift mostly inline (`24:`, `head:`); two head items missing (#11); OpenCode-only table omits 4 of 21 (#12). |

## §7 Spot checks

`oc2:` = snapshot of OpenCode `527f0b93` (`packages/…`); `oc:` = LUNA `crates/harness/src/opencode/mod.rs` (working tree, file clean per baseline). All read-only.

| # | Claim (deliverable location) | Verdict | Line(s) checked |
|---|---|---|---|
| S1 | G-0: LUNA readiness needs 2xx JSON with `version` from `/api/health` or `/global/health` (capability-inventory "global blocker") | **confirmed** | `oc:474-487` `detect()` loops `("/api/health", V2), ("/global/health", V1)`, requires `resp.json()` with string `version`. |
| S2 | G-0: `/api/health` gone at 2.0.22; `/global/health` falls through to the SPA (HTML 200) | **confirmed** | `git grep 'api/health'` hits at `d44b52ca` (v2.0.3: `protocol/src/groups/health.ts:16`, `server/src/process.ts:188`, `openapi.json:9`), none at `13453f2da8c` ("refactor(protocol): consolidate server status") or `527f0b93`. `oc2:cli/src/services/web-ui.ts:17-24`: only `/api`, `/api/*`, `/auth/*`, `/openapi.json` route to the API (unknown → empty 404); everything else serves `index.html`. "Removed in v2.0.4" not independently confirmed (no local tags), but irrelevant to the 2.0.22 conclusion. |
| S3 | v2 readiness is `GET /api/info` 503 → 200 / 500 (+`retry-after`) | **confirmed** | `oc2:server/src/process.ts:185-214` (`status: ready ? 200 : failed ? 500 : 503`, `retry-after: 1` while starting/stopping). |
| S4 | Permission reply body is `{decision: once/always/reject, message?}`; LUNA sends `{reply}` (S0) | **confirmed** | `oc2:protocol/src/groups/permission.ts:117-122`; `oc2:schema/src/permission.ts:41`; OpenAPI `required:["decision"]`, `additionalProperties:false`. LUNA `oc:2985-2994` posts `json!({"reply": reply})` for both generations. |
| S5 | Command body needs `name`; LUNA v2 sends `{command, text}` (S1) | **confirmed** | `oc2:protocol/src/groups/session.ts:417-424` (`name`, `...PromptInput.Prompt.fields`, `delivery?`); OpenAPI `required:["name","text"]`. LUNA `oc:2427-2433` sends `{"command", "arguments"}` (v1) / `{"command", "text"}` (v2). |
| S6 | `form.created` has data `{form: Form.Info}` and no top-level `sessionID`, so LUNA drops it | **confirmed** | `oc2:schema/src/form.ts:170` (`Created … schema: { form: Info }`), `:122-133` (`Info` carries `sessionID`), `:171-172` (`replied`/`cancelled` do carry top-level `sessionID`). LUNA `oc:3635-3641` returns `Vec::new()` when `data.sessionID` is missing. |
| S7 | v2.0.22 tool names are `shell` and `subagent`; LUNA maps only `bash`/`task` | **confirmed** | `oc2:core/src/tool/plugin/shell.ts:22` `name = "shell"`; `subagent.ts:17` `name = "subagent"`. LUNA `oc_tool_call` (`oc:3534-3624`) matches `"bash"` (+10) and `"task"` (+60), no `shell`/`subagent` arm, so both fall to `Unknown`. |
| S8 | Adapter cuts tool output to 4 KiB before the journal | **confirmed** | `oc:3519-3520` `OUTPUT_CAP = 4096` (comment "journal keeps the rest" is wrong for this path); `oc:3346-3361` applies `cap_text` before `AgentEvent::ToolResult{diff: None}`. |
| S9 | `RuntimeDelivery` defaults to `Unsupported` and OpenCode doesn't override | **confirmed** | `crates/harness/src/lib.rs:230-234` trait default `Unsupported`; overrides only in `claude/mod.rs:399`, `codex/mod.rs:675`; no `luna_runtime_delivery` in `opencode/mod.rs`. |
| S10 | Fork takes `{before?}`; LUNA sends `{messageID}` | **confirmed**, with a wording contradiction | `oc2:protocol/src/groups/session.ts:312-315`; `oc2:core/src/session.ts:307-343` and `session/projector.ts:136-138`: `before` is **exclusive** (`lt seq`), omitted = through newest. LUNA `oc:805-808` sends `{"messageID"}`. Contradiction: `tool-and-event-matrix.md` §2 E15 says "use a real **assistant** message id as the checkpoint", while OC-12 step 1 and the §2 payload example say `<user msg id>`. With exclusive semantics an assistant id drops that reply (finding #1). Also F-SES-06 says "OpenAPI requires `before`": **refuted**, OpenAPI requires a body object but `before` is optional (`required` absent). |
| S11 | Restart claims are swept only by the managed service | **confirmed** | `oc2:server/src/process.ts:104-108` installs `installRestartContinuity` only `if (lifecycle)`; `:232-239` doc; `oc2:cli/src/server-process.ts:100-160` passes the lifecycle (`onListen` + `ServiceRegistration.register`) only when `serviceOptions` exist (service mode); `oc2:core/src/session/execution/restart.ts:33,55-59` ("unregistered servers sharing the database never sweep"). So `--stdio` servers don't sweep, as the plan assumes. |
| S12 | Login-shell probe tries `-l -i -c` then `-l -c`; resolver order PATH → login PATH → install paths | **confirmed** | `crates/harness/src/shell_env.rs:126-135` (`_ => vec![["-l","-i","-c"], ["-l","-c"]]`; csh/tcsh and fish differ, not mentioned in the deliverables, harmless); `crates/harness/src/executable.rs:313-315` (process `PATH` first, then `login_shell_path`). |
| S13 | `session.usage.updated` carries session-cumulative totals | **confirmed** | `oc2:core/src/session/projector.ts:58-73` `applyUsage` adds to `SessionTable` columns; `:75-104` publishes the row totals as `UsageUpdated`. |
| S14 | `--stdio` deletes the password from the environment | **confirmed** | `oc2:cli/src/server-process.ts:72-77` (reads `Env.password`, then `delete process.env.OPENCODE_PASSWORD` and `OPENCODE_SERVER_PASSWORD` when `mode === "stdio"`); `:163` prints `{"url"}`; `:167` waits for stdin close. Note `:164` default mode prints a random password to stdout when none is set in env (irrelevant once LUNA sets it). |
| S15 | `mcp.add` is an in-memory override | **confirmed** | `oc2:core/src/mcp/index.ts:635-639` (`overrides.set(name, config)` then `state.reload()`). |
| S16 | v2 session create reads `permissions[{action, resource, effect}]`; LUNA sends `permission` with v1 rules | **confirmed** | OpenAPI `Permission.Rule` `required:[action,resource,effect]`, `additionalProperties:false`; LUNA `oc:2147-2160` rules `{permission, pattern, action}`, `oc:2214,2216,2237` key `"permission"`. |
| S17 | "v2.0.22 OpenAPI, 136 operations" with the §4 family counts | **partly refuted** | Committed `oc2:protocol/openapi.json` has 136 ops but server 1 / credential 3 / vcs 5; §4 uses docs-sidebar counts (server 3, credential 5, vcs 6; sum 141). Source groups have server 3, credential 5 (`server.pair`/`server.connect`/`credential.list`/`credential.create` absent from the committed JSON). |

Verdict: 15 confirmed, 1 confirmed-with-contradiction (S10), 1 partly refuted (S17), plus the F-SES-06 sub-claim refuted. No P0/P1 driver was refuted.
