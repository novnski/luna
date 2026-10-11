# 9 October test-audit ledgers, rebased onto `5285702d`

Repeat with `python3 scripts/qa-test-ledger-rebase.py <report.json>`.
- Raw reports: [before](evidence/ledger-rebase-before.json) and
  [after](evidence/ledger-rebase-after.json).
- Traces: [trace](evidence/ledger-trace.txt) and
  [absent retained rows](evidence/absent-retained.txt).

## Baseline identity

- **The base SHA no longer exists.** The audit names base `e4a0f534`, which is
  not in this repository; it was rewritten before landing, like the T3 audit's
  SHA. The campaign landed as the `test-audit/*` merges on 9 October, ending at
  `ecd541d4` ("Record the 2026-10-09 test-audit results"). Deltas below are
  measured from `ecd541d4`.
- **Test count.** The audit ended at 2,888 Rust test declarations. At
  `5285702d` there were 2,917, counting multi-line test attributes such as
  `#[cfg_attr(windows, ignore = …)]`.

## Retained rows (R/F) that no longer exist: 24, none lost

**20 were renamed or merged by their lane's own repair or consolidation**,
while the ledger kept the old name:

| Lane | Ledger name | Now |
| --- | --- | --- |
| L02 | `managed_offline_backup_refuses_missing_v13_v14_feature_tables` | `managed_offline_backup_refuses_missing_required_tables` |
| L04 | `effort_maps_special_modes` | `effort_maps_special_modes_and_clamps_xhigh_by_model_family` |
| L06 | `the_card_carries_one_row_per_comment` | `a_sent_comment_block_becomes_one_pill_with_one_row_per_comment` |
| L06 | `eval_never_escapes_unit_interval_dense_sweep` | `catalog_curves_pin_endpoints_and_stay_monotonic_inside_the_unit_interval` (the 1e-5 sweep noted in L06) |
| L06 | `source_context_chat_finds_local_snapshot`, `new_thread_handoff_is_continuous_and_staged`, `escape_interrupts_only_the_active_live_chat`, `steps_forward_and_back_through_the_list`, `context_intents_carry_revision_and_binding_fences` | Folded in L06's consolidation commit `e6d58669`; its surviving additions are listed in the trace |
| L07 | `pinned_modes_ignore_the_os` | `system_mode_follows_the_os_and_pinned_modes_ignore_it` |
| L07 | `capture_dimensions_use_retina_scale_for_ordinary_windows` | `capture_dimensions_use_retina_scale_and_cap_large_windows` |
| L07 | `oklch_accents_match_reference` | `oklch_matches_reference_colors` |
| L07 | `settings_without_appearance_default_to_system`, `general_page_renders_existing_preferences_and_changes_them` | Folded in L07's commit `8a3bf971` |
| L08 | `delivery_gate_makes_row_untakeable` | `delivery_gate_blocks_a_claim` |
| L08 | `transport_error_preserves_retryable_kind` | `service_error_kinds_survive_the_wire` |
| L08 | `unacked_writes_replay_idempotently_after_reconnect` (F) | `acked_write_is_not_re_pushed_on_reconnect` (the repaired assertion) |
| L09 | `affected_languages_keep_utf8_boundaries_for_incomplete_code` | `incomplete_unicode_code_keeps_spans_valid_and_non_overlapping` |
| L09 | `captured_app_identity_is_luna_owned`, `default_and_legacy_override_stay_luna_owned` (F) | `service_env_keeps_the_data_root_and_never_the_bearer`, `default_is_home_luna_and_explicit_data_dir_wins` |

**4 were changed by later product work:**
- `v2_recovered_step_failure_is_a_transient_warning` became
  `v2_retried_step_warns_and_continues_its_interrupted_text` (`8931c3e1`).
- `reasoning_ahead_of_its_message_role_is_held_and_replayed` and
  `main_feed_user_text_is_the_prompt_echo_and_never_renders` were retired when
  OpenCode began rendering text and reasoning on arrival (`7a2968ad`).
- `right_terminal_picker_with_no_history_waits_for_new` was retired by the
  right-sidebar icon rail (`7e4bca47`).

## Removed rows (C/D) whose named keeper is gone: 4

| Removed test | Keeper it cited | Outcome |
| --- | --- | --- |
| `harness opencode::v2_recovered_step_failure_does_not_poison_successful_execution` (D) | `v2_recovered_step_failure_is_a_transient_warning` | Renamed keeper (above); the contract holds. |
| `drafts-fixture::fixture_startup_loads_saved_settings_and_fresh_roots_default` (D) | `composer_send_behavior_is_opt_in_for_old_and_partial_settings` | That keeper was consolidated into `partial_legacy_settings_keep_their_values_and_default_new_keys` (L07 C row); the contract holds. |
| `ui settings::registry_offers_both_appearances_…` (D) | `visual_fixture_matrix_covers_every_variant_and_scene` | L09 deleted that copied count deliberately. The other cited keeper, `builtins_have_complete_provenance_and_no_validation_errors`, still owns registry validity. |
| `doc commands::processed_commands_are_skipped` (D, L08) and `engine e2e::processed_commands_are_skipped_on_redelivery` (C, L02) | Each other | **Circular cross-lane deletion**, the same failure the audit's review caught once for `resize_drag_sample`. The drain-level behaviour survives in `e2e::retry_reissues_a_swallowed_send`. Whether the pure `evaluate_command` Skip row is still guarded is settled by mutation in [README.md](README.md). |

## Removed rows that look re-declared: 4, all name collisions

Four L08 deletions in `doc/src/workspace.rs` (`snapshot_round_trips_between_docs`,
`field_mutators_round_trip`, `delete_chat_tombstones_row_and_session`,
`spaces_round_trip_and_mutate`) share names with existing tests in
`doc/src/registry/tests.rs` and `doc/src/schema.rs`. Those are separate
contracts, not re-additions.
