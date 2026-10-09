# L08 — sync, documents, protocol and RPC test audit

- **Lane:** L08-sync-doc-proto-rpc (campaign 2026-10-09, steps 3–5).
- **Scope:** every test-bearing file of `crates/sync`, `crates/doc`, `crates/proto` and `crates/rpc` (40 files: inline `#[cfg(test)]` modules, `src/**/tests.rs` module files, `tests/*.rs`, and the `rpc/examples/e2e_driver.rs` inline module), plus the production code those tests reach.
- **Baseline (`e4a0f534`):** 297 test declarations; 12,726 test LOC (whole `tests/` files and `*tests.rs` module files, plus every `#[cfg(test)] mod` block).
- **After:** 256 declarations (297 − 33 D − 9 C + 1 new consolidated table); 11,780 test LOC (−946). Production code −641 net lines (dead or test-only items listed under *Seams removed*).
- **Marks:** R 242 · F 13 · C 9 · D 33.
- **Not run here:** by campaign rule this lane only edits; `rustfmt --edition 2024 --check` and `git diff --check` pass on every changed file. The coordinator compiles and runs the lane.

Wire and document-schema contracts shared with `edge/` TypeScript and `apps/ios` Swift were checked against the other side before marking (registry merge vectors ⇄ `edge/src/registry-core.test.ts`, device frames ⇄ `edge/src/device-frame.test.ts`, chat frames ⇄ `edge/src/chat-frames.test.ts`, `claude-code`/`checkoutId`/`changeRequest` ⇄ iOS `WorkspaceStore.swift`/`ChangeRequestTracker.swift`). All of those stay R.

## Ledger

| Mark | Test | Evidence |
| --- | --- | --- |
| D | `crates/doc/src/commands.rs::tests::processed_commands_are_skipped` | Skip row of `evaluate_command`. Stronger keeper: crates/engine/tests/e2e.rs::processed_commands_are_skipped_on_redelivery (real DocHost drain does not execute a processed command, then asserts `evaluate_command` == Skip directly). |
| R | `crates/doc/src/commands.rs::tests::expired_commands_are_expired` | pure `evaluate_command` decision-table row (sole owner: no engine test drives Expired/Superseded through `DocHost`; the only engine use, e2e.rs::processed_commands_are_skipped_on_redelivery, covers Skip); TTL expiry. |
| R | `crates/doc/src/commands.rs::tests::newer_steer_supersedes_older_pending_steer` | pure `evaluate_command` decision-table row (sole owner: no engine test drives Expired/Superseded through `DocHost`; the only engine use, e2e.rs::processed_commands_are_skipped_on_redelivery, covers Skip); steer supersession. |
| R | `crates/doc/src/commands.rs::tests::interrupt_for_past_turn_is_superseded` | pure `evaluate_command` decision-table row (sole owner: no engine test drives Expired/Superseded through `DocHost`; the only engine use, e2e.rs::processed_commands_are_skipped_on_redelivery, covers Skip); interrupt aimed at a finished vs current turn. |
| R | `crates/doc/src/commands.rs::tests::runs_are_not_superseded_by_newer_runs` | pure `evaluate_command` decision-table row (sole owner: no engine test drives Expired/Superseded through `DocHost`; the only engine use, e2e.rs::processed_commands_are_skipped_on_redelivery, covers Skip); negative control that queued runs both execute. |
| D | `crates/doc/src/commands.rs::tests::composer_cancel_rules` | Only caller of `can_composer_cancel`, which has zero non-test callers (rg whole repo). Detects nothing a user can reach; no contract. Unlocks deleting `can_composer_cancel`. |
| R | `crates/doc/src/commands.rs::tests::typed_approval_response_round_trips` | Command-ledger payload tags `respondApproval`/`allowOnce` are stored in the Loro doc and read by other devices (doc-schema contract). |
| R | `crates/doc/src/parts.rs::tests::computer_use_unknown_refreshes_the_same_persisted_tool_part` | Owner of fold refresh-in-place: a re-sent ToolCall with the same id updates the one part (status/screenshot) and stays resolved; also proves `sanitize_tool_call` keeps ComputerUse. Keeper for tool_call_refresh_is_idempotent. |
| R | `crates/doc/src/parts.rs::tests::async_question_folds_once_without_becoming_pending_input` | Fold idempotence + `aq-` id for async questions (id is what RespondAsyncQuestion targets). |
| R | `crates/doc/src/parts.rs::tests::reasoning_deltas_fold_into_their_own_part` | Pure fold rule: reasoning merges, empty heartbeat mints nothing, text breaks the block. |
| D | `crates/doc/src/parts.rs::tests::oversized_reasoning_splits_like_text` | Tests only `split_parts`/`join_continuations`, which have zero production callers (rg crates+apps; no caller since 240b008b). Dead code; no contract. Unlocks deleting both fns and the `MSG_INLINE_MAX` import. |
| F | `crates/doc/src/parts.rs::tests::generated_image_fold_is_ordered_atomic_and_idempotent` | Fold ordering/idempotence kept; dropped the final `join_continuations(split_parts(..))` assertion that exercised dead code. |
| R | `crates/doc/src/parts.rs::tests::text_deltas_merge_until_broken_by_tool` | Pure fold rule: text merges until a tool part breaks it. |
| R | `crates/doc/src/parts.rs::tests::session_started_resets_accumulator` | Pure fold rule: SessionStarted clears the accumulator. |
| D | `crates/doc/src/parts.rs::tests::tool_call_refresh_is_idempotent` | Detects a duplicated part on an identical ToolCall re-send. Same generic ToolCall arm is proven by computer_use_unknown_refreshes_the_same_persisted_tool_part (single part after same-id refresh, fields updated). No seam. |
| R | `crates/doc/src/parts.rs::tests::tool_result_marks_resolution` | Fold rule: ToolResult sets resolved and propagates is_error (is_error not asserted elsewhere). |
| R | `crates/doc/src/parts.rs::tests::sanitize_strips_heavy_inputs_and_is_idempotent` | Render-only privacy policy (security): WriteFile content never enters the doc; fixpoint. |
| R | `crates/doc/src/parts.rs::tests::sanitize_keeps_a_spawns_model_and_drops_its_prompt` | Privacy whitelist for spawns: model/type kept, prompt dropped. |
| R | `crates/doc/src/parts.rs::tests::sanitize_still_strips_a_non_spawn_carrying_a_model_argument` | Privacy: genus gate, not the key, decides retention. |
| R | `crates/doc/src/parts.rs::tests::sanitize_drops_a_spawn_input_that_names_nothing_worth_keeping` | Privacy: empty whitelist result is None, keeps the fixpoint exact. |
| D | `crates/doc/src/parts.rs::tests::split_and_join_round_trip` | Only exercises dead `split_parts`/`join_continuations` (zero production callers). No contract. Unlocks deleting them. |
| D | `crates/doc/src/parts.rs::tests::continuation_ids_are_deterministic` | Only caller of `continuation_id` (zero production callers; edge/iOS do not consume the Rust fn). Format pin of dead code. Unlocks deleting it. |
| R | `crates/doc/src/parts.rs::tests::summarize_inlines_small_outputs_and_marks_big_cuts` | Pure function with a real input space (fences, blank, multibyte cut); table owner. |
| R | `crates/doc/src/parts.rs::tests::diff_stat_counts_line_changes` | Pure diff-stat counter; owner. |
| F | `crates/doc/src/parts.rs::tests::fold_strips_output_to_summary_and_diff_to_stats` | Called the test-only copy `apply_sidecar_refs`; now drives the live `apply_journal_output_refs` (byte-identical body) so the A1 strip + `{chat}/{part}` ref shape is pinned on the production path. |
| F | `crates/doc/src/parts.rs::tests::sidecar_refs_stamp_once_and_only_where_content_exists` | Same repoint to `apply_journal_output_refs`; keeps stamp-only-with-content + idempotence. |
| R | `crates/doc/src/parts.rs::tests::subagent_events_refresh_the_spawn_chip_in_place` | Subagent chip lifecycle on the parent part (incident-driven: per-delta rewrites polluted the parent doc). |
| R | `crates/doc/src/parts.rs::tests::child_observations_keep_waiting_usage_result_and_reactivation_distinct` | Chip status transitions Waiting/Idle/Done/Running + activity reset; no engine test asserts these at part level. |
| R | `crates/doc/src/parts.rs::tests::subagent_events_never_decorate_a_non_spawn_chip` | 2026-08-20 regression: background shells must not become spawn chips. |
| D | `crates/doc/src/parts.rs::tests::sidecar_payload_carries_full_texts` | Only caller of `sidecar_payload` fn (zero production callers; engine builds `SidecarPayload` directly). Unlocks deleting the fn. |
| D | `crates/doc/src/queue.rs::tests::insert_restores_a_taken_row_at_the_head` | Tests `insert_queued` + `take_queued`, both without production callers since 5cb05eaa (claims replaced take/undo). Unlocks deleting both. |
| D | `crates/doc/src/queue.rs::tests::insert_past_the_end_appends_rather_than_failing` | Only exercises dead `insert_queued`. Unlocks deleting it. |
| R | `crates/doc/src/queue.rs::tests::push_and_read_round_trip` | Hand-written Loro row codec (`write_queued_map`/`queued_from_json`) round trip incl. attachments. |
| R | `crates/doc/src/queue.rs::tests::hold_policy_round_trips_and_old_rows_default_to_queue` | Doc-schema compat: legacy rows default follow-up behavior to Queue. |
| R | `crates/doc/src/queue.rs::tests::steer_intent_and_hold_reason_round_trip_without_rewriting_content` | Snapshot import round trip of steer intent + hold reason (doc schema). |
| R | `crates/doc/src/queue.rs::tests::rejected_native_steer_releases_the_same_row_with_reason` | Claim release keeps identity/revision and records ProviderRejected. |
| F | `crates/doc/src/queue.rs::tests::delivery_gate_round_trips_and_finish_clears_it_atomically` | Called wrapper `finish_queued_edit` (no production caller); now calls the live `finish_queued_edit_with_inputs`. camelCase gate wire keys kept. |
| F | `crates/doc/src/queue.rs::tests::editing_attachments_preserves_row_identity_order_and_policy` | Called wrapper `finish_queued_edit_with_attachments` (no production caller); now drives `finish_queued_edit_with_inputs`. |
| F | `crates/doc/src/queue.rs::tests::cancelling_an_edit_only_clears_the_gate_and_empty_commit_discards` | Repointed from wrapper `finish_queued_edit` to `finish_queued_edit_with_inputs`. |
| R | `crates/doc/src/queue.rs::tests::empty_ids_and_text_are_rejected` | push validation (no empty id/text rows). |
| R | `crates/doc/src/queue.rs::tests::editing_to_empty_deletes_the_row` | set_queued_text delete gesture + no-op semantics. |
| R | `crates/doc/src/queue.rs::tests::move_reorders_and_clamps` | MovableList move + clamp + no-write cases. |
| D | `crates/doc/src/queue.rs::tests::take_head_pops_in_order_and_take_is_once_only` | Only exercises dead `take_queue_head`/`take_queued`; once-only dispatch is owned by claim_is_atomic_against_stale_edit_remove_and_replay. Unlocks deleting both. |
| F | `crates/doc/src/queue.rs::tests::delivery_gate_makes_row_untakeable` | Contract kept on the live path: renamed `delivery_gate_blocks_a_claim`; asserts `claim_queued` returns Blocked and the row stays Held (was asserted through dead `take_queued`). |
| F | `crates/doc/src/queue.rs::tests::claim_is_atomic_against_stale_edit_remove_and_replay` | Replaced the dead `take_queued` probe with a second claim under another command id → Blocked (the real second-dispatch path). |
| R | `crates/doc/src/queue.rs::tests::steering_acceptance_round_trips_then_becomes_unknown_or_settles` | Steer acceptance survives restart and settles/turns unknown (no replay). |
| R | `crates/doc/src/queue.rs::tests::queue_revision_fences_a_stale_claim_after_edit` | Revision CAS fences stale claims. |
| R | `crates/doc/src/queue.rs::tests::context_boundary_holds_relevant_rows_and_fails_closed_on_claims` | Context-boundary hold + fail-closed against in-flight delivery. |
| R | `crates/doc/src/queue.rs::tests::concurrent_reorder_converges_without_losing_rows` | Real Loro concurrency: concurrent moves converge with every row once. |
| R | `crates/doc/src/rebuild.rs::tests::rebuild_strips_fat_parts_and_owes_them_to_the_sidecar` | Migration (epoch-2 thin rebuild) on real Loro docs: strip, sidecar debt, only pending commands carried. |
| R | `crates/doc/src/rebuild.rs::tests::rebuilding_a_thin_doc_is_a_no_op_strip` | Migration idempotence. |
| R | `crates/doc/src/registry/tests.rs::hlc_orders_lexicographically` | merge vector shared 1:1 with edge/src/registry-core.test.ts (Rust⇄TS contract; header says change both together). |
| R | `crates/doc/src/registry/tests.rs::hlc_clock_is_monotonic_across_regressions` | merge vector shared 1:1 with edge/src/registry-core.test.ts (Rust⇄TS contract; header says change both together). |
| R | `crates/doc/src/registry/tests.rs::upsert_creates_update_never_does` | merge vector shared 1:1 with edge/src/registry-core.test.ts (Rust⇄TS contract; header says change both together). |
| R | `crates/doc/src/registry/tests.rs::field_lww_newer_wins_older_and_ties_lose` | merge vector shared 1:1 with edge/src/registry-core.test.ts (Rust⇄TS contract; header says change both together). |
| R | `crates/doc/src/registry/tests.rs::same_ms_conflicts_settle_by_device_deterministically` | merge vector shared 1:1 with edge/src/registry-core.test.ts (Rust⇄TS contract; header says change both together). |
| R | `crates/doc/src/registry/tests.rs::null_deletes_fields_with_a_clock` | merge vector shared 1:1 with edge/src/registry-core.test.ts (Rust⇄TS contract; header says change both together). |
| R | `crates/doc/src/registry/tests.rs::delete_only_wins_when_causally_newer` | merge vector shared 1:1 with edge/src/registry-core.test.ts (Rust⇄TS contract; header says change both together). |
| R | `crates/doc/src/registry/tests.rs::delete_on_missing_plants_guard_tombstone` | merge vector shared 1:1 with edge/src/registry-core.test.ts (Rust⇄TS contract; header says change both together). |
| R | `crates/doc/src/registry/tests.rs::seed_ops_preserve_original_causality` | merge vector shared 1:1 with edge/src/registry-core.test.ts (Rust⇄TS contract; header says change both together). |
| R | `crates/doc/src/registry/tests.rs::wire_shapes_match_the_edge` | Exact RowOp/RegistryRow JSON the TS registry produces/consumes (cross-language wire). |
| R | `crates/doc/src/registry/tests.rs::rows_round_trip_and_upsert_refreshes` | Typed row codec fidelity (device capabilities, chat config) through RegistryRow; no transport suite asserts full-row equality. |
| R | `crates/doc/src/registry/tests.rs::own_push_ack_never_advances_the_cursor` | Field-incident regression (cursor jump skipped peers' batches). |
| R | `crates/doc/src/registry/tests.rs::broadcast_seq_gap_applies_rows_but_holds_the_cursor` | Cursor contiguity regression. |
| R | `crates/doc/src/registry/tests.rs::pre_epoch_snapshots_resync_in_full_once` | Persistence migration: pre-epoch snapshots zero the cursor. |
| R | `crates/doc/src/registry/tests.rs::future_harness_chat_rows_stay_visible_without_their_config` | Field incident: unknown config from newer peers must cost the config, not the row. |
| R | `crates/doc/src/registry/tests.rs::field_mutators_round_trip` | Live RegistryDoc mutators (used by engine workspace_host) incl. missing-row false. |
| R | `crates/doc/src/registry/tests.rs::delete_chat_tombstones_row_and_session` | Live delete_chat cascade to session row. |
| R | `crates/doc/src/registry/tests.rs::spaces_round_trip_and_mutate` | Live space mutators + display_name fallback. |
| R | `crates/doc/src/registry/tests.rs::chat_seen_is_monotonic` | Monotonic seen guard adds no pending write. |
| D | `crates/doc/src/registry/tests.rs::two_docs_converge_through_a_server` | Detects two docs not converging through a server running the same merge. Stronger keepers: crates/sync/tests/registry_client.rs::two_clients_converge_and_stream_live_updates (real WS + mock DO protocol, same merge) and same_ms_conflicts_settle_by_device_deterministically (identical winner in both orders); live interop registry_edge.rs. No seam. |
| R | `crates/doc/src/registry/tests.rs::late_create_chat_config_survives_a_prior_claim` | Regression (missing-harness-icon clobber) on partial-field claims. |
| R | `crates/doc/src/registry/tests.rs::delete_space_cascades_and_converges` | Cascade to chats+sessions, peer convergence and idempotent re-delete (engine m5 test covers single-device cascade only). |
| R | `crates/doc/src/registry/tests.rs::persistence_round_trips_rows_pending_and_cursor` | Only proof that AUTHORITATIVE rows plus cursor and pending survive to_bytes/from_bytes (registry_client persistence test checks pending only). |
| R | `crates/doc/src/registry/tests.rs::state_frames_delta_replace_and_reseed` | apply_state Delta/Reseeded branches incl. full replace re-seeding locally authoritative rows. |
| R | `crates/doc/src/registry/tests.rs::reconnect_replay_is_idempotent` | Owner of replay idempotence; registry_client defers to it explicitly. |
| R | `crates/doc/src/registry/tests.rs::migration_seeds_pending_upserts_that_lose_to_live_writes` | Legacy→registry migration incl. two devices seeding the same legacy doc (engine workspace_sync covers single-seed only). |
| R | `crates/doc/src/registry/tests.rs::completion_marker_replicates_and_survives_next_turn` | lastCompletedTurn replication contract. |
| R | `crates/doc/src/schema.rs::tests::generated_image_persists_updates_and_salvages` | Doc-schema image part persistence + salvage (mirrored by edge generated-images.test.ts). |
| D | `crates/doc/src/schema.rs::tests::round_trips_message_entries` | push_message→read_entries literal equality; same writer helpers are asserted by conversation_replacement_keeps_non_conversation_state (read == literal entry, chat_id) and snapshot_round_trips_between_docs. Duplicate invocation. |
| R | `crates/doc/src/schema.rs::tests::conversation_replacement_keeps_non_conversation_state` | History rewrite keeps checkpoints/context state; keeper for the basic entry round trip. |
| R | `crates/doc/src/schema.rs::tests::segment_sync_persists_subagent_chip_fields_on_live_parts` | Regression: SegmentWriter::sync dropped chip fields. |
| R | `crates/doc/src/schema.rs::tests::update_subagent_chip_refuses_non_spawn_parts` | Genus gate at the doc boundary (2026-08-20 incident) + chip status strings. |
| R | `crates/doc/src/schema.rs::tests::resolve_input_stamps_the_part_in_place` | Orphan input resolution idempotence. |
| R | `crates/doc/src/schema.rs::tests::async_question_round_trips_and_dismisses_once` | Async question persistence + single dismissal. |
| R | `crates/doc/src/schema.rs::tests::snapshot_round_trips_between_docs` | Snapshot export/import contract for session docs. |
| R | `crates/doc/src/schema.rs::tests::two_peers_converge_on_concurrent_inserts` | Real Loro concurrency for the messages list. |
| R | `crates/doc/src/schema.rs::tests::segment_writer_streams_text_incrementally` | SegmentWriter multi-sync + finish produces merged text and resolved tool. |
| R | `crates/doc/src/schema.rs::tests::segment_writer_streams_reasoning_incrementally` | Old-reader degradation contract: reasoning lives in `reasoning`, never `text`. |
| F | `crates/doc/src/schema.rs::tests::segment_writer_round_trips_stripped_tool_fields` | Repointed from test-only `apply_sidecar_refs` to the live `apply_journal_output_refs`; regression (output/diff dropped in update_part_fields) kept. |
| R | `crates/doc/src/schema.rs::tests::pre_strip_doc_parts_still_round_trip` | Old pre-strip docs still read (schema additive-only contract). |
| R | `crates/doc/src/schema.rs::tests::set_message_status_stamps_existing_entry` | Recovery status stamping. |
| R | `crates/doc/src/schema.rs::tests::command_queue_and_outcome_round_trip` | Command ledger persistence; now also carries a non-empty attachment list (absorbs crates/doc/tests/attachments_roundtrip.rs). |
| R | `crates/doc/src/schema.rs::tests::tail_materializes_last_n_joined` | Tail materializer contract (mirrored by edge tail.ts). |
| R | `crates/doc/src/schema.rs::tests::malformed_entries_salvage_instead_of_vanishing` | 2026-08-10 incident: salvage instead of blank transcript. |
| R | `crates/doc/src/schema.rs::context_usage_tests::context_snapshot_survives_remote_import_restart_and_rebuild` | Context usage replication, replacement semantics and thin rebuild carry-over. |
| R | `crates/doc/src/transcript_delta.rs::tests::append_and_edit_round_trip` | Delta codec apply after JSON round trip (RPC wire). |
| R | `crates/doc/src/transcript_delta.rs::tests::remove_and_mid_insert_round_trip` | Delta codec remove + mid insert. |
| R | `crates/doc/src/transcript_delta.rs::tests::streaming_tick_is_a_text_append` | Hot path: only appended tokens travel. |
| R | `crates/doc/src/transcript_delta.rs::tests::streaming_reasoning_tick_is_a_text_append` | Reasoning growth rides the append hot path. |
| R | `crates/doc/src/transcript_delta.rs::tests::non_append_change_falls_back_to_upsert` | Non-prefix rewrite re-sends the entry. |
| R | `crates/doc/src/transcript_delta.rs::tests::append_length_mismatch_is_desync` | Desync detection (transcript_json_decode only compares decoder parity, not the error). |
| R | `crates/doc/src/transcript_delta.rs::tests::large_change_falls_back_to_reset` | Reset fallback threshold. |
| R | `crates/doc/src/transcript_delta.rs::tests::desync_surfaces_instead_of_corrupting` | Upsert after a missing anchor errors. |
| R | `crates/doc/src/transcript_delta.rs::context_update_tests::usage_envelope_is_compatible_with_old_hosts_and_readers` | Old host/reader compat of the TranscriptUpdate envelope (wire). |
| R | `crates/doc/src/transcript_delta.rs::context_update_tests::replay_cutoff_keeps_only_historical_parts_and_unicode_text_prefixes` | Replay baseline cutoff incl. char-boundary safety. |
| D | `crates/doc/src/workspace.rs::tests::set_chat_config_round_trips_and_ignores_missing_rows` | Legacy WorkspaceDoc is now only a one-time migration source: production calls only `from_doc`+`read_all` (engine workspace_host.rs:341); every WorkspaceDoc receiver in the repo is enumerated (engine workspace_host, engine tests/workspace_sync.rs, doc registry tests) and none calls this mutator. Only caller of `set_chat_config`. Unlocks deleting it. |
| D | `crates/doc/src/workspace.rs::tests::conversation_source_context_round_trips_with_legacy_fields` | Legacy WorkspaceDoc is now only a one-time migration source: production calls only `from_doc`+`read_all` (engine workspace_host.rs:341); every WorkspaceDoc receiver in the repo is enumerated (engine workspace_host, engine tests/workspace_sync.rs, doc registry tests) and none calls this mutator. Only caller of `set_chat_source_context`. Unlocks deleting it. |
| R | `crates/doc/src/workspace.rs::tests::completion_marker_survives_workspace_sync_and_legacy_rows` | Migration read path: legacy session rows without `lastCompletedTurn` still decode. |
| R | `crates/doc/src/workspace.rs::tests::rows_round_trip` | Migration read-path fidelity: devices (capabilities), chat config and sessions decode as written; the registry migration test only compares registry vs legacy reads. |
| D | `crates/doc/src/workspace.rs::tests::snapshot_round_trips_between_docs` | Detects a legacy snapshot not decoding after import. Stronger keeper: crates/engine/tests/workspace_sync.rs::legacy_workspace_doc_migrates_instantly_on_first_boot (real snapshot bytes through DocsStore, `from_doc`+`read_all`, migrated rows visible and converged). |
| D | `crates/doc/src/workspace.rs::tests::field_mutators_round_trip` | Legacy WorkspaceDoc is now only a one-time migration source: production calls only `from_doc`+`read_all` (engine workspace_host.rs:341); every WorkspaceDoc receiver in the repo is enumerated (engine workspace_host, engine tests/workspace_sync.rs, doc registry tests) and none calls this mutator. Only caller of rename_chat/set_chat_archived/set_chat_last_message/rename_device/set_device_last_seen/chat on WorkspaceDoc (RegistryDoc twins stay tested in registry/tests.rs). Unlocks deleting them. |
| D | `crates/doc/src/workspace.rs::tests::delete_chat_tombstones_row_and_session` | Legacy WorkspaceDoc is now only a one-time migration source: production calls only `from_doc`+`read_all` (engine workspace_host.rs:341); every WorkspaceDoc receiver in the repo is enumerated (engine workspace_host, engine tests/workspace_sync.rs, doc registry tests) and none calls this mutator. Only caller of WorkspaceDoc::delete_chat. Unlocks deleting it. |
| D | `crates/doc/src/workspace.rs::tests::two_peers_converge_on_disjoint_rows` | The legacy doc no longer syncs between peers (registry replaced it); CRDT convergence of a doc nobody writes. Keeper for the live contract: registry_client::two_clients_converge_and_stream_live_updates. |
| D | `crates/doc/src/workspace.rs::tests::spaces_round_trip_and_mutate` | Legacy WorkspaceDoc is now only a one-time migration source: production calls only `from_doc`+`read_all` (engine workspace_host.rs:341); every WorkspaceDoc receiver in the repo is enumerated (engine workspace_host, engine tests/workspace_sync.rs, doc registry tests) and none calls this mutator. Only caller of space/rename_space/set_space_git on WorkspaceDoc. Unlocks deleting them. |
| D | `crates/doc/src/workspace.rs::tests::delete_space_cascades_and_converges_across_peers` | Legacy WorkspaceDoc is now only a one-time migration source: production calls only `from_doc`+`read_all` (engine workspace_host.rs:341); every WorkspaceDoc receiver in the repo is enumerated (engine workspace_host, engine tests/workspace_sync.rs, doc registry tests) and none calls this mutator. Only caller of WorkspaceDoc::delete_space; live cascade owned by registry/tests.rs::delete_space_cascades_and_converges. |
| D | `crates/doc/src/workspace.rs::tests::chat_seen_is_monotonic_and_settles_lww` | Legacy WorkspaceDoc is now only a one-time migration source: production calls only `from_doc`+`read_all` (engine workspace_host.rs:341); every WorkspaceDoc receiver in the repo is enumerated (engine workspace_host, engine tests/workspace_sync.rs, doc registry tests) and none calls this mutator. Only caller of WorkspaceDoc::set_chat_seen; live rule owned by registry/tests.rs::chat_seen_is_monotonic. |
| D | `crates/doc/src/workspace.rs::tests::schema_version_stamp_is_idempotent` | Legacy WorkspaceDoc is now only a one-time migration source: production calls only `from_doc`+`read_all` (engine workspace_host.rs:341); every WorkspaceDoc receiver in the repo is enumerated (engine workspace_host, engine tests/workspace_sync.rs, doc registry tests) and none calls this mutator. Only caller of `ensure_schema_version` (and `WORKSPACE_SCHEMA_VERSION`). Unlocks deleting both. |
| D | `crates/doc/src/workspace.rs::tests::concurrent_rename_settles_lww_on_both_peers` | Legacy WorkspaceDoc is now only a one-time migration source: production calls only `from_doc`+`read_all` (engine workspace_host.rs:341); every WorkspaceDoc receiver in the repo is enumerated (engine workspace_host, engine tests/workspace_sync.rs, doc registry tests) and none calls this mutator. Only caller of WorkspaceDoc::rename_chat in a peer scenario; live LWW owned by registry merge vectors. |
| C | `crates/doc/tests/attachments_roundtrip.rs::run_request_attachments_survive_command_round_trip` | "Round 17 probe": a non-empty attachment list must survive the command ledger. Folded into schema.rs::command_queue_and_outcome_round_trip (Steer payload now carries an attachment; same generic payload→Loro conversion in `queue_command`); RunRequest serde symmetry stays owned by proto agent.rs::run_request_attachments_default_and_round_trip. File deleted. |
| R | `crates/doc/tests/transcript_json_decode.rs::wire_frames_preserve_decoder_and_application_semantics` | Wire decoder parity oracle over a malformed-frame matrix, with LUNA_PERF_EVIDENCE artifact. |
| R | `crates/proto/src/activity.rs::tests::search_defaults_are_project_scoped_and_bounded` | Wire default of HistorySearchRequest (scope, limit 50). Cross-lane: engine tests/session_history.rs::request_contract_remains_bounded repeats the limit only. |
| D | `crates/proto/src/activity.rs::tests::seen_and_acknowledged_attention_remains_unresolved` | Predicate pin (`unresolved()` = lifecycle != Resolved, ApprovalRequired actionable). Keepers at real boundaries: sync store.rs::attention_reconcile_persists_presentation_without_resolving_on_seen_or_ack (seen+acked item stays counted unresolved until reconcile) and engine tests/attention.rs (acknowledged persists unresolved across restart; transitions claimed for actionable kinds). No seam. |
| R | `crates/proto/src/agent.rs::tests::agent_event_round_trips` | AgentEvent is persisted as JSONL in the run journal and replayed across restarts (engine run_journal.rs); only test round-tripping a nested ToolCall event (storage contract). |
| R | `crates/proto/src/agent.rs::tests::subagent_model_reads_every_spelling_and_only_off_a_spawn` | Pure fn with a real input space (key spellings, genus gate, blanks); used by UI chips and doc sanitize. |
| R | `crates/proto/src/agent.rs::tests::run_request_attachments_default_and_round_trip` | Old-wire RunRequest (stored in command docs) parses with safe defaults; empty list omitted for old readers. |
| R | `crates/proto/src/agent.rs::tests::run_request_worktree_default_and_round_trip` | Old-wire compat for `worktree` + camelCase repoPath. |
| R | `crates/proto/src/agent.rs::tests::harness_id_uses_kebab_case` | Cross-language: iOS reads `claude-code` (WorkspaceStore.swift, NewSessionView.swift). |
| R | `crates/proto/src/agent.rs::tests::older_approval_actions_remain_provider_and_luna_marker_round_trips` | Exact old approval JSON round-trips; lunaCrossTask marker. |
| R | `crates/proto/src/agent.rs::generated_image_tests::generated_image_event_round_trip` | Journal storage tag names `generatedImage`/`mimeType` for replay of old journals. |
| R | `crates/proto/src/context_control.rs::tests::running_and_unknown_compaction_block_new_turns` | Fail-closed predicate used by 6 gates (rpc.rs, sessions.rs, doc_host.rs, context_transfer, UI). engine task_commands automatic_codex_context_events_gate_real_queue_drain_and_restart observes queue holds that may come from the queue hold reason, so it is not proven to exercise this predicate. |
| R | `crates/proto/src/cross_task_tools.rs::tests::model_arguments_cannot_smuggle_authority_or_provider_ids` | Security: deny_unknown_fields keeps model-supplied authority/provider ids out. |
| R | `crates/proto/src/draft.rs::tests::stale_save_outcome_uses_the_camel_case_wire_contract` | RPC/doc wire shape between differently versioned peers (retention bar: wire protocol); explicit variant-field rename `currentRevision`. |
| R | `crates/proto/src/entities.rs::tests::dense_pin_keys_remain_ordered_without_neighbor_rewrites` | Pure fractional-index fn (pin ordering) owner. |
| R | `crates/proto/src/entities.rs::tests::legacy_integer_pin_keys_deserialize_in_numeric_order` | Migration: legacy integer pin keys. |
| R | `crates/proto/src/entities.rs::tests::legacy_chat_config_defaults_to_safe_ask_never_full` | Security default: legacy danger-full-access config reads as Ask. |
| R | `crates/proto/src/entities.rs::tests::checkout_change_request_status_round_trips_all_states_as_camel_case` | Cross-language: iOS ChangeRequestTracker decodes checkoutId/changeRequest/baseRef. |
| R | `crates/proto/src/entities.rs::tests::checkout_file_diff_text_contract_is_camel_case` | RPC/doc wire shape between differently versioned peers (retention bar: wire protocol). |
| R | `crates/proto/src/entities.rs::tests::workspace_file_requests_flatten_target_and_omit_options` | RPC/doc wire shape between differently versioned peers (retention bar: wire protocol). |
| R | `crates/proto/src/entities.rs::tests::workspace_write_outcomes_have_stable_tags` | RPC/doc wire shape between differently versioned peers (retention bar: wire protocol). |
| R | `crates/proto/src/entities.rs::tests::workspace_file_change_contract_is_camel_case` | RPC/doc wire shape between differently versioned peers (retention bar: wire protocol). |
| R | `crates/proto/src/git_actions.rs::tests::pr_status_wire_keeps_no_match_distinct_from_unsupported_and_unavailable` | RPC/doc wire shape between differently versioned peers (retention bar: wire protocol); noMatch vs unsupported/unavailable is UI-visible (ui pull_requests tests read noMatch). |
| R | `crates/proto/src/motion.rs::tests::the_pulse_is_a_full_cosine_cycle` | Keeper for pulse math owned here. Cross-lane: ui motion.rs::pulse_wave_endpoints is an exact duplicate. |
| R | `crates/proto/src/motion.rs::tests::stagger_offsets_each_cell_and_wraps` | Keeper for staggered_phase (wrap + unit interval for any input). Cross-lane: ui motion.rs::stagger_wraps_and_orders_cells repeats it (keep only its matrix_wave line). |
| R | `crates/proto/src/motion.rs::tests::gradient_spin_holds_dim_then_snaps_back` | Keeper for gspin_opacity; now also carries ui motion.rs::gspin_pulse_shape's eases-down/eases-up assertions so that duplicate can go. |
| D | `crates/proto/src/motion.rs::tests::the_gradient_wave_travels_upward` | Only caller of `gspin_cell_phase`, which has zero production callers (ui loaders.rs inlines its own copy). Unlocks deleting it. |
| F | `crates/proto/src/motion.rs::tests::the_mark_sweeps_from_tail_to_head` | Dropped the loop over dead `mark_phase` (zero production callers; rem_euclid makes it vacuous); mark_cell_stagger assertions kept. Cross-lane: ui loaders.rs::mark_stagger_follows_flight_axis duplicates the rest. |
| D | `crates/proto/src/motion.rs::tests::the_mini_ring_visits_every_cell_once` | Pins `MINI_RING`/`MINI_RING_LEN`, which have zero references outside the test. Copied-constant check of dead code. Unlocks deleting both. |
| R | `crates/proto/src/runtime_contract.rs::tests::legacy_inventory_json_decodes_with_unknown_contract_facts` | Compat: old inventory JSON decodes with Unknown facts. |
| R | `crates/proto/src/runtime_contract.rs::tests::public_diagnostics_omit_scrubbed_provider_session_id` | Privacy: scrubbed provider session id never serialized. |
| R | `crates/proto/src/task_identity.rs::tests::lineage_fields_remain_independent_on_the_wire` | RPC/doc wire shape between differently versioned peers (retention bar: wire protocol); sourceTaskId/lifecycleOwnerTaskId consumed by engine MCP outputs. |
| R | `crates/proto/src/task_identity.rs::tests::reconciliation_states_use_stable_camel_case_names` | RPC/doc wire shape between differently versioned peers (retention bar: wire protocol). |
| R | `crates/proto/src/usage.rs::tests::reported_resets_require_a_recent_ready_check_for_countdown` | Pure decision table (reset_state) used by UI usage panel; sole owner. |
| R | `crates/proto/src/usage.rs::tests::physical_sources_and_verified_accounts_dedupe` | merge_usage_snapshots dedupe rule owner (engine usage_history only covers duplicate snapshots). |
| R | `crates/proto/src/usage.rs::tests::opencode_go_limits_merge_only_verified_matching_accounts` | Merge rule for unverified accounts. |
| C | `crates/proto/src/view.rs::gate_tests::workspace_scope_controls_the_auth_gate` | Folded into one table `gate_tests::gate_phase_resolves_connection_scope_and_auth` (same rows, plus Connecting/Failed/Synced+SignedIn rows carried from ui state.rs::gate_phases so that UI duplicate can go). Security-relevant auth gate kept at its owner. |
| C | `crates/proto/src/view.rs::gate_tests::development_and_local_never_use_the_workos_gate` | Folded into one table `gate_tests::gate_phase_resolves_connection_scope_and_auth` (same rows, plus Connecting/Failed/Synced+SignedIn rows carried from ui state.rs::gate_phases so that UI duplicate can go). Security-relevant auth gate kept at its owner. |
| C | `crates/proto/src/view.rs::gate_tests::missing_scope_falls_back_to_a_synced_gate` | Folded into one table `gate_tests::gate_phase_resolves_connection_scope_and_auth` (same rows, plus Connecting/Failed/Synced+SignedIn rows carried from ui state.rs::gate_phases so that UI duplicate can go). Security-relevant auth gate kept at its owner. |
| D | `crates/proto/src/view.rs::checkout_tests::local_resolves_by_whether_the_ref_has_a_worktree` | proto `view::{CheckoutKind, CheckoutPlan, checkout_plan, checkout_label}` have zero references outside view.rs (UI pickers.rs defines its own enums and methods; the proto copy served the removed TUI). Dead code; unlocks deleting the whole section. |
| D | `crates/proto/src/view.rs::checkout_tests::new_worktree_carries_its_base_and_tolerates_none` | proto `view::{CheckoutKind, CheckoutPlan, checkout_plan, checkout_label}` have zero references outside view.rs (UI pickers.rs defines its own enums and methods; the proto copy served the removed TUI). Dead code; unlocks deleting the whole section. |
| D | `crates/proto/src/view.rs::checkout_tests::labels_say_which_of_the_three_outcomes_you_will_get` | proto `view::{CheckoutKind, CheckoutPlan, checkout_plan, checkout_label}` have zero references outside view.rs (UI pickers.rs defines its own enums and methods; the proto copy served the removed TUI). Dead code; unlocks deleting the whole section. |
| R | `crates/proto/src/workspace.rs::tests::workspace_scope_uses_wire_safe_names` | RPC/doc wire shape between differently versioned peers (retention bar: wire protocol). |
| R | `crates/proto/src/workspace.rs::tests::engine_info_uses_camel_case_fields` | Exact capability inventory is a deliberate user decision (ed3a5f71: 'The pin stays exact, so a renamed capability still fails'); capability strings are compared across versions (iOS reads message-queue-v1). |
| R | `crates/proto/src/workspace.rs::tests::old_engine_info_defaults_to_no_capabilities` | Compat: older engines advertise nothing. |
| D | `crates/rpc/examples/e2e_driver.rs::tests::transcript_reset_frame_is_materialized` | Replays luna-doc's TranscriptFrame decode+apply through a 2-line wrapper; owned by doc transcript_delta tests and tests/transcript_json_decode.rs, and the driver itself is exercised by scripts/e2e-smoke.sh. Example tests do not run under plain `cargo test`. |
| R | `crates/rpc/src/device_room.rs::tests::round_trips_header_and_payload` | Codec vector ported from edge/src/device-frame.test.ts (cross-language). |
| R | `crates/rpc/src/device_room.rs::tests::handles_empty_payloads_and_long_headers` | Multi-byte uleb128 vector shared with the TS encoder. |
| R | `crates/rpc/src/device_room.rs::tests::byte_parity_with_ts_encoder` | Byte-exact parity with the TS encoder. |
| R | `crates/rpc/src/device_room.rs::tests::decodes_relay_control_payloads` | Relay control error codes from the DO. |
| R | `crates/rpc/src/device_room.rs::tests::rejects_malformed_frames` | Codec hardening (truncation, overflow). |
| R | `crates/rpc/src/device_room.rs::tests::ws_url_shapes` | URL contract with the edge `/device/{id}/ws` route. |
| C | `crates/rpc/src/lib.rs::tests::memory_call_stream_and_error` | Echo/stream parts duplicate subscription_lifecycle.rs (WS, same serve_connection/RpcClient: closing_a_quiet_view_releases_its_server_lease, finite_stream_preserves_order_and_closes); its unique Failed-kind assertion is folded into `service_error_kinds_survive_the_wire`. |
| R | `crates/rpc/src/lib.rs::tests::nullable_unary_result_completes` | Regression: null replies must settle; ServerFrame ok:null vs absent. |
| R | `crates/rpc/src/lib.rs::tests::transport_error_preserves_retryable_kind` | Renamed `service_error_kinds_survive_the_wire`; now one owner for error-kind preservation across the frame codec (Failed + retryable Transport). |
| R | `crates/rpc/src/lib.rs::tests::checked_stream_acknowledges_support_and_preserves_unknown_method` | Protocol: checked subscriptions surface UnknownMethod from old services. |
| R | `crates/rpc/src/lib.rs::tests::dropping_checked_subscription_cancels_pending_server_stream` | Only proof that dropping a CHECKED subscription cancels the server stream. |
| D | `crates/rpc/src/lib.rs::tests::websocket_round_trip` | Duplicate of subscription_lifecycle.rs over the same WS path: finite_stream_preserves_order_and_closes (ordered stream + close) and closing_a_quiet_view_releases_its_server_lease (Echo call). |
| R | `crates/rpc/src/lib.rs::tests::handshake_with_origin_header_is_rejected` | Security: browser Origin handshakes refused; native clients still served. |
| D | `crates/rpc/src/lib.rs::tests::dropping_stream_receiver_cancels_server_side` | Name promises server-side cancellation but only asserts the next call works. Keeper proving both: subscription_lifecycle.rs::closing_a_quiet_view_releases_its_server_lease. Unlocks the TestService `Never` arm. |
| R | `crates/rpc/src/remote_auth.rs::tests::policy_inventory_is_complete_and_unknown_fails_closed` | Security source inspection: every declared RPC method has a remote-auth policy; unknown fails closed (cheapest independent guard). |
| R | `crates/rpc/src/remote_auth.rs::tests::concurrent_pairing_redemption_persists_one_use` | Security: single-use pairing under concurrency, no credential on disk, 0600 file mode. |
| R | `crates/rpc/tests/device_room.rs::relay_route_without_paired_device_session_fails_before_dial` | Security: no dial without a paired session. |
| R | `crates/rpc/tests/device_room.rs::token_recovery_wakes_a_signed_out_host_relay_immediately` | Token signal wakes the relay (no 2-minute sleep). |
| R | `crates/rpc/tests/device_room.rs::sign_out_closes_a_live_host_relay_immediately` | Sign-out revokes the host socket. |
| R | `crates/rpc/tests/device_room.rs::temporary_token_failure_keeps_live_host_and_peer_connections` | Refresh failure is not a sign-out. |
| R | `crates/rpc/tests/device_room.rs::token_rotation_does_not_interrupt_a_live_peer_link` | Rotation keeps live links. |
| R | `crates/rpc/tests/device_room.rs::sign_out_closes_cached_peer_links` | Sign-out closes cached peer links (shared helper row). |
| R | `crates/rpc/tests/device_room.rs::sign_out_still_closes_links_after_network_event_lag` | Regression: lagged network bus must not disable revocation. |
| R | `crates/rpc/tests/device_room.rs::relay_serves_multiple_clients_end_to_end` | E2E relay over the fake DO: call, stream, multiplexing. |
| R | `crates/rpc/tests/device_room.rs::client_disconnect_tears_down_virtual_conn` | client_closed tears down server streams. |
| R | `crates/rpc/tests/device_room.rs::host_offline_fails_fast_and_cools_down` | host_offline admission + cooldown. |
| R | `crates/rpc/tests/device_room.rs::presence_reset_clears_cooldown_immediately` | Presence-driven cooldown reset. |
| R | `crates/rpc/tests/device_room.rs::host_supersede_drops_old_links_and_recovers` | Supersede + recovery. |
| R | `crates/rpc/tests/device_room.rs::legacy_nudges_do_not_grant_rpc_access` | Security: nudges grant no RPC access. |
| R | `crates/rpc/tests/device_room.rs::live_edge_relay_round_trip` | Ignored live-edge interop keeper (real DeviceRoom DO). |
| R | `crates/rpc/tests/device_room.rs::zombie_relay_path_trips_the_echo_deadline` | 2026-08-19 incident regression; needs the `set_client_liveness_for_tests` seam (no clock-free alternative). |
| R | `crates/rpc/tests/subscription_lifecycle.rs::closing_a_quiet_view_releases_its_server_lease` | WS keeper: dropped view releases its lease; socket still serves calls. |
| R | `crates/rpc/tests/subscription_lifecycle.rs::stalled_view_does_not_wedge_other_socket_requests` | Back-pressure isolation + bounded memory. |
| R | `crates/rpc/tests/subscription_lifecycle.rs::finite_stream_preserves_order_and_closes` | WS keeper: ordered finite stream closes. |
| R | `crates/rpc/tests/subscription_lifecycle.rs::dropping_client_releases_a_live_watch` | Client drop releases watches. |
| R | `crates/rpc/tests/subscription_lifecycle.rs::inbound_eof_rejects_new_requests_on_a_half_open_transport` | Half-open transport fails closed. |
| R | `crates/rpc/tests/subscription_lifecycle.rs::canceling_server_connection_releases_child_stream_tasks` | Abort releases child tasks. |
| F | `crates/rpc/tests/subscription_lifecycle.rs::subscriptions_from_a_foreign_executor_use_the_clients_runtime` | Vacuous half: `Notify` keeps one permit, so a single wait after dropping both subscriptions passed if only one released. Now waits for a release after each drop. |
| R | `crates/sync/src/chat_client/tests.rs::catch_up_plan_covers_the_decision_table` | Pure plan table owner; now also carries the fresh-reader unreadable-frontier row. |
| R | `crates/sync/src/chat_client/tests.rs::fresh_join_backfills_rows_and_advances_cursor` | Actor join/backfill + replay-vs-live origin. |
| R | `crates/sync/src/chat_client/tests.rs::contained_frontier_skips_the_checkpoint_download` | Actor wiring of the contained branch. |
| R | `crates/sync/src/chat_client/tests.rs::missing_frontier_fetches_and_imports_the_checkpoint_first` | Actor wiring of the fetch branch. |
| C | `crates/sync/src/chat_client/tests.rs::unacked_pushes_survive_reconnect_and_acks_retire_them` | Near-duplicate of drops_and_refused_dials_deliver_the_push_exactly_once (drop mid-push, same batch id replayed, one ack, cursor 1, cursor_advances [1]). Its two unique assertions (pushed payload bytes; pending_pushes == 1 while unacked) were carried into that keeper. |
| R | `crates/sync/src/chat_client/tests.rs::permanent_rejection_retires_transient_keeps_and_retries` | F2 regression: permanent vs transient push errors. |
| R | `crates/sync/src/chat_client/tests.rs::oversized_enqueue_is_refused_at_the_door` | F2: row-cap gate (socket would close with no error). |
| R | `crates/sync/src/chat_client/tests.rs::shutdown_interrupts_a_hung_checkpoint_fetch` | F4: shutdown not blocked by a hung fetch. |
| R | `crates/sync/src/chat_client/tests.rs::server_reset_is_counted_and_head_seq_stays_honest` | F3: server reset surfaced. |
| R | `crates/sync/src/chat_client/tests.rs::hung_checkpoint_fetch_fails_the_join_within_deadline` | F4: join deadline. |
| R | `crates/sync/src/chat_client/tests.rs::seeded_at_zero_room_fetches_the_checkpoint` | M1 seed regression in both presence checks. |
| R | `crates/sync/src/chat_client/tests.rs::checkpoint_fetch_overlaps_row_backfill` | 450kbps cold-open ordering contract. |
| R | `crates/sync/src/chat_client/tests.rs::pending_push_flushes_before_backfill_completes` | Early replay ordering (old order deadlocked). |
| C | `crates/sync/src/chat_client/tests.rs::empty_frontier_with_real_checkpoint_is_not_contained` | Called pure `plan_catch_up(.., false)` — a row the table already had (cursor 2); the incident's real fix and regression live in engine chat2_host.rs (`contains_frontier(&[])` must be false, commit 2c9e306e). Folded as an annotated cursor-0 row into catch_up_plan_covers_the_decision_table. |
| R | `crates/sync/src/chat_client/tests.rs::drops_and_refused_dials_deliver_the_push_exactly_once` | Exactly-once delivery across drops/refusals; now also asserts the pushed payload and that the unacked batch stays queued. |
| R | `crates/sync/src/chat_client/tests.rs::connect_and_die_sessions_grow_backoff_until_a_stable_session_resets_it` | Backoff stability gate. |
| R | `crates/sync/src/chat_client/tests.rs::os_offline_parks_dials_and_the_online_event_unparks_immediately` | Event-driven unpark. |
| R | `crates/sync/src/chat_client/tests.rs::live_row_gap_holds_cursor_and_repairs` | 2026-08-19 wedge regression. |
| R | `crates/sync/src/chat_client/tests.rs::checkpointless_amnesty_refetches_from_zero` | Amnesty refetch for wedged cursors. |
| R | `crates/sync/src/chat_client/tests.rs::causal_gap_refetches_checkpoint_without_skipping_parked_row` | Causal gap repair. |
| R | `crates/sync/src/chat_client/tests.rs::http_polling_returns_to_live_after_replay_and_rearms_on_revisit_or_failure` | HTTP fallback lifecycle. |
| R | `crates/sync/src/chat_client/tests.rs::http_catchup_crosses_a_contained_checkpoint_and_repairs_causal_gaps` | HTTP catch-up repair. |
| R | `crates/sync/src/chat_frames.rs::tests::pins_the_wire_layout` | Byte layout shared with edge chat-frames.test.ts. |
| R | `crates/sync/src/chat_frames.rs::tests::round_trips_and_rejects_malformed` | Codec hardening. |
| R | `crates/sync/src/chat_frames.rs::tests::typed_headers_parse_server_shapes` | Server header shapes (ack.dup default). |
| R | `crates/sync/src/dial.rs::tests::interleaves_families_from_resolver_first_pick` | RFC 8305 ordering (pure). |
| R | `crates/sync/src/dial.rs::tests::single_family_passes_through` | Ordering edge case. |
| R | `crates/sync/src/dial.rs::tests::blackholed_first_address_does_not_block_the_working_one` | Real sockets: race beats a blackholed address. |
| R | `crates/sync/src/dial.rs::tests::all_failures_surface_the_last_error` | Real sockets: failure surfaces, no hang. |
| R | `crates/sync/src/registry.rs::admission_tests::guarded_rows_wait_for_reconnect_after_snapshot_without_advancing_cursor` | Freeze consistency: no doc mutation across a snapshot. (CI-routed `luna-sync --lib`). |
| D | `crates/sync/src/registry.rs::admission_tests::legacy_constructor_still_applies_fake_socket_frames_without_a_gate` | Detects the ungated constructor no longer applying frames. Every ungated public constructor (`connect`→`connect_via_tuned`→`connect_with_transport(.., None)`) is exercised end-to-end by crates/sync/tests/registry_client.rs (cursor_delta_sync_on_reconnect asserts the cursor advances over real WS). |
| R | `crates/sync/src/registry.rs::admission_tests::guarded_https_ack_denial_retries_same_pending_batch_after_thaw` | Busy ack keeps the same batch and requests replay. (CI-routed `luna-sync --lib`). |
| R | `crates/sync/src/registry.rs::admission_tests::guarded_https_pull_denial_requests_refetch_from_unchanged_cursor` | Denied pull refetches from the unchanged cursor. (CI-routed `luna-sync --lib`). |
| R | `crates/sync/src/registry.rs::admission_tests::admitted_doc_mutation_keeps_freeze_quiescing_until_it_finishes` | Admitted writers delay the freeze. (CI-routed `luna-sync --lib`). |
| R | `crates/sync/src/registry.rs::admission_tests::begin_stop_revokes_fake_dial_and_https_child_before_async_join` | Stop revokes dial + HTTPS child (join and drop rows). (CI-routed `luna-sync --lib`). |
| R | `crates/sync/src/registry.rs::admission_tests::dropping_unused_registry_drain_aborts_both_handles` | RegistryDrain Drop aborts both tasks (begin_stop may pass via the shutdown signal alone). (CI-routed `luna-sync --lib`). |
| R | `crates/sync/src/registry.rs::admission_tests::actor_exit_abort_keeps_https_handle_available_for_join` | OfflineTasks abort keeps the joinable handle. (CI-routed `luna-sync --lib`). |
| R | `crates/sync/src/registry.rs::admission_tests::shutdown_and_drop_cancel_https_child_and_fake_dial` | Shutdown and drop both cancel children. (CI-routed `luna-sync --lib`). |
| R | `crates/sync/src/registry.rs::admission_tests::retired_admission_stops_guarded_actor_without_applying_frame` | Retired epoch stops the actor before applying. (CI-routed `luna-sync --lib`). |
| R | `crates/sync/src/registry.rs::admission_tests::canceled_guarded_constructor_aborts_its_actor` | Constructor cancellation aborts the actor. (CI-routed `luna-sync --lib`). |
| R | `crates/sync/src/registry.rs::admission_tests::signaled_stop_awaits_pending_guarded_constructor_actor` | Signaled stop awaits the actor. (CI-routed `luna-sync --lib`). |
| R | `crates/sync/src/registry.rs::admission_tests::stop_wins_ready_tie_and_awaits_pending_actor_and_https_child` | Stop wins a ready tie. (CI-routed `luna-sync --lib`). |
| R | `crates/sync/src/store.rs::tests::attention_reconcile_persists_presentation_without_resolving_on_seen_or_ack` | Attention storage contract; keeper for the deleted proto predicate test. |
| R | `crates/sync/src/store.rs::tests::attention_heartbeat_is_stable_and_recovery_kind_change_reopens_notice` | Heartbeat stability + kind change reopens. |
| R | `crates/sync/src/store.rs::tests::attention_recent_is_bounded_without_evicting_unresolved_work` | Bound without evicting unresolved items. |
| R | `crates/sync/src/store.rs::tests::repeat_outage_reopens_without_inheriting_old_acknowledgment` | Repeat outage does not inherit ack. |
| R | `crates/sync/src/store.rs::tests::guarded_snapshot_saves_require_the_matching_epoch` | Backup freeze admission (storage). |
| R | `crates/sync/src/store.rs::tests::guarded_default_connection_rejects_legacy_mutations_across_freeze` | Read-only default connection under guard. |
| R | `crates/sync/src/store.rs::tests::guarded_database_registration_covers_aliases_and_all_live_handles` | Open conflict incl. symlink aliases. |
| R | `crates/sync/src/store.rs::tests::concurrent_open_modes_never_share_a_database` | Guarded/unguarded exclusivity under races. |
| R | `crates/sync/src/store.rs::tests::opening_reservation_blocks_before_sqlite_open` | Reservation precedes sqlite open. |
| R | `crates/sync/src/store.rs::tests::snapshot_roundtrip_and_overwrite` | Unguarded save/load/overwrite and key isolation. |
| R | `crates/sync/src/store.rs::tests::activity_index_reopens_deduplicates_pages_and_rolls_back_failed_rebuild` | Activity index storage + rollback + policy. |
| R | `crates/sync/src/store.rs::tests::activity_index_serializes_concurrent_task_updates` | Concurrent task replacement. |
| R | `crates/sync/src/store.rs::tests::task_activity_page_uses_exact_task_and_opaque_cursor` | Task paging contract. |
| R | `crates/sync/src/store.rs::tests::cursor_rides_the_snapshot_row` | Legacy save keeps the chat2 cursor. |
| C | `crates/sync/src/store.rs::tests::processed_ledger_claims_exactly_once` | Near-duplicate of reopen_preserves_data_and_migrations_are_idempotent (both mark cmd-1 and check re-claim). Its unique first-claim/not-yet-processed assertions were carried into that keeper. |
| R | `crates/sync/src/store.rs::tests::reopen_preserves_data_and_migrations_are_idempotent` | Reopen/migration idempotence; now also owns the processed-ledger exactly-once claim. |
| R | `crates/sync/src/store.rs::publication_tests::outbox_survives_restart_initializes_once_and_preserves_cursor` | Publication outbox durability. |
| R | `crates/sync/src/store.rs::draft_tests::appshot_lifecycle_is_paired_minimal_and_bound_to_accepted_task` | Appshot privacy (no captured text persisted) + lifecycle. |
| R | `crates/sync/src/store.rs::draft_tests::appshot_removal_and_stale_save_do_not_recreate_capture` | Stale save cannot resurrect a capture. |
| R | `crates/sync/src/store.rs::draft_tests::appshot_staging_waits_for_saved_screenshot_blob` | Staging waits for the blob. |
| R | `crates/sync/src/store.rs::draft_tests::accepted_queue_is_not_recorded_as_sent` | Queued vs Sent distinction. |
| F | `crates/sync/src/store.rs::draft_tests::draft_revision_submission_and_blob_ownership_are_atomic` | Assertions unchanged; the owner-count query moved from a `#[cfg(test)]` method on DocsStore into a test-local helper, removing the production seam. |
| R | `crates/sync/src/store.rs::publication_failure_tests::guarded_enqueue_denial_does_not_invalidate_later_admitted_snapshot` | Denied enqueue leaves no failed-publication mark. |
| R | `crates/sync/src/store.rs::publication_failure_tests::cursor_save_after_failed_outbox_write_keeps_a_durable_replay_obligation` | Injected disk failure keeps the replay obligation. |
| R | `crates/sync/src/store.rs::task_command_tests::receipts_survive_restart_detect_conflicts_and_recover_transactions` | Task command receipts storage contract. |
| R | `crates/sync/src/store.rs::task_command_tests::task_cleanup_preserves_shared_blob_and_recovers_after_failed_retirement` | Shared blob survives cleanup; failed retirement recovers. |
| R | `crates/sync/src/store.rs::task_command_tests::cleanup_keyset_pages_advance_past_the_first_batch` | Keyset paging past the first batch. |
| R | `crates/sync/src/write_epoch.rs::tests::freeze_drains_writers_and_separates_snapshot_from_next_epoch` | WriteEpoch state machine owner (backup freeze contract used by every store/registry writer). |
| R | `crates/sync/src/write_epoch.rs::tests::timed_out_or_cancelled_drain_reopens_without_advancing_epoch` | WriteEpoch state machine owner (backup freeze contract used by every store/registry writer). |
| R | `crates/sync/src/write_epoch.rs::tests::dropping_flush_guard_reopens_admission` | WriteEpoch state machine owner (backup freeze contract used by every store/registry writer). |
| R | `crates/sync/src/write_epoch.rs::tests::retirement_wakes_waiter_and_cancelled_freeze_stays_retired` | WriteEpoch state machine owner (backup freeze contract used by every store/registry writer). |
| R | `crates/sync/src/write_epoch.rs::tests::dropped_exclusive_guards_cannot_reopen_retired_epoch` | WriteEpoch state machine owner (backup freeze contract used by every store/registry writer). |
| R | `crates/sync/src/write_epoch.rs::tests::previously_admitted_writer_can_finish_after_retirement` | WriteEpoch state machine owner (backup freeze contract used by every store/registry writer). |
| R | `crates/sync/tests/activity_index_write_e2e.rs::activity_replacement_preserves_atomicity_search_and_restart` | File-backed acceptance matrix with LUNA_PERF_EVIDENCE artifact. |
| C | `crates/sync/tests/project_command_store.rs::newer_database_schema_is_rejected_without_mutation` | Same contract as side_chat_store.rs::next_schema_is_rejected_before_wal_or_migration_changes (future schema fails closed, bytes unchanged), which is stronger (also no WAL created); its `newer` wording assertion was carried into that keeper. |
| R | `crates/sync/tests/project_command_store.rs::migration_snapshot_ownership_and_attempt_fence` | File-backed project command storage + attempt fence. |
| R | `crates/sync/tests/registry_client.rs::two_clients_converge_and_stream_live_updates` | Transport keeper: convergence + live propagation over WS. |
| R | `crates/sync/tests/registry_client.rs::cursor_delta_sync_on_reconnect` | Delta catch-up via cursor; keeper for the ungated constructor. |
| F | `crates/sync/tests/registry_client.rs::unacked_writes_replay_idempotently_after_reconnect` | Name promised an unacked replay the body never performs (its own comment defers that to the merge layer). Renamed `acked_write_is_not_re_pushed_on_reconnect` and documented; replay idempotence stays owned by luna-doc registry `reconnect_replay_is_idempotent`. |
| R | `crates/sync/tests/registry_client.rs::server_wipe_reseeds_from_client_rows` | Operator wipe re-seed. |
| R | `crates/sync/tests/registry_client.rs::gc_floor_forces_full_resync` | GC floor full resync. |
| R | `crates/sync/tests/registry_client.rs::presence_beats_reach_peers` | Presence incl. late joiners. |
| R | `crates/sync/tests/registry_client.rs::probe_answers_and_stats_flow` | Probe/stats over the socket. |
| R | `crates/sync/tests/registry_client.rs::persisted_doc_survives_restart_with_docs_store` | Pending survives restart and pushes. |
| R | `crates/sync/tests/registry_client.rs::churn_stays_bounded_no_history_growth` | Wedge regression: bounded rows. |
| R | `crates/sync/tests/registry_client.rs::applied_events_fire_for_republish` | Applied event for republish. |
| R | `crates/sync/tests/registry_client.rs::unreadable_http_ack_retries_instead_of_stranding_the_batch` | Regression: stranded batch on unreadable ack. |
| R | `crates/sync/tests/registry_edge.rs::two_rust_clients_converge_through_a_real_registry_do` | Ignored live interop with the TS RegistryRoom DO (Rust⇄TS). |
| R | `crates/sync/tests/registry_edge.rs::cursor_delta_and_churn_stay_bounded_on_a_real_do` | Ignored live churn bound on the real DO. |
| R | `crates/sync/tests/side_chat_store.rs::v13_profile_migrates_once_and_preserves_existing_command_storage` | Migration from v13. |
| R | `crates/sync/tests/side_chat_store.rs::failed_v14_migration_rolls_back_all_new_tables_and_version` | Migration rollback. |
| R | `crates/sync/tests/side_chat_store.rs::next_schema_is_rejected_before_wal_or_migration_changes` | Keeper for future-schema rejection (absorbs project_command_store's `newer` wording). |

## Layer plan

The redundant layers in this lane were not whole suites but **dead production surfaces kept alive only by their own unit tests**, plus a few in-crate replays of contracts that already have a stronger transport or end-to-end keeper.

### Keeper per contract

| Contract | Keeper | Retired duplicates |
| --- | --- | --- |
| Registry merge semantics (Rust ⇄ TS) | `crates/doc/src/registry/tests.rs` merge vectors (mirrored in `edge/src/registry-core.test.ts`) | — |
| Registry convergence over a server | `crates/sync/tests/registry_client.rs::two_clients_converge_and_stream_live_updates` (+ live `registry_edge.rs`) | `registry/tests.rs::two_docs_converge_through_a_server` |
| Registry replay idempotence | `registry/tests.rs::reconnect_replay_is_idempotent` | `registry_client.rs` test renamed to what it proves (`acked_write_is_not_re_pushed_on_reconnect`) |
| Ungated registry constructor applies frames | `registry_client.rs::cursor_delta_sync_on_reconnect` | `sync registry.rs::admission_tests::legacy_constructor_still_applies_fake_socket_frames_without_a_gate` |
| Legacy workspace snapshot → registry migration | `crates/engine/tests/workspace_sync.rs::legacy_workspace_doc_migrates_instantly_on_first_boot`; decode fidelity in `workspace.rs::{rows_round_trip, completion_marker_survives_workspace_sync_and_legacy_rows}`; multi-seed in `registry/tests.rs::migration_seeds_pending_upserts_that_lose_to_live_writes` | 11 `workspace.rs` tests of mutators nobody calls |
| Session-doc entry round trip | `schema.rs::conversation_replacement_keeps_non_conversation_state`, `snapshot_round_trips_between_docs` | `schema.rs::round_trips_message_entries` |
| Command ledger payload survives Loro | `schema.rs::command_queue_and_outcome_round_trip` (now with an attachment list) + `proto agent.rs::run_request_attachments_default_and_round_trip` | `crates/doc/tests/attachments_roundtrip.rs` (file deleted) |
| Processed command is skipped | `crates/engine/tests/e2e.rs::processed_commands_are_skipped_on_redelivery` | `commands.rs::processed_commands_are_skipped` |
| Fold refresh-in-place | `parts.rs::computer_use_unknown_refreshes_the_same_persisted_tool_part` | `parts.rs::tool_call_refresh_is_idempotent` |
| Journal/sidecar ref shape `{chat}/{part}[.diff]` | `parts.rs::{fold_strips_output_to_summary_and_diff_to_stats, sidecar_refs_stamp_once_and_only_where_content_exists}`, `schema.rs::segment_writer_round_trips_stripped_tool_fields` — now on the live `apply_journal_output_refs` | test-only twin `apply_sidecar_refs` |
| Queue once-only dispatch / gated rows | `queue.rs::{claim_is_atomic_against_stale_edit_remove_and_replay, delivery_gate_blocks_a_claim}` on `claim_queued` | take/insert tests of removed APIs |
| Attention acknowledged ≠ resolved | `sync store.rs::attention_reconcile_persists_presentation_without_resolving_on_seen_or_ack`, `crates/engine/tests/attention.rs` | `proto activity.rs::seen_and_acknowledged_attention_remains_unresolved` |
| Boot gate | `proto view.rs::gate_tests::gate_phase_resolves_connection_scope_and_auth` (one table) | 3 proto gate tests (folded); UI duplicate (cross-lane) |
| Loader math | `proto motion.rs` tests | UI duplicates (cross-lane) |
| RPC stream/echo over the socket | `crates/rpc/tests/subscription_lifecycle.rs` | `rpc lib.rs::{websocket_round_trip, dropping_stream_receiver_cancels_server_side}`, echo/stream half of `memory_call_stream_and_error` |
| RPC error kinds across the frame codec | `rpc lib.rs::service_error_kinds_survive_the_wire` | `memory_call_stream_and_error` (Failed row carried) |
| chat2 catch-up plan | `chat_client/tests.rs::catch_up_plan_covers_the_decision_table` (+ engine `chat2_host.rs` `contains_frontier(&[])` regression) | `empty_frontier_with_real_checkpoint_is_not_contained` (row carried) |
| chat2 exactly-once push across drops | `chat_client/tests.rs::drops_and_refused_dials_deliver_the_push_exactly_once` | `unacked_pushes_survive_reconnect_and_acks_retire_them` (payload + pending assertions carried) |
| Processed-ledger claim | `store.rs::reopen_preserves_data_and_migrations_are_idempotent` | `processed_ledger_claims_exactly_once` (assertions carried) |
| Future schema fails closed | `crates/sync/tests/side_chat_store.rs::next_schema_is_rejected_before_wal_or_migration_changes` | `project_command_store.rs::newer_database_schema_is_rejected_without_mutation` (`newer` wording carried) |
| e2e driver transcript decode | luna-doc `transcript_delta` tests + `tests/transcript_json_decode.rs`; the driver itself via `scripts/e2e-smoke.sh` | `rpc/examples/e2e_driver.rs` inline test |

### Assertions carried into keepers

- `proto motion.rs::gradient_spin_holds_dim_then_snaps_back` gained the eases-down/eases-up checks from `ui motion.rs::gspin_pulse_shape`.
- `proto view.rs::gate_phase_resolves_connection_scope_and_auth` carries `ui state.rs::gate_phases` rows (Connecting → Loading, Failed → Failed, Synced+SignedIn → Ready).
- `chat_client/tests.rs::drops_and_refused_dials_deliver_the_push_exactly_once`: pushed payload bytes, `pending_pushes == 1` while unacked.
- `chat_client/tests.rs::catch_up_plan_covers_the_decision_table`: annotated cursor-0 unreadable-frontier row.
- `store.rs::reopen_preserves_data_and_migrations_are_idempotent`: not-yet-processed and first-claim assertions.
- `side_chat_store.rs::next_schema_is_rejected_before_wal_or_migration_changes`: `newer` wording.
- `schema.rs::command_queue_and_outcome_round_trip`: a non-empty attachment list.
- `rpc lib.rs::service_error_kinds_survive_the_wire`: the `Failed` kind row.

### Seams removed (production items whose only callers were the retired tests)

All verified with a repo-wide `rg` over `crates/` and `apps/` (and receiver enumeration for `WorkspaceDoc`, whose instances exist only in `engine/src/workspace_host.rs`, `engine/tests/workspace_sync.rs` and `doc/src/registry/tests.rs`).

- `crates/doc/src/commands.rs`: `can_composer_cancel`.
- `crates/doc/src/parts.rs`: `apply_sidecar_refs` (byte-identical test-only twin of `apply_journal_output_refs`, whose doc comment now carries the key-shape note), `sidecar_payload` (fn; `SidecarPayload` stays), `continuation_id`, `split_parts`, `join_continuations`, and the `MSG_INLINE_MAX` import.
- `crates/doc/src/queue.rs`: `insert_queued`, `take_queued`, `take_queue_head`, wrappers `finish_queued_edit` and `finish_queued_edit_with_attachments` (callers removed in 5cb05eaa and 436da359; their doc moved onto `finish_queued_edit_with_inputs`).
- `crates/doc/src/workspace.rs`: `WorkspaceDoc::{doc, rename_device, set_device_last_seen, space, rename_space, set_space_git, delete_space, set_chat_seen, set_chat_unread, set_chat_pinned_order, chat, rename_chat, set_chat_archived, set_chat_space, set_chat_branch, set_chat_source_context, set_chat_cwd, set_chat_checkout, set_chat_config, set_chat_harness_session, set_chat_last_message, set_chat_settlement_override, delete_chat, ensure_schema_version}`, private `existing_row`, `WORKSPACE_SCHEMA_VERSION`. Kept: `new/default/from_doc/export_snapshot`, the four `upsert_*` writers (engine and registry migration tests build legacy snapshots with them), the readers and `read_all`. Module doc now says the doc is a legacy migration source.
- `crates/proto/src/motion.rs`: `MINI_RING`, `MINI_RING_LEN`, `gspin_cell_phase` (UI `loaders.rs` inlines its own copy), `mark_phase`.
- `crates/proto/src/view.rs`: the whole checkout-selection section (`CheckoutKind`, `CheckoutPlan`, `checkout_plan`, `checkout_label`; UI `pickers.rs` defines its own).
- `crates/sync/src/store.rs`: `#[cfg(test)] DocsStore::attachment_owner_count` (the identical query is now a test-local helper in `draft_tests`).
- Retained on purpose: `crates/rpc/src/device_room.rs::set_client_liveness_for_tests` (`#[doc(hidden)]`), required by the 2026-08-19 zombie-relay regression; no clock-free alternative exists. `crates/sync` feature `mock-server` (shared with engine integration tests).

## Cross-lane requests

1. **L06 (ui shell)** — `crates/ui/src/motion.rs`: delete `tests::pulse_wave_endpoints` (exact duplicate of `proto motion.rs::the_pulse_is_a_full_cosine_cycle`) and `tests::gspin_pulse_shape` (now fully carried by `proto motion.rs::gradient_spin_holds_dim_then_snaps_back`); trim `tests::stagger_wraps_and_orders_cells` to its `matrix_wave` assertion (the `staggered_phase` rows are owned by `proto motion.rs::stagger_offsets_each_cell_and_wraps`).
2. **L06 (ui shell)** — `crates/ui/src/state.rs`: delete `tests::gate_phases`; every row is now in `proto view.rs::gate_tests::gate_phase_resolves_connection_scope_and_auth` (`gate_phase` is re-exported from proto).
3. **L05 (ui chat)** — `crates/ui/src/loaders.rs`: delete `tests::mark_stagger_follows_flight_axis` (duplicate of `proto motion.rs::the_mark_sweeps_from_tail_to_head`). Optional production follow-up: `loaders.rs` lines ~155–172 inline the gradient cell-phase math whose proto copy (`gspin_cell_phase`) was dead and is now removed.
4. **L03 (engine it m–z)** — `crates/engine/tests/session_history.rs::request_contract_remains_bounded` repeats the limit-50 default owned by `proto activity.rs::search_defaults_are_project_scoped_and_bounded`; D candidate.
5. **L01 (engine src)** — `crates/engine/src/doc_host.rs:7073` comment names the removed `apply_sidecar_refs`; it should say `apply_journal_output_refs` (comment only, no compile impact).
6. **CI routing (L10 / coordinator)** — no routed target moved or disappeared (`cargo test --locked -p luna-sync --lib` in `ui-tests.yml` still exists; no `[[test]]` entries reference the deleted `crates/doc/tests/attachments_roundtrip.rs`). Gap, not caused by this lane: `luna-doc`, `luna-proto`, `luna-rpc` and the `luna-sync` integration tests are not run by any workflow; `crates/sync/tests/registry_client.rs` needs `--features mock-server` when built without the engine (feature unification hides this in workspace builds).

## Possible product defects

- None observed as failing. One assertion was strengthened in a way that could expose a latent bug: `crates/rpc/tests/subscription_lifecycle.rs::subscriptions_from_a_foreign_executor_use_the_clients_runtime` now requires *each* dropped subscription (plain, then checked) to release its server lease when the caller has no Tokio runtime. Before, one `Notify` permit let it pass if only one released. If it fails at integration, treat it as a cleanup-runtime defect in `crates/rpc/src/client.rs`, not a test error.

## Follow-ups

- `crates/sync/src/registry.rs::RegistryClient::connect_with_tuned` (`pub(crate)`) has no callers anywhere; likely a `dead_code` warning today. Not test-related, left alone.
- `crates/doc/src/workspace.rs::presence_key` and `crates/proto/src/motion.rs::{LUNA_PULSE_MS, GRADIENT_SPIN_MS}` have no callers; not test-related, left alone.
- `WorkspaceDoc::upsert_*` now exist only to build legacy snapshots in two tests; once the legacy migration is retired, the whole module and `engine/tests/workspace_sync.rs::legacy_workspace_doc_migrates_instantly_on_first_boot` can go together.
- The UI keeps second implementations of the checkout plan (`pickers.rs`) and gradient cell phase (`loaders.rs`); the proto copies were dead, so the UI versions are now the only ones.
