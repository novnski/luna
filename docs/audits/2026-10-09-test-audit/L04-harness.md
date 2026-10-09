# L04-harness test-audit ledger

- **Lane:** L04-harness, branch `test-audit/L04-harness`, base `e4a0f534`.
- **Scope:** all of `crates/harness/**`: inline test modules in 15 `src/` files, `src/opencode/tests.rs`, `src/process/windows/tests.rs`, 25 `tests/*.rs` targets and `tests/fixtures/` (scripted Claude/Codex/OpenCode peers and recorded real-CLI transcripts).
- **Baseline:** 389 test declarations (the `provider_lifecycle_audit` `case!` macro is one declaration expanding to 11 tests, so 399 runtime tests). Test LOC: 2,794 in inline `#[cfg(test)]` modules + 19,801 in test files (`tests/*.rs`, `src/opencode/tests.rs`, `src/process/windows/tests.rs`) = 22,595; fixtures 10,351 lines in 40 text files plus 1 committed `.pyc`.
- **After:** 364 declarations (374 runtime tests). Inline test modules 2,320 lines (+38 in the new `codex/approvals.rs` module), test files 19,663 lines. One fixture artifact deleted (`tests/fixtures/__pycache__/fake-claude-goals.cpython-314.pyc`); no peer or recorded transcript changed, so engine suites that spawn these fixtures are unaffected.
- **Marks:** R 360, F 2, C 7, D 20.
- **CI routing at baseline:** `ui-tests.yml` runs `--lib opencode::` and `--test opencode` (Linux); `windows.yml` runs `--lib --test codex_availability --test windows_native` with `native-fixture`. No workflow runs any other harness integration target (`claude*.rs`, `codex.rs`, `permission_*`, `native_inputs`, …); see Cross-lane requests.
- Nothing was compiled or run in this lane (coordinator-only). Evidence comes from reading every test, its production owner, the peers' scripts and recorded fixtures, and `git log -S`.

## Ledger

| Mark | Test | Evidence |
| --- | --- | --- |
| R | `crates/harness/src/claude/catalog.rs::tests::effort_maps_special_modes` | Keeper, renamed `effort_maps_special_modes_and_clamps_xhigh_by_model_family`: the name-heuristic `--effort` fallback (used by `runtime_controls::effort_flag` when no ladder is discovered; `claude_cc06` covers only the ladder path). Absorbs the family rows below as a table through `to_effort`. |
| C | `crates/harness/src/claude/catalog.rs::tests::xhigh_family_matching` | Folded into the `to_effort` table above (same 7 model rows, asserted as xhigh vs max). Detects: substring list drift. Unlocks: `supports_xhigh` `pub(crate)` -> private (only caller is `to_effort`). |
| D | `crates/harness/src/claude/catalog.rs::tests::ultrathink_prefixes_prompt` | Owner: `claude_inputs::typed_skill_with_images_and_prose_expands_from_the_last_text_block` (rows chip-ultrathink / typed-slash-ultrathink pin the exact `Ultrathink:\n` stdin bytes) + `claude_peers::luna_child_report_reaches_claude_...` (no prefix on the peer envelope); non-Ultrathink passthrough is every text turn. Unlocks nothing. |
| R | `crates/harness/src/claude/controls.rs::tests::parses_questions_tolerantly` | Only owner of AskUserQuestion alias tolerance (`title`/`prompt`/`multi_select`, string vs object options); real-shape rows overlap `claude_permissions` r13 but the aliases have no other proof. |
| D | `crates/harness/src/claude/controls.rs::tests::answers_key_by_question_text` | Owner: `claude-permissions-peer.py` r13 (driven by `claude_permissions::ask_user_question_keeps_descriptions_and_dismissal_denies`) checks `updatedInput.answers` keyed by question text incl. a multi-select array and `questions` preserved; `fake-claude.sh` askuser checks `"Pick one":"B"`. Same function, stronger boundary. |
| R | `crates/harness/src/claude/mod.rs::tests::permission_modes_map_to_exact_cli_arguments` | Security CLI contract: the only exact pin of `--permission-mode acceptEdits`, `auto`, `bypassPermissions` per mode; `claude_effective_config` pins only `--restricted`/`--dangerously-skip-permissions` presence and the peers echo whatever mode they get. |
| R | `crates/harness/src/claude/normalize.rs::tests::decodes_typed_tools` | Tool-decoding table; Edit->EditFile, TodoWrite->Todo, unknown->Unknown have no integration owner (`claude::happy_path` covers only Bash and mcp__). |
| R | `crates/harness/src/claude/normalize.rs::tests::stream_deltas_map_to_text_reasoning_and_heartbeats` | Keeper for stream-delta shapes: redacted-thinking and input_json heartbeats (stall detection) and dropped signatures have no other owner (the live 2.1.228 capture carries these frames but its test does not assert them). Now also holds the tagged input_json row. |
| C | `crates/harness/src/claude/normalize.rs::tests::subagent_stream_deltas_arrive_tagged` | Tagged text row duplicated `claude::happy_path_normalizes_events_and_tags_subagents` (`Subagent{sub-1, TextDelta SUBAGENT}`); the unique tagged-input_json-dropped row moved into `stream_deltas_map_to_text_reasoning_and_heartbeats`. |
| R | `crates/harness/src/claude/normalize.rs::tests::subagent_tool_calls_and_results_arrive_tagged_without_boundary_rotation` | Tagging overlaps `claude::happy_path`, but the parent assistant-message id not rotating on a subagent frame (would split parent text) has no integration owner. |
| R | `crates/harness/src/claude/normalize.rs::tests::spawn_prompt_seeds_the_subagent_opening_user_message` | Positive row also in `claude::captured_live_background_subagent_frames_replay_correctly`; the negatives (Task without prompt, Bash with a `prompt` field) are unique. |
| R | `crates/harness/src/claude/normalize.rs::tests::the_interruption_marker_is_not_a_steer` | Regression (spinning spawn chip): interruption markers on the child feed; no fixture carries them. |
| R | `crates/harness/src/claude/normalize.rs::tests::send_message_steers_rekey_onto_the_spawn_feed` | Root-level SendMessage re-key + unknown-recipient and shell-task negatives; `claude::native_nested_agents_...` covers only a nested SendMessage. |
| R | `crates/harness/src/claude/normalize.rs::tests::tagged_user_text_becomes_a_subagent_steer` | Negatives (system-reminder, blank, untagged parent text) and mixed result+text frames are unique. |
| R | `crates/harness/src/claude/normalize.rs::tests::task_notification_settles_the_subagent_with_a_tagged_done` | completed/failed overlap `claude::native_nested_agents_...`; non-terminal `running` and id-less notifications closing nothing are unique. |
| D | `crates/harness/src/claude/normalize.rs::tests::shell_task_notification_never_settles_a_subagent` | Owners: `claude::native_nested_agents_keep_ownership_model_usage_and_completion` (fixture rows 29-32: Bash `run_in_background` + `local_bash` task_started + task_notification for `toolu_A_bg`; asserts the exact subagent-Done set A,B,C,D,E) and `claude_coordination::shell_lifecycle_is_observed_without_subagent_reclassification` (Shell NativeTaskChanged Running->Completed, no Subagent for toolu_bash). |
| D | `crates/harness/src/claude/normalize.rs::tests::subagent_assistant_text_blocks_emit_tagged_text` | Owner: `claude::native_nested_agents_...` asserts tagged `TextDelta "Splitting the work.\n\n"` / `"Lexer has three states.\n\n"` and the tagged tool calls from full assistant blocks. |
| D | `crates/harness/src/claude/normalize.rs::tests::wake_turn_init_is_deduped_but_second_result_still_emits_done` | Owners: `claude::eager_done_forwards_wake_turn_as_second_done` and `claude::captured_live_background_subagent_frames_replay_correctly` (exactly one SessionStarted, two Completed Dones on the same session) through the whole driver. |
| R | `crates/harness/src/claude/normalize.rs::tests::first_init_receipt_matches_session_and_can_be_taken_once` | Retained with the code it tests: `SessionIntegrationReceipt`/`take_init_integration_receipt` have no production consumer (`#[allow(dead_code)]`, d64a46ce). Product decision, see Follow-ups. |
| R | `crates/harness/src/claude/normalize.rs::tests::duplicate_init_cannot_replace_or_reissue_first_receipt` | As above (dead receipt seam; follow-up). |
| R | `crates/harness/src/claude/normalize.rs::tests::missing_or_oversized_session_identity_keeps_start_event_without_receipt` | As above (dead receipt seam; follow-up). The SessionStarted-still-emitted half is live behavior. |
| R | `crates/harness/src/claude/normalize.rs::tests::rejected_or_empty_wire_observation_creates_no_receipt` | As above (dead receipt seam; follow-up). |
| C | `crates/harness/src/claude/normalize.rs::tests::ede_diagnostics_never_surface_as_errors` | Folded with the next two into one table `result_errors_drop_diagnostics_but_keep_real_text_and_subtype_wording` (row 1: diagnostic-only error_during_execution -> Errored, no error). Interrupted variant also owned by `claude::cc01_stop_sends_a_unique_cancel_queued_interrupt_and_drains_before_signals`. |
| C | `crates/harness/src/claude/normalize.rs::tests::real_errors_survive_diagnostic_filtering` | Row 2 of the consolidated result-error table. |
| C | `crates/harness/src/claude/normalize.rs::tests::known_failure_subtypes_keep_mapped_wording` | Row 3 (diagnostic-only error_max_turns -> mapped wording) kept in the table; the empty-`errors` row dropped: owned by `claude::error_codes_map_to_readable_messages` (scenario:error ends `error_max_turns` with `errors: []`). |
| R | `crates/harness/src/claude/normalize.rs::context_tests::context_counts_cached_prompt_and_matches_primary_model` | Context-meter contract (cache reads counted, child usage ignored, window from the primary model); no harness integration asserts Claude ContextUsage. |
| D | `crates/harness/src/claude/wire.rs::tests::parses_known_and_unknown_frames` | Owners: every fake-CLI suite parses init; `claude::cc02_lifecycle_unknown_and_tool_bodies_are_decoded_and_retained` asserts unknown types retained (`unknown:future_frame`) and non-JSON reported (`invalid:non_json`). |
| R | `crates/harness/src/claude/wire.rs::tests::init_observation_keeps_only_allowlisted_names_and_statuses` | Security/parser owner for the init observation consumed by `luna_tools.rs` (Connected status gate); secret redaction in Debug. |
| R | `crates/harness/src/claude/wire.rs::tests::missing_unknown_and_malformed_observation_fields_do_not_break_init` | Parser robustness owner; malformed `tools` rejection pinned here only. |
| R | `crates/harness/src/claude/wire.rs::tests::oversized_observation_fields_are_ignored_without_truncation_claims` | Bounds owner (MAX_INIT_OBSERVATIONS / MAX_INIT_NAME_BYTES). |
| R | `crates/harness/src/claude/wire.rs::tests::user_line_shape_matches_protocol` | Stream-json user-line wire pin; `parent_tool_use_id: null` is asserted nowhere else (`claude_inputs` pins type/content only). |
| R | `crates/harness/src/codex/catalog.rs::tests::effort_clamps_like_codex_ts` | Codex rejects `minimal` and has no ultracode/ultrathink: those clamps are pinned only here (`fake-codex.sh` checks only the `ultra` passthrough). |
| R | `crates/harness/src/codex/mod.rs::computer_use_approval_tests::only_explicit_one_request_form_choice_is_supported` | Computer-use approval safety: url mode / enum without `once` never auto-answer; fakes only send the accepted shape. |
| R | `crates/harness/src/codex/mod.rs::computer_use_approval_tests::app_target_requires_a_concrete_provider_name` | `_meta.app_name` path and the no-name negative are unique; fakes use the message form only. |
| R | `crates/harness/src/codex/mod.rs::tests::async_question_requires_typed_agent_message_metadata` | Harness-level only owner (engine e2e scripts AsyncQuestions through a mock harness). |
| R | `crates/harness/src/codex/mod.rs::tests::named_command_passes_file_auth_override_to_synthetic_child` | Auth contract for `codex login` (CODEX_HOME + `cli_auth_credentials_store="file"`); no other caller-level test. |
| C | `crates/harness/src/codex/mod.rs::tests::approval_questions_are_typed_and_machine_scoped` | Needed the `#[cfg(test)] approval_question` / `approvals::legacy_question` seam. Header/options/single-command rows owned by `codex::approvals_round_trip_as_input_requests`; the unique rows (argv-array join, multi-path join, machine scope) moved as `codex/approvals.rs::tests::approval_card_joins_argv_and_paths_and_is_machine_scoped`, calling the private builder directly. Seams removed. |
| D | `crates/harness/src/codex/mod.rs::tests::permission_modes_map_to_exact_app_server_settings` | Owner: `permission_modes::codex_start_resume_and_turn_keep_the_same_explicit_permission_policy` pins approvalPolicy/approvalsReviewer/sandbox/sandboxPolicy for Ask/Auto/Full on start, resume and turn through the real adapter. |
| R | `crates/harness/src/codex/mod.rs::tests::model_page_skips_hidden_and_unknown_efforts` | Parser rows (hidden, unknown effort, description trim) not exercised by `codex::models_discovers_visible_catalog_with_pagination`. |
| R | `crates/harness/src/codex/mod.rs::tests::turn_router_never_revives_completed_turns` | Turn-router ordering invariant (late turn/start response after turn/completed); no fake scenario reproduces that interleaving. |
| R | `crates/harness/src/codex/native.rs::tests::only_quota_backend_id_is_a_verified_account_identity` | Privacy contract: email never in the account signature; only the backend id is a verified identity. |
| R | `crates/harness/src/codex/native.rs::tests::sparse_codex_windows_keep_reported_duration_and_other_bucket_separate` | Quota-window parser owner. |
| R | `crates/harness/src/codex/native.rs::tests::compact_completion_requires_matching_native_context_item` | Other-thread/stale rows overlap `codex::native_compaction_resumes_bound_thread_...`; wrong item type and failed-turn -> Failed are unique. |
| R | `crates/harness/src/codex/native.rs::tests::fork_params_keep_before_exclusive_and_through_inclusive` | Fork boundary params + semantic readback; integration forks use the Tip boundary only. |
| D | `crates/harness/src/codex/normalize.rs::computer_use_tests::exact_provider_turn_lifecycle_and_evidence_without_payload` | Owner: `codex::computer_use_approval_and_provider_evidence_stay_on_exact_turn` (fake `scenario:computer-use:allow` sends started, a stale-turn completion, the real completion with `private pixels` and a duplicate completion; asserts exactly [Running, ProviderCompleted] on Finder with screenshot observed, one ToolResult, no payload). |
| R | `crates/harness/src/codex/normalize.rs::computer_use_tests::wrong_target_and_late_results_cannot_settle_an_active_call` | Foreign-thread start row is unique (fakes only send wrong-turn completions). |
| R | `crates/harness/src/codex/normalize.rs::computer_use_tests::no_observed_computer_use_remains_generic_and_disconnect_is_unknown` | `take_unknown` reporting once is unique; `codex::computer_use_disconnect_reports_unknown_without_replay` asserts only `any` Unknown. |
| R | `crates/harness/src/codex/normalize.rs::computer_use_tests::completion_only_surface_is_observed_on_current_turn` | node_repl completion-only surface with task binding; not in any fake. |
| R | `crates/harness/src/codex/normalize.rs::computer_use_tests::settled_item_id_may_be_reused_on_a_later_turn` | Item-id reuse across turns; not in any fake. |
| R | `crates/harness/src/codex/normalize.rs::tests::reasoning_parts_preserve_chunking_and_existing_paragraph_breaks` | `item/reasoning/textDelta` contentIndex chunking, existing breaks, empty delta, snake_case ids; `codex::reasoning_preserves_summary_parts_...` covers the summary path only. |
| R | `crates/harness/src/codex/normalize.rs::tests::user_message_text_accepts_both_shapes` | Wire tolerance (text vs content blocks) used for steer correlation; no single fake carries both. |
| R | `crates/harness/src/codex/normalize.rs::tests::delta_accepts_both_spellings` | Spellings overlap `codex::happy_path`; empty/missing -> None rows unique. |
| R | `crates/harness/src/codex/normalize.rs::tests::command_execution_maps_exit_code_to_error` | snake_case `command_execution`/`exit_code` tolerance unique (happy path is camelCase). |
| R | `crates/harness/src/codex/normalize.rs::tests::file_change_variants_map_to_typed_calls` | Declined->error and multi-change `f3#1` ApplyPatch rows unique (`codex::happy_path`/`tool_history` cover add/update). |
| R | `crates/harness/src/codex/normalize.rs::tests::usage_reads_last_snapshot_under_both_spellings` | snake_case and absent rows unique. |
| R | `crates/harness/src/codex/normalize.rs::tests::v1_spawns_and_controls_have_distinct_roles` | Both tool spellings, failed spawn, futureControl and invalid receiver lists; `codex::v1_spawns_bind_children_...` uses one fixture shape. |
| R | `crates/harness/src/codex/normalize.rs::tests::activity_updates_are_not_spawns_in_either_item_phase` | Asserts no event at all for activity kinds in both phases; `v2_lifecycle` only asserts the spawn list. |
| D | `crates/harness/src/codex/normalize.rs::tests::sub_agent_activity_maps_to_a_named_parent_chip` | Owner: `codex::child_thread_routing_tags_and_never_settles_parent` (fake emits started+completed `subAgentActivity` call_alpha; asserts `ToolCall Unknown "Agent: alpha"` and a non-error ToolResult for call_alpha). |
| R | `crates/harness/src/codex/normalize.rs::tests::child_usage_notification_keeps_only_the_provider_reported_snapshot` | Child usage notification; no fake emits child tokenUsage. |
| R | `crates/harness/src/codex/normalize.rs::tests::child_idle_and_active_are_observed_without_settling_a_turn` | Child idle/active -> ChildStatus; the fake only sends `running` (consumed). |
| R | `crates/harness/src/codex/normalize.rs::tests::child_routing_table_fails_open_to_parent` | Routing table owner (fail-open rule for unknown methods). |
| R | `crates/harness/src/codex/normalize.rs::tests::notification_thread_ids_read_both_shapes` | thread/started `thread.id` vs `threadId` shapes. |
| D | `crates/harness/src/codex/normalize.rs::tests::turn_error_extraction` | Owners: `codex::turn_failed_maps_to_errored_done` (Done error `boom`) and `codex::happy_path_maps_deltas_items_usage_and_done` (no error -> None). |
| R | `crates/harness/src/codex/normalize.rs::context_tests::context_uses_latest_call_not_thread_total` | `last` vs `total` distinction and snake_case/zero-window rows; the fake sends `last` only. |
| D | `crates/harness/src/codex/normalize.rs::context_tests::context_compaction_requires_root_thread_turn_and_item_correlation` | Owners: `codex::automatic_context_compaction_is_correlated_and_never_completes_the_turn` (fake: child-thread compaction and stale item ignored, matching id completes, op id equal, thread/turn exact) and `codex::stale_turn_and_duplicate_full_compaction_lifecycles_are_ignored` (other-turn start ignored). |
| D | `crates/harness/src/codex/normalize.rs::context_tests::interrupted_context_compaction_becomes_unknown_once` | Owner: `codex::interrupted_or_disconnected_automatic_compaction_becomes_unknown_once` asserts the exact status list [Running, Unknown] (so once). |
| R | `crates/harness/src/codex/normalize.rs::generated_image_tests::image_generation_lifecycle_ignores_inline_result_and_preserves_ids` | snake_case variant, exact ids/name, idempotent completion and revisedPrompt privacy are unique; `codex::image_generation_fake_lifecycle_...` covers success/failure/missing only. |
| R | `crates/harness/src/codex/normalize.rs::generated_image_tests::image_generation_errors_resolve_the_chip_without_an_image` | Six failure variants vs two in the fake. |
| D | `crates/harness/src/codex/subagents.rs::tests::binding_is_stable_and_children_are_independent` | Owner: `codex::child_identity_survives_early_output_and_later_activity_ids` (fixture `child-identity.jsonl`: later `interacted` activity for alpha never rebinds; alpha/beta transcripts asserted exactly). |
| D | `crates/harness/src/codex/subagents.rs::tests::early_content_waits_for_the_spawn_and_drains_once_in_order` | Owner: same test: `early alpha` precedes the spawn, the chip exists before it binds, and alpha text equals `early alphalater alpha` despite started+completed binding twice (a re-drain would duplicate it). |
| R | `crates/harness/src/codex/subagents.rs::tests::root_and_missing_ids_never_bind_or_buffer` | Root/empty ids never buffer; not reachable through a fake. |
| R | `crates/harness/src/codex/subagents.rs::tests::unbound_backlog_is_bounded_without_affecting_known_children` | Memory bound (MAX_PENDING_BYTES). |
| R | `crates/harness/src/executable.rs::tests::windows_discovery_uses_pathext_order_and_skips_empty_path_entries` | Platform contract for executable discovery through the production `Platform`/env injection (a real production parameter, not a test seam); runs on every OS, while `codex_availability`/`windows_native` only run on Windows. |
| R | `crates/harness/src/executable.rs::tests::windows_pathext_environment_reorders_extensions` | Platform contract for executable discovery through the production `Platform`/env injection (a real production parameter, not a test seam); runs on every OS, while `codex_availability`/`windows_native` only run on Windows. |
| R | `crates/harness/src/executable.rs::tests::windows_extra_candidates_resolve_through_pathext_variants` | Platform contract for executable discovery through the production `Platform`/env injection (a real production parameter, not a test seam); runs on every OS, while `codex_availability`/`windows_native` only run on Windows. |
| R | `crates/harness/src/executable.rs::tests::windows_node_manager_dirs_use_explicit_environment_and_userprofile_fallback` | Platform contract for executable discovery through the production `Platform`/env injection (a real production parameter, not a test seam); runs on every OS, while `codex_availability`/`windows_native` only run on Windows. |
| R | `crates/harness/src/executable.rs::tests::windows_node_manager_discovery_finds_active_and_default_installations` | Platform contract for executable discovery through the production `Platform`/env injection (a real production parameter, not a test seam); runs on every OS, while `codex_availability`/`windows_native` only run on Windows. |
| R | `crates/harness/src/executable.rs::tests::windows_node_manager_defaults_honor_redirected_appdata_and_volta_override` | Platform contract for executable discovery through the production `Platform`/env injection (a real production parameter, not a test seam); runs on every OS, while `codex_availability`/`windows_native` only run on Windows. |
| R | `crates/harness/src/executable.rs::tests::unix_discovery_preserves_exact_name_and_source_order` | Platform contract for executable discovery through the production `Platform`/env injection (a real production parameter, not a test seam); runs on every OS, while `codex_availability`/`windows_native` only run on Windows. |
| R | `crates/harness/src/executable.rs::tests::windows_native_overrides_accept_batch_shims_and_reject_the_unlaunchable` | Platform contract for executable discovery through the production `Platform`/env injection (a real production parameter, not a test seam); runs on every OS, while `codex_availability`/`windows_native` only run on Windows. |
| R | `crates/harness/src/executable.rs::tests::portable_base_dir_uses_current_dir_when_home_is_unset` | Platform contract for executable discovery through the production `Platform`/env injection (a real production parameter, not a test seam); runs on every OS, while `codex_availability`/`windows_native` only run on Windows. |
| R | `crates/harness/src/jsonrpc.rs::tests::rpc_error_preserves_string_and_structured_details` | JSON-RPC error renderer table (string/object/array `data`). |
| R | `crates/harness/src/jsonrpc.rs::tests::rpc_error_handles_missing_null_and_unstructured_fields` | Same renderer, degenerate shapes; integration only reaches the message-only path. |
| R | `crates/harness/src/opencode/mod.rs::context_tests::context_includes_cache_and_output_but_not_reasoning` | Context-meter arithmetic; HTTP fakes send zero cache. |
| R | `crates/harness/src/opencode/tests.rs::native_fork_adapter_remaps_regenerated_ids_and_rejects_leakage` | Fork id remap + delete of an unverified fork over the HTTP fake. |
| R | `crates/harness/src/opencode/tests.rs::native_fork_progress_distinguishes_preflight_acknowledgement_and_readback_timeout` | Fork progress receipts (preflight vs ack vs readback timeout). |
| R | `crates/harness/src/opencode/tests.rs::fork_cutoff_includes_checkpoint_by_excluding_the_next_message` | Fork boundary planner table. |
| R | `crates/harness/src/opencode/tests.rs::fork_readback_ignores_regenerated_ids_but_preserves_semantic_nested_fields` | Semantic readback comparison. |
| R | `crates/harness/src/opencode/tests.rs::compact_posts_an_object_body_on_the_v2_route` | Only owner of the v2 compaction route/body wire shape. |
| R | `crates/harness/src/opencode/tests.rs::permission_modes_compile_to_session_scoped_rules` | Security ruleset owner. Uses the `#[cfg(test)] opencode_permission_rules` wrapper (rules_json is private to permissions.rs); seam retained, see Follow-ups. |
| R | `crates/harness/src/opencode/tests.rs::session_allowance_is_a_session_rule_and_replies_use_decision` | Reply-body decision table (never `always`). |
| R | `crates/harness/src/opencode/tests.rs::completed_stream_snapshots_ignore_buffered_deltas` | Reconnect regression: buffered deltas after a completed snapshot. |
| R | `crates/harness/src/opencode/tests.rs::empty_sse_attempt_does_not_consume_initial_connection` | SSE admission regression. |
| R | `crates/harness/src/opencode/tests.rs::empty_sse_attempts_exhaust_reconnect_budget` | Reconnect budget. |
| R | `crates/harness/src/opencode/tests.rs::reconnect_reconciles_pending_request_ownership_and_settlement` | Watchdog exemption reconciliation. |
| R | `crates/harness/src/opencode/tests.rs::reconnect_recovers_parked_child_completion_without_crossing_owners` | History recovery of parked child completion. |
| R | `crates/harness/src/opencode/tests.rs::queued_turn_ignores_previous_turn_duplicate_terminal` | Duplicate terminal regression. |
| R | `crates/harness/src/opencode/tests.rs::v2_wire_streams_text_and_settles_on_execution_success` | Per-step Usage (10,2) is unique; text path overlaps `tests/opencode.rs::thinking_streams_...`. |
| R | `crates/harness/src/opencode/tests.rs::v2_wire_tool_frames_open_and_resolve_chips` | Tool envelope over v2 frames. |
| D | `crates/harness/src/opencode/tests.rs::v2_execution_failure_and_interrupt_settle_the_turn` | Interrupt half duplicated `tests/opencode.rs::interrupt_aborts_and_settles_interrupted` (same frames; also asserts the bodiless interrupt POST); failure half owned by `v2_frames_normalize_to_v1_payloads` (empty message -> error type) plus `tests/opencode.rs::execution_failure_with_no_content_settles_errored` (failure -> Done error). |
| R | `crates/harness/src/opencode/tests.rs::v2_terminal_failures_emit_one_persistent_error` | S9Q failure ordering: one persistent error on Done. |
| R | `crates/harness/src/opencode/tests.rs::v2_recovered_step_failure_is_a_transient_warning` | Keeper for recovered step failures (warning, no error, text, foreign failure ignored). |
| R | `crates/harness/src/opencode/tests.rs::catalog_decodes_fragmented_http_without_retaining_unused_fields` | Fragmented HTTP decode. |
| R | `crates/harness/src/opencode/tests.rs::cancelled_catalog_decode_releases_a_stalled_http_body` | Cancellation releases the body reader. |
| R | `crates/harness/src/opencode/tests.rs::provider_discovery_ignores_metadata_and_accepts_null_optional_fields` | Catalog parser tolerance. |
| R | `crates/harness/src/opencode/tests.rs::models_map_provider_catalog_with_variant_ladders` | `connected` filter and variant ladders (v0.2.21 field report). |
| R | `crates/harness/src/opencode/tests.rs::missing_connected_list_falls_back_to_the_full_catalog` | Catalog fallback rule. |
| R | `crates/harness/src/opencode/tests.rs::variants_only_ride_models_that_advertise_them` | Variant picker table; XHigh clamp row also in `tests/opencode.rs::model_and_advertised_variant_ride_the_session`, other rows unique. |
| R | `crates/harness/src/opencode/tests.rs::reasoning_parts_stream_as_reasoning_deltas` | Snapshot-suffix row unique. |
| R | `crates/harness/src/opencode/tests.rs::reasoning_ahead_of_its_message_role_is_held_and_replayed` | Held-part replay ordering. |
| R | `crates/harness/src/opencode/tests.rs::main_feed_user_text_is_the_prompt_echo_and_never_renders` | Prompt echo vs child user message. |
| R | `crates/harness/src/opencode/tests.rs::tool_parts_open_and_resolve_once` | Replayed terminal frame never resolves twice. |
| R | `crates/harness/src/opencode/tests.rs::task_spawn_registers_child_by_metadata_and_completion_settles` | Child binding by tool metadata. |
| R | `crates/harness/src/opencode/tests.rs::child_binding_falls_back_to_title_match` | Title/FIFO binding and foreign-parent rejection. |
| R | `crates/harness/src/opencode/tests.rs::tool_names_type_the_common_calls` | Tool-typing table (read->ReadFile only is in integration). |
| R | `crates/harness/src/opencode/tests.rs::commands_map_from_wire` | Nameless-drop and missing-description rows unique. |
| D | `crates/harness/src/opencode/tests.rs::stall_env_and_startup_env_parse` | Junk: asserts `stall_bound()`/`startup_timeout()` equal their own default constants with the env unset (no parse exercised; fails if the runner sets the env). Name promises parsing it never does. |
| R | `crates/harness/src/opencode/tests.rs::directory_header_percent_encodes` | Directory header encoding (fakes use `/tmp`). |
| R | `crates/harness/src/opencode/tests.rs::v2_frames_normalize_to_v1_payloads` | v2->v1 codec table owner. |
| R | `crates/harness/src/opencode/tests.rs::v2_model_list_folds_into_provider_catalog` | v2 model list codec owner. |
| R | `crates/harness/src/opencode/tests.rs::prompt_body_v2_carries_text_and_files_only` | File uri/name and no model/parts rows unique. |
| R | `crates/harness/src/opencode/tests.rs::permissions_stay_session_scoped_and_never_persist_grants` | Ownership of permission replies (foreign/ownerless never answered). |
| R | `crates/harness/src/opencode/tests.rs::permissions_without_auto_approve_require_an_explicit_answer` | Keeper; now a table that also carries the task-allowance row (Deny->reject, Allow once->once, Allow for task->reject). |
| C | `crates/harness/src/opencode/tests.rs::task_allowance_never_persists_through_v2` | Same mechanics as `permissions_without_auto_approve_require_an_explicit_answer`; carried as its third row. |
| R | `crates/harness/src/opencode/tests.rs::v2_tools_are_scoped_and_retired_and_real_failures_are_visible` | Tool-name scoping/retirement in the codec. |
| R | `crates/harness/src/opencode/tests.rs::v2_reasoning_deltas_and_failed_tools_reach_the_feed` | Failed tool -> error ToolResult end to end. |
| R | `crates/harness/src/opencode/tests.rs::prompt_error_and_interrupt_before_busy_still_settle` | Before-busy failure/interrupt settle (integration emits execution.started first). |
| D | `crates/harness/src/opencode/tests.rs::v2_model_selection_and_prompt_use_the_documented_bodies` | Owner: `tests/opencode.rs::model_and_advertised_variant_ride_the_session` asserts the exact model POST body with variant and the prompt text/files/delivery/`msg_` id (both suites run in CI). |
| R | `crates/harness/src/opencode/tests.rs::v2_pending_tool_overflow_fails_the_run_instead_of_growing_forever` | Memory bound. |
| R | `crates/harness/src/opencode/tests.rs::v2_external_interrupt_is_not_reported_as_success` | External interrupt without a local stop. |
| D | `crates/harness/src/opencode/tests.rs::v2_recovered_step_failure_does_not_poison_successful_execution` | Strict subset of `v2_recovered_step_failure_is_a_transient_warning` (same frames; that test also asserts no error, one warning and the foreign failure). |
| R | `crates/harness/src/process.rs::tests::unix_launch_retains_tokio_types_and_output` | Architecture contract: non-Windows builds keep Tokio's Command/Child types (type ascription) and output capture. |
| R | `crates/harness/src/process/windows/tests.rs::parked_child_helper` | Windows platform/security contract (Windows CI `--lib`). Helper entry point re-executed by the tests below (not a test). |
| R | `crates/harness/src/process/windows/tests.rs::unrelated_inheritable_handles_are_not_passed_to_agents` | Windows platform/security contract (Windows CI `--lib`). Security: handle allow-list. |
| R | `crates/harness/src/process/windows/tests.rs::creation_is_already_in_job_and_setup_failure_rolls_back` | Windows platform/security contract (Windows CI `--lib`). Job membership at creation + rollback. |
| R | `crates/harness/src/process/windows/tests.rs::ordinary_subprocess_cannot_keep_agent_pipes_open` | Windows platform/security contract (Windows CI `--lib`). Pipe isolation from unrelated subprocesses. |
| R | `crates/harness/src/process/windows/tests.rs::crash_interval_owner_helper` | Windows platform/security contract (Windows CI `--lib`). Helper entry point for the crash-interval test. |
| R | `crates/harness/src/process/windows/tests.rs::owner_death_immediately_after_creation_kills_suspended_child` | Windows platform/security contract (Windows CI `--lib`). Crash-interval kill guarantee. |
| R | `crates/harness/src/process/windows/tests.rs::batch_inner_helper` | Windows platform/security contract (Windows CI `--lib`). Helper entry point for the batch argv test. |
| R | `crates/harness/src/process/windows/tests.rs::batch_scripts_spawn_through_cmd_with_literal_arguments` | Windows platform/security contract (Windows CI `--lib`). Batch argv literalness at the Command layer (windows_native covers the harness layer). |
| R | `crates/harness/src/process/windows/tests.rs::batch_scripts_report_shim_exit_codes_and_kill_their_tree` | Windows platform/security contract (Windows CI `--lib`). Exit-code propagation and tree kill. |
| R | `crates/harness/src/process/windows/tests.rs::bare_batch_program_resolves_from_child_path` | Windows platform/security contract (Windows CI `--lib`). PATHEXT resolution from the child PATH. |
| R | `crates/harness/src/shell_env.rs::unix::tests::parses_path_between_markers` | Snapshot parser owner. |
| R | `crates/harness/src/shell_env.rs::unix::tests::ignores_marker_echoed_by_init` | Parser regression: rc noise echoing the marker. |
| D | `crates/harness/src/shell_env.rs::unix::tests::snapshots_path_from_fake_shell` | Owner: `shell_env_resolution::cli_on_login_shell_path_only_is_resolved` drives the same fake `$SHELL` (identical `-c` payload) through `login_shell_path()` and asserts the shell-shaped PATH prefix, then resolves `claude` through it. |
| R | `crates/harness/src/shell_env.rs::unix::tests::falls_back_when_interactive_attempt_hangs` | Interactive-hang fallback (timing bound is the contract). |
| R | `crates/harness/src/shell_env.rs::unix::tests::gives_up_on_a_shell_that_never_answers` | Give-up bound. |
| R | `crates/harness/tests/claude.rs::synthetic_luna_instructions_append_without_replacing_provider_prompt` | Fake exits unless `--append-system-prompt` carries the contract text and env is set. |
| R | `crates/harness/tests/claude.rs::happy_path_normalizes_events_and_tags_subagents` | Keeper: init dedupe, typed tools, subagent tagging, usage/Done through the driver. |
| R | `crates/harness/tests/claude.rs::eager_done_forwards_wake_turn_as_second_done` | Keeper: eager result + wake turn ordering. |
| R | `crates/harness/tests/claude.rs::ask_user_question_round_trips_through_the_control_channel` | AskUserQuestion bridge, no harness-emitted input lifecycle. |
| R | `crates/harness/tests/claude.rs::steering_lines_are_written_to_stdin_mid_run` | Legacy follow-up steer boundary. |
| R | `crates/harness/tests/claude.rs::explicit_steer_does_not_claim_incorporation_from_a_text_echo` | Negative: text echo is not a receipt (pre-2.1.286). |
| R | `crates/harness/tests/claude.rs::explicit_claude_write_ack_without_echo_is_not_incorporated` | Negative: write ack alone is not a receipt (pre-2.1.286). |
| R | `crates/harness/tests/claude.rs::interrupt_escalates_to_sigterm_and_ends_with_interrupted_done` | Signal escalation. |
| R | `crates/harness/tests/claude.rs::error_codes_map_to_readable_messages` | Keeper for assistant error codes, rejected rate limits and empty-`errors` subtype wording. |
| R | `crates/harness/tests/claude.rs::missing_binary_is_not_installed` | NotInstalled mapping. |
| R | `crates/harness/tests/claude.rs::captured_live_background_subagent_frames_replay_correctly` | Recorded real 2.1.228 capture replayed through the driver (retention: real-provider transcript). |
| R | `crates/harness/tests/claude.rs::live_real_cli_single_turn` | `#[ignore]` live smoke against the real CLI (lead QA). |
| R | `crates/harness/tests/claude.rs::commands_come_from_the_initialize_control_request` | Command discovery + cache. |
| R | `crates/harness/tests/claude.rs::models_come_from_the_initialize_control_request` | Live model catalog overlay. |
| R | `crates/harness/tests/claude.rs::live_models_discovery` | `#[ignore]` live read-only smoke. |
| R | `crates/harness/tests/claude.rs::live_commands_discovery` | `#[ignore]` live read-only smoke. |
| R | `crates/harness/tests/claude.rs::title_run_disables_tools_and_denies_unexpected_permissions` | Title-run argv and deny path (fake exits otherwise). |
| R | `crates/harness/tests/claude.rs::cc01_every_unsupported_inbound_control_gets_one_explicit_error` | Compat-ledger-driven control refusals + noise. |
| R | `crates/harness/tests/claude.rs::cc01_withdrawn_approval_is_never_answered_late` | control_cancel_request withdrawal. |
| R | `crates/harness/tests/claude.rs::cc01_explicit_steer_settles_only_on_its_exact_lifecycle_receipt` | msg_lifecycle receipts. |
| R | `crates/harness/tests/claude.rs::cc01_refused_steer_is_rejected_not_accepted` | Refused lifecycle. |
| R | `crates/harness/tests/claude.rs::cc01_silent_lifecycle_falls_back_to_the_write_acknowledgement` | Silent-lifecycle fallback. |
| R | `crates/harness/tests/claude.rs::cc01_stop_rejects_an_unreceipted_steer_the_cli_cancelled` | cancel_queued receipt rejects the steer. |
| R | `crates/harness/tests/claude.rs::cc01_stop_sends_a_unique_cancel_queued_interrupt_and_drains_before_signals` | Keeper: interrupt request shape, drain before signals; also owns diagnostic-only result -> no error text. |
| R | `crates/harness/tests/claude.rs::cc01_auth_and_limit_variants_end_with_readable_errors` | Auth/limit wording and unknown subtype. |
| R | `crates/harness/tests/claude.rs::e24_foreign_results_never_complete_the_pending_human_turn` | E24 ownership. |
| R | `crates/harness/tests/claude.rs::e24_follow_up_queued_behind_a_wake_turn_settles_only_its_own_receipt` | E24 B2. |
| R | `crates/harness/tests/claude.rs::e24_older_runtime_keeps_legacy_completion` | E24 B4 legacy. |
| R | `crates/harness/tests/claude.rs::e24_follow_up_folded_into_a_wake_turn_settles_without_waiting` | E24 B6. |
| R | `crates/harness/tests/claude.rs::e24_batched_follow_ups_settle_together` | E24 B5. |
| R | `crates/harness/tests/claude.rs::cc02_lifecycle_unknown_and_tool_bodies_are_decoded_and_retained` | Keeper for event decoding, bounded/redacted retention, invalid frames. |
| R | `crates/harness/tests/claude.rs::native_nested_agents_keep_ownership_model_usage_and_completion` | Keeper: recorded-shape 2.1.286 native agent graph through the driver. |
| R | `crates/harness/tests/claude.rs::native_child_stop_is_authority_checked_and_settles_from_task_events` | stop_task authority and settlement. |
| R | `crates/harness/tests/claude_availability.rs::availability_child` | Helper entry point re-executed with a controlled env (not a test). |
| R | `crates/harness/tests/claude_availability.rs::claude_availability_honors_override_without_path_cli` | Keeper: override makes Claude installed; its second probe also covers the no-CLI case. |
| D | `crates/harness/tests/claude_availability.rs::claude_availability_reports_not_installed_without_any_cli` | Exact duplicate of the second probe in `claude_availability_honors_override_without_path_cli` (`probe(dir, None, false)` guarded by the same `system_wide_claude_present()` check). |
| R | `crates/harness/tests/claude_cc06.rs::explicit_steer_settles_only_on_its_own_native_replay` | A1/A2/A4/A9/A10 replay receipts. |
| R | `crates/harness/tests/claude_cc06.rs::foreign_result_never_completes_the_outstanding_human_turn` | A5/A6/D1/D2/D5 ownership + accounting. |
| R | `crates/harness/tests/claude_cc06.rs::launch_flags_follow_discovery_and_readback_reports_the_applied_runtime` | B1-B4 ladder `--effort` + readback. |
| R | `crates/harness/tests/claude_cc06.rs::staged_live_switch_applies_and_reads_back_before_the_prompt` | C1/C3 live switch ordering. |
| R | `crates/harness/tests/claude_cc06.rs::failed_live_switch_never_writes_the_prompt` | C2/B3 failed switch. |
| R | `crates/harness/tests/claude_coordination.rs::native_workflow_graph_survives_the_parked_wake_shape` | W-1..W-11 workflow graph. |
| R | `crates/harness/tests/claude_coordination.rs::launched_claude_child_drops_inherited_session_identity` | C-3 env scrub. |
| R | `crates/harness/tests/claude_coordination.rs::resume_held_by_an_external_background_session_is_refused` | B-4 writer guard. |
| R | `crates/harness/tests/claude_coordination.rs::external_sessions_and_resume_use_the_configured_provider_home` | Provider home governs readback and guard. |
| R | `crates/harness/tests/claude_coordination.rs::resume_checks_writers_beyond_the_external_session_display_limit` | Guard beyond display limit. |
| R | `crates/harness/tests/claude_coordination.rs::resume_ignores_luna_owned_stopped_and_unrelated_rows` | B-4/B-5 non-blocking rows. |
| R | `crates/harness/tests/claude_coordination.rs::resume_proceeds_when_the_session_listing_is_unavailable` | Fail-open listing. |
| R | `crates/harness/tests/claude_coordination.rs::external_session_listing_maps_real_rows_and_explicit_unavailability` | Real `claude agents --json` row mapping. |
| R | `crates/harness/tests/claude_coordination.rs::live_workflow_capture_replays_into_a_settled_graph` | `#[ignore]` live capture replay. |
| R | `crates/harness/tests/claude_coordination.rs::shell_lifecycle_is_observed_without_subagent_reclassification` | Keeper for shell task lifecycle without subagent tagging. |
| R | `crates/harness/tests/claude_effective_config.rs::fake_peer_reproduces_recorded_2_1_286_matrix` | Conformance of the fake against the recorded real-CLI matrix; pins ASK_AUTO_TOOL_GRANT. |
| R | `crates/harness/tests/claude_effective_config.rs::discovery_and_launch_agree_for_every_mode` | G-1..G-12 discovery == launch per mode. |
| R | `crates/harness/tests/claude_effective_config.rs::discovery_cache_tracks_project_policy_account_and_config_changes` | Discovery cache keys. |
| R | `crates/harness/tests/claude_effective_config.rs::failed_discovery_is_not_cached_and_legacy_call_answers` | Failed probe not cached. |
| R | `crates/harness/tests/claude_effective_config.rs::title_run_keeps_tools_disabled` | G-13 exactly one `--tools ""` (claude::title_run cannot see a duplicate flag). |
| R | `crates/harness/tests/claude_effective_config.rs::live_e23_effective_configuration` | `#[ignore]` live E23 acceptance. |
| R | `crates/harness/tests/claude_extensions.rs::hook_lifecycle_becomes_root_hook_chips_with_truthful_outcomes` | Hook chips/outcomes (C2-C14). |
| R | `crates/harness/tests/claude_extensions.rs::hooks_after_a_root_result_wait_for_the_next_root_activity` | Idle hook ordering. |
| R | `crates/harness/tests/claude_extensions.rs::hook_seen_before_a_preinit_failure_still_precedes_done` | Pre-init hook ordering. |
| R | `crates/harness/tests/claude_extensions.rs::title_runs_do_not_request_hook_events` | Title-run argv. |
| R | `crates/harness/tests/claude_extensions.rs::effective_view_follows_launch_policy_scope_and_precedence` | Effective settings view. |
| R | `crates/harness/tests/claude_extensions.rs::managed_policy_and_disable_all_hooks_bound_hook_effect` | Managed policy precedence. |
| F | `crates/harness/tests/claude_extensions.rs::typed_skill_dispatches_native_invocation_after_images` | Contract kept (readback argv, settings layer, image-then-command blocks). Removed the B9 row: it called `claude::extensions::skill_invocation_text`, a `pub fn` with no production caller (production builds the line in `claude/input.rs::plan_skill_dispatch`), so it proved dead code, not dispatch. Dead fn deleted. See Possible product defects. |
| R | `crates/harness/tests/claude_extensions.rs::typed_skill_outside_the_effective_catalog_is_rejected_before_any_user_message` | Rejection before any user line. |
| R | `crates/harness/tests/claude_extensions.rs::extension_view_reloads_when_settings_files_change` | Fingerprint reload. |
| R | `crates/harness/tests/claude_goals.rs::goal_start_sends_claude_goal_command_and_reads_the_verdict` | G1/G2/G4/G5. |
| R | `crates/harness/tests/claude_goals.rs::impossible_verdict_clears_the_goal_with_claudes_reason` | G6. |
| R | `crates/harness/tests/claude_goals.rs::unrecoverable_error_clear_is_reported` | G7. |
| R | `crates/harness/tests/claude_goals.rs::gate_refusal_is_never_active_and_keeps_claudes_text` | G3. |
| R | `crates/harness/tests/claude_goals.rs::unresolved_loop_stays_active_with_the_latest_check` | G8. |
| R | `crates/harness/tests/claude_goals.rs::late_verdict_is_read_within_the_bounded_wait` | G5/G10 bounded wait. |
| R | `crates/harness/tests/claude_goals.rs::warm_goal_start_uses_the_live_session` | G1 warm. |
| R | `crates/harness/tests/claude_goals.rs::native_goal_read_and_clear_use_claudes_own_record_and_command` | G11/G12. |
| R | `crates/harness/tests/claude_goals.rs::resumed_session_seeds_claudes_restored_goal` | G17. |
| R | `crates/harness/tests/claude_goals.rs::report_findings_become_shared_findings_with_provenance` | R1/R2/R4/R8/R9. |
| R | `crates/harness/tests/claude_goals.rs::empty_and_failed_reports_are_truthful` | R3. |
| R | `crates/harness/tests/claude_goals.rs::native_schedules_follow_claudes_own_lifecycle` | S1/S2/S3/S5. |
| R | `crates/harness/tests/claude_history.rs::history_controls_disable_hooks_and_drop_inherited_session_identity` | History controls isolation. |
| R | `crates/harness/tests/claude_history.rs::history_rewinds_refuse_an_external_session_writer_before_mutation` | External writer guard. |
| R | `crates/harness/tests/claude_history.rs::completed_turns_report_native_prompt_uuids_as_checkpoints` | A1-A6 checkpoints. |
| R | `crates/harness/tests/claude_history.rs::batched_prompts_checkpoint_the_first_consumed_uuid` | A3 batches. |
| R | `crates/harness/tests/claude_history.rs::forks_copy_the_native_prefix_into_an_independent_session` | B1-B6 forks. |
| R | `crates/harness/tests/claude_history.rs::same_session_rewind_truncates_or_refuses_natively` | C1-C5/C8 rewinds. |
| R | `crates/harness/tests/claude_history.rs::file_checkpoints_preview_then_restore_claude_edits` | File checkpoint restore. |
| R | `crates/harness/tests/claude_history.rs::real_cli_rewound_transcript_reads_back_the_retained_prefix` | Recorded real-CLI rewound transcript. |
| R | `crates/harness/tests/claude_inputs.rs::first_turn_sends_validated_bytes_in_order_and_image_only_without_text` | Image bytes/order/media type; text-only string content. |
| R | `crates/harness/tests/claude_inputs.rs::invalid_inputs_reject_before_any_process_or_stdin_line` | Invalid inputs rejected before spawn. |
| R | `crates/harness/tests/claude_inputs.rs::explicit_steer_carries_images_and_invalid_steers_are_rejected_unwritten` | Steer images. |
| R | `crates/harness/tests/claude_inputs.rs::resumed_process_receives_images_in_its_first_line` | Resume first line. |
| R | `crates/harness/tests/claude_inputs.rs::typed_skill_with_images_and_prose_expands_from_the_last_text_block` | Keeper for skill dispatch and Ultrathink prefix bytes. |
| R | `crates/harness/tests/claude_luna_tools.rs::claude_luna_tools_fence_matrix_over_shared_mcp_endpoint` | LUNA MCP fence matrix. |
| R | `crates/harness/tests/claude_luna_tools.rs::claude_contract_with_tools_but_no_executor_is_refused` | F6/F15 refusal. |
| R | `crates/harness/tests/claude_mcp.rs::status_reads_back_per_launch_mode_and_redacts_configuration` | MCP status readback + redaction. |
| R | `crates/harness/tests/claude_mcp.rs::pending_server_stays_pending_at_the_deadline` | Pending at deadline. |
| R | `crates/harness/tests/claude_mcp.rs::toggle_is_provider_owned_persistent_and_per_project` | Toggle persistence. |
| R | `crates/harness/tests/claude_mcp.rs::toggle_of_unknown_server_is_rejected_before_any_write` | Unknown server rejection. |
| R | `crates/harness/tests/claude_mcp.rs::unconfirmed_toggle_is_indeterminate` | Indeterminate outcome. |
| R | `crates/harness/tests/claude_mcp.rs::app_resource_read_is_ui_only_bounded_and_never_rendered` | App resource read bounds. |
| R | `crates/harness/tests/claude_mcp.rs::session_evidence_comes_only_from_the_runs_own_init_frame` | Session MCP evidence from own init. |
| R | `crates/harness/tests/claude_peers.rs::luna_child_report_reaches_claude_as_a_host_declared_peer_and_its_echo_stays_silent` | Peer envelope (no Ultrathink prefix). |
| R | `crates/harness/tests/claude_peers.rs::native_socket_peer_arrival_is_an_attributed_observation_not_a_user_turn` | Socket peer arrival. |
| R | `crates/harness/tests/claude_peers.rs::held_child_report_ends_the_turn_visibly_instead_of_hanging` | Held report. |
| R | `crates/harness/tests/claude_peers.rs::socket_holds_are_reported_and_never_end_the_turn` | Socket holds. |
| R | `crates/harness/tests/claude_permissions.rs::approval_projects_native_context_and_exact_session_grant` | Approval card + exact session grant. |
| R | `crates/harness/tests/claude_permissions.rs::native_cancel_withdraws_without_reply_and_replays_are_ignored` | Native cancel + replay. |
| R | `crates/harness/tests/claude_permissions.rs::duplicate_live_request_renders_once_and_concurrent_answers_route_by_id` | Duplicate/concurrent routing. |
| R | `crates/harness/tests/claude_permissions.rs::subagent_requests_reach_the_user_and_mcp_context_is_kept` | Sub-agent requests. |
| R | `crates/harness/tests/claude_permissions.rs::full_mode_allows_once_but_never_plan_or_interactive_tools` | Full-mode rules. |
| R | `crates/harness/tests/claude_permissions.rs::ask_user_question_keeps_descriptions_and_dismissal_denies` | Keeper for AskUserQuestion answers (peer r13/r14). |
| R | `crates/harness/tests/claude_permissions.rs::mcp_elicitation_uses_the_shared_wizard_and_answers_once` | Elicitation. |
| R | `crates/harness/tests/claude_permissions.rs::unknown_controls_get_an_error_and_undeclared_dialogs_no_answer` | Undeclared dialog silence, id-less/malformed can_use_tool (distinct from cc01's ledger sweep). |
| R | `crates/harness/tests/claude_permissions.rs::stop_withdraws_pending_approval_without_a_late_grant` | Stop withdraws approval. |
| R | `crates/harness/tests/claude_permissions.rs::weaker_native_mode_fails_closed_and_stricter_mode_is_reported` | Mode mismatch handling. |
| R | `crates/harness/tests/claude_tool_results.rs::rich_results_keep_content_arguments_diffs_and_ownership` | Rich result envelopes. |
| R | `crates/harness/tests/claude_tool_results.rs::result_frame_structured_output_without_a_call_becomes_its_own_part` | Structured output part. |
| R | `crates/harness/tests/claude_tool_results.rs::live_capture_replay_projects_every_received_result` | `#[ignore]` live replay. |
| R | `crates/harness/tests/claude_tool_results.rs::saved_bash_artifact_is_complete_and_model_preview_is_preserved` | Saved bash artifact. |
| R | `crates/harness/tests/claude_tool_results.rs::saved_bash_artifact_refusals_preserve_the_native_receipt_and_settle` | Artifact refusals. |
| R | `crates/harness/tests/claude_tool_results.rs::saved_bash_output_call_replay_retains_loaded_bytes_after_artifact_removal` | Replay retains bytes. |
| R | `crates/harness/tests/codex.rs::synthetic_luna_mcp_tool_is_scoped_to_exact_thread_turn_and_call` | LUNA MCP tool scoping to thread/turn/call; resume path. |
| R | `crates/harness/tests/codex.rs::cancelled_luna_turn_rejects_late_tool_request_before_executor` | Late tool call after cancel never reaches the executor. |
| R | `crates/harness/tests/codex.rs::declared_luna_tools_without_engine_handler_fail_before_provider_start` | Refusal before provider start. |
| R | `crates/harness/tests/codex.rs::ordinary_threads_are_native_visible_and_strict_resume_never_rebinds` | Durable native thread + strict resume never rebinds. |
| R | `crates/harness/tests/codex.rs::native_metadata_client_uses_supported_methods_only` | Native metadata client methods. |
| R | `crates/harness/tests/codex.rs::native_fork_reports_acknowledged_id_before_mismatch_or_timeout_and_not_on_preflight` | Fork progress receipts over `codex-fork-peer.py`. |
| R | `crates/harness/tests/codex.rs::native_fork_uses_two_processes_and_verifies_persisted_history_without_inventory_scans` | Fork mutation/readback process split. |
| R | `crates/harness/tests/codex.rs::native_compaction_resumes_bound_thread_before_start_and_waits_for_matching_completion` | Native compaction ordering and matching completion. |
| R | `crates/harness/tests/codex.rs::native_compaction_load_owner_and_binding_failures_never_start_or_replay_compaction` | Pre-dispatch compaction refusals. |
| R | `crates/harness/tests/codex.rs::ordinary_start_and_resume_inherit_the_selected_codex_home_config` | CODEX_HOME config/cwd/file-auth on start and resume. |
| R | `crates/harness/tests/codex.rs::automatic_context_compaction_is_correlated_and_never_completes_the_turn` | Keeper for automatic compaction correlation (child/stale ignored) and ContextUsage. |
| R | `crates/harness/tests/codex.rs::stale_turn_and_duplicate_full_compaction_lifecycles_are_ignored` | Keeper: stale-turn and duplicate compaction lifecycles. |
| R | `crates/harness/tests/codex.rs::canonical_terminal_turn_resolves_pending_compaction_before_done` | Pending compaction -> Unknown before Done; process stays open. |
| R | `crates/harness/tests/codex.rs::interrupted_or_disconnected_automatic_compaction_becomes_unknown_once` | Keeper: exact [Running, Unknown] on interrupt/disconnect. |
| R | `crates/harness/tests/codex.rs::live_l06_native_visibility_rename_archive_and_reopen` | `#[ignore]` live QA (real thread). |
| R | `crates/harness/tests/codex.rs::reasoning_preserves_summary_parts_and_item_boundaries_per_thread` | Summary-part boundaries per thread. |
| R | `crates/harness/tests/codex.rs::tool_history_preserves_read_search_patch_web_and_image_events` | Tool history typing and diffs. |
| R | `crates/harness/tests/codex.rs::happy_path_maps_deltas_items_usage_and_done` | Keeper: turn/thread params, delta spellings, item lifecycles, usage before Done. |
| R | `crates/harness/tests/codex.rs::steering_uses_turn_steer_with_expected_turn_id` | Follow-up steer via turn/steer with expectedTurnId. |
| R | `crates/harness/tests/codex.rs::explicit_steer_native_record_emits_one_correlated_boundary` | Explicit steer: one correlated boundary. |
| R | `crates/harness/tests/codex.rs::explicit_steer_ignores_stale_and_duplicate_echoes` | Stale/duplicate echoes ignored. |
| R | `crates/harness/tests/codex.rs::explicit_steer_uncorrelated_ack_stays_unknown_without_replay` | Uncorrelated ack -> Unknown, no replay. |
| R | `crates/harness/tests/codex.rs::explicit_steer_rejection_never_falls_back_to_turn_start` | Rejection never manufactures a turn. |
| R | `crates/harness/tests/codex.rs::explicit_steer_lost_response_is_unknown_and_never_replayed` | Lost response -> Unknown. |
| R | `crates/harness/tests/codex.rs::explicit_steer_ack_only_remains_pending_without_boundary` | Ack alone is not incorporation. |
| R | `crates/harness/tests/codex.rs::rejected_steer_falls_back_to_a_follow_up_turn` | Follow-up delivery fallback turn. |
| R | `crates/harness/tests/codex.rs::approvals_round_trip_as_input_requests` | Keeper for approval cards (header/options/command/path) and bridge ownership. |
| R | `crates/harness/tests/codex.rs::approval_no_answer_becomes_decline` | No answer -> decline. |
| R | `crates/harness/tests/codex.rs::computer_use_approval_and_provider_evidence_stay_on_exact_turn` | Keeper for computer-use lifecycle, payload privacy, stale/duplicate completions. |
| R | `crates/harness/tests/codex.rs::full_mode_permission_paths_never_park_a_user_approval` | Full mode never parks approvals. |
| R | `crates/harness/tests/codex.rs::computer_use_denial_has_no_computer_action` | Denial -> no computer action. |
| R | `crates/harness/tests/codex.rs::computer_use_without_luna_task_binding_remains_unavailable` | No task binding -> unavailable. |
| R | `crates/harness/tests/codex.rs::provider_permission_event_uses_exact_turn_and_one_request_grant` | Provider permission grant scope. |
| R | `crates/harness/tests/codex.rs::stale_t1_lifecycle_cannot_cancel_t2_approval_or_computer_call` | Old-turn lifecycle race (4 rows). |
| R | `crates/harness/tests/codex.rs::next_turn_response_owns_approval_before_started_notification` | Response-before-started ownership. |
| R | `crates/harness/tests/codex.rs::ended_t2_withholds_a_delayed_allow_once_answer` | Late answer after turn end withheld. |
| R | `crates/harness/tests/codex.rs::computer_use_disconnect_reports_unknown_without_replay` | Disconnect -> Unknown + Errored. |
| R | `crates/harness/tests/codex.rs::computer_use_interrupt_reports_unknown_without_provider_success` | Interrupt -> Unknown, never ProviderCompleted. |
| R | `crates/harness/tests/codex.rs::interrupt_sends_turn_interrupt_and_maps_aborted` | turn/interrupt mapping. |
| R | `crates/harness/tests/codex.rs::unresponsive_child_is_reaped_with_interrupted_done` | Signal escalation reaps a wedged server. |
| R | `crates/harness/tests/codex.rs::turn_failed_maps_to_errored_done` | Keeper for turn error extraction (`boom`). |
| R | `crates/harness/tests/codex.rs::resume_falls_back_to_fresh_thread` | Non-strict resume fallback. |
| R | `crates/harness/tests/codex.rs::resume_reuses_the_existing_thread` | Resume reuse. |
| R | `crates/harness/tests/codex.rs::missing_binary_is_not_installed` | NotInstalled mapping. |
| R | `crates/harness/tests/codex.rs::models_discovers_visible_catalog_with_pagination` | model/list pagination, tiers, identity surface. |
| R | `crates/harness/tests/codex.rs::resumed_parent_recovers_v1_and_v2_child_owners_without_replaying_chips` | Resumed child ownership (v1/v2/legacy). |
| R | `crates/harness/tests/codex.rs::v2_lifecycle_reuses_chips_and_reopens_the_same_child_for_followup` | v2 lifecycle fixture. |
| R | `crates/harness/tests/codex.rs::v1_spawns_bind_children_and_controls_do_not_create_agents` | v1 fixture spawns vs controls. |
| R | `crates/harness/tests/codex.rs::child_identity_survives_early_output_and_later_activity_ids` | Keeper for child binding/buffering (`child-identity.jsonl`). |
| R | `crates/harness/tests/codex.rs::child_thread_routing_tags_and_never_settles_parent` | Keeper for child routing and the named spawn chip. |
| R | `crates/harness/tests/codex.rs::live_subagent_spawn_and_followup_keep_one_transcript` | `#[ignore]` live multi-agent smoke. |
| R | `crates/harness/tests/codex.rs::live_real_app_server_single_turn` | `#[ignore]` live smoke. |
| R | `crates/harness/tests/codex.rs::live_real_app_server_tool_and_steer` | `#[ignore]` live smoke. |
| R | `crates/harness/tests/codex.rs::commands_come_from_skills_list` | skills/list -> picker commands + cache. |
| R | `crates/harness/tests/codex.rs::skill_observations_keep_duplicate_names_in_separate_cwd_groups` | Retained with its code: `discover_skill_observations` is `#[doc(hidden)] pub` with no production caller (928aa665). See Follow-ups. |
| R | `crates/harness/tests/codex.rs::native_skill_catalog_uses_isolated_home_and_exact_workspace` | Native skill catalog. |
| R | `crates/harness/tests/codex.rs::native_mcp_config_keeps_commands_and_tokens_out_of_inventory` | MCP config redaction. |
| R | `crates/harness/tests/codex.rs::native_mcp_config_uses_each_selected_project_cwd` | Per-project config/read. |
| R | `crates/harness/tests/codex.rs::skill_observation_parser_ignores_unknown_and_malformed_payloads` | Retained with its code: `observe_skill_discovery` (`#[doc(hidden)] pub`) has no production caller. See Follow-ups. |
| R | `crates/harness/tests/codex.rs::skill_observation_parser_bounds_groups_names_and_total_retention` | As above (bounds of the uncalled observation parser). |
| R | `crates/harness/tests/codex.rs::live_commands_discovery` | `#[ignore]` live read-only smoke. |
| R | `crates/harness/tests/codex.rs::title_run_preserves_read_only_and_replaces_coding_instructions` | Title-run params. |
| R | `crates/harness/tests/codex.rs::image_generation_fake_lifecycle_reaches_done_without_inline_payload` | Image generation lifecycle without inline payload. |
| R | `crates/harness/tests/codex.rs::real_image_generation_smoke` | `#[ignore]` live smoke. |
| R | `crates/harness/tests/codex.rs::native_steering_reconciles_lost_ack_and_terminal_readback` | Steer reconciliation. |
| R | `crates/harness/tests/codex.rs::native_steering_records_multiple_ids_once_in_order` | Multiple steer ids once, in order. |
| R | `crates/harness/tests/codex.rs::native_terminal_interruption_is_truthful_without_local_stop` | External interruption. |
| R | `crates/harness/tests/codex.rs::native_steering_resume_readback_does_not_reopen_old_turn` | Resume reconciliation. |
| R | `crates/harness/tests/codex.rs::native_terminal_status_wins_the_local_interrupt_race` | Terminal status vs local interrupt race. |
| R | `crates/harness/tests/codex.rs::generic_mcp_elicitation_roundtrips_typed_fields_and_actions` | MCP elicitation table (19 scenarios). |
| R | `crates/harness/tests/codex.rs::goal_start_adopts_the_turn_codex_starts_itself` | Goal start adopts server turn. |
| R | `crates/harness/tests/codex.rs::goal_start_on_a_resumed_thread_clears_before_loading_it` | Goal clear before resume. |
| R | `crates/harness/tests/codex.rs::goal_start_without_a_server_turn_settles_with_an_error` | No server turn -> error. |
| R | `crates/harness/tests/codex.rs::rejected_goal_start_reports_the_provider_message` | Goal rejection wording. |
| R | `crates/harness/tests/codex.rs::warm_goal_starts_on_the_loaded_runtime` | Warm goal. |
| R | `crates/harness/tests/codex.rs::subagent_approval_and_null_turn_mcp_prompt_reach_the_user` | Sub-agent approval/MCP context. |
| R | `crates/harness/tests/codex.rs::subagent_approvals_close_on_resolution_and_child_turn_end` | Parked answers close. |
| R | `crates/harness/tests/codex.rs::mcp_tool_call_approval_renders_tool_context_and_sends_persist` | MCP tool approval table. |
| R | `crates/harness/tests/codex.rs::retrying_errors_warnings_and_mcp_progress_are_transient` | Transient notices. |
| R | `crates/harness/tests/codex.rs::request_user_input_keeps_descriptions_free_text_and_secret_flags` | requestUserInput flags. |
| R | `crates/harness/tests/codex.rs::initial_turn_start_carries_the_engine_user_message_id` | Client user-message id on turn/start. |
| R | `crates/harness/tests/codex.rs::native_rewind_accepts_terminal_history_and_refuses_live_or_ambiguous_turns` | Rewind acceptance table. |
| R | `crates/harness/tests/codex_availability.rs::availability_child` | Helper entry point re-executed with a controlled env (not a test). |
| R | `crates/harness/tests/codex_availability.rs::codex_availability_honors_native_override` | Override + shim. |
| R | `crates/harness/tests/codex_availability.rs::codex_availability_finds_npm_cmd_shim` | npm shim on PATH and APPDATA backfill. |
| R | `crates/harness/tests/composer_catalog.rs::installed_catalog_preserves_exact_identities_and_filters_policy` | Composer catalog identities/policy. |
| R | `crates/harness/tests/mcp_startup_isolation.rs::metadata_and_qa_launches_keep_mcp_boundaries` | MCP isolation (single-test binary). |
| R | `crates/harness/tests/native_inputs.rs::image_only_start_and_attached_steer_preserve_native_identity` | Image-only start + steer identity. |
| R | `crates/harness/tests/native_inputs.rs::invalid_image_and_selection_fail_before_spawning_provider` | Pre-spawn validation. |
| R | `crates/harness/tests/native_inputs.rs::unavailable_native_selection_rejects_before_turn_start` | Catalog rejection before turn/start. |
| R | `crates/harness/tests/native_inputs.rs::correlated_warm_steer_fallback_keeps_the_complete_input` | Steer fallback keeps input. |
| R | `crates/harness/tests/native_inputs.rs::host_native_tiff_and_unsupported_svg_follow_explicit_policy` | Image format policy. |
| R | `crates/harness/tests/opencode.rs::thinking_streams_and_the_turn_settles_only_on_idle` | Premature-done regression; reasoning dedupe. |
| R | `crates/harness/tests/opencode.rs::foreign_session_idle_never_settles_our_turn` | Foreign idle regression. |
| R | `crates/harness/tests/opencode.rs::model_and_advertised_variant_ride_the_session` | Keeper for model POST body (variant clamp) and prompt body. |
| R | `crates/harness/tests/opencode.rs::steer_queues_mid_turn_and_delivers_at_idle` | Steer queued to idle (uses a 100 ms sleep for mailbox landing: no observable event exists; noted). |
| R | `crates/harness/tests/opencode.rs::interrupt_aborts_and_settles_interrupted` | Keeper for interrupt (bodiless POST, Interrupted). |
| R | `crates/harness/tests/opencode.rs::execution_failure_with_no_content_settles_errored` | Keeper for one persistent failure error. |
| R | `crates/harness/tests/opencode.rs::resume_reuses_the_durable_session` | Resume + permission PATCH. |
| R | `crates/harness/tests/opencode.rs::failed_resume_never_silently_starts_a_fresh_session` | Failed resume never rebinds. |
| R | `crates/harness/tests/opencode.rs::slash_command_routes_through_the_command_endpoint` | Command endpoint routing. |
| R | `crates/harness/tests/opencode.rs::first_prompt_waits_for_the_live_event_subscription` | Subscription-before-prompt gate. |
| R | `crates/harness/tests/opencode.rs::models_discover_from_the_model_list` | Enabled filter + commands priming. |
| R | `crates/harness/tests/opencode.rs::models_refresh_large_catalogs_and_never_return_a_stale_one` | Stale catalog regression. |
| R | `crates/harness/tests/opencode.rs::session_create_failure_is_not_retried` | OCF-74 no blind retry. |
| R | `crates/harness/tests/permission_detail.rs::permission_detail_wire_choices_and_scope` | 12-row wire choice table. |
| R | `crates/harness/tests/permission_detail.rs::permission_detail_remote_resolution_closes_parked_answer` | Remote resolution closes the parked answer. |
| R | `crates/harness/tests/permission_modes.rs::codex_start_resume_and_turn_keep_the_same_explicit_permission_policy` | Keeper for Codex permission settings per mode. |
| R | `crates/harness/tests/provider_lifecycle_audit.rs::$name` | Macro `case!` table, 11 rows (Stop/drop during initialize, first turn/start, follow-up turn/start, steer, lost steer reply, Claude blocked steer write): reap + no replay. |
| R | `crates/harness/tests/provider_lifecycle_audit.rs::malformed_model_catalog_is_not_authoritative_empty` | Malformed catalog is not an authoritative empty list. |
| R | `crates/harness/tests/provider_lifecycle_audit.rs::cyclic_model_pagination_is_not_authoritative_partial` | Cyclic pagination. |
| R | `crates/harness/tests/shell_env_resolution.rs::cli_on_login_shell_path_only_is_resolved` | Keeper for login-shell PATH snapshot through a fake $SHELL. |
| R | `crates/harness/tests/windows_native.rs::managed_process_protocol_progresses_with_one_blocking_worker` | Windows platform/security contract (Windows CI). File repaired to compile (see `batch_overrides_launch_through_cmd`). |
| R | `crates/harness/tests/windows_native.rs::output_captures_both_streams_with_default_or_null_stdio` | Windows platform/security contract (Windows CI). File repaired to compile (see `batch_overrides_launch_through_cmd`). |
| R | `crates/harness/tests/windows_native.rs::native_launch_matches_tokio_argv_environment_cwd_and_path` | Windows platform/security contract (Windows CI). File repaired to compile (see `batch_overrides_launch_through_cmd`). |
| R | `crates/harness/tests/windows_native.rs::invalid_launch_inputs_fail_without_starting_a_child` | Windows platform/security contract (Windows CI). File repaired to compile (see `batch_overrides_launch_through_cmd`). |
| R | `crates/harness/tests/windows_native.rs::process_exit_drains_buffered_output_despite_inherited_descendant_pipes` | Windows platform/security contract (Windows CI). File repaired to compile (see `batch_overrides_launch_through_cmd`). |
| R | `crates/harness/tests/windows_native.rs::batch_override_helper` | Helper entry point for `batch_overrides_launch_through_cmd` (not a test). |
| F | `crates/harness/tests/windows_native.rs::batch_overrides_launch_through_cmd` | Contract kept. The file did not compile on Windows: `Value` was never imported and the `request()` RunRequest literal lacked `permission_mode`/`parent_permission_ceiling`/`environment_permission_ceiling`/`permission_policy` (added in cbeaaca3, 2026-09-18). Windows CI runs fail in ~3s without running tests, so this went unnoticed. Repaired both. |
| R | `crates/harness/tests/windows_native.rs::application_exit_without_destructors_kills_owned_processes` | Windows platform/security contract (Windows CI). File repaired to compile (see `batch_overrides_launch_through_cmd`). |
| R | `crates/harness/tests/windows_native.rs::batch_arguments_resist_shell_interpretation` | Windows platform/security contract (Windows CI). File repaired to compile (see `batch_overrides_launch_through_cmd`). |
| R | `crates/harness/tests/windows_native.rs::batch_executable_path_rejects_percent_expansion` | Windows platform/security contract (Windows CI). File repaired to compile (see `batch_overrides_launch_through_cmd`). |

## Layer plan

The redundant layer in this lane is inline normalizer/router unit tests that replay, frame for frame, scenarios the fake-CLI integration suites already drive through the real transport (stdin/stdout or HTTP/SSE). Each pure decoder keeps one table-driven owner for the rows no fixture reaches (spelling tolerance, bounds, negatives); rows a fixture already proves were retired.

Keeper per contract:

| Contract | Keeper |
| --- | --- |
| Claude init dedupe, eager result + wake turn | `tests/claude.rs::eager_done_forwards_wake_turn_as_second_done`, `::captured_live_background_subagent_frames_replay_correctly` |
| Claude subagent tagging, tagged text blocks, shell tasks never settle a spawn | `tests/claude.rs::happy_path_normalizes_events_and_tags_subagents`, `::native_nested_agents_keep_ownership_model_usage_and_completion`, `tests/claude_coordination.rs::shell_lifecycle_is_observed_without_subagent_reclassification` |
| Claude unknown/invalid frame retention | `tests/claude.rs::cc02_lifecycle_unknown_and_tool_bodies_are_decoded_and_retained` |
| Claude result error text | `src/claude/normalize.rs::tests::result_errors_drop_diagnostics_but_keep_real_text_and_subtype_wording` (rows) + `tests/claude.rs::error_codes_map_to_readable_messages` (empty `errors`) + `::cc01_stop_sends_…` (interrupted, diagnostic-only) |
| Claude Ultrathink prefix bytes | `tests/claude_inputs.rs::typed_skill_with_images_and_prose_expands_from_the_last_text_block` |
| Claude AskUserQuestion answers | `tests/claude_permissions.rs::ask_user_question_keeps_descriptions_and_dismissal_denies` (peer r13 verifies the reply bytes) |
| Claude `--effort` fallback heuristic | `src/claude/catalog.rs::tests::effort_maps_special_modes_and_clamps_xhigh_by_model_family` (ladder path: `tests/claude_cc06.rs`) |
| Codex permission settings per mode | `tests/permission_modes.rs::codex_start_resume_and_turn_keep_the_same_explicit_permission_policy` |
| Codex approval cards | `tests/codex.rs::approvals_round_trip_as_input_requests` + `src/codex/approvals.rs::tests::approval_card_joins_argv_and_paths_and_is_machine_scoped` |
| Codex computer-use lifecycle | `tests/codex.rs::computer_use_approval_and_provider_evidence_stay_on_exact_turn` (+ remaining inline rows for foreign-thread start, once-only unknown, node_repl, id reuse) |
| Codex automatic compaction | `tests/codex.rs::automatic_context_compaction_is_correlated_…`, `::stale_turn_and_duplicate_…`, `::interrupted_or_disconnected_…` |
| Codex child binding/buffering and spawn chip | `tests/codex.rs::child_identity_survives_early_output_and_later_activity_ids`, `::child_thread_routing_tags_and_never_settles_parent` |
| Codex turn error | `tests/codex.rs::turn_failed_maps_to_errored_done` |
| OpenCode interrupt, failure, model/prompt bodies | `tests/opencode.rs::interrupt_aborts_and_settles_interrupted`, `::execution_failure_with_no_content_settles_errored`, `::model_and_advertised_variant_ride_the_session` |
| OpenCode recovered step failure | `src/opencode/tests.rs::v2_recovered_step_failure_is_a_transient_warning` |
| OpenCode permission replies without auto-approve | `src/opencode/tests.rs::permissions_without_auto_approve_require_an_explicit_answer` (now a 3-row table) |
| Login-shell PATH snapshot | `tests/shell_env_resolution.rs::cli_on_login_shell_path_only_is_resolved` |
| Claude installed signal | `tests/claude_availability.rs::claude_availability_honors_override_without_path_cli` |

Retired: 20 declarations deleted (see D rows); no whole test file retired, so no `Cargo.toml` `[[test]]` or workflow target changes.

Assertions carried into keepers:

- the seven `supports_xhigh` model rows → `to_effort` table (`claude/catalog.rs`);
- tagged `input_json_delta` dropped → `stream_deltas_map_to_text_reasoning_and_heartbeats`;
- three Claude result-error tests → one three-row table;
- Codex approval argv-array join, multi-path join and machine scope → `codex/approvals.rs::tests` (calls the private builder; no seam);
- OpenCode "Allow for task" → `reject` → third row of `permissions_without_auto_approve_require_an_explicit_answer`.

Production seams removed:

- `#[cfg(test)] fn approval_question` (`src/codex/mod.rs`) and `#[cfg(test)] pub(super) fn legacy_question` (`src/codex/approvals.rs`);
- `pub fn claude::extensions::skill_invocation_text`: dead code whose only caller was a test row (production builds the line in `claude/input.rs::plan_skill_dispatch`);
- `claude::catalog::supports_xhigh` narrowed from `pub(crate)` to private.

Seams retained, with reason:

- `#[cfg(test)] pub(super) fn opencode::permissions::opencode_permission_rules`: wraps the private `rules_json`; removing it needs the ruleset test moved to the HTTP boundary (Follow-ups).
- `#[cfg(test)] pid` on `process/windows/escrow.rs::Escrow`: the Windows crash-interval security test needs the escrow pid.

F repairs: `tests/windows_native.rs` (did not compile on Windows) and `tests/claude_extensions.rs::typed_skill_dispatches_native_invocation_after_images` (row asserted dead code).

## Cross-lane requests

1. **CI workflows owner (`.github/workflows/ui-tests.yml`):** no workflow runs the unix harness integration suites, which are now the sole owners of the contracts listed above (inline duplicates were deleted, and those inline tests ran in Windows CI's `--lib`). Add to the Linux `session-sync-regressions` job: `cargo test --locked -p luna-harness --tests` (fake peers need `python3` and `/bin/sh`, both present on `ubuntu-24.04`). Existing filters still match: `--lib opencode::` and `--test opencode` targets are unchanged; `windows.yml` targets are unchanged.
2. **CI workflows owner (`.github/workflows/windows.yml`):** every recent `Windows tests` run on `main` fails in 3–4 s (for example run 37954353045), so its jobs never execute. `tests/windows_native.rs` had not compiled since cbeaaca3 (2026-09-18) without anyone noticing. Investigate the runner/billing failure.
3. **Repository root `.gitignore` owner:** add `__pycache__/`. A `.pyc` from `fake-claude-goals.py` had been committed under `crates/harness/tests/fixtures/__pycache__/` (deleted here).

No shared fixture or `tests/support` file was changed, so no other lane needs to coordinate fixture edits.

## Possible product defects

1. **Typed Claude skill prefix may double.** The deleted helper `skill_invocation_text` promised that "an existing identical prefix is kept" (CC-12 B9), but nothing called it. Production `claude/input.rs::plan_skill_dispatch` (the branch with no `[@…](skill://…)` chip, around lines 583–589) always writes `/{name} {prompt}`. A selection whose prose already starts with `/{name}` would reach Claude as `/{name} /{name} …`. Whether the composer can send that shape is unverified; the B9 row only ever proved dead code.
2. **`tests/windows_native.rs` did not compile** (missing `use serde_json::Value`; the `RunRequest` literal lacked four fields). Repaired here as an F; see cross-lane request 2 for why CI never reported it.

## Follow-ups

1. **Dead observation code kept with its tests (product decision).** `claude/normalize.rs` `SessionIntegrationReceipt`, `take_init_integration_receipt`, `bounded_observation_identity`, `has_observed_names` and `MAX_OBSERVED_*` are marked `#[allow(dead_code)]` "until source ownership can be mapped" (d64a46ce). The same is true of `codex/mod.rs` `discover_skill_observations`, `observe_skill_discovery` and `CodexSkillDiscoveryObservation(s)`, which are `#[doc(hidden)] pub` with no non-test caller (928aa665). Their 4 inline and 3 `tests/codex.rs` tests guard behavior no product path reaches. Either wire the consumer or delete each code path together with its tests.
2. Move `permission_modes_compile_to_session_scoped_rules` to the HTTP boundary: `TurnWire` already records the `POST /api/session` `permissions` body. Then delete the `opencode_permission_rules` wrapper.
3. `tests/codex.rs::computer_use_disconnect_reports_unknown_without_replay` asserts only `any` Unknown. The once-only property still lives in inline `no_observed_computer_use_remains_generic_and_disconnect_is_unknown`. Tighten the boundary test to exactly one Unknown, then retire the inline row.
4. `tests/opencode.rs::steer_queues_mid_turn_and_delivers_at_idle` uses a 100 ms sleep for "steer landed in the queue": a timing window standing in for an event. No observable event exists today.
5. `tests/claude_extensions.rs::effective_view_follows_launch_policy_scope_and_precedence` writes `view-full.json` into a tempdir that is deleted at test end, so the "evidence" is never retained.
6. `tests/claude_extensions.rs::hook_lifecycle_becomes_root_hook_chips_with_truthful_outcomes` also asserts `luna_proto::view::tool_chip_content` and `tool_group_summary` (proto/UI view helpers). The proto lane should confirm it owns those, after which the rows here can go.
7. `tests/codex_availability.rs::probe` takes an `nvm` parameter that every caller passes as `None`.
