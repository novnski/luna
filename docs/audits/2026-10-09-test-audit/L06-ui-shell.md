# L06 ui-shell test audit ledger

- **Lane:** L06-ui-shell (campaign 2026-10-09, CAMPAIGN.md steps 3–5).
- **Scope:** the 30 `crates/ui/src` files in the lane manifest (shell view family: `shell.rs` and `shell/{activity_center,command_palette,spaces,tabs,usage}.rs`, `state.rs`, `pickers.rs` + `pickers_project_defaults_tests.rs`, `queue.rs`, `popover.rs`, `rail.rs`, the three `project_commands_*_tests.rs`, `motion.rs` + `motion/windows_pulse.rs`, `sound.rs`, `notify.rs`, `power.rs`, `app_menus.rs`, `badges.rs`, `change_requests.rs`, `file_icons.rs`, `icons.rs`, `lib.rs`, `new_thread_background_{effects,mask}.rs`, `task_commands.rs`, `workspace_links.rs`) plus `crates/ui/tests/**` (only `fixtures/generated-images.json`, read by `transcript.rs` tests in L05; retained untouched).
- **Baseline (e4a0f534):** 372 test declarations; 11,637 lines inside test fns; 13,830 non-blank lines of inline `#[cfg(test)]` modules plus the four test-only files.
- **After this lane:** 341 declarations (−31); 11,300 test-fn lines; 13,496 test-module lines. Production −34 lines (two test-only seams, two dead items).
- **Marks:** R 332 · F 8 · C 18 · D 14.
- **CI routing:** every lane test runs in `ui-tests.yml` (`cargo test -p luna-ui --lib`, Linux and macOS filters `native_diff_font_geometry`, `notify::tests::`) and `windows.yml` (`--lib --skip native_diff_font_geometry`). No target, module path or filter that a workflow names was moved or deleted.
- **Method:** each declaration read in full with its production owner and callers; overlapping suites checked in `crates/proto` (`view.rs`, `motion.rs`), `crates/engine` (`permissions.rs`), `comments.rs` and `files/preview.rs`; history via `git log -S/-G` where a reason was unclear (877a43cc, bccd12d2, 3900ad4c, 55c5cda4). Nothing was compiled or run in this lane; the coordinator owns validation.

## Ledger

Paths are relative to `crates/ui/src`. A renamed keeper keeps its baseline name here; the evidence gives the new name.

| Mark | Test | Evidence |
|---|---|---|
| R | `app_menus.rs::tests::app_menu_ends_with_quit` | Native menu-bar contract: `Quit LUNA` is the last app-menu item and dispatches the app-owned `Quit` (buffer-flushing quit). Only owner; no fixture or QA script inspects `app_menus()`. |
| R | `app_menus.rs::tests::app_menu_offers_settings` | Settings item dispatches `shell::OpenSettings`; only owner of the menu wiring. |
| R | `app_menus.rs::tests::about_enabled_only_on_macos` | Platform gating: About (AppKit panel) is enabled only on macOS. |
| R | `app_menus.rs::tests::edit_menu_uses_composer_clipboard_os_actions` | Edit items carry `OsAction` cut/copy/paste/selectAll so AppKit routes them through the responder chain; losing `os_action` silently breaks menu clipboard. |
| R | `app_menus.rs::tests::view_menu_offers_all_three_appearance_modes` | View menu is the only menu-bar appearance switch; an inventory, but a user-facing platform contract with no other owner (retained when uncertain). |
| R | `app_menus.rs::tests::app_bindings_use_platform_settings_convention` | Key-equivalent contract on real `KeyBinding`s (cmd-, vs ctrl-, ; cmd-q/w/m macOS-only); construction itself parses each combo. |
| R | `badges.rs::tests::a_plain_message_carries_no_badges` | `badges::split` leaves a prompt without a trailing comment block untouched (the transcript must never eat user text). |
| C | `badges.rs::tests::a_sent_comment_block_becomes_one_pill` | Absorbed into `a_sent_comment_block_becomes_one_pill_with_one_row_per_comment` (same two diff-comment input shape): stripped body, one badge and the `2 comments` label are asserted there. |
| R | `badges.rs::tests::the_card_carries_one_row_per_comment` | Keeper, renamed `a_sent_comment_block_becomes_one_pill_with_one_row_per_comment`: decode of `with_comments` bytes through `split` (transcript/composer entry) into per-row location, L/R tag and body. `comments::tests` (L05) round-trips cover file comments and side-marker quoting, not this ordering. |
| R | `badges.rs::tests::a_multiline_body_rejoins_its_continuation_lines` | Decode side of continuation-line indentation; `comments::tests::multiline_bodies_indent_under_their_bullet` only checks the encoded bytes. |
| R | `badges.rs::tests::a_path_with_a_colon_still_splits_on_the_side_marker` | Side-marker parse when the path itself contains `:`; comments.rs covers the colon only for file comments. |
| R | `badges.rs::tests::a_comment_only_send_keeps_its_stand_in_body` | Comment-only send decodes back to `COMMENT_ONLY_TEXT` with a `1 comment` pill. |
| R | `change_requests.rs::tests::shared_checkout_keeps_conversation_branches_independent` | Two conversations on one checkout keep separate watch targets and match snapshots only by their own branch. |
| R | `change_requests.rs::tests::source_context_chat_finds_local_snapshot` | Keeper, renamed `change_request_for_chat_requires_the_exact_device_checkout_and_branch`: one table over `change_request_for_chat` (only owner of the matcher); this row is `same checkout -> Some(90)`. |
| C | `change_requests.rs::tests::remote_chat_requires_the_same_device` | Row `another device` of `change_request_for_chat_requires_the_exact_device_checkout_and_branch` (identical input/assertion). |
| C | `change_requests.rs::tests::mismatched_checkout_rejects_cwd_match` | Row `another checkout at the same cwd` of the matcher table (source checkout id differs from the snapshot's). |
| R | `change_requests.rs::tests::source_less_worktree_hides_stale_legacy_metadata` | Pre-source-context chats never surface legacy scalar branch/PR metadata or watch targets. |
| R | `change_requests.rs::tests::source_less_project_root_hides_legacy_branch_metadata` | Same rule for a project-root chat with a Space present. Production currently ignores `_spaces`, so it reaches the same early return as the worktree case, but it guards reintroducing a space-path fallback (retained when uncertain). |
| C | `change_requests.rs::tests::different_branch_hides_snapshot` | Row `another conversation branch` of the matcher table (legacy scalar `branch` still names the snapshot branch). |
| R | `change_requests.rs::tests::shared_chats_deduplicate_and_last_archive_removes_target` | Watch dedupe per checkout; the last archive removes the target. |
| R | `change_requests.rs::tests::unsupported_device_has_no_targets_or_visible_snapshot` | Version-skew negative cache hides snapshots and stops watches for an unsupported host. |
| R | `change_requests.rs::tests::host_upgrade_reenables_a_previously_unsupported_device` | Same version keeps the negative cache; a version change clears it. `state::tests::device_version_change_reenables_change_request_capability` proves the `apply_devices` wiring separately. |
| R | `change_requests.rs::tests::successful_none_clears_a_previous_pr` | A `None` snapshot replaces a stale PR badge. |
| R | `change_requests.rs::tests::local_and_remote_watch_params_route_to_the_checkout_host` | RPC param wire shape: `targetDeviceId` only when the checkout host is not this viewport. |
| R | `change_requests.rs::tests::badge_models_cover_open_merged_and_closed` | Table: PR state -> label/tone, number formatting, title newline flattening. |
| R | `file_icons.rs::tests::dark_icons_use_distinct_assets_with_brighter_accents` | Dark assets get distinct paths (image-cache invalidation) and recolored bytes; neutral colors untouched. |
| R | `file_icons.rs::tests::asset_source_preserves_authored_icon_bytes` | Light assets are served byte-identical to the embedded file. |
| R | `file_icons.rs::tests::exact_names_are_case_insensitive_and_ignore_parent_dots` | Resolution precedence table for exact basenames. |
| R | `file_icons.rs::tests::compound_extensions_win_longest_first` | Longest compound extension wins. |
| R | `file_icons.rs::tests::folders_resolve_name_and_expansion_state` | Folder name/expansion resolution. |
| R | `file_icons.rs::tests::appearance_independence_and_hints_are_supported` | Language and MIME hints resolve when the name does not. |
| R | `file_icons.rs::tests::symlinks_and_unknown_files_fall_back_cleanly` | Symlink and unknown fallbacks; `has_specific_file_icon` predicate. |
| R | `file_icons.rs::tests::every_manifest_reference_is_embedded_and_every_asset_is_svg` | Package contract: every manifest definition resolves to an embedded file (not covered by the bundle test, which iterates embedded files only). |
| R | `file_icons.rs::tests::complete_theme_bundle_is_registered` | Bundle registration (>=350 assets), every embedded svg has intrinsic size, foreign prefixes empty. |
| F | `icons.rs::tests::every_registered_icon_loads_and_parses` | Repaired a vacuous loop: it passed if `list("icons/")` returned nothing. Now asserts a non-empty listing and absorbs the two tests below (composite `Assets` contract). |
| C | `icons.rs::tests::unknown_paths_are_none` | Absorbed into `every_registered_icon_loads_and_parses` (`icons/nope.svg` loads as `None`). |
| C | `icons.rs::tests::list_filters_by_prefix` | Absorbed into `every_registered_icon_loads_and_parses` (non-empty icons listing, empty `fonts/` listing). |
| R | `lib.rs::activity_notification_tests::banner_target_requires_an_exact_l15_environment_and_task` | Notification click-target parser: exact `l15:<env>:<task>`, rejects empty parts and foreign prefixes. |
| R | `lib.rs::appshot_activation_tests::appshot_capture_skips_any_focused_viewer_window` | Real GPUI windows: capture refuses while any LUNA window is focused. |
| R | `motion.rs::tests::pulse_stride_reestablishes_after_each_paint` | Pulse-clock lease stride (perf regression: a retired fast animation must not leave a sticky stride). |
| R | `motion.rs::tests::eval_never_escapes_unit_interval_dense_sweep` | Keeper, renamed `catalog_curves_pin_endpoints_and_stay_monotonic_inside_the_unit_interval`: SIGABRT regression (eval escaped [0,1]); now also carries the endpoint/clamp and monotonicity checks for the same five curves (monotonicity at the 1e-5 sweep stride instead of 1e-2). |
| C | `motion.rs::tests::bezier_linear_is_identity` | Row `linear` of `bezier_known_values` (identity at the five sample points). |
| R | `motion.rs::tests::bezier_known_values` | Keeper: independently computed bezier references; absorbs the linear row; also pins EASE_OUT_EXPO/EASE_OUT/EASE control points. |
| C | `motion.rs::tests::bezier_endpoints_and_clamping` | Absorbed into `catalog_curves_pin_endpoints_and_stay_monotonic_inside_the_unit_interval` (same curve list). |
| C | `motion.rs::tests::bezier_is_monotonic_for_catalog_curves` | Absorbed into `catalog_curves_pin_endpoints_and_stay_monotonic_inside_the_unit_interval` (same curve list, denser stride). |
| R | `motion.rs::tests::spec_delay_holds_then_runs` | MotionSpec delay/progress semantics. |
| D | `motion.rs::tests::catalog_timings_match_luna` | Copied constant inventory: 877a43cc changed `MENU_IN` 140->180 and edited this assertion in lockstep (the module doc and feature-inventory §1.12 still say 0.14s), so it never guarded a spec. EASE_OUT_EXPO stays pinned by `bezier_known_values`. Unlocks deleting dead `CHEVRON` (its only reference). |
| D | `motion.rs::tests::pulse_wave_endpoints` | Replay of `luna_proto::motion` helpers re-exported by `pub use`; keeper `luna_proto::motion::tests::the_pulse_is_a_full_cosine_cycle` asserts the same seven values. |
| D | `motion.rs::tests::stagger_wraps_and_orders_cells` | First three asserts replay `luna_proto::motion::tests::stagger_offsets_each_cell_and_wraps` (identical rows plus a range sweep). The last assert only exercised `matrix_wave`, a pub fn with no non-test caller (the loaders drive the gradient matrix with `gspin_opacity`); deleted with it. |
| D | `motion.rs::tests::lerp_basics` | Restates `from + (to - from) * t`. `lerp` is exercised at stronger owners: `mix_endpoints_and_transparent_blend` (t=0/1 endpoints, exact 0.03 alpha midpoint) and `hover_fade_ramps_and_reverses_continuously`. |
| R | `motion.rs::tests::hover_fade_ramps_and_reverses_continuously` | Hover fade continuity on reversal. |
| R | `motion.rs::tests::hover_fade_reduced_motion_snaps` | Reduced motion snaps hover fades. |
| R | `motion.rs::tests::hover_fade_leave_without_enter_is_inert` | Leave-only keys create no entry. |
| R | `motion.rs::tests::hover_tick_reports_flight_and_prunes` | Frame requests stop when settled; rest entries pruned. |
| R | `motion.rs::tests::hover_tick_evicts_unread_entries` | Unmounted-mid-hover leak regression. |
| R | `motion.rs::tests::mix_endpoints_and_transparent_blend` | Premultiplied color blend (no grey mid-fade); keeper for `lerp`. |
| D | `motion.rs::tests::hover_spec_matches_tailwind_transition_colors` | Copied constants restating the `HOVER_FADE` and `EASE_TAILWIND` definitions (same lockstep pattern as the catalog test); no behavior exercised. |
| C | `motion.rs::tests::gspin_pulse_shape` | Replay of `luna_proto::motion::tests::gradient_spin_holds_dim_then_snaps_back` (L08), which already asserts start/dim/wrap rows and a bounds sweep. Its rest-band (0.9) and strictly-between easing rows (0.2, 0.96) go to L08 as a cross-lane request. |
| R | `motion/windows_pulse.rs::tests::native_high_resolution_ticks_are_delivered` | Platform contract: the real Windows high-resolution waitable timer delivers ticks (windows CI job). |
| R | `new_thread_background_effects.rs::tests::cold_artwork_fades_in_once_and_warm_navigation_does_not_restart_it` | Readiness fade: cold artwork fades once; warm navigation never restarts it. |
| R | `new_thread_background_effects.rs::tests::prewarming_decodes_off_thread_and_reuses_artwork_without_hero_geometry` | Real PNG decode off-thread, Arc reuse, BGRA byte order. |
| R | `new_thread_background_effects.rs::tests::every_effect_is_generated_once_independently_of_viewport` | One pending raster job per effect; cached Arc returned afterwards. |
| R | `new_thread_background_effects.rs::tests::light_treatments_use_light_paper_without_inverting_source_hues` | Light treatments brighten without inverting source hue. |
| R | `new_thread_background_effects.rs::tests::appearance_changes_cache_both_variants_and_share_unchanged_dither` | Both appearance variants cached (7 entries), dither shared. |
| R | `new_thread_background_effects.rs::tests::raster_treatments_preserve_source_dimensions_and_alpha` | Raster treatments keep dimensions and alpha. |
| R | `new_thread_background_mask.rs::tests::background_paint_sees_same_frame_composer_bounds_even_when_painted_first` | Real GPUI paint-order regression: the mask consumes same-frame composer bounds. |
| R | `new_thread_background_mask.rs::tests::mask_tracks_current_surface_in_window_space_without_rounding` | Mask layout math across sidebar/right-panel widths. |
| R | `new_thread_background_mask.rs::tests::taller_background_stays_cleared_below_the_composer` | Clearance extends to the hero bottom; feather clamps at 280. |
| F | `new_thread_background_mask.rs::tests::new_thread_cutout_reveal_preserves_the_bottom_fade_and_image_extent` | Dropped `assert_eq!(CUTOUT_REVEAL_OPACITY, 0.5)`, a copy of the constant's own definition; the reveal-mask assertions remain. |
| R | `new_thread_background_mask.rs::tests::new_thread_main_fade_uses_the_full_height_at_every_window_size` | Bottom fade spans the full image height for both passes. |
| R | `notify.rs::tests::applescript_escaping` | Security: AppleScript literal escaping (quotes, backslashes, newlines). CI filter `notify::tests::` (ui-tests.yml macOS job); the `cfg(any(target_os = "macos", test))` widening lets Linux CI run it too, so it is retained. |
| R | `notify.rs::tests::clicked_banner_reports_its_chat_id` | macOS native delegate routes banner clicks to the chat id (CI filter `notify::tests::`). |
| R | `pickers.rs::tests::provider_handoff_contains_only_bounded_visible_conversation_text` | Security/size contract: provider-switch handoff carries only visible text, bounded by `MAX_CONTEXT_HANDOFF_*`. |
| R | `pickers.rs::tests::provider_handoff_and_clear_confirmations_reset_at_lifecycle_boundaries` | Transient opt-ins reset on dismissal/retarget. |
| F | `pickers.rs::tests::permission_defaults_and_degradation_follow_provider_capabilities` | Removed two asserts on `provider_effective_permission_mode`, a `#[cfg(test)]` copy of engine degradation with no production caller; deleted the seam. Real degradation is `luna_engine::permissions::resolve_run_policy`, keeper `luna_engine` `permissions::tests::unsupported_auto_degrades_to_ask_never_full`. Selectable-mode inventory, defaults and preview rows retained. |
| R | `pickers.rs::tests::permission_chip_and_disconnected_change_preserve_authoritative_access` | Chip shows the host's effective mode; a disconnected change stages nothing and surfaces an error. |
| R | `pickers.rs::tests::switching_provider_drops_foreign_permission_draft` | Provider switch drops a foreign permission draft and blocks running. |
| R | `pickers.rs::tests::claude_auto_selection_is_blocked_for_an_ineligible_model` | Auto blocked for ineligible Claude models; also the keeper for `default_model` = first catalog row (a different default breaks the Auto-selection row). |
| R | `pickers.rs::tests::workspace_footer_pair_keeps_its_leading_edge_and_gap` | Real GPUI layout of the footer pair across widths. |
| R | `pickers.rs::tests::picker_completion_and_dismissal_have_distinct_focus_behavior` | Composer-focus return only on completion/trigger close. |
| R | `pickers.rs::tests::nested_model_settings_navigate_and_preserve_independent_choices` | Keyboard paths through nested settings incl. `simulate_keystrokes`. |
| R | `pickers.rs::tests::nested_model_menu_mouse_paths_work_on_both_sides` | Real mouse dispatch: submenu side, safe-cone grace, click/hover semantics. |
| R | `pickers.rs::tests::remembered_options_stay_valid_for_the_sent_model_without_a_catalog` | Remembered options never leak to another model; provider default wins on a cleared draft. |
| R | `pickers.rs::tests::tab_search_never_leaves_the_viewed_harness` | Scoped search stays in the viewed harness. |
| R | `pickers.rs::tests::favorites_tab_search_ranks_only_starred_rows` | Favorites tab lists only starred rows. |
| R | `pickers.rs::tests::harness_tab_lists_stars_first_and_description_still_matches` | Stars first; description stays searchable. |
| R | `pickers.rs::tests::normalize_drops_default_alias_and_folds_orphan_1m_rows` | Cross-version catalog normalization (older engines' alias/1M rows). |
| R | `pickers.rs::tests::normalize_gives_claude_rows_their_versioned_catalog_labels` | Claude rows adopt versioned labels. |
| R | `pickers.rs::tests::traits_summary_formats_non_defaults` | Trigger summary, stale-choice fallback, offered-option filtering, customized flag. |
| R | `pickers.rs::tests::folder_paths_and_breadcrumbs` | Path parent/child/breadcrumb math. |
| R | `pickers.rs::tests::completion_prefix_lengths` | Case-insensitive prefix length on byte boundaries. |
| R | `pickers.rs::tests::segment_target_resolution` | Exact-case > case-insensitive > unique prefix resolution. |
| R | `pickers.rs::tests::typed_path_target_expands_absolute_and_home_paths` | Table owner for typed paths; `spaces::project_flow_tests` cover two rows end to end. |
| R | `pickers.rs::tests::browser_navigation_reducer` | Asserts `browser_rows` hides files (name over-promises; follow-up rename). |
| R | `pickers.rs::tests::resolved_chat_config_requires_harness` | No config without a harness; derived sandbox. Mostly field copies (uncertain; retained, follow-up). |
| D | `pickers.rs::tests::default_model_is_first_catalog_row` | `default_model` is `models.first()`; the default-row contract is enforced through `resolved()` by `claude_auto_selection_is_blocked_for_an_ineligible_model` (catalog [fable-5-1, haiku-4-5] must default to the Auto-eligible first row). |
| R | `pickers.rs::tests::default_reasoning_prefers_high_then_medium` | Default contract: High, then Medium, then first level. |
| R | `pickers.rs::tests::clamp_reasoning_keeps_offered_levels_and_heals_foreign_ones` | Foreign remembered levels heal to the default. |
| R | `pickers.rs::tests::mock_harness_hidden_unless_alone` | Mock hidden in production unless alone or opted in. |
| R | `pickers.rs::tests::offered_harnesses_follow_the_catalog_enabled_flags` | Enabled flags filter; legacy catalogs follow install probes. |
| R | `pickers.rs::tests::offered_harnesses_require_an_installed_cli` | Regression #128: no fallback to uninstalled agents. |
| R | `pickers_project_defaults_tests.rs::project_defaults_tests::project_defaults_switch_restart_clear_and_preserve_draft_choices` | GPUI + persisted `ComposerDefaults`: project switch, restart, inherit, existing-task precedence. |
| R | `pickers_project_defaults_tests.rs::project_defaults_tests::project_defaults_unavailable_choice_is_retained_and_blocked` | Unavailable project choice is kept and blocks running. |
| R | `pickers_project_defaults_tests.rs::project_defaults_tests::project_defaults_provider_only_does_not_import_remembered_model` | Only owner of the `ProjectExecutionDefaults::resolve` provider-without-model rule (proto has no test). |
| R | `popover.rs::tests::trigger_press_note_distinguishes_dismiss_from_open` | Trigger click while open dismisses instead of reopening (both handler orders). |
| R | `popover.rs::tests::menu_step_wraps_and_enters` | Menu keyboard stepping table. |
| R | `popover.rs::tests::filter_ranks_prefix_before_substring` | Keeper for `match_rank` through `filter_indices`. |
| D | `popover.rs::tests::match_rank_kinds` | Every row (prefix 0, substring 1, miss, empty query) is proven through `filter_indices` by `filter_ranks_prefix_before_substring` (order, case-insensitivity, empty/whitespace query keeps input order, no-match empty). |
| R | `popover.rs::tests::key_classification` | Menu key classification incl. readline ctrl-n/p. |
| R | `popover.rs::tests::tracked_upper_spaces_letters` | Tracked-uppercase formatting. |
| D | `popover.rs::tests::loadable_accessors` | Trivial accessor/derive restatement (`ready`, `error`, `is_loading`, derived Default). `ready()`/`error()` are asserted at real flows in `spaces::project_flow_tests::stale_folder_and_drive_results_cannot_replace_current_device`. |
| R | `popover.rs::tests::scrollbar_metrics_hidden_when_content_fits_or_viewport_tiny` | Scrollbar layout math. |
| R | `popover.rs::tests::scrollbar_metrics_scales_thumb_to_content_ratio` | Scrollbar layout math. |
| R | `popover.rs::tests::scrollbar_metrics_clamps_min_thumb_and_position` | Scrollbar clamps. |
| R | `popover.rs::tests::horizontal_scrollbar_metrics_match_the_vertical_treatment` | Separate (duplicated) horizontal implementation; its own owner. |
| R | `popover.rs::tests::horizontal_scrollbar_hides_without_overflow_and_clamps_position` | Horizontal clamps. |
| R | `popover.rs::tests::scrollbar_press_and_drag_work_in_the_metrics_domain` | Track press and thumb drag targets. |
| R | `popover.rs::tests::hide_wakes_step_from_linger_end_through_the_fade` | Hide-timer wake schedule. |
| R | `popover.rs::tests::hide_timer_keeps_one_wake_in_flight` | Single wake in flight; drag holds the rail. |
| R | `popover.rs::tests::scrollbar_metrics_from_parts_matches_viewport_shape` | `from_parts` equals `from_viewport`. |
| R | `popover.rs::tests::reset_menu_scroll_rewinds_without_marking_motion` | Rewind is not motion. |
| R | `popover.rs::search_highlight_tests::inline_matches_keep_adjacent_word_boundaries` | Search highlight ranges. |
| R | `popover.rs::search_highlight_tests::highlights_repeated_case_insensitive_and_overlapping_words` | Repeated/overlapping merge. |
| R | `popover.rs::search_highlight_tests::preserves_original_unicode_boundaries_after_lowercase_expansion` | Unicode lowercase-expansion index mapping. |
| F | `power.rs::tests::acquire_release_switch_error_and_drop` | Dropped the `held_task()` assertion and the `#[cfg(test)] held_task` accessor; the adapter event log already proves the failed acquire left nothing held (`d` acquires with no preceding release; drop releases once). |
| R | `power.rs::tests::selection_requires_opt_in_fresh_local_working_status` | Keep-awake only for an opted-in, local, freshly Working task. |
| R | `project_commands_accessibility_tests.rs::accessibility_tests::command_card_rendered_edit_toggles_and_cancel` | Rendered card, held RPC: mouse and keyboard Setup/Wait toggles, tab order, Cancel saves nothing; writes trace artifacts. |
| R | `project_commands_accessibility_tests.rs::accessibility_tests::command_card_keyboard_remove_sends_one_synthetic_save` | Destructive Remove arms on first Enter, confirms with exactly one save, keeps focus. |
| R | `project_commands_accessibility_tests.rs::accessibility_tests::command_card_keyboard_save_is_single_flight` | Double Enter on Save issues one request while the reply is held. |
| R | `project_commands_engine_epoch_tests.rs::engine_epoch_tests::commands_engine_epoch_delayed_status_success` | Engine replacement via the production attach boundary: old status never reaches the new panel (HeldRpc, trace). |
| R | `project_commands_engine_epoch_tests.rs::engine_epoch_tests::commands_engine_epoch_delayed_status_error` | Same with an old-engine error. |
| R | `project_commands_engine_epoch_tests.rs::engine_epoch_tests::commands_engine_epoch_execute_same_device_generation` | Old execution never emits Output or terminal lookups on the replacement (same device). |
| R | `project_commands_engine_epoch_tests.rs::engine_epoch_tests::commands_engine_epoch_execute_different_device` | Same across devices. |
| R | `project_commands_engine_epoch_tests.rs::engine_epoch_tests::commands_engine_epoch_stuck_old_request_cannot_keep_new_engine_busy` | A held old request cannot keep the new engine busy; real poll tick. |
| R | `project_commands_engine_epoch_tests.rs::engine_epoch_tests::commands_engine_epoch_same_engine_control_remains_usable` | Control: same engine emits Output; dropped entities stop polling. |
| R | `project_commands_engine_epoch_tests.rs::engine_epoch_tests::commands_button_admission_stale_run` | Stale painted Run callback is not rebound to engine B (real mouse dispatch). |
| R | `project_commands_engine_epoch_tests.rs::engine_epoch_tests::commands_button_admission_stale_retry` | Same for Retry. |
| R | `project_commands_engine_epoch_tests.rs::engine_epoch_tests::commands_button_admission_stale_stop` | Same for Stop. |
| R | `project_commands_engine_epoch_tests.rs::engine_epoch_tests::commands_button_admission_live_busy_rejects_old_enabled_callback` | Old enabled button cannot dispatch while busy. |
| R | `project_commands_engine_epoch_tests.rs::engine_epoch_tests::commands_button_admission_fresh_same_engine_control` | Control: fresh button dispatches exactly once. |
| R | `project_commands_engine_epoch_tests.rs::engine_epoch_tests::commands_retry_current_stop_survives_status_poll` | Retry then Stop during a held status poll: exactly one write each. |
| R | `project_commands_engine_epoch_tests.rs::engine_epoch_tests::commands_captured_setup_disclosure_uses_receipt_not_edited_action` | Disclosure shows the captured receipt command/checkout, never the edited action. |
| R | `project_commands_same_chat_tests.rs::same_chat_context_tests::commands_same_chat_stale_run_project_move` | Same-chat project move: stale painted Run does not dispatch in the new context. |
| R | `project_commands_same_chat_tests.rs::same_chat_context_tests::commands_same_chat_stale_status_project_move` | Old status does not replace the moved project's data. |
| R | `project_commands_same_chat_tests.rs::same_chat_context_tests::commands_same_chat_stale_run_materialization` | Same for worktree materialization. |
| R | `project_commands_same_chat_tests.rs::same_chat_context_tests::commands_same_chat_stale_status_materialization` | Same for status. |
| R | `project_commands_same_chat_tests.rs::same_chat_context_tests::commands_same_chat_fresh_context_controls` | Control: fresh context dispatches once per transition. |
| R | `project_commands_same_chat_tests.rs::same_chat_context_tests::commands_same_chat_unrelated_metadata_control` | Control: title/label edits do not fence. |
| R | `queue.rs::tests::queue_edit_upload_ids_are_stable_distinct_and_accepted_by_the_upload_jail` | Real `luna_engine::Uploads` jail accepts the id (<=64 safe bytes; native-QA regression). |
| R | `queue.rs::tests::queue_preview_work_follows_the_visible_rows` | Virtualized preview window math. |
| F | `queue.rs::tests::primary_action_distinguishes_explicit_override_from_delivery_fences` | Kept the resolution table; dropped the copied tooltip-string inventory (bccd12d2 rewrote the SteeringAccepted text and this list in lockstep). The DeliveryUnknown text stays proven through Composer in `empty_modifier_queue_action_executes_one_fenced_send_now_and_blocks_unknown`. |
| R | `queue.rs::tests::empty_modifier_queue_action_executes_one_fenced_send_now_and_blocks_unknown` | Composer -> memory RPC: one fenced receipt command; unknown delivery blocked. |
| R | `queue.rs::tests::host_authoritative_queue_actions_route_to_the_host` | Copied method list, but an authority contract (owner-host routing for row mutations); only MOVE is also proven at a boundary. Retained when uncertain. |
| R | `queue.rs::tests::rejected_owner_queue_move_restores_the_watched_order` | Real composer/RPC/watch flow: false ACK and owner denial roll back without replay. |
| R | `queue.rs::tests::mutation_acknowledgements_detect_conflicts_and_malformed_replies` | Reply wire contract per method. |
| R | `queue.rs::tests::the_whole_panel_maps_to_a_clamped_queue_drop_slot` | Drop-slot math. |
| R | `queue.rs::tests::drag_offsets_move_the_real_row_and_open_its_destination` | Drag offsets. |
| R | `queue.rs::tests::rows_flatten_multi_line_messages` | One-line row text. |
| R | `queue.rs::tests::appshot_context_is_hidden_in_clean_and_legacy_queue_rows` | Appshot context hidden in queue text. |
| R | `queue.rs::tests::attachment_labels_decode_app_names_and_preserve_ordinary_images` | Attachment label decoding incl. malformed trailers. |
| R | `queue.rs::tests::legacy_attachment_trailers_are_hidden_from_queue_text` | Legacy trailers hidden. |
| R | `queue.rs::scroll_tests::queue_wheel_does_not_scroll_the_transcript_even_at_its_boundaries` | Real wheel dispatch: queue scroll never leaks to the transcript. |
| R | `queue.rs::appshot_edit_tests::restoring_displaced_draft_keeps_appshots_and_new_captures` | Draft restore keeps appshots captured during save. |
| R | `rail.rs::tests::capacity_counts_slots_that_fit` | Rail capacity and cap. |
| R | `rail.rs::tests::buckets_are_identity_under_capacity` | Bucket identity under capacity. |
| R | `rail.rs::tests::buckets_partition_evenly_over_capacity` | Even contiguous partition. |
| R | `rail.rs::tests::bucket_of_maps_ticks_to_their_bucket` | Bucket lookup and degenerate inputs. |
| R | `rail.rs::tests::ticks_map_user_prompts_with_reply_openings` | Prompt ticks with reply openings. |
| R | `rail.rs::tests::ticks_include_echoes_deduped` | Echo ticks deduped. |
| R | `rail.rs::tests::tick_without_reply_yet` | Pending prompt has no reply; empty transcript. |
| R | `rail.rs::tests::active_tick_tracks_viewport_top` | Active tick lookup. |
| R | `rail.rs::tests::rail_width_gate` | 768 px visibility breakpoint. |
| R | `rail.rs::tests::glide_timeline_matches_absolute_eased_interpolation` | Glide timeline telescopes to the absolute eased curve. |
| R | `rail.rs::tests::glide_timeline_survives_remaining_distance_reestimate` | Mid-flight re-estimate continues the same timeline. |
| R | `rail.rs::tests::glide_timeline_step_clamps` | Step clamps. |
| R | `rail.rs::tests::glide_first_frame_is_gentle` | First 16 ms frame < 2% of the distance (duration asserted as the premise). |
| R | `rail.rs::tests::preview_truncation` | Multi-byte-safe truncation. |
| R | `shell.rs::tests::settings_entry_and_fixture_routes_keep_their_targets` | Default section and fixture routes used by QA fixtures. |
| R | `shell.rs::tests::history_search_parses_all_supported_filters_and_requires_current_profile` | Search-filter parser; stale-profile project dropped; global scope key independent of selection. |
| R | `shell.rs::tests::history_review_drops_late_cancelled_and_profile_changed_responses` | Profile-unchanged predicate and flow liveness. |
| R | `shell.rs::tests::history_origin_rejects_delayed_response_with_identical_chat_metadata` | Keeper for `HistoryEngineOrigin::run_if_current` and generation tracking. |
| D | `shell.rs::tests::history_origin_blocks_fork_edit_and_restore_submission_after_replacement` | Loops three operation labels through the same one-line `run_if_current` closure; the labels never reach fork/edit/restore code (all go through the single `mutate_task_reviewed` call site), so the name promises more than it exercises. Stale/current rows remain in `history_origin_rejects_delayed_response_with_identical_chat_metadata`. |
| R | `shell.rs::tests::history_fork_permission_choice_preserves_immutable_l04_ceiling` | Permission ceiling for history forks; Ask never selectable. |
| R | `shell.rs::tests::sidebar_drag_nudges_each_edge_once_until_rearmed` | Keeper for the shared `motion::resize_drag_sample` clamp/latch through `sidebar_drag_sample`. |
| R | `shell.rs::tests::sidebar_drag_stays_exact_in_range_and_reduced_motion_never_nudges` | In-range exactness; reduced motion never bounces. |
| D | `shell.rs::tests::right_pane_uses_the_shared_clamp_and_edge_latch` | Calls the shared `motion::resize_drag_sample` with literal bounds (820), not the right-pane drag path, so it re-proves the helper's clamp/latch already owned by `sidebar_drag_nudges_each_edge_once_until_rearmed` (and the files/preview tests, L07). |
| R | `shell.rs::tests::sidebar_bounce_has_rounded_out_and_return_phases` | Only owner of `motion::resize_bounce_offset`. |
| R | `shell.rs::tests::every_default_shortcut_binds_on_this_platform` | Every default combo parses (dead-shortcut regression). |
| R | `shell.rs::tests::island_stays_centered_on_controls_while_expanding` | Titlebar island geometry. |
| F | `shell.rs::tests::new_thread_handoff_is_continuous_and_staged` | Renamed `new_thread_background_height_scales_and_clamps_with_the_viewport`: dropped the `bottom_stack_measurement_matches` truth table (the fn is `a == b`) and the `new_thread_background_opacity` restatement of its own constant; kept the height scaling/clamp math. The old name promised a handoff the test never exercised. |
| R | `shell.rs::tests::right_pane_ceiling_preserves_the_chat_floor` | Right-pane ceiling keeps the chat floor. |
| R | `shell.rs::tests::right_pane_takeover_consumes_the_chat_column` | Takeover width. |
| R | `shell.rs::tests::escape_interrupts_only_the_active_live_chat` | Keeper, renamed `escape_interrupts_only_an_opted_in_active_live_chat`: one table over `resolve_shell_escape`. |
| C | `shell.rs::tests::escape_ignores_non_live_or_ineligible_views` | Rows `blocking overlay`, `idle chat`, `no selected chat`, `already interrupting`, `settings route`, `another key` of the escape table. |
| C | `shell.rs::tests::escape_interrupt_is_opt_in` | Row `not opted in` of the escape table. |
| R | `shell.rs::tests::only_visible_sync_steps_block_escape` | Which sync steps block Escape. |
| R | `shell.rs::tests::right_pane_takeover_control_reverses_direction` | Expand/collapse icon direction (two-row mapping; retained). |
| D | `shell.rs::tests::pane_resize_hitboxes_yield_the_titlebar_chrome` | `PANE_RESIZE_HITBOX_TOP` is defined as `Theme::TITLEBAR_HEIGHT` (self-comparison); the other two lines copy literal constants. No behavior exercised. |
| R | `shell.rs::tests::right_panel_content_keeps_the_larger_width_only_during_transition` | Stable content width during transitions. |
| R | `shell.rs::tests::remote_shutdown_waits_for_ipc_release` | Real TCP listener release. |
| R | `shell.rs::tests::rejected_legacy_session_stops_and_reboots_local` | Real bootstrap, sign-out, stop and local reboot. |
| R | `shell.rs::tests::remote_shutdown_waits_for_engine_lock_release` | Real `InstanceLock` release (unix). |
| R | `shell.rs::tests::remote_shutdown_times_out_while_ipc_remains_open` | Timeout error path. |
| R | `shell.rs::tests::account_actions_follow_the_attached_workspace_scope` | Account action per scope. |
| R | `shell.rs::tests::local_sign_in_offers_the_in_place_switch` | Sync-flow reducer: switch offer, postponement, revocation. |
| R | `shell.rs::tests::import_summary_errors_are_a_failure_not_a_success` | Import summary wire tolerance; errors fail. |
| R | `shell.rs::tests::spaces_only_local_work_still_gets_the_import_offer` | Projects-only profiles get the import offer. |
| R | `shell.rs::tests::dismissed_import_failure_stays_reachable_on_a_synced_runtime` | Retry stays reachable after dismissal. |
| R | `shell.rs::tests::switch_lifecycle_survives_the_runtime_replacement_window` | Driver-owned states survive auth/scope edges. |
| R | `shell.rs::tests::synced_sign_out_blocks_every_viewport_and_cannot_switch_accounts` | Security: new credentials cannot reopen the previous account's store. |
| D | `shell.rs::tests::titlebar_cluster_matches_luna_window_controls` | The 88/12 cluster starts are re-proven by `titlebar_spacer_selects_per_platform_and_fullscreen` (78/2 spacers and declared-start equality), `linux_caption_controls_reserve_titlebar_space` (88) and `cluster_clearance_clears_the_overlay_buttons` (88/12 starts; 84 = 10 + CLUSTER_BUTTONS_WIDTH + 8 - 16). The other five lines copy constant literals. |
| R | `shell.rs::tests::titlebar_spacer_selects_per_platform_and_fullscreen` | Spacer per platform/fullscreen. |
| R | `shell.rs::tests::windows_caption_controls_reserve_titlebar_space` | Windows caption padding. |
| R | `shell.rs::tests::linux_caption_controls_reserve_titlebar_space` | Linux caption layout. |
| R | `shell.rs::tests::cluster_clearance_clears_the_overlay_buttons` | Overlay clearance math. |
| R | `shell.rs::tests::session_panels_default_closed_per_chat` | Panels default closed per chat. |
| R | `shell.rs::tests::session_panels_flags_are_chat_scoped` | Flags scoped per chat. |
| R | `shell.rs::tests::session_panels_both_flags_coexist_per_chat` | Terminal and changes coexist in one chat. |
| R | `shell.rs::tests::session_panels_update_tracks_right_surfaces` | Right-surface tracking per chat. |
| R | `shell.rs::tests::files_surface_is_single_instance_per_tab_list` | Files surface is unique. |
| R | `shell.rs::tests::file_editors_are_distinct_surface_tabs_with_stable_titles` | File editors distinct; titles. |
| R | `shell.rs::tests::sidebar_chat_height_is_uniform` | Row height scales with interface size. |
| F | `shell.rs::tests::sidebar_harness_geometry_reflects_row_hierarchy` | Dropped `SIDEBAR_MARK_GAP == Theme::SPACE_SM` (the const is defined as that value); kept the icon-fits-slot invariants. |
| R | `shell.rs::tests::sidebar_height_change_is_not_a_reorder` | Height changes are not reorders. |
| R | `shell.rs::tests::resort_offsets_empty_when_order_unchanged` | FLIP diff: no offsets without reorder. |
| R | `shell.rs::tests::resort_offsets_activity_moves_row_to_top` | FLIP diff for a move to top. |
| R | `shell.rs::tests::resort_offsets_respect_heights_and_gap` | FLIP diff with mixed heights. |
| R | `shell.rs::tests::resort_offsets_ignore_added_and_removed_keys` | FLIP diff ignores inserts/removals. |
| D | `shell.rs::tests::resort_glide_spec_matches_original` | Copied constant restating `RESORT` (260 ms, EASE_RESORT); same lockstep pattern as the motion catalog. |
| R | `shell.rs::tests::nav_history_starts_with_nothing_to_walk` | Nav history state machine. |
| R | `shell.rs::tests::nav_push_then_back_and_forward` | Back/forward walk. |
| R | `shell.rs::tests::nav_push_dedups_the_current_route` | Dedup of current route. |
| R | `shell.rs::tests::nav_push_truncates_the_forward_branch` | Forward branch truncation. |
| R | `shell.rs::tests::nav_replace_swaps_in_place` | Boot replace keeps Back disabled. |
| R | `shell.rs::tests::nav_settings_sections_are_distinct_entries` | Section changes are navigations. |
| R | `shell.rs::tests::sidebar_disclosure_motion_lands_exactly_on_its_target` | Disclosure tween lands exactly. |
| R | `shell.rs::exit_regressions::sidebar_usage_refresh_bridges_to_tokio_without_a_gpui_reactor` | Real Shell + memory RPC: usage refresh bridges to tokio. |
| R | `shell.rs::exit_regressions::command_palette_groups_actions_chats_and_content_and_restores_focus` | Palette grouping, keyboard navigation, focus restore. |
| R | `shell.rs::exit_regressions::command_palette_default_enter_opens_current_content_without_widening` | Positive default-Enter row (results present); the safety test covers pending/empty/error/offline. |
| R | `shell.rs::exit_regressions::command_palette_default_enter_is_safe_and_scope_requires_explicit_navigation` | Default Enter never widens scope or acts on pending/empty/error/offline. |
| R | `shell.rs::exit_regressions::command_palette_replacement_hides_stale_content_and_keeps_append_rows` | Replacement hides stale rows; append keeps rows. |
| R | `shell.rs::exit_regressions::command_palette_keeps_keyboard_target_stable_across_async_dedup` | Keyboard target stable across async dedup. |
| D | `shell.rs::exit_regressions::sidebar_renders_chat_navigation_without_l14_search_chrome` | Its only distinguishing assertion is that `debug_bounds("history-search-scope")` is absent, but 3900ad4c removed that id from production in the same commit, so the negative passes for an unrelated reason (no element carries the id anywhere). The rest (default Route::Chat, sidebar width > 0, a render call) is smoke covered by every drawing Shell-window test. |
| R | `shell.rs::exit_regressions::command_palette_rejects_delayed_query_project_and_profile_results` | Delayed results fenced by query, project and engine. |
| R | `shell.rs::exit_regressions::session_history_scope_fences_out_of_order_results_and_loads_older_pages` | Scope fencing and paging through a memory RPC. |
| R | `shell.rs::exit_regressions::context_handoff_follows_new_task_draft_owner_through_navigation_and_restart` | Draft-owner handoff through navigation and restart. |
| R | `shell.rs::exit_regressions::cancelled_or_profile_replaced_handoff_never_lands_on_an_unowned_draft` | Cancelled/replaced handoff never writes an unowned draft. |
| R | `shell.rs::exit_regressions::appshot_destinations_retain_last_session_and_use_new_canvas_defaults` | Appshot destination routing. |
| R | `shell.rs::exit_regressions::pane_geometry_uses_one_animation_time_per_frame` | One animation time per frame; reversal; image suspension. |
| R | `shell.rs::exit_regressions::panel_saves_preserve_settings_selected_outside_the_shell` | Settings persistence (storage contract) across concurrent writers. |
| R | `shell.rs::exit_regressions::right_terminal_picker_restores_saved_output_without_spawning` | Saved terminal output restored without spawning. |
| R | `shell.rs::exit_regressions::right_terminal_picker_with_no_history_waits_for_new` | No history waits for an explicit New. |
| R | `shell.rs::exit_regressions::opening_terminals_focuses_the_terminal_once` | Terminal focus once; redraws do not steal focus. |
| R | `shell.rs::exit_regressions::session_navigation_focuses_composer_once` | Composer focus once per navigation. |
| R | `shell.rs::exit_regressions::transcript_links_open_new_tabs_and_reject_stale_sessions` | Link routing, preference, stale-session rejection, release on close. |
| R | `shell.rs::exit_regressions::markdown_preview_events_open_browser_from_tree_and_file_tabs` | Preview link events from tree and file tabs. |
| R | `shell.rs::exit_regressions::browser_tabs_keep_session_ownership_and_release_on_close` | Browser tab ownership and release. |
| R | `shell.rs::exit_regressions::cmd_w_closes_the_active_right_pane_surface_before_the_window` | Cmd-W ladder. |
| R | `shell.rs::exit_regressions::lifecycle_actions_keep_pending_and_failed_file_saves_alive` | Quit/close/runtime/update wait for file saves. |
| R | `shell.rs::right_tab_mouse_regressions::subagent_close_press_does_not_start_parent_drag` | Real mouse: close press does not start a drag. |
| R | `shell.rs::right_tab_mouse_regressions::tab_strip_scrolls_over_chips_inside_titlebar` | Wheel scrolls the strip inside the titlebar. |
| R | `shell.rs::right_tab_mouse_regressions::subagent_tab_body_still_selects_drags_and_middle_closes` | Select, platform drag policy, middle-close. |
| R | `shell.rs::right_tab_mouse_regressions::subagent_tab_click_jitter_selects_without_starting_a_drag` | Windows click jitter does not drag (windows only). |
| R | `shell.rs::shortcut_focus_regressions::explicit_blur_does_not_refocus_a_mounted_input` | Explicit blur is respected. |
| R | `shell.rs::shortcut_focus_regressions::shortcuts_recover_from_retained_editor_focus` | Shortcuts recover from hidden focused editors. |
| R | `shell.rs::shortcut_focus_regressions::shortcuts_work_after_mouse_focus_changes` | Shortcuts dispatch right after mouse focus changes. |
| F | `shell/activity_center.rs::tests::completion_stays_in_activity_and_requests_can_notify_in_background` | The name promised background banners, but no test anywhere asserted `banner == true`. Added the unfocused approval-request row (banner + Request sound); a regression that never banners (e.g. inverted focus check) now fails. |
| R | `shell/activity_center.rs::tests::background_muted_and_wrong_device_routes_are_bounded` | Muted/important-only/wrong-device routes deliver nothing. |
| R | `shell/activity_center.rs::tests::async_snapshot_revision_fence_rejects_late_older_read` | Snapshot revision fence. |
| R | `shell/activity_center.rs::tests::taskless_local_connectivity_can_notify_without_a_selected_task` | Taskless connectivity attention sound. |
| R | `shell/command_palette.rs::tests::x11_unflagged_enter_repeats_activate_once_until_release` | X11 repeat regression: unflagged repeats activate once. |
| R | `shell/command_palette.rs::tests::flagged_enter_repeats_do_not_activate` | Flagged repeats never activate. |
| R | `shell/command_palette.rs::tests::action_search_hides_empty_section_and_preserves_order` | Action list order and filtering. |
| R | `shell/command_palette.rs::tests::theme_action_targets_the_opposite_resolved_appearance` | Theme action targets the opposite appearance. |
| R | `shell/command_palette.rs::tests::search_matches_words_across_chat_metadata` | Word matching across metadata. |
| R | `shell/spaces.rs::tests::activity_view_shows_actionable_and_running_tasks_not_idle_pins` | Activity ordering and buckets. |
| R | `shell/spaces.rs::tests::activity_attention_matches_the_exact_task_device` | Attention keyed by (device, task). |
| R | `shell/spaces.rs::tests::equal_sidebar_timestamps_sort_by_stable_chat_id` | Deterministic tie-break. |
| R | `shell/spaces.rs::tests::global_pin_order_precedes_recency_and_filtered_subsets_keep_it` | Pin order precedes recency. |
| R | `shell/spaces.rs::tests::current_device_is_promoted_without_resorting_remote_groups` | Local group promotion. |
| R | `shell/spaces.rs::tests::missing_current_device_leaves_group_order_untouched` | No-op when the local group is absent. |
| R | `shell/spaces.rs::project_flow_tests::canceled_move_origin_cannot_leak_into_reopened_add_project` | Real Shell: stale completion cannot leak a move origin. |
| R | `shell/spaces.rs::project_flow_tests::replaced_add_project_ignores_old_completion_and_returns_current_move_origin` | Replaced flow ignores old completion. |
| R | `shell/spaces.rs::project_flow_tests::devices_locations_folders_and_back_clear_stale_state` | Add-project steps clear stale state. |
| R | `shell/spaces.rs::project_flow_tests::standalone_opens_local_home_and_paths_require_return` | Local home; typed paths; pending `~` expansion. |
| R | `shell/spaces.rs::project_flow_tests::local_home_lists_children_and_manual_open_keeps_add_target` | Home listing and manual open. |
| R | `shell/spaces.rs::project_flow_tests::stale_folder_and_drive_results_cannot_replace_current_device` | Stale folder/drive results fenced by flow, request and device. |
| R | `shell/spaces.rs::project_flow_tests::existing_project_match_requires_exact_device_and_path` | Exact device+path match. |
| R | `shell/tabs.rs::cycle_tests::steps_forward_and_back_through_the_list` | Keeper, renamed `cycle_target_steps_wraps_and_enters_from_the_matching_end`: one table over `cycle_target` (pure fn, single owner). |
| C | `shell/tabs.rs::cycle_tests::wraps_at_both_ends` | Rows `Wraps at both ends` of the cycle table. |
| C | `shell/tabs.rs::cycle_tests::a_single_session_cycles_to_itself` | Rows `One row resolves in both directions` of the cycle table (comment carried). |
| C | `shell/tabs.rs::cycle_tests::no_selection_enters_the_list_from_the_matching_end` | Rows `No (or a vanished) selection` of the cycle table. |
| C | `shell/tabs.rs::cycle_tests::an_empty_list_has_nothing_to_select` | Rows `An empty list` of the cycle table. |
| R | `shell/usage.rs::tests::cache_savings_keeps_partial_price_coverage_visible` | Partial price coverage stays visible. |
| R | `shell/usage.rs::tests::reset_countdown_does_not_claim_a_past_window_is_current` | Reset label truthfulness. |
| R | `shell/usage.rs::tests::compact_quota_status_preserves_stale_and_last_good_truth` | Stale/last-good quota truth. |
| R | `shell/usage.rs::tests::pace_requires_a_fresh_verified_window` | Pace requires a fresh verified window. |
| R | `shell/usage.rs::tests::exact_model_rate_validation_rejects_unknown_as_zero` | Price override validation. |
| R | `shell/usage.rs::tests::repeated_local_hour_displays_distinct_dst_offsets` | DST-repeated hour shows offsets. |
| R | `sound.rs::macos_appshot::embedded_appshot_decodes_natively_without_playback` | macOS AVAudioPlayer decodes the embedded asset (platform/asset contract). |
| R | `sound.rs::tests::interrupted_and_expired_activity_never_chime` | No chime on interrupt/expiry. |
| R | `sound.rs::tests::a_run_error_chimes_once_and_never_masquerades_as_completion` | Error chimes once as Attention. |
| R | `sound.rs::tests::durable_connectivity_degradation_chimes_once_per_outage` | Connectivity chime once per outage. |
| R | `sound.rs::tests::ordinary_queue_completions_survive_coalesced_working_states` | Coalesced queue completions still chime. |
| R | `sound.rs::tests::pending_send_consumes_completion_but_preserves_input_requests` | Pending send consumes completion, not requests. |
| R | `sound.rs::tests::stale_completion_is_consumed_without_replaying_on_a_heartbeat` | Stale completion never replays. |
| R | `sound.rs::tests::temp_files_are_exclusive_and_cleaned_up` | Exclusive temp files cleaned up. |
| R | `sound.rs::tests::embedded_chimes_are_wav` | Embedded asset format contract. |
| R | `sound.rs::tests::connectivity_boot_outages_seed_silently_then_later_outages_alert` | Boot outages seed silently. |
| R | `sound.rs::tests::attention_gate_coalesces_session_and_connectivity_watch_callbacks` | Attention gate coalescing window. |
| R | `state.rs::tests::remote_luna_approval_never_sends_an_rpc_frame` | Security: local-only approvals never cross the wire. |
| R | `state.rs::tests::fixture_local_handle_delegates_approval_to_its_synthetic_core` | In-process approval path; feature `appshots-fixture` only, so CI's featureless `--lib` run never executes it (follow-up). |
| R | `state.rs::tests::legacy_daemon_identity_falls_back_to_synced_scope` | Legacy identity wire fallback -> Synced/SignIn. |
| R | `state.rs::tests::remote_viewport_refuses_legacy_daemon_identity` | Real WS listener: legacy identity refused, no profile created. |
| R | `state.rs::tests::bootstrap_embeds_engine_when_port_is_free` | Real bootstrap: in-process engine, Ready, local approval path. |
| R | `state.rs::tests::bootstrap_reports_local_assembly_failure_before_returning_a_handle` | Corrupt store fails bootstrap and releases the port. |
| R | `state.rs::tests::deferred_engine_failure_remains_observable_after_early_attach` | Deferred failure observable after attach. |
| R | `state.rs::tests::remote_viewport_observes_deferred_engine_failure` | Remote viewport observes deferred failure over IPC. |
| R | `state.rs::tests::an_embedded_engine_serves_the_ipc_port_for_other_viewports` | Second viewport attaches; port released on shutdown. |
| R | `state.rs::tests::concurrent_bootstraps_elect_one_embedded_engine` | Bootstrap election: exactly one embeds. |
| R | `state.rs::tests::a_stranger_on_the_ipc_port_does_not_wedge_the_window` | Foreign listener refused; recovery after release. |
| R | `state.rs::tests::production_bootstrap_opens_local_data_without_sign_in` | Local profile, AuthStatus SignedOut, no dev-user data. |
| R | `state.rs::tests::invalid_legacy_session_starts_local_engine_without_cloud_onboarding` | Legacy session file still opens Local without cloud data. |
| R | `state.rs::tests::bootstrap_connects_when_daemon_is_listening` | Remote attach to a daemon; STOP_ENGINE not exposed. |
| R | `state.rs::tests::bootstrap_refuses_a_luna_listener_for_another_data_root` | Security: data-root ownership claim. |
| R | `state.rs::tests::transcript_revision_tracks_replay_echoes_and_subagent_content` | Transcript revision invalidation rules. |
| R | `state.rs::tests::session_heartbeats_preserve_freshness_without_redrawing_unchanged_status` | Heartbeat redraw rules. |
| R | `state.rs::tests::unchanged_device_heartbeat_still_retires_stale_session_indicators` | Device tick retires stale indicators. |
| R | `state.rs::tests::device_heartbeats_keep_freshness_without_repainting_unchanged_presentation` | Device presence redraw rules. |
| R | `state.rs::tests::local_workspace_hides_the_unknown_device_sentinel` | Local workspace naming. |
| R | `state.rs::tests::device_version_change_reenables_change_request_capability` | `apply_devices` wiring of the change-request negative cache. |
| R | `state.rs::tests::transfer_percent_tracks_snapshots_by_upload_id` | Transfer percent snapshots, 99% cap. |
| R | `state.rs::tests::upload_progress_percent_clamps_and_clears` | Upload percent clamps. |
| R | `state.rs::tests::send_pending_overlays_working_until_the_grace_window` | Pending-send overlay bounded by grace. |
| R | `state.rs::tests::send_pending_acked_when_the_host_writes_the_message_back` | Ack on write-back. |
| R | `state.rs::tests::send_failure_cleanup_only_ends_its_own_overlay` | Late cleanup ends only its own overlay. |
| R | `state.rs::tests::chats_sort_by_last_message_desc_with_created_fallback` | Only owner of `luna_proto::view::sort_chats` (proto has no test; follow-up move). |
| R | `state.rs::tests::chat_sort_ties_are_deterministic` | `sort_chats` tie-break. |
| R | `state.rs::tests::working_indicator_staleness` | Only owner of `luna_proto::view::effective_indicator` staleness boundary. |
| R | `state.rs::tests::indicator_kinds` | `effective_indicator` per status. |
| R | `state.rs::tests::display_status_derivation` | Only owner of `luna_proto::view::display_status`. |
| R | `state.rs::tests::active_list_sorts_by_recency_only_status_never_moves_rows` | `sort_active`: status never moves rows (user report). |
| R | `state.rs::tests::tabs_order_by_creation_not_activity` | `sort_tabs` by creation. |
| R | `state.rs::tests::apply_spaces_sorts_and_heals_selection` | Space selection healing. |
| R | `state.rs::tests::selected_transcript_watch_applies_later_doc_changes_without_rearming` | Real EngineCore + Loro doc write reaches the standing watch. |
| R | `state.rs::tests::chats_in_space_filters_and_orders` | Space filter/order and overview. |
| R | `state.rs::tests::sidebar_all_projects_excludes_chats_without_project` | Projectless chats hidden from the sidebar. |
| R | `state.rs::tests::apply_chats_drops_vanished_selection` | Vanished selection dropped. |
| R | `state.rs::tests::apply_chat_config_stamps_the_row` | Config stamped on the right row; unknown chat is a no-op (no panic). |
| R | `state.rs::tests::visible_chats_filters_archived` | Archived hidden. |
| R | `state.rs::tests::jump_slots_count_the_rows_the_sidebar_draws` | Jump slots follow drawn rows (referenced by the tabs.rs comment). |
| R | `state.rs::tests::archive_shortcut_only_targets_an_open_active_chat` | Archive shortcut target. |
| R | `state.rs::tests::echoes_show_until_doc_frame_confirms` | Echo lifecycle. |
| R | `state.rs::tests::transcript_replay_barrier_requires_the_opening_reset` | Replay barrier needs the opening Reset. |
| D | `state.rs::tests::gate_phases` | Coordinator: deleted after L08 folded its last three rows (Connecting, Failed, Synced+SignedIn) into `luna_proto::view::gate_tests::gate_phase_resolves_connection_scope_and_auth`; `state::gate_phase` is that function re-exported. Lane note: Its Local, Synced+SignedOut and NeedsOrganization rows duplicated `luna_proto::view::gate_tests` (L08); trimmed to the rows proto does not cover (Connecting, Failed, Synced+SignedIn). Cross-lane request: move those three rows into proto, then delete this test. |
| R | `state.rs::tests::auth_changes_do_not_change_a_local_runtime_scope_or_watches` | Auth updates never change a Local scope or watches. |
| R | `state.rs::tests::auth_frames_parse_both_wire_shapes` | Only owner of `parse_auth_state` wire shapes. |
| R | `state.rs::tests::project_labels_from_cwd` | `project_label` table. |
| R | `state.rs::tests::grouped_sidebar_preserves_recency_order` | `group_chats` order. |
| R | `state.rs::tests::relative_times_match_luna_format` | `format_time_ago` table. |
| R | `state.rs::tests::chat_location_joins_project_and_branch` | `chat_location` table. |
| R | `state.rs::tests::org_gate_reducers` | Org name validation, ListOrgs tolerant parse, sort/dedupe. |
| R | `state.rs::tests::review_comment_body_updates_are_scoped_and_keep_current_metadata` | Review-comment updates scoped per chat. |
| R | `state.rs::tests::review_comment_flush_waits_for_every_file_surface` | Flush waits for every surface. |
| R | `state.rs::tests::version_triple_parses_and_gates_device_features` | Only owner of `luna_proto::version_triple` and the device gate. |
| R | `state.rs::tests::explicit_capabilities_distinguish_same_version_builds` | Capabilities over versions. |
| R | `state.rs::tests::delivery_degradation_and_queued_sends_tell_the_truth` | Delivery degradation, queued/undelivered truth. |
| R | `task_commands.rs::tests::context_intents_carry_revision_and_binding_fences` | Keeper, renamed `context_and_organization_intents_carry_revision_and_binding_fences`: fence inventory for task intents (concurrency contract; copied list retained when uncertain). |
| C | `task_commands.rs::tests::organization_intents_carry_revision_and_binding_fences` | Same two predicates over a second list; folded into the keeper's single list. |
| R | `workspace_links.rs::tests::resolves_relative_absolute_and_location_links` | Link parser table. |
| R | `workspace_links.rs::tests::resolves_canonical_file_mentions` | `luna-file:` mentions, percent decoding. |
| R | `workspace_links.rs::tests::rejects_external_and_unsafe_targets` | Security: traversal and foreign roots rejected. |

## Layer plan

Most of this lane is already at the strongest boundary the UI has: headless GPUI windows driving the real `Shell`, `Pickers`, `Composer` and `TerminalPanel` entities, the production engine-attach path with `engine_epoch_fixture::HeldRpc`, memory-RPC services, and real `EngineHandle::bootstrap` runs over TCP/IPC with real `EngineCore`/Loro documents. Those suites are the keepers and are untouched. The redundant layers removed were:

1. **UI replays of `luna_proto` helpers.** `motion.rs` re-exports `pulse_wave`, `pulse_opacity`, `pulse_scale`, `staggered_phase` and `gspin_opacity` from `luna_proto::motion`, whose own suite already asserts the same rows. Retired `pulse_wave_endpoints`, `stagger_wraps_and_orders_cells`, `gspin_pulse_shape`. Keepers: `luna_proto::motion::tests::{the_pulse_is_a_full_cosine_cycle, stagger_offsets_each_cell_and_wraps, gradient_spin_holds_dim_then_snaps_back}`. `state::tests::gate_phases` was trimmed to the rows `luna_proto::view::gate_tests` lacks.
2. **Copied design-constant inventories** that restate a definition and were edited in lockstep with it: `motion::tests::{catalog_timings_match_luna, hover_spec_matches_tailwind_transition_colors}`, `shell::tests::{resort_glide_spec_matches_original, pane_resize_hitboxes_yield_the_titlebar_chrome, titlebar_cluster_matches_luna_window_controls}`, plus constant self-assertions inside `new_thread_cutout_reveal_preserves_the_bottom_fade_and_image_extent`, `sidebar_harness_geometry_reflects_row_hierarchy` and the queue tooltip list. No keeper is needed for a constant; where a constant drives behavior, the behavior test stays (`bezier_known_values` pins the expo control points, the titlebar layout tests pin 88/12/82).
3. **Shared-helper replays and trivial restatements** with a stronger owner: `right_pane_uses_the_shared_clamp_and_edge_latch` → `sidebar_drag_nudges_each_edge_once_until_rearmed`; `match_rank_kinds` → `filter_ranks_prefix_before_substring`; `lerp_basics` → `mix_endpoints_and_transparent_blend`; `default_model_is_first_catalog_row` → `claude_auto_selection_is_blocked_for_an_ineligible_model`; `loadable_accessors` → the Loadable reads in `spaces::project_flow_tests`; `history_origin_blocks_fork_edit_and_restore_submission_after_replacement` → `history_origin_rejects_delayed_response_with_identical_chat_metadata`.
4. **A negative control that cannot fail:** `sidebar_renders_chat_navigation_without_l14_search_chrome` asserted the absence of an element id that no production code has emitted since 3900ad4c.
5. **Near-duplicate unit tests folded into one table-driven owner** (assertions carried verbatim): `change_request_for_chat_requires_the_exact_device_checkout_and_branch` (4 → 1), `cycle_target_steps_wraps_and_enters_from_the_matching_end` (5 → 1), `escape_interrupts_only_an_opted_in_active_live_chat` (3 → 1), `context_and_organization_intents_carry_revision_and_binding_fences` (2 → 1), `bezier_known_values` (+ linear row), `catalog_curves_pin_endpoints_and_stay_monotonic_inside_the_unit_interval` (dense-sweep regression + endpoint/clamp + monotonic), `every_registered_icon_loads_and_parses` (3 → 1, now non-vacuous), `a_sent_comment_block_becomes_one_pill_with_one_row_per_comment` (2 → 1).

**Assertion added (F, passes the authoring gate):** `activity_center::tests::completion_stays_in_activity_and_requests_can_notify_in_background` now asserts that an approval request banners when the app is unfocused under the background-only policy. Behavior: desktop banners for requests. Regression: a broken focus/background condition that never banners (no test asserted `banner == true` anywhere). Not covered elsewhere. No seam: `route_transition` is the production router.

**Production seams and dead code removed (only references were the deleted assertions, verified with `rg` across `crates/` and `apps/`):**

- `pickers.rs`: `#[cfg(test)] fn provider_effective_permission_mode` (a test-only re-implementation of engine permission degradation; the real owner is `luna_engine::permissions::resolve_run_policy`, tested by `permissions::tests::unsupported_auto_degrades_to_ask_never_full`).
- `power.rs`: `#[cfg(test)] fn KeepAwake::held_task` (the adapter event log already proves the state).
- `motion.rs`: `pub const CHEVRON` (its doc said "keep only for callers that still crossfade"; there were none) and `pub fn matrix_wave` (no non-test caller in `crates/` or `apps/`; `loaders.rs` drives the gradient matrix with `gspin_opacity`).

**Seams inspected and retained:** `state.rs` `EngineHandle::from_test_client`, `AppState::set_test_engine`, `AppState::attach_engine_for_epoch_fixture` (used by composer, attachments, queue, pane_chat, goals_e2e, settings and `engine_epoch_fixture.rs` in other lanes); `lib.rs` `engine_epoch_fixture`/`test_platform` modules (shared support used by L05/L07 tests); `notify.rs` `#[cfg(any(target_os = "macos", test))] applescript_escape` (lets Linux CI run the injection-escaping test); `app_menus::app_key_bindings(macos: bool)` (production calls it with `cfg!`).

## Cross-lane requests

1. **L08, `crates/proto/src/motion.rs`** (`tests::gradient_spin_holds_dim_then_snaps_back`): add the rows retired with `ui::motion::tests::gspin_pulse_shape`:
   ```rust
   close(gspin_opacity(0.9, GSPIN_DIM), GSPIN_DIM, "rest band");
   let fall = gspin_opacity(0.2, GSPIN_DIM);
   assert!(fall > GSPIN_DIM && fall < 1.0, "eases down");
   let rise = gspin_opacity(0.96, GSPIN_DIM);
   assert!(rise > GSPIN_DIM && rise < 1.0, "eases up");
   ```
2. **L08, `crates/proto/src/view.rs`** (`gate_tests`): add the three rows still held by `ui::state::tests::gate_phases` — `Connecting → Loading`, `Failed("boom") → Failed("boom")`, `Ready + Synced + SignedIn{org_id: None} → Ready`. Once landed, delete `state::tests::gate_phases` (L06 file; the coordinator can drop it in the merge).
3. **L05, `crates/ui/src/comments.rs`:** `badges::tests` is the keeper for decoding *diff-side* comment blocks through `badges::split`; the file-comment and side-marker-quoting decode rows exist only in `comments::tests` (`file_comments_use_current_workspace_locations_without_diff_tags`, `mixed_editor_and_diff_comments_share_one_review_block`, `file_comment_body_may_quote_a_diff_side_marker`, `a_body_quoting_a_side_marker_survives_the_round_trip`). Do not retire those as duplicates of badges.
4. **L07, `crates/ui/src/files/preview.rs`:** its `resize_drag_sample` tests are now one of two owners of the shared `motion::resize_drag_sample` latch (the other is `shell::tests::sidebar_drag_nudges_each_edge_once_until_rearmed`); keep at least one.
5. **CI workflow routing:** none needed.

## Possible product defects

None found. No retained test looked like it encodes a bug. Two documentation discrepancies (not product behavior):

- `crates/ui/src/motion.rs` module header and `docs/research/feature-inventory.md` §1.12 still say `menu-in 0.14s`; `MENU_IN` is 180 ms ease-out-quint since 877a43cc.
- `crates/ui/src/state.rs` (comment above `pub use luna_proto::view::{…}`) says those helpers have "their own test suite" in `luna_proto::view`; their only tests are in `state::tests`.

## Follow-ups

- Move the `luna_proto::view` helper tests (`working_indicator_staleness`, `indicator_kinds`, `display_status_derivation`, `active_list_sorts_by_recency_only_status_never_moves_rows`, `tabs_order_by_creation_not_activity`, `chats_sort_by_last_message_desc_with_created_fallback`, `chat_sort_ties_are_deterministic`, `project_labels_from_cwd`, `grouped_sidebar_preserves_recency_order`, `relative_times_match_luna_format`, `chat_location_joins_project_and_branch`, `auth_frames_parse_both_wire_shapes`) and `version_triple_parses_and_gates_device_features` (first half) into `crates/proto` beside their production owner, and fix the state.rs comment. Needs L06 + L08 together, so it was not done here.
- `state::tests::fixture_local_handle_delegates_approval_to_its_synthetic_core` is `#[cfg(feature = "appshots-fixture")]`; CI's featureless `--lib` runs never execute it. Route a featured run or move it to the fixture.
- `pickers::tests::browser_navigation_reducer` only asserts `browser_rows` hides files (rename); `resolved_chat_config_requires_harness` mostly copies fields back and leaves the derived `permission_degradation_reason` unasserted.
- `queue::tests::host_authoritative_queue_actions_route_to_the_host` and the task-command fence inventory copy `matches!` lists; replace with boundary proofs (as `rejected_owner_queue_move_restores_the_watched_order` does for MOVE) when convenient.
- `change_requests.rs`: `desired_watch_targets`, `conversation_branch` and `change_request_for_chat` ignore their `spaces` parameters; `source_less_project_root_hides_legacy_branch_metadata` is retained only as a guard against re-adding a space-path fallback.
- Risk note for the coordinator: the monotonicity check in `catalog_curves_pin_endpoints_and_stay_monotonic_inside_the_unit_interval` now runs at the 1e-5 sweep stride (tolerance 1e-4) instead of 1e-2; Newton's 1e-6 x-tolerance bounds y-error well inside that, but it is the one behavioral tightening in this lane.
