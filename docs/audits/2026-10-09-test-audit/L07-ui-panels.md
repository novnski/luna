# L07 — UI panels test audit

- **Lane:** L07-ui-panels (test-audit campaign 2026-10-09), branch `test-audit/L07-ui-panels`.
- **Scope:** 41 `crates/ui/src` files of the panel family — settings, files, terminal, changes (incl. the Git/PR held-RPC lifetime suites), theme, appshots, browser and appearance — their inline tests and the test-only seams in their production code.
- **Baseline (pinned `e4a0f534`):** 402 test declarations; 12,001 non-blank test lines (`#[cfg(test)]` modules plus the six dedicated `*_tests.rs` files). Baseline pass/fail was run by the coordinator; this lane did not build or run tests.
- **After:** 357 declarations; 11,560 non-blank test lines. Marks: **R 353 · F 4 · C 16 · D 29**.
- **Diff `e4a0f534..` (code commit):** tests +189 / −690; production +1 / −8 (one `cfg` line in `appshots/shortcut.rs`; dead `Theme::HEADER_HEIGHT` and `Theme::for_appearance` in `theme.rs`); docs: this ledger.

Paths below are relative to `crates/ui/src/`. Table-driven macro suites are one row.

## Ledger

| Mark | Test | Evidence |
| --- | --- | --- |
| C | `appearance.rs::tests::system_mode_follows_the_os` | Second half of the same 6-row `resolve` table; folded into `system_mode_follows_the_os_and_pinned_modes_ignore_it` (one loop over both OS appearances). |
| R | `appearance.rs::tests::pinned_modes_ignore_the_os` | Keeper, renamed `system_mode_follows_the_os_and_pinned_modes_ignore_it`: whole `resolve(mode, system)` table; catches a pinned mode following the OS or System ignoring it. |
| D | `appearance.rs::tests::default_mode_is_system` | Asserts `#[default]` attribute only. Remaining proof: `settings::tests::partial_legacy_settings_keep_their_values_and_default_new_keys` loads a pre-appearance file and asserts `AppearanceMode::System` through the real `UiSettings::load` path (a mutated default fails it). Unlocks nothing. |
| R | `appearance.rs::tests::mode_serialises_stably` | `ui-settings.json` wire strings system/light/dark both directions; config contract no other test pins (round_trip only compares structs). |
| D | `appshots.rs::tests::fake_capture_permission_recovery_preserves_source_and_image_text_pair` | Junk: the test-local `FakeCaptureBackend` implements the permission state machine the test asserts; only production calls are a static `setup_description` substring and `with_appshots` escaping, owned by `appshot_context_is_escaped_and_strip_safe` / `queued_edit_round_trip_keeps_context_and_rebinds_uploaded_images`. Real permission flow needs TCC, no unit contract. Unlocks the 50-line fake backend. |
| R | `appshots.rs::tests::capture_staging_removes_chrome_backing_surface_padding_before_layout` | Regression from a real Chrome capture: transparent backing columns trimmed, sRGB kept, aspect feeds composer layout. |
| R | `appshots.rs::tests::padding_trim_preserves_rounded_corners_faint_pixels_and_interior_transparency` | Image transform with real input space: alpha=1 edges, corner and interior holes survive trimming. |
| R | `appshots.rs::tests::padding_trim_keeps_opaque_margins_and_empty_images_byte_identical` | No-op path stays byte-identical for opaque/fully transparent images (no re-encode drift). |
| R | `appshots.rs::tests::padding_trim_preserves_16_bit_color_and_alpha_precision` | 16-bit PNG trimmed without precision loss; distinct decoder branch. |
| R | `appshots.rs::tests::restored_queue_appshots_trim_legacy_padding_without_changing_attachment_identity` | Migration: queued legacy padded appshots are trimmed on restore while the upload id and original bytes stay immutable. |
| R | `appshots.rs::tests::png_dimensions_reads_ihdr_and_rejects_invalid_images` | IHDR parser incl. zero-width rejection; used by attachments/transcript sizing. |
| R | `appshots.rs::tests::capture_dimensions_and_names_are_bounded_before_staging` | Security bound: dimension/byte caps and filename sanitising before staging. |
| R | `appshots.rs::tests::appshot_context_is_escaped_and_strip_safe` | Prompt-byte contract: XML escaping of app/bundle/title/path/AX text and display stripping. Absorbed the empty-prompt row. |
| R | `appshots.rs::tests::appshot_invalid_xml_characters_round_trip_through_queue_and_presentation` | Invalid XML chars replaced consistently across encode, presentation and queue restore. |
| R | `appshots.rs::tests::queued_edit_round_trip_keeps_context_and_rebinds_uploaded_images` | Queued-edit round trip with and without attachment wrapper; images rebound to new paths. |
| R | `appshots.rs::tests::queued_edit_rejects_invalid_or_unmatched_context_without_losing_images` | Malformed/duplicated/unmatched context rejected; plain text keeps ordinary images. |
| C | `appshots.rs::tests::empty_prompt_round_trips_to_empty_display` | Same `with_appshots` + `strip_context_for_display` pair as the escaping keeper; folded into `appshot_context_is_escaped_and_strip_safe` as an Appshot-only row. |
| D | `appshots.rs::tests::wayland_capabilities_explain_picker_and_system_shortcut_fallbacks` | Junk: substring greps of static `match` copy plus `is_ready(UserSelection)`, which only picks a badge colour in settings/appshots.rs. No behaviour; settings-gallery fixture renders the copy for review. |
| D | `appshots/linux/atspi.rs::tests::window_identity_never_uses_fuzzy_titles_or_application_names` | Junk: asserts `#[derive(PartialEq)]` on a 3-field struct; name promises `select_window`'s exact-title/PID rule, which it never calls (needs a live AT-SPI bus; no unit contract). Unlocks the test-only `identity` helper. |
| R | `appshots/linux/atspi.rs::tests::hung_enrichment_releases_the_capture_pipeline` | `within_deadline` returns an error for a never-ready future and the value otherwise; guards the capture service against a hung D-Bus peer. |
| R | `appshots/linux/mod.rs::tests::activation_path_stays_inside_data_directory` | Path/security contract: the `luna appshot` activation socket lives inside the data dir, not a shared temp path. |
| R | `appshots/linux/portal.rs::tests::cancellation_survives_both_portal_error_paths` | Dependency mapping: both ashpd cancellation shapes become `CaptureError::Cancelled`; other errors stay failures (ashpd upgrade guard). |
| R | `appshots/macos.rs::tests::fallback_rejects_oversized_or_invalid_native_acquisition` | Bounds/NaN/zero rejection before native acquisition (security/memory bound). |
| R | `appshots/macos.rs::tests::expired_ax_budget_never_calls_the_native_element` | Deadline guard returns before any AX FFI call (null element would crash otherwise). |
| R | `appshots/macos.rs::tests::focused_document_wins_over_onscreen_helpers_and_other_documents` | `same_window_bounds` picks the focused document over Chromium helpers; tiny windows rejected. No native harness exists. |
| R | `appshots/macos.rs::tests::capture_skips_hidden_helpers_without_rejecting_small_visible_windows` | `visible_capture_window` table: offscreen, zero/NaN alpha, missing bounds rejected; small visible kept. |
| R | `appshots/macos.rs::tests::capture_dimensions_use_retina_scale_for_ordinary_windows` | Keeper, renamed `capture_dimensions_use_retina_scale_and_cap_large_windows`: one table for `capture_dimensions` (2x scale, 4096 cap). |
| C | `appshots/macos.rs::tests::capture_dimensions_cap_large_fullscreen_windows` | Second row of the same pure function; folded into the retina keeper table. |
| R | `appshots/macos.rs::tests::secure_text_subroles_are_redacted` | Security: AXSecureTextField role/subrole redacted from Appshot text. |
| R | `appshots/shortcut.rs::tests::global_shortcuts_require_a_supported_modified_key` | Parser table: unmodified, fn-modified and unknown keys refused for global hotkeys. |
| R | `appshots/shortcut.rs::tests::portal_uses_xkb_key_names_and_selected_modifiers` | XDG portal wire contract (xkb names, modifier order). Now `cfg(target_os = "linux")`: production `portal_trigger` drops its `cfg(any(linux, test))` seam; Linux CI (`ui-tests.yml`) still runs it. |
| R | `browser/mod.rs::tests::address_enter_rejects_unsupported_schemes_and_escape_restores_url` | Real key dispatch: Enter on `javascript:` keeps the page and shows validation; Escape restores the URL and focus. |
| R | `browser/mod.rs::tests::customized_app_shortcuts_win_over_browser_defaults` | Keymap conflict resolution: user-rebound app chords suppress browser defaults; only owner of `bind_keys` precedence. |
| R | `browser/model.rs::tests::normalizes_web_addresses_and_loopback_ports` | Address parser table (loopback, IPv6, scheme inference). |
| R | `browser/model.rs::tests::rejects_non_web_schemes_credentials_and_bad_input` | Security table: javascript/file/data/luna schemes, credentials and control chars refused. |
| D | `browser/model.rs::tests::native_visibility_never_outlives_its_surface` | Private 2-bool truth table duplicated at the real boundary: `browser-fixture` (macOS job and `scripts/test-linux-browser.sh` X11/Wayland in ui-tests.yml) asserts "background page stayed visible", "native page intercepted the resize drag" and "closed tab retained its native view". |
| R | `browser/model.rs::tests::tab_labels_fall_back_to_host_then_browser` | Tab label fallback title -> host -> "Browser"; no fixture asserts labels. |
| R | `changes.rs::tests::editing_staged_diff_comments_preserves_identity_and_cancellation` | Real Changes entity: edit/cancel/commit keep comment id, cite path and focus; only owner of diff-comment editing. |
| R | `changes.rs::tests::parses_files_hunks_and_lines` | Unified-diff parser owner: hunks, counts and line numbering across hunks. |
| R | `changes.rs::tests::detects_new_deleted_binary_and_renamed` | Parser statuses, binary, rename paths and notices. |
| R | `changes.rs::tests::empty_and_garbage_patches_parse_to_nothing` | Parser robustness incl. truncated hunk. |
| R | `changes.rs::tests::quoted_and_spaced_paths` | Quoted git paths; no PATCH fixture row exercises them. |
| R | `changes.rs::tests::hunk_headers_parse_with_and_without_counts` | `@@ -7 +9 @@` count-less headers; not in the shared PATCH. |
| R | `changes.rs::tests::rows_flatten_to_line_granularity` | Row layout math: spans tile, flat indices, collapsed files, notice rows. |
| R | `changes.rs::tests::sticky_header_tracks_the_logical_top_row` | Sticky header ownership table at header boundaries. |
| R | `changes.rs::tests::sticky_header_is_pushed_by_the_next_file` | Push-offset math. |
| R | `changes.rs::tests::sticky_header_uses_the_content_theme_in_dark_and_light` | Retained although it mirrors `sticky_file_header_paint`: its tint != glass_overlay row is the c4a14a8b regression guard. Follow-up: trim the mirrored formulas. |
| R | `changes.rs::tests::split_pairs_align_edits_and_strand_the_rest` | Split-view pairing algorithm incl. -a +b -c +d blocks. |
| R | `changes.rs::tests::no_newline_markers_keep_their_edit_paired` | Regression: double no-newline markers keep the edit paired. |
| R | `changes.rs::tests::split_flattening_pairs_rows_and_keeps_heights_analytic` | Split rows and analytic split height. |
| R | `changes.rs::tests::capped_pairing_agrees_with_the_full_pairing_and_stays_bounded` | Perf bound: capped pairing is a true prefix and does not materialise 20k rows. |
| R | `changes.rs::tests::a_split_row_offers_each_column_its_own_anchor` | Comment anchors per column; no duplicate context anchor. |
| R | `changes.rs::tests::split_rows_carry_the_comments_of_both_columns` | Comment cards attach once and in column order. |
| R | `changes.rs::tests::a_split_rows_right_column_is_never_a_deletion` | Invariant behind the `+` placement (notes cite editable lines). |
| R | `changes.rs::tests::truncate_caps_lines_and_appends_notice` | Truncation cap + notice; owns analytic `body_height` (notice + hunk + line + pad). |
| R | `changes.rs::tests::gutters_fit_the_largest_line_number` | Gutter width math (accent-bar gap regression). |
| R | `changes.rs::tests::horizontal_geometry_counts_tabs_and_unicode_columns` | Tab/CJK/combining column math. |
| R | `changes.rs::tests::horizontal_content_width_compensates_for_local_gutters` | Unified/split total width invariant across gutter widths. |
| R | `changes.rs::tests::native_diff_font_geometry` | Name-routed in CI (ui-tests.yml macOS, windows.yml); real font metrics regression. Must keep this exact name. |
| R | `changes.rs::tests::horizontal_scroll_and_width_are_independent_per_file` | Per-file scroll/width independence and shared row offset. Absorbed the slot-id uniqueness row. |
| D | `changes.rs::tests::horizontal_scroll_reset_returns_to_origin` | Calls a one-line wrapper over gpui `ScrollHandle::set_offset(default)`; asserts gpui behaviour, not the caller contract (mode/scope switches reset scroll), which no test covered before or after. Follow-up for an entity-level test. |
| C | `changes.rs::tests::horizontal_scroll_slots_share_offset_but_keep_unique_ids` | Shared offset already asserted by `horizontal_scroll_and_width_are_independent_per_file`; its unique-id assertion was folded there. |
| D | `changes.rs::tests::body_height_is_analytic` | Duplicate contract: `truncate_caps_lines_and_appends_notice` asserts body_height = notice + hunk headers + lines + pad and `split_flattening_pairs_rows_and_keeps_heights_analytic` the untruncated split height; `rows_flatten_to_line_granularity` covers Meta rows via body_row_count. |
| R | `changes.rs::tests::diff_resolution_prefers_checkout_id_then_cwd` | Checkout-id then device+cwd resolution precedence. |
| R | `changes.rs::tests::phases` | Diff phase incl. summarized-without-patch edge. |
| C | `changes.rs::tests::header_label_pluralizes` | `scope_label(WorkingTree)` delegates to `uncommitted_label`; the singular row was folded into `scope_labels_and_clean_messages` (plural already there). |
| R | `changes.rs::tests::scope_labels_and_clean_messages` | Header/empty-state copy with pluralisation and base interpolation. Absorbed the singular row. |
| R | `changes.rs::tests::base_ref_defaults_to_repo_default_then_main` | Base-ref fallback chain table. |
| R | `changes.rs::tests::scope_modes_are_wire_stable` | `GetCheckoutDiff` mode wire strings the engine matches on, plus default scope. |
| F | `changes.rs::tests::diff_scope_menu_keeps_history_as_a_separate_surface` | Copied the `DiffScope::ALL` literal; repaired to assert the exclusion contract (History and Commit never in the scope menu) without restating the list. |
| R | `changes.rs::tests::diff_frames_replace_lists_and_upsert_singles` | Watch frame application: list replace, single upsert, no-op, malformed. |
| R | `changes.rs::tests::full_diff_highlights_map_old_new_and_context_by_source_line` | Old/new/context lines map to the right highlighted document. |
| R | `changes.rs::tests::the_default_code_font_size_reproduces_the_historical_diff_size` | Regression: default code font keeps the 12.0 diff size. |
| R | `changes.rs::tests::scaled_diff_sizes_keep_their_proportions_and_stay_clamped` | Scaling and clamp of diff sizes. |
| R | `changes.rs::tests::split_line_runs_use_affected_old_and_new_documents` | TSX/Kotlin/Dockerfile runs keep highlight colours on both sides. |
| R | `changes.rs::tests::excerpt_parses_old_and_new_hunks_as_separate_documents` | Excerpt highlighting keeps old/new hunks separate (block comment spans). |
| R | `changes.rs::tests::mismatched_full_sources_are_rejected_atomically` | Stale full sources rejected instead of misaligned highlights. |
| R | `changes/git_actions/lifetime_tests.rs::case!{13 rows: git_ui_lifetime_*}` | `case!` table of 13 rows (commit/restore/message x engine/checkout/current/edit). HeldRpc memory-RPC boundary; accepted writes never canceled or replayed through a replacement engine. Strongest boundary for Git UI lifetimes. |
| R | `changes/git_actions/ownership_tests.rs::ownership_case!{5 rows: git_ui_ownership_*}` | `ownership_case!` table of 5 rows: refresh/close-reopen/edit during an accepted write keep busy ownership and settle once. Distinct transitions from lifetime_tests; shared setup is a follow-up. |
| R | `changes/pull_requests/cold_git_preview_tests.rs::pr_ui_cold_git_preview_success_after_engine_replacement` | Row of one `run_case` table: retired cold A preview read settles once and leaves no busy state. |
| R | `changes/pull_requests/cold_git_preview_tests.rs::pr_ui_cold_git_preview_error_after_engine_replacement` | Row of the same table: error variant. |
| R | `changes/pull_requests/cold_git_preview_tests.rs::pr_ui_cold_git_preview_manual_b_refresh_control` | Row of the same table: explicit B refresh is not overwritten by late A. |
| R | `changes/pull_requests/lifetime_tests.rs::late_test!{20 rows: pr_ui_lifetime_*}` | `late_test!` table (preview/details/create x engine/gap/selection/cwd/close/branch/remote/unchanged): accepted create never canceled; late A never mutates the replacement context. |
| R | `changes/pull_requests/lifetime_tests.rs::pr_ui_lifetime_loaded_draft_survives_unchanged_git_refresh` | Unchanged Git refresh keeps the authored title/body and preview authority. |
| R | `changes/pull_requests/lifetime_tests.rs::pr_ui_lifetime_cached_a_preview_cannot_dispatch_through_b` | Security/ownership: cached A preview cannot authorize a B create; fresh B dispatch keeps the user's text. |
| R | `files/client.rs::tests::cached_directory_refresh_collects_pages_but_initial_load_stays_lazy` | `list_directory_snapshot` pagination owner through a scripted transport. |
| R | `files/client.rs::tests::refresh_does_not_expand_unvisited_pages_unless_a_cached_child_is_missing` | Refresh only walks further pages when a cached child is missing. |
| R | `files/client.rs::tests::failed_or_repeated_later_page_does_not_publish_a_partial_refresh` | Transport failure or repeated cursor never publishes a partial listing. |
| D | `files/client.rs::tests::local_params_preserve_the_typed_workspace_shape` | Duplicate: `write_decodes_conflicts_without_changing_request_shape` asserts the local typed shape through the real `call` path, including no `targetDeviceId`. |
| D | `files/client.rs::tests::remote_params_add_only_the_relay_target` | Duplicate: `deterministic_transport_covers_list_search_read_and_watch_routing` asserts chatId + targetDeviceId on all five routed calls; absent spaceId/checkoutPath is luna_proto serde, not a client contract. |
| D | `files/client.rs::tests::protocol_responses_decode_into_the_final_backend_types` | Duplicate: the deterministic transport test decodes the same list/search/read payloads through `WorkspaceFilesClient::call`. |
| F | `files/client.rs::tests::malformed_response_is_reported_as_decode_error` | Vacuous: built `FilesClientError::Decode` itself and asserted it. Repaired to drive `list_directory` over a malformed reply and assert Decode and `!retryable()` (preview.rs/files mod.rs retry only retryable errors). |
| R | `files/client.rs::tests::deterministic_transport_covers_list_search_read_and_watch_routing` | Routing + decode of every client method incl. watch subscription; keeper for request shape. |
| R | `files/client.rs::tests::write_decodes_conflicts_without_changing_request_shape` | Write conflict decode and local request shape keeper. |
| R | `files/client.rs::tests::remote_images_keep_checkout_hash_and_offset_across_chunks` | Chunked image protocol: identity/hash/offset carried across chunks. |
| R | `files/client.rs::tests::image_reads_reject_inconsistent_or_unsupported_responses` | Identity drift and bad offsets rejected. |
| R | `files/client.rs::tests::image_reads_reject_malformed_repeated_and_oversized_chunks` | Malformed/oversized/repeated chunks and missing checkout rejected (memory bound). |
| R | `files/document.rs::tests::programmatic_load_is_clean_and_user_edits_use_revisions` | FileDocument state-machine owner (no RPC-level save test exists): Load is clean; edits bump revision. |
| R | `files/document.rs::tests::stale_key_or_generation_is_rejected` | FileDocument state-machine owner (no RPC-level save test exists): Stale generation/key cannot apply. |
| R | `files/document.rs::tests::unsupported_and_truncated_files_are_read_only` | FileDocument state-machine owner (no RPC-level save test exists): Read-only phases for unsupported/truncated files. |
| R | `files/document.rs::tests::save_snapshot_only_cleans_the_revision_it_captured` | FileDocument state-machine owner (no RPC-level save test exists): Save completion cleans only its captured revision. |
| R | `files/document.rs::tests::save_uses_the_read_checkout_identity_not_the_chat_metadata` | FileDocument state-machine owner (no RPC-level save test exists): Regression: save targets the engine-returned checkout id. |
| R | `files/document.rs::tests::failed_and_conflicting_saves_preserve_dirty_content` | FileDocument state-machine owner (no RPC-level save test exists): Failed/conflicting saves keep dirty content and stop autosave. |
| R | `files/document.rs::tests::stale_save_result_cannot_clean_a_new_request` | FileDocument state-machine owner (no RPC-level save test exists): Wrong-revision completion is ignored. |
| R | `files/document.rs::tests::external_reload_is_clean_but_external_dirty_state_blocks_autosave` | FileDocument state-machine owner (no RPC-level save test exists): External change while dirty blocks autosave. |
| R | `files/document.rs::tests::deletion_preserves_dirty_revision_and_stops_autosave` | FileDocument state-machine owner (no RPC-level save test exists): Deleted-on-disk keeps edits editable but stops autosave. |
| R | `files/document.rs::tests::same_content_recreation_restores_disk_state_without_losing_dirty_edits` | FileDocument state-machine owner (no RPC-level save test exists): Recreated file restores disk state, keeps edits. |
| R | `files/document.rs::tests::explicit_discard_resolves_dirty_state_for_lifecycle_exit` | FileDocument state-machine owner (no RPC-level save test exists): Discard clears dirty state for quit/close. |
| R | `files/editor.rs::tests::menu_availability_tracks_selection_editability_and_clipboard` | Context-menu truth table (cut/copy/paste by selection, read-only, clipboard). |
| R | `files/editor.rs::lifetime_tests::dropping_file_editor_releases_entity_with_context_menu` | Leak regression: closed editor not retained by its menu callback. |
| F | `files/editor_adapter.rs::tests::line_relative_spans_become_absolute_utf8_ranges` | Name promised line-relative translation but its only span sat on line 0 (offset 0). Repaired with a second-line span after a multi-byte first line; dropping the line-start offset now fails. |
| R | `files/editor_adapter.rs::tests::style_runs_are_ordered_non_overlapping_and_cover_the_request` | Style runs tile the requested range for gpui-base. |
| D | `files/editor_adapter.rs::tests::stale_highlight_ranges_are_rejected_before_updated_unicode_is_shaped` | Asserts the hazard (stale runs split a char) plus a one-line `==` wrapper; `install_highlighter` is never exercised. The stale-highlight gate is owned by `preview::tests::file_highlight_completion_rejects_a_result_after_a_user_edit`. |
| D | `files/editor_adapter.rs::tests::editor_caret_uses_the_same_theme_token_as_other_inputs` | Identity copier of the `caret: theme.caret` struct field; caret colour contract owned by theme tests (`every_accent_is_one_coherent_color_identity`). |
| R | `files/image_preview.rs::tests::recognizes_only_supported_workspace_formats` | Extension table for workspace images. |
| R | `files/image_preview.rs::tests::suspension_cancels_load_rejects_late_results_and_releases_images` | Lifecycle: suspension cancels, late results ignored, memory released. |
| R | `files/image_preview.rs::tests::deletion_and_memory_limits_are_visible_and_release_on_disposal` | Memory limit and deletion states visible; release on drop. |
| R | `files/image_preview.rs::tests::image_preview_loads_through_owner_and_resolves_legacy_identity` | Owner-routed load incl. legacy checkout identity lookup. |
| R | `files/image_preview.rs::tests::rendered_workspace_image_click_keeps_zoom_in_the_file_panel` | Linux headless E2E: click keeps focus, ctrl-wheel zooms. |
| R | `files/markdown_media.rs::tests::image_collection_preserves_nested_and_repeated_media` | Nested/repeated Markdown image sources deduplicated in order. |
| D | `files/markdown_preview.rs::tests::preview_retains_mail_links_without_allowing_active_schemes` | Private predicate duplicated at the real boundary: `preview::markdown_buffer_tests::markdown_preview_web_links_keep_the_file_session_and_full_target` routes mailto through `link_ui` (OS handler opens it) and rejects javascript:/credential URLs. |
| R | `files/markdown_preview.rs::tests::comments_map_to_original_lines_and_containing_blocks` | CRLF/Unicode source-line mapping for Markdown comments. |
| R | `files/markdown_preview.rs::tests::markdown_paths_and_links` | Security: relative link resolution refuses traversal and encoded absolute paths. |
| R | `files/markdown_preview.rs::layout_tests::code_fences_scroll_copy_and_share_persistent_fit_preference` | Linux headless: fence scroll, copy confirmation, persisted fit mode. |
| R | `files/markdown_preview.rs::layout_tests::task_click_edits_buffer_and_supports_undo_redo_and_stale_guards` | Linux headless: task click edits the buffer, undo/redo, stale and read-only guards. |
| R | `files/markdown_preview.rs::layout_tests::selection_stays_in_its_document_with_two_markdown_surfaces` | Linux headless: selection scoped to its surface. |
| R | `files/markdown_preview.rs::layout_tests::rendered_image_opens_centered_lightbox_and_escape_restores_focus` | Linux headless: lightbox focus round trip. |
| R | `files/markdown_preview.rs::layout_tests::rendered_mermaid_opens_lightbox_and_toggles_source` | Linux headless: mermaid lightbox/source toggle. |
| R | `files/markdown_preview.rs::async_tests::edits_and_suspension_cancel_obsolete_preview_work` | Obsolete parse work cancelled; suspension releases media. |
| R | `files/markdown_preview.rs::async_tests::raster_admission_accounts_for_cpu_pixels_and_gpu_texture` | Memory admission counts CPU + GPU copies. |
| R | `files/markdown_preview.rs::async_tests::referenced_image_change_invalidates_cache_and_memory_is_bounded` | Cache invalidation and memory bound on target change. |
| R | `files/model.rs::tests::background_refresh_keeps_loaded_rows_even_when_empty_or_failed` | FileTreeModel owner: Background refresh never blanks loaded rows. |
| R | `files/model.rs::tests::changes_during_load_are_remembered_and_collapsed_caches_become_stale` | FileTreeModel owner: Invalidation during load; also owns in-flight load de-duplication. |
| R | `files/model.rs::tests::refreshing_a_directory_preserves_descendants_expansion_and_selection` | FileTreeModel owner: Refresh keeps expansion/selection. |
| R | `files/model.rs::tests::paginated_refresh_prunes_missing_children_only_after_the_last_page` | FileTreeModel owner: Pruning waits for the last page. |
| R | `files/model.rs::tests::refresh_removes_deleted_subtrees_and_handles_directory_becoming_file` | FileTreeModel owner: Dir-to-file replacement removes subtree. |
| R | `files/model.rs::tests::root_page_is_sorted_and_flattened` | FileTreeModel owner: Sort order: directories, files, symlinks. |
| R | `files/model.rs::tests::expanded_directories_add_rows_at_the_correct_depth` | FileTreeModel owner: Row depth for expanded children. |
| R | `files/model.rs::tests::ignored_directories_propagate_ignored_state_to_all_descendants` | FileTreeModel owner: Ignored state inherited. |
| R | `files/model.rs::tests::collapsing_an_ancestor_moves_hidden_selection_to_it` | FileTreeModel owner: Selection follows collapse. |
| R | `files/model.rs::tests::pagination_appends_and_deduplicates_entries` | FileTreeModel owner: Page append dedupe. |
| R | `files/model.rs::tests::directory_pages_reject_self_parents_and_ancestors` | FileTreeModel owner: Robustness: self/ancestor entries ignored. |
| R | `files/model.rs::tests::directory_pages_reject_non_direct_descendants` | FileTreeModel owner: Robustness: grandchildren ignored. |
| R | `files/model.rs::tests::visible_rows_stop_at_cycles_in_a_corrupted_model` | FileTreeModel owner: Cycle guard. |
| C | `files/model.rs::tests::stale_generation_cannot_replace_the_tree` | Same reset + stale `apply_page` contract as `reset_discards_loaded_content_and_invalidates_inflight_pages`; its node-absent assertion folded there. |
| R | `files/model.rs::tests::removing_a_directory_removes_all_descendants` | FileTreeModel owner: Watch delete removes subtree. |
| R | `files/model.rs::tests::empty_error_and_load_more_rows_follow_load_state` | FileTreeModel owner: Loading/error/empty rows. |
| R | `files/model.rs::tests::keyboard_selection_follows_visible_rows` | FileTreeModel owner: Keyboard navigation order. |
| D | `files/model.rs::tests::an_inflight_directory_load_is_not_duplicated` | Duplicate: `changes_during_load_are_remembered_and_collapsed_caches_become_stale` asserts `!begin_load` for the in-flight root (generation and node valid, so only the Loading guard can refuse). |
| R | `files/model.rs::tests::a_large_monorepo_page_stays_flat_and_addressable` | FileTreeModel owner: Scale: 10k entries. |
| R | `files/model.rs::tests::reset_discards_loaded_content_and_invalidates_inflight_pages` | FileTreeModel owner: Privacy/generation keeper: reset drops content and rejects stale pages; absorbed the stale-page node assertion. |
| R | `files/preview.rs::tests::preview_rows_track_the_code_font_size_from_the_default_to_the_maximum` | Row height source for virtualised preview. |
| R | `files/preview.rs::tests::the_default_code_font_size_reproduces_the_historical_per_surface_sizes` | Regression: default sizes unchanged on fresh install. |
| R | `files/preview.rs::tests::scaled_preview_sizes_keep_their_proportions_and_stay_clamped` | Scaling/clamp of editor and preview sizes. |
| D | `files/preview.rs::tests::tree_split_uses_the_standard_resize_geometry_and_limits` | View-local replay of the shared `motion::resize_drag_sample` (owned by `shell::tests::right_pane_uses_the_shared_clamp_and_edge_latch`) plus a constant restatement (`HALF_WIDTH * 2 == 20`). |
| D | `files/preview.rs::tests::read_only_reasons_have_specific_messages` | Substring greps of a static `match` of copy; no behaviour. |
| R | `files/preview.rs::tests::reset_drops_documents_and_active_preview_from_the_previous_target` | Privacy: previous target's documents dropped on reset. Absorbed the preserved-preferences assertions. |
| R | `files/preview.rs::tests::document_cache_evicts_the_oldest_safe_entries` | Eviction never drops dirty/conflict/flush/loading/protected docs. |
| R | `files/preview.rs::tests::document_cache_uses_retained_bytes_not_only_entry_count` | Byte budget eviction. |
| R | `files/preview.rs::tests::document_eviction_cleans_path_scoped_companion_state` | Eviction cleans highlights/anchors/recency. |
| C | `files/preview.rs::tests::reset_preserves_global_editor_preferences` | Same `reset()` call; its assertions folded into `reset_drops_documents_and_active_preview_from_the_previous_target` (constructed with word wrap + 15px). |
| R | `files/preview.rs::tests::file_highlight_completion_rejects_a_result_after_a_user_edit` | Stale async highlight rejected after an edit (keeper for the stale-highlight gate). |
| R | `files/preview.rs::tests::autosave_is_opt_in_and_enabling_schedules_dirty_documents` | Autosave opt-in schedules dirty docs. |
| R | `files/preview.rs::tests::sidebar_layout_changes_are_immediate_without_a_user_toggle` | Responsive tree sidebar without animation. |
| R | `files/preview.rs::tests::sidebar_motion_reverses_from_its_current_width` | Motion reversal math. |
| R | `files/preview.rs::tests::sidebar_motion_snaps_when_reduced_motion_is_enabled` | Reduced-motion snap. |
| R | `files/preview.rs::tests::wide_layout_respects_an_explicitly_hidden_tree_sidebar` | Explicit hide survives breakpoint changes. |
| R | `files/preview.rs::tests::explicitly_showing_tree_sidebar_clears_responsive_dismissal` | Explicit show clears dismissal. |
| R | `files/preview.rs::tests::dirty_reload_waits_for_explicit_discard_confirmation` | Dirty reload needs confirmation; repeated clicks cannot bypass. |
| R | `files/preview.rs::tests::clean_reload_proceeds_without_confirmation` | Clean reload proceeds and clears stale confirmation. |
| R | `files/preview.rs::tests::failed_save_blocks_lifecycle_until_explicit_recovery` | Predicate negative (dirty Ready does not block) is not covered by shell.rs quit test. |
| R | `files/preview.rs::tests::terminal_save_outcomes_release_review_comment_flushes` | Comment flush released on terminal save outcomes. |
| R | `files/preview.rs::tests::directory_rename_updates_open_descendant_paths` | Rename path rewrite incl. prefix-collision negative. |
| R | `files/preview.rs::tests::document_paths_match_recreated_files_and_directory_descendants` | Descendant match incl. src-old negative. |
| R | `files/preview.rs::tests::comment_ranges_anchor_normal_and_trailing_empty_lines` | Comment anchor ranges incl. trailing empty line. |
| R | `files/preview.rs::tests::comment_overlay_stays_inside_the_editor_viewport` | Overlay placement bound. |
| R | `files/preview.rs::tests::comment_overlay_is_compact_when_space_allows_and_inset_when_narrow` | Overlay horizontal layout. |
| R | `files/preview.rs::markdown_buffer_tests::markdown_inline_comments_use_file_review_workflow` | Linux headless E2E: plus/draft/commit/edit/remove/escape on Markdown comments via real clicks. |
| R | `files/preview.rs::markdown_buffer_tests::editing_file_comments_keeps_the_live_anchor` | Editor comment edit keeps the live decoration anchor. |
| R | `files/preview.rs::markdown_buffer_tests::image_rename_preserves_unsaved_text_on_reopen_and_watcher` | Rename to image extension keeps unsaved text and pending save. |
| R | `files/preview.rs::markdown_buffer_tests::markdown_preview_web_links_keep_the_file_session_and_full_target` | Link routing E2E: owner session, full target, mailto via OS, unsafe URLs rejected (keeper for preview_link_outcome). |
| R | `files/preview.rs::markdown_buffer_tests::preview_reads_unsaved_buffer_without_starting_a_save` | Preview reads the buffer without saving; released on clear. |
| R | `files/search.rs::tests::search_state_rejects_stale_queries` | Stale generation/query results rejected. |
| R | `files/search.rs::tests::search_tree_groups_matches_under_real_ancestors` | Search tree grouping and depth. |
| R | `files/search.rs::tests::search_tree_uses_path_components_instead_of_name_prefixes` | Regression: name-prefix collision (THIRD_PARTY_NOTICES). |
| R | `files/search.rs::tests::search_tree_collapses_and_restores_matching_branches` | Collapse/restore of search branches. |
| R | `files/tree.rs::tests::directory_load_keeps_the_viewport_on_the_same_path` | Viewport anchoring across loads. |
| R | `files/tree.rs::tests::removed_anchor_falls_back_to_parent_then_neighbor` | Anchor fallback when rows disappear. |
| R | `files/watch.rs::tests::sequence_gaps_require_resync` | Watch stream sequencing contract (gaps/replays force resync). |
| C | `settings.rs::tests::composer_send_behavior_is_opt_in_for_old_and_partial_settings` | Same legacy file `{sidebarWidth, soundEnabled}` as the keeper; composer/follow-up/web-link/background/sound-gate defaults folded into `partial_legacy_settings_keep_their_values_and_default_new_keys`. |
| R | `settings.rs::tests::window_geometry_round_trips_and_legacy_settings_default` | Config/migration contract: Geometry persistence and legacy default. |
| R | `settings.rs::tests::window_geometry_restores_display_identity_with_overlapping_local_coordinates` | Config/migration contract: Display identity restore. |
| R | `settings.rs::tests::window_geometry_recenters_when_saved_display_is_disconnected` | Config/migration contract: Recenter on missing display; oversized fit. |
| R | `settings.rs::tests::window_geometry_without_display_identity_uses_primary` | Config/migration contract: Legacy geometry uses the primary display. |
| R | `settings.rs::tests::window_geometry_rejects_invalid_values_without_resetting_settings` | Config/migration contract: Invalid geometry dropped without resetting settings. |
| R | `settings.rs::tests::window_geometry_preserves_position_on_negative_coordinate_display` | Config/migration contract: Negative-origin displays. |
| R | `settings.rs::tests::window_geometry_fits_smaller_display_and_keeps_titlebar_visible` | Config/migration contract: Fit + minimum size. |
| R | `settings.rs::tests::unknown_keys_from_a_newer_build_are_ignored` | Config/migration contract: Forward compatibility with newer files. |
| R | `settings.rs::tests::appshot_shortcut_defaults_round_trip_and_reset` | Config/migration contract: Platform defaults and persistence of the Appshot chord. |
| R | `settings.rs::tests::appshot_settings_are_serialized_only_on_desktop` | Config/migration contract: Platform contract: Appshot keys only on desktop. |
| R | `settings.rs::tests::appshot_sound_migrates_mute_and_persists_independently` | Config/migration contract: Migration of the old mute. |
| R | `settings.rs::tests::background_cleanup_only_removes_files_owned_by_the_setting` | Config/migration contract: Storage safety: never deletes unrelated files. |
| R | `settings.rs::tests::invalid_background_replacement_preserves_previous_image_and_settings` | Config/migration contract: Atomic replacement on invalid input. |
| R | `settings.rs::tests::valid_background_replacement_persists_renderable_image_before_retiring_previous` | Config/migration contract: Replacement ordering. |
| R | `settings.rs::tests::invalid_initial_background_import_does_not_create_managed_files_or_settings` | Config/migration contract: No residue on failed import. |
| R | `settings.rs::tests::obsolete_steering_preference_does_not_reset_other_settings` | Config/migration contract: Obsolete key tolerated and dropped. |
| R | `settings.rs::tests::round_trip` | Config/migration contract: Full config round trip and key spellings. |
| R | `settings.rs::tests::invalid_saved_usage_rates_do_not_appear_as_active_overrides` | Config/migration contract: Invalid rate overrides dropped. |
| R | `settings.rs::tests::activity_shortcut_defaults_to_a_free_chord_and_fixed_chords_remain_reserved` | Config/migration contract: Activity default and reserved chords. |
| R | `settings.rs::tests::stale_revision_cannot_be_considered_the_latest_save` | Config/migration contract: Save revision ordering. |
| R | `settings.rs::tests::transcript_width_loads_legacy_defaults_and_normalizes_persisted_values` | Config/migration contract: Transcript width migration/normalisation. |
| R | `settings.rs::tests::code_fence_generation_tracks_every_mode_transition_only` | Config/migration contract: Generation bumps only on mode change. |
| R | `settings.rs::tests::legacy_project_organization_normalizes_to_one_list` | Config/migration contract: Migration byProject -> one list. |
| R | `settings.rs::tests::settings_without_appearance_default_to_system` | Keeper, renamed `partial_legacy_settings_keep_their_values_and_default_new_keys`: one real-file load of a partial legacy file; absorbs composer, UI-font and keymap legacy-default tests and owns the AppearanceMode default. |
| R | `settings.rs::tests::session_sound_preferences_round_trip_and_gate_each_event` | Per-event sound gating under the master switch. |
| R | `settings.rs::tests::legacy_accent_color_migrates_to_an_explicit_preset` | Migration accentColor -> accent preset; old key not re-saved. |
| C | `settings.rs::tests::settings_without_ui_font_default_to_geist` | Same legacy file as the keeper; UI font family/size and diff wrap defaults folded into `partial_legacy_settings_keep_their_values_and_default_new_keys`. |
| R | `settings.rs::tests::unsupported_ui_font_size_snaps_to_the_nearest_choice` | Config contract: Off-ladder UI size snaps. |
| R | `settings.rs::tests::unknown_ui_font_falls_back_without_resetting_settings` | Config contract: Unknown family falls back without reset. |
| R | `settings.rs::tests::missing_and_corrupt_files_yield_defaults` | Config contract: Missing/corrupt file -> defaults. |
| R | `settings.rs::tests::loaded_values_are_clamped` | Clamp-table keeper through `load` and `clamped()`; absorbed the terminal-height, right-pane-unbounded and sidebar-NaN rows. |
| R | `settings.rs::tests::legacy_files_editor_font_size_becomes_the_code_font_size` | Config contract: Migration filesEditorFontSize -> codeFontSize. |
| R | `settings.rs::tests::git_history_column_widths_are_clamped_and_nan_heals` | Config contract: History column clamp. |
| R | `settings.rs::tests::git_history_column_order_deduplicates_and_restores_missing_columns` | Config contract: History order normalisation. |
| C | `settings.rs::tests::large_right_pane_width_is_preserved` | One `clamped()` row (no persisted right-pane maximum); folded into `loaded_values_are_clamped`. |
| C | `settings.rs::tests::nan_heals_to_default` | One `clamped()` row (sidebar NaN -> default); folded into `loaded_values_are_clamped`. |
| R | `settings.rs::tests::defaults_match_luna` | Default panel sizes/visibility contract. |
| R | `settings.rs::tests::legacy_panel_defaults_migrate_and_custom_shortcuts_survive` | Keymap contract: Keymap migration keeps custom chords. |
| R | `settings.rs::tests::legacy_migration_reserves_custom_chords_and_survives_round_trip` | Keymap contract: Migration never steals a custom chord. |
| R | `settings.rs::tests::new_project_shortcut_migrates_and_persists` | Keymap contract: New Project shortcut migration. |
| R | `settings.rs::tests::keymap_defaults_and_reset` | Keymap contract: Default chords and reset. |
| R | `settings.rs::tests::every_shortcut_default_is_unique_and_bindable` | Keymap contract: No default conflicts (normalized chords) and every default parses; keeper for physical-key distinctness. |
| R | `settings.rs::tests::combo_recording` | Keymap contract: Recorder spelling per platform. |
| R | `settings.rs::tests::every_default_is_spelled_the_way_the_recorder_spells_it` | Keymap contract: Default/recorder round trip on both platforms. |
| D | `settings.rs::tests::defaults_are_distinct_physical_keys` | Duplicate: `every_shortcut_default_is_unique_and_bindable` asserts `conflicted_shortcuts(defaults)` is empty, which compares `normalized_combo` (modifier-order and synonym aware), a stricter equivalence than `platform_combo`; only unavailable-on-platform rows differed. |
| R | `settings.rs::tests::conflict_detection` | Keymap contract: conflicts reported for both sides and cleared on reset. |
| R | `settings.rs::tests::combo_translation` | Keymap contract: platform and display spelling. |
| C | `settings.rs::tests::keymap_survives_old_settings_files` | Pre-keymap file load; keymap default and sidebar_grouped assertions folded into the legacy-file keeper (same absent keys). |
| D | `settings.rs::tests::escape_stopping_is_opt_in_for_old_and_partial_settings` | Exact duplicate: identical input file and all three assertions (Escape off, width, sound) already in `partial_legacy_settings_keep_their_values_and_default_new_keys`. |
| R | `settings.rs::tests::jump_slots_get_set_and_reset` | Keymap contract: Jump slot accessors and bounds. |
| R | `settings.rs::tests::short_or_long_jump_lists_heal_to_the_slot_count` | Keymap contract: Jump list healing. |
| R | `settings.rs::tests::a_malformed_jump_list_heals_without_losing_other_settings` | Keymap contract: Healing on load keeps other settings. |
| R | `settings.rs::tests::legacy_modifier_enter_shortcut_heals_without_losing_other_customizations` | Keymap contract: mod-enter healed (reserved for send). |
| R | `settings.rs::tests::jump_hints_need_an_exact_modifier_match` | Keymap contract: Jump hint overlay modifiers. |
| R | `settings.rs::tests::modifier_send_hint_needs_only_the_primary_modifier` | Keymap contract: Send hint modifiers. |
| R | `settings.rs::tests::badge_combos_use_mac_glyphs_and_linux_text` | Keymap contract: Badge glyph ordering. |
| R | `settings.rs::tests::combo_modifiers_reads_the_stored_form` | Keymap contract: Stored-form modifier parse. |
| R | `settings.rs::tests::a_keymap_missing_newer_shortcuts_keeps_its_customizations` | Keymap contract: Upgrade keeps customizations. |
| C | `settings.rs::tests::terminal_height_clamps_on_load` | Terminal height min via load and max via `clamped()`; folded into `loaded_values_are_clamped` (load path ends in `migrated().clamped()`). |
| R | `settings/accounts.rs::tests::lifecycle_controls_match_provider_contracts` | Provider lifecycle actions per harness/state. |
| R | `settings/accounts.rs::tests::page_navigation_reads_the_shared_background_usage_cache` | Which loads force a usage probe (cache contract with the engine warm loop). |
| R | `settings/accounts.rs::tests::usage_thresholds_match_luna` | Warn/critical thresholds. |
| D | `settings/accounts.rs::tests::usage_colors_map_to_theme_accents` | Restates a 3-arm match onto theme fields (expected values are the implementation). |
| R | `settings/accounts.rs::tests::reset_formatting_is_absolute` | Reset label horizons (<22h clock, <week weekday, else date). |
| R | `settings/accounts.rs::tests::provider_grouping_keeps_engine_order_even_when_active_is_later` | Regression: switching never reorders cards. |
| D | `settings/appearance.rs::tests::every_mode_gets_a_card` | Copied inventory (`ALL.len() == 3`) plus non-empty labels from a static match. |
| D | `settings/appearance.rs::tests::registry_offers_both_appearances_and_keeps_single_dark_families_valid` | Copied inventory of builtin variant counts; luna-theme owns the registry (`builtins_have_complete_provenance_and_no_validation_errors` asserts luna-light/luna-dark, `visual_fixture_matrix_covers_every_variant_and_scene` the 30 variants). |
| R | `settings/appearance.rs::tests::accent_helper_explains_default_and_override_scope` | Retained (uncertain): copy selection with preset interpolation; cheap. |
| R | `settings/appearance.rs::tests::surface_helper_explains_theme_default_and_global_overrides` | Retained (uncertain): copy branch reflects the resolved surface treatment. |
| R | `settings/appearance.rs::tests::font_options_appear_once_in_stable_order` | Picker order and uniqueness of bundled + installed families. |
| R | `settings/appearance.rs::tests::only_the_terminal_catalog_is_narrowed_to_fixed_width` | Terminal picker limited to monospace. |
| R | `settings/appearance.rs::tests::font_keyboard_navigation_stops_at_edges_and_skips_unavailable` | Keyboard navigation bounds. |
| R | `settings/appearance.rs::tests::filtering_narrows_to_matches_and_navigation_stays_inside_them` | Filter ranking and navigation. |
| R | `settings/appearance.rs::tests::each_font_kind_gets_distinct_labels_and_element_ids` | Element id uniqueness (gpui id collisions). |
| D | `settings/appearance.rs::tests::pixel_sizes_render_whole_and_fractional_values` | Duplicate: `every_size_dropdown_labels_its_whole_ladder_in_pixels` covers "13 px"/"12.5 px" through `size_labels`; the other rows restate `f32::clamp` in `typography::clamp_font_size`. |
| R | `settings/appearance.rs::tests::font_size_options_are_ordered_and_include_the_default` | Ladder ordering includes default. |
| R | `settings/appearance.rs::tests::mono_size_ladder_keeps_both_defaults_exactly_reachable` | Both mono defaults reachable; off-ladder snaps. |
| R | `settings/appearance.rs::tests::every_size_dropdown_labels_its_whole_ladder_in_pixels` | Dropdown labels for every kind; keeper for format_px. |
| R | `settings/appearance.rs::tests::conversation_width_drag_coalesces_and_persists_the_last_value` | Drag coalescing and persistence through settings flush. |
| R | `settings/appearance.rs::tests::size_dropdowns_commit_from_the_ladder_and_enter_leaves_menus_closed` | Menu commit and Enter-reopen regression. |
| R | `settings/appearance.rs::tests::persisted_proportional_terminal_family_resolves_to_geist_mono` | Migration: proportional terminal family falls back. |
| R | `settings/archived.rs::tests::only_archived_rows_show` | Retained (uncertain): cheap filter guard for the Archived page; no fixture asserts it. |
| R | `settings/composer.rs::tests::round_trip` | Storage contract: Composer defaults persistence incl. legacy Ask. |
| R | `settings/composer.rs::tests::missing_and_corrupt_files_yield_defaults` | Storage contract: Missing/corrupt file -> defaults. |
| R | `settings/composer.rs::tests::concurrent_target_saves_leave_a_complete_preference` | Storage contract: Atomic concurrent saves. |
| R | `settings/composer.rs::tests::favorites_toggle_and_persist` | Storage contract: Favorites per harness persist. |
| R | `settings/composer.rs::tests::remember_model_updates_harness_and_row` | Storage contract: Remember model updates harness. |
| R | `settings/devices.rs::tests::presence_window` | 70s presence window incl. clock skew. |
| R | `settings/devices.rs::tests::last_seen_formatting` | Last-seen formatting table. |
| R | `settings/devices.rs::tests::local_subtitle_does_not_claim_synced_metadata` | Truthfulness regression: local workspace never claims sync. |
| F | `settings/general.rs::tests::general_page_renders_existing_preferences_and_changes_them` | Asserted the fields its own setters assigned (identity copier) around a render smoke. Repaired as `general_page_renders_and_emits_one_event_per_change`: subscribes to `GeneralEvent` (what shell.rs persists) and asserts one event per real change, none for repeats. |
| R | `settings/project_defaults_button_admission_tests.rs::defaults_button_admission_project_replacement` | Real mouse/keyboard delivery over HeldRpc: stale project/device input cannot save; a fresh choice saves once. |
| R | `settings/project_defaults_button_admission_tests.rs::defaults_button_admission_same_device_generation` | Real mouse/keyboard delivery over HeldRpc: stale project/device input cannot save; a fresh choice saves once. |
| R | `settings/project_defaults_button_admission_tests.rs::defaults_button_admission_different_device` | Real mouse/keyboard delivery over HeldRpc: stale project/device input cannot save; a fresh choice saves once. |
| R | `settings/project_defaults_button_admission_tests.rs::defaults_button_admission_fresh_choice_once` | Real mouse/keyboard delivery over HeldRpc: stale project/device input cannot save; a fresh choice saves once. |
| R | `settings/project_defaults_button_admission_tests.rs::defaults_button_admission_live_busy_preserves_first_save` | Real mouse/keyboard delivery over HeldRpc: stale project/device input cannot save; a fresh choice saves once. |
| R | `settings/project_defaults_button_admission_tests.rs::defaults_button_admission_same_project_watch_keyboard` | Real mouse/keyboard delivery over HeldRpc: stale project/device input cannot save; a fresh choice saves once. |
| R | `settings/project_defaults_engine_epoch_tests.rs::engine_epoch_defaults_save_success_same_device` | Row of the `retained_defaults_case` table: late engine-A completions never mutate the replacement engine's Defaults page. |
| R | `settings/project_defaults_engine_epoch_tests.rs::engine_epoch_defaults_save_error_same_device` | Row of the `retained_defaults_case` table: late engine-A completions never mutate the replacement engine's Defaults page. |
| R | `settings/project_defaults_engine_epoch_tests.rs::engine_epoch_defaults_models_success_same_device` | Row of the `retained_defaults_case` table: late engine-A completions never mutate the replacement engine's Defaults page. |
| R | `settings/project_defaults_engine_epoch_tests.rs::engine_epoch_defaults_models_error_same_device` | Row of the `retained_defaults_case` table: late engine-A completions never mutate the replacement engine's Defaults page. |
| R | `settings/project_defaults_engine_epoch_tests.rs::engine_epoch_defaults_save_success_different_device` | Row of the `retained_defaults_case` table: late engine-A completions never mutate the replacement engine's Defaults page. |
| R | `settings/project_defaults_engine_epoch_tests.rs::engine_epoch_defaults_save_error_different_device` | Row of the `retained_defaults_case` table: late engine-A completions never mutate the replacement engine's Defaults page. |
| R | `settings/project_defaults_engine_epoch_tests.rs::engine_epoch_defaults_models_success_different_device` | Row of the `retained_defaults_case` table: late engine-A completions never mutate the replacement engine's Defaults page. |
| R | `settings/project_defaults_engine_epoch_tests.rs::engine_epoch_defaults_models_error_different_device` | Row of the `retained_defaults_case` table: late engine-A completions never mutate the replacement engine's Defaults page. |
| R | `settings/shortcuts.rs::tests::appshots_setup_can_be_enabled_and_configured_by_keyboard` | Keyboard accessibility of the Appshots settings. |
| R | `settings/shortcuts.rs::tests::recorder_refuses_bound_actions_before_they_can_run` | Recorder intercepts bound chords and restores actions after. |
| R | `settings/shortcuts.rs::tests::recording_outcomes` | Recorder outcome table. |
| R | `settings/shortcuts.rs::tests::every_shortcut_lands_in_a_rendered_group` | Every shortcut renders in a non-empty group. |
| R | `settings/shortcuts.rs::tests::conflicting_records_are_refused` | Conflict owner at record time. |
| R | `settings/shortcuts.rs::tests::appshot_binding_participates_in_existing_conflict_checks` | Appshot chord in conflict checks both directions. |
| D | `settings/shortcuts.rs::tests::modifier_send_labels_are_platform_specific` | Restates a two-literal if/else of label copy. |
| R | `settings/shortcuts.rs::tests::modifier_send_is_always_reserved_for_the_composer` | mod-enter reserved for send. |
| R | `terminal/emulator.rs::tests::plain_text_lands_on_row_zero` | Emulator wrapper over pinned alacritty_terminal 0.26 / vte 0.15 fed real bytes (strongest boundary for the VT surface): text + cursor mapping. |
| R | `terminal/emulator.rs::tests::crlf_moves_lines_and_cr_returns_to_column_zero` | Emulator wrapper over pinned alacritty_terminal 0.26 / vte 0.15 fed real bytes (strongest boundary for the VT surface): CR/LF. |
| R | `terminal/emulator.rs::tests::long_line_wraps_at_the_grid_width` | Emulator wrapper over pinned alacritty_terminal 0.26 / vte 0.15 fed real bytes (strongest boundary for the VT surface): wrap. |
| R | `terminal/emulator.rs::tests::sgr_colors_and_attributes` | Emulator wrapper over pinned alacritty_terminal 0.26 / vte 0.15 fed real bytes (strongest boundary for the VT surface): SGR to CellColor mapping. |
| R | `terminal/emulator.rs::tests::bright_256_and_truecolor_sgr` | Emulator wrapper over pinned alacritty_terminal 0.26 / vte 0.15 fed real bytes (strongest boundary for the VT surface): bright/256/RGB mapping. |
| R | `terminal/emulator.rs::tests::inverse_and_hidden_resolve_in_display_colors` | Emulator wrapper over pinned alacritty_terminal 0.26 / vte 0.15 fed real bytes (strongest boundary for the VT surface): display_colors. |
| R | `terminal/emulator.rs::tests::cursor_addressing_and_relative_moves` | Emulator wrapper over pinned alacritty_terminal 0.26 / vte 0.15 fed real bytes (strongest boundary for the VT surface): 1-based CSI to 0-based cursor. |
| R | `terminal/emulator.rs::tests::clear_screen_and_home` | Emulator wrapper over pinned alacritty_terminal 0.26 / vte 0.15 fed real bytes (strongest boundary for the VT surface): ED/CUP. |
| R | `terminal/emulator.rs::tests::erase_line_variants` | Emulator wrapper over pinned alacritty_terminal 0.26 / vte 0.15 fed real bytes (strongest boundary for the VT surface): EL. |
| R | `terminal/emulator.rs::tests::scrollback_history_and_scrolling` | Emulator wrapper over pinned alacritty_terminal 0.26 / vte 0.15 fed real bytes (strongest boundary for the VT surface): scrollback API and clamps. |
| R | `terminal/emulator.rs::tests::alt_screen_restores_primary_content` | Emulator wrapper over pinned alacritty_terminal 0.26 / vte 0.15 fed real bytes (strongest boundary for the VT surface): alt screen. |
| R | `terminal/emulator.rs::tests::dsr_cursor_report_produces_pty_response` | Emulator wrapper over pinned alacritty_terminal 0.26 / vte 0.15 fed real bytes (strongest boundary for the VT surface): PtyWrite plumbing returned by feed. |
| R | `terminal/emulator.rs::tests::osc_title_and_bell` | Emulator wrapper over pinned alacritty_terminal 0.26 / vte 0.15 fed real bytes (strongest boundary for the VT surface): listener title/bell plumbing. |
| R | `terminal/emulator.rs::tests::app_cursor_and_bracketed_paste_modes_toggle` | Emulator wrapper over pinned alacritty_terminal 0.26 / vte 0.15 fed real bytes (strongest boundary for the VT surface): mode flags read by the view. |
| R | `terminal/emulator.rs::tests::hidden_cursor_mode` | Emulator wrapper over pinned alacritty_terminal 0.26 / vte 0.15 fed real bytes (strongest boundary for the VT surface): DECTCEM. |
| R | `terminal/emulator.rs::tests::resize_preserves_content_and_reflows_cursor` | Emulator wrapper over pinned alacritty_terminal 0.26 / vte 0.15 fed real bytes (strongest boundary for the VT surface): resize. |
| R | `terminal/emulator.rs::tests::wide_chars_occupy_two_cells_with_spacer` | Emulator wrapper over pinned alacritty_terminal 0.26 / vte 0.15 fed real bytes (strongest boundary for the VT surface): wide-char spacer cells. |
| R | `terminal/emulator.rs::tests::grid_point_offsets_by_the_scrollback_position` | Emulator wrapper over pinned alacritty_terminal 0.26 / vte 0.15 fed real bytes (strongest boundary for the VT surface): viewport-to-grid translation. |
| R | `terminal/emulator.rs::tests::simple_selection_yields_its_text_and_marks_its_cells` | Emulator wrapper over pinned alacritty_terminal 0.26 / vte 0.15 fed real bytes (strongest boundary for the VT surface): selection text + cell marks. |
| R | `terminal/emulator.rs::tests::semantic_selection_expands_to_the_word` | Emulator wrapper over pinned alacritty_terminal 0.26 / vte 0.15 fed real bytes (strongest boundary for the VT surface): double-click selection. |
| R | `terminal/emulator.rs::tests::line_selection_takes_the_whole_row` | Emulator wrapper over pinned alacritty_terminal 0.26 / vte 0.15 fed real bytes (strongest boundary for the VT surface): triple-click selection. |
| R | `terminal/emulator.rs::tests::selection_spans_rows_with_a_newline` | Emulator wrapper over pinned alacritty_terminal 0.26 / vte 0.15 fed real bytes (strongest boundary for the VT surface): multi-row copy. |
| R | `terminal/emulator.rs::tests::selection_follows_its_text_when_output_scrolls` | Emulator wrapper over pinned alacritty_terminal 0.26 / vte 0.15 fed real bytes (strongest boundary for the VT surface): grid-anchored selection. |
| R | `terminal/emulator.rs::tests::a_click_without_a_drag_selects_nothing` | Emulator wrapper over pinned alacritty_terminal 0.26 / vte 0.15 fed real bytes (strongest boundary for the VT surface): bare click is not a selection. |
| R | `terminal/emulator.rs::tests::utf8_split_across_feeds_reassembles` | Emulator wrapper over pinned alacritty_terminal 0.26 / vte 0.15 fed real bytes (strongest boundary for the VT surface): UTF-8 split across frames. |
| R | `terminal/panel.rs::tests::height_clamps_between_160_and_55vh` | Runtime height clamp incl. NaN and tiny windows. |
| R | `terminal/panel.rs::tests::backoff_doubles_and_caps` | Reconnect backoff table. |
| R | `terminal/panel.rs::tests::selection_edge_scroll_uses_terminal_direction` | Edge auto-scroll direction. |
| R | `terminal/panel.rs::tests::rail_parts_map_history_top_and_bottom` | Scroll rail math incl. no false activity flash. |
| R | `terminal/panel.rs::tests::reorder_moves_forward_and_backward` | Tab reorder. |
| R | `terminal/panel.rs::tests::drop_index_quantizes_and_clamps` | Drop index. |
| R | `terminal/panel.rs::tests::slide_offsets_shift_toward_the_gap` | Slide animation offsets. |
| R | `terminal/panel.rs::tests::active_index_tracks_reorders` | Active tab after reorder. |
| R | `terminal/panel.rs::tests::active_index_tracks_closes` | Active tab after close. |
| C | `terminal/panel.rs::tests::exit_message_format` | Substring/edge checks of the same bytes; the emulator keeper now asserts the visible row and the trailing CRLF via the cursor position. |
| R | `terminal/panel.rs::tests::shell_titles` | Tab titles from shell paths incl. Windows. |
| R | `terminal/panel.rs::tests::stream_events_deserialize_per_contract` | Terminal stream JSON wire contract (`type`/`exitCode`); engine tests use typed events only. Follow-up: move to luna-proto. |
| R | `terminal/panel.rs::tests::base64_round_trip_and_tolerance` | Base64 frame tolerance. |
| R | `terminal/panel.rs::tests::exit_message_feeds_cleanly_through_the_emulator` | Exit notice through the real emulator; absorbed the format test (own row, trailing CRLF parks the cursor). |
| R | `terminal/view.rs::tests::printables_prefer_key_char` | PTY input encoding table. |
| R | `terminal/view.rs::tests::control_keys_and_sequences` | xterm control sequences. |
| R | `terminal/view.rs::tests::arrows_respect_app_cursor_mode` | DECCKM arrows. |
| R | `terminal/view.rs::tests::ctrl_combos_map_to_control_bytes` | Ctrl caret encoding. |
| R | `terminal/view.rs::tests::alt_prefixes_escape` | Alt ESC prefix. |
| R | `terminal/view.rs::tests::platform_primary_combos_fall_through` | Cmd chords left to the app. |
| R | `terminal/view.rs::tests::paste_wraps_when_bracketed` | Security: bracketed paste strips close-bracket injection. |
| R | `terminal/view.rs::tests::coalescer_schedules_once_per_burst` | Input coalescer scheduling. |
| R | `terminal/view.rs::tests::cube_is_appearance_independent` | 256-colour cube math. |
| R | `terminal/view.rs::tests::grayscale_ramp_mirrors_in_light` | Grayscale ramp per appearance. |
| R | `terminal/view.rs::tests::registered_theme_owns_terminal_background_foreground_selection_and_ansi` | Theme-owned terminal colours and selection alpha range. |
| R | `terminal/view.rs::tests::every_registered_variant_resolves_all_ansi_slots_from_its_theme` | All builtin variants map ANSI 0-15. |
| D | `terminal/view.rs::tests::timing_constants_match_spec` | Copies two constants back as literals; no behaviour (timing behaviour is not asserted anywhere). |
| R | `terminal/view.rs::tests::pointer_maps_to_the_cell_it_is_over` | Pointer-to-cell math. |
| R | `terminal/view.rs::tests::side_splits_the_cell_at_its_midpoint` | Cell side split. |
| R | `terminal/view.rs::tests::overshoot_clamps_into_the_grid` | Drag overshoot clamps. |
| R | `terminal/view.rs::tests::overshoot_forces_the_side_to_the_edge` | Overshoot side forcing. |
| R | `terminal/view.rs::tests::degenerate_metrics_do_not_panic` | NaN/zero metrics safe. |
| D | `terminal/view.rs::tests::drag_threshold_matches_the_gpui_default` | Name promises a cross-check with gpui, but gpui's `DRAG_THRESHOLD` (vendor/zui div.rs) is private; it only restates the literal 2.0. |
| C | `theme.rs::tests::neutral_950_is_0a0a0a` | One more `oklch_to_srgb` reference row; folded into `oklch_matches_reference_colors`. |
| R | `theme.rs::tests::scrollbar_thumbs_follow_the_active_appearance` | Design-system contract: Scrollbar tone/alpha per appearance. |
| R | `theme.rs::tests::oklch_accents_match_reference` | Design-system contract: Keeper, renamed `oklch_matches_reference_colors`: independent CSS Color 4 reference values incl. neutral-950. |
| R | `theme.rs::tests::hsl_roundtrips_through_rgb` | Design-system contract: Colour space round trip. |
| R | `theme.rs::tests::contrast_ratio_hits_known_anchors` | Design-system contract: Contrast formula anchors. |
| R | `theme.rs::tests::luna_accent_is_the_exact_upstream_default` | Design-system contract: Default accent exact values. |
| R | `theme.rs::tests::previous_preview_accent_names_migrate_without_resetting_settings` | Design-system contract: Migration of old accent names. |
| R | `theme.rs::tests::theme_recommendation_and_surface_override_resolve_independently` | Design-system contract: Theme vs surface override resolution. |
| R | `theme.rs::tests::forced_frost_preserves_shell_text_contrast_for_every_builtin` | Design-system contract: Contrast over adverse backdrops for every builtin. |
| R | `theme.rs::tests::runtime_hardening_protects_native_custom_theme_edits` | Design-system contract: Hardening of unreadable custom themes. |
| R | `theme.rs::tests::runtime_hardening_leaves_curated_builtin_text_unchanged` | Design-system contract: Hardening leaves curated text. |
| R | `theme.rs::tests::every_accent_is_one_coherent_color_identity` | Design-system contract: Accent identity keeper (caret, code_text, busy, selection, semantic colours). |
| R | `theme.rs::tests::every_settings_swatch_has_a_distinct_primary` | Design-system contract: Distinct swatches. |
| R | `theme.rs::tests::text_contrast_is_paired_across_appearances` | Design-system contract: Paired text contrast. |
| R | `theme.rs::tests::text_tones_clear_wcag_aa` | Design-system contract: WCAG floors on bg and surface. |
| R | `theme.rs::tests::accents_clear_contrast_on_their_background` | Design-system contract: Light danger/warning/success floors. |
| C | `theme.rs::tests::code_and_syntax_tones_are_readable` | Subsumed for code_text (every_accent) and keyword/string/number (syntax_palette, every kind, every accent); its diff_add/diff_del 3:1 floor moved into `syntax_palette_resolves_every_kind_on_code_and_diff_backgrounds` for every accent. |
| R | `theme.rs::tests::syntax_palette_resolves_every_kind_on_code_and_diff_backgrounds` | Design-system contract: Syntax contrast keeper; absorbed the diff tint floor. |
| D | `theme.rs::tests::caret_is_findable_on_its_background` | Subsumed: `every_accent_is_one_coherent_color_identity` asserts caret == accent and accent >= 4.5:1 on bg for every accent in both appearances (3:1 floor implied). |
| R | `theme.rs::tests::solid_button_is_legible_in_both_appearances` | Design-system contract: Solid button 7:1 floor. |
| R | `theme.rs::tests::surfaces_are_separated_in_both_appearances` | Design-system contract: Surface separation. |
| R | `theme.rs::tests::dark_elevation_ladder_is_strictly_ordered` | Design-system contract: Elevation ladder. |
| R | `theme.rs::tests::light_elevation_is_flat_white_and_leans_on_borders` | Design-system contract: Light elevation flat white. |
| R | `theme.rs::tests::bare_plates_are_visible_against_their_panel` | Design-system contract: Regression: bare plates visible. |
| R | `theme.rs::tests::neutrals_are_achromatic` | Design-system contract: Neutral chroma. |
| R | `theme.rs::tests::hairlines_and_washes_flip_tone_with_appearance` | Design-system contract: Appearance-dependent helpers; keeper for the appearance mirror. |
| R | `theme.rs::tests::hover_wash_is_visible_on_its_surface` | Design-system contract: Hover visibility. |
| R | `theme.rs::tests::faintest_fills_survive_in_both_appearances` | Design-system contract: Regression: faint fills in light mode. |
| R | `theme.rs::tests::both_appearances_stay_frosted_and_light_runs_heavier` | Design-system contract: Frost relationships. |
| R | `theme.rs::tests::input_plate_never_reads_as_recessed` | Design-system contract: Input plate lift. |
| R | `theme.rs::tests::card_selection_paints_nothing_behind_its_row` | Design-system contract: Regression: inset-only card selection. |
| R | `theme.rs::tests::glass_selection_is_edge_only_and_subtle` | Design-system contract: Glass selection ring. |
| D | `theme.rs::tests::appearance_mirror_tracks_installed_theme` | Setter/getter identity; the behaviour (helpers follow the installed appearance) is owned by `hairlines_and_washes_flip_tone_with_appearance`. |
| D | `theme.rs::tests::appearance_rebuild_preserves_selected_ui_family` | Identity copier of the `with_font_sans` builder; name promises an appearance rebuild it never runs (`Theme::install_preferences`/`install_selection_inner` untested; follow-up). Unlocked dead `Theme::for_appearance`. |
| R | `theme.rs::tests::window_appearance_maps_onto_ours` | Design-system contract: gpui WindowAppearance mapping incl. vibrant. |
| R | `theme.rs::tests::scrim_is_black_but_lighter_in_light_mode` | Design-system contract: Scrim tone. |
| R | `theme.rs::tests::mix_endpoints_and_midpoint` | Design-system contract: mix() endpoints/clamp. |
| R | `theme.rs::tests::popup_foregrounds_keep_glass_and_solid_theme_surfaces_unchanged` | Design-system contract: Popup text hierarchy. |
| R | `theme.rs::tests::composer_tint_moves_toward_sidebar_without_hiding_backdrop` | Design-system contract: Composer tint bounds. |
| D | `theme.rs::tests::layout_numbers_match_luna` | Copied constants ("match luna" without reading the web source). Unlocked dead `Theme::HEADER_HEIGHT`, whose only reader was this test. |
| R | `theme_library.rs::tests::failed_persistence_leaves_library_state_unchanged` | Storage atomicity: a failed persist leaves library and warning unchanged. |

## Layer plan

Keeper per contract after cutover (strongest real boundary first):

| Contract | Keeper | Retired layer / assertions carried in |
| --- | --- | --- |
| Appshot prompt codec (escape, strip, queue restore) | `appshots.rs` codec tests | Fake capture backend that implemented its own permission states (D); empty-prompt row carried into `appshot_context_is_escaped_and_strip_safe`. Replays in `queue.rs`/`composer.rs`/`attachments.rs` belong to L05/L06. |
| macOS capture sizing | `appshots/macos.rs::capture_dimensions_use_retina_scale_and_cap_large_windows` | Two one-row tests merged into one table. |
| Global shortcut / portal trigger | `appshots/shortcut.rs` tests | Portal test now Linux-only; production seam removed. |
| Browser native visibility | `browser-fixture` (macOS job + `scripts/test-linux-browser.sh`, `ui-tests.yml`) | `presentation()` truth table (D). |
| Browser address safety | `browser/model.rs` parser tables | — |
| Diff parse/layout/heights | `changes.rs` inline tables (pure functions with real input space) | `body_height_is_analytic` (D, covered by truncate + split height); scroll slot-id row carried into `horizontal_scroll_and_width_are_independent_per_file`; singular label row carried into `scope_labels_and_clean_messages`; scope-menu test reduced to the exclusion contract (F). |
| Git / PR UI lifetimes | HeldRpc suites `git_actions/{lifetime,ownership}_tests.rs`, `pull_requests/{lifetime,cold_git_preview}_tests.rs` | — |
| Workspace files client | `files/client.rs::deterministic_transport_covers_list_search_read_and_watch_routing` + `write_decodes_conflicts_without_changing_request_shape` (shape/decode), pagination trio, image-chunk trio | `request_params` and bare serde checks (D); malformed decode test repaired through the client (F). |
| File tree model | `files/model.rs`; generation keeper `reset_discards_loaded_content_and_invalidates_inflight_pages`; in-flight dedupe owned by `changes_during_load_are_remembered_and_collapsed_caches_become_stale` | Stale-generation node assertion carried into the reset keeper. |
| Files surface | `files/preview.rs`; link routing E2E `markdown_preview_web_links_keep_the_file_session_and_full_target`; stale highlight gate `file_highlight_completion_rejects_a_result_after_a_user_edit` | `preview_link_outcome` unit (D); editor-adapter hazard test (D); tree-split replay of `motion::resize_drag_sample` (D); reset-preferences assertions carried into the reset keeper. |
| Settings persistence | `settings.rs`; legacy-file keeper `partial_legacy_settings_keep_their_values_and_default_new_keys`; clamp keeper `loaded_values_are_clamped`; keymap uniqueness `every_shortcut_default_is_unique_and_bindable` | Composer/UI-font/keymap legacy defaults, sidebar NaN, right-pane, terminal-height rows carried in; Escape and physical-key duplicates (D); `AppearanceMode` default owned by the legacy-file keeper. |
| General page events | `settings/general.rs::general_page_renders_and_emits_one_event_per_change` | Identity-setter assertions replaced by `GeneralEvent` delivery (F). |
| Theme design system | `every_accent_is_one_coherent_color_identity` (caret/code/busy), `syntax_palette_resolves_every_kind_on_code_and_diff_backgrounds` (syntax + diff tints, every accent), `hairlines_and_washes_flip_tone_with_appearance` (appearance mirror), `oklch_matches_reference_colors` | Caret, code/syntax, mirror, builder and constant tests (D/C). |
| Terminal | `terminal/emulator.rs` byte-level suite; `terminal/view.rs` input encoding; `terminal/panel.rs::exit_message_feeds_cleanly_through_the_emulator` | Exit-message format test carried into the emulator keeper (row + cursor); constant restatements (D). |

Seams:

- **Removed:** `Shortcut::portal_trigger` `#[cfg(any(target_os = "linux", test))]` → `#[cfg(target_os = "linux")]` (test gated the same way). Dead `Theme::HEADER_HEIGHT` and `Theme::for_appearance` (only readers were deleted tests; `rg` across `crates/` and `apps/` finds no other caller).
- **Retained (other callers):** `appshots::tests::shot` (`attachments.rs`, `composer.rs`, `queue.rs`, `shell.rs` tests); `FilesSurface::test_images_visible` and `seed_pending_exit_test_document` (`shell.rs` tests); `MarkdownPreview::test_tree` / `test_block_bounds` (`files/preview.rs` tests); `WorkspaceFilesClient::with_transport` (`files/client.rs` and `files/image_preview.rs` tests); `TerminalPanel::command_panel_for_epoch_fixture` (`project_commands_engine_epoch_tests.rs`); `theme::lock_appearance` (theme and `motion` tests).

CI routing: unchanged. `changes.rs::native_diff_font_geometry` keeps its name (routed by `ui-tests.yml` and `windows.yml`). The Linux-only portal test still runs in the `ui-tests.yml` Linux `cargo test -p luna-ui --lib` job. No deleted or renamed test is named in workflows, scripts or docs.

## Cross-lane requests

1. **L05/L06 (`shell.rs`):** keep `shell::tests::right_pane_uses_the_shared_clamp_and_edge_latch` as the single owner of `motion::resize_drag_sample`; L07 deleted the files tree-split replay that relied on it.
2. **L05/L06 (`shell.rs`):** if the tests at `shell.rs` ~20055 (`test_images_visible`) and ~20970 (`seed_pending_exit_test_document`) are retired, delete the two `#[cfg(test)]` accessors in `files/preview.rs` during the merge; they have no other caller.
3. **L05/L06 (`queue.rs`, `composer.rs`, `attachments.rs`):** appshot prompt escaping/strip/restore is owned by `appshots.rs`; replays of the codec there are consolidation candidates. `appshots::tests::shot` must stay `pub(crate)` while they use it.
4. **Owner of `engine_epoch_fixture.rs` (shared test support):** add per-method scripted reply queues and `subscribe` to `HeldRpc` so `files/client.rs` and `files/image_preview.rs` tests can use the real `EngineHandle` path; that unlocks deleting the `#[cfg(test)] WorkspaceFilesClient::with_transport` seam.
5. **Sync/proto lane:** `terminal/panel.rs::stream_events_deserialize_per_contract` pins the `TerminalEvent`/`TerminalSession` JSON wire shape (`type`, `exitCode`); it belongs in `crates/proto` beside the type. Kept here until a proto keeper exists.
6. **CI workflows:** no routing change needed.

## Possible product defects

None found. No retained assertion looks like it encodes a bug. The four F repairs are expected to pass on the baseline; if `settings/general.rs::general_page_renders_and_emits_one_event_per_change` fails, a General setter emits on a no-op change or event delivery differs and the shell would persist spurious changes.

## Follow-ups

- Changes pane: an entity-level test that switching diff mode or scope resets horizontal scroll (the deleted wrapper test never covered the callers).
- Theme: a test at `Theme::install_preferences` / `install_selection_inner` that the typography family and sizes survive an appearance or theme switch (the deleted builder test only named this contract).
- `changes.rs::sticky_header_uses_the_content_theme_in_dark_and_light` mirrors `sticky_file_header_paint`; trim it to the overlay-plane guard from c4a14a8b.
- `theme.rs`: `luna_accent_is_the_exact_upstream_default`, `solid_button_is_legible_in_both_appearances`, `accents_clear_contrast_on_their_background` and `light_elevation_is_flat_white_and_leans_on_borders` repeat rows already in `every_accent_is_one_coherent_color_identity` / `surfaces_are_separated_in_both_appearances`; trim the duplicate rows.
- `terminal/emulator.rs`: most of the 25 tests pin pinned-alacritty VT behaviour through a thin wrapper; consider one table-driven byte-stream suite on the next alacritty upgrade.
- `git_actions/lifetime_tests.rs` and `ownership_tests.rs` duplicate fixture setup (previews, routes, state seeding); share it when either suite is next touched.
- Retained with low confidence: `settings/appearance.rs` accent/surface helper copy tests and `settings/archived.rs::only_archived_rows_show` (cheap; no stronger owner). `archived.rs::group_by_device` is untested.
