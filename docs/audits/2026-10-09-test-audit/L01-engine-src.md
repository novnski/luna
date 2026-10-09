# L01 engine-src test audit ledger

- **Lane:** L01-engine-src (campaign 2026-10-09, branch `test-audit/L01-engine-src`, base `e4a0f534`).
- **Scope:** the inline `#[cfg(test)]` tests in the 43 `crates/engine/src` files of the lane manifest, plus test-only seams in those files' production code. `crates/engine/tests/**` (lanes L02/L03) was read as keeper evidence only and not edited.
- **Baseline (`e4a0f534`):** 449 test declarations; 20,947 lines inside inline test modules (brace-matched `#[cfg(test)] mod` blocks).
- **After:** 417 declarations; 20,587 test-module lines. Marks: **R 413, F 0, C 17, D 19**. Test-only production seams removed: 8 functions (≈100 lines).
- **CI routing context:** `luna-engine --lib` runs in `ui-tests.yml` (Linux) and `windows.yml` (Windows). Most `crates/engine/tests` suites run only locally; `session_publication`, `restart_resume`, `codex_subagents`, `local_profiles` (Linux) and `codex_catalog`, `codex_login_resolution`, `auth` (Windows) are the CI-routed ones. Every D below names a keeper that exists today; see Cross-lane requests for routing.
- **Method:** every declaration was read in full with its production owner, callers, overlapping suites (`rg` across `crates/*/tests`, `crates/ui`, examples) and, where the reason was unclear, `git log -S`. No build or test was run in this lane (coordinator compiles and tests); edited files were formatted with `rustfmt --edition 2024`.

## Ledger

| Mark | Test | Evidence |
| --- | --- | --- |
| R | engine/src/activity_history.rs::tests::computer_use_provider_completion_never_becomes_semantic_success | Semantic contract: a provider-completed computer-use call stays `Unknown`, never `Succeeded`; no integration suite drives `tool_event` for ComputerUse (rg `ComputerUse` in crates/engine/tests: none). |
| R | engine/src/activity_history.rs::tests::luna_status_is_tool_activity_without_indexing_its_input_or_output | Privacy: `luna_status` MCP input (untrusted) is never indexed and gets a fixed summary; `tests/session_history.rs` covers generic MCP input only, not the luna_status special case. |
| R | engine/src/activity_history.rs::tests::appshot_history_keeps_viewer_capture_and_remote_task_ownership_separate | Appshot envelope ownership (viewer device vs remote environment), excluded-term redaction, `available_here` gating; no engine/sync integration test builds appshot envelopes (sync `activity_index_write_e2e` only stores pre-built rows). |
| R | engine/src/activity_history.rs::tests::user_prompt_history_excludes_appshot_accessibility_context | Privacy: accessibility text and image paths after the appshot marker never enter the index; only owner. |
| R | engine/src/activity_history.rs::tests::redacts_secrets_and_excludes_sensitive_paths_without_indexing_output | Overlaps `tests/session_history.rs::session_history_search_rebuild_filters_links_redacts_and_reopens` (secret redaction, `.env` exclusion) but uniquely asserts tool output is never serialized and the bounded-index `Withheld` receipt; keep. |
| R | engine/src/agent_accounts.rs::tests::inventory_home_resolution_does_not_materialize_named_account | Read-only inventory/usage target never materializes a named shadow home; no integration covers `codex_home_for_inventory`/`codex_usage_target` side effects. |
| D | engine/src/agent_accounts.rs::tests::fixture_marker_disables_claude_keychain_fallback_even_without_credentials_file | Asserts only that the fixture helper writes its marker, read back through the `#[cfg(test)]` accessor `claude_keychain_fallback_disabled` that re-implements the same path join (identity copier + test-only seam). The marker's real effect (Claude usage reports Unsupported without spawning the CLI) is exercised by every hermetic engine suite that calls `disable_claude_keychain_fallback()` (e.g. `tests/usage_refresh.rs::usage_cache_background_lanes_failure_and_restart`), and production's no-Keychain contract stays owned by `tests/no_keychain.rs::claude_identity_without_secret_is_available_without_credential_verification` plus `claude_usage_without_credentials_file_never_falls_back_to_keychain` below. Unlocks deleting the `#[cfg(test)] fn claude_keychain_fallback_disabled`. |
| R | engine/src/agent_accounts.rs::tests::claude_disconnect_persists_without_touching_native_login | Claude disconnect persists across reload, blocks execution, reconnects, and metadata holds no tokens. `tests/m5c_accounts_uploads_titles.rs::disconnect_preserves_provider_and_legacy_credentials` covers only the untouched-credentials part; Codex persistence is in `tests/provider_scope_e2e.rs`, Claude's `claude_enabled` path is only here. |
| R | engine/src/agent_accounts.rs::tests::codex_scoped_listing_excludes_other_provider_discovery | `list_harness(Codex)` must not report Claude accounts/warnings; integration suites only call all-provider `list`. |
| R | engine/src/agent_accounts.rs::tests::native_identity_changes_rotate_execution_auth_identity | `auth_instance_id` rotates when the native login file changes identity (drives runtime retirement). `tests/account_lifecycle_acceptance.rs` switches between named accounts, not native identity changes. |
| R | engine/src/agent_accounts.rs::tests::codex_label_rename_roundtrips_through_persisted_metadata | Rename persists across reload and stays distinct from provider email; no engine integration calls `rename` (UI only via RPC). |
| R | engine/src/agent_accounts.rs::tests::managed_codex_auth_store_parser_only_accepts_file | Config contract: parser of managed `cli_auth_credentials_store` and the file-requirement compatibility rule; pure function, single owner. |
| R | engine/src/agent_accounts.rs::tests::provider_managed_native_codex_remains_available_without_auth_json | Keyring-managed native Codex keeps executing with `Unknown` auth and a named warning; no other owner. |
| R | engine/src/agent_accounts.rs::tests::codex_named_accounts_share_sessions_but_keep_auth_private | Security: named shadow homes symlink shared state but keep a private non-symlink `auth.json`, metadata never holds keys; acceptance suite writes homes by hand and never checks this. |
| R | engine/src/agent_accounts.rs::tests::codex_shadow_rejects_symlinked_private_auth | Security: symlinked private auth is rejected; only owner. |
| R | engine/src/agent_accounts.rs::tests::plan_labels | Table owner for provider plan strings; `tests/m5c_accounts_uploads_titles.rs` covers only `Max 20×`/`ChatGPT Plus`. |
| R | engine/src/agent_accounts.rs::tests::codex_window_labels_track_the_window_span | Pure mapping with a recorded regression (30-day free window is "Month"); only owner. |
| R | engine/src/agent_accounts.rs::tests::claude_usage_windows_map_buckets_and_percent | Provider usage-response parser (wire contract: bucket names, percent to fraction, reset time); only owner. |
| R | engine/src/agent_accounts.rs::tests::claude_usage_windows_none_without_any_bucket | Same parser, empty/extra-only responses must yield "unavailable", not an empty snapshot; distinct outcome. |
| D | engine/src/agent_accounts.rs::tests::openai_url_scan | `scan_openai_url` is exercised end to end by `tests/codex_login_resolution.rs::login_child` (fake CLI prints `open <URL> in your browser`, asserts `start.url == FAKE_URL`; runs in windows.yml) and `tests/codex_login_resolver.rs` (asserts the exact URL). The `None` row only restates `str::find`. |
| R | engine/src/agent_accounts.rs::tests::noop_browser_script_is_stable_and_executable | Platform contract behind the "two auth tabs" fix: `BROWSER` script must be executable and idempotent; the login suites never inspect `BROWSER`. |
| R | engine/src/agent_accounts.rs::tests::urlencode_matches_encode_uri_component | `claude_login_flow_is_pkce_paste_code` covers `:`/`/` in the redirect only; the space-to-`%20` scope encoding has no other owner. Retained as uncertain. |
| R | engine/src/agent_accounts.rs::tests::compose_claude_credentials_live_mcp_wins_over_stale_slot | `compose_claude_credentials` runs only in PKCE paste-code completion (token exchange is never driven by integration tests); slot login vs live MCP/plugin secrets is a credential-safety contract. |
| R | engine/src/agent_accounts.rs::tests::compose_claude_credentials_does_not_resurrect_absent_live_mcp | Same owner, distinct rule (absent live MCP stays absent). |
| R | engine/src/agent_accounts.rs::tests::compose_claude_credentials_passthrough_without_live_or_oauth | Same owner, distinct rule (no live file / API-key shapes pass through verbatim). |
| R | engine/src/agent_accounts.rs::tests::claude_usage_without_credentials_file_never_falls_back_to_keychain | Security: no credentials file means Unsupported without Keychain. Precondition switched from the removed `#[cfg(test)]` accessor to a direct marker-path check. |
| R | engine/src/attention.rs::tests::recovery_projection_tracks_authoritative_phase_without_detail_leakage | Recovery phase to attention kind/target/dedup mapping and no detail leakage; `tests/attention.rs` never sets `recovery`. |
| R | engine/src/attention.rs::tests::local_connectivity_projection_distinguishes_offline_from_normal_local_mode | Connectivity attention (Disabled/Connected silent, stable id across Offline/Reconnecting); no integration owner. |
| R | engine/src/auth.rs::tests::concurrent_dns_failure_is_shared_with_http_and_room_consumers | Regression for #352: the DNS cause (via `describe_http_error`) reaches every consumer verbatim, never leaks the refresh secret, one lookup for all. `tests/auth.rs::temporary_token_errors_reach_http_and_websocket_consumers` covers the fan-out with a dropped connection but only matches `TemporarilyUnavailable(_)`, not the shared reason. |
| R | engine/src/auth.rs::tests::legacy_plaintext_session_is_reported_but_not_read_or_migrated | Security: a legacy plaintext session is neither read nor migrated; UI `rejected_legacy_session_stops_and_reboots_local` covers the boot fallback only. |
| R | engine/src/auth.rs::tests::logout_retains_the_reference_until_locked_vault_revocation_succeeds | Data safety: sign-out keeps the session reference until vault revocation succeeds; no integration locks the vault. |
| R | engine/src/auth.rs::tests::auth_config_debug_redacts_dev_bearer | Security: `Debug` redaction; `tests/auth.rs::dev_bearer_is_not_exposed_in_auth_metadata` covers serialized state only. |
| R | engine/src/auth.rs::tests::base64url_round_trips_jwt_payload | Codec owner: unpadded base64url at real JWT lengths; integration fake JWTs hit only a few payload lengths. |
| R | engine/src/auth.rs::tests::url_coding_round_trips | Codec owner for OAuth query and loopback callback encoding (space, `%`); `loopback_callback_completes_headed_sign_in` covers only `:` and `/`. |
| R | engine/src/auth.rs::tests::auth_state_serializes_as_proto_shape | Wire contract: engine `AuthState` JSON must parse as `luna_proto::AuthState`. |
| R | engine/src/backup.rs::tests::recovery_returns_old_isolated_root_after_interrupted_swap | Crash recovery of the staged-restore journal (Prepared phase). `recover_interrupted_restore` has no production caller (see Follow-ups); retained until the owner wires or removes it. |
| R | engine/src/backup.rs::tests::recovery_preserves_failed_new_root_after_interrupted_readback | Same owner, NewActive phase keeps the failed root aside; see Follow-ups. |
| R | engine/src/backup.rs::tests::retention_keeps_recent_daily_and_older_weekly_points | Table owner for `retention_candidates` (status count and pruning); no integration asserts the policy. |
| R | engine/src/change_requests.rs::tests::requested_conversation_branch_overrides_live_checkout_branch | The conversation-owned branch beats the live checkout branch; not covered by `tests/device_routing.rs`. |
| D | engine/src/change_requests.rs::tests::two_subscribers_share_one_provider_query | Stronger proof: `tests/device_routing.rs::checkout_change_request_stream_matches_locally_and_through_device_routing` asserts one host resolution shared by local, routed desktop and iOS subscribers (`lookup.resolves == 1`). |
| R | engine/src/change_requests.rs::tests::branch_change_moves_to_a_new_cache_key | Cache key follows branch changes; only owner. Call switched from the removed `#[cfg(test)] watch_checkout` wrapper to `watch_checkout_for_branch(.., None)`. |
| R | engine/src/change_requests.rs::tests::successful_none_clears_a_previous_pull_request | A successful "no PR" clears the badge; only owner (wrapper call switched as above). |
| R | engine/src/change_requests.rs::tests::unchanged_success_is_not_reemitted | Semantic de-duplication of stream frames; only owner (wrapper call switched). |
| R | engine/src/change_requests.rs::tests::failure_keeps_the_last_success | Stream-level failure keeps the last success and later subscribers read the cache; only owner (wrapper call switched). |
| R | engine/src/change_requests.rs::tests::authentication_and_rate_limit_keep_the_last_success | Private `refresh` call shape overlapping the RateLimited case above, but it alone pins that an Authentication failure keeps the last PR (`refresh` does not branch on error kind today, so this is a product rule). Retained as uncertain. |
| R | engine/src/change_requests.rs::tests::failure_backoff_grows_and_caps | Pure backoff table; only owner. |
| R | engine/src/change_requests.rs::tests::dropping_last_subscriber_evicts_cache_and_stops_refresh_work | Demand-lease eviction stops polling; only owner (wrapper call switched). |
| R | engine/src/change_requests.rs::tests::shutdown_ends_an_active_stream | Shutdown closes streams; only owner (wrapper call switched). |
| R | engine/src/chat2_host.rs::frontier_tests::http_sync_and_exhausted_checkpoint_retries_retain_dns_cause | Pull, push and checkpoint errors keep the DNS cause and never the bearer token; no integration asserts chat transport error text. |
| C | engine/src/chat2_host.rs::frontier_tests::encoded_empty_frontier_is_not_contained | Near-duplicate of `empty_frontier_is_not_contained` (same incident, same predicate); its encoded-empty version-vector assertion is folded into that test. |
| R | engine/src/chat2_host.rs::frontier_tests::empty_frontier_is_not_contained | "Add Tweets" incident regression for `contains_frontier`; now the single owner for empty bytes, an encoded-empty version vector and the contained positive control. |
| R | engine/src/chat2_host.rs::tests::parked_rows_do_not_persist_a_cursor_until_their_history_arrives | The cursor must not advance past a parked row; `tests/session_publication.rs` reproduces PendingDependencies but never checks the persisted cursor. |
| R | engine/src/context_controls.rs::tests::only_verified_compactors_are_enabled | Matches the "restates declared flags" junk pattern, but it is the only guard that Claude compaction stays disabled: no integration drives a Claude `CompactContext` rejection (Codex acceptance is in `tests/idle_archive.rs`). Retained; see Follow-ups. |
| R | engine/src/credentials.rs::tests::encrypted_lifecycle_is_scoped_redacted_and_revocable | Scope denial, metadata redaction, forget/revoke and vault-file permissions; `tests/no_keychain.rs` covers key-file permissions and ciphertext only. |
| R | engine/src/credentials.rs::tests::replaced_value_invalidates_the_previous_lease | A revision bump revokes old leases; only owner. |
| R | engine/src/credentials.rs::tests::locked_and_unavailable_key_stores_fail_closed | Key-store failure mapping without creating a vault; only owner. |
| R | engine/src/credentials.rs::tests::remote_ingress_is_denied_before_storage | Security: remote secret ingress is refused before storage; only owner. |
| D | engine/src/credentials.rs::tests::existing_ciphertext_with_a_missing_key_never_creates_a_replacement | Stronger proof on the production file key store: `tests/no_keychain.rs::missing_and_unsafe_vault_keys_preserve_existing_ciphertext` ("missing" row: reads and writes fail, vault bytes unchanged, key not recreated). The inline copy drives only the in-memory test store. |
| R | engine/src/credentials.rs::tests::independent_brokers_share_one_transaction_and_preserve_both_writes | Concurrent writers through two handles keep both entries; `production_vault_survives_restart_and_two_handles_without_os_store` is sequential only. |
| R | engine/src/credentials.rs::tests::corrupted_ciphertext_never_returns_material | Security: tampered ciphertext yields `VaultCorrupt`; only owner. |
| R | engine/src/cross_task_tools.rs::tests::control_evidence_snapshot_is_local_exact_and_revalidates_every_field | Security fence on control snapshots (scope, staleness, mismatched backend result, cancellation); integration suites never drive these errors. |
| R | engine/src/cross_task_tools.rs::tests::control_evidence_receipt_distinguishes_unknown_error_and_mismatch | Identity fence on command receipts (8 mismatch rows); only owner. |
| R | engine/src/cross_task_tools.rs::tests::list_and_read_are_local_bounded_and_untrusted | List, untrusted and scope parts overlap `tests/cross_task_tools.rs::local_backend_pages_only_visible_tasks_and_waits_on_status_rows`; the `MAX_SUMMARY_CHARS` truncation bound is only here. |
| R | engine/src/cross_task_tools.rs::tests::send_requires_user_request_and_returns_only_durable_queue_state | Idempotency and conflict overlap the integration suite; the explicit-user-request guard and Steer-is-Unsupported are only here. |
| R | engine/src/cross_task_tools.rs::tests::wait_is_bounded_and_cancellation_aware | Timeout overlaps the integration suite; cancellation is only here. |
| R | engine/src/cross_task_tools.rs::tests::catalog_omits_unverified_delivery_and_controls | `ClaudeInstructionsOnly` delivery attaches nothing, and send is omitted without approval support; the integration catalog covers only the RPC capability set. |
| R | engine/src/cross_task_tools.rs::tests::control_requires_exact_short_lived_user_grant | Security: mismatched or over-long grants are refused; integration uses only valid grants. |
| R | engine/src/cross_task_tools.rs::tests::compiled_runtime_dispatches_only_attached_local_reads | Runtime-contract dispatch refuses tools not attached to the compiled contract; only owner at this layer. |
| R | engine/src/device_write.rs::tests::old_ticket_holds_os_lock_until_released_then_fresh_epoch_starts | The InstanceLock lease outlives the owner until the last permit drops; `pub(crate)`, no integration reach. |
| R | engine/src/device_write.rs::tests::retirement_wakes_waiting_handle_and_freeze_cannot_reopen | Retirement wakes waiters and freezes permanently; only owner. |
| R | engine/src/device_write.rs::tests::owner_drop_retires_admission_without_an_explicit_shutdown | Drop retires admission and releases the lock; only owner. |
| D | engine/src/diff_sync.rs::watch_budget_tests::small_tree_is_watchable | Subsumed by `small_checkout_watches_root_only`: `watch_targets` returns the root only when `exceeds_watch_budget` is false. |
| D | engine/src/diff_sync.rs::watch_budget_tests::budget_is_exceeded_and_probe_stays_bounded | Subsumed by `over_budget_root_falls_back_to_the_git_dir` (same `MAX_WATCH_DIRS + 50` tree, observed through `watch_targets`); the "probe stays bounded" half of the name was never asserted. |
| R | engine/src/diff_sync.rs::watch_budget_tests::small_checkout_watches_root_only | Owner for the under-budget path (one root watch covers an inline `.git`). |
| R | engine/src/diff_sync.rs::watch_budget_tests::linked_worktree_watches_root_and_git_dir | A linked worktree also watches its external git dir; only owner. |
| R | engine/src/diff_sync.rs::watch_budget_tests::over_budget_root_falls_back_to_the_git_dir | Owner for the over-budget fallback (CPU-starvation regression). |
| R | engine/src/diff_sync.rs::watch_budget_tests::symlinked_dir_is_not_followed | Symlink cycles terminate; only owner. |
| R | engine/src/doc_host.rs::async_question_tests::answer_becomes_contextual_steering_and_skip_has_no_prompt | Prompt-byte owner (exact text) plus unknown-question rejection; `tests/e2e.rs` checks only substrings of the same prompts. |
| R | engine/src/doc_host.rs::luna_approval_recovery_tests::restarted_luna_approval_rejects_generic_answers_without_resolving_input | Security: a LUNA cross-task approval can never be answered through generic commands after restart; only owner. |
| R | engine/src/doc_host.rs::transfer_progress_tests::set_upserts_by_upload_id_and_clear_retires_only_its_entry | Transfer ring upsert/clear; no integration watches `WatchTransfers`. |
| R | engine/src/doc_host.rs::transfer_progress_tests::guard_retires_the_entry_on_every_exit_path | No phantom ring after a failed attempt; only owner. |
| D | engine/src/doc_host.rs::transfer_progress_tests::late_subscriber_sees_current_set_first | Asserts `tokio::sync::watch` semantics (`subscribe().borrow()` sees the current value); `watch_transfers` is a bare `subscribe()`, so no LUNA behavior is under test. |
| R | engine/src/doc_host.rs::source_context_tests::capture_source_context_reads_the_dispatch_checkout | Real git repo: branch, root, head and checkout id captured at dispatch; no integration asserts the captured context. |
| R | engine/src/doc_host.rs::degrade_grace_tests::blips_shorter_than_the_grace_never_report | Grace-timer state machine; only owner. |
| R | engine/src/doc_host.rs::degrade_grace_tests::persistent_degradation_reports_after_the_grace_and_clears_instantly | Same state machine, report and clear edges; distinct case. |
| R | engine/src/doc_host.rs::degrade_grace_tests::sources_are_independent_and_closed_chats_are_dropped | Same state machine, per-source independence and leak prevention; distinct case. |
| R | engine/src/doc_host.rs::part_segment_tests::hash_and_colon_are_escaped_unreserved_pass_through | Wire contract with the Worker for sidecar part ids. |
| R | engine/src/doc_host.rs::executable_row_hash_tests::map_key_order_never_changes_the_grant_and_legacy_bytes_are_kept | Storage/security contract: the grant hash is key-order independent and keeps legacy bytes (2026-10-06 regression). |
| D | engine/src/doc_host.rs::queued_message_prompt_tests::dispatch_adds_the_attachment_transport_to_visible_queue_text | Stronger proof: `tests/message_queue.rs::a_message_with_attachments_holds_even_for_a_steerable_agent` asserts the exact dispatched prompt bytes (`"with a file\n\nAttached images (local files — open them to view):\n- att-1"`). |
| R | engine/src/doc_host.rs::queued_message_prompt_tests::legacy_expanded_rows_are_not_expanded_twice | Migration contract for already-expanded legacy rows; only owner. |
| R | engine/src/doc_host.rs::queued_message_prompt_tests::attachment_only_rows_get_a_non_empty_prompt_body | Prompt bytes for attachment-only rows; only owner. |
| R | engine/src/doc_host.rs::publication_eviction_tests::lru_eviction_replays_unacknowledged_updates_after_reopen | Durability of unacknowledged chat2 pushes across LRU eviction; `tests/document_residency.rs` covers eviction without pending pushes. |
| R | engine/src/doc_host.rs::publication_eviction_tests::retired_fat_handle_commits_queue_rows_into_the_thin_lineage | Queue writes on a retired handle land in the thin lineage with cursor and epoch kept; only owner. |
| R | engine/src/doc_host.rs::approved_queue_projection_tests::approved_send_postcommit_failure_blocks_stale_saves_and_recovers_on_restart | A post-commit install failure poisons stale saves and imports, then recovers on restart; only owner (production approved-queue path). |
| R | engine/src/doc_host.rs::approved_queue_projection_tests::approved_send_sql_failure_is_precommit_and_never_returns_queued | Receipt and queue outcome overlap `tests/cross_task_tools.rs::approved_provider_send_sql_failure_has_no_queued_receipt_or_row`; the no-outbox-leak and save-recovers-after-failure assertions are only here. |
| R | engine/src/doc_host.rs::unwired_interrupt_projection_tests::checked_shutdown_retains_open_doc_after_synthetic_sql_failure | Checked shutdown keeps the only copy on SQL failure; `tests/document_residency.rs` covers eviction, not shutdown. |
| R | engine/src/doc_host.rs::unwired_interrupt_projection_tests::unwired_interrupt_persists_one_exact_id_and_rejects_payload_collision | Owner for the dormant `persist_unwired_interrupt_with_id` (no runtime caller, `#[allow(dead_code)]`, commit 846a8b42). Retained pending an owner decision; see Follow-ups. |
| R | engine/src/doc_host.rs::unwired_interrupt_projection_tests::unwired_interrupt_storage_failure_publishes_no_row_or_outbox | Same dormant owner; see Follow-ups. |
| R | engine/src/doc_host.rs::unwired_interrupt_projection_tests::unwired_interrupt_post_commit_failure_survives_restart_once | Same dormant owner; overlaps the approved-queue post-commit test. See Follow-ups. |
| R | engine/src/doc_host.rs::unwired_interrupt_projection_tests::unwired_interrupt_commit_blocks_stale_snapshot_saver | The projection lock blocks a stale saver; the only test of that lock ordering (driven through the dormant entry point). |
| R | engine/src/doc_host.rs::unwired_interrupt_projection_tests::remote_row_before_projection_survives_with_cursor_and_stop | A remote row plus a projection keep cursor and command; only owner. |
| R | engine/src/doc_host.rs::unwired_interrupt_projection_tests::handle_backed_checkpoint_and_replay_keep_cursor_with_stop | Checkpoint and replay through a handle-backed sink keep the cursor; only owner. |
| R | engine/src/doc_host.rs::unwired_interrupt_projection_tests::projection_blocks_remote_row_until_stop_is_installed | Remote import waits for the projection lock; the only test of that ordering. |
| R | engine/src/doc_host.rs::unwired_interrupt_projection_tests::poisoned_projection_defers_remote_rows_and_checkpoint_without_cursor_change | A poisoned projection defers rows and checkpoints; overlaps the approved-queue post-commit test but also covers replay rows and the cursor. |
| R | engine/src/doc_host.rs::unwired_interrupt_projection_tests::remote_cursor_sql_failure_keeps_committed_stop_and_old_cursor | A cursor SQL failure keeps the committed command and the old cursor; only owner. |
| R | engine/src/git_actions.rs::tests::pr_link_uses_pushed_branch_even_when_upstream_tracks_another_name | PR head selection after a push to a differently named upstream; pure, only owner. |
| R | engine/src/git_actions.rs::tests::pr_status_explicitly_rejects_local_bare_remote | Real git repo: a local bare remote is `UnsupportedHost`, not a provider lookup; only owner (`tests/github_draft_pr.rs` covers draft creation, not status). |
| R | engine/src/git_actions.rs::tests::pr_status_rejects_stale_branch_preview_before_provider_lookup | Real git: a branch switch invalidates the PR preview before any provider call; only owner. |
| R | engine/src/git_actions.rs::tests::pr_context_remains_valid_when_only_file_token_changes | Positive half of the PR context fence (file edits alone keep the PR context valid); no boundary test reaches it. |
| R | engine/src/git_actions.rs::tests::selected_commit_preserves_other_staged_and_unstaged_work | Data safety on a real repo: committing selected paths leaves other staged/unstaged work intact; only owner. |
| R | engine/src/git_actions.rs::tests::nested_chat_directory_uses_the_same_checkout_identity | Nested cwd resolves to the same checkout; only owner. |
| R | engine/src/git_actions.rs::tests::unusual_filename_does_not_expand_as_a_pathspec | Security: `:(glob)` filenames are literal, not pathspecs; only owner. |
| R | engine/src/git_actions.rs::tests::stale_preview_and_changed_remote_are_rejected | Preview token fences file and remote changes; `tests/git_checkout_ancestry_e2e.rs` covers vanished checkouts only. |
| R | engine/src/git_actions.rs::tests::switched_branch_invalidates_the_preview | Branch switch invalidates action previews; only owner. |
| R | engine/src/git_actions.rs::tests::failed_push_reports_committed_sha_and_retry_does_not_commit_again | Commit-then-push failure keeps the SHA and never double-commits; only owner. |
| R | engine/src/git_actions.rs::tests::timed_out_push_reports_uncertain_remote_outcome | Push timeout reports uncertainty (unix hook fixture); only owner. |
| R | engine/src/git_actions.rs::tests::timed_out_combined_push_preserves_commit_success_receipt | Combined action keeps the commit receipt when the push times out; only owner. |
| R | engine/src/git_actions.rs::tests::combined_action_without_remote_does_not_commit | Combined action refuses before committing when no remote exists; only owner. |
| R | engine/src/git_actions.rs::tests::push_uses_only_the_local_bare_remote | Push reaches the configured bare remote only; only owner. |
| R | engine/src/git_checkpoints.rs::tests::private_capture_diff_and_isolated_restore_preserve_index_split | Real worktree: private capture, owner/nesting/shared-checkout guards, mid-restore change detection, index split preserved. `tests/workspace_data_safety.rs` covers ignored-file capture/restore only. |
| R | engine/src/git_checkpoints.rs::tests::sparse_capture_failure_is_visible | Sparse checkouts fail visibly instead of capturing partial state; only owner. |
| R | engine/src/git_checkpoints.rs::tests::late_done_only_closes_its_own_run | A late Done closes only its own run's checkpoint; only owner. |
| R | engine/src/history_operations.rs::tests::codex_before_user_is_exclusive_and_after_assistant_is_inclusive | Table owner for `plan_history` boundaries; `tests/task_commands.rs::codex_settled_terminal_turns_keep_checkpoints_and_history_prefixes` covers one terminal-turn case. |
| R | engine/src/history_operations.rs::tests::before_user_reads_the_checkpoint_of_a_split_turns_last_entry | Split-turn checkpoint regression; only owner. |
| R | engine/src/history_operations.rs::tests::opencode_before_user_uses_the_turn_checkpoint_like_codex | OpenCode boundary parity; only owner. |
| R | engine/src/history_operations.rs::tests::capability_matrix_does_not_claim_unimplemented_native_forks | Restates the capability table, but it is the only guard that Pi/Cursor/Mock never advertise native forks (delivery for Claude/Codex is covered by `tests/claude_history.rs`/`tests/task_commands.rs`). Retained as uncertain. |
| R | engine/src/history_operations.rs::tests::tip_and_before_user_reject_unpaired_user_history | Incomplete/steered history is refused; only owner. |
| R | engine/src/http_error.rs::tests::reports_dns_cause_without_url_secrets | Security/diagnostics owner of `describe_http_error`: DNS cause kept, URL credentials/path/query/fragment stripped. |
| R | engine/src/instance_lock.rs::tests::holder_probe_reports_pid_without_disturbing_the_lock | Platform lock contract (runs in windows.yml `--lib`); only owner. |
| R | engine/src/instance_lock.rs::tests::second_acquire_fails_while_held_then_succeeds_after_drop | Single-instance guard and pid in the error; only owner. |
| R | engine/src/instance_lock.rs::tests::subprocess_lock_holder | Windows child half of the subprocess contention test (returns unless its env var is set); required by the next row. |
| R | engine/src/instance_lock.rs::tests::subprocess_contention_reports_pid_and_releases_on_exit | Windows cross-process contention and release on exit; only owner. |
| R | engine/src/integration_actions.rs::tests::complete_fresh_owner_membership_decides_exact_identity_only | Owner of the attachment gate. The whole module is dormant (`#[allow(dead_code)] pub(crate) mod integration_actions`, "No production owner driver is registered"); retained pending an owner decision, see Follow-ups. |
| R | engine/src/integration_actions.rs::tests::raw_discovery_names_and_partial_or_stale_receipts_are_unknown | Same dormant gate (stale/partial readback is Unknown); see Follow-ups. |
| R | engine/src/integration_actions.rs::tests::wrong_owner_or_selected_session_binding_is_unknown | Same dormant gate (binding mismatch rows); see Follow-ups. |
| R | engine/src/integration_actions.rs::tests::duplicate_or_invalid_membership_and_conflicting_inventory_are_unknown | Same dormant gate (malformed membership rows); see Follow-ups. |
| R | engine/src/integration_actions.rs::tests::read_only_source_returns_typed_help_and_never_offers_an_action | Dormant action service, read-only Codex source never acts; see Follow-ups. |
| R | engine/src/integration_actions.rs::tests::duplicate_key_survives_restart_and_changed_intent_is_rejected | Dormant service; fake owner driver. See Follow-ups. |
| R | engine/src/integration_actions.rs::tests::stale_and_concurrent_owner_revisions_cannot_apply | Dormant service; fake owner driver. See Follow-ups. |
| R | engine/src/integration_actions.rs::tests::failures_and_mismatched_readback_never_report_success | Dormant service; fake owner driver. See Follow-ups. |
| R | engine/src/integration_actions.rs::tests::lost_response_after_commit_resolves_from_owner_receipt_without_replay | Dormant service; fake owner driver. See Follow-ups. |
| R | engine/src/integration_actions.rs::tests::uncertain_response_without_owner_receipt_is_never_replayed | Dormant service; fake owner driver. See Follow-ups. |
| R | engine/src/integration_actions.rs::tests::names_paths_and_unknown_owners_are_not_action_targets | Dormant service; identity validation. See Follow-ups. |
| R | engine/src/integration_settings.rs::tests::native_catalog_enriches_exact_files_without_claiming_session_attachment | Privacy and attachment-truth contract of the Codex inventory merge; only owner (no engine integration reads Codex inventory). |
| R | engine/src/integration_settings.rs::tests::settings_are_scoped_and_revision_guarded | Instruction settings persistence and revision guard; only owner. |
| R | engine/src/integration_settings.rs::tests::inventory_reads_only_explicit_synthetic_sources | File-only inventory never claims ownership or attachment; only owner. |
| R | engine/src/integration_settings.rs::tests::mcp_readback_uses_workspace_identity_without_rewriting_home_facts | Workspace-scoped MCP identity and path privacy; only owner. |
| R | engine/src/integration_settings.rs::tests::synthetic_duplicate_names_keep_source_identity_and_leak_no_private_values | Identity stability and no secret/path leakage; only owner. |
| R | engine/src/integration_settings.rs::tests::global_then_project_compile_with_redacted_readback | Instruction order into the runtime contract and redacted diagnostics; only owner. |
| R | engine/src/lib.rs::local_adoption_url_tests::canonical_host_guard_rejects_vendor_aliases_and_credentials | Security: owned-edge URL canonicalization rejects credentials and aliases; only owner. |
| C | engine/src/lib.rs::device_name_tests::explicit_override_wins_and_is_trimmed | Folded into the table test `device_name_tests::first_nonblank_candidate_wins_else_platform_fallback` (row 1). |
| C | engine/src/lib.rs::device_name_tests::native_friendly_name_wins_over_hostnames | Folded into the same table (row 2). |
| C | engine/src/lib.rs::device_name_tests::windows_computer_name_is_used_when_present | Duplicate invocation: `select_local_device_name` ignores the platform unless every candidate is blank, so this is row 2 again; dropped from the table. Candidate ordering per platform belongs to `local_device_name`, which this test never reached. |
| C | engine/src/lib.rs::device_name_tests::blank_candidates_are_ignored | Folded into the same table (row 3). |
| C | engine/src/lib.rs::device_name_tests::final_fallback_is_platform_specific_and_distinct | Folded into the same table (row 4). |
| R | engine/src/local_import.rs::tests::admitted_import_finishes_rows_marker_and_summary_while_freeze_quiesces | Device-freeze admission for imports; `tests/local_import.rs` never freezes. |
| R | engine/src/local_import.rs::tests::admitted_import_finishes_after_owner_retirement | Admitted import completes after retirement and reopens; only owner. |
| R | engine/src/local_import.rs::tests::run_busy_or_retired_does_not_migrate_source_or_quarantine_marker | Busy/Retired imports touch nothing (source DB, marker, target); only owner. |
| R | engine/src/local_import.rs::tests::admitted_run_finishes_all_copies_and_flush_before_freeze_snapshot | Journal/resume/receipt copies and registry flush land before the snapshot; also the keeper for journal admission formerly covered by the deleted standalone journal-helper test. |
| R | engine/src/local_import.rs::tests::failed_ledger_and_flush_leave_no_marker_and_retry_after_owner_restart | SQL failure injection keeps the marker absent and retries after restart; only owner. |
| R | engine/src/local_import.rs::tests::concurrent_importer_clones_serialize_dedup_and_file_publication | Concurrency of cloned importers (needs the `#[cfg(test)] before_run_lock` hook to order the race); only owner. |
| R | engine/src/local_import.rs::tests::failed_file_publication_keeps_marker_absent_and_retryable | Same injection as `tests/local_import.rs::per_item_failures_surface_in_the_summary_and_leave_the_row_retryable`, but uniquely asserts the marker stays absent until a clean retry and the ledger row is merged. |
| R | engine/src/local_import.rs::tests::status_blocks_marker_quarantine_and_source_migration_until_restart | `status` under freeze/retirement never quarantines or migrates; only owner. |
| D | engine/src/local_import.rs::tests::journal_import_waits_for_same_freeze_and_retries_after_restart | Drove `LocalImporter::copy_journal_with_handle`, a `#[cfg(test)]`-only function production never calls (tests whose only purpose is preserving a test-only export). The real path's admission is owned by `run_busy_or_retired_does_not_migrate_source_or_quarantine_marker` (Busy/Retired before any write) and `admitted_run_finishes_all_copies_and_flush_before_freeze_snapshot` / `failed_ledger_and_flush_leave_no_marker_and_retry_after_owner_restart` (byte-exact journal copies, retry with a replacement owner). Unlocks deleting `copy_journal_with_handle`. |
| R | engine/src/local_import.rs::tests::import_events_serialize_camel_case | Wire contract: import stream events are camelCase JSON for the UI; integration suites use the Rust enum only. |
| R | engine/src/permissions.rs::tests::legacy_danger_fields_cannot_broaden_default_ask | Security: legacy sandbox/auto-approve fields cannot widen Ask; only owner. |
| R | engine/src/permissions.rs::tests::unsupported_auto_degrades_to_ask_never_full | Security degradation rule; only owner. |
| R | engine/src/permissions.rs::tests::claude_auto_is_model_gated_and_hidden_ask_is_never_selectable | Claude Auto model gating; only owner. |
| R | engine/src/permissions.rs::tests::every_ceiling_only_reduces_privilege | Security: parent/environment ceilings only reduce; only owner. |
| R | engine/src/permissions.rs::tests::provider_matrix_only_advertises_verified_modes | Restates the capability table, but it is the security default the UI and resolver consume; Opencode's row is also exercised by `unsupported_auto_degrades_to_ask_never_full`. Retained as uncertain. |
| R | engine/src/profile.rs::tests::local_profile_id_is_stable_and_not_the_dev_identity | Identity contract (UUID, not dev-user); `tests/local_profiles.rs` checks stability across reopen only. |
| R | engine/src/profile.rs::tests::local_profiles_in_different_data_dirs_have_different_ids | Distinct identities per data dir; only owner. |
| D | engine/src/profile.rs::tests::local_profile_resolves_isolated_store_and_upload_roots | Stronger proofs: `profile_assembler_uses_the_resolved_local_roots` (real assembly: Local scope, literal `profiles/local/uploads`, `profiles/local` created, no dev-org root) and `tests/local_import.rs::local_work_imports_into_synced_profile_once` (rows written through `EngineProfile::local` are found by the importer at the hard-coded `profiles/local` store). |
| R | engine/src/profile.rs::tests::synced_profile_uses_account_scoped_roots | Path sanitization of org/user ids into store/upload roots; `tests/local_profiles.rs::existing_session_opens_the_historical_cloud_layout_in_place` covers only plain ids. |
| D | engine/src/profile.rs::tests::legacy_uploads_are_claimed_by_only_one_account | Stronger proof at the real boundary: `tests/local_profiles.rs::synced_accounts_isolate_uploads_and_assign_the_legacy_cache_once` (first account reads the legacy cache, the second is jailed out, ownership survives switching back); that suite is CI-routed in ui-tests.yml. |
| R | engine/src/profile.rs::tests::profile_assembler_uses_the_resolved_local_roots | Now the owner of the local profile layout (see the deleted row above). |
| R | engine/src/profile.rs::tests::development_constructor_uses_account_scoped_uploads | Development layout `orgs/dev-org/dev-user`; only owner. |
| R | engine/src/recovery.rs::tests::policy_matrix_never_replays_unknown_or_action_required_failures | Pure recovery decision table (never auto-replay Unknown/auth/credits/model failures); `tests/recovery.rs` covers Unknown delivery only. |
| R | engine/src/recovery.rs::tests::overload_backoff_is_exponential_jittered_and_capped_at_four_attempts | Backoff growth and cap; only owner. |
| R | engine/src/recovery.rs::tests::subscription_reset_must_be_known_and_inside_initial_six_hours | Subscription-window rules including the expired-window clamp regression; only owner. |
| R | engine/src/registry.rs::tests::mid_turn_steering_requires_support_and_a_step_boundary | Predicate the UI queue uses to decide live steering; only owner. |
| R | engine/src/registry.rs::tests::lazy_slot_lists_without_resolving | Listing never resolves lazy harnesses; only owner. |
| R | engine/src/registry.rs::tests::descriptor_without_new_fields_parses_with_fallbacks | Wire compatibility with older catalogs; only owner. |
| R | engine/src/registry.rs::tests::newly_found_harnesses_enable_themselves_without_reviving_opt_outs | Detection-based enablement and persisted opt-outs; `tests/provider_scope_e2e.rs` toggles one harness only. |
| R | engine/src/registry.rs::tests::detection_never_enables_the_mock | Mock is never enabled by detection; only owner. |
| R | engine/src/registry.rs::tests::legacy_allow_list_migrates_to_opt_outs | Prefs migration; only owner. |
| R | engine/src/registry.rs::tests::machine_without_clis_enables_nothing | #128 regression; only owner. |
| R | engine/src/registry.rs::tests::codex_lazy_descriptor_matches_resolved_harness | Catalog descriptor parity before/after resolve; only owner. |
| R | engine/src/registry.rs::title_tests::title_preferences_persist_and_validate_harness_model_pairs | Title settings persistence and validation; only owner. |
| R | engine/src/registry.rs::title_tests::old_harness_preferences_default_to_automatic_titles | Prefs file compatibility; only owner. |
| R | engine/src/registry.rs::guarded_prefs_tests::frozen_or_retired_registry_keeps_file_and_memory_unchanged | Device-freeze admission for prefs; only owner. |
| R | engine/src/registry.rs::guarded_prefs_tests::failed_publication_does_not_clobber_existing_prefs_or_state | Pre-publication failure (cfg(test) hook field) keeps file and memory; only owner. |
| R | engine/src/registry.rs::guarded_prefs_tests::postpublication_sync_failure_reports_uncertainty_with_truthful_state | Post-publication sync failure reports uncertainty and retries; only owner. |
| R | engine/src/registry.rs::guarded_prefs_tests::in_flight_publication_drains_before_snapshot | Freeze waits for in-flight publication; only owner. |
| R | engine/src/registry.rs::guarded_prefs_tests::concurrent_title_and_toggle_updates_keep_both_changes | Concurrent writers keep both changes; only owner. |
| R | engine/src/registry.rs::guarded_prefs_tests::failed_legacy_migration_is_explicit_then_retries_and_survives_restart | Migration failure is explicit and retryable; only owner. |
| R | engine/src/registry.rs::guarded_prefs_tests::guarded_registry_rejects_foreign_root_and_ticket | Foreign device root/ticket rejected; only owner. |
| R | engine/src/repos.rs::tests::registry_commit_drains_into_snapshot_and_retries_after_freeze | Repo registry admission across freeze/retirement; only owner. |
| R | engine/src/repos.rs::tests::registry_failure_preserves_old_json_and_malformed_data_fails_closed | Atomic registry writes and fail-closed parsing; only owner. |
| R | engine/src/repos.rs::tests::postpublication_error_reports_uncertain_registry_outcome | User-facing recovery guidance after an uncertain write; only owner. |
| R | engine/src/repos.rs::tests::concurrent_registration_keeps_both_paths | Concurrent registration; only owner. |
| R | engine/src/repos.rs::tests::create_repo_during_snapshot_reports_add_repo_recovery_path | Create during a snapshot reports the Add Repo path; only owner. |
| R | engine/src/repos.rs::tests::parses_common_github_remote_forms | Parser for avatar owner/repo lookup (`parse_github_remote`, distinct from `source_control::parse_git_remote`); only owner. |
| R | engine/src/repos.rs::tests::github_avatar_downloads_once_into_the_local_cache | Avatar cache downloads once (loopback HTTP); only owner. |
| R | engine/src/repos.rs::tests::linux_drives_take_media_and_mnt_mounts_system_first | `/proc/mounts` parser table; only owner. |
| R | engine/src/repos.rs::tests::linux_drives_take_custom_top_level_block_mounts_not_system_splits | Same parser, custom roots vs system splits; distinct rows. |
| R | engine/src/repos.rs::tests::finish_drives_dedupes_by_mount_point_keeping_the_first_name | Dedupe/naming rule; only owner. |
| R | engine/src/repos.rs::tests::mount_point_unescape_handles_octal_and_lone_backslash | Octal unescape edge cases; only owner. |
| R | engine/src/repos.rs::tests::list_drives_resolves_on_this_platform | Platform smoke: the only execution of `macos_drives`/`windows_drives` (a panic surfaces as "worker exited"); its dedupe assertion restates `finish_drives`. Retained as the platform probe (runs in windows.yml `--lib`). |
| D | engine/src/repos.rs::tests::fuzzy_score_matches_a_path_subsequence | Exercised `nucleo_matcher` through a test-local copy of the matcher configuration (`score`), not `rank_file_matches`; production ranking is owned by `search_files_prefers_filename_matches`, `search_files_obeys_gitignore_and_returns_directories` and `tests/m5_repos_diffs_terminals.rs::rpc_dispatch_for_m5_methods` (SearchFiles). Unlocks deleting the `score` helper. |
| R | engine/src/repos.rs::tests::git_history_matches_unicode_case_insensitively | Unicode case folding in history search; `tests/m5_repos_diffs_terminals.rs::git_history_search_is_fuzzy_complete_and_accepts_sha_prefixes` is ASCII only. |
| R | engine/src/repos.rs::tests::history_compaction_handles_a_twenty_thousand_commit_gap | Deep-gap compaction without recursion blowup; only owner. |
| R | engine/src/repos.rs::tests::search_files_obeys_gitignore_and_returns_directories | Search honors gitignore, keeps dotfiles and directories, never `.git/`. Now calls the production `search_files_cached` (fresh cache) instead of the removed `#[cfg(test)] search_files_blocking`. |
| R | engine/src/repos.rs::tests::empty_search_features_recent_paths_before_shallow_entries | Featured recent paths first; switched to `search_files_cached` as above. |
| R | engine/src/repos.rs::tests::search_files_prefers_filename_matches | Ranking owner; switched to `search_files_cached` as above. |
| R | engine/src/repos.rs::tests::cached_search_reuses_one_walk_within_the_ttl | Cache TTL reuse; only owner. |
| R | engine/src/repos.rs::tests::cancelled_search_stops_before_walking | Cancellation before the walk; now calls `search_files_cached` directly instead of the removed `#[cfg(test)] search_files_blocking_with_cancel`. |
| D | engine/src/repos.rs::tests::workspace_checkout_rejects_sibling_paths | Stronger proof through the real RPC: `tests/m5_repos_diffs_terminals.rs::rpc_dispatch_for_m5_methods` resolves SearchFiles by space and chat (root accepted) and rejects a chat whose cwd is a sibling folder ("chat cwd must stay inside its workspace checkout"); SearchFiles resolves through `WorkspaceFiles::resolve_target`, which calls `workspace_checkout`. |
| R | engine/src/repos.rs::tests::concurrent_searches_of_one_checkout_do_not_cancel_each_other | Concurrent searches regression; only owner. |
| R | engine/src/rpc.rs::tests::checkpoint_restore_owner_gate_catches_parent_and_nested_tasks | Security predicate table for restore ownership overlap; the git_checkpoints tests stub this gate with closures. |
| R | engine/src/rpc.rs::tests::checkpoint_restore_rechecks_deleted_or_moved_target_before_file_mutation | TOCTOU: restore re-checks owner after delete/move before mutating; only owner. |
| R | engine/src/rpc.rs::tests::project_cleanup_waits_for_authoritative_registry_then_continues | Cleanup waits for registry readiness, then completes; only owner. |
| R | engine/src/rpc.rs::tests::disabled_agent_usage_is_removed_from_all_snapshot_sections | Disabled providers vanish from sources, buckets and limits; usage integration suites never disable a harness. |
| R | engine/src/rpc.rs::tests::control_evidence_backend_uses_only_local_synthetic_task_state | Real RPC backend scope fence for control evidence (foreign project, missing task, wrong environment); only owner. |
| R | engine/src/rpc.rs::tests::control_evidence_scope_requires_same_local_managed_project | Private predicate overlapping the row above, but adds remote/projectless targets, wrong-id and missing-scope rows. Retained. |
| R | engine/src/rpc.rs::tests::control_receipt_readback_rejects_wrong_command_or_provenance | Backend-layer receipt validator (`validate_local_control_receipt`), distinct code from the tools-layer validator tested in cross_task_tools.rs. |
| R | engine/src/rpc.rs::tests::cross_task_queue_uses_one_stable_id_and_waits_for_run_end | Builder-shaped, but the only guard of `hold_for_turn_end`/Queue follow-up and actor provenance for cross-task sends; `tests/cross_task_tools.rs` checks ids and text only. |
| R | engine/src/rpc.rs::tests::integration_reads_do_not_silently_use_the_wrong_device | Device fence plus the filesystem-only default (`probeProvider` false); only owner. |
| R | engine/src/rpc.rs::tests::validated_move_rejects_changed_or_missing_topology_without_relabeling | Project move fence; only owner. |
| R | engine/src/rpc.rs::tests::agent_account_params_accept_ui_shape | Wire contract with the UI's account params; only owner. |
| R | engine/src/rpc.rs::tests::local_device_is_not_forwardable | Security routing table: credential/local methods never forward; only owner. |
| R | engine/src/rpc.rs::tests::forward_deadlines_are_tiered_and_bounded | Bounded forward deadlines (the "Sending…" wedge); restates constants but is the only guard. Retained as uncertain. |
| R | engine/src/rpc.rs::tests::tool_file_paths_keep_workspace_activity_only | Featured-path extraction keeps file tools only; only owner. |
| R | engine/src/rpc.rs::context_usage_tests::replay_cutoff_travels_with_coalesced_backfill_and_live_content | Transcript stream replay watermark through `doc_messages_stream` + `EngineChatSink`; only owner. |
| R | engine/src/rpc.rs::context_usage_tests::replay_metadata_and_backfill_leave_interleaved_local_content_live | Metadata replay does not reset live animation; interleaved local content stays live; only owner at the stream layer. |
| R | engine/src/rpc.rs::context_usage_tests::replay_preserves_each_watchers_opening_cutoff_without_consuming_live_text | Per-watcher cutoffs; only owner. |
| R | engine/src/rpc.rs::context_usage_tests::reopening_rearms_history_for_previously_live_text | Reattach re-arms history; only owner. |
| R | engine/src/rpc.rs::context_usage_tests::context_only_commits_reach_remote_watch_and_reconnect | Context-usage-only commits reach a remote watcher and reconnect; only owner. |
| R | engine/src/run_journal.rs::tests::guarded_run_ticket_keeps_events_ordered_across_freeze | Journal admission across freeze; only owner. |
| R | engine/src/run_journal.rs::tests::wrong_coordinator_ticket_cannot_append | Foreign tickets cannot append; only owner. |
| R | engine/src/run_journal.rs::tests::full_line_after_write_error_is_confirmed_once | Write-error classification (full line confirmed); only owner. |
| R | engine/src/run_journal.rs::tests::partial_line_after_write_error_is_uncertain_and_next_seq_is_recovered | Partial line is uncertain and seq recovers; only owner. |
| R | engine/src/run_journal.rs::tests::redacted_runtime_receipts_are_session_scoped_and_durable | Runtime receipt durability and session scoping; only owner. |
| R | engine/src/run_journal.rs::tests::appends_are_monotonic_and_replayable | Replay cursor semantics incl. the cursor-ahead fallback; only owner. |
| D | engine/src/run_journal.rs::tests::seq_continues_across_reopen | Subsumed by sibling `torn_tail_line_is_tolerated`, which reopens the journal and asserts the next append is seq 2 (with a torn tail). |
| R | engine/src/run_journal.rs::tests::stale_scan_flags_journals_without_terminal_done | Stale-session scan; restart suites rely on it but assert recovery outcomes, not the scan. |
| R | engine/src/run_journal.rs::tests::torn_tail_line_is_tolerated | Torn-tail tolerance and seq continuation across reopen (now the single owner of both). |
| R | engine/src/runtime_contract.rs::tests::tool_and_guidance_are_exactly_co_present | Prompt/tool contract per delivery; only owner. |
| R | engine/src/runtime_contract.rs::tests::provenance_order_and_untrusted_context_are_deterministic | Prompt-byte order, untrusted-context neutralization and redacted diagnostics; only owner. |
| R | engine/src/runtime_contract.rs::tests::equivalent_instruction_sets_have_one_stable_hash | Stable contract hash; only owner. |
| R | engine/src/runtime_contract.rs::tests::status_handler_rejects_unattached_tools_and_bad_input | Tool dispatch guards; only owner. |
| R | engine/src/runtime_contract.rs::tests::provider_resume_keeps_exact_acknowledged_contract | Resume/restart rules for acknowledged contracts; only owner. |
| R | engine/src/sessions.rs::tests::git_checkpoint_fresh_and_warm_turns_follow_confirmed_boundaries | Checkpoint start/end bound to confirmed turn boundaries through real dispatch (fresh and warm runs); only owner — `tests/workspace_data_safety.rs` drives `GitCheckpoints` directly, never through runs. |
| R | engine/src/sessions.rs::tests::git_checkpoint_replacement_done_cannot_close_new_run | A replaced run's late Done cannot close the new run's checkpoint; only owner. |
| R | engine/src/sessions.rs::tests::git_checkpoint_explicit_steer_and_rejected_dispatch_keep_turns_distinct | Steers and rejected dispatches keep checkpoint turns distinct; only owner. |
| R | engine/src/sessions.rs::tests::freeze_before_dispatch_waits_without_accepting_a_run | Device freeze blocks dispatch without accepting a run; only owner. |
| R | engine/src/sessions.rs::tests::active_and_parked_run_timeout_freeze_then_interrupt_releases_ticket | Freeze timeout with active/parked runs; interrupt releases the ticket; only owner. |
| R | engine/src/sessions.rs::tests::failed_run_append_stops_without_fanout_or_assistant_fold | Journal append failure through a real dispatch: no fanout, no assistant fold, CheckingDelivery recovery; only owner (uses the `fail_after_appends_for_tests` journal hook). |
| R | engine/src/sessions.rs::tests::failed_terminal_append_aborts_prior_stream_and_shows_uncertainty | Terminal append failure aborts the streamed entry and shows uncertainty; only owner. |
| R | engine/src/sessions.rs::tests::typed_approval_answers_fail_closed_for_unenforceable_task_allowance | Security: AllowTask without enforceable allowance answers Deny; only owner. |
| R | engine/src/sessions.rs::tests::provider_context_compaction_is_bound_to_generation_and_operation | Compaction reducer fenced by binding generation and operation id; only owner. |
| R | engine/src/sessions.rs::tests::expired_approval_resolves_once_with_a_fail_closed_answer | Expiry resolves once with an empty (deny) answer; only owner. |
| R | engine/src/sessions.rs::tests::ordinary_input_route_cannot_consume_a_typed_approval | Security: ordinary input route cannot answer an approval; only owner. |
| R | engine/src/sessions.rs::tests::generic_answers_cannot_consume_server_owned_luna_approval | Security on the live path (doc_host's sibling covers the restart path); only owner here. |
| R | engine/src/sessions.rs::tests::approved_send_commit_fence_rejects_wrong_call_target_revision_and_replaced_turn | Wired approved-send fence (call, target revision, replaced turn); only owner. |
| R | engine/src/sessions.rs::tests::approved_send_postcommit_install_failure_keeps_applied_receipt_through_rpc | Caller-visible RPC outcome of a post-commit install failure (Applied/queued), distinct from doc_host's storage-state test; keeps the `fail_next_approved_queue_install_for_test` seam alive. |
| R | engine/src/sessions.rs::tests::private_interrupt_dispatch_classifies_exact_durable_receipts | Owner of the dormant `dispatch_luna_interrupt_grant_once` (`#[allow(dead_code)]`, "Fake-only, private one-shot proof. No provider callback, UI, or catalog calls this stage"). Retained pending an owner decision; see Follow-ups. |
| R | engine/src/sessions.rs::tests::private_interrupt_dispatch_uses_same_id_readback_without_replay | Same dormant dispatch; see Follow-ups. |
| R | engine/src/sessions.rs::tests::private_interrupt_dispatch_bounds_stuck_same_id_readback | Same dormant dispatch; see Follow-ups. |
| R | engine/src/sessions.rs::tests::private_interrupt_dispatch_rejects_expired_or_stale_source_before_submit | Same dormant dispatch; see Follow-ups. |
| R | engine/src/sessions.rs::tests::private_interrupt_dispatch_rejects_changed_target_before_submit | Same dormant dispatch; see Follow-ups. |
| R | engine/src/sessions.rs::tests::private_interrupt_dispatch_rejects_source_or_target_change_during_read | Same dormant dispatch; see Follow-ups. |
| R | engine/src/sessions.rs::tests::private_interrupt_grant_has_one_consumer_under_concurrency | Same dormant dispatch; see Follow-ups. |
| R | engine/src/sessions.rs::tests::prospective_interrupt_registers_one_exact_inert_pending_event | Owner of the dormant `register_luna_prospective_interrupt` ("Dormant registration proof. No provider callback or catalog calls it"). Retained pending an owner decision; see Follow-ups. |
| R | engine/src/sessions.rs::tests::prospective_interrupt_local_allow_once_mints_only_one_private_grant | Dormant `respond_luna_prospective_interrupt_local_ui`; see Follow-ups. |
| R | engine/src/sessions.rs::tests::prospective_interrupt_local_denial_and_lost_receivers_mint_no_grant | Same dormant response path; see Follow-ups. |
| R | engine/src/sessions.rs::tests::prospective_interrupt_local_allow_once_rejects_every_changed_target_field | Same dormant response path; see Follow-ups. |
| R | engine/src/sessions.rs::tests::prospective_interrupt_local_allow_once_rejects_source_or_question_drift | Same dormant response path; see Follow-ups. |
| R | engine/src/sessions.rs::tests::prospective_interrupt_local_allow_once_rejects_changes_during_target_read | Same dormant response path; see Follow-ups. |
| R | engine/src/sessions.rs::tests::prospective_interrupt_local_allow_once_rejects_missing_backend_and_replay | Same dormant response path; see Follow-ups. |
| R | engine/src/sessions.rs::tests::prospective_interrupt_rejects_changed_or_unscoped_evidence_without_event | Dormant registration; see Follow-ups. |
| R | engine/src/sessions.rs::tests::prospective_interrupt_rejects_stale_source_and_rebind_during_read | Dormant registration; see Follow-ups. |
| R | engine/src/sessions.rs::tests::prospective_interrupt_cleanup_is_bounded_and_restart_cannot_revive_it | Dormant registration cleanup; see Follow-ups. |
| R | engine/src/sessions.rs::tests::provider_wait_returns_exact_claimed_call_evidence_once | Wired claim path used by `invoke_approved_luna_send`; only owner. |
| R | engine/src/sessions.rs::tests::control_preparation_keeps_only_exact_read_only_evidence | Wired `prepare_luna_control_evidence` (called from `invoke_approved_luna_send`); only owner. |
| R | engine/src/sessions.rs::tests::control_preparation_rejects_changed_snapshot_and_unscoped_targets | Same wired stage; only owner. |
| R | engine/src/sessions.rs::tests::control_preparation_closes_when_source_changes_during_backend_await | Same wired stage; only owner. |
| R | engine/src/sessions.rs::tests::provider_turn_fence_requires_exact_checkpoint_and_claims_each_call_once | Provider turn fence for LUNA tool calls; only owner. |
| R | engine/src/sessions.rs::tests::provider_turn_fence_rejects_rebinding_replacement_cancel_and_completion | Same fence, invalidation cases; only owner. |
| R | engine/src/sessions.rs::tests::provider_turn_waits_for_durable_binding_and_checkpoint_then_claims_once | Wired wait-and-claim; calls switched from the removed `#[cfg(test)]` bool wrapper `wait_and_claim_luna_provider_call` to `wait_and_claim_luna_provider_call_evidence(..).await.is_some()`. |
| R | engine/src/sessions.rs::tests::provider_turn_wait_rejects_missing_binding_rebind_and_stale_context | Same wired wait; wrapper calls switched as above. |
| R | engine/src/sessions.rs::tests::provider_turn_wait_wakes_closed_on_cancel_done_and_run_drop | Same wired wait; wrapper calls switched as above. |
| R | engine/src/sessions.rs::tests::provider_turn_wait_has_a_strict_cap_and_failed_binding_is_terminal | Same wired wait (500 ms cap); wrapper calls switched as above. |
| R | engine/src/sessions.rs::tests::provider_turn_wait_rearms_only_the_same_warm_run_binding | Same wired wait; wrapper calls switched as above. |
| R | engine/src/sessions.rs::tests::failed_journal_append_has_no_seq_zero_fanout_and_visible_uncertainty | Lower-level sibling of `failed_run_append_stops_without_fanout_or_assistant_fold`, but it alone pins that pending provider questions are answered (empty) and the run token cancelled on journal failure, with ObserveOnly, non-automatic recovery. |
| R | engine/src/sessions.rs::tests::local_luna_approval_is_one_shot_even_for_concurrent_clicks | Wired `respond_luna_local_ui_approval` (UI composer) is one-shot under concurrency; registered through the dormant LunaCrossTask kind. |
| R | engine/src/sessions.rs::tests::local_luna_approval_fails_closed_on_expiry_run_change_and_lost_resolver | Same wired response path, fail-closed cases. |
| R | engine/src/sessions.rs::tests::synthetic_archive_allow_once_dispatches_exactly_once_with_receipt_readback | Fake-only synthetic archive grant (`LunaSyntheticArchiveProposal`: "No production run registers one yet"). Retained pending an owner decision; see Follow-ups. The shared fixture now calls `claim_luna_provider_call_evidence(..).is_some()` instead of the removed `#[cfg(test)]` wrapper. |
| R | engine/src/sessions.rs::tests::synthetic_archive_uncertain_receipt_is_never_replayed | Same fake-only grant; see Follow-ups. |
| R | engine/src/sessions.rs::tests::synthetic_archive_never_dispatches_changed_stale_or_remote_grants | Same fake-only grant; see Follow-ups. |
| R | engine/src/sessions.rs::tests::synthetic_archive_denial_expiry_replacement_and_lost_resolver_mint_no_grant | Same fake-only grant; see Follow-ups. |
| R | engine/src/sessions.rs::tests::synthetic_archive_live_claim_change_before_ui_response_mints_no_grant | Same fake-only grant; see Follow-ups. |
| R | engine/src/sessions.rs::tests::synthetic_archive_grant_is_unusable_after_binding_turn_or_run_changes | Same fake-only grant; see Follow-ups. |
| R | engine/src/sessions.rs::tests::synthetic_archive_rejects_unmatched_question_expiry_and_channel_failure | Same fake-only grant; see Follow-ups. |
| R | engine/src/sessions.rs::tests::generated_image_failure_is_sanitized_even_inside_subagents | Privacy: failed generated images never journal source paths or names, owner preserved; only owner. |
| R | engine/src/sessions.rs::tests::stale_attempt_results_cannot_mutate_a_newer_chain_or_attempt | Recovery chain fencing against stale result writes; `tests/recovery.rs` covers stale timers/claims, not stale result writes. |
| R | engine/src/sessions.rs::tests::live_routing_requires_the_same_runtime_configuration | Warm-runtime routing guard (model, reasoning, account, auth instance, attachments); only owner. |
| R | engine/src/sessions.rs::tests::provider_account_change_rejects_an_active_turn_without_interrupting_it | Account change refuses while a turn runs and leaves it alive; only owner. |
| R | engine/src/sessions.rs::tests::clean_tool_ids_ride_verbatim_and_fit_id_re | Subagent doc-id codec (storage ids), clean branch; only owner. |
| R | engine/src/sessions.rs::tests::unclean_or_oversized_ids_hash_deterministically | Same codec, hashed branch; distinct case. |
| R | engine/src/source_control.rs::tests::github_cli_uses_exact_arguments_and_checkout_cwd | CLI wire contract: exact `gh pr list` argv, cwd, env and limits; only owner. |
| R | engine/src/source_control.rs::tests::github_cli_selects_the_inspected_enterprise_repository | Enterprise host/repo selection in argv; only owner. |
| R | engine/src/source_control.rs::tests::real_git_checkout_resolves_through_host_fake_gh | E2E keeper: real git checkout plus an executable fake `gh` resolve a PR end to end. |
| R | engine/src/source_control.rs::tests::git_inspector_uses_origin_without_upstream_in_a_multi_remote_checkout | Real git, no upstream: origin selection and `[owner:branch, branch]` selectors; now also the owner of the no-upstream fallback (see the deleted row below). |
| R | engine/src/source_control.rs::tests::git_inspector_never_runs_remote_transport_for_an_absent_default_branch | Security: inspection never invokes repository-controlled transport; only owner. |
| C | engine/src/source_control.rs::tests::merged_pull_request_is_returned_when_no_open_exists | Folded with its closed twin into the table test `terminal_pull_request_is_returned_when_no_open_exists` (MERGED row). |
| C | engine/src/source_control.rs::tests::closed_pull_request_is_returned_when_no_open_exists | Folded into the same table (CLOSED row; now also asserts the number). |
| R | engine/src/source_control.rs::tests::open_pull_request_wins_over_newer_terminal_candidates | Selection rule; only owner. |
| R | engine/src/source_control.rs::tests::newest_terminal_pull_request_wins_without_an_open_candidate | Selection rule; only owner. |
| R | engine/src/source_control.rs::tests::empty_branch_lookup_does_not_repeat_owner_prefixed_selector | Regression: one lookup per branch; only owner. |
| R | engine/src/source_control.rs::tests::pull_request_from_another_fork_is_rejected | Fork ownership filter; only owner. |
| C | engine/src/source_control.rs::tests::partial_or_invalid_json_is_classified_as_decode | Folded into the table test `github_cli_failures_are_classified_without_exposing_stderr` (two Decode rows). |
| C | engine/src/source_control.rs::tests::truncated_json_is_classified_as_decode | Folded into the same table (truncated-output Decode row). |
| C | engine/src/source_control.rs::tests::missing_github_executable_is_classified | Folded into the same table (CliUnavailable row). |
| C | engine/src/source_control.rs::tests::authentication_failure_is_classified_without_exposing_stderr | Folded into the same table (Authentication row, keeps the `secret-value` leak check). |
| C | engine/src/source_control.rs::tests::rate_limit_failure_is_classified | Folded into the same table (RateLimited row). |
| C | engine/src/source_control.rs::tests::generic_github_failure_is_classified_without_exposing_stderr | Folded into the same table (CommandFailed row, keeps the `sensitive details` leak check). |
| C | engine/src/source_control.rs::tests::timeout_is_classified | Folded into the same table (Timeout row). |
| R | engine/src/source_control.rs::tests::terminal_pull_request_is_suppressed_on_default_branch | Default-branch suppression; only owner. |
| R | engine/src/source_control.rs::tests::unknown_default_branch_resolves_through_the_provider_before_suppression | Provider default-branch lookup argv and suppression; only owner. |
| R | engine/src/source_control.rs::tests::open_candidates_skip_the_provider_default_branch_lookup | No extra provider call for open PRs; only owner. |
| R | engine/src/source_control.rs::tests::provider_default_branch_failure_keeps_the_terminal_pull_request | Lookup failure keeps the terminal PR; only owner. |
| R | engine/src/source_control.rs::tests::git_inspector_reads_branch_upstream_remote_and_default_branch | Inspector argv sequence and fork upstream parsing; only owner. |
| R | engine/src/source_control.rs::tests::parses_supported_git_remote_forms | Remote URL parser table; only owner. |
| R | engine/src/source_control.rs::tests::rejects_unsupported_or_malformed_remotes | Same parser, rejection table. |
| R | engine/src/source_control.rs::tests::fork_remote_owner_selector_has_priority | Fork owner selector priority; `git_inspector_reads_branch_upstream_remote_and_default_branch` covers head/owner but not selectors. |
| D | engine/src/source_control.rs::tests::branch_without_upstream_keeps_safe_local_fallback | Subsumed by `git_inspector_uses_origin_without_upstream_in_a_multi_remote_checkout`, which resolves a real checkout with no upstream through `BranchHeadContext::resolve` and asserts `upstream_ref == None` and selectors `[owner:local, local]`. |
| R | engine/src/source_control.rs::tests::matching_local_and_upstream_branches_are_deduplicated | `refs/remotes/` stripping and selector dedupe; only owner. |
| R | engine/src/task_identity.rs::tests::guarded_log_retires_without_losing_committed_events_and_reopens | Identity log admission and retirement; `tests/task_identity.rs` never freezes or retires. |
| R | engine/src/task_identity.rs::tests::guarded_log_sqlite_failure_keeps_projection_retryable_after_restart | SQL failure keeps projection retryable; only owner. |
| R | engine/src/task_identity.rs::tests::retired_command_admission_rejects_before_callback_and_marks_post_provider_unknown | Retired admission rejects before callbacks; fork marks ProviderUnknown; only owner. |
| R | engine/src/task_identity.rs::tests::guarded_command_holds_ticket_through_callback_until_freeze_can_flush | Ticket held through the commit callback; only owner. |
| R | engine/src/task_identity.rs::tests::product_assembly_installs_identity_admission_before_publication | Assembly wires admission before publication; only owner. |
| D | engine/src/task_identity.rs::tests::external_import_key_is_deterministic_but_not_a_task_id | Stronger proof: `tests/task_identity.rs::import_is_deduplicated_bound_before_chat_visibility_and_tombstoned` (repeat preview/adopt finds the same task through the import key, the task id is a fresh UUID distinct from the provider id). |
| R | engine/src/task_identity.rs::tests::provider_prefix_is_cleanly_owned | `[LUNA] ` provider-title prefix and its removal; user-visible naming, only owner. |
| R | engine/src/task_identity.rs::tests::codex_binding_identity_tracks_canonical_store_not_auth_overlay | Binding identity hash tracks the canonical store; only owner. |
| R | engine/src/terminals.rs::windows_tests::conpty_close_joins_natural_cleanup_without_blocking_pool_capacity | Windows ConPTY lifecycle (runs only in windows.yml `--lib`); only owner. |
| R | engine/src/terminals.rs::windows_tests::conpty_unicode_cwd_resize_replay_and_exit | Windows ConPTY Unicode cwd, resize, replay, exit; only owner. |
| R | engine/src/terminals.rs::windows_tests::close_and_final_owner_drop_kill_conpty_children | Windows child cleanup; only owner. |
| R | engine/src/terminals.rs::windows_tests::conpty_cleanup_kills_descendants_on_close_shutdown_and_drop | Windows descendant cleanup scoped to one job; only owner. |
| R | engine/src/terminals.rs::windows_tests::conpty_final_output_is_drained_before_exit | Windows final output drain; only owner. |
| R | engine/src/terminals/history.rs::tests::bounded_history_reopens_as_interrupted_and_deletes_only_one_task | Reopen-as-Interrupted and task-scoped delete overlap `tests/l21_terminals.rs::shutdown_interrupts_history_and_task_delete_is_scoped`; the byte bound, last-chunk retention and 0700 directory mode are only here. |
| R | engine/src/terminals/windows.rs::resolution_tests::missing_path_does_not_delegate_shell_search_to_windows | Windows security: no implicit shell search without PATH; only owner. |
| R | engine/src/titles.rs::tests::cheapest_prefers_small_tier_then_last | Title model selection; only owner. |
| R | engine/src/titles.rs::tests::tool_use_rejects_the_title_instead_of_accepting_coding_output | Security: a tool call during titling rejects the title; only owner. |
| R | engine/src/titles.rs::tests::configured_title_harness_and_model_run_outside_the_project | Title runs read-only, outside the project, with the configured harness/model and quoted prompt; `tests/m5c_accounts_uploads_titles.rs::titling_e2e_names_chat_and_renames_worktree_branch` covers the happy path only. |
| R | engine/src/titles.rs::tests::commit_message_draft_reuses_restricted_title_harness | Commit drafts reuse the restricted title runner; only owner. |
| R | engine/src/titles.rs::tests::titles_are_cleaned | Title cleanup (quotes, headings, blank); only owner. |
| R | engine/src/transcript_history.rs::tests::metadata_replay_does_not_consume_local_or_remote_live_parts | Behavior overlaps `rpc.rs::context_usage_tests::replay_metadata_and_backfill_leave_interleaved_local_content_live`; this one also pins zero inspected parts for metadata (work bound, via the `#[cfg(test)] inspected_parts` counter). |
| R | engine/src/transcript_history.rs::tests::interleaved_replay_preserves_live_prefix_and_joins_continuations | Continuation joining is only covered here. |
| R | engine/src/transcript_history.rs::tests::replay_work_scales_with_changed_parts_not_transcript_size | Performance contract (work proportional to changed parts); only owner. |
| R | engine/src/uploads.rs::tests::sanitize_names | File-name sanitizer table; `pending_ref_traversal_cannot_escape_the_jail` covers traversal only. |
| R | engine/src/uploads.rs::tests::sweep_spares_a_fresh_empty_staging_dir | Parallel-chunk race regression; only owner. |
| D | engine/src/uploads.rs::tests::commit_assembles_chunks_into_a_durable_file | Stronger proof: `tests/m5c_accounts_uploads_titles.rs::uploads_chunk_commit_readback_and_jail` (out-of-order chunks commit to exact bytes); staging removal after commit stays asserted by `guarded_commit_drains_into_snapshot_and_retries_after_freeze` and `guarded_commit_prepublication_io_failure_preserves_staging`. |
| R | engine/src/uploads.rs::tests::guarded_commit_drains_into_snapshot_and_retries_after_freeze | Upload admission across freeze/retirement; only owner. |
| R | engine/src/uploads.rs::tests::guarded_commit_prepublication_io_failure_preserves_staging | Pre-publication failure keeps staging retryable; only owner. |
| R | engine/src/uploads.rs::tests::pending_refs_round_trip_and_resolve_after_commit | `pending://` wire format and resolution; only owner. |
| R | engine/src/uploads.rs::tests::pending_refs_in_scans_ref_lines_only | Ref scanning only on attachment lines; only owner. |
| R | engine/src/uploads.rs::tests::pending_ref_traversal_cannot_escape_the_jail | Security: hostile names stay inside the jail; only owner. |
| R | engine/src/uploads.rs::generated_image_tests::generated_image_formats_are_sniffed_and_replays_are_idempotent | Format sniffing and idempotent replay; `tests/workspace_data_safety.rs::generated_image_replay_cannot_replace_published_history` covers PNG replay only. |
| R | engine/src/uploads.rs::generated_image_tests::generated_image_rejects_untrusted_sources_without_publishing_files | Security rejections (relative, outside, dir, non-raster, oversized, symlink); only owner. |
| R | engine/src/uploads.rs::generated_image_tests::generated_image_changed_during_copy_leaves_no_partial_final_or_temp | TOCTOU during copy (unix); only owner. |
| R | engine/src/uploads.rs::generated_image_tests::generated_image_copy_locks_source_and_parent_against_mutation | Windows locking during copy; only owner. |
| R | engine/src/usage.rs::tests::any_explicit_usage_fixture_blocks_live_fallback | Safety: any fixture variable disables live provider reads; only owner. |
| R | engine/src/usage.rs::tests::live_codex_mapping_does_not_invent_a_primary_or_reset | Codex quota mapping; only owner. |
| R | engine/src/usage.rs::tests::claude_usage_maps_only_reported_account_and_model_scoped_windows | Claude usage wire mapping; only owner. |
| R | engine/src/usage.rs::tests::litellm_prices_require_complete_rates_and_unambiguous_aliases | LiteLLM price parser; only owner. |
| D | engine/src/usage.rs::tests::exact_price_override_wins_over_provider_reported_cost | Duplicate invocation: `user_rate_prices_exact_model_even_when_provider_reports_cost` already prices a provider-reported record (bucket 1, `cost_usd: Some(0.5)`) at the UserOverride rate, and `aggregate_records` does not branch on provider for overrides. |
| R | engine/src/usage.rs::tests::claude_repeated_blocks_have_same_dedupe_identity | Claude transcript dedupe identity; only owner. |
| R | engine/src/usage.rs::tests::codex_cached_and_reasoning_are_not_added_twice | Codex token accounting; only owner. |
| R | engine/src/usage.rs::tests::append_and_rewrite_keep_correct_records | Scan cache append/rewrite detection; only owner. |
| R | engine/src/usage.rs::tests::timed_out_file_resumes_its_saved_prefix | Partial scan resume; only owner. |
| R | engine/src/usage.rs::tests::fork_copy_burst_is_excluded_but_later_work_counts | Fork-copy exclusion regression; only owner. |
| R | engine/src/usage.rs::tests::synthetic_marker_is_required | Safety: synthetic roots need the marker; only owner. |
| R | engine/src/usage.rs::tests::marked_root_cannot_contain_a_real_provider_home | Safety: a marked root cannot contain a real home; only owner. |
| R | engine/src/usage.rs::tests::symlink_alias_to_nested_real_home_is_rejected | Safety: symlink alias rejection; only owner. |
| R | engine/src/usage.rs::tests::aggregation_keeps_tokens_and_unknown_cost_without_double_counting | Aggregation, dedupe and cost quality; only owner. |
| R | engine/src/usage.rs::tests::user_rate_prices_exact_model_even_when_provider_reports_cost | Now the single owner of the user-override pricing rule (exact model, cache savings, beats provider cost). |
| R | engine/src/usage.rs::tests::opencode_go_fixture_accepts_only_named_windows_with_account_provenance | OpenCode Go fixture normalization and last-good cache; only owner. |
| R | engine/src/usage.rs::tests::opencode_history_fails_before_opening_a_transcript | OpenCode history is unsupported before any open; only owner. |
| R | engine/src/usage.rs::tests::explicit_live_homes_scan_and_resume_without_provider_writes | Live-home scan, cache reuse and provider filtering; overlaps `tests/usage_history.rs` caching but uniquely covers enabled-provider filtering and the empty selection. |
| R | engine/src/usage.rs::tests::selected_provider_history_readback | `#[ignore]` plus env-gated manual live readback against the user's provider homes; never runs in CI. Retained as a manual live probe. |
| R | engine/src/usage.rs::tests::current_litellm_price_readback | `#[ignore]` plus env-gated manual network probe; retained as a manual live probe. |
| R | engine/src/usage.rs::tests::synthetic_cache_rejects_symlinked_files | Safety: symlinked cache files are refused; only owner. |
| R | engine/src/usage_opencode.rs::tests::maps_only_reported_go_windows_without_a_fixed_month_duration | OpenCode Go limit-response parser (wire contract); only owner. |
| R | engine/src/usage_opencode.rs::tests::selected_go_usage_readback | `#[ignore]` plus env-gated manual live probe; retained. |
| R | engine/src/workspace_files.rs::tests::relative_paths_preserve_normal_and_unicode_components | Path codec (Unicode); only owner. |
| R | engine/src/workspace_files.rs::tests::relative_paths_reject_unsafe_shapes | Security path table; `tests/workspace_files.rs` covers `../outside` and `.git/config` only. |
| R | engine/src/workspace_files.rs::tests::relative_paths_reject_drive_letters_and_ntfs_alternate_data_streams | Windows-shaped path rejection for files and directories; only owner. |
| R | engine/src/workspace_files.rs::tests::list_orders_directories_pages_and_detects_stale_cursors | Paging and stale cursors; only owner. |
| R | engine/src/workspace_files.rs::tests::list_honors_ignore_rules_but_never_exposes_git | Ignore rules (filtered/all) and `.git` hiding; the RPC suite checks `.git` hiding at the root only. |
| R | engine/src/workspace_files.rs::tests::search_ranks_filename_and_nested_path_matches | Search ranking; only owner. |
| R | engine/src/workspace_files.rs::tests::search_rejects_empty_and_whitespace_queries | Input validation; only owner. |
| R | engine/src/workspace_files.rs::tests::search_keeps_only_the_best_matches_while_walking | Bounded best-N during the walk; only owner. |
| R | engine/src/workspace_files.rs::tests::read_classifies_utf8_bom_and_line_endings | Encoding/line-ending classification; only owner. |
| R | engine/src/workspace_files.rs::tests::read_hashes_original_bytes_and_rejects_lossy_text | Content hash over original bytes, lossy text refused; only owner. |
| R | engine/src/workspace_files.rs::tests::read_enforces_editable_and_preview_limits | Size limits; only owner. |
| R | engine/src/workspace_files.rs::tests::read_does_not_follow_file_or_parent_symlinks | Security: symlink refusal; only owner. |
| R | engine/src/workspace_files.rs::tests::write_preserves_requested_encoding_line_endings_and_permissions | Write fidelity (BOM, CRLF, mode); RPC suite writes plain UTF-8/LF only. |
| R | engine/src/workspace_files.rs::tests::write_conflicts_leave_the_current_file_untouched | Conflict rows beyond the RPC suite's single hash conflict. |
| R | engine/src/workspace_files.rs::tests::write_requires_normalized_text_and_enforces_size_limit | Input normalization and size limit; only owner. |
| R | engine/src/workspace_files.rs::tests::write_rejects_binary_and_mixed_source_files | Binary/mixed refusal; only owner. |
| R | engine/src/workspace_files.rs::tests::write_deleted_file_returns_typed_conflict_without_creating_it | Deleted-file conflict without recreation; only owner. |
| R | engine/src/workspace_files.rs::tests::watch_normalizes_renames_deduplicates_and_filters_git | Watch event normalization table; only owner. |
| R | engine/src/workspace_files.rs::tests::watch_preserves_the_final_state_of_remove_and_create_sequences | Final-state collapsing; only owner. |
| R | engine/src/workspace_files.rs::tests::new_subscribers_do_not_create_sequence_gaps_for_existing_subscribers | Shared sequence across subscribers; only owner. |
| R | engine/src/workspace_files.rs::tests::lag_recovery_drops_old_frames_without_advancing_shared_sequence | Lag recovery semantics; only owner. |
| R | engine/src/workspace_files.rs::tests::watch_starts_with_resync_and_shares_one_checkout_entry | Shared checkout watcher and last-subscriber cancel; the RPC suite only checks the opening resync flag. |
| R | engine/src/workspace_files.rs::tests::watch_streams_native_file_changes_and_stops_on_cancel | Native change delivery overlaps `tests/workspace_files.rs::workspace_file_rpcs_list_search_read_write_and_watch`; stream closure on cancel is only here. |
| R | engine/src/workspace_files.rs::tests::watch_reconciles_native_remove_and_recreate_bursts_promptly | Native remove/recreate bursts; only owner. |
| R | engine/src/workspace_files.rs::tests::watch_reports_native_atomic_replacements_promptly | Atomic replacement (save-by-rename); only owner. |
| R | engine/src/workspace_files.rs::tests::watch_publishes_before_a_continuous_native_burst_finishes | Publication during a continuous burst; only owner. |
| R | engine/src/workspace_files.rs::tests::watch_repair_only_mode_keeps_the_stream_recoverable | Repair-only (over budget) mode; only owner. |
| R | engine/src/workspace_files.rs::image_tests::workspace_image_chunks_are_bounded_and_versioned | Chunk bounds and content-hash versioning; the RPC suite reads one small SVG chunk. |
| R | engine/src/workspace_files.rs::image_tests::workspace_images_reject_large_files_and_symlinks | Size and symlink refusal for images; only owner. |
| R | engine/src/workspace_host.rs::tests::guarded_mutation_denial_leaves_registry_and_dirty_wake_unchanged | Registry admission denial leaves state untouched; only owner. |
| R | engine/src/workspace_host.rs::tests::admitted_mutation_drains_before_freeze_and_reopens_latest_row | Admitted mutation drains before freeze; only owner. |
| R | engine/src/workspace_host.rs::tests::import_row_rejects_ticket_from_another_device_root | Foreign tickets rejected; only owner. |
| R | engine/src/workspace_host.rs::tests::queued_worker_cannot_overwrite_newer_checked_flush | Snapshot ordering race (uses the `#[cfg(test)] snapshot_after_capture` hook); only owner. |
| R | engine/src/workspace_host.rs::tests::checked_flush_reports_sqlite_failure_without_publishing_new_bytes | Checked flush failure; only owner. |
| R | engine/src/workspace_host.rs::tests::debounced_worker_retries_failed_snapshot_without_another_change | Debounced retry; only owner. |
| R | engine/src/workspace_host.rs::tests::checked_drain_waits_for_worker_then_saves_latest_registry | Drain ordering; only owner. |
| R | engine/src/workspace_host.rs::tests::checked_shutdown_retries_failed_final_snapshot_without_new_change | Shutdown retry; only owner. |
| R | engine/src/workspace_host.rs::tests::registry_handoff_waits_before_flush_even_after_drain_cancellation | Handoff ordering; only owner. |
| R | engine/src/workspace_host.rs::tests::stale_join_generation_cannot_install_or_clear_new_room | Room join generation fence; only owner. |
| R | engine/src/workspace_host.rs::tests::disconnect_cancels_pending_initial_join_without_network | Disconnect cancels pending join; only owner. |
| R | engine/src/workspace_host.rs::tests::guarded_join_drain_awaits_pending_url_and_owner_permit_holds_lock | Join drain and permit lifetime; only owner. |
| R | engine/src/workspace_host.rs::tests::assembled_host_uses_owner_admission_and_reopens_after_retirement | Assembly installs admission; only owner. |
| R | engine/src/workspace_host.rs::tests::guarded_snapshot_ticket_drains_before_freeze_and_denies_flush_phase | Snapshot ticket vs freeze phases; only owner. |
| R | engine/src/workspace_host.rs::tests::guarded_boot_reports_initial_sqlite_failure_before_join_and_can_retry | Boot failure before join; only owner. |
| R | engine/src/workspace_host.rs::tests::guarded_snapshot_sqlite_failure_retries_and_reopens_latest_row | Snapshot failure retry; only owner. |
| R | engine/src/workspace_host.rs::tests::retired_guarded_boot_preserves_legacy_row_and_root_lease | Retired boot preserves the legacy row and lease; only owner. |
| R | engine/src/workspace_host.rs::tests::disconnect_and_drain_close_all_host_tasks_before_network_poll | Task lifecycle on disconnect; only owner. |
| R | engine/src/workspace_host.rs::tests::registry_http_sync_retains_dns_cause | Registry HTTP errors keep the DNS cause and never the token; only owner. |
| R | engine/src/workspace_host.rs::tests::presence_publish_keeps_unchanged_lists_quiet_and_late_subscribers_current | Quiet publication (no spurious wakes); only owner. |
| C | engine/src/workspace_host.rs::tests::boot_repairs_the_legacy_unknown_device_sentinel | Merged with its twin into `boot_repairs_only_the_legacy_unknown_device_sentinel` (sentinel row). |
| C | engine/src/workspace_host.rs::tests::boot_preserves_a_user_selected_device_name | Merged into the same test (user-name row). |
| D | engine/src/workspace_host.rs::tests::linked_worktree_resolves_to_the_checkout_root | Copied fixture of `tests/workspace_sync.rs::claim_resolves_a_worktree_cwd_to_the_repo_root_space`, which builds the same `proj`/`clever-ember` layout and proves through the real claim path that the worktree cwd resolves to the root's space with no phantom space. |
| R | engine/src/workspace_host.rs::tests::primary_checkouts_and_plain_folders_resolve_to_none | Negative cases (primary checkout, plain folder, foreign gitdir) of `linked_worktree_root`, also used by doc_host and repos; only owner. |

## Layer plan

The lane has no redundant *file* to retire: every inline module still owns contracts that no boundary suite reaches (crash/freeze admission fences, codecs, parsers, privacy filters, Windows-only behavior). The redundant **layer** is the inline replay of contracts that an integration suite already proves at a real boundary; those rows are D. A second, smaller layer is near-duplicate tests of one pure function; those rows are C and now live in one table-driven test.

### Keeper per retired contract

| Contract | Retired inline test(s) | Keeper |
| --- | --- | --- |
| `scan_openai_url` extracts the authorize URL from CLI output | `agent_accounts::openai_url_scan` | `tests/codex_login_resolution.rs::login_child` (via `codex_login_honors_override_only_installation`), `tests/codex_login_resolver.rs::codex_login_resolves_override_and_adds_its_directory_to_child_path` |
| Keychain-fallback fixture marker | `agent_accounts::fixture_marker_disables_claude_keychain_fallback_even_without_credentials_file` | Every hermetic engine suite that writes the marker; no-Keychain production rule: `tests/no_keychain.rs::claude_identity_without_secret_is_available_without_credential_verification`, `agent_accounts::claude_usage_without_credentials_file_never_falls_back_to_keychain` |
| Change-request subscribers share one provider query | `change_requests::two_subscribers_share_one_provider_query` | `tests/device_routing.rs::checkout_change_request_stream_matches_locally_and_through_device_routing` |
| Missing vault key never replaces existing ciphertext | `credentials::existing_ciphertext_with_a_missing_key_never_creates_a_replacement` | `tests/no_keychain.rs::missing_and_unsafe_vault_keys_preserve_existing_ciphertext` |
| Watch budget under/over threshold | `diff_sync::small_tree_is_watchable`, `diff_sync::budget_is_exceeded_and_probe_stays_bounded` | `diff_sync::small_checkout_watches_root_only`, `diff_sync::over_budget_root_falls_back_to_the_git_dir` |
| Transfer ring current value for late subscribers | `doc_host::late_subscriber_sees_current_set_first` | None needed: `tokio::sync::watch` semantics, `watch_transfers` is a bare `subscribe()` |
| Attachment transport appended at dispatch | `doc_host::dispatch_adds_the_attachment_transport_to_visible_queue_text` | `tests/message_queue.rs::a_message_with_attachments_holds_even_for_a_steerable_agent` (exact prompt bytes) |
| Journal import admission | `local_import::journal_import_waits_for_same_freeze_and_retries_after_restart` | `local_import::run_busy_or_retired_does_not_migrate_source_or_quarantine_marker`, `admitted_run_finishes_all_copies_and_flush_before_freeze_snapshot`, `failed_ledger_and_flush_leave_no_marker_and_retry_after_owner_restart` |
| Local profile on-disk layout | `profile::local_profile_resolves_isolated_store_and_upload_roots` | `profile::profile_assembler_uses_the_resolved_local_roots`, `tests/local_import.rs::local_work_imports_into_synced_profile_once` |
| Legacy uploads claimed by one account | `profile::legacy_uploads_are_claimed_by_only_one_account` | `tests/local_profiles.rs::synced_accounts_isolate_uploads_and_assign_the_legacy_cache_once` (CI-routed) |
| Path fuzzy matching | `repos::fuzzy_score_matches_a_path_subsequence` | `repos::search_files_prefers_filename_matches`, `repos::search_files_obeys_gitignore_and_returns_directories`, `tests/m5_repos_diffs_terminals.rs::rpc_dispatch_for_m5_methods` |
| Workspace checkout rejects sibling folders | `repos::workspace_checkout_rejects_sibling_paths` | `tests/m5_repos_diffs_terminals.rs::rpc_dispatch_for_m5_methods` (SearchFiles by space/chat, sibling cwd refused) |
| Journal seq continues after reopen | `run_journal::seq_continues_across_reopen` | `run_journal::torn_tail_line_is_tolerated` |
| No-upstream branch selectors | `source_control::branch_without_upstream_keeps_safe_local_fallback` | `source_control::git_inspector_uses_origin_without_upstream_in_a_multi_remote_checkout` (real git) |
| Import key dedupe, task id stays a fresh UUID | `task_identity::external_import_key_is_deterministic_but_not_a_task_id` | `tests/task_identity.rs::import_is_deduplicated_bound_before_chat_visibility_and_tombstoned` |
| Chunked upload commit | `uploads::commit_assembles_chunks_into_a_durable_file` | `tests/m5c_accounts_uploads_titles.rs::uploads_chunk_commit_readback_and_jail`; staging cleanup in `uploads::guarded_commit_*` |
| User price override beats provider cost | `usage::exact_price_override_wins_over_provider_reported_cost` | `usage::user_rate_prices_exact_model_even_when_provider_reports_cost` |
| Linked worktree cwd resolves to the checkout root | `workspace_host::linked_worktree_resolves_to_the_checkout_root` | `tests/workspace_sync.rs::claim_resolves_a_worktree_cwd_to_the_repo_root_space` |

### Consolidations (assertions carried into keepers)

- `chat2_host::frontier_tests::empty_frontier_is_not_contained` now also asserts the encoded-empty version vector is not contained (from the deleted `encoded_empty_frontier_is_not_contained`).
- `lib.rs::device_name_tests::first_nonblank_candidate_wins_else_platform_fallback`: one table replaces five tests; the Windows row was a duplicate invocation and was dropped.
- `source_control::terminal_pull_request_is_returned_when_no_open_exists` (MERGED/CLOSED table) and `source_control::github_cli_failures_are_classified_without_exposing_stderr` (eight failure rows, both stderr leak checks kept) replace nine tests.
- `workspace_host::boot_repairs_only_the_legacy_unknown_device_sentinel` merges two one-assertion tests.
- `agent_accounts::claude_usage_without_credentials_file_never_falls_back_to_keychain` keeps its "marker absent" precondition as a direct path check instead of the removed accessor.

### Test-only production seams removed

| Seam | File | Replacement in retained tests |
| --- | --- | --- |
| `#[cfg(test)] fn claude_keychain_fallback_disabled` | `agent_accounts.rs` | direct `claude_config_dir.join(DISABLE_CLAUDE_KEYCHAIN_FALLBACK)` check |
| `#[cfg(test)] fn watch_checkout` (wrapper) | `change_requests.rs` | `watch_checkout_for_branch(cwd, identity, None)` |
| `#[cfg(test)] fn copy_journal_with_handle` | `local_import.rs` | none (its only test was deleted) |
| `#[cfg(test)] fn set_prefs_before_publish_for_tests` | `registry.rs` | none (no caller anywhere) |
| `#[cfg(test)] fn search_files_blocking`, `search_files_blocking_with_cancel` | `repos.rs` | production `search_files_cached` with a fresh `FileIndexCache` (test-local `search` helper) |
| `#[cfg(test)] fn wait_and_claim_luna_provider_call`, `claim_luna_provider_call` (bool wrappers) | `sessions.rs` | the production `*_evidence(..)` methods with `.is_some()` |

Test-local helper removed: `repos::tests::score` (a copy of the matcher configuration).

### Seams kept (each still has a test with no boundary alternative)

`DeviceWriteOwner::epoch_for_tests`, `RunJournal::fail_after_appends_for_tests`/`fail_next_append_for_tests`, `DocHost::fail_next_approved_queue_install_for_test`, `LocalImporter` `before_run_lock` hook, `HarnessRegistry` `prefs_before_publish`/`prefs_after_publish` fields, `WorkspaceHost` `snapshot_after_capture` hook, `TranscriptHistory::inspected_parts`, `Repos::register_with_hooks`, `http_error::test_support::FailingDns`. `AuthConfig::use_in_memory_credentials_for_tests`, `test_credential_broker` and the `CredentialBroker::*_for_tests` helpers are also used by `crates/ui` and its examples (other lanes), and `WorkspaceHost::set_workspace_repair_precommit_hook_for_tests` by `tests/workspace_repair_authority.rs`.

## Cross-lane requests

1. **Keepers relied on by this lane's deletions (L02/L03, please keep or carry their assertions):** `tests/codex_login_resolution.rs` (`login_child`, `codex_login_honors_override_only_installation`), `tests/codex_login_resolver.rs`, `tests/device_routing.rs::checkout_change_request_stream_matches_locally_and_through_device_routing`, `tests/no_keychain.rs::missing_and_unsafe_vault_keys_preserve_existing_ciphertext` and `::claude_identity_without_secret_is_available_without_credential_verification`, `tests/message_queue.rs::a_message_with_attachments_holds_even_for_a_steerable_agent`, `tests/local_profiles.rs::synced_accounts_isolate_uploads_and_assign_the_legacy_cache_once`, `tests/local_import.rs::local_work_imports_into_synced_profile_once`, `tests/m5_repos_diffs_terminals.rs::rpc_dispatch_for_m5_methods`, `tests/task_identity.rs::import_is_deduplicated_bound_before_chat_visibility_and_tombstoned`, `tests/m5c_accounts_uploads_titles.rs::uploads_chunk_commit_readback_and_jail`, `tests/workspace_sync.rs::claim_resolves_a_worktree_cwd_to_the_repo_root_space`.
2. **CI routing (workflow owner):** the deleted rows ran in CI through `luna-engine --lib` (Linux and Windows); most keepers above do not. To keep CI coverage, add to the `ui-tests.yml` `session-sync-regressions` engine step: `--test message_queue --test m5_repos_diffs_terminals --test m5c_accounts_uploads_titles --test workspace_sync --test task_identity --test device_routing --test no_keychain --test local_import` (`local_profiles` is already routed; `codex_login_resolution` runs in `windows.yml`). No target, module path or filter named in any workflow moved or disappeared in this lane, so no routing edit is *required*.
3. **Optional boundary carries that would unlock further deletions here (L02/L03):**
   - `tests/task_commands.rs`: a Claude `CompactContext` command rejected with `UnsupportedOperation` would replace `context_controls::only_verified_compactors_are_enabled` (a capability restatement).
   - `tests/local_import.rs::per_item_failures_surface_in_the_summary_and_leave_the_row_retryable`: assert `!dir.path().join("local-import.json").exists()` after the failed run and `.is_file()` after the clean retry; then `local_import::failed_file_publication_keeps_marker_absent_and_retryable` can be deleted.
   - `tests/session_history.rs::session_history_search_rebuild_filters_links_redacts_and_reopens`: assert `raw_activity_index` never contains "private tool output must not enter the history index"; then the tool-output half of `activity_history::redacts_secrets_and_excludes_sensitive_paths_without_indexing_output` becomes redundant (its bounded-index `Withheld` half would still need an owner).
4. **UI lane:** `crates/ui` and its examples depend on the engine's `*_for_tests` credential/auth helpers listed above; they were not touched.

## Possible product defects

None found. No retained assertion looked like it encodes a bug, and no test was suspected to fail on the baseline. Weak assertions are listed under Follow-ups.

## Follow-ups

1. **Dormant production subsystems kept alive only by their tests (owner decision: wire or delete; each deletion removes the code and its tests together):**
   - `crates/engine/src/integration_actions.rs` — the whole module is `#[allow(dead_code)] pub(crate) mod integration_actions;` ("No production owner driver is registered"); 11 tests, ≈420 production lines.
   - `doc_host.rs` `persist_unwired_interrupt_with_id` ("Dormant B0a prerequisite … no runtime path calls it yet", commit 846a8b42); 9 tests. Two of them (`unwired_interrupt_commit_blocks_stale_snapshot_saver`, `projection_blocks_remote_row_until_stop_is_installed`) are the only tests of projection-lock ordering and would need re-homing on the approved-queue path.
   - `sessions.rs` prospective interrupt, private interrupt dispatch and synthetic archive grants (`LunaProspectiveInterrupt`, `LunaCrossTask` pending kinds; `register_luna_prospective_interrupt`, `respond_luna_prospective_interrupt_local_ui`, `dispatch_luna_interrupt_grant_once`); 24 tests.
   - `backup.rs` `pub fn recover_interrupted_restore` has no caller (the CLI `Recover` uses `recover_managed_restore`), yet staged activation writes the journal it recovers; wire it into `luna backup recover`/startup or delete it with its 2 tests.
2. `local_import.rs` `pub fn LocalImporter::new` (legacy unguarded constructor) has no caller anywhere; delete in a production cleanup.
3. Capability-table restatements kept for lack of a delivery test: `context_controls::only_verified_compactors_are_enabled`, `permissions::provider_matrix_only_advertises_verified_modes`, `history_operations::capability_matrix_does_not_claim_unimplemented_native_forks`, `rpc::forward_deadlines_are_tiered_and_bounded`. Replace each with a delivery-level assertion, then delete.
4. `repos::list_drives_resolves_on_this_platform`: its dedupe check uses `Vec::dedup` (consecutive only) and restates `finish_drives`; keep it as a platform smoke but drop or replace that assertion.
5. `registry.rs` carries an orphaned doc comment ("`descriptors()` stamps the per-device enabled flag …") above `newly_found_harnesses_enable_themselves_without_reviving_opt_outs`, left by an earlier test deletion.
6. Manual live probes (`usage::selected_provider_history_readback`, `usage::current_litellm_price_readback`, `usage_opencode::selected_go_usage_readback`) are `#[ignore]` and env-gated; consider moving them to a `scripts/` live-readback harness with a retained artifact, per the E2E-first preference.
