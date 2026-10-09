# L02 engine integration suites A-L: test-audit ledger

- **Lane:** L02-engine-it-a-l (campaign 2026-10-09, branch `test-audit/L02-engine-it-a-l`, base `e4a0f534`).
- **Scope:** `crates/engine/tests/[a-l]*.rs` (53 test targets) and the shared support module `crates/engine/tests/board_tools_support/mod.rs` (no test declarations; used by the three `board_tools*` targets). `crates/engine/src` belongs to L01, `crates/engine/tests/support/` to L03, `crates/harness/**` to L04.
- **Baseline:** 284 test declarations (including 7 `#[ignore]`d: 4 opt-in live/perf/evidence tests and 3 L29-blocked relay tests), 42,117 lines across the 54 files (1,951 of them in `board_tools_support/mod.rs`).
- **After:** 276 declarations, 41,410 lines. Marks: R 270, F 6, C 4, D 4.
- **Method:** every declaration read in full together with its production owner where needed, overlapping suites (in and outside this lane), `.github/workflows/*.yml` routing (`ui-tests.yml` runs `codex_subagents`, `local_profiles`; `windows.yml` runs `codex_catalog`, `codex_login_resolution`, `auth`), and `git log -S` history for every D/C. Nothing was compiled or run in this lane (coordinator runs everything).

## Ledger

| Mark | Test | Evidence |
|---|---|---|
| R | `crates/engine/tests/account_lifecycle_acceptance.rs::synthetic_account_switch_retires_runtime_without_rebinding_or_tool_replay` | Sole owner: `set_provider_account` retires the warm runtime, next run resumes the same provider session under the new account with file-auth option, no tool replay, Claude re-activation keeps the binding. No other caller of `set_provider_account` in tests. |
| R | `crates/engine/tests/agent_delegation_background.rs::independent_parent_work_and_completion_followup_survive_restart` | Real RPC + durable child + queue + restart: child creation returns before the child finishes, parent keeps working, completion follow-up waits behind the active parent turn, `completedAtMs` persisted, exactly one continuation across restart. Distinct from `claude_child_chats` (provider wire, not scheduling). |
| R | `crates/engine/tests/agent_workspace_controls.rs::provider_defaults_persist_reject_stale_or_unsupported_choices_and_resolve_children` | Sole owner of `Get/SetProviderDefaults` RPC: revision fence, invalid model/effort/option/runtime-contract rejection, child inherits saved defaults but not across an explicit model, persists across restart, clear bumps revision. |
| R | `crates/engine/tests/agent_workspace_controls.rs::background_open_preserves_selection_and_close_preserves_conversation` | Only RPC proof of `openBackgroundTab` keeping focus and active tab, replay identity, close not archiving. `chat_workbench.rs` uses `openTab`, not the background op. |
| R | `crates/engine/tests/agent_workspace_controls.rs::agent_catalog_exposes_bound_workspace_and_rejects_unrelated_targets` | `get_workspace`/`manage_workspace` catalog, compact tool guide (<800 B), delegation topic guide fields, helper background tab, unrelated archive refusal, `list_threads` agent row. `cross_task_tools` MCP test covers other workspace ops; these assertions are unique. |
| R | `crates/engine/tests/agent_workspace_controls.rs::surviving_forks_keep_workspace_tools_after_source_deletion` | Regression: after source deletion a fork and its legacy nested fork re-root the workspace on the fork; unrelated targets still refused; deleted source caller refused. No other owner. |
| R | `crates/engine/tests/agent_workspace_controls.rs::moving_an_existing_background_tab_into_a_split_preserves_focus_and_is_replayable` | `openBackgroundTab` with a split destination for an already-open tab keeps focus, replays identically, `closeTabs` (plural) does not archive. Distinct op/destination from the other workbench tests. |
| R | `crates/engine/tests/attention.rs::request_lifecycle_dedup_persistence_and_exact_routing` | Attention RPC owner: private question text never serialized, seen/ack lifecycle, command-id dedup, restart persistence, resolve -> recent + one transition, foreign target device refused. |
| R | `crates/engine/tests/attention.rs::approval_queue_completion_and_concurrent_claims_use_real_rpc_sources` | Approval, review-required queue row and completion sources; concurrent claims deliver each transition once; clear-recent persists across restart. |
| R | `crates/engine/tests/attention.rs::failed_session_survives_restart_and_resolves_only_on_settled_status` | TaskFailed attention persists across restart, resolves only on a later settled status, a newer failure is a new item. |
| R | `crates/engine/tests/attention.rs::long_history_attention_preserves_latest_assistant_and_measures_cost` | Opt-in (`#[ignore]`, needs `LUNA_PERF_EVIDENCE`) performance/salvage E2E with a retained evidence artifact; latest-assistant and malformed-slot semantics are not covered elsewhere. |
| R | `crates/engine/tests/auth.rs::dev_mode_is_signed_in_with_configured_bearer` | Dev-mode contract (signed in, bearer = dev id, no-op sign-in) against the real `Auth`; windows CI routes `--test auth`. |
| R | `crates/engine/tests/auth.rs::dev_bearer_is_not_exposed_in_auth_metadata` | Security: org part of the dev bearer never appears in serialized auth state. |
| R | `crates/engine/tests/auth.rs::headless_flow_exchanges_pasted_code_and_gates_on_org` | Stub-edge HTTP: CSRF state check, org gate, vault-only session metadata with 0600 mode, org-scoped refresh wakes token subscribers, sign-out removes session. |
| R | `crates/engine/tests/auth.rs::short_lived_tokens_refresh_on_demand` | Refresh below the 30 s slack and rotated refresh-token chaining. |
| R | `crates/engine/tests/auth.rs::revoked_refresh_token_signs_out` | 401 refresh revokes session file without rewriting the captured startup fact. |
| D | `crates/engine/tests/auth.rs::offline_refresh_loop_backs_off_without_revoking_session` | Sleep(200 ms) window over the background loop. Detects: loop retries without backoff or revokes on transport failure. Stronger proof remains: `twenty_consumers_and_background_loop_share_a_failed_refresh` (same dropped-refresh edge + spawned loop, refresh count stays 1 across two consumer batches, still signed in, session file kept) and `refresh_recovers_after_cooldown_without_a_network_event` (loop does attempt and recovers, exactly 2 refreshes). Unlocks: 22 test lines; no production seam. History: 0c4f4526 predates the shared-cooldown test 853872d3. |
| R | `crates/engine/tests/auth.rs::twenty_consumers_and_background_loop_share_a_failed_refresh` | Keeper for refresh backoff: shared cooldown across 40 consumers and the background loop, no revocation on transport failure. |
| R | `crates/engine/tests/auth.rs::temporary_token_errors_reach_http_and_websocket_consumers` | Temporary token errors propagate to edge room URL, chat2 HTTP transport and `list_orgs` with one refresh. (L01's inline `auth::tests::concurrent_dns_failure_is_shared_with_http_and_room_consumers` injects DNS via a test seam; this real-HTTP suite is the stronger boundary.) |
| R | `crates/engine/tests/auth.rs::transient_http_statuses_do_not_revoke_the_session` | Table (408/429/503): retryable statuses stay signed in with the session file. |
| R | `crates/engine/tests/auth.rs::refresh_recovers_after_cooldown_without_a_network_event` | Background loop recovers on its own timer, exactly one extra refresh. |
| R | `crates/engine/tests/auth.rs::explicit_retry_allows_one_shared_attempt_before_cooldown_expires` | `retry_refresh` admits exactly one shared attempt for 20 consumers. |
| R | `crates/engine/tests/auth.rs::cancelling_a_consumer_does_not_cancel_the_shared_refresh` | Cancellation safety of the shared refresh future. |
| R | `crates/engine/tests/auth.rs::sign_out_during_shared_refresh_does_not_restore_credentials` | Race: an in-flight refresh after sign-out returns SignedOut and writes nothing. |
| R | `crates/engine/tests/auth.rs::an_old_refresh_cannot_report_signed_out_after_a_new_sign_in` | Generation fence across 200/401/503 stale responses. |
| R | `crates/engine/tests/auth.rs::loopback_callback_completes_headed_sign_in` | Loopback callback: wrong state 400 without exchange, valid callback signs in org-scoped. |
| R | `crates/engine/tests/auth.rs::sign_out_invalidates_pending_and_in_flight_oauth_callbacks` | Pending callback 400 and in-flight exchange 409 after sign-out; nothing persisted. |
| R | `crates/engine/tests/auth.rs::detect_probes_edge_dev_mode` | `Auth::detect` honours an edge reporting dev auth. |
| R | `crates/engine/tests/automations_e2e.rs::one_occurrence_two_consumers_duplicate_mutation_and_restart` | Clock-controlled RPC->SQLite->task command: duplicate mutation idempotent, concurrent ticks claim once, restart does not replay. No inline automation tests exist in src. |
| R | `crates/engine/tests/automations_e2e.rs::pause_revision_fence_and_sleep_coalescing` | Paused automation never runs; missed interval slots coalesce into one run. |
| R | `crates/engine/tests/automations_e2e.rs::unknown_dispatch_survives_restart_without_replay` | Lost provider acceptance stays `unknown` across restart, raises redacted attention, deletion resolves attention without replay. |
| R | `crates/engine/tests/automations_e2e.rs::calendar_preview_skips_gap_and_chooses_first_fold` | Sole owner of DST gap/fold recurrence (`automation::preview_times`), a pure function with a real input space. |
| R | `crates/engine/tests/automations_e2e.rs::pr_watch_reads_complete_pages_and_wakes_once_per_meaningful_change` | PR watch paging, own-comment suppression, wake budget, concurrent tick dedupe through a fake GitHub read API. |
| R | `crates/engine/tests/automations_e2e.rs::pr_watch_read_failure_is_unknown_and_does_not_advance_cursor` | Read failure keeps the wake cursor, records an error, pause stops reads. |
| R | `crates/engine/tests/automations_e2e.rs::native_agent_schedule_requires_exact_local_allow_once` | fake-codex peer: agent-created automation needs a local Allow-once card; generic provider approval and task allowance cannot mint it. |
| R | `crates/engine/tests/automations_e2e.rs::scheduled_prompt_is_not_starved_by_due_slow_pr_watches` | Fairness: 40 stalled PR watches cannot starve a due prompt. |
| R | `crates/engine/tests/automations_e2e.rs::held_user_queue_and_pause_at_admission_prevent_scheduled_delivery` | Held user queue skips the automation; pause during admission prevents delivery. |
| R | `crates/engine/tests/automations_e2e.rs::new_conversation_keeps_approved_access_and_mutation_failure_is_atomic` | New-conversation target gets the approved (Ask) access, not the origin's later Full; SQL trigger proves atomic mutation rollback. |
| R | `crates/engine/tests/automations_e2e.rs::previews_expire_and_archived_origins_never_run` | Preview expiry/staleness, invalid timezone, nonexistent DST local times, archived origin skipped. |
| R | `crates/engine/tests/automations_e2e.rs::restart_reconciliation_advances_beyond_a_full_page_of_unknown_claims` | Reconciliation pages past 100 stale claims to `unknown` with no provider calls. |
| R | `crates/engine/tests/automations_e2e.rs::live_github_observation_uses_real_bounded_read_api` | Opt-in (`#[ignore]`) read-only live gh proof with evidence; the only real-API check of `read_pr_watch`. |
| R | `crates/engine/tests/automations_e2e.rs::excessive_calendar_backlog_pauses_without_starving_other_due_work` | Ancient backlog pauses and fails the calendar automation without blocking other due work. |
| R | `crates/engine/tests/backup_control.rs::bb_style_control_drives_persisted_manual_and_daily_copy_backup` | `BackupControl` owner: configure, manual backup 0600 archive, inspect, stage, fresh-instance readback, automatic daily tick only after changes. |
| R | `crates/engine/tests/backup_control.rs::staged_checkpoint_fails_closed_across_restart_and_retries_after_repair` | Staged checkpoint integrity: changed/missing stage fails closed across restart, repair retries, legacy state without evidence needs review. |
| F | `crates/engine/tests/backup_control.rs::local_rpc_reports_copy_only_status_and_rejects_foreign_target` | Retained contract (backup RPC status/now/inspect/stage, foreign target and key configuration refused). Repair: drop `EngineCore::with_backup_control_for_tests`; the engine now builds its control through production `IsolatedRoot::open(data_dir)` from the marked owner root after the persisted configure, so the test proves the real wiring. Unlocks deleting the seam in `crates/engine/src/lib.rs` (cross-lane L01); `rg` shows no other caller. |
| R | `crates/engine/tests/backup_control.rs::ordinary_unmarked_fixture_root_has_no_backup_control_or_vault_probe` | Unmarked root: backup Unavailable, BACKUP_NOW refused, no credentials dir; managed offline backup and restore then work and status reflects them. |
| R | `crates/engine/tests/backup_restore.rs::automation_tables_survive_encrypted_archive_and_activation` | Byte-exact automation definition/receipt/occurrence restore (schema 19). |
| R | `crates/engine/tests/backup_restore.rs::workbench_layout_archive_roundtrip_preserves_ids_ratios_and_closed_history` | Workbench layout file restored byte-exact, pending temp file excluded. |
| R | `crates/engine/tests/backup_restore.rs::sqlite_backup_restore_preserves_workspace_w_and_older_execution_lineage` | Workspace repair revision, lineage, executable authority and recovery rows survive restore. |
| R | `crates/engine/tests/backup_restore.rs::bb_style_archive_is_gzip_manifest_verified_and_staged_without_a_key` | bb-style archive format v3, gzip magic, keyless staging. `backup_control` covers the control layer, not the archive format. |
| R | `crates/engine/tests/backup_restore.rs::managed_offline_daily_backup_requires_stopped_engine_and_private_sibling` | Managed offline backup refuses a running engine, 0700/0600 sibling destination, daily cadence, hash chain. |
| R | `crates/engine/tests/backup_restore.rs::managed_offline_restore_preserves_excluded_files_and_keeps_rollback` | Restore keeps excluded secrets/remote grants, rollback dir, original-path guard, engine-running refusal. |
| R | `crates/engine/tests/backup_restore.rs::managed_offline_backup_refuses_unreviewed_future_schema` | Future schema refused (migration contract). |
| C | `crates/engine/tests/backup_restore.rs::managed_offline_backup_refuses_missing_v12_authority_table` | Identical shape to the table test (drop one required table, expect `missing table X`); the same `database_counts` loop checks v12 and v13+ tables. Folded as the first row of `managed_offline_backup_refuses_missing_required_tables` (renamed from `..._missing_v13_v14_feature_tables`, which already held v15/v18/v19 rows). Unlocks 13 lines. |
| R | `crates/engine/tests/backup_restore.rs::managed_offline_backup_refuses_missing_v13_v14_feature_tables` | Keeper table for required-table validation; renamed `managed_offline_backup_refuses_missing_required_tables` and now also owns `local_executable_authority`. (`prompt_stash`, introduced in v16, has no row: follow-up.) |
| R | `crates/engine/tests/backup_restore.rs::backup_refuses_missing_workspace_authority_and_recovery_columns` | Required-column table on the copy-backup path (different entry point from managed offline). |
| R | `crates/engine/tests/backup_restore.rs::managed_offline_v9_archive_remains_restorable_after_current_migrations` | Old-schema archive restores and migrates to 19. |
| R | `crates/engine/tests/backup_restore.rs::managed_offline_recovery_replays_only_journaled_owned_paths` | Interrupted restore recovery replays only journaled paths. |
| R | `crates/engine/tests/backup_restore.rs::managed_offline_failed_readback_rolls_back_without_touching_exclusions` | Readback failure rolls back without touching excluded files (fault via L01 `_for_tests` seam). |
| R | `crates/engine/tests/backup_restore.rs::active_engine_lock_refuses_copy_backup_until_writer_stops` | Instance lock blocks export until the writer stops. |
| R | `crates/engine/tests/backup_restore.rs::staged_activity_rebuild_survives_restart_and_detects_interrupted_readback` | Activity index rebuild and staged blob tamper detection. |
| R | `crates/engine/tests/backup_restore.rs::staged_restore_rebuilds_appshot_history_from_v9_source_facts_on_current_schema` | Appshot activity rebuilt from source facts. |
| R | `crates/engine/tests/backup_restore.rs::chat2_snapshot_requires_full_engine_replay_before_activation` | Pending chat2 updates block activation; rejected updates mark a rebuild blocker. |
| R | `crates/engine/tests/backup_restore.rs::staged_activity_replays_luna_task_identity_without_provider_access` | Task identity log replay during staging. |
| R | `crates/engine/tests/backup_restore.rs::invalid_task_identity_log_blocks_staged_rebuild` | Invalid/orphan identity logs block staged rebuild. |
| R | `crates/engine/tests/backup_restore.rs::malformed_registry_row_cannot_be_silently_counted_as_absent` | Malformed registry rows block rebuild instead of being dropped. |
| R | `crates/engine/tests/backup_restore.rs::export_is_encrypted_verified_and_excludes_secrets` | Security: encryption, wrong key/truncation/tamper rejection, secrets excluded. |
| R | `crates/engine/tests/backup_restore.rs::staged_restore_detects_change_then_activates_and_rolls_back` | Low-space refusal leaves no stage, conflict detection, activation, rollback. |
| R | `crates/engine/tests/backup_restore.rs::restore_refuses_excluded_target_material_without_mutation` | Excluded target material refuses staging without mutation. |
| R | `crates/engine/tests/backup_restore.rs::failed_post_activation_readback_rolls_back_synthetic_root` | Post-activation readback failure rolls back and keeps the failed root. |
| R | `crates/engine/tests/backup_restore.rs::staged_copy_engine_boot_probe_keeps_original_stage_untouched` | Real EngineCore boots a staged copy; corrupt copy fails; stage untouched. |
| R | `crates/engine/tests/backup_restore.rs::github_pr_attempt_journal_survives_backup_and_staging` | Uncertain-write PR journal is archived (lock file excluded). |
| R | `crates/engine/tests/backup_restore.rs::side_chat_metadata_and_lazy_transcript_survive_backup_restore` | Side-chat rows and lazy transcript survive restore. |
| R | `crates/engine/tests/board_authoring_e2e.rs::discussion_reads_the_project_collects_settings_and_saves_one_ticket` | Cases 1,3-6,10,12,16-18 through ClaudeHarness + `fake-claude-board-authoring.py`: project-aware briefed run, read-only proposal, one ticket across retries and a later run, leak report, save-then-close. |
| R | `crates/engine/tests/board_authoring_e2e.rs::close_restart_resume_and_discard_keep_only_a_board_draft` | Cases 1,2,7-9,15: close stops process and endpoint, storage projection refuses chat ids, restart keeps the draft, project-bound resume, discard keeps only exclusion rows. |
| R | `crates/engine/tests/board_authoring_e2e.rs::escalation_in_a_proposal_is_not_applied_and_close_during_a_tool_call_creates_nothing` | Cases 11,13,14: escalation stays data, forged/foreign calls refused, close during an active tool call is bounded and creates nothing. |
| R | `crates/engine/tests/board_authoring_e2e.rs::a_full_access_discussion_is_not_copied_into_the_ticket` | Case 12 reverse: discussion access never reaches the ticket profile; discard after save ends and keeps the ticket. |
| R | `crates/engine/tests/board_authoring_e2e.rs::codex_discussion_refuses_file_changes_and_runs_commands` | Case 19 on the Codex peer (`fake-codex-board.py`): read-only sandbox, file change declined, command accepted, briefing first. Distinct provider path. |
| R | `crates/engine/tests/board_chat_model_e2e.rs::save_creates_nothing_and_board_start_makes_one_main_worker` | Cases 1,2,8,17: save creates no chat/run, one frozen MainChat worker, retry/second parent reuse. |
| R | `crates/engine/tests/board_chat_model_e2e.rs::main_parent_side_chats_and_three_tickets` | Cases 3,5: fresh and with-history side chats sent once; three tickets -> three side workers; coordinator not badged. |
| R | `crates/engine/tests/board_chat_model_e2e.rs::conversion_keeps_identity_history_run_draft_and_placement` | Case 4: in-place conversion keeps id/history/binding/draft/placement and the active run; duplicates refused. |
| R | `crates/engine/tests/board_chat_model_e2e.rs::navigation_survives_closed_tabs_and_restart_and_titles_stay_in_sync` | Cases 6,7,15: navigation from durable records, two-way rename sync, attempt/source titles independent. |
| R | `crates/engine/tests/board_chat_model_e2e.rs::side_chats_cannot_nest_or_fork_through_rpc_or_tools` | Cases 11,15,18: every nesting/fork path (RPC and tool) refused with alternatives; sibling allowed. |
| R | `crates/engine/tests/board_chat_model_e2e.rs::side_handoff_sibling_and_self_promotion_attempts` | Cases 12-14,16: sibling inherits creator model/access, handoff brief, no self-promotion or adopt sequence. |
| R | `crates/engine/tests/board_chat_model_e2e.rs::fixed_parent_rejects_idle_and_active_reparenting` | Case 17: idle and active workers keep their original parent; no extra runs. |
| R | `crates/engine/tests/board_chat_model_e2e.rs::archived_parent_keeps_the_worker_navigable` | Case 10: archived parent leaves worker directly navigable, link not rewritten. |
| R | `crates/engine/tests/board_chat_model_e2e.rs::legacy_nested_children_migrate_without_data_loss` | Case 9: legacy nested children migrate additively; active record deferred; no further nesting. |
| R | `crates/engine/tests/board_github_e2e.rs::board_github_e2e` | Single env-owning test running 10 scenarios against the fake `gh` on PATH (identity, paging/fidelity, freshness/backoff/account scope, timeout, dedupe, snapshot/start, link/deps, writeback/readback/local Done, concurrent writeback, dispatch revalidation). Sole owner of `source_control/issues.rs` behaviour. |
| R | `crates/engine/tests/board_lifecycle_e2e.rs::first_start_stop_and_continue_share_one_chat_and_one_brief` | Cases 1-3,10: one chat/one first-start instruction, Stop via the chat's own command, continuation with delta, reorder has no effect. |
| R | `crates/engine/tests/board_lifecycle_e2e.rs::rapid_toggles_wait_for_stop_and_send_one_continuation` | Case 4: toggles behind a stubborn stop coalesce into one continuation. |
| R | `crates/engine/tests/board_lifecycle_e2e.rs::duplicate_requests_from_two_clients_share_one_outcome` | Case 5: same request id -> identical receipt; distinct ids -> one launch. |
| R | `crates/engine/tests/board_lifecycle_e2e.rs::queued_launch_is_cancelled_before_sending` | Case 6: admission-queued launch cancelled unsent on Backlog. |
| R | `crates/engine/tests/board_lifecycle_e2e.rs::clearing_a_dependency_requires_a_new_start` | Case 17: dependency hold survives clearing and restart; only an explicit Start launches. (board_github's dependency step covers a Backlog ticket only.) |
| R | `crates/engine/tests/board_lifecycle_e2e.rs::unknown_first_delivery_is_reconciled_not_resent` | Cases 7,8: lost acceptance stays Unknown with Retry action, never resent by moves/restart. |
| R | `crates/engine/tests/board_lifecycle_e2e.rs::restart_with_ready_and_stopping_never_resends` | Case 8: restart restores Ready without resend and settles Stopping without relaunch. |
| R | `crates/engine/tests/board_lifecycle_e2e.rs::stale_stop_and_review_callbacks_change_nothing` | Case 9: late stop receipts and stale passing reviews change nothing; current pass reaches Done once. |
| R | `crates/engine/tests/board_lifecycle_e2e.rs::unavailable_model_is_refused_without_fallback` | Case 11: structured refusals with supported choices; saved settings kept. |
| R | `crates/engine/tests/board_lifecycle_e2e.rs::three_ticket_batch_is_bounded_with_per_ticket_results` | Case 12: per-ticket results, one writer per checkout, retried batch idempotent. |
| R | `crates/engine/tests/board_lifecycle_e2e.rs::status_reports_are_owner_bound_and_stale_safe` | Case 13: owner-bound, stale-safe, idempotent status reports that cannot express Done. |
| R | `crates/engine/tests/board_lifecycle_e2e.rs::brief_edit_keeps_sent_snapshot_and_send_update_delivers_once` | Case 14: sent snapshot immutable; Send update delivers once. |
| R | `crates/engine/tests/board_lifecycle_e2e.rs::report_back_is_delivered_once_across_restart` | Case 15: report delivered once to the archived source across restart, source not woken. |
| R | `crates/engine/tests/board_lifecycle_e2e.rs::deferred_settings_hold_ready_until_chosen` | Case 16: SettingsUnresolved hold; start once with exactly the chosen settings. |
| R | `crates/engine/tests/board_review_e2e.rs::committed_initial_result_is_in_the_review_packet` | Case 11 row (born repository) via the shared helper: committed result compared with the pre-launch base. |
| R | `crates/engine/tests/board_review_e2e.rs::committed_first_result_in_an_unborn_repository_is_reviewed` | Case 11 row (unborn repository): same helper, distinct Git edge case. |
| R | `crates/engine/tests/board_review_e2e.rs::committed_repair_checks_match_the_retained_review_base` | Case 11: committed repair keeps the review base with fresh checks. |
| R | `crates/engine/tests/board_review_e2e.rs::main_ticket_codex_reviewed_by_claude_repairs_seeded_defect_then_cleans_up` | Cases 1,3,8: cross-harness reviewer placement, forged submit refused, repair loop, Done, exact cleanup, archived details readable. |
| R | `crates/engine/tests/board_review_e2e.rs::side_ticket_sibling_reviewer_and_stop_during_review` | Cases 2,10: sibling reviewer under the main; Stop reaches exactly the reviewer; late pass stays history. |
| R | `crates/engine/tests/board_review_e2e.rs::diff_mutated_during_review_is_stale_and_reviewed_again` | Case 4: mutated diff makes the verdict stale; new basis reviewed; stale checks never complete. |
| R | `crates/engine/tests/board_review_e2e.rs::restart_at_launch_and_delivery_boundaries_is_once_only` | Case 5: concurrent retries share a run; one reviewer chat and one repair delivery across restarts. |
| R | `crates/engine/tests/board_review_e2e.rs::unavailable_reviewer_model_is_unresolved_without_fallback` | Case 6: unavailable model refused with choices; no reviewer turn. |
| R | `crates/engine/tests/board_review_e2e.rs::omitted_checks_and_unobserved_substitution_never_complete` | Case 6: missing required checks and observed substitution never yield Done. |
| R | `crates/engine/tests/board_review_e2e.rs::reviewer_prose_without_result_is_relaunched_once_then_unresolved` | Case 6: one relaunch, then unresolved; no third turn. |
| R | `crates/engine/tests/board_review_e2e.rs::exhausted_repair_rounds_need_attention` | Case 6: repair cap reached -> Needs attention; model cannot grant another round. |
| R | `crates/engine/tests/board_review_e2e.rs::a_current_failed_check_cannot_reuse_an_older_pass` | Case 12: newer failed check on the same diff blocks Done. |
| R | `crates/engine/tests/board_review_e2e.rs::inherited_reviewer_model_substitution_is_rejected` | Case 12: inherited model requires a matching observation. |
| R | `crates/engine/tests/board_review_e2e.rs::large_artifact_tail_changes_invalidate_review` | Case 13/14: same-size change past the first MiB invalidates review and old checks. |
| R | `crates/engine/tests/board_review_e2e.rs::new_nested_files_are_in_the_review_packet` | Case 14: collapsed untracked directories still list nested files. |
| R | `crates/engine/tests/board_review_e2e.rs::converted_main_chat_keeps_identity_with_fallback_reviewer` | Case 7: converted chat keeps identity/history; implementation-model fallback reviewer in a separate side chat. |
| R | `crates/engine/tests/board_review_e2e.rs::cleanup_waits_for_late_reviewer_and_survives_layout_conflicts` | Case 9: cleanup waits for the reviewer, survives layout changes, journals once. |
| R | `crates/engine/tests/board_store_e2e.rs::create_is_save_only_idempotent_and_project_scoped` | Cases 1,2,5,6,10: save-only creation, idempotency/conflict, per-project keys, scope, paging. |
| R | `crates/engine/tests/board_store_e2e.rs::update_archive_rank_and_watch_share_one_canonical_state` | Cases 3,7,9,11: field-level merge vs stale, archive/unarchive, rank without execution, one watch event. |
| R | `crates/engine/tests/board_store_e2e.rs::restart_and_backup_restore_preserve_tickets_keys_order_and_journal` | Cases 4,5,8: restart and encrypted backup/restore round-trip board state and receipts. |
| R | `crates/engine/tests/board_store_e2e.rs::every_board_method_is_routed_and_older_profiles_upgrade_additively` | Cases 12,13: every `BOARD_METHODS` entry routed with structured errors; schema 18 -> 19 additive. |
| R | `crates/engine/tests/board_store_e2e.rs::heavy_board_rpcs_fit_a_two_mib_worker_stack` | Case 14 regression: heavy board RPCs answer on a 2 MiB worker stack (overflow aborts the binary). |
| R | `crates/engine/tests/board_tools_codex_e2e.rs::codex_board_tools_follow_role_authority_and_context_through_luna_mcp` | Shared `board_tools_support` scenario over CodexHarness + `fake-codex-board.py` (private per-process endpoint, Codex `_meta`, contextCompaction item). Each provider has its own adapter and peer, so this is per-provider delivery proof, not one fake standing in for three. See follow-up on the scenario's not-landed fallback branches. |
| R | `crates/engine/tests/board_tools_e2e.rs::claude_board_tools_follow_role_authority_and_context_through_luna_mcp` | Shared `board_tools_support` scenario over ClaudeHarness + `fake-claude-luna.py` (shared per-process endpoint, compact_boundary). Each provider has its own adapter and peer, so this is per-provider delivery proof, not one fake standing in for three. See follow-up on the scenario's not-landed fallback branches. |
| R | `crates/engine/tests/board_tools_opencode_e2e.rs::opencode_board_tools_follow_role_authority_and_context_through_luna_mcp` | Shared `board_tools_support` scenario over OpencodeHarness + `fake-opencode-board.py` (inline config layer, `/api/mcp` readback, bus compaction). Each provider has its own adapter and peer, so this is per-provider delivery proof, not one fake standing in for three. See follow-up on the scenario's not-landed fallback branches. |
| R | `crates/engine/tests/born_chat2_race.rs::transcript_survives_open_racing_create_chat` | Regression (born-gen2 race, 2026-08-11): doc opened before its registry row lands on chat2 and the transcript survives restart. |
| R | `crates/engine/tests/chat_workbench.rs::optimistic_main_becomes_available_after_creation_without_recreating_deleted_chats` | Workbench owner: unavailable before creation, watch publishes after, deleted main not recreated. |
| R | `crates/engine/tests/chat_workbench.rs::new_thread_opens_where_asked_without_a_layout_conflict` | Regression: CreateChildTab leaves the layout revision alone so split/pane opens do not conflict. |
| R | `crates/engine/tests/chat_workbench.rs::non_main_source_creates_fresh_and_context_threads_in_the_host_layout` | Fresh child and context-snapshot side chat from a non-main source; persisted context and published rows across restart. |
| R | `crates/engine/tests/chat_workbench.rs::balance_panes_preserves_mixed_layouts_and_equalizes_leaf_area` | Failure cases 28-29: balance is idempotent, equal leaf area, ratio guard at 16 panes, replay/conflict, restart. |
| R | `crates/engine/tests/chat_workbench.rs::empty_pane_focus_survives_restart_and_guides_reopen` | Empty-pane focus survives restart and guides reopen; invalid targets rejected. |
| R | `crates/engine/tests/chat_workbench.rs::nested_tabs_moves_reopen_watches_and_restart_use_the_same_chats` | Nested moves/resize/reopen, protected main, no chat edits or provider runs, restart. |
| R | `crates/engine/tests/chat_workbench.rs::command_receipts_revision_conflicts_and_account_profiles_are_isolated` | Command-id replay, revision conflict, reused id, retired writer, per-profile isolation. |
| R | `crates/engine/tests/chat_workbench.rs::malformed_targets_limits_and_missing_chats_do_not_corrupt_layouts` | Invalid commands leave layout intact; deletion prunes via watch; pane/ratio limits. |
| R | `crates/engine/tests/chat_workbench.rs::failed_storage_and_corrupt_files_preserve_last_good_state` | Symlinked/corrupt layout files: last good state and corrupt bytes preserved. |
| R | `crates/engine/tests/claude_cc06_e2e.rs::explicit_claude_steer_settles_by_native_replay_receipt` | E11 via `fake-claude-cc06.py`: steer row settles only on the native replay of its own uuid, one process. |
| R | `crates/engine/tests/claude_cc06_e2e.rs::foreign_claude_result_does_not_complete_the_human_turn` | E24 on the CC-06 replay-receipt path (`origin` results, replayed uuids). Overlaps `claude_turn_identity` (stronger assertions, different peer/frame shapes); retained because the peers drive different receipt code. |
| R | `crates/engine/tests/claude_cc06_e2e.rs::parked_claude_run_switches_model_natively_and_usage_reports_it` | E10/D1/D5: native set_model before the prompt, one process, live usage latest-wins and stream-observed limits. |
| R | `crates/engine/tests/claude_cc06_e2e.rs::rejected_claude_switch_redispatches_once_with_the_new_model` | C2: rejected switch re-dispatches once with the new model and resumed session. |
| R | `crates/engine/tests/claude_cc06_e2e.rs::retired_claude_runtime_is_not_a_live_usage_row` | Warm idle is live; another session's row and a retired runtime are not; ledger kept. |
| R | `crates/engine/tests/claude_child_chats.rs::child_permission_changes_obey_creation_and_current_parent_ceilings` | Table (creation cap / current parent cap / explicit grant): picker, runtime contract and actual `--permission-mode` argv agree. |
| R | `crates/engine/tests/claude_child_chats.rs::child_permission_change_rejects_hidden_ask_without_mutation` | Full above an Ask parent ceiling is rejected without mutation. |
| R | `crates/engine/tests/claude_child_chats.rs::claude_manual_child_runs_fresh_reports_as_a_peer_and_survives_restart` | A1-A7/B1: fresh native child session, peer envelope to the parent, no duplicate delivery across restart, child resume. |
| R | `crates/engine/tests/claude_child_chats.rs::claude_native_peers_are_attributed_and_holds_never_strand_a_turn` | C1-C7/B3-B5: peer attribution, mid-turn split, socket holds, held report ends the turn, permission-class parity skip. |
| R | `crates/engine/tests/claude_command_scope.rs::list_commands_uses_the_chat_or_space_run_scope` | E23: `ListCommands` probes under the chat/space run scope (cwd, mode, account); cache and account re-probe; unknown chat refused. |
| R | `crates/engine/tests/claude_goals_review.rs::claude_goal_on_a_cold_task_runs_claudes_goal_and_marks_it_achieved` | G1/G5/G9/G15 via `fake-claude-goals.py`: `/goal` line, single user row, achieved mark on the right entry. |
| R | `crates/engine/tests/claude_goals_review.rs::claude_goal_warm_start_clear_and_unsupported_controls` | G1 warm/G11/G12: live session reuse, pause refused, clear in a resumed restricted process. |
| R | `crates/engine/tests/claude_goals_review.rs::claude_goal_is_refused_while_a_turn_runs` | G13: refused mid-turn, nothing written to stdin. |
| R | `crates/engine/tests/claude_goals_review.rs::claude_review_runs_code_review_and_shows_shared_findings` | R1/R2/R5: `/code-review` target mapping, shared findings, malformed ref refused. |
| R | `crates/engine/tests/claude_goals_review.rs::claude_schedules_persist_for_the_binding` | S1-S4: native schedules persisted with ids/states. |
| R | `crates/engine/tests/claude_history.rs::claude_history_operations_use_native_identities_end_to_end` | CC-10 single scenario via `fake-claude-history.py`: checkpoints, missing-checkpoint refusals, file rewind digests, forks, side-fork prefixes, unknown/external turns, edits. |
| R | `crates/engine/tests/claude_inputs.rs::claude_images_reach_the_cli_on_every_admitted_turn_type` | CC-04: images reach the CLI on cold/steer/follow-up/auto-steer/image-only/typed-skill turns with resume and one CC-12 readback. |
| R | `crates/engine/tests/claude_inputs.rs::invalid_claude_input_rejects_recoverably_with_the_exact_draft_envelope` | Table (SVG/TIFF/CMYK/mention/foreign path): recoverable rejection, no process, exact draft survives restart. |
| F | `crates/engine/tests/claude_luna_tools.rs::claude_chat_reads_creates_and_sends_through_luna_mcp_without_extra_approvals` | Retained CC-07 contract. Repair: removed a never-incremented `approvals` counter whose `assert_eq!(approvals, 0)` compared a constant with itself; the panic on `InputRequested` is the real guard and is now the stated check. |
| R | `crates/engine/tests/claude_mcp_inventory.rs::claude_effective_mcp_inventory_actions_and_resources` | CC-11: secret-free inventory, launch-mode status, provider-owned disable/enable with readback, forged/remote/other-provider refusals, inert ui:// reads, session evidence scoping. |
| R | `crates/engine/tests/claude_native_agents.rs::nested_native_agents_land_in_their_owning_transcripts` | E25: nested agent graph lands in owning transcripts with status/model/usage; no leakage to the root. |
| R | `crates/engine/tests/claude_native_tasks.rs::parked_workflow_updates_persist_and_stream_without_reopening_the_turn` | CC-13: native workflow ledger persists after Done and streams via WatchNativeTasks. |
| R | `crates/engine/tests/claude_native_tasks.rs::provider_exit_mid_workflow_settles_it_as_session_ended` | Provider exit settles an active workflow as SessionEnded. |
| R | `crates/engine/tests/claude_permissions.rs::claude_approval_details_persist_and_session_choice_reaches_the_cli` | CC-03: details persisted, forged index refused, AllowTask maps to the exact native session choice (peer-verified). |
| R | `crates/engine/tests/claude_permissions.rs::claude_native_cancel_retires_the_card_and_never_answers` | Native cancel retires the card before turn end; late answer refused. |
| R | `crates/engine/tests/claude_tool_results.rs::claude_rich_results_survive_journal_lazy_fetch_and_restart` | CC-05: chip order, private previews, lazy journal bodies, child id scoping, restart equality. |
| R | `crates/engine/tests/claude_tool_results.rs::saved_bash_output_survives_lazy_fetch_and_restart_without_the_provider_file` | Saved Bash output survives without the provider file. |
| R | `crates/engine/tests/claude_tool_results.rs::saved_bash_error_detail_keeps_failure_stderr_and_interrupt_evidence` | Failure detail keeps stderr and interrupt evidence. |
| R | `crates/engine/tests/claude_turn_identity.rs::e24_foreign_result_never_completes_the_human_receipt_or_releases_the_queue` | E24 keeper: foreign result journaled as withheld, no Idle/Done/queue release, ordering and exactly two Dones. |
| R | `crates/engine/tests/codex_catalog.rs::catalog_child` | Child probe spawned by the next test with a controlled env; returns early when run directly by design. Windows CI routes `--test codex_catalog`. |
| R | `crates/engine/tests/codex_catalog.rs::npm_codex_is_offered_by_production_catalog` | Windows npm `codex.cmd` shim discovered by the production registry and `ListHarnesses` (installed+enabled). |
| R | `crates/engine/tests/codex_goals.rs::cold_goal_resume_attaches_runtime_without_replacing_goal_or_user_input` | Metadata Resume after restart attaches an owned runtime without replacing the goal or adding user input; steer reaches it. |
| R | `crates/engine/tests/codex_goals.rs::cold_goal_resume_refusal_and_missing_turn_are_not_applied` | Table (rejectAttach/noTurnOnAttach/incompatibleRuntime): ProviderAcceptanceUnknown, competing runtime preserved. |
| R | `crates/engine/tests/codex_goals.rs::cold_goal_resume_accepts_completion_before_readiness_is_observed` | Completion before readiness is accepted as Applied. |
| R | `crates/engine/tests/codex_goals.rs::goal_sent_to_a_cold_task_starts_working_and_is_marked_achieved` | Regression: cold goal starts the runtime and is marked achieved. |
| R | `crates/engine/tests/codex_goals.rs::goal_sent_to_a_warm_task_starts_on_the_loaded_runtime` | Warm goal runs on the loaded connection as a new turn. |
| R | `crates/engine/tests/codex_login_resolution.rs::login_child` | Child probe spawned with a controlled env; returns early when run directly by design. Windows CI routes this binary. |
| R | `crates/engine/tests/codex_login_resolution.rs::codex_login_honors_override_only_installation` | Override-only install logs in; an unchanged existing login is not completion; cancel preserves the auth home. Runs on Windows CI, unlike `codex_login_resolver`. |
| R | `crates/engine/tests/codex_login_resolution.rs::codex_login_runs_the_npm_cmd_shim` | Windows PATHEXT `.cmd` shim login. |
| R | `crates/engine/tests/codex_login_resolution.rs::codex_login_without_any_cli_fails_with_a_named_error` | Missing CLI yields an error naming codex. |
| R | `crates/engine/tests/codex_login_resolver.rs::codex_login_resolves_override_and_adds_its_directory_to_child_path` | Unix: interpreter beside the overridden CLI is added to the child PATH (single-test binary for env). |
| R | `crates/engine/tests/codex_subagents.rs::v1_children_persist_under_their_spawn_chips` | Real CodexHarness v1 child decoding, persistence and resumed-child append. Routed by ui-tests.yml. |
| R | `crates/engine/tests/codex_subagents.rs::v2_followup_appends_to_the_original_document_and_updates_the_original_chip` | v2 lifecycle: follow-up appends to the original document, chip updated with usage/error. |
| R | `crates/engine/tests/codex_subagents.rs::parent_turn_end_does_not_claim_an_uncompleted_child_stopped` | Parent Done marks an unobserved child Unobserved, not stopped. |
| R | `crates/engine/tests/context_transfer.rs::context_transfer_preview_commit_delivery_restart_and_delta` | Preview is read-only, apply is idempotent, delivery once without hidden reasoning, delta after restart. |
| R | `crates/engine/tests/context_transfer.rs::context_transfer_budget_stale_preview_and_deleted_source` | Budget bound, oversized prompt refused, stale preview conflict, deleted source items unavailable. |
| R | `crates/engine/tests/context_transfer.rs::context_transfer_unknown_acceptance_survives_restart_without_reinjection` | Unknown acceptance survives restart and blocks re-injection. |
| R | `crates/engine/tests/context_transfer.rs::portable_context_and_provider_roundtrip_keep_identity_and_explicit_delivery` | Portable fork and provider handoff round trip keep lineage; new binding generation. |
| R | `crates/engine/tests/context_transfer.rs::context_transfer_payload_and_projection_rollback_with_receipt_failure` | SQL trigger proves atomic rollback of payload and projection. |
| R | `crates/engine/tests/context_transfer.rs::portable_new_task_preview_creates_nothing_and_apply_is_atomic` | New-task portable fork: preview creates nothing; apply atomic and recoverable. |
| R | `crates/engine/tests/context_transfer.rs::partial_message_delta_continues_after_delivered_utf8_range` | Partial UTF-8 delta continues from the delivered byte range. |
| R | `crates/engine/tests/context_transfer.rs::warm_provider_retirement_cannot_restore_resume_across_fresh_binding` | Regression: a fresh managed binding overrides the old provider's late resume cache. |
| F | `crates/engine/tests/cross_task_tools.rs::fake_codex_tool_uses_real_durable_queue_without_extra_approval` | Retained L25 contract (Codex tool send writes a durable receipt/queue row, denied/foreign sends write nothing, persisted across restart). Repair: removed the never-incremented `approvals` counter and its self-comparing `assert_eq!(result.0, 0)`; the `InputRequested` panic is the guard. |
| R | `crates/engine/tests/cross_task_tools.rs::provider_send_rejects_archived_target_without_parking` | Archived target refused without an approval card or receipt. |
| R | `crates/engine/tests/cross_task_tools.rs::approved_provider_send_sql_failure_has_no_queued_receipt_or_row` | Storage failure leaves no applied receipt or queued row. |
| R | `crates/engine/tests/cross_task_tools.rs::local_backend_pages_only_visible_tasks_and_waits_on_status_rows` | Backend catalog/scopes, visible-project paging, read redaction, wait cursors, control create/archive/move/interrupt, send idempotency/conflict. |
| R | `crates/engine/tests/cross_task_tools.rs::luna_mcp_endpoint_enforces_identity_fences_and_parks_approval` | Real per-process MCP endpoint: token/Origin/Host/method/path/size fences, thread/turn/call-id fences, scope, workspace ops, cancelled/stopped parked calls, per-process tokens, retirement. |
| R | `crates/engine/tests/device_routing.rs::checkout_change_request_stream_matches_locally_and_through_device_routing` | Stream proxy over the relay matches local; iOS wire format; one host resolution shared. |
| R | `crates/engine/tests/device_routing.rs::unsupported_remote_change_request_watch_keeps_the_shared_device_link` | Capability rejection from a legacy host keeps the shared link. |
| D | `crates/engine/tests/device_routing.rs::target_device_id_routes_over_the_relay` | `#[ignore]`d since 2c3b28a6 (L29) and cannot pass by design: `ReadAttachmentChunk` and `ListFolders` are local-only in `luna_rpc::remote_auth::method_policy` and `QueueCommand` is denied. Detects nothing in any run. Remaining proof: unary forward and stream proxy over the relay (`terminal_stream_proxies_over_the_relay`, `checkout_change_request_stream_matches_locally_and_through_device_routing`, `unsupported_remote_change_request_watch_keeps_the_shared_device_link`), remote denials (`remote_auth.rs::two_clients_pair_scope_revoke_stream_and_restart`, `luna_rpc::remote_auth::tests::policy_inventory_is_complete_and_unknown_fails_closed`), own-device `targetDeviceId` handled locally (exercised by `attention.rs` `GetAttention`). Unlocks ~190 lines and the `SessionCommandPayload`/`SandboxLevel` imports. |
| R | `crates/engine/tests/device_routing.rs::terminal_stream_proxies_over_the_relay` | M5: terminal open/write/subscribe/close over the relay (TerminalOperate scope). |
| F | `crates/engine/tests/device_routing.rs::workspace_file_surface_proxies_over_the_relay` | Was `#[ignore]`d only because its final `WriteWorkspaceFile` step is local-only since L29. Repair: drop that write step (denial owned by `remote_auth.rs`) and re-enable, restoring the only E2E proof of remote list/search/read/image-chunk/watch reads (all `files-read`). Behaviour not run in this lane: if it fails, it is a remote-read regression or my misjudgment (see report). |
| R | `crates/engine/tests/device_routing.rs::remote_target_without_links_fails_clearly` | Offline routing fails clearly and never falls back locally. |
| D | `crates/engine/tests/device_routing.rs::queue_watch_and_single_consumption_route_to_the_remote_chat_host` | `#[ignore]`d since 2c3b28a6: `SendQueuedMessageNow` is local-only under L29, so the remote send and the remote/local race cannot run. Detects nothing. Remaining proof: single consumption `message_queue.rs::concurrent_drains_release_one_message`; remote stream proxy covered by the change-request and terminal relay tests; denial by `remote_auth`. Unlocks ~170 lines. |
| R | `crates/engine/tests/diff_sync_churn.rs::entry_survives_transient_identity_failure` | Regression (reconcile runaway): memoized identity survives a transient git failure. |
| R | `crates/engine/tests/diff_sync_churn.rs::chat_flap_keeps_entry_and_sustained_absence_removes_it` | Orphan grace: flap keeps the entry, sustained absence removes it. |
| R | `crates/engine/tests/diff_sync_churn.rs::deleted_checkout_is_evicted_after_grace` | Vanished checkout still evicted by the repair tick. |
| R | `crates/engine/tests/document_residency.rs::quiet_host_reclaims_unwatched_documents_and_preserves_live_views` | DocHost eviction reclaims idle docs, keeps watched/written ones, reopen keeps content. |
| R | `crates/engine/tests/document_residency.rs::failed_snapshot_does_not_evict_the_only_unsaved_copy` | Failed snapshot keeps the only unsaved copy until SQLite accepts it. |
| R | `crates/engine/tests/drafts.rs::submission_retry_preserves_the_exact_destination_across_restart` | Draft submission destination fence across restart. |
| R | `crates/engine/tests/drafts.rs::appshot_draft_lifecycle_projects_into_owned_history_without_private_context` | Appshot history projection without private accessibility/path context. |
| R | `crates/engine/tests/drafts.rs::rich_draft_restarts_and_missing_blob_stays_visibly_unsaved` | Rich draft restart; missing blob becomes NeedsReattachment. |
| R | `crates/engine/tests/e2e.rs::queued_run_command_executes_end_to_end` | Doc command -> executor -> fold (write content stripped) -> journal seq -> broadcast -> Idle. |
| R | `crates/engine/tests/e2e.rs::session_status_transitions_idle_working_idle` | Watch publishes Working then Idle. |
| R | `crates/engine/tests/e2e.rs::interrupt_stamps_streaming_entry_aborted` | Durable Interrupt stamps the streaming entry Aborted and closes the journal. |
| R | `crates/engine/tests/e2e.rs::interrupt_is_scoped_to_the_target_chat` | Interrupt isolation across chats. |
| R | `crates/engine/tests/e2e.rs::steer_with_no_live_run_falls_back_to_new_turn` | Steer without a live run queues a new turn. |
| C | `crates/engine/tests/e2e.rs::processed_commands_are_skipped_on_redelivery` | Same setup as `retry_reissues_a_swallowed_send` (ledger-marked command terminalized Rejected without executing). Its extra checks: session never started (carried into the keeper) and `luna_doc::evaluate_command` -> Skip, a crate-local replay of `luna_doc::commands::tests::processed_commands_are_skipped`. Also used a 300 ms sleep window. Unlocks 70 lines. |
| R | `crates/engine/tests/e2e.rs::retry_reissues_a_swallowed_send` | Keeper for crash-window commands: Rejected without execution, no session started (absorbed), retry mints exactly one fresh attempt, healthy retry is a no-op. |
| R | `crates/engine/tests/e2e.rs::recover_stale_journal_stamps_aborted_on_boot` | Boot recovery stamps Aborted and closes the journal. |
| R | `crates/engine/tests/e2e.rs::rpc_surface_over_in_memory_transport` | RPC wire: ListHarnesses/Models, WatchSessions, WatchDocMessages reset-frame shape and deltas, QueueCommand. |
| R | `crates/engine/tests/e2e.rs::model_catalog_cache_survives_refresh_failure_and_engine_reopen` | Model cache: refresh, failure fallback, reopen. Distinct from `model_cache_lifecycle.rs` (worker retirement). |
| C | `crates/engine/tests/e2e.rs::respond_input_resolves_pending_question` | Same AskingHarness flow as `wrong_id_respond_is_rejected_and_correct_answer_still_resumes` minus the bogus answer. Its unique assertions (answer command Applied, Input part resolved, wait for Idle) moved into that keeper. Unlocks ~150 lines. |
| R | `crates/engine/tests/e2e.rs::typed_live_approval_rejects_respond_input_and_accepts_only_respond_approval` | Keeper for the RespondInput bypass of a typed approval; generic AllowOnce on a details-less card. |
| F | `crates/engine/tests/e2e.rs::structured_approval_preserves_details_and_rejects_unadvertised_choice` | Retained (details persisted, forged index refused, queue drains after Allow, native Cancel holds the queue). Repair: removed its verbatim copy of the RespondInput-bypass block (duplicate invocation of the contract owned by the previous test). |
| R | `crates/engine/tests/e2e.rs::recovered_typed_approval_cannot_replay_as_input_but_ordinary_question_can` | Recovered cards: ordinary answer becomes a new turn; typed approval needs respondApproval and closes safely. |
| R | `crates/engine/tests/e2e.rs::rpc_permission_change_keeps_active_effective_until_replacement_starts` | Table (restrict/escalate): pending policy, redacted execution snapshot, steer blocked, replacement applies, idle immediate apply. |
| R | `crates/engine/tests/e2e.rs::unsupported_auto_never_runs_as_hidden_ask` | OpenCode Auto never degrades to hidden Ask; provider never started. |
| R | `crates/engine/tests/e2e.rs::async_question_skip_and_late_answer_use_distinct_durable_paths` | Async questions: skip leaves open, late answer, idempotent duplicate, amendment. |
| R | `crates/engine/tests/e2e.rs::async_answer_steer_waits_for_in_flight_tool_boundary` | Async answer steer waits for the tool boundary without interrupting. |
| F | `crates/engine/tests/e2e.rs::wrong_id_respond_is_rejected_and_correct_answer_still_resumes` | Keeper for pending-question answers (absorbs `respond_input_resolves_pending_question`). Repair: the final immediate `session_status == Idle` assertion raced the run task (it writes Complete before settling status); now waits for Idle and also asserts command Applied and the resolved Input part. |
| R | `crates/engine/tests/e2e.rs::interrupt_unblocks_a_run_awaiting_input` | Interrupt unparks a blocked question promptly; next run works. |
| R | `crates/engine/tests/e2e.rs::harness_emitted_input_twin_is_dropped_and_answer_resumes` | Regression: harness-emitted InputRequested twin is dropped. |
| R | `crates/engine/tests/e2e.rs::attachment_upload_then_run_threads_refs_and_paths` | Chunked upload -> run threads refs and paths -> ReadAttachmentChunk (engine path; Claude path in `claude_inputs`). |
| R | `crates/engine/tests/e2e.rs::real_claude_sees_uploaded_image_inline` | Opt-in (`#[ignore]`) live Claude proof of inline images. |
| R | `crates/engine/tests/e2e.rs::empty_reasoning_deltas_are_heartbeats_not_journal_noise` | Empty reasoning deltas never reach the journal. |
| R | `crates/engine/tests/e2e.rs::parked_session_ignores_trailing_frames_and_stays_idle` | Regression: trailing frames after Done do not re-arm Working or open a phantom entry. |
| R | `crates/engine/tests/e2e.rs::stale_tool_echo_after_steer_boundary_does_not_split_text` | Regression: stale tool echo after a steer boundary does not split text; late result persisted. |
| R | `crates/engine/tests/e2e.rs::parked_steer_restamps_started_at_and_idle_clears_it` | Regression: timer base cleared on Idle and restamped on parked steer. |
| R | `crates/engine/tests/e2e.rs::context_usage_settles_after_done_without_reopening_the_turn` | Post-Done context usage updates without reopening the turn; subagent usage ignored. |
| R | `crates/engine/tests/e2e.rs::pending_steer_handoff_does_not_publish_a_completion` | Table (steer/routed dispatch): no premature completion; duplicate Done does not ring twice. |
| R | `crates/engine/tests/e2e.rs::generated_image_is_materialized_before_publication_and_survives_reopen` | Generated image copied into uploads before publication; no source path in journal/doc. |
| R | `crates/engine/tests/e2e.rs::real_image_generation_profile_smoke` | Opt-in (`#[ignore]`) live Codex image smoke. |
| R | `crates/engine/tests/e2e.rs::mcp_form_url_validation_concurrency_and_secret_boundaries_e2e` | MCP form/URL elicitation: concurrency, validation, secret URL never durable, stop/restart stale answers. |
| R | `crates/engine/tests/e2e.rs::codex_retry_and_warning_are_transient_notices_not_error_parts` | Codex adapter peer: retry/warning are transient notices, real error stays. |
| R | `crates/engine/tests/e2e.rs::parked_async_question_is_visible_once_without_reopening_turn` | Late async question after Done gets one durable card. |
| R | `crates/engine/tests/e2e.rs::carried_question_preserves_unattended_deadline_and_hold` | Table (auto/hold): carried card keeps deadline; hold never auto-answers. |
| R | `crates/engine/tests/git_checkout_ancestry_e2e.rs::vanished_nested_worktree_cannot_preview_or_stage_ancestor_checkout` | Real git: vanished linked worktree never previews/stages the ancestor; controls for intact worktree and subdirectory. |
| R | `crates/engine/tests/github_cli_login_shell.rs::github_cli_on_login_shell_path_only_resolves_pull_request` | gh reachable only via the login shell PATH resolves the PR with prompts disabled. |
| R | `crates/engine/tests/github_draft_pr.rs::draft_pr_subprocess_acceptance` | Production PR service vs fake gh: preflight failures, exact draft POST once, lost-response reconciliation, no push/merge. |
| R | `crates/engine/tests/github_pr_ancestry_e2e.rs::vanished_nested_worktree_cannot_preview_ancestor_pull_request` | PR preview never falls back to the ancestor checkout; no POST. |
| R | `crates/engine/tests/idle_archive.rs::idle_compaction_releases_only_its_writer_and_keeps_the_binding` | Compaction releases only the target writer, keeps binding/history, waits past the 10 s RPC deadline, no replay. |
| R | `crates/engine/tests/idle_archive.rs::idle_archive_waits_for_only_the_target_writer_and_preserves_identity_on_restart` | Archive retires only the target writer; identity kept; restart replay does not repeat. |
| R | `crates/engine/tests/idle_archive.rs::active_archive_preserves_both_the_active_turn_and_other_warm_writer` | Active turn conflict: nothing touched. |
| R | `crates/engine/tests/idle_archive.rs::queued_dispatch_waiting_on_archive_rechecks_the_committed_state` | Race: a dispatch claimed during archive rechecks and becomes DeliveryUnknown. |
| R | `crates/engine/tests/idle_archive.rs::unreleased_writer_blocks_archive_before_any_native_mutation` | Stuck writer blocks archive before native calls. |
| R | `crates/engine/tests/idle_archive.rs::failed_or_unknown_archive_never_commits_or_replays_the_same_command` | Table (reject/lost): Pending ProviderAcceptanceUnknown, no replay across restart. |
| R | `crates/engine/tests/idle_archive.rs::disconnected_native_archive_does_not_leave_an_unexplained_pending_receipt` | Cancelled caller still gets an explained pending receipt; no replay. |
| R | `crates/engine/tests/l21_terminals.rs::detach_reconnect_close_and_saved_output` | Real PTY: detach/resume by seq, close kills only the shell, saved output after reopen. |
| R | `crates/engine/tests/l21_terminals.rs::shutdown_interrupts_history_and_task_delete_is_scoped` | Task delete scoped; shutdown marks Interrupted with engineExit. |
| R | `crates/engine/tests/local_first.rs::signed_out_workos_boot_serves_local_data_without_dev_identity` | Signed-out WorkOS boot is Local over RPC, local profile dir, no dev dir, no links. |
| D | `crates/engine/tests/local_first.rs::clean_local_auth_construction_does_not_probe_edge_health` | Detects: `build_auth` probing the edge. Stronger proof remains: `local_runtime_does_not_start_the_edge_updater` uses the same counting `rejecting_edge` and client id, asserts scope Local and zero requests after `build_auth` and runtime assembly. History: 98c2045c (Aug 8) predates the keeper 7ee12cb4 (Aug 10). Unlocks 16 lines. |
| R | `crates/engine/tests/local_first.rs::unconfigured_luna_auth_stays_local_and_does_not_load_a_saved_session` | OFFLINE_EDGE_URL: offline-only, ignores saved session and bearer, no links/updater. |
| R | `crates/engine/tests/local_first.rs::local_runtime_does_not_start_the_edge_updater` | Keeper: local runtime makes zero edge requests, no updater, no links. |
| R | `crates/engine/tests/local_first.rs::revoked_captured_session_stays_on_its_synced_cache` | Revoked session stays on its captured synced profile. |
| R | `crates/engine/tests/local_first.rs::transient_refresh_failure_keeps_synced_recovery_supervisors_alive` | Network errors are not revocation; links kept; no updater for fixtures. |
| R | `crates/engine/tests/local_first.rs::workspace_recovers_from_an_unreachable_edge_without_restarting` | Registry supervisor survives temporary token failure and reconnects. |
| R | `crates/engine/tests/local_first.rs::development_without_an_explicit_bearer_stays_offline` | Development without bearer stays offline. |
| R | `crates/engine/tests/local_first.rs::explicit_dev_bearer_keeps_online_routing_enabled` | Explicit dev bearer enables links. |
| R | `crates/engine/tests/local_first.rs::headless_stop_rpc_drains_the_daemon_and_releases_ipc` | StopEngine drains the daemon and frees the IPC port. |
| R | `crates/engine/tests/local_first.rs::headless_sign_out_closes_joined_edge_rooms_and_stops_daemon` | SignOut replies, closes edge sockets, stops the daemon. |
| R | `crates/engine/tests/local_first.rs::online_runtime_shutdown_stops_edge_workers_and_retires_the_graph` | Shutdown stops edge workers and frees the engine graph. |
| R | `crates/engine/tests/local_import.rs::local_work_imports_into_synced_profile_once` | Import rows with chat2 lineage, verbatim doc bytes, journals, marker grant, idempotent second run. |
| R | `crates/engine/tests/local_import.rs::import_without_local_profile_is_a_clean_no_op` | No local profile -> clean empty summary. |
| R | `crates/engine/tests/local_import.rs::per_item_failures_surface_in_the_summary_and_leave_the_row_retryable` | Per-item failure reported; failed row left for retry. |
| R | `crates/engine/tests/local_import.rs::corrupt_marker_is_moved_aside_not_clobbered` | Corrupt marker moved aside, grant rebuilt. |
| R | `crates/engine/tests/local_import.rs::marker_persistence_failure_is_an_import_error` | Marker write failure reported. |
| R | `crates/engine/tests/local_import.rs::spaces_only_profile_imports_its_spaces` | Spaces-only profile imports spaces. |
| R | `crates/engine/tests/local_import.rs::marker_keeps_one_entry_per_account` | Per-account marker entries coexist. |
| R | `crates/engine/tests/local_import.rs::later_local_work_imports_as_a_delta` | Later local work imports as a delta. |
| R | `crates/engine/tests/local_profiles.rs::resolved_profile_ids_separate_accounts_on_one_device_and_survive_restart` | Profile ids differ per account, stable across restart, 64-hex. Routed by ui-tests.yml. |
| R | `crates/engine/tests/local_profiles.rs::concurrent_engine_info_and_runtime_share_one_device_identity` | Keeper table (fresh root, truncated legacy `device-id`): 32 concurrent `engine_info` calls publish one non-empty persisted identity used by the runtime. |
| C | `crates/engine/tests/local_profiles.rs::empty_legacy_device_identity_is_repaired_once_for_all_boots` | Near-duplicate of the test above with a pre-seeded empty `device-id`. Folded as the second row of `concurrent_engine_info_and_runtime_share_one_device_identity` (non-empty check applied to both rows). Unlocks 18 lines. |
| R | `crates/engine/tests/local_profiles.rs::local_and_synced_profiles_remain_isolated_across_restarts` | Local/synced isolation of chats, docs and uploads across restarts; local identity not rewritten. |
| R | `crates/engine/tests/local_profiles.rs::synced_accounts_isolate_uploads_and_assign_the_legacy_cache_once` | Per-account upload jails; legacy cache owned once. |
| R | `crates/engine/tests/local_profiles.rs::existing_session_opens_the_historical_cloud_layout_in_place` | Existing session resolves the historical orgs/ layout without migration. |
| R | `crates/engine/tests/local_profiles.rs::signing_in_does_not_activate_sync_for_the_running_local_profile` | Sign-in during a local runtime performs only the exchange; no online writes. |

## Layer plan

The lane is already almost entirely made of keepers at the preferred boundary:
real `EngineCore` + in-memory RPC + SQLite/Loro + real Git repos + scripted
provider peers from `crates/harness/tests/fixtures` (Claude, Codex, OpenCode
peers, fake `gh`, stub edge HTTP/WebSocket servers). No layer in this lane
replays a shared helper through one fake for several providers: the three
board-tools binaries share a scenario but drive three different adapters and
peers. The redundancy found is local (duplicate declarations inside a suite)
plus three ignored tests that no longer run.

Keeper per contract (only where this lane changed something or overlaps
exist):

| Contract | Keeper | Retired / changed |
|---|---|---|
| Refresh backoff, no revocation on transport failure | `auth.rs::twenty_consumers_and_background_loop_share_a_failed_refresh`, `auth.rs::refresh_recovers_after_cooldown_without_a_network_event` | `offline_refresh_loop_backs_off_without_revoking_session` (D) |
| Backup control over local RPC with production wiring | `backup_control.rs::local_rpc_reports_copy_only_status_and_rejects_foreign_target` (now through `IsolatedRoot::open(data_dir)`) | seam `EngineCore::with_backup_control_for_tests` unused (cross-lane L01) |
| Managed offline backup refuses missing required tables | `backup_restore.rs::managed_offline_backup_refuses_missing_required_tables` (table) | `..._missing_v12_authority_table` folded as a row (C) |
| No extra approval card for app coordination | panic on `InputRequested` in `claude_luna_tools.rs` and `cross_task_tools.rs::fake_codex_tool_uses_real_durable_queue_without_extra_approval` | self-comparing `approvals` counters removed (F) |
| Relay forwarding (unary + streams) | `device_routing.rs::terminal_stream_proxies_over_the_relay`, `checkout_change_request_stream_matches_locally_and_through_device_routing`, `unsupported_remote_change_request_watch_keeps_the_shared_device_link` | ignored `target_device_id_routes_over_the_relay` (D) |
| Remote workspace file reads (`files-read`) | `device_routing.rs::workspace_file_surface_proxies_over_the_relay` (re-enabled, write step dropped) | — |
| Remote mutation denial (L29) | `remote_auth.rs::two_clients_pair_scope_revoke_stream_and_restart` + `luna_rpc::remote_auth::tests::policy_inventory_is_complete_and_unknown_fails_closed` (other lanes) | ignored `queue_watch_and_single_consumption_route_to_the_remote_chat_host` (D) |
| Queue row consumed once | `message_queue.rs::concurrent_drains_release_one_message` (other lane) | same |
| Crash-window command never executes, retry mints one attempt | `e2e.rs::retry_reissues_a_swallowed_send` (+ "no session started") | `processed_commands_are_skipped_on_redelivery` (C) |
| Pure command disposition Skip | `luna_doc::commands::tests::processed_commands_are_skipped` (doc lane) | crate-local replay removed with the C above |
| Pending question answered / wrong id rejected | `e2e.rs::wrong_id_respond_is_rejected_and_correct_answer_still_resumes` (+ Applied, resolved part, waits for Idle) | `respond_input_resolves_pending_question` (C) |
| RespondInput cannot bypass a typed approval | `e2e.rs::typed_live_approval_rejects_respond_input_and_accepts_only_respond_approval` | duplicate block in `structured_approval_preserves_details_and_rejects_unadvertised_choice` (F) |
| `build_auth` / local runtime make no edge requests | `local_first.rs::local_runtime_does_not_start_the_edge_updater` | `clean_local_auth_construction_does_not_probe_edge_health` (D) |
| One device identity for concurrent boots (fresh and truncated legacy) | `local_profiles.rs::concurrent_engine_info_and_runtime_share_one_device_identity` (2-row table) | `empty_legacy_device_identity_is_repaired_once_for_all_boots` (C) |
| E24 foreign Claude result | `claude_turn_identity.rs::e24_...` | `claude_cc06_e2e.rs::foreign_claude_result_does_not_complete_the_human_turn` retained: different peer drives the replay-receipt path |

Assertions carried into keepers: "no session started" (e2e retry test);
"answer command Applied" + "Input part resolved" + "wait for Idle" (e2e
wrong-id test); `local_executable_authority` row (backup table); non-empty
identity applied to both rows (local_profiles table).

Seams removed in owned files: none existed. Seam unlocked in another lane:
`EngineCore::with_backup_control_for_tests` (`crates/engine/src/lib.rs`).

Seams this lane still depends on (L01 must keep them, or move these tests to
another boundary first): `ChildThreads::seed_child_for_qa`
(`agent_delegation_background`, `agent_workspace_controls`,
`board_chat_model_e2e`, `claude_child_chats`); `board_qa_reconcile`,
`board_qa_mark_done`, `board_qa_call_as_agent`, `board_lifecycle_tick`
(`board_lifecycle_e2e`); `BackupService::verify_staged_for_tests`,
`stage_restore_with_available_bytes_for_tests`,
`activate_with_readback_for_tests`,
`restore_managed_offline_with_readback_for_tests` (`backup_restore`,
`board_store_e2e`); `AuthConfig::use_in_memory_credentials_for_tests`,
`test_credential_broker` (`auth`, `local_profiles`);
`TaskIdentity::preview_import_observation_for_tests`
(`board_authoring_e2e`).

Retired files: none. `Cargo.toml` has no `[[test]]` entries; no target names
changed.

## Cross-lane requests

1. **L01 (`crates/engine/src/lib.rs`)**: delete
   `EngineCore::with_backup_control_for_tests` (the `#[doc(hidden)]` builder
   around line 771). After this lane `rg with_backup_control_for_tests` finds
   no caller; production already sets `backup_control` from
   `backup::IsolatedRoot::open(data_dir)`.
2. **L01 (`crates/engine/src/auth.rs` inline tests)**: optional review only.
   `auth::tests::concurrent_dns_failure_is_shared_with_http_and_room_consumers`
   overlaps `crates/engine/tests/auth.rs::twenty_consumers_and_background_loop_share_a_failed_refresh`
   and `temporary_token_errors_reach_http_and_websocket_consumers` except for
   DNS-failure classification through the `http_error::test_support::FailingDns`
   seam. Keep or fold at L01's discretion.
3. **Keepers owned by other lanes that this lane's deletions rely on** (please
   retain): `crates/engine/tests/message_queue.rs::concurrent_drains_release_one_message`,
   `crates/engine/tests/remote_auth.rs::two_clients_pair_scope_revoke_stream_and_restart`,
   `crates/rpc/src/remote_auth.rs::tests::policy_inventory_is_complete_and_unknown_fails_closed`, and `crates/doc/src/commands.rs::tests::processed_commands_are_skipped`.
4. **L01 seams listed under the layer plan**: do not delete without first
   moving the listed tests.
5. **CI workflows**: no routing change needed. `ui-tests.yml` (`--test
   codex_subagents --test local_profiles`) and `windows.yml` (`--test
   codex_catalog --test codex_login_resolution`, `--test auth`) name targets
   that still exist; no name filters reference removed tests.

## Possible product defects

- None confirmed (nothing was run in this lane).
- `device_routing.rs::workspace_file_surface_proxies_over_the_relay` had not
  run since 2026-09-24. If it fails after re-enabling, remote (`files-read`)
  workspace browsing over the relay has regressed, or a read method's policy
  changed; treat it as a defect report, not a stale test.
- `board_tools_support::board_tools_scenario` accepts "any structured board
  error" for `board_execution_options`, `board_move`, `board_control stop`,
  `board_convert_chat`, `BoardReviewStart` and `BoardAuthoringStart`
  ("slice not landed" fallbacks). `BoardError::not_implemented` is no longer
  constructed anywhere in `crates/engine/src`, so if any provider run takes a
  fallback today, a real board-tool failure is passing silently. Check the
  `implemented`/`skipped` fields in
  `target/qa/board/tools/{claude,codex,opencode}.json` after the integrated
  run.

## Follow-ups

- Tighten `crates/engine/tests/board_tools_support/mod.rs` once the evidence
  above shows `implemented: true` for every provider: make steps 9-11 require
  a receipt, make the convert/review/authoring branches require success, and
  drop the dead `notImplemented` alternatives for the sibling and handoff
  steps (`BoardError::not_implemented` is never constructed). Left as-is here
  because the lane cannot run the three providers to confirm which branches
  are taken.
- `backup_restore.rs::managed_offline_backup_refuses_missing_required_tables`
  has no `prompt_stash` row although `database_counts` requires it from schema
  16; add the row in an authoring pass (new assertion, out of scope for a
  pruning lane).
- `context_transfer.rs::context_transfer_preview_commit_delivery_restart_and_delta`
  asserts `delivered|consumed` and then `== "consumed"`; the first assertion is
  redundant (harmless, left untouched).
- `automations_e2e.rs::calendar_preview_skips_gap_and_chooses_first_fold`
  assembles an engine it never uses; the DST assertions only need
  `automation::preview_times` (cosmetic).
- `e2e.rs` writes `save_input_audit_evidence` into the system temp dir even
  without `LUNA_AUDIT_EVIDENCE_DIR`; consider gating it like the other
  evidence writers.
