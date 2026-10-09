# L03 — engine integration suites m–z

- **Lane:** L03-engine-it-m-z (test-audit campaign 2026-10-09, branch `test-audit/L03-engine-it-m-z`).
- **Scope:** `crates/engine/tests/[m-z]*.rs` (48 suites) and `crates/engine/tests/support/workspace_identity_repair_contract.rs`, as listed in the lane manifest. Production owners were read in `crates/engine/src` (L01) and `crates/doc`, `crates/proto`, `crates/sync` for overlap only.
- **Baseline (pinned `e4a0f534`):** 318 test declarations in 49 files, 39,497 lines (test code and in-file fixtures; there is no separate support crate beyond the one `support/` module).
- **After cutover:** 310 declarations, 48 files, 38,977 lines (−520; `relay_delivery.rs` −342).
- **Marks:** R 308 · F 2 · C 3 · D 5.
- **CI routing:** `.github/workflows/ui-tests.yml` runs `--test session_publication --test restart_resume` by target name only; both targets remain, so no routing change. No other workflow names a target in this lane. No `[[test]]` entries exist in `crates/engine/Cargo.toml`.
- **Method:** every declaration was read in full, with its production owner, overlapping suites (other engine suites, `crates/engine/src` inline tests, `crates/doc` registry tests, `crates/proto`), git history (`git log -S`) where the reason was unclear, and workflow/script references. Nothing was compiled or run in this lane; the coordinator owns builds and tests.

Most of this lane is the keeper layer the campaign prefers: real Git repositories in temp dirs, real Loro documents and SQLite (with trigger-injected storage faults), real Codex/Claude adapters against the scripted peers in `crates/harness/tests/fixtures`, and two-engine registry rooms. Deletions are limited to tests whose contract has a stronger remaining owner or no contract at all.

## Ledger

| Mark | Test | Evidence |
| --- | --- | --- |
| R | `crates/engine/tests/m5_repos_diffs_terminals.rs::repos_round_trip_add_branches_worktrees` | Sole owner of `Repos` add/dedupe/non-repo reject, default-branch-first, worktree create under the worktrees root + refs (current/worktree_path) + distinct checkout identity + delete removes the `luna/` branch, repo create sanitize/duplicate reject; real git. `repos.rs` inline tests cover registry snapshots only. |
| R | `crates/engine/tests/m5_repos_diffs_terminals.rs::git_history_is_topological_paged_and_carries_public_refs` | History paging (cursor, first-page-only totals), merge parents, branch/remote/tag refs on a real repo; inline `history_compaction_*`/unicode tests are different predicates. |
| R | `crates/engine/tests/m5_repos_diffs_terminals.rs::git_history_reports_ahead_and_behind_against_integration_branch` | Only proof of the integration comparison (`upstream/main`, ahead/behind). |
| R | `crates/engine/tests/m5_repos_diffs_terminals.rs::git_history_search_is_fuzzy_complete_and_accepts_sha_prefixes` | End-to-end fuzzy history search + SHA-prefix lookup; inline `git_history_matches_unicode_case_insensitively` only covers the matcher. |
| R | `crates/engine/tests/m5_repos_diffs_terminals.rs::fetch_all_updates_only_remote_tracking_refs` | Safety contract: fetch-all never touches HEAD, branch or index/untracked state. |
| R | `crates/engine/tests/m5_repos_diffs_terminals.rs::folder_lister_flags_and_ordering` | ListFolders ordering (dirs first, dotfiles hidden) and is_dir/is_repo flags; RPC test only checks the array exists. |
| R | `crates/engine/tests/m5_repos_diffs_terminals.rs::folder_lister_caps_at_500_with_truncated_flag` | `FOLDER_LIST_MAX_ENTRIES` cap + truncated flag. |
| R | `crates/engine/tests/m5_repos_diffs_terminals.rs::folder_lister_timeout_path` | Sole proof a wedged listing fails by timeout instead of hanging. Needs the `#[doc(hidden)] list_folders_with(.., hang_for_test)` seam (repos.rs, L01); no real-boundary wedge exists, so kept (follow-up). |
| R | `crates/engine/tests/m5_repos_diffs_terminals.rs::diff_capture_tracked_untracked_and_checksum` | Working-tree diff: tracked + untracked hunks, numstat, stable/changed checksum. `diff_sync.rs` inline tests cover watch budgets only. |
| R | `crates/engine/tests/m5_repos_diffs_terminals.rs::diff_capture_against_merge_base_shows_branch_changes` | Branch diff vs merge-base; unknown ref errors instead of silently falling back. |
| R | `crates/engine/tests/m5_repos_diffs_terminals.rs::diff_file_text_returns_both_checked_sources` | Old/new source pair with hashes; `../` path escape rejected (security). |
| R | `crates/engine/tests/m5_repos_diffs_terminals.rs::commit_diff_captures_one_commit_and_roots_diff_the_empty_tree` | Commit diff excludes working tree; root commit diffs against the empty tree. |
| R | `crates/engine/tests/m5_repos_diffs_terminals.rs::turn_diff_captures_only_changes_since_snapshot` | Turn diff excludes pre-turn tracked edits and pre-existing untracked files. |
| R | `crates/engine/tests/m5_repos_diffs_terminals.rs::diff_capture_truncates_at_patch_cap` | Patch cap + truncation marker. |
| R | `crates/engine/tests/m5_repos_diffs_terminals.rs::spaces_sync_stamps_git_presence_and_reacts_to_git_init` | `spaces.rs` has no tests; sole proof the owner corrects a lying git flag and reacts to `git init`. |
| D | `crates/engine/tests/m5_repos_diffs_terminals.rs::delete_space_cascades_chats_and_sessions` | Exercises `WorkspaceHost::delete_space`, a wrapper with no production caller (production deletes via `delete_space_if_chats`, rpc.rs:2895); asserts no session despite its name; the device_id check cannot distinguish inheritance (space device == engine device). Cascade owners: crates/doc/src/workspace.rs + registry/tests.rs delete-space tests (cross-replica) and `task_commands::project_delete_requires_exact_children_and_preserves_workspace_files` (real RPC). Chat cwd inheritance from its space stays proven by `rpc_dispatch_for_m5_methods` (SearchFiles by chat). Unlocks: with this lane's two callers migrated, one rpc.rs inline test is the wrapper's last user. |
| R | `crates/engine/tests/m5_repos_diffs_terminals.rs::diff_sync_publishes_without_rewriting_chat_branch` | Branch identity is conversation-owned (watcher must not relabel chat rows) + watcher republish within the repair interval. |
| R | `crates/engine/tests/m5_repos_diffs_terminals.rs::checkout_file_diff_text_rpc_fits_the_default_worker_stack` | Regression for 1ef4aca7 (dispatcher async stack overflow on a default worker stack). |
| R | `crates/engine/tests/m5_repos_diffs_terminals.rs::checkout_file_diff_text_rpc_reads_pinned_commit_sources` | Commit-mode source pair ignores live edits (d903bd73). |
| R | `crates/engine/tests/m5_repos_diffs_terminals.rs::terminal_e2e_replay_live_resize_exit` | Unix PTY: exit code on live streams, stream end, post-exit full replay from seq 1, close removes a history-less terminal. `l21_terminals.rs` covers the history-backed variant; `terminals.rs` inline tests are Windows ConPTY only. |
| R | `crates/engine/tests/m5_repos_diffs_terminals.rs::terminal_guards_input_size_and_cwd` | Only Unix proof of `MAX_INPUT_BYTES` and bad-cwd refusal. |
| R | `crates/engine/tests/m5_repos_diffs_terminals.rs::rpc_dispatch_for_m5_methods` | Wire routing for repo/worktree/folder/search/terminal methods + SearchFiles root-id and cwd-escape guards. |
| R | `crates/engine/tests/m5c_accounts_uploads_titles.rs::claude_provider_login_is_discovered_with_stable_native_identity` | Native Claude discovery: stable id, plan label, no LUNA slot dir, reconnect leaves credential bytes untouched. |
| R | `crates/engine/tests/m5c_accounts_uploads_titles.rs::claude_reconnect_leaves_live_provider_and_mcp_credentials_untouched` | Keeper for "reconnect never rewrites live credentials" (accessToken, device token, MCP OAuth, plugin secrets with stale Alice material present). |
| D | `crates/engine/tests/m5c_accounts_uploads_titles.rs::claude_reconnect_never_rewrites_live_shared_credentials` | Strict subset of `claude_reconnect_leaves_live_provider_and_mcp_credentials_untouched`: same Alice list -> Bob live + MCP OAuth -> list -> activate Alice -> read creds, asserting only accessToken + one MCP token (keeper also asserts trustedDeviceToken and pluginSecrets, with stale Alice MCP present). Both added in 4a290fdd. No seam. |
| R | `crates/engine/tests/m5c_accounts_uploads_titles.rs::codex_discovery_is_native_only_and_api_keys_are_redacted` | Codex native-only identity and API-key redaction from the serialized snapshot (security). |
| R | `crates/engine/tests/m5c_accounts_uploads_titles.rs::disconnect_preserves_provider_and_legacy_credentials` | Path-shaped ids never reach the filesystem; legacy slot artifacts preserved and refused. (Inline `agent_accounts::claude_disconnect_persists_without_touching_native_login` is narrower.) |
| D | `crates/engine/tests/m5c_accounts_uploads_titles.rs::snapshot_wire_shape` | Serializes `AgentAccountsSnapshot::default()` and asserts `{accounts:[],warnings:[]}`: restates a plain derive (single-word fields, `rename_all` is a no-op). Only consumers are Rust UI modules deserializing the same type (ui/src/settings/accounts.rs, pickers.rs, harnesses.rs); no iOS/edge consumer (rg). Populated snapshots are serialized by `codex_discovery_is_native_only_and_api_keys_are_redacted`. |
| C | `crates/engine/tests/m5c_accounts_uploads_titles.rs::claude_login_flow_is_pkce_paste_code` | `rpc_dispatch_for_m5c_methods` already drives start(paste-code) -> poll pending -> cancel -> poll error -> complete error over the wire. Carried into it: authorize URL prefix, `code_challenge_method=S256`, console redirect_uri, and "a cancelled login cannot be completed". |
| R | `crates/engine/tests/m5c_accounts_uploads_titles.rs::uploads_chunk_commit_readback_and_jail` | Keeper for chunked staging (out of order, idempotent retry, hole detection), readback, path jail, image-only reads, bogus ids. (Inline `uploads::commit_assembles_chunks_into_a_durable_file` overlaps.) |
| R | `crates/engine/tests/m5c_accounts_uploads_titles.rs::titling_e2e_names_chat_and_renames_worktree_branch` | Auto-title -> worktree branch rename E2E. Follow-up: the "never re-titled" negative relies on a 400 ms sleep window. |
| R | `crates/engine/tests/m5c_accounts_uploads_titles.rs::rename_worktree_branch_guards_and_collisions` | Rename guards, hash-suffix collisions, slug edge-case table (pure slug function, one table). |
| R | `crates/engine/tests/m5c_accounts_uploads_titles.rs::rpc_dispatch_for_m5c_methods` | Wire keeper: uploads + jail over RPC, remote credential refusal, Claude PKCE login lifecycle (now incl. URL contract and cancelled completion), junk ids. |
| R | `crates/engine/tests/message_queue.rs::queue_drains_immediately_when_the_agent_is_idle` | Idle drain; queue id becomes the transcript message id. Keeper suite for `DocHost` queue policy (no doc_host.rs/crates/doc overlap). |
| R | `crates/engine/tests/message_queue.rs::queue_holds_during_a_turn_and_flushes_in_order_at_its_end` | Only terminal Done releases Queue rows; FIFO order; ids stable. |
| R | `crates/engine/tests/message_queue.rs::restart_restores_the_queue_frozen_until_an_explicit_send` | Restart never dispatches a recovered queue; explicit send thaws. |
| R | `crates/engine/tests/message_queue.rs::restart_marks_an_unresolved_claim_unknown_without_replaying_it` | Claimed row -> DeliveryUnknown on restart, no replay. |
| R | `crates/engine/tests/message_queue.rs::legacy_queue_edits_preserve_claimed_delivery_evidence_across_restart` | Legacy UPDATE/MOVE RPCs refuse claimed/accepted/unknown rows; held row still editable. |
| R | `crates/engine/tests/message_queue.rs::restored_queue_claim_cannot_change_an_unrelated_command_receipt` | Opening a recovered queue leaves unrelated receipts byte-equal (table of 3 mismatches). |
| R | `crates/engine/tests/message_queue.rs::restart_removes_a_claim_whose_provider_receipt_was_already_applied` | Applied correlation settles a stale claimed projection; delivered once. |
| R | `crates/engine/tests/message_queue.rs::cancelling_a_turn_freezes_the_queue_until_an_explicit_send` | Cancel freezes the queue across edits; each explicit send selects one row. |
| R | `crates/engine/tests/message_queue.rs::a_steer_command_holds_for_an_agent_that_takes_no_mid_turn_prompt` | 2026-08-13 report regression (steer posted into a turn-boundary mailbox). |
| R | `crates/engine/tests/message_queue.rs::queued_text_waits_for_a_steerable_turn_even_with_legacy_policy` | Omitted/false legacy policies still wait one turn at a time. |
| R | `crates/engine/tests/message_queue.rs::steer_preference_uses_native_acceptance_and_settles_the_same_row` | Native steer settles the same row id without a second run. |
| R | `crates/engine/tests/message_queue.rs::accepted_steer_without_incorporation_becomes_unknown_and_never_replays` | Accepted-only steer -> unknown; single-flight; survives restart. |
| R | `crates/engine/tests/message_queue.rs::rejected_steer_never_auto_drains_but_explicit_send_and_queue_still_work` | Provider-rejected steer stays at FIFO head across Done, notifications and restart. |
| R | `crates/engine/tests/message_queue.rs::persistent_done_releases_unconfirmed_steer_for_authorized_next_send` | Persistent stream: Done finalizes unknown steer; authorized next send reuses the stream once. |
| R | `crates/engine/tests/message_queue.rs::steer_now_leaves_the_row_when_the_agent_cannot_steer_mid_turn` | SteeringUnavailable hold; no downgrade at Done. |
| R | `crates/engine/tests/message_queue.rs::stale_steer_remains_queued_when_the_previous_turn_is_already_idle` | StaleTurn hold. |
| R | `crates/engine/tests/message_queue.rs::a_message_with_attachments_holds_even_for_a_steerable_agent` | Attachments never steer; trailer materialized only on dispatch. |
| R | `crates/engine/tests/message_queue.rs::send_now_interrupts_the_running_turn` | Send-now interrupts and takes exactly one row. |
| R | `crates/engine/tests/message_queue.rs::send_now_from_a_paused_queue_does_not_claim_a_sibling_on_idle` | Paused send-now does not release siblings. |
| R | `crates/engine/tests/message_queue.rs::editing_a_queued_message_to_empty_removes_it` | Empty edit is the delete gesture. |
| R | `crates/engine/tests/message_queue.rs::acknowledged_removal_cannot_materialize_after_turn_end` | Acknowledged removal serialized against drain. |
| R | `crates/engine/tests/message_queue.rs::queue_rpc_reorders_and_streams` | WatchQueue/Queue/Move/Remove over RPC. |
| R | `crates/engine/tests/message_queue.rs::explicit_queue_delivery_is_rejected_off_host_without_taking_the_row` | Non-host send/steer/remove fail without taking the row. |
| R | `crates/engine/tests/message_queue.rs::concurrent_drains_release_one_message` | Review regression: concurrent drains release one head. |
| R | `crates/engine/tests/message_queue.rs::a_message_holds_while_the_agent_waits_on_a_question` | AwaitingInput hold. |
| R | `crates/engine/tests/message_queue.rs::a_steer_request_stays_queued_while_approval_is_pending` | AwaitingApproval hold. |
| R | `crates/engine/tests/message_queue.rs::an_acquired_edit_blocks_turn_end_and_commit_sends_the_new_text` | Edit lease blocks Done promotion; commit sends new text. |
| R | `crates/engine/tests/message_queue.rs::only_one_editor_wins_and_an_expired_edit_requires_review` | Single editor; expired lease -> ReviewRequired, never send. |
| R | `crates/engine/tests/message_queue.rs::protected_edit_rpc_round_trips_its_camel_case_protocol` | Begin/Finish edit RPC wire contract. |
| R | `crates/engine/tests/message_queue.rs::composer_edit_commits_attachments_in_place_and_cancel_preserves_them` | Attachment edit in place; cancel preserves. |
| R | `crates/engine/tests/message_queue.rs::failed_queue_dispatch_stays_paused_until_explicit_retry` | Failed dispatch (no chat row) does not spin; explicit retry delivers. |
| R | `crates/engine/tests/message_queue.rs::queued_turn_uses_current_config_at_turn_end_and_send_now` | Table (Done / send-now): queued turn uses current config. |
| R | `crates/engine/tests/message_queue.rs::pending_execution_changes_hold_steer_until_explicit_send` | Table (selection / account): changed runtime holds native steer; GetRuntimeContract transition kinds. |
| R | `crates/engine/tests/message_queue.rs::queue_completion_markers_distinguish_normal_turns_from_interrupts` | Table: completion marker published only for natural completion. |
| R | `crates/engine/tests/model_cache_lifecycle.rs::retired_engines_stop_background_catalog_discovery` | Retired/dropped engine never probes providers and releases the registry (table: explicit/implicit shutdown); the 2.3 s window is the contract. |
| R | `crates/engine/tests/native_inputs.rs::draft_restart_queue_edit_and_native_image_steer_preserve_inputs` | Offline E2E through the real CodexHarness + native-inputs-codex.py peer: draft restart, inputItems edit semantics, localImage start/steer payloads. |
| R | `crates/engine/tests/native_inputs.rs::known_selected_startup_rejection_and_timeout_recover_frozen_snapshot_after_restart` | Table (rejected/timeout): frozen submission recoverable across restart, no turn/start. |
| R | `crates/engine/tests/native_inputs.rs::startup_rejection_preserves_unknown_or_accepted_receipts_and_does_not_replay` | Table (unknown/accepted): receipts unchanged, submission pending, no replay. |
| R | `crates/engine/tests/native_inputs.rs::recoverable_discard_clears_matching_native_inputs_but_keeps_newer_identity` | Table: discard clears the matching draft but keeps a newer one. |
| R | `crates/engine/tests/no_keychain.rs::claude_identity_without_secret_is_available_without_credential_verification` | Identity-only Claude login is usable with Unknown authentication; activate writes nothing. |
| R | `crates/engine/tests/no_keychain.rs::production_vault_survives_restart_and_two_handles_without_os_store` | Production file key store: 0600/0700 perms, restart, two handles, ciphertext only. |
| R | `crates/engine/tests/no_keychain.rs::missing_and_unsafe_vault_keys_preserve_existing_ciphertext` | Table (missing/short/insecure/symlink/hardlink key) fails closed without replacing keys. Keeper over inline `credentials::existing_ciphertext_with_a_missing_key_never_creates_a_replacement` (memory store). |
| R | `crates/engine/tests/no_keychain.rs::interrupted_first_key_write_is_recoverable_without_replacing_keys` | SIGXFSZ/RLIMIT_FSIZE child process leaves no partial key; recovery works. |
| R | `crates/engine/tests/no_keychain.rs::redirected_credential_directory_and_lock_do_not_touch_foreign_targets` | Symlinked dir / lock symlink / hardlink never modify foreign targets. |
| R | `crates/engine/tests/pane_chat.rs::manual_children_provision_checkout_access_and_parent_defaults` | Child provisioning matrix (legacy defaults, smuggled authority, invalid branch table creates nothing, worktree/named/retry, grant above ceiling, re-cap on parent lowering, oversized rollback, non-git). Uses the shared `seed_child_for_qa` seam. |
| R | `crates/engine/tests/pane_chat.rs::snapshot_side_context_survives_goals_reviews_and_rejected_first_send` | Table Codex/Claude x goal/review: frozen parent context delivered outside the prompt; rejected first send leaves no echo. |
| R | `crates/engine/tests/pane_chat.rs::codex_side_chat_forks_inherits_and_runs_independently` | Row (Codex, auto) of the native-fork side-chat scenario; Codex rows exercise the metadata app-server history check. |
| R | `crates/engine/tests/pane_chat.rs::codex_full_side_chat_inherits_effective_access_with_legacy_sandbox` | Row (Codex, full + legacy workspace-write sandbox): effective Full is inherited. |
| R | `crates/engine/tests/pane_chat.rs::claude_side_chat_forks_inherits_and_runs_independently` | Row (Claude): different engine branches (harness native_history_turns, ClaudeMcpTools delivery), not one fake claiming provider behavior. |
| R | `crates/engine/tests/pane_chat.rs::codex_side_chat_ordinary_queue_steer_stop_attachments_and_model` | Row (Codex) of side-chat ordinary commands (queue, steer, Stop, attachments, model/access, legacy send, view close, nested fork refusal, source deletion). |
| R | `crates/engine/tests/pane_chat.rs::claude_side_chat_ordinary_queue_steer_stop_attachments_and_model` | Row (Claude) of the same scenario. |
| F | `crates/engine/tests/pane_chat.rs::legacy_child_rows_contracts_and_requests_decode_unchanged` | Storage/wire compatibility for legacy child rows, receipts, runtime contexts and requests. Repaired: `if let Ok(receipt) = from_value::<ChildThreadReceipt>(..)` silently skipped the assertion when the legacy receipt stopped decoding; now `.expect(..)` (ChildThreadReceipt/CommandReceipt have no deny_unknown_fields and every required field is present). |
| R | `crates/engine/tests/pane_chat.rs::child_execution_selects_provider_model_options_and_keeps_fresh_history_after_restart` | Cross-provider child execution selection, no parent history copied, retry idempotent, restart. |
| R | `crates/engine/tests/pane_chat.rs::child_execution_rejects_invalid_choices_before_identity_or_worktree_creation` | Invalid provider/model/reasoning/options table rejected before any identity or worktree. |
| R | `crates/engine/tests/pane_chat.rs::blank_child_provider_choice_stays_unbound_and_independent_until_first_send` | Initial provider choice is not a context reset; started child requires an explicit boundary. |
| R | `crates/engine/tests/pane_chat.rs::blank_child_tabs_preserve_drafts_and_running_sessions_across_view_changes` | Blank tabs never start; view changes never stop turns; forged create fields rejected. |
| R | `crates/engine/tests/pane_chat_mcp.rs::codex_create_thread_schema_grant_cap_deny_and_child_identity` | Real CodexHarness + fake-codex.sh MCP create_thread: schema, grant/cap/deny/forged index, provisioning only after approval, child role instructions. |
| R | `crates/engine/tests/pane_chat_mcp.rs::claude_create_thread_schema_grant_cap_deny_and_child_identity` | Real ClaudeHarness + fake-claude-luna.py plan: same contract on the Claude transport. |
| R | `crates/engine/tests/previews.rs::preview_watch_follows_the_session_checkout_and_owning_device` | WatchPreviews follows chat cwd and owning device; removal empties. |
| R | `crates/engine/tests/project_commands.rs::checkout_commands_and_setup_lifecycle` | Real PTY project commands per checkout, failure/retry/stale completion, survivor process group, stop, setup dedupe/gate, restart. (project_commands.rs has no inline tests.) |
| R | `crates/engine/tests/project_commands.rs::generation_ownership_deletion_and_backup` | Checkout generations, foreign device refusal, backup/restore never launches commands. Setup migrated from the test-only `WorkspaceHost::delete_space` to production `delete_space_if_chats("p", ["a"])`. |
| R | `crates/engine/tests/project_commands.rs::durable_task_run_waits_for_setup_and_records_failure` | Legacy SessionCommand path: run rejected behind failed setup, retry, second run applied. |
| R | `crates/engine/tests/project_commands.rs::pending_setup_survives_both_checkout_publication_boundaries` | Pending setup across publication boundaries; restart cancels without replay. |
| R | `crates/engine/tests/project_commands.rs::stale_project_command_editor_cannot_clobber_another_client` | CAS regression for SaveProjectCommands. |
| R | `crates/engine/tests/project_commands_context.rs::uuid_project_move_uses_current_action_and_preserves_task_cwd` | After moveProject, the destination's action runs in the unchanged task cwd; old receipts fenced. |
| R | `crates/engine/tests/project_commands_context.rs::uuid_materialization_dependency_uses_real_run_path` | Only proof an explicit project action after UUID run-with-worktree materialization targets the materialized cwd (identity asserts overlap `workspace_identity_repair::new_worktree_materialization_keeps_sibling_root_and_initial_binding`; follow-up to trim them). |
| R | `crates/engine/tests/project_commands_worktree_setup.rs::uuid_worktree_setup_fail_restart_retry_and_continue` | UUID path: metadata durable before setup shell runs, failed setup gates provider, restart no replay, duplicate submit refused, retry, follow-up in the worktree. |
| R | `crates/engine/tests/project_defaults.rs::project_defaults_rpc_persistence_clearing_and_existing_task_preservation` | Validation table, CAS, restart persistence, clearing, existing tasks untouched. |
| R | `crates/engine/tests/project_defaults.rs::project_defaults_old_space_json_inherits` | Migration: legacy space JSON without executionDefaults inherits; explicit null rejected. |
| R | `crates/engine/tests/prompt_stash.rs::stash_survives_source_clear_restart_and_shared_owner_release` | Stash idempotency/conflict, jail, unsupported contexts, attachment ownership refcount, restart, NeedsReattachment, 50-entry cap. |
| R | `crates/engine/tests/provider_scope_e2e.rs::only_three_providers_run_and_retired_luna_history_is_deleted` | Product scope (three runnable providers), last-provider toggle, retired task + snapshot cleanup, Codex disconnect persists across reopen. |
| R | `crates/engine/tests/queue_preflight.rs::local_rejection_survives_restart_and_only_explicit_retry_delivers_once` | executionUnavailable hold survives restart/edits; explicit retry delivers once. |
| R | `crates/engine/tests/queue_preflight.rs::rejected_send_now_preserves_the_running_turn` | Rejected send-now never interrupts the active turn. |
| R | `crates/engine/tests/queue_preflight.rs::provider_entry_error_stays_unknown_across_restart` | Provider-entry error stays claimed/unknown; one provider call. |
| R | `crates/engine/tests/queue_preflight.rs::rejected_receipt_transaction_failure_never_releases_a_claim_for_replay` | SQLite-trigger rejection commit failure never releases the claim. |
| R | `crates/engine/tests/queued_attachments.rs::run_defers_until_attachment_bytes_land_then_executes_rewritten` | 2026-08-19 incident: pending:// refs defer then rewrite to committed paths. |
| R | `crates/engine/tests/recovery.rs::ordinary_fork_recovery_preserves_frozen_run_and_binding_across_restart` | Table (rejected/accepted): fork recovery held across restart; resend vs continue. Follow-up: `if let Some(before)` skips binding asserts when no pre-restart binding. |
| R | `crates/engine/tests/recovery.rs::confirmed_rejection_resends_the_frozen_request_once` | Rejected-before-acceptance resends the frozen request exactly once. |
| R | `crates/engine/tests/recovery.rs::unchanged_task_defaults_allow_frozen_recovery_before_and_after_restart` | Table (restart): inherited defaults do not fence recovery. |
| R | `crates/engine/tests/recovery.rs::changing_task_config_to_inherited_defaults_still_fences_recovery` | A synced config change supersedes recovery. |
| R | `crates/engine/tests/recovery.rs::accepted_failure_continues_same_session_without_repeating_effects` | Accepted failure continues the session without replaying effects. |
| R | `crates/engine/tests/recovery.rs::explicit_stop_survives_restart_without_recovery_or_queue_release` | Stop never becomes auto-retry; queue held. |
| R | `crates/engine/tests/recovery.rs::explicit_new_original_after_stop_is_not_suppressed` | A new explicit run after Stop dispatches. |
| R | `crates/engine/tests/recovery.rs::accepted_original_is_never_downgraded_when_continuation_is_rejected` | Frozen original survives a rejected continuation. |
| R | `crates/engine/tests/recovery.rs::ambiguous_continuation_start_checks_delivery_and_cannot_resend_original` | Unknown continuation -> observe-only; retry rejected. |
| R | `crates/engine/tests/recovery.rs::unknown_delivery_is_visible_and_never_replayed` | Unknown delivery: retry rejected, cancel applied. |
| R | `crates/engine/tests/recovery.rs::cancellation_and_chain_identity_survive_restart_without_replay` | Cancelled chain survives restart without replay. |
| R | `crates/engine/tests/recovery.rs::terminal_old_chain_does_not_mask_a_newer_interrupted_original_on_restart` | Table (accepted/unknown): newer original wins on restart. |
| R | `crates/engine/tests/recovery.rs::terminal_journal_failure_without_a_recovery_row_is_visible_after_restart` | Journal-only terminal failure becomes visible recovery. |
| R | `crates/engine/tests/recovery.rs::terminal_sweep_does_not_borrow_an_older_segment_for_a_newer_pending_run` | Segment attribution regression. |
| R | `crates/engine/tests/recovery.rs::restart_after_earliest_attempt_claim_preserves_chain_budget_and_does_not_dispatch` | Claimed-not-dispatched attempt reconciles to CheckingDelivery. |
| R | `crates/engine/tests/recovery.rs::restart_during_accepted_attempt_continues_same_chain_without_replaying_input` | Accepted attempt -> RecoveryNeeded/ContinueSession. |
| R | `crates/engine/tests/recovery.rs::newer_user_activity_supersedes_the_scheduled_chain` | Newer activity supersedes; timer cancelled. |
| R | `crates/engine/tests/recovery.rs::two_clients_cannot_claim_the_same_recovery_attempt` | One claim wins. |
| R | `crates/engine/tests/recovery.rs::duplicate_retry_after_lost_response_returns_the_receipt_without_another_dispatch` | Command-id idempotency. |
| R | `crates/engine/tests/recovery.rs::timer_claim_fences_a_late_cancellation_without_a_second_retry` | Timer claim fences cancel. |
| R | `crates/engine/tests/recovery.rs::configuration_commit_supersedes_a_pending_timer_before_dispatch` | Config commit supersedes timer. |
| R | `crates/engine/tests/recovery.rs::permission_grant_supersedes_a_pending_timer_before_dispatch` | Permission grant supersedes timer. |
| R | `crates/engine/tests/recovery.rs::overdue_in_process_timer_expires_without_dispatching` | Overdue deadline expires without dispatch. |
| R | `crates/engine/tests/recovery.rs::stale_timer_snapshot_cannot_claim_a_rescheduled_attempt` | Stale timer snapshot cannot claim. |
| R | `crates/engine/tests/recovery.rs::recovery_transition_storage_failure_rolls_back_receipt_and_reopens_safely` | SQLite-trigger transition failure rolls back receipt. |
| R | `crates/engine/tests/recovery.rs::stop_recovery_storage_failure_never_acknowledges_without_a_durable_hold` | Stop never acknowledged without a durable hold. |
| R | `crates/engine/tests/recovery.rs::recovery_storage_failure_rolls_back_registry_projection_and_receipt_together` | Registry projection and receipt roll back together. |
| R | `crates/engine/tests/recovery.rs::legacy_side_stop_holds_ordinary_fork_recovery_across_restart` | Compatibility StopSideChat holds fork recovery across restart. |
| D | `crates/engine/tests/relay_delivery.rs::rows_dark_command_delivers_over_the_peer_relay_exactly_once` | `#[ignore]` since 2c3b28a6 because L29 deliberately rejects unauthenticated legacy relay delivery; no workflow or script runs it with --ignored (rg); it cannot pass and protects nothing. Superseding owners: `remote_auth::two_clients_pair_scope_revoke_stream_and_restart` (RELAY_COMMAND/QUEUE_COMMAND remote calls are "local only") and `remote_doc_authority::direct_relay_ingest_cannot_bypass_owner_authority`. Its fake device room duplicates device_routing.rs; later edits (436da359, ed10a80f) were only RunRequest field upkeep. Whole file deleted (no [[test]] entry, workflow or doc reference). No seam: `ingest_relayed_command` and `start_host_relay` have production callers. |
| R | `crates/engine/tests/remote_auth.rs::two_clients_pair_scope_revoke_stream_and_restart` | L29 pairing, scopes, replay refusal, local-only writes, revoke ends streams, credentials never stored, restart. |
| R | `crates/engine/tests/remote_auth.rs::expired_session_and_invalid_reauthentication_cannot_reuse_connection` | Expiry and failed reauthentication drop the connection. |
| R | `crates/engine/tests/remote_auth.rs::corrupt_remote_authority_disables_remote_without_blocking_local_engine` | Corrupt authority file disables remote only. |
| R | `crates/engine/tests/remote_auth.rs::same_machine_different_profile_has_distinct_remote_authority` | Per-profile environment authority. |
| R | `crates/engine/tests/remote_auth.rs::mcp_authorization_urls_are_owner_only_even_with_remote_read_scopes` | MCP elicitation URLs are owner-only (security). |
| R | `crates/engine/tests/remote_doc_authority.rs::foreign_rows_cannot_run_from_an_open_chat_or_replay` | Foreign rows through the real EngineChatSink (row/replay/checkpoint/restart) never run. |
| R | `crates/engine/tests/remote_doc_authority.rs::direct_relay_ingest_cannot_bypass_owner_authority` | Relay ingest denied (now the owner of the retired relay test's contract). |
| R | `crates/engine/tests/remote_doc_authority.rs::a_foreign_edit_cannot_reuse_a_locally_authorized_queue_id` | Foreign edit of a local queue id -> RemoteAuthorityRequired. |
| R | `crates/engine/tests/remote_doc_authority.rs::stale_viewer_queue_mutations_cannot_rewrite_the_owners_claimed_evidence` | Non-owner RPC edits fail closed; claimed evidence intact. |
| R | `crates/engine/tests/remote_doc_authority.rs::locally_accepted_command_and_queue_still_dispatch` | Positive control keeping this suite's runs==0 negatives honest. |
| R | `crates/engine/tests/restart_resume.rs::restart_roundtrip_restores_chats_transcript_and_resume` | CI-routed (ui-tests.yml). Restart restores rows/transcript; stored session id injected as resume. |
| R | `crates/engine/tests/restart_resume.rs::kill_crash_recovers_resume_from_journal_and_stamps_aborted` | Journal-recovered session id; streaming entry stamped aborted. |
| R | `crates/engine/tests/restart_resume.rs::persistent_session_serves_multiple_turns_on_one_child` | One persistent child serves two turns. |
| R | `crates/engine/tests/restart_resume.rs::fresh_crash_requires_safe_reconciliation_without_replay` | Fresh crash -> observe-only reconciliation, no replay. |
| R | `crates/engine/tests/restart_resume.rs::resume_is_cwd_scoped` | Resume is cwd-scoped. |
| R | `crates/engine/tests/restart_resume.rs::uncertain_startup_crash_is_not_replayed_and_keeps_resume` | Keeper, now table-driven over fail_starts [1, u32::MAX]; absorbs the 150 ms no-revival-loop window and the 2026-08-13 tombstone incident note. |
| C | `crates/engine/tests/restart_resume.rs::persistent_startup_crash_keeps_stored_session_id` | Same flow and assertions as the uncertain-startup keeper (fail_starts only affects spawns after CheckingDelivery, which never happen). Folded in as the `u32::MAX` row; carried the 150 ms window and incident rationale. |
| R | `crates/engine/tests/restart_resume.rs::real_claude_remembers_codeword_across_engine_restart` | `#[ignore]` real-CLI proof with a documented command; only real-provider evidence for resume across restart. |
| R | `crates/engine/tests/restart_resume.rs::steer_after_restart_dispatches_new_turn_with_resume` | Steer with no live run becomes a resumed turn. |
| R | `crates/engine/tests/secret_input.rs::secret_answer_is_delivered_in_memory_and_never_stored` | Real CodexHarness + codex-adapter-peer.py; TRACE log + data-dir byte scan prove the secret is never stored. |
| R | `crates/engine/tests/secret_input.rs::secret_request_after_restart_closes_without_replay` | Restart closes secret cards; legacy durable answers rejected; no replay. |
| R | `crates/engine/tests/self_continued_quiesce.rs::self_continued_turn_parks_on_the_short_window` | 2026-08-13 incident: short quiesce window for self-continued turns (separate binary for process-global knobs). |
| R | `crates/engine/tests/self_continued_quiesce.rs::steered_turn_keeps_the_normal_window` | Steered turns keep the normal window. |
| R | `crates/engine/tests/session_history.rs::session_history_search_rebuild_filters_links_redacts_and_reopens` | History index through docs/RPC/SQLite: scope, filters, redaction, policy minimization, recorded model, deletion, reopen. |
| D | `crates/engine/tests/session_history.rs::request_contract_remains_bounded` | Only asserts the serde `default = default_history_limit` (50) for `{}`, restating proto/activity.rs:186-194. Every sender sets `limit` explicitly (ui/src/shell.rs:1771, drafts.rs:211, session_history.rs search()), so the default is unobservable; the name promises "bounded" but the bound is luna-sync `request.limit.clamp(1, 100)` (store.rs:806, sync lane). |
| R | `crates/engine/tests/session_publication.rs::codex_adapter_restart_publication` | CI-routed. Fake-codex.sh incident regression: durable replay restores both turns; lost-ACK restart adds no duplicate rows. |
| R | `crates/engine/tests/session_publication.rs::real_codex_restart_publication` | `#[ignore]` real-Codex variant of the same scenario (docs/session-publication.md). |
| R | `crates/engine/tests/session_publication.rs::disconnected_cleanup_is_durable_before_snapshot_debounce` | Disconnected updates durable before the snapshot debounce. |
| R | `crates/engine/tests/session_publication.rs::supplied_incident_snapshots_reconcile_via_durable_bootstrap` | `#[ignore]` opt-in incident reproduction documented in docs/session-publication.md (follow-up: retire when the incident data is gone). |
| R | `crates/engine/tests/session_publication.rs::rejected_update_retries_failed_checkpoint_and_retires_only_after_success` | Oversized row falls back to checkpoint retry; frontier matches bytes. |
| R | `crates/engine/tests/side_chats.rs::persistent_forks_migrate_legacy_states_without_recreating_identity` | Legacy state table migrates to active across two restarts without new identity. |
| R | `crates/engine/tests/side_chats.rs::persistent_forks_are_nested_discoverable_and_independent_of_source` | Keeper: nesting refused, archive/unarchive, survives source deletion, published rows; now also owns "explicit delete retires the fork" (read and send refused, no provider run). |
| R | `crates/engine/tests/side_chats.rs::imported_fork_history_stays_visible_without_implicit_provider_adoption` | Local import keeps history, refuses sends without managed identity. |
| R | `crates/engine/tests/side_chats.rs::side_history_projection_removes_rewound_tail_and_empty_history` | Projection follows rewind and empty history. |
| R | `crates/engine/tests/side_chats.rs::side_chat_lifecycle_is_durable_isolated_and_history_does_not_run` | Idempotent create/send, parent isolation, history never runs a provider. |
| R | `crates/engine/tests/side_chats.rs::deadlines_and_restart_do_not_end_side_chats_or_replay_turns` | Deadlines and restart never end side chats or replay. |
| R | `crates/engine/tests/side_chats.rs::anchored_side_chat_preserves_origin_without_mutating_parent` | Anchor/origin preserved; parent unchanged. |
| R | `crates/engine/tests/side_chats.rs::creation_replay_rejects_changed_anchor_text` | Request dedupe includes anchor text. |
| R | `crates/engine/tests/side_chats.rs::paged_history_and_restart_never_start_a_provider` | List/read paging; restart never starts a provider. |
| C | `crates/engine/tests/side_chats.rs::parent_deletion_preserves_fork_and_explicit_delete_retires_it` | Parent-deletion survival was already asserted by `persistent_forks_are_nested_discoverable_and_independent_of_source`, which also deletes the fork. Deletion is mode-independent (rpc.rs:1634 -> side_chats.rs `retire` by id; `read` refuses state "deleted"). Carried into the keeper: ReadSideChat and SendSideChat fail after the delete and no provider run happens. |
| F | `crates/engine/tests/side_chats.rs::fork_permissions_follow_explicit_own_configuration` | Repaired a fixture that promised more than it checked: the `_expected_mode` column (Ask/Auto/Full) has been unasserted since 29da9bdd changed the contract to "the fork's explicit grant decides"; dropped the dead column so the table states what it proves. |
| R | `crates/engine/tests/side_chats.rs::side_creation_cannot_claim_an_existing_regular_task_id` | Side creation cannot hijack an existing task id. |
| R | `crates/engine/tests/side_chats.rs::side_documents_and_tool_blobs_do_not_export_to_configured_edge` | Privacy: side docs, sub-docs and tool blobs never reach Edge; restored outboxes stay local. |
| R | `crates/engine/tests/side_chats.rs::fallback_side_chat_registers_managed_identity_without_starting_provider` | Only check of fallback-mode managed identity lineage (source_task_id = parent). |
| R | `crates/engine/tests/side_chats.rs::abandoned_creation_is_reconciled_after_restart_without_runtime_or_replay` | Preparing -> creationFailed on restart; request id not reusable. |
| R | `crates/engine/tests/side_chats.rs::legacy_view_close_does_not_write_lifecycle_or_stop_a_turn` | SQL trigger proves view close writes no lifecycle and does not stop the turn. |
| R | `crates/engine/tests/skill_library_e2e.rs::file_backed_provider_skill_lifecycle` | Real filesystem skill save/edit CAS, supporting files, cross-provider install, guarded remove, validation. |
| R | `crates/engine/tests/skill_library_e2e.rs::outside_symlink_is_never_read_or_mutated` | Symlink escape never read or mutated (security). |
| R | `crates/engine/tests/skill_library_e2e.rs::competing_installs_never_replace_a_winner` | Concurrent installs: one winner. |
| R | `crates/engine/tests/support/workspace_identity_repair_contract.rs::repair_contract_revision_fence_duplicate_and_restart` | RPC repair: R+1, duplicate receipt without new event, stale rename conflict, v2 id, log prefix, restart. (Store-level W/R CAS is owned by workspace_repair_authority.rs.) |
| R | `crates/engine/tests/support/workspace_identity_repair_contract.rs::repair_contract_stale_preview_cannot_cross_a_task_edit` | Stale preview after a task edit conflicts. |
| R | `crates/engine/tests/support/workspace_identity_repair_contract.rs::repair_contract_unverifiable_bound_sibling_does_not_veto_unbound_repair` | Blocked sibling does not veto an unbound repair. |
| R | `crates/engine/tests/support/workspace_identity_repair_contract.rs::repair_contract_foreign_store_native_id_and_moved_root_remain_fenced` | Table (foreign store / native id mismatch / moved root) stays blocked with no side effects. |
| R | `crates/engine/tests/support/workspace_identity_repair_contract.rs::repair_contract_pending_queued_work_is_preserved_and_blocks_correction` | Queued work blocks repair and is untouched. |
| R | `crates/engine/tests/support/workspace_identity_repair_contract.rs::repair_contract_snapshot_failure_cannot_publish_mapping_or_revision` | Snapshot-trigger failure publishes nothing; reopen confirms. |
| R | `crates/engine/tests/task_commands.rs::create_delete_identity_and_local_cleanup_share_the_receipt_transaction` | Create/delete identity + local cleanup atomic with the receipt (trigger-injected failures, crash/reopen). Uses lib.rs cleanup seams (shared). |
| R | `crates/engine/tests/task_commands.rs::project_delete_requires_exact_children_and_preserves_workspace_files` | Keeper for project deletion over RPC: exact children, cascade, workspace files kept, policy cleanup. |
| R | `crates/engine/tests/task_commands.rs::removed_project_keeps_sessions_and_readding_the_folder_restores_them` | Remove stops runs and hides sessions without deleting; re-adding restores. |
| R | `crates/engine/tests/task_commands.rs::project_cleanup_failure_resumes_from_durable_child_list` | Cleanup failure resumes after reopen. |
| R | `crates/engine/tests/task_commands.rs::project_recovery_waits_for_durable_registry_delete` | Registry write failure keeps everything until durable. |
| R | `crates/engine/tests/task_commands.rs::deletion_settles_a_held_synthetic_run_before_tombstoning` | Delete stops a live run first. |
| R | `crates/engine/tests/task_commands.rs::deletion_cleanup_failure_is_reported_and_resumes_after_restart` | Cleanup failure reported and resumed. |
| R | `crates/engine/tests/task_commands.rs::abandoned_task_preparation_reopens_drafts_without_deleting_data` | Abandoned preparation reopens drafts. |
| R | `crates/engine/tests/task_commands.rs::stale_project_preparation_never_deletes_a_moved_survivor` | Moved survivor never deleted. Setup migrated from `WorkspaceHost::delete_space` to production `delete_space_if_chats("old-project", [])`. |
| R | `crates/engine/tests/task_commands.rs::cleanup_worker_advances_beyond_one_batch_without_touching_live_tasks` | Batching beyond 20; live tasks untouched; one worker. |
| R | `crates/engine/tests/task_commands.rs::local_projection_rolls_back_with_terminal_receipt_failure_and_recovers_after_reopen` | Projection rollback never published; exact retry applies once. |
| R | `crates/engine/tests/task_commands.rs::duplicate_send_survives_two_clients_response_loss_and_restart` | Two clients, lost response, restart: one run; command-id conflict. |
| R | `crates/engine/tests/task_commands.rs::durable_queue_commands_dedupe_and_revision_fence_edit_remove` | Queue command dedupe, revision fences, storage-failure retries. |
| R | `crates/engine/tests/task_commands.rs::failed_queue_snapshot_never_applies_or_clears_the_draft_and_retry_reopens_once` | Failed queue snapshot keeps the draft; retry applies once. |
| R | `crates/engine/tests/task_commands.rs::failed_edit_and_remove_projections_stay_absent_after_abrupt_reopen` | Failed edit/remove leave row and authority intact across reopen. |
| R | `crates/engine/tests/task_commands.rs::failed_durable_claim_never_reaches_the_provider_and_reopens_held` | Claim persistence precedes dispatch. |
| R | `crates/engine/tests/task_commands.rs::failed_correlation_commit_keeps_receipt_pending_and_claim_visible_on_reopen` | Correlation commit failure -> pending receipt, unknown on reopen. |
| R | `crates/engine/tests/task_commands.rs::native_steer_caller_applies_only_for_supported_attachment_free_held_rows` | Table x3: steerQueuedNow receipt codes (message_queue.rs owns DocHost policy; this owns the receipt contract). |
| R | `crates/engine/tests/task_commands.rs::accepted_stop_fences_send_now_and_holds_the_queue` | Accepted Stop fences send-now; queue held. |
| R | `crates/engine/tests/task_commands.rs::revision_binding_turn_and_lifecycle_fences_are_visible` | Every fence reason code; snapshot paging and reload gap. |
| R | `crates/engine/tests/task_commands.rs::organization_commands_are_durable_ordered_and_revision_fenced` | Pin order, unread, archive clears pin, restart. |
| R | `crates/engine/tests/task_commands.rs::dense_pin_insertion_changes_only_requested_tasks_and_rejects_stale_neighbors` | Dense insertion leaves neighbors; stale neighbors fail. |
| R | `crates/engine/tests/task_commands.rs::same_workspace_project_move_is_atomic_and_preserves_task_state` | Git same-workspace move preserves state; cross-workspace rejected. |
| R | `crates/engine/tests/task_commands.rs::exact_non_git_workspace_move_succeeds_and_different_root_is_rejected` | Non-git exact root move; other root rejected. |
| R | `crates/engine/tests/task_commands.rs::active_task_cannot_move_under_execution` | ActiveTurnConflict on move. |
| R | `crates/engine/tests/task_commands.rs::clear_context_atomically_advances_binding_and_retains_local_state` | Clear context atomic; queue held for review; draft kept; restart. |
| R | `crates/engine/tests/task_commands.rs::provider_switch_creates_fresh_binding_and_holds_incompatible_queue` | Provider switch: fresh binding, sanitized handoff, queue review. |
| R | `crates/engine/tests/task_commands.rs::compact_is_idempotent_and_restart_unknown_is_never_replayed` | Compact idempotent; automatic lifecycle makes no manual receipts; unknown never replayed. |
| R | `crates/engine/tests/task_commands.rs::automatic_codex_context_events_gate_real_queue_drain_and_restart` | Automatic compaction gates drain; missing completion -> Unknown. |
| R | `crates/engine/tests/task_commands.rs::model_change_waits_for_safe_turn_and_keeps_binding_generation` | setModel fenced while active; keeps generation; cannot change access/account. |
| R | `crates/engine/tests/task_commands.rs::staged_history_fork_persists_acknowledged_id_and_replay_never_mutates_twice` | Staged fork ack/preflight failure; replay never forks twice. |
| R | `crates/engine/tests/task_commands.rs::fork_initial_prompt_grant_commits_with_its_snapshot` | Fork prompt grant atomic with its snapshot. |
| R | `crates/engine/tests/task_commands.rs::native_tip_fork_creates_independent_task_binding_and_lineage` | Tip fork lineage, binding, access reset, no descendant forks, provenance survives source deletion/restart. |
| R | `crates/engine/tests/task_commands.rs::earlier_message_edit_rebinds_to_native_fork_and_keeps_recovery_snapshot` | Edit-earlier-message fences, rebind, recovery restore. |
| R | `crates/engine/tests/task_commands.rs::side_chat_native_fork_preserves_binding_and_never_replays_unknown_creation` | Table (success/compacted/ack failure): creation never replays a native fork. |
| R | `crates/engine/tests/task_commands.rs::side_chat_context_snapshot_is_an_explicit_native_fork_alternative` | Explicit snapshot never forks; dedupe includes the choice. |
| R | `crates/engine/tests/task_commands.rs::codex_settled_terminal_turns_keep_checkpoints_and_history_prefixes` | Table x3 terminal kinds keep checkpoints for history planning. |
| R | `crates/engine/tests/task_commands.rs::run_with_unsorted_model_options_dispatches_after_doc_round_trip` | 2026-10-06 regression (modelOptions key order quarantined runs). |
| R | `crates/engine/tests/task_commands.rs::queued_message_with_unsorted_execution_options_dispatches` | Same regression for queued rows. |
| R | `crates/engine/tests/task_commands.rs::receipted_run_that_fails_local_authority_is_rejected_not_left_sending` | Tampered accepted run is rejected ("nothing was sent"), never dispatched. |
| R | `crates/engine/tests/task_cwd_immutability.rs::existing_task_directory_is_immutable_across_direct_legacy_and_restart_paths` | setCwd command and legacy setChatCwd both rejected; restart confirms. |
| R | `crates/engine/tests/task_identity.rs::non_codex_managed_tasks_continue_and_keep_local_metadata_behavior` | Binding durable before SessionStarted publication; explicit resume mismatch rejected. |
| R | `crates/engine/tests/task_identity.rs::restart_replays_lineage_dedup_and_immutable_binding_generations` | Lineage, dedup, generations replay; foreign provider home -> ExternalChange. |
| R | `crates/engine/tests/task_identity.rs::reconciliation_reports_missing_foreign_active_and_unsupported_truthfully` | Reconciliation state table + EnvironmentMissing. |
| R | `crates/engine/tests/task_identity.rs::import_is_deduplicated_bound_before_chat_visibility_and_tombstoned` | Import dedupe, tombstone, foreign active. |
| R | `crates/engine/tests/task_identity.rs::adoption_transitions_ownership_normalizes_lifecycle_and_recovers_materialization` | Ownership transitions; interrupted materialization recovered only by the owning home. |
| R | `crates/engine/tests/task_work.rs::failed_child_work_remains_failed_and_needs_attention` | Failed child work state. |
| R | `crates/engine/tests/task_work.rs::nested_ownership_stop_receipts_and_independent_child_survival` | Ownership, stale preview, service exclusion, idempotent stop receipts. |
| R | `crates/engine/tests/task_work.rs::archive_refuses_background_native_work_until_its_terminal_event` | Archive refused while native work runs. |
| R | `crates/engine/tests/task_work.rs::native_stop_acknowledgement_unknown_restart_and_terminal_readback` | Durable intent before provider I/O; unknown never replayed across restart. |
| R | `crates/engine/tests/task_work.rs::warm_child_reuse_does_not_inherit_earlier_stop_or_new_child_ownership` | Warm child reuse ownership boundaries. |
| R | `crates/engine/tests/task_work.rs::nested_native_child_and_shell_remain_owned_after_intermediate_completion` | Nested native + shell ownership; historical rows unknown. |
| R | `crates/engine/tests/timeline_order_e2e.rs::notices_written_mid_turn_stay_in_chronological_order` | Six recorded failure cases of mid-turn notice ordering. |
| R | `crates/engine/tests/tool_output.rs::received_tool_payloads_and_live_activity_survive_steer_and_reopen` | Real CodexHarness + tool-output-codex.py: lossless outputs, privacy, lazy blobs, reopen. |
| R | `crates/engine/tests/transcript_salvage.rs::blank_journaled_chat_recovers_entries_from_fat_rollback` | Boot salvage refills a blank journaled chat; no duplicates on reboot. |
| R | `crates/engine/tests/turn_quiesce.rs::parked_self_continuation_folds_and_requiesces` | 2026-08-12 incident: self-continued output folds, completes, notifies. |
| R | `crates/engine/tests/turn_quiesce.rs::missing_turn_end_settles_instead_of_working_forever` | Lost Done settles via the watchdog. |
| R | `crates/engine/tests/turn_quiesce.rs::open_tool_call_never_quiesces` | Open tool call holds the watchdog off. |
| R | `crates/engine/tests/turn_quiesce.rs::stale_tool_echo_stays_parked` | Stale tool echo does not resume a parked session. |
| R | `crates/engine/tests/unarchive_on_send.rs::sending_a_message_unarchives_the_chat` | Run unarchives; Interrupt does not. |
| R | `crates/engine/tests/usage_account_selection.rs::account_disconnect_during_scan_rejects_stale_usage_snapshot` | Stale account history never published (blocking-pool boundary, no sleeps). |
| R | `crates/engine/tests/usage_history.rs::equal_calls_count_separately_but_repeated_notifications_do_not` | Codex dedupe semantics, warm cache. |
| R | `crates/engine/tests/usage_history.rs::cold_snapshot_includes_archived_codex_sessions` | Explicit owner for archived sessions in a cold scan (also a precondition inside `legacy_cache`, which is weaker ownership). |
| R | `crates/engine/tests/usage_history.rs::archiving_a_cached_rollout_never_counts_it_twice` | Archive move not double counted or partial. |
| R | `crates/engine/tests/usage_history.rs::extreme_period_boundaries_fall_back_without_panicking` | Overflowing period boundaries fall back. |
| R | `crates/engine/tests/usage_history.rs::environment_merge_keeps_scanner_pricing_warnings` | Merge keeps warnings; duplicate source excluded. |
| R | `crates/engine/tests/usage_history.rs::environment_merge_keeps_older_partial_summary_and_distinct_warnings` | Partial state not erased by newer data. |
| R | `crates/engine/tests/usage_history.rs::parser_upgrade_retains_deleted_rollout_history_with_an_explicit_warning` | Cache migration retains deleted history with a warning. |
| R | `crates/engine/tests/usage_history.rs::parser_upgrade_reparses_available_rollouts_and_supersedes_archived_legacy_rows` | Table: available sources reparsed authoritatively. |
| R | `crates/engine/tests/usage_history.rs::unknown_cache_versions_do_not_publish_unverifiable_history` | Unknown cache version publishes nothing. |
| R | `crates/engine/tests/usage_history.rs::cached_history_survives_a_removed_transcript_directory` | Table x4 retained cache after directory removal. |
| R | `crates/engine/tests/usage_history.rs::isolated_qa_cache_paths_preserve_provider_home_boundaries` | Child-process HOME isolation: provider-home descendants stay read-only. |
| R | `crates/engine/tests/usage_refresh.rs::usage_cache_background_lanes_failure_and_restart` | Quota reads never wait for history, failures retain values, restart keeps scoped values. |
| R | `crates/engine/tests/workbench_performance_e2e.rs::transcript_resume_confirms_exact_content_and_falls_back_safely` | Transcript resume token correctness (forged, cross-chat, same-length rewrite, restart). |
| R | `crates/engine/tests/workbench_performance_e2e.rs::host_history_wire_and_rewrite_measurements` | `#[ignore]` documented perf harness (docs/design/harness-evolution/slice-6-verification.md); retains an evidence artifact. |
| R | `crates/engine/tests/workbench_performance_e2e.rs::changed_rows_structural_rewrites_and_context_match_durable_history` | Projection converges to the document across rewrites, continuations, malformed slots, context updates. |
| R | `crates/engine/tests/workspace_data_safety.rs::committed_upload_collisions_preserve_original_and_staging` | Upload collision/replay safety (keeper over uploads.rs inline overlaps). |
| R | `crates/engine/tests/workspace_data_safety.rs::concurrent_upload_collision_publishes_only_one_payload` | Concurrent collision: one publication. |
| R | `crates/engine/tests/workspace_data_safety.rs::upload_publication_rejects_existing_symlink` | Symlink publication refused. |
| R | `crates/engine/tests/workspace_data_safety.rs::generated_image_replay_cannot_replace_published_history` | Generated image replay with changed bytes refused. |
| R | `crates/engine/tests/workspace_data_safety.rs::checkpoints_capture_and_restore_force_added_ignored_files` | Checkpoints include force-added ignored files and restore index/HEAD. |
| R | `crates/engine/tests/workspace_data_safety.rs::checkpoint_restore_rejects_ignored_file_edits_after_preview` | Restore refuses after concurrent edits. |
| R | `crates/engine/tests/workspace_data_safety.rs::refused_worktree_deletion_preserves_unregistered_directory` | Row (unregistered) of `refused_worktree_deletion`. |
| R | `crates/engine/tests/workspace_data_safety.rs::refused_worktree_deletion_preserves_primary_checkout` | Row (primary checkout). |
| R | `crates/engine/tests/workspace_data_safety.rs::refused_worktree_deletion_preserves_locked_worktree` | Row (locked worktree). |
| R | `crates/engine/tests/workspace_files.rs::workspace_file_rpcs_list_search_read_write_and_watch` | Workspace file RPCs incl. CAS conflicts, concurrent writers, image jail, watch. |
| R | `crates/engine/tests/workspace_files.rs::workspace_file_rpcs_preserve_plain_folder_search_support` | Plain (non-git) folder support. |
| R | `crates/engine/tests/workspace_files.rs::write_rejects_changed_checkout_even_when_contents_match` | Checkout identity fence on writes. |
| R | `crates/engine/tests/workspace_identity_repair.rs::missing_import_preview_is_display_only_and_preserves_stored_history` | Missing-root import preview is display-only. Uses `preview_import_observation_for_tests` (task_identity, L01). |
| R | `crates/engine/tests/workspace_identity_repair.rs::import_preview_does_not_mislabel_file_or_dangling_alias_as_missing_root` | Files and dangling aliases are not "missing". |
| R | `crates/engine/tests/workspace_identity_repair.rs::missing_import_cwd_does_not_hide_an_ambiguous_selected_project` | Ambiguous selected project refused without writes. |
| R | `crates/engine/tests/workspace_identity_repair.rs::canonical_aliases_share_only_the_same_environment_and_source_scope` | Alias canonicalization and source scope. |
| R | `crates/engine/tests/workspace_identity_repair.rs::ambiguous_relative_creation_is_refused_without_identity_side_effects` | Relative cwd table refused with no identity writes. |
| R | `crates/engine/tests/workspace_identity_repair.rs::project_grouping_move_preserves_execution_identity_and_binding` | Grouping move keeps identity and binding. |
| R | `crates/engine/tests/workspace_identity_repair.rs::legacy_unbound_repair_after_project_move_preserves_origin_and_current_group` | Legacy repair after move keeps origin and group. |
| R | `crates/engine/tests/workspace_identity_repair.rs::lww_space_retarget_cannot_authorize_an_unrelated_legacy_root` | LWW retarget cannot authorize an unrelated root. |
| R | `crates/engine/tests/workspace_identity_repair.rs::registered_linked_worktree_remains_a_valid_legacy_repair_root` | Registered linked worktree stays repairable. |
| R | `crates/engine/tests/workspace_identity_repair.rs::project_home_alias_uses_owner_home_not_engine_working_directory` | `~` resolves to the owner HOME (child-process isolation). |
| R | `crates/engine/tests/workspace_identity_repair.rs::new_worktree_materialization_keeps_sibling_root_and_initial_binding` | Identity keeper for UUID run-with-worktree materialization, restart replay, external move refused, cwd/resume fenced. |
| R | `crates/engine/tests/workspace_identity_roots.rs::project_uuid_registration_preserves_each_root` | Live projection after B registers (before_a captured pre-B); distinct risk from the restart replay case. Part of the documented 3-case baseline (docs/qa/workspace-identity-repair-design-2026-09-26.md). |
| R | `crates/engine/tests/workspace_identity_roots.rs::project_uuid_roots_and_receipts_survive_restart` | Persisted replay of roots and receipts. |
| R | `crates/engine/tests/workspace_identity_roots.rs::project_reused_worktrees_preserve_roots_and_source_space` | Linked worktrees of one project keep roots and source space. |
| R | `crates/engine/tests/workspace_repair_authority.rs::repair_transaction_advances_r_and_w_once_and_preserves_receipts_on_restart` | Store-level R/W advance once; duplicate receipt; restart. |
| R | `crates/engine/tests/workspace_repair_authority.rs::repair_cas_and_sql_failure_leave_mapping_revision_and_receipt_uncommitted` | CAS and SQL failures leave R/W/receipt uncommitted. |
| R | `crates/engine/tests/workspace_repair_authority.rs::identical_old_hash_and_local_edit_cannot_renew_pre_repair_authority` | Old authority not renewed; unrelated R does not invalidate W grants. |
| R | `crates/engine/tests/workspace_repair_authority.rs::restored_old_queue_is_inert_until_a_new_explicit_current_consent` | Restored old queue inert until explicit consent. |
| R | `crates/engine/tests/workspace_repair_authority.rs::quiescence_blocks_pending_work_and_only_exempts_its_own_repair_receipt` | Pending work blocks repair; own receipt exempt. |
| R | `crates/engine/tests/workspace_repair_authority.rs::quiescence_blocks_every_retryable_recovery_state_without_rewriting_it` | Table x6 recovery states block repair. |
| R | `crates/engine/tests/workspace_repair_authority.rs::idle_warm_runtime_blocks_repair_and_is_not_retired_implicitly` | Warm runtime blocks repair. |
| R | `crates/engine/tests/workspace_repair_authority.rs::delayed_dispatch_keeps_original_w_and_rechecks_after_the_transition_gate` | Delayed dispatch with stale W rejected. |
| R | `crates/engine/tests/workspace_repair_authority.rs::old_recovery_retry_after_repair_cannot_reseal_the_frozen_request` | Old recovery cannot reseal. |
| R | `crates/engine/tests/workspace_repair_authority.rs::initial_materialization_is_bound_to_original_pending_run_without_repair_or_receipt_renewal` | Materialization fence bound to the original run. |
| R | `crates/engine/tests/workspace_repair_authority.rs::repair_refuses_a_chat_changed_after_proof_before_registry_commit` | Race window between proof and registry commit; only proof, needs the `set_workspace_repair_precommit_hook_for_tests` seam (workspace_host.rs, L01). |
| R | `crates/engine/tests/workspace_repair_authority.rs::repair_with_unchanged_final_chat_witness_still_applies_once` | Documented failure-matrix control for the race test (only RPC-level W == R+1 check). |
| R | `crates/engine/tests/workspace_sync.rs::two_engines_share_a_workspace` | Two engines over the in-process registry room: devices, spaces, chats, sessions, preview, LWW renames. |
| R | `crates/engine/tests/workspace_sync.rs::claim_on_first_command_creates_the_chat_row` | Claim while connected (live push); offline variant covers backlog flush, a distinct risk. |
| R | `crates/engine/tests/workspace_sync.rs::offline_project_claim_syncs_and_survives_viewer_restart` | Offline claim syncs and survives viewer restart. |
| R | `crates/engine/tests/workspace_sync.rs::claim_resolves_a_worktree_cwd_to_the_repo_root_space` | No phantom worktree space. |
| R | `crates/engine/tests/workspace_sync.rs::claimed_chat_row_records_the_run_harness` | Claimed row records the dispatched harness. |
| R | `crates/engine/tests/workspace_sync.rs::non_host_engine_leaves_remote_chats_commands_alone` | Non-host never executes commands. |
| R | `crates/engine/tests/workspace_sync.rs::chat_config_selects_the_run_harness` | Chat config picks the harness. |
| R | `crates/engine/tests/workspace_sync.rs::two_engines_converge_through_a_real_workspace_room` | `#[ignore]` live-edge variant with documented command. |
| R | `crates/engine/tests/workspace_sync.rs::legacy_workspace_doc_migrates_instantly_on_first_boot` | Migration of the legacy workspace doc; rollback snapshot retained. |
| R | `crates/engine/tests/workspace_sync.rs::side_chat_visibility_survives_registry_sync_without_loading_history` | Peers get only the visibility marker, never history or runtime. |
| R | `crates/engine/tests/worktree_on_run.rs::run_with_worktree_spec_materializes_on_host_and_reuses` | Legacy chat run-with-worktree materializes on host, stamps the branch, reuses the checkout. |

## Layer plan

This lane is itself the keeper layer for most engine contracts, so the plan is mostly "keep and name the owner". The redundant layers found were small: one retired file, a few strict-subset or near-duplicate declarations, and one test-only engine wrapper.

### Keeper per contract

| Contract | Keeper | Notes |
| --- | --- | --- |
| DocHost queue policy (drain, hold, steer, claims, edit leases, restart) | `message_queue.rs` | No inline overlap in `doc_host.rs` or `crates/doc`. |
| Queue/steer **receipt** contract and fences | `task_commands.rs` | Distinct boundary from `message_queue.rs` (receipt codes vs DocHost state). |
| Preflight holds (execution unavailable) | `queue_preflight.rs` | |
| Recovery state machine | `recovery.rs` | SQLite-trigger fault injection per transition. |
| Restart/resume continuity, startup-crash tombstone | `restart_resume.rs` (CI-routed) | `uncertain_startup_crash_is_not_replayed_and_keeps_resume` is now table-driven over `fail_starts` [1, `u32::MAX`]. |
| Project/space deletion cascade | `task_commands::project_delete_requires_exact_children_and_preserves_workspace_files` (real RPC) + `crates/doc` delete-space tests (replicas) | Retired `m5::delete_space_cascades_chats_and_sessions`, which only exercised a test-only wrapper. |
| Claude reconnect never rewrites live credentials | `m5c::claude_reconnect_leaves_live_provider_and_mcp_credentials_untouched` | Strict-subset sibling deleted. |
| Claude login (PKCE, paste-code lifecycle) | `m5c::rpc_dispatch_for_m5c_methods` (wire) | Direct-API duplicate folded in. |
| Legacy peer-relay delivery is refused (L29) | `remote_auth::two_clients_pair_scope_revoke_stream_and_restart`, `remote_doc_authority::direct_relay_ingest_cannot_bypass_owner_authority` | `relay_delivery.rs` retired (ignored, contradicts current product). |
| Side-chat deletion / source independence | `side_chats::persistent_forks_are_nested_discoverable_and_independent_of_source` | Absorbed the parent-deletion test. |
| Native-fork side chats per provider | `pane_chat.rs` Codex and Claude rows | Rows retained: provider identity selects different engine branches (Codex metadata app-server history check vs harness `native_history_turns`; different runtime delivery). |
| Workspace identity allocation (baseline repro) | `workspace_identity_roots.rs` | Registration (live projection) and restart (persisted replay) cases cover different risks. |
| Identity repair over RPC | `workspace_identity_repair.rs` + `support/workspace_identity_repair_contract.rs` | |
| Repair W/R store lifecycle | `workspace_repair_authority.rs` | Header assigns proof/allocation to the RPC suites; no duplicate failure points. |
| UUID run-with-worktree materialization (identity) | `workspace_identity_repair::new_worktree_materialization_keeps_sibling_root_and_initial_binding` | `worktree_on_run.rs` keeps the legacy-chat path; `project_commands_context` keeps the Commands target; `project_commands_worktree_setup` keeps the setup gate. |
| Unix PTY terminals | `m5_repos_diffs_terminals.rs` terminal tests | `terminals.rs` inline tests are Windows-only; `l21_terminals.rs` covers the history-backed variant. |
| Vault/key store without OS keychain | `no_keychain.rs` | Production `LocalKeyStore`; inline credentials tests use an in-memory store. |

### Retired files and layers

- `crates/engine/tests/relay_delivery.rs` (whole file, 342 lines): its only test had been `#[ignore]`d since 2c3b28a6 because the product now rejects that path.

### Assertions carried into keepers

- `rpc_dispatch_for_m5c_methods`: authorize URL starts with `https://claude.ai/oauth/authorize?code=true`, carries `code_challenge_method=S256` and the console `redirect_uri`; completing a cancelled login fails.
- `uncertain_startup_crash_is_not_replayed_and_keeps_resume`: the `u32::MAX` (helper down hard) row, the 150 ms no-revival-loop window, and the 2026-08-13 tombstone-incident rationale; every assertion now runs for both rows.
- `persistent_forks_are_nested_discoverable_and_independent_of_source`: after deleting the fork, `ReadSideChat` and `SendSideChat` fail and no provider run happens.

### Repairs (F)

- `pane_chat::legacy_child_rows_contracts_and_requests_decode_unchanged`: the legacy `ChildThreadReceipt` decode used `if let Ok(..)`, so the compatibility assertion vanished if decoding regressed; it now `expect`s the decode.
- `side_chats::fork_permissions_follow_explicit_own_configuration`: removed the `_expected_mode` table column that has been unasserted since 29da9bdd changed the contract.

### Production seams

- None removed in this lane: every seam the lane's tests touch lives in L01 files (`crates/engine/src`).
- Unlocked for L01: `WorkspaceHost::delete_space` (workspace_host.rs:1781) has no production caller. This lane migrated its two remaining callers (`project_commands::generation_ownership_deletion_and_backup`, `task_commands::stale_project_preparation_never_deletes_a_moved_survivor`) to the production `delete_space_if_chats`; one rpc.rs inline test still uses the wrapper (see cross-lane requests).
- Retained on purpose (tests have no real-boundary alternative): `Repos::list_folders_with(.., hang_for_test)` (wedged listing timeout), `WorkspaceHost::set_workspace_repair_precommit_hook_for_tests` (proof/commit race), and the shared fixtures `seed_child_for_qa`, `cleanup_boot_id_for_tests`, `cleanup_worker_count_for_tests`, `run_project_cleanup_maintenance_for_tests`, `use_in_memory_credentials_for_tests`, `preview_import_observation_for_tests` (each also used by other suites, UI examples or apps).

### Considered and retained (for the preservation review)

- `#[ignore]` opt-in tests with documented commands stay: `restart_resume::real_claude_remembers_codeword_across_engine_restart`, `session_publication::real_codex_restart_publication`, `session_publication::supplied_incident_snapshots_reconcile_via_durable_bootstrap` (docs/session-publication.md), `workbench_performance_e2e::host_history_wire_and_rewrite_measurements` (slice-6 verification doc), `workspace_sync::two_engines_converge_through_a_real_workspace_room`.
- `workspace_sync::claim_on_first_command_creates_the_chat_row` vs `offline_project_claim_syncs_and_survives_viewer_restart`: live push vs backlog flush are different failure modes.
- `usage_history::cold_snapshot_includes_archived_codex_sessions`: also implied by a helper precondition elsewhere, but that is weak ownership; kept as the explicit owner.
- `workspace_repair_authority::repair_with_unchanged_final_chat_witness_still_applies_once`: documented control for the race test; the only RPC-level W == R+1 check.
- `project_commands_context::uuid_materialization_dependency_uses_real_run_path`: identity assertions overlap the identity keeper, but it is the only proof an explicit action after UUID materialization targets the new cwd.

## Cross-lane requests

1. **L01 (`crates/engine/src/rpc.rs`, `crates/engine/src/workspace_host.rs`)**: migrate the one remaining `#[cfg(test)]` caller of `WorkspaceHost::delete_space`, rpc.rs ~12693 (`core.workspace.delete_space("delayed-project")`), to `delete_space_if_chats("delayed-project", &expected_chat_ids)`, then delete `WorkspaceHost::delete_space` (workspace_host.rs:1778-1783, doc comment included). rpc.rs ~13106 calls `RegistryDoc::delete_space` (luna-doc) and is unaffected. No other caller exists in crates or apps (rg); this lane's two callers are already migrated.
2. **L01 (informational, for your own C/D decisions)**: inline tests that overlap keepers in this lane:
   - `credentials.rs::existing_ciphertext_with_a_missing_key_never_creates_a_replacement` ↔ `no_keychain::missing_and_unsafe_vault_keys_preserve_existing_ciphertext` ("missing" row, production key store).
   - `uploads.rs::commit_assembles_chunks_into_a_durable_file` ↔ `m5c::uploads_chunk_commit_readback_and_jail`.
   - `uploads.rs::generated_image_*` ↔ `workspace_data_safety::generated_image_replay_cannot_replace_published_history` (partial).
   - `agent_accounts.rs::claude_disconnect_persists_without_touching_native_login` ↔ `m5c::disconnect_preserves_provider_and_legacy_credentials` (partial; the inline test also checks `resolve_execution` across reload).
3. **Workflows**: none. `relay_delivery` was not named by any workflow; `session_publication` and `restart_resume` (ui-tests.yml) keep their targets.
4. **L04 (`crates/harness/tests/fixtures`)**: none. Fixtures used here (`fake-codex.sh`, `fake-claude-luna.py`, `codex-adapter-peer.py`, `native-inputs-codex.py`, `tool-output-codex.py`) are unchanged.

## Possible product defects

None found. No retained assertion in this lane appears to encode a bug. Gaps worth tracking:

- The product has no authenticated peer-relay delivery path. The retired `relay_delivery.rs` documented the old unauthenticated behavior, which L29 deliberately rejects, so this is a product gap, not a defect.

## Follow-ups

- If authenticated relay delivery returns, write its E2E fresh in `remote_auth.rs`/`device_routing.rs` rather than restoring `relay_delivery.rs`.
- Several negatives rely on sleep windows because no "did not dispatch" event exists (`m5c::titling_e2e_names_chat_and_renames_worktree_branch` 400 ms, `message_queue.rs` 100–250 ms, `workspace_sync::non_host_engine_leaves_remote_chats_commands_alone` 400 ms). If the engine grows a drain-settled event, assert on it instead.
- `recovery::ordinary_fork_recovery_preserves_frozen_run_and_binding_across_restart`: `if let Some(before)` skips the binding-continuity assertions when no binding existed before restart; after a run confirms the accepted row always has one, assert it for that row.
- `project_commands_context::uuid_materialization_dependency_uses_real_run_path`: trim identity assertions already owned by `workspace_identity_repair::new_worktree_materialization_keeps_sibling_root_and_initial_binding` so the test reads as the Commands contract.
- `m5::folder_lister_timeout_path` keeps a `hang_for_test` bool in a public API; consider a `pub(crate)` timeout constructor exercised by an inline test if L01 prefers no public seam.
- `session_publication::supplied_incident_snapshots_reconcile_via_durable_bootstrap`: retire once the private incident snapshots are no longer kept.
- `pane_chat_mcp::codex_create_thread_schema_grant_cap_deny_and_child_identity`: the deny table carries an unused `index` field (`let _ = index;`); cosmetic.
