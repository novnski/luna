# L09 small crates, apps and edge: test-audit ledger

- Lane: `L09-small-crates-apps-edge`, branch `test-audit/L09-small-crates-apps-edge`, base `e4a0f534`.
- Scope: `crates/preview`, `crates/syntax`, `crates/theme`, `crates/update`, `apps/luna`,
  `apps/luna-headless`, `apps/luna-relay` (inline tests, integration suites, test-only seams), and the
  edge unit and workerd suites listed in the manifest (`edge/src/*.test.ts`, `edge/test/workerd/*`,
  both vitest configs).
- Baseline: 202 test declarations (148 Rust `#[test]`/`#[tokio::test]`, 54 vitest `it`), 8,861 test
  LOC (integration/test files counted whole, plus inline `#[cfg(test)]` modules).
- Final: 187 declarations (139 Rust, 48 vitest), 8,676 test LOC. Marks: R 177, F 9, C 3, D 13.
- CI routing read: `preview-tests.yml` (`cargo test -p luna-preview` on ubuntu and macOS, `npm test`
  in edge), `deploy.yml` (edge typecheck and `npm test`), `windows.yml` (`cargo test -p luna -p
  luna-update` on Windows). `luna-syntax`, `luna-theme`, `luna-relay` and `luna-headless` have no CI
  test job. No workflow names a test or filter this lane changed.

## Ledger

| Mark | Test | Evidence |
|---|---|---|
| R | `crates/preview/src/catalog.rs::tests::urls_survive_port_changes_daemon_restarts_and_multiple_services` | Only proof that hostnames persist across a catalog reopen (daemon restart), device rename and multi-service labels (`my-app-api`); `tests/proxy.rs` covers only an in-process single-service port change. |
| R | `crates/preview/src/catalog.rs::tests::duplicate_device_names_get_persistent_distinct_aliases` | Only proof of peer-label collision allocation (`macbook-2`) and its persistence; local alias keeps precedence. |
| R | `crates/preview/src/catalog.rs::tests::hostnames_cannot_address_arbitrary_hosts` | Security table for `valid_hostname`, the only filter on peer advertisements in `set_remote`; no other caller or test. |
| R | `crates/preview/src/discovery.rs::tests::service_identity_ignores_port_configuration` | Pure arg parser; the catalog path strips the literal port before calling it, so `--port` skipping in `command_identity` has no other guard. |
| R | `crates/preview/src/discovery.rs::tests::accepts_only_loopback_reachable_listeners` | Linux `/proc/net/tcp` parser and its loopback-only security filter; no integration test feeds a non-loopback socket. |
| R | `crates/preview/src/discovery.rs::tests::same_process_matches_the_scanner_start_time` | Platform contract that the per-connection `same_process` check agrees with the scanner clock; proxy suites use their own connectors and never reach it. |
| R | `crates/preview/src/discovery.rs::tests::requires_an_http_response` | Only proof that a non-HTTP banner is rejected and a 401 accepted; the integration non-HTTP listener never answers at all. |
| R | `crates/preview/src/peer.rs::tests::unreachable_stun_does_not_block_usable_peer_candidates` | Regression with a black-hole STUN server; no other suite configures one. |
| R | `crates/preview/src/peer.rs::tests::a_dropped_peer_pairs_again_without_waiting_for_ice_failure` | Bounded (8s) re-pair after initiator and non-initiator drops; `tests/leak.rs` churns only the non-initiator side and asserts task counts, not latency. |
| R | `crates/preview/src/peer.rs::tests::actual_webrtc_pair_streams_in_both_directions` | Only real WebRTC stream test in both directions with a 1 MiB WebSocket-mode stream; `tests/coordinator.rs` is ignored without a local Worker. |
| D | `crates/preview/src/service.rs::tests::a_confirmed_http_server_is_never_probed_again` | Replays `ProbeMemory::plan` for 1,000 cycles; added in the same commit (8d98c6d7) as the keeper `crates/preview/tests/discovery.rs::a_discovered_server_is_probed_once_not_every_cycle`, which counts real `HEAD` requests over several scan cycles on ubuntu and macOS CI. `ProbeMemory` stays (production caller in `start`); no seam unlocked. |
| R | `crates/preview/src/service.rs::tests::non_http_listeners_back_off_and_reused_ports_are_probed_afresh` | Backoff schedule and pid-keyed verdict reset have no integration proof (the real non-HTTP listener asserts nothing about probe counts). |
| R | `crates/preview/src/service.rs::tests::backoff_is_capped` | Cap on the not-HTTP backoff; without it a late-serving dev server could wait hours to appear. No other proof. |
| R | `crates/preview/tests/coordinator.rs::authenticated_coordinator_pairs_devices_for_large_http_preview` | The only Worker → catalog → SDP/ICE → P2P → HTTP end-to-end; ignored by design, runs against `wrangler dev`. |
| R | `crates/preview/tests/discovery.rs::only_current_project_http_processes_are_exposed_and_removals_are_live` | Real processes; project isolation, live removal, fixed-port retry and release. |
| R | `crates/preview/tests/discovery.rs::a_discovered_server_is_probed_once_not_every_cycle` | Keeper for the probe-once contract (absorbs the deleted `ProbeMemory` replay). |
| R | `crates/preview/tests/leak.rs::remote_preview_churn_does_not_accumulate_tasks_or_memory` | Leak and re-pair churn over a real WebRTC pair; unique. |
| R | `crates/preview/tests/proxy.rs::streaming_headers_websocket_cancellation_and_restart` | Proxy keeper: CONNECT, headers, cookies, streaming, redirects, WebSocket, cancellation, restart keeps URL. |
| R | `crates/preview/tests/transport.rs::independent_streams_large_bodies_half_close_and_cancellation` | Mux flow control: a blocked stream must not stall others; the proxy suite cannot isolate this. |
| R | `crates/syntax/src/lib.rs::tests::compiled_queries_are_shared_across_concurrent_documents` | Only guard of the per-grammar `OnceLock` cache documented on `cached_configuration`. |
| R | `crates/syntax/src/lib.rs::tests::aliases_keep_language_variants_distinct` | Table owner for `language_for_alias`, used by UI fences. |
| R | `crates/syntax/src/lib.rs::tests::paths_and_exact_names_are_table_driven` | Table owner for `language_for_path`, used by the file preview. |
| F | `crates/syntax/src/lib.rs::tests::shebang_is_only_used_after_explicit_hints` | Name promised precedence but only shebang detection was asserted. Now a table that also proves path beats shebang and fence beats path (`tool.rb` + python shebang → Ruby; + `bash` fence → Bash). |
| R | `crates/syntax/src/lib.rs::tests::spans_are_valid_sorted_non_overlapping_and_line_relative` | Public `from_absolute_spans` splitting and the `InvalidUtf8Boundary` error path; no other proof of the error. |
| R | `crates/syntax/src/lib.rs::tests::normalization_preserves_overlap_precedence_and_tie_order` | Pure normalizer table; includes an equal-precedence tie (Keyword vs String). |
| R | `crates/syntax/src/lib.rs::tests::minified_lines_normalize_without_quadratic_rescans` | Only guard for minified single-line input (span count above 20,000 under default limits). |
| D | `crates/syntax/src/lib.rs::tests::rust_highlighting_distinguishes_structural_categories` | Its five role assertions are a subset of the keeper `crates/syntax/tests/quality.rs::rust_reference_span_snapshot` (same public `highlight`, same `Widget`/`build`/`format!`/`42` fixture, plus 6 more roles). Removed its only helper `highlighted_fragments`. |
| R | `crates/syntax/src/lib.rs::tests::rust_multiline_raw_unicode_and_incomplete_code_remain_valid` | Only proof that multi-line comment and raw-string spans continue onto their continuation lines. |
| R | `crates/syntax/src/lib.rs::tests::limits_and_unbundled_languages_degrade_with_typed_errors` | Typed `SourceTooLarge` and `UnknownLanguage` errors at the public API. |
| C | `crates/syntax/src/lib.rs::tests::rust_queries_load_for_the_bundled_abi` | Folded as a Rust row into `every_registered_grammar_and_query_loads_for_the_bundled_abi`, which lacked Rust. Its second assertion compared two tree-sitter constants and could not fail on LUNA code. |
| R | `crates/syntax/src/lib.rs::tests::every_registered_grammar_and_query_loads_for_the_bundled_abi` | The ABI/query-load table named in `docs/syntax-highlighting.md`; now covers all 28 grammars. |
| D | `crates/syntax/src/lib.rs::tests::affected_composed_and_project_queries_load_for_pinned_grammars` | Same `configuration(language)` + non-empty capture assertion for TypeScript, Tsx, Kotlin, Dockerfile, all already rows of the keeper `every_registered_grammar_and_query_loads_for_the_bundled_abi` (both added in a82e032f). |
| F | `crates/syntax/src/lib.rs::tests::html_injects_javascript_and_css_with_a_bounded_registry` | Name promised CSS injection but every assertion came from HTML or JavaScript. Added: the style line carries a `Property` span, which the HTML query cannot produce (`tree-sitter-html` highlights have no property capture; CSS maps `class_name`/`property_name` to `@property`). |
| R | `crates/syntax/src/lib.rs::tests::jsonc_accepts_and_highlights_comments` | JSONC comment highlighting; distinct from JSON. |
| F | `crates/syntax/src/lib.rs::tests::unknown_markdown_fence_does_not_break_parent_highlighting` | Asserted only the detected language. Now headings before and after the unknown fence must keep `MarkupHeading`, which is what the name promises. |
| R | `crates/syntax/src/lib.rs::tests::markdown_highlights_block_and_inline_semantics` | Block and inline Markdown roles; unique. |
| R | `crates/syntax/src/lib.rs::tests::markdown_fences_use_all_bundled_child_grammars` | Fence injection of Rust and YAML; unique. |
| R | `crates/syntax/tests/quality.rs::rust_reference_span_snapshot` | Rust role keeper (absorbs the two deleted Rust role tests). |
| R | `crates/syntax/tests/quality.rs::typescript_composes_javascript_and_typescript_roles` | Composed TypeScript query roles. |
| R | `crates/syntax/tests/quality.rs::tsx_composes_javascript_jsx_and_typescript_roles` | Composed TSX query roles. |
| R | `crates/syntax/tests/quality.rs::kotlin_project_query_covers_structural_roles` | Project-owned Kotlin query (`queries/kotlin/highlights.scm`), including negative guesses. |
| R | `crates/syntax/tests/quality.rs::dockerfile_uses_bounded_shell_and_structured_data_injections` | Dockerfile heredoc injections and the unsupported-XML fallback. |
| R | `crates/syntax/tests/quality.rs::affected_languages_keep_utf8_boundaries_for_incomplete_code` | Renamed `incomplete_unicode_code_keeps_spans_valid_and_non_overlapping`; keeper of span validity for broken Unicode input, now with a Rust row. |
| C | `crates/syntax/tests/quality.rs::every_span_is_valid_and_non_overlapping_for_incomplete_unicode` | Re-implemented `assert_valid_document` inline for one Rust source; that source is now a row of the renamed keeper above (which also checks line count). |
| R | `crates/syntax/tests/quality.rs::benchmark_small_medium_large_and_incomplete_documents` | Ignored diagnostic the docs tell parser changes to run; assertion-free by design, not a CI test. |
| D | `crates/syntax/tests/quality.rs::reference_contains_expected_visual_roles` | Asserts only that Keyword, Function and Number kinds appear somewhere in `fn call() { let value = 42; }`; the keeper `rust_reference_span_snapshot` pins those kinds on exact text. |
| F | `crates/theme/src/lib.rs::tests::colors_round_trip_all_supported_css_hex_lengths` | A pure round trip passes even if `#abc` expands wrongly or alpha is dropped (both directions stay consistent). Now pins the parsed `Color` and the stored JSON string for each length (the theme-library storage format). |
| R | `crates/theme/src/lib.rs::tests::accent_overlay_meets_interaction_contrast` | Accessibility contract over every preset and appearance. |
| R | `crates/theme/src/lib.rs::tests::surface_preference_resolves_independently_from_theme_recommendation` | The UI keeper `crates/ui/src/theme.rs::tests::theme_recommendation_and_surface_override_resolve_independently` never resolves ThemeDefault against a Frosted recommendation; retained as the only full table. |
| R | `crates/theme/src/lib.rs::tests::builtins_have_complete_provenance_and_no_validation_errors` | Built-in registry validation gate (contrast, provenance, both Luna variants). |
| D | `crates/theme/src/lib.rs::tests::visual_fixture_matrix_covers_every_variant_and_scene` | Asserts a copied variant count (30) and `30 * 10 == 300`; no rendering or scene review happens. `VisualFixture` stays because `docs/theme-system.md` uses it as the manual QA checklist. |
| R | `crates/theme/src/library.rs::tests::linked_reload_keeps_last_known_good_on_failure` | Real files; last-known-good on a broken linked source. |
| R | `crates/theme/src/library.rs::tests::linked_reload_ignores_failures_in_unselected_sibling_variants` | Selection-scoped reload. |
| F | `crates/theme/src/library.rs::tests::linked_reload_hardens_low_contrast_palette` | Checked `variants[0]`, the untouched light variant, and "any report has an adjustment", so it passed even if reload kept the old dark palette. Now asserts the reloaded dark variant meets 4.5:1 and its report records the `text` repair from `#111111`. |
| R | `crates/theme/src/library.rs::tests::snapshot_round_trips_and_duplicate_gets_independent_ids` | Storage round trip through the real save path (second save exercises the backup rename). |
| R | `crates/theme/src/library.rs::tests::load_recovers_from_backup_left_between_replacement_renames` | Crash-recovery contract. |
| R | `crates/theme/src/library.rs::tests::rejects_libraries_above_entry_limit_before_serializing_or_cloning` | Capacity limit before any write. |
| R | `crates/theme/src/library.rs::tests::contrast_findings_do_not_block_install` | Structural vs contrast classification at install. |
| R | `crates/theme/src/library.rs::tests::install_rejects_structurally_invalid_palettes` | Blocking classification at install. |
| R | `crates/theme/src/library.rs::tests::editable_duplicate_reloads_and_keeps_last_known_good_on_failure` | Editable-file path reload and last-known-good. |
| R | `crates/theme/src/vscode.rs::tests::resolves_jsonc_includes_and_semantic_overlays` | Importer with real files: JSONC, includes, semantic tokens, terminal colors. |
| R | `crates/theme/src/vscode.rs::tests::reports_dropped_font_styles` | Importer report contract. |
| R | `crates/theme/src/vscode.rs::tests::hardens_ayu_style_low_contrast_workbench_roles_without_blocking_import` | Regression for a real theme shape (Ayu Mirage). |
| R | `crates/theme/src/vscode.rs::tests::hardens_and_reports_low_contrast_faint_text` | Faint-text repair and its report mapping. |
| R | `crates/theme/src/vscode.rs::tests::rejects_package_declarations_and_includes_outside_selected_root` | Path-containment security for package, include and token files. |
| R | `crates/theme/src/vscode.rs::tests::bounds_include_depth_and_source_size` | Resource bounds (depth, size, variant count). |
| R | `crates/theme/src/vscode.rs::tests::flattens_translucent_foundational_surfaces` | Translucent surface flattening. |
| R | `crates/theme/src/vscode.rs::tests::appearance_detection_handles_high_contrast_and_unknown_ui_themes` | `hc-light` and unknown `uiTheme` classification; the compile-level tests only use `vs`, `vs-dark` and `type: light`. |
| R | `crates/theme/src/vscode.rs::tests::package_detection_classifies_variants_and_keeps_partial_failures` | Package compile with a partial failure. |
| R | `crates/theme/src/vscode.rs::tests::standalone_file_detects_declared_appearance` | Single-file compile path. |
| R | `crates/update/src/lib.rs::tests::update_feed_override_requires_an_https_base_url` | Security of the debug feed override. |
| R | `crates/update/src/lib.rs::tests::version_compare` | Pure version table; garbage never counts as newer. |
| R | `crates/update/src/lib.rs::tests::install_kind_detection` | Install-path classification table across OS strings. |
| R | `crates/update/src/lib.rs::tests::artifact_names_match_packaging` | Release-name contract with `scripts/package-*.sh`; weak (restates the format) but the only Rust-side guard. |
| R | `crates/update/src/lib.rs::tests::windows_is_not_misclassified_as_linux` | Windows platform key. |
| R | `crates/update/src/lib.rs::tests::windows_install_is_always_unmanaged` | Windows must not take the Unix managed path. |
| R | `crates/update/src/lib.rs::tests::windows_rejects_platform_specific_updates_before_side_effects` | Windows refusal before any disk side effect. |
| F | `crates/update/src/lib.rs::tests::manifest_parses_with_and_without_files` → `manifest_parses_files_and_rejects_a_manifest_without_them` | Coordinator repair: failed on the baseline (`missing field files`). 4099e720 made `files` required for signed release manifests (`verify` also refuses an empty map), so the bare half encoded the pre-signing contract. Now asserts the bare manifest is refused at parse. |
| R | `crates/update/src/lib.rs::tests::headless_symlink_swap` | Real symlink swap and unstaged refusal. |
| R | `crates/update/src/windows.rs::tests::portable_install_requires_explicit_configuration` | Windows managed-install marker. |
| R | `crates/update/src/windows.rs::tests::changed_staging_file_is_rejected` | `verify` guard at the `apply` entry (sidecar digest). |
| R | `crates/update/src/windows.rs::tests::failed_swap_after_old_image_moved_restores_the_installation` | Crash-safety of the swap with a real sharing violation. |
| R | `crates/update/src/windows.rs::tests::corrupt_copy_is_rejected_before_the_installation_moves` | Copy re-verification inside `swap_into_place`. |
| R | `crates/update/src/windows.rs::tests::backup_recovers_a_missing_installation_and_clears_when_intact` | Recovery after a crash between renames. |
| R | `crates/update/src/windows.rs::tests::missing_checksum_is_rejected_before_download` | Mandatory checksum before any request or file. |
| D | `apps/luna/src/auth_cli.rs::tests::signed_out_local_status_is_healthy` | Keeper `apps/luna/tests/local_adoption.rs::bare_candidate_is_local_without_service_variables` runs the real `luna status` on the same Local + SignedOut state and asserts exit 0 (the `healthy` flag) and `Mode: local only`. Only the auth sentence was unique, and it is not a contract. |
| R | `apps/luna/src/auth_cli.rs::tests::synced_status_still_requires_a_complete_account` | Synced unhealthy state (exit 1) has no CLI-level proof: it needs a WorkOS session. |
| R | `apps/luna/src/auth_cli.rs::tests::login_and_logout_lock_out_a_running_engine` | Lockout message while an engine holds the data-dir lock; the CLI suite only covers refusal before the lock. |
| R | `apps/luna/src/auth_cli.rs::tests::logout_explicitly_removes_a_legacy_session_without_migrating_it` | Real `logout` against a temp data dir. |
| R | `apps/luna/src/daemon.rs::tests::systemd_unit_shape` | Unit-file contract, including systemd quote escaping. |
| F | `apps/luna/src/daemon.rs::tests::captured_app_identity_is_luna_owned` | Renamed `service_env_keeps_the_data_root_and_never_the_bearer`. The rebrand commit 68e63c9f rewrote its `ZERON_*` negatives to `LUNA_*`, so it asserted both `contains` and `!contains` for the same keys and failed on every platform. Repaired to its original contract: data dir, IPC port and edge URL are captured, the bearer is never written. |
| R | `apps/luna/src/daemon.rs::tests::installed_exe_uses_the_current_symlink` | Managed-install ExecStart path. |
| R | `apps/luna/src/daemon.rs::tests::launchd_plist_shape` | Plist contract, including XML escaping. |
| R | `apps/luna/src/main.rs::log_file_tests::second_launch_never_rotates_a_live_processes_log` | 2026-08-04 log-clobber regression with real `flock`. |
| R | `apps/luna/src/native_fixture.rs::tests::mock_task_seed_is_idempotent_and_stays_in_fixture_root` | The CLI fixture test runs twice but never counts spaces, chats and sessions; this is the only no-duplication proof. |
| R | `apps/luna/src/native_fixture.rs::tests::marker_and_private_temp_root_are_required` | Fixture-root safety guards; the CLI covers the happy path only. |
| R | `apps/luna/src/native_fixture.rs::tests::no_argument_route_requires_exact_prepared_qa_bundle` | QA bundle routing guards. |
| R | `apps/luna/src/native_fixture.rs::tests::qa_bundle_symlink_and_malformed_path_fail_closed` | Symlink and malformed-path refusal. |
| R | `apps/luna/src/paths.rs::tests::explicit_data_dir_needs_no_home` | Explicit data dir without HOME. |
| F | `apps/luna/src/paths.rs::tests::default_and_legacy_override_stay_luna_owned` | Renamed `default_is_home_luna_and_explicit_data_dir_wins`. The same rebrand turned the legacy `ZERON_DATA_DIR` row into `LUNA_DATA_DIR`, so it asserted that the explicit override is ignored, contradicting production and the test above; it failed on macOS and Linux. Repaired to the real contract: HOME default, explicit `LUNA_DATA_DIR` wins over HOME (what QA worktree isolation relies on). |
| R | `apps/luna/src/paths.rs::tests::explorer_launch_without_home_uses_local_app_data` | Windows default root. |
| R | `apps/luna/src/paths.rs::tests::windows_profile_fallback_handles_unicode_and_apostrophes` | Windows USERPROFILE fallback. |
| R | `apps/luna/src/paths.rs::tests::windows_default_does_not_depend_on_shell_home` | Windows ignores a shell HOME. |
| R | `apps/luna/tests/inspect_cli.rs::reads_exact_synthetic_engine_and_reports_bounded_targets` | Real binary over a real engine: inspect JSON contract, cursors, exit codes. |
| R | `apps/luna/tests/inspect_cli.rs::reports_unavailable_malformed_and_engine_failures` | Exit-code and error-kind table. |
| R | `apps/luna/tests/inspect_cli.rs::inspect_json_parse_errors_are_machine_readable_without_changing_help_or_other_commands` | Parse-error JSON and the boundaries around it. |
| R | `apps/luna/tests/local_adoption.rs::implicit_local_cli_refuses_a_copied_device_id_at_another_root` | Root-claim binding through the real binary. |
| R | `apps/luna/tests/local_adoption.rs::bare_candidate_is_local_without_service_variables` | Keeper for local-only status (absorbs the deleted `account_status` unit). |
| R | `apps/luna/tests/local_adoption.rs::explicit_loopback_development_config_is_available_without_a_network_probe` | Development mode via loopback edge. |
| R | `apps/luna/tests/local_adoption.rs::partial_or_luna_service_configuration_fails_closed` | Fail-closed service configuration table. |
| R | `apps/luna/tests/local_adoption.rs::local_candidate_refuses_login_without_owned_sync` | Login refusal without owned sync. |
| R | `apps/luna/tests/local_adoption.rs::actual_binary_previews_and_restores_only_its_synthetic_original_root` | Backup preview and restore through the binary. |
| R | `apps/luna/tests/local_adoption.rs::actual_binary_prepares_mock_only_native_fixture_without_a_window` | Native fixture `--check-only`, run twice. |
| R | `apps/luna/tests/remote_cli.rs::remote_cli_issues_lists_and_revokes_one_real_engine_session` | Pair, list and revoke through the binary and a real engine. |
| R | `apps/luna/tests/ssh_cli.rs::ssh_cli_enrolls_reconnects_rejects_wrong_target_and_stops_only_owned` | Main SSH end-to-end over fake system SSH. |
| R | `apps/luna/tests/ssh_cli.rs::ssh_cli_rejects_local_checksum_and_version_before_transport` | Local checksum and version refusal. |
| R | `apps/luna/tests/ssh_cli.rs::foreign_platform_artifact_is_never_executed_on_client` | Security: a foreign artifact never runs on the client. |
| D | `apps/luna/tests/ssh_cli.rs::persistent_remote_read_view_is_available` | Greps `luna ssh view --help` for `--task`. The keepers `remote_read_window_exit_cancels_a_pending_ssh_connect`, `held_target_lock_never_blocks_read_view_exit` and the main SSH test run `luna ssh view ... --task ...` and observe its `connecting`/`connected` states, which fail if the subcommand or flag disappears. |
| R | `apps/luna/tests/ssh_cli.rs::remote_read_window_exit_cancels_a_pending_ssh_connect` | Cancel on exit; keeper for the `ssh view` surface. |
| R | `apps/luna/tests/ssh_cli.rs::held_target_lock_never_blocks_read_view_exit` | Busy target fails before SSH side effects. |
| R | `apps/luna/tests/ssh_cli.rs::l30_native_qa_fixture_server` | Ignored QA server the native QA parent starts; a harness, not a CI test. |
| R | `apps/luna/tests/ssh_remote_script.rs::staged_checksum_version_reuse_and_owned_stop` | The shipped POSIX helper itself against a disposable HOME. |
| R | `apps/luna-headless/tests/pair.rs::minimal_binary_issues_one_time_pairing_for_exact_engine` | Headless pairing and mismatched-target revocation through the binary. |
| R | `apps/luna-headless/tests/startup.rs::packaged_headless_startup_guards_remote_socket_and_shutdown` | Packaged startup, remote-socket guard, clean stop. |
| R | `apps/luna-headless/tests/startup.rs::second_synthetic_headless_cannot_bind_same_remote_listener` | Remote listener exclusivity. |
| R | `apps/luna-relay/src/server.rs::tests::host_cannot_register_after_configuration_revoked_between_checks` | Admission-to-registration race; the integration suite cannot schedule this window. |
| R | `apps/luna-relay/src/server.rs::tests::client_reservation_observes_reload_and_releases_route_count` | Route-count leak on reload. |
| R | `apps/luna-relay/src/server.rs::tests::client_cannot_reserve_after_admission_revision_is_revoked` | Stale admission at reservation. |
| R | `apps/luna-relay/src/server.rs::tests::client_reservation_observes_replaced_host_even_when_subscribed_late` | Late-subscription fence on host replacement. |
| R | `apps/luna-relay/src/server.rs::tests::stale_bridge_cannot_deliver_after_reload` | Bridge after reload; `tests/routing.rs` covers replacement, not reload, for bridges. |
| R | `apps/luna-relay/src/server.rs::tests::old_host_cleanup_does_not_remove_new_generation` | `HostLease` generation check; `replacement_host_fences_old_pending_bridge_and_client` never checks the new host stays registered. |
| R | `apps/luna-relay/src/server.rs::tests::already_signaled_stop_closes_proxy_without_waiting_for_another_change` | Pre-signaled stop fence. |
| R | `apps/luna-relay/src/server.rs::tests::already_changed_revision_closes_proxy_before_first_frame` | Pre-changed revision fence. |
| R | `apps/luna-relay/src/server.rs::tests::failed_pong_write_removes_host_and_pending_routes` | Write failure cleanup. |
| R | `apps/luna-relay/src/server.rs::tests::failed_ready_write_removes_new_host` | Ready-write failure cleanup. |
| F | `apps/luna-relay/src/server.rs::tests::stalled_control_write_has_a_deadline` | Depended on a `#[cfg(test)]` 75 ms `CONTROL_WRITE_TIMEOUT` (production 5 s). Seam removed; the test now waits on the production deadline (`CONTROL_WRITE_TIMEOUT + 1s`) and also requires the timed-out send to return an error. About 5 s slower. |
| F | `apps/luna-relay/src/server.rs::tests::stop_interrupts_stalled_heartbeat_ping` | Passed vacuously: the test-only 75 ms write timeout fired inside its 200 ms window whether or not stop was observed. With the seam gone (5 s deadline) only the stop path can finish in time. Repaired by the seam removal; test text unchanged. |
| R | `apps/luna-relay/src/server.rs::tests::slow_consumer_closes_bridge_instead_of_growing_buffer` | Bounded data writes (uses the unchanged 5 s data deadline). |
| R | `apps/luna-relay/src/server.rs::tests::missing_host_heartbeat_expires_and_removes_route` | Heartbeat expiry (injected period). |
| R | `apps/luna-relay/src/tunnel.rs::tests::malformed_host_control_joins_pending_bridge_before_return` | Pending bridge joined before return. |
| R | `apps/luna-relay/src/tunnel.rs::tests::stopped_host_interrupts_stalled_control_pong` | Stop interrupts a stalled host write (tunnel has no timing seam). |
| R | `apps/luna-relay/src/tunnel.rs::tests::already_stopped_host_skips_control_write` | Pre-stopped host never writes. |
| R | `apps/luna-relay/tests/routing.rs::exact_route_keeps_l29_principals_separate_and_revocation_closes_active_routes` | Relay keeper: principals, scopes, revocation, replacement, config reload. |
| R | `apps/luna-relay/tests/routing.rs::host_rejects_local_owner_ipc_even_with_valid_relay_credentials` | Host refuses an unguarded local-owner socket. |
| R | `apps/luna-relay/tests/routing.rs::route_authentication_rejects_wrong_role_target_and_replayed_challenge` | Admission security table, replay and Origin rejection. |
| R | `apps/luna-relay/tests/routing.rs::replacement_host_fences_old_pending_bridge_and_client` | Generation fencing over real sockets. |
| R | `apps/luna-relay/tests/routing.rs::actual_relay_host_client_binaries_start_route_and_stop` | The three shipped binaries end to end. |
| R | `apps/luna-relay/tests/routing.rs::config_rejects_scope_sprawl_and_reused_roles` | Config validation table. |
| R | `edge/src/chat-frames.test.ts::chat2 frame codec::pins the wire layout: [type u8][headerLen u32 LE][header][payload]` | Cross-language (Rust, Swift) chat2 frame layout vector. |
| R | `edge/src/chat-frames.test.ts::chat2 frame codec::round-trips every frame type, with and without payload` | Every frame type and empty payloads. |
| R | `edge/src/chat-frames.test.ts::chat2 frame codec::round-trips a subarray view (offset ≠ 0 — the ws buffer case)` | WebSocket buffer-offset regression. |
| R | `edge/src/chat-frames.test.ts::chat2 frame codec::rejects malformed frames as undefined, never throws` | Malformed-input table. |
| R | `edge/src/chat-frames.test.ts::chat2 frame codec::rejects oversized headers (payloads are unbounded here; the DO caps frames)` | Header bound. |
| R | `edge/src/device-frame.test.ts::device frame codec::round-trips header + payload` | Only edge-side proof of the device frame codec (`crates/engine/tests/device_routing.rs` uses a Rust fake relay). Weak: TS-only round trip; see Follow-ups. |
| R | `edge/src/device-frame.test.ts::device frame codec::handles empty payloads and long headers` | Multi-byte varint length (200-char header) and empty payload. |
| R | `edge/src/device-host-liveness.test.ts::device-room host selection::prefers the live host over an older corpse listed first` | `pickLiveHost` regression case; per-row `it` already acts as the table, each row carries its bug note. |
| R | `edge/src/device-host-liveness.test.ts::device-room host selection::still finds the live host when the corpse is listed last` | Order independence. |
| R | `edge/src/device-host-liveness.test.ts::device-room host selection::picks the freshest of several live hosts` | Freshest wins. |
| R | `edge/src/device-host-liveness.test.ts::device-room host selection::reports no host when every socket is stale — clients get host_offline` | Stale cutoff. |
| R | `edge/src/device-host-liveness.test.ts::device-room host selection::treats a socket attached before this deploy (no timestamps) as dead` | Legacy socket. |
| R | `edge/src/device-host-liveness.test.ts::device-room host selection::keeps a just-joined host that has not pinged yet` | Join grace. |
| R | `edge/src/device-host-liveness.test.ts::device-room host selection::has no host in an empty room` | Empty room. |
| R | `edge/src/device-host-liveness.test.ts::device-room host selection::skips the socket whose close is being handled` | Close-handling regression. |
| R | `edge/src/device-host-liveness.test.ts::device-room host selection::still finds a successor when the departing socket is excluded` | Successor selection. |
| R | `edge/src/preview-room.test.ts::preview coordinator admission::accepts bounded service metadata and strips extra fields` | Catalog admission security; the workerd preview suite never sends a malformed catalog. |
| R | `edge/src/preview-room.test.ts::preview coordinator admission::only relays pairing and authenticated SDP/ICE, never proxy frames` | Signal filter security; not exercised by the workerd suite. |
| R | `edge/src/registry-core.test.ts::hlc::orders by (ms, counter, device) lexicographically` | HLC ordering and fixed width. |
| R | `edge/src/registry-core.test.ts::applyOp::creates rows via upsert, never via update` | Keeper for "update never creates a row". |
| R | `edge/src/registry-core.test.ts::applyOp::field-level LWW: newer clock wins, older loses, ties lose` | LWW per field; replay idempotence. |
| R | `edge/src/registry-core.test.ts::applyOp::same-ms conflicts settle by device id, deterministically` | Deterministic tie-break. |
| R | `edge/src/registry-core.test.ts::applyOp::null field values delete the field (still clocked)` | Clocked field deletion. |
| R | `edge/src/registry-core.test.ts::applyOp::delete tombstones only when causally newer than the row` | Tombstone rules and revival. |
| R | `edge/src/registry-core.test.ts::applyOp::delete on a missing row plants a guard tombstone` | Guard tombstone. |
| R | `edge/src/registry-core.test.ts::applyOp::per-field clock overrides (re-seed) preserve original causality` | Re-seed causality. |
| R | `edge/src/registry-core.test.ts::applyOp::rowToSeedOp round-trips tombstones` | Seed op for tombstones. |
| R | `edge/src/registry-core.test.ts::applyOp::converges over every causally-valid arrival order` | Convergence over permutations. |
| D | `edge/src/registry-core.test.ts::applyOp::drops an update that outruns its row's create (never invent rows)` | Same `applyOp(undefined, update)` → unchanged assertion as the keeper `creates rows via upsert, never via update` (which also asserts no row). Its second half only re-creates a row with `upsert`. |
| R | `edge/src/registry-core.test.ts::validateOp::accepts well-formed ops and rejects malformed ones` | Op validation table. |
| R | `edge/src/registry-core.test.ts::maxClock::returns the newest clock across fields and tombstone` | `maxClock` over fields and tombstone. |
| R | `edge/src/session-doc/generated-images.test.ts::generated image parts::preserves only the structured reference through render/doc conversion` | Doc-schema conversion. |
| R | `edge/src/session-doc/generated-images.test.ts::generated image parts::keeps images atomic through split/join` | Images never split. |
| R | `edge/src/session-doc/generated-images.test.ts::generated image parts::degrades malformed references without rendering paths as text` | Security: malformed references degrade to an error part. |
| D | `edge/src/update-log.test.ts::update log chunking::stores a small update as a single non-continuation row` | FakeSql twin. Keeper `edge/test/workerd/update-log.workerd.test.ts::keeps interleaved small and chunked updates in seq order` reads small updates back from real DO SQLite; the `cont` column value is storage layout. |
| D | `edge/src/update-log.test.ts::update log chunking::splits an update above the row cap and reassembles it byte-identically` | FakeSql twin of `a whale update above the row cap round-trips byte-identically`, where the real ~2 MB cap (pinned by the cap probe) rejects any oversized chunk. |
| D | `edge/src/update-log.test.ts::update log chunking::keeps interleaved small and chunked updates in order` | Exact FakeSql twin of the workerd test of the same name. |
| C | `edge/src/update-log.test.ts::update log chunking::respects a subarray view's offset when slicing chunks` | Moved into `edge/test/workerd/update-log.workerd.test.ts` (real DO SQLite), the stronger keeper; the workerd tier had no offset case. |
| D | `edge/src/update-log.test.ts::update log chunking::migrates a legacy table and reads its rows as one update each` | FakeSql twin of `migrates a legacy pre-cont table via real ALTER TABLE`, which runs the real ALTER. |
| D | `edge/src/update-log.test.ts::update log chunking::ensureUpdateLog is idempotent on an already-migrated table` | The workerd migration test calls `ensureUpdateLog` again on the migrated table (same swallowed duplicate-column ALTER). |
| R | `edge/src/update-log.test.ts::fold path over chunked rows::replays a chunked whale update, folds it into a snapshot, and keeps appending` | Real LoroDoc over chunked rows; loro-wasm cannot load in the workerd pool, so this FakeSql test is the only fold proof. |
| R | `edge/test/workerd/chat-log.workerd.test.ts::chat2 log on real DO SQLite::appends assign dense seqs and read back byte-identical` | chat2 log on real DO SQLite. |
| R | `edge/test/workerd/chat-log.workerd.test.ts::chat2 log on real DO SQLite::batchId replay dedupes to the original seq without appending` | Re-push idempotence. |
| R | `edge/test/workerd/chat-log.workerd.test.ts::chat2 log on real DO SQLite::caps rows at 1MB — comfortably under the platform's ~2MB value cap` | Row cap on the real platform. |
| R | `edge/test/workerd/chat-log.workerd.test.ts::chat2 log on real DO SQLite::rowsAfter honors the cursor and the exclude-own-device reconnect path` | Cursor and own-device exclusion. |
| R | `edge/test/workerd/chat-log.workerd.test.ts::chat2 log on real DO SQLite::checkpoint commit prunes covered rows and stores blob + frontier` | Checkpoint commit. |
| R | `edge/test/workerd/chat-log.workerd.test.ts::chat2 log on real DO SQLite::checkpoint guard is floor-monotonic and head-bounded` | Checkpoint guards. |
| R | `edge/test/workerd/chat-log.workerd.test.ts::chat2 log on real DO SQLite::a churn cycle stays bounded: push → checkpoint → push never accumulates` | Bounded churn. |
| R | `edge/test/workerd/preview.workerd.test.ts::authenticated preview coordination on workerd::checks organization membership before routing` | Org membership on the real route. |
| R | `edge/test/workerd/preview.workerd.test.ts::authenticated preview coordination on workerd::stamps sender identity, isolates users, removes departed devices and rejects bytes` | Sender stamping, isolation and presence on a real DO. |
| R | `edge/test/workerd/update-log.workerd.test.ts::update log on real DO SQLite::the platform still caps SQL values around 2MB (the assumption FakeSql hardcodes)` | Platform cap probe. |
| R | `edge/test/workerd/update-log.workerd.test.ts::update log on real DO SQLite::a whale update above the row cap round-trips byte-identically` | Chunking keeper. |
| R | `edge/test/workerd/update-log.workerd.test.ts::update log on real DO SQLite::keeps interleaved small and chunked updates in seq order` | Ordering keeper. |
| R | `edge/test/workerd/update-log.workerd.test.ts::update log on real DO SQLite::migrates a legacy pre-cont table via real ALTER TABLE` | Migration and idempotence keeper. |

Support files with no declarations: `edge/test/workerd/fixture.ts` and `edge/test/workerd/env.d.ts`
(workerd entry and bindings, used by both workerd suites), `edge/vitest.config.ts` and
`edge/vitest.workerd.config.ts` (tier split); all unchanged.

## Layer plan

| Contract | Keeper | Retired or changed |
|---|---|---|
| Discovered dev servers are probed once | `crates/preview/tests/discovery.rs::a_discovered_server_is_probed_once_not_every_cycle` (real Python server, request log, CI on ubuntu and macOS) | Deleted the `ProbeMemory` replay; backoff, reuse and cap rows stay inline (no other proof). |
| Rust highlight roles | `crates/syntax/tests/quality.rs::rust_reference_span_snapshot` | Deleted two inline/integration role subsets and the `highlighted_fragments` helper. |
| Every bundled grammar and query loads | `crates/syntax/src/lib.rs::tests::every_registered_grammar_and_query_loads_for_the_bundled_abi` | Gained a Rust row; deleted the four-language subset and the Rust-only ABI test. |
| Span validity on broken Unicode input | `crates/syntax/tests/quality.rs::incomplete_unicode_code_keeps_spans_valid_and_non_overlapping` | Gained the Rust source row; deleted its inline re-implementation. |
| Local-only `luna status` is healthy | `apps/luna/tests/local_adoption.rs::bare_candidate_is_local_without_service_variables` | Deleted the `account_status` unit for that state. |
| `luna ssh view` exists with `--task` | `apps/luna/tests/ssh_cli.rs::remote_read_window_exit_cancels_a_pending_ssh_connect` and siblings | Deleted the `--help` grep. |
| Update-log storage (chunking, order, offsets, migration, platform cap) | `edge/test/workerd/update-log.workerd.test.ts` (real DO SQLite) | Retired the FakeSql storage layer: five twins deleted, the subarray case moved to workerd; FakeSql reduced to the statements the Loro fold test needs (legacy-table mode removed). |
| Registry update never invents rows | `edge/src/registry-core.test.ts::applyOp::creates rows via upsert, never via update` | Deleted the duplicate. |

- Assertions carried into keepers: the Rust ABI row (syntax table), the Rust broken-Unicode row
  (quality table), the subarray-offset case (workerd update log).
- Seams removed: `#[cfg(test)] const CONTROL_WRITE_TIMEOUT = 75ms` / `#[cfg(not(test))]` in
  `apps/luna-relay/src/server.rs` (production and test now share the 5 s deadline; `rg` shows no other
  reference). Integration tests already linked the non-test value.
- Seams considered and kept: `ProbeMemory` (`pub(crate)`, production caller), `detect_install_from_for_os`
  and `open_log_file_in` (production callers pass the real OS or directory), `pickLiveHost` export
  (production caller in `device-room.ts`; moving the cases to a workerd DeviceRoom fixture is a
  follow-up, not a deletion), `VisualFixture` (referenced by `docs/theme-system.md`).

## Cross-lane requests

- None required for compilation: no edited item is referenced outside this lane's files.
- CI routing (`.github/workflows/*` owner): nothing moved or renamed that a workflow names. FYI for the
  Windows job (`cargo test -p luna -p luna-update`): it compiles
  `apps/luna/tests/ssh_cli.rs` and `apps/luna/tests/ssh_remote_script.rs`, which import
  `std::os::unix` without a `#![cfg(unix)]` gate (see Follow-ups); and the daemon test repaired here
  was failing on every platform, so that job should turn greener after this lane merges.
- Docs owner: `docs/theme-system.md` remains the only consumer of `luna_theme::VisualFixture`; if the
  checklist moves to prose, the enum can be deleted.

## Possible product defects

- `crates/update/src/lib.rs::tests::manifest_parses_with_and_without_files` is expected to fail on the
  baseline. Commit 4099e720 replaced `#[serde(default)]` on `Manifest::files` with the new
  `#[serde(skip)] verified` field, so `{"version":"0.1.1"}` now fails with a missing-field error.
  `validate_manifest` already rejects an empty `files`, so either restoring `#[serde(default)]`
  (product) or asserting that a manifest without files is rejected (test) is safe; intent is unclear,
  so the test is retained unchanged.
- Two baseline failures were test-side corruption, not product defects, and are repaired as F:
  `apps/luna/src/daemon.rs` (`captured_app_identity_is_luna_owned` asserted `contains` and
  `!contains` for the same keys) and `apps/luna/src/paths.rs`
  (`default_and_legacy_override_stay_luna_owned` asserted that `LUNA_DATA_DIR` is ignored). Both came
  from the 68e63c9f rebrand rewriting `ZERON_*` legacy names to `LUNA_*`; the pre-rebrand versions in
  69397256 show the original intent. Expect both to fail at `e4a0f534` and pass on this branch.

## Follow-ups

- `apps/luna/tests/ssh_cli.rs` and `apps/luna/tests/ssh_remote_script.rs` need `#![cfg(unix)]`: they
  use `std::os::unix` unconditionally, so `cargo test -p luna` cannot compile them on Windows. Not
  changed here because it is a build-gating fix this lane cannot verify.
- `edge/src/device-frame.test.ts` only round-trips through the TypeScript codec. Pin one byte vector
  shared with `luna_engine::encode_device_frame` (varint header length, JSON header, payload) so
  Rust/edge layout drift fails a test, as `chat-frames.test.ts` already does for chat2.
- `crates/preview/src/peer.rs::tests::actual_webrtc_pair_streams_in_both_directions` says an asset burst
  "must pair once" but never counts signals; a signal counter on the forwarders would make that
  claim testable.
- `crates/syntax/src/lib.rs`: `supports_language` always returns `true`, so
  `HighlightError::GrammarUnavailable` is unreachable; `limits_and_unbundled_languages_degrade_with_typed_errors`
  cannot exercise it. Candidate dead-code removal for the syntax owner.
- `crates/update/src/lib.rs::tests::artifact_names_match_packaging` restates the `format!` it checks;
  the stronger proof is `scripts/verify-updates-e2e.sh`, which is not in CI.
