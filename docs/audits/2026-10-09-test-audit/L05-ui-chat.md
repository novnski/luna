# L05 ui-chat — test-audit ledger

- **Lane:** L05-ui-chat (campaign 2026-10-09, coordinator-run builds/tests).
- **Branch:** `test-audit/L05-ui-chat`, from `e4a0f534`.
- **Scope:** the chat view family in `crates/ui/src` (25 files): `attachments.rs`, `comments.rs`, `composer.rs`, `composer_dock.rs`, `composer_dock/panel_handoff.rs`, `context_usage.rs`, `edge_fade.rs`, `history.rs`, `image_media.rs`, `image_viewer.rs`, `links.rs`, `loaders.rs`, `markdown/{link_destination,link_interaction,link_presentation,links,mend,mermaid,parser,render,selection,veil}.rs`, `syntax_cache.rs`, `transcript.rs`, `typography.rs`. Inline tests and the production code in those files. Fixtures in `crates/ui/examples` belong to L10.
- **Baseline (`e4a0f534`):** 426 test declarations (`#[test]`, `#[gpui::test]`, `#[tokio::test]`); 13,388 lines inside `#[cfg(test)]` modules; 53,406 total lines in scope.
- **After this lane:** 408 declarations, 13,114 test-module lines, 53,086 total lines. Diff against the baseline: test code +15/-289, production code +5/-51, plus this ledger.
- **Marks:** R 407, F 1, C 3, D 15.
- **CI routing:** `.github/workflows/ui-tests.yml` runs `cargo test --release --locked -p luna-ui --lib -- --test-threads=1` on `ubuntu-24.04`. `windows.yml` runs the same lib target. Neither names a test in this scope. Linux-gated keepers run only on the ubuntu job: `transcript::tests::user_fold_scroll` (25), `link_interaction::rendered_tests` (5), the image viewer lightbox, and 3 composer input tests.
- **Boundary note:** CI does not run the `crates/ui/examples` appshot fixtures, so in-crate tests are the only automated proof for most chat-view behaviour. The strongest keepers here are the GPUI-rendered tests (`TestAppContext`, `test_platform::headless`), the `receive_transcript_update` reducer, and the composer draft lifecycle over an in-memory RPC client. Most pure-function tables have no stronger owner, so this lane retains most tests.

## Ledger

| Mark | Test | Evidence |
| --- | --- | --- |
| R | `attachments.rs::tests::appshot_cards_follow_their_exact_image_reference` | Persisted prompt text transport: an appshot card binds to its exact image ref (paths with `&`), ordinary refs stay plain. Only proof of `presentations` lookup inside `parse_user_message_images`. |
| R | `attachments.rs::tests::duplicate_or_invalid_appshot_metadata_stays_an_ordinary_attachment` | Security/privacy: duplicate appshot metadata never attaches AX text to an image and context stays hidden (`text == "Question"`). Keeper for the context-strip contract. |
| R | `attachments.rs::tests::queue_images_have_a_small_retained_pixel_budget` | Memory bound of `queue_thumbnail_image` (<=160x112). Only proof; regression = queue rows retaining full-size decodes. |
| R | `attachments.rs::tests::with_attachments_round_trips_through_parse` | Wire/persistence contract of the attachment trailer (path, basename, `index:path` id). Codec owner; transcript row test covers a single ref only. |
| R | `attachments.rs::tests::image_only_send_hides_placeholder_body` | Prompt-byte contract: image-only sends persist `ATTACHMENT_ONLY_TEXT` (only assertion of the stand-in body bytes) and display empty. |
| D | `attachments.rs::tests::appshot_context_is_hidden_but_image_remains` | Duplicate invocation. Detects: appshot context stripped while the ref survives. Remaining proof: `tests::duplicate_or_invalid_appshot_metadata_stays_an_ordinary_attachment` (same CONTEXT_MARKER strip, `attachments[0]` must exist), `tests::appshot_cards_follow_their_exact_image_reference`, `tests::with_attachments_round_trips_through_parse` (path parse), `appshots::tests::queued_edit_round_trip_keeps_context_and_rebinds_uploaded_images`. Unlocks: 12 test lines; no seam. |
| R | `attachments.rs::tests::plain_text_passes_through_unchanged` | Negative contract: no trailer means text untouched and no refs (guards false-positive parsing of ordinary prompts). |
| R | `attachments.rs::tests::marker_is_case_insensitive_and_requires_ref_lines` | Parser edge cases of the persisted trailer (case-insensitive marker, empty trailer stays text). |
| R | `attachments.rs::tests::rail_text_summarizes_image_only_sends` | User-visible rail copy (`Attached image` / `N attached images`); only proof of `user_message_rail_text`. |
| R | `attachments.rs::tests::ensure_extension_matches_browser_heuristic` | Staged names carry type extensions agents sniff; table of real edge cases (`photo.j`, `archive.tar.gz`). |
| R | `attachments.rs::tests::supported_formats_match_engine_jail` | Cross-crate contract with the engine read-back jail (`mime_by_ext`); regression = staging a format the host refuses. |
| R | `attachments.rs::tests::retry_ladder_is_2s_doubling_capped_at_15s` | User-visible backoff contract (2s doubling, 15s cap) of transcript image retries. |
| R | `attachments.rs::tests::upload_chunk_fits_the_relay_frame_ceiling` | Protocol contract: chunk + envelope under the 1 MiB relay frame and base64-aligned. Static but independent. |
| R | `attachments.rs::tests::chunk_ranges_cover_the_buffer_exactly` | Upload chunk planning (empty file one chunk, exact multiple, contiguous tiling). Pure function owner. |
| R | `attachments.rs::tests::attachment_deadline_scales_and_caps` | Whole-upload deadline formula from a user report (no hour-long `Sending...`). |
| R | `attachments.rs::generated_image_tests::generated_image_cache_isolates_policy_aliases_and_load_claims` | Cache identity includes validation policy (mime); aliases never satisfy generated keys; load claims per policy. Only proof. |
| R | `attachments.rs::generated_image_tests::generated_image_history_is_evicted_with_decoded_memory_accounting` | Budgeted LRU eviction with decoded-pixel accounting for generated images. Only eviction proof. |
| R | `attachments.rs::generated_image_tests::generated_image_decoder_checks_actual_type_and_downsamples` | Security/memory: generated metadata mime must match sniffed type; 3072px downsampled to 2048. Only proof for `decode_generated_image` mismatch/downsample. |
| R | `attachments.rs::generated_image_tests::generated_image_animation_retains_only_first_static_frame` | Only proof that a valid `image/gif` generated image is accepted (mime allow-list) and flattened to a first-frame PNG on the thumbnail path. |
| R | `attachments.rs::generated_image_tests::generated_image_chunk_reader_targets_owner_and_bounds_decode` | RPC param contract (`path`, `targetDeviceId`) for READ_ATTACHMENT_CHUNK plus decode bounds through `read_attachment_image`. |
| R | `attachments.rs::generated_image_tests::generated_image_cancelled_load_releases_claim_without_losing_completed_images` | Lifecycle: dropping `AttachmentLoadGuard` releases a claim but never clobbers a completed load. Distinct from the alias test. |
| R | `attachments.rs::generated_image_tests::generated_image_decode_accepts_attachment_byte_cap_but_rejects_excess` | Byte-cap contract: 24 MiB intake cap independent of the 8 MiB preview cap. |
| R | `comments.rs::tests::empty_set_leaves_the_prompt_untouched` | Prompt-byte contract: no comments means prompt unchanged. |
| R | `comments.rs::tests::comments_append_as_located_bullets` | Prompt-byte contract: exact `- path:line (R\|L): body` bullets the agent reads. Keeper for unrenamed diff citations on both sides. |
| R | `comments.rs::tests::comment_only_send_gets_a_body` | Prompt-byte owner for `COMMENT_ONLY_TEXT`. `badges::tests::a_comment_only_send_keeps_its_stand_in_body` asserts the same bytes through the badge split; kept here as the codec owner (see cross-lane note). |
| R | `comments.rs::tests::multiline_bodies_indent_under_their_bullet` | Prompt-byte contract for multi-line bodies (continuation indentation); badges test checks only the parsed body. |
| R | `comments.rs::tests::file_comments_use_current_workspace_locations_without_diff_tags` | File-comment header and untagged citation bytes plus round trip; only proof of REVIEW_COMMENT_BLOCK_HEADER. |
| R | `comments.rs::tests::mixed_editor_and_diff_comments_share_one_review_block` | Mixed diff/file comments share one block; colon paths still parse. Only mixed-source proof. |
| R | `comments.rs::tests::file_comment_body_may_quote_a_diff_side_marker` | Parser edge: an untagged file comment quoting `(L):` keeps its body. |
| R | `comments.rs::tests::chip_label_pluralizes` | User-visible chip copy; trivial but the only proof. |
| R | `comments.rs::tests::a_body_quoting_a_side_marker_survives_the_round_trip` | Parser edge: a tagged diff comment quoting `(L):` keeps tag R and full body. |
| R | `comments.rs::tests::a_renamed_file_cites_the_side_the_line_lives_in` | Rename contract: Old-side lines cite the pre-rename path; grouping key unchanged. |
| D | `comments.rs::tests::an_unrenamed_file_cites_its_only_path_on_both_sides` | Duplicate invocation. Detects: unrenamed Old/New comments cite `path`. Remaining proof: `tests::comments_append_as_located_bullets` asserts `- src/lib.rs:7 (L)` and `- src/main.rs:42 (R)` for unrenamed comments; rename branch owned by `tests::a_renamed_file_cites_the_side_the_line_lives_in`. Unlocks 10 test lines. |
| R | `comments.rs::tests::card_height_grows_with_body_lines` | Layout math: card height grows per line and empty bodies take one line. |
| R | `comments.rs::tests::long_lines_are_charged_for_their_soft_wraps` | Layout math: soft-wrap charging and the 8-line cap of `card_body_lines`. |
| R | `composer.rs::tests::delayed_high_revision_load_merges_typing_and_saves_above_both` | Composer draft durability via the in-memory RPC (`DraftTestRpc`); the fake stores rows, the composer's merge/revision/settlement logic is under test. Regression: typing during a slow draft read is lost or saved below the stored revision. |
| R | `composer.rs::tests::canvas_picker_repaint_during_load_does_not_create_a_conflict` | Composer draft durability via the in-memory RPC (`DraftTestRpc`); the fake stores rows, the composer's merge/revision/settlement logic is under test. Regression: picker repaint during load bumps the revision and creates a spurious conflict save. |
| R | `composer.rs::tests::engine_generation_change_discards_an_old_profile_load` | Composer draft durability via the in-memory RPC (`DraftTestRpc`); the fake stores rows, the composer's merge/revision/settlement logic is under test. Regression: an old profile's draft read leaks into the new engine/profile. |
| R | `composer.rs::tests::close_flushes_dirty_drafts_from_both_sides_of_navigation` | Composer draft durability via the in-memory RPC (`DraftTestRpc`); the fake stores rows, the composer's merge/revision/settlement logic is under test. Regression: quitting loses the draft of the chat navigated away from. |
| R | `composer.rs::tests::applied_task_send_clears_pending_save_without_reusing_revision` | Composer draft durability via the in-memory RPC (`DraftTestRpc`); the fake stores rows, the composer's merge/revision/settlement logic is under test. Regression: an applied send resurrects as a draft or reuses a revision. |
| R | `composer.rs::tests::applied_task_send_preserves_newer_typing` | Composer draft durability via the in-memory RPC (`DraftTestRpc`); the fake stores rows, the composer's merge/revision/settlement logic is under test. Regression: settlement of a sent message wipes text typed after send. |
| R | `composer.rs::tests::rejected_task_send_restores_and_saves_its_draft` | Composer draft durability via the in-memory RPC (`DraftTestRpc`); the fake stores rows, the composer's merge/revision/settlement logic is under test. Regression: a rejected send loses the prompt; submission must become Recoverable. |
| R | `composer.rs::tests::pending_task_send_closes_with_durable_snapshot_and_keeps_new_canvas_draft` | Composer draft durability via the in-memory RPC (`DraftTestRpc`); the fake stores rows, the composer's merge/revision/settlement logic is under test. Regression: pending delivery blocks close, resends, or returns the sent text as a draft. |
| R | `composer.rs::tests::first_send_does_not_flag_its_own_pending_delivery` | Composer draft durability via the in-memory RPC (`DraftTestRpc`); the fake stores rows, the composer's merge/revision/settlement logic is under test. 2026-10-06 report: first send showed a spurious delivery warning; restarted composer must still warn. |
| R | `composer.rs::tests::restarted_pending_delivery_with_newer_typing_still_warns` | Composer draft durability via the in-memory RPC (`DraftTestRpc`); the fake stores rows, the composer's merge/revision/settlement logic is under test. Regression: restart merges the submitted snapshot into newer typing or drops the warning. |
| R | `composer.rs::tests::unknown_task_send_closes_and_later_rejection_remains_recoverable` | Composer draft durability via the in-memory RPC (`DraftTestRpc`); the fake stores rows, the composer's merge/revision/settlement logic is under test. Regression: lost reply makes the prompt unrecoverable or triggers a resend. |
| R | `composer.rs::tests::failed_snapshot_save_does_not_allow_pending_send_to_close` | Composer draft durability via the in-memory RPC (`DraftTestRpc`); the fake stores rows, the composer's merge/revision/settlement logic is under test. Regression: a send proceeds without a durable snapshot. |
| R | `composer.rs::tests::failed_save_keeps_orderly_close_blocked` | Composer draft durability via the in-memory RPC (`DraftTestRpc`); the fake stores rows, the composer's merge/revision/settlement logic is under test. Regression: close proceeds while the draft failed to save (data loss). |
| R | `composer.rs::tests::recovery_action_merges_older_text_and_attachment_into_newer_draft` | Composer draft durability via the in-memory RPC (`DraftTestRpc`); the fake stores rows, the composer's merge/revision/settlement logic is under test. Recovery merge of text+attachment and `recovered` settlement. |
| R | `composer.rs::tests::first_submit_canvas_rekey_shows_setup_recovery_action` | Composer draft durability via the in-memory RPC (`DraftTestRpc`); the fake stores rows, the composer's merge/revision/settlement logic is under test. Setup-failure recovery action surfaces on the re-keyed task without resend. |
| R | `composer.rs::tests::recoverable_setup_preserves_genuinely_newer_task_drafts` | Composer draft durability via the in-memory RPC (`DraftTestRpc`); the fake stores rows, the composer's merge/revision/settlement logic is under test. Table over text/attachment/context/revision: newer drafts are never overwritten by recovery. |
| R | `composer.rs::tests::dock_morph_lands_on_the_two_row_session_pill_with_a_continuous_editor_origin` | Rendered gpui window: editor origin stays fixed and height interpolates through the dock morph. Only geometry proof. |
| R | `composer.rs::tests::composer_padding_and_file_prompt_restore_focus` | Rendered focus contract: padding click focuses the editor; file prompt accept/cancel and drops restore focus and keep the draft. |
| R | `composer.rs::tests::composer_picker_escape_restores_focus_but_click_away_does_not` | Rendered focus contract for picker dismissal paths. |
| R | `composer.rs::tests::inputs_release_focus_on_click_away` | Rendered focus contract: click-away capture never steals focus acquired by the clicked input. |
| R | `composer.rs::tests::context_running_or_unknown_blocks_before_draft_capture` | Submit gate during context compaction keeps the draft and revision and shows the exact copy. |
| R | `composer.rs::tests::enter_on_empty_composer_during_a_live_run_never_interrupts` | Issue #406 regression at the real RPC client boundary: Enter on empty composer must not send an interrupt. |
| R | `composer.rs::tests::a_press_of_two_or_more_clicks_takes_the_whole_field_and_leaves_the_drag_disarmed` | Input press-intent table (multi-click select-all does not arm drag). |
| R | `composer.rs::tests::message_enter_bindings_cover_both_platform_modifiers` | Keymap contract: exact binding vectors for Enter/ModEnter on cmd and ctrl. Keeper for the opposite-modifier binding. |
| D | `composer.rs::tests::message_enter_adds_only_the_fixed_opposite_modifier_binding` | Subsumed. Detects: ModEnter+cmd contains `shift-cmd-enter` OppositeSubmit and no `ctrl-enter`/`alt-cmd-enter`. Remaining proof: `tests::message_enter_bindings_cover_both_platform_modifiers` asserts exact `Vec` equality for the same input (presence and absence both implied). Unlocks 11 test lines. |
| R | `composer.rs::tests::enter_accepts_a_completion_before_submit_or_newline` | Enter priority table (completion beats submit/newline). |
| R | `composer.rs::tests::numeric_answer_input_keeps_digit_keys_for_text` | Digit shortcuts only when the answer input is not focused; distinct from `Wizard::press_number`. |
| R | `composer.rs::tests::question_card_keeps_prompt_composer_independent_across_routes` | Question card vs prompt composer independence across navigation and transient transcript blips. |
| R | `composer.rs::tests::stable_outer_width_only_schedules_reflow_on_real_changes` | Reflow threshold (0.5px) prevents layout feedback loops. |
| R | `composer.rs::tests::mention_tooltip_wait_survives_pointer_jitter_and_promotes_once` | Tooltip state machine: stale timers never reveal; one activation keeps its generation. |
| R | `composer.rs::tests::mention_tooltip_changes_target_and_cancels_disappeared_target` | Tooltip state machine: retarget restarts the wait; vanished target hides. |
| R | `composer.rs::tests::mention_tooltip_stays_visible_over_chip_or_popup_only` | Hover containment predicate (chip or popup). |
| R | `composer.rs::tests::mention_wash_moves_wholly_to_the_next_visual_row_at_a_wrap` | Wrap segmentation of chip wash rows. |
| R | `composer.rs::tests::mention_token_requires_a_token_boundary_and_tracks_full_token` | `@` token detection table (emails and mid-word never open). |
| R | `composer.rs::tests::slash_token_only_opens_the_prompt` | Slash command token table (offset 0 only, absolute paths excluded). |
| R | `composer.rs::tests::dismissed_mentions_reject_stale_responses` | Async race guard: stale file-search responses are ignored. |
| R | `composer.rs::tests::file_mentions_serialize_to_strict_local_markdown` | Prompt-byte contract of `luna-file:` links (escaping, directory slash) and parse-back. |
| R | `composer.rs::tests::dropped_mentions_are_separated_from_surrounding_text` | Drop insertion bytes and cursor advance. |
| R | `composer.rs::tests::dropped_mentions_reject_paths_outside_the_workspace` | Security: absolute and parent paths never become mentions. |
| R | `composer.rs::tests::file_mentions_reject_external_or_noncanonical_markdown` | Security: only canonical `luna-file:` links parse as mentions (table). |
| R | `composer.rs::tests::duplicate_mention_basenames_use_unique_suffixes` | Disambiguation of duplicate basenames in chips. |
| R | `composer.rs::tests::mention_suffixes_compare_path_components` | Negative control for suffix disambiguation (component-wise, not substring). |
| R | `composer.rs::tests::projection_maps_and_expands_atomic_chip_ranges` | Display/raw offset mapping for atomic chips (caret and selection). |
| R | `composer.rs::tests::sent_mention_display_projects_chips_for_the_transcript` | Owner of `sent_mention_display` incl. directory mentions; `transcript::tests::user_rows_project_file_mentions_into_chips` covers one file mention at the row boundary. |
| R | `composer.rs::tests::sent_mention_display_leaves_plain_prompts_untouched` | Fast path and hostile-path rejection for sent prompts. |
| R | `composer.rs::tests::luna_approval_marker_chooses_only_the_direct_single_question_route` | Approval routing contract: only a lone LunaCrossTask question takes the direct route; decision mapping. |
| R | `composer.rs::tests::caret_blink_phase` | Caret blink timing contract. |
| R | `composer.rs::tests::auto_grow_math` | Composer height math (floor 124, cap 308, linear growth). Constant asserts are redundant but the formula asserts are the only proof. |
| R | `composer.rs::tests::appshot_strip_height_tracks_cards` | Strip height layout math. |
| R | `composer.rs::tests::appshot_images_share_height_and_adapt_width_without_losing_aspect_ratio` | Appshot contain-fit table (aspect ratio, caps, fallback). |
| R | `composer.rs::tests::input_wheel_scroll_uses_gpui_direction_and_clamps` | Wheel direction and clamping table. |
| R | `composer.rs::tests::input_scroll_reveals_only_when_caret_leaves_viewport` | Caret-follow minimal scroll table. |
| R | `composer.rs::tests::input_drag_autoscroll_is_edge_proportional_and_capped` | Drag autoscroll table. |
| R | `composer.rs::tests::flip_morph_starts_once_per_committed_flip` | Morph state machine: one morph per committed flip, cleared after. |
| R | `composer.rs::tests::resolved_layout_does_not_keep_notifying_on_repaint` | Linux headless render: unchanged draws must not re-publish ViewportChanged (layout loop). |
| R | `composer.rs::tests::single_line_address_reveals_caret_and_maps_scrolled_pointer` | Linux headless: single-line input horizontal scroll, caret reveal, newline stripping. |
| R | `composer.rs::tests::layout_cache_reuses_resize_frames_and_invalidates_text_inputs` | Linux headless shaping cache contract; uses the `#[cfg(test)] layout_rebuilds` counter (retained seam, see follow-ups). |
| R | `composer.rs::tests::resize_reveals_only_complete_rows` | Resize clip math (whole rows only). |
| R | `composer.rs::tests::resize_keeps_text_anchored_to_the_input_origin` | Caret-follow during resize table. |
| R | `composer.rs::tests::scroll_fade_ignores_temporary_resize_overflow` | Fade edges ignore transient resize overflow. |
| R | `composer.rs::tests::scroll_fade_tracks_real_overflow_edges` | Fade edges track real overflow. |
| R | `composer.rs::tests::content_resize_retargets_from_visible_height_and_settles` | Morph retarget from on-screen height; reduced motion cancels. |
| R | `composer.rs::tests::flip_morph_height_ramps_monotonically_to_target` | Morph curve monotonic and exact landing. |
| R | `composer.rs::tests::flip_morph_reverse_hands_off_from_current_height` | Reversal continuity. |
| R | `composer.rs::tests::flip_morph_snaps_for_reduced_motion_and_first_paint` | Reduced motion and first paint never morph. |
| R | `composer.rs::tests::route_change_never_arms_the_morph` | Route changes snap and kill in-flight morphs. |
| R | `composer.rs::tests::composer_surface_radius_tracks_the_dock_morph` | Radius endpoints of the dock morph; restates a linear formula but is the only proof. |
| R | `composer.rs::tests::flip_morph_tracks_live_target_and_drives_fade` | Live target tracking and fade progress. |
| R | `composer.rs::tests::new_thread_route_changes_use_the_coordinated_timeline` | New-thread transition spec and endpoints. |
| R | `composer.rs::tests::route_chrome_crossfade_never_duplicates_picker_controls` | Crossfade invariant: never both chromes visible. |
| R | `composer.rs::tests::staged_comments_alone_are_content` | `composer_has_content` table (text, attachments, comments). Keeper for comment-only content. |
| D | `composer.rs::tests::a_comment_only_stage_queues_during_a_live_run` | Composes two pure functions in the test itself. Detects nothing beyond its parts. Remaining proof: `tests::staged_comments_alone_are_content` (`composer_has_content("", 0, 1)` is content) and `tests::send_button_morph` (`send_button_mode(true, true) == Queue`, `(true, false) == Stop`); production composition is exercised by `enter_on_empty_composer_during_a_live_run_never_interrupts`. Unlocks 15 test lines. |
| R | `composer.rs::tests::send_button_morph` | Full truth table of `send_button_mode`. |
| R | `composer.rs::tests::queued_submit_does_not_publish_an_optimistic_transcript_echo` | Only guard of the queue-row no-echo policy; weak (restates `!queue`), see follow-ups for a boundary replacement. |
| R | `composer.rs::tests::failed_submission_never_overwrites_newer_typing` | Newer-draft detection table used on failed sends. |
| R | `composer.rs::tests::interrupt_tracking_is_idempotent_per_chat` | Interrupt de-dup per chat; thin wrapper over HashSet but the only proof. |
| R | `composer.rs::tests::interrupt_tracking_releases_only_settled_chats` | Interrupt release for settled chats. |
| R | `composer.rs::tests::interrupt_payload_targets_the_active_turn_command` | Wire contract of the interrupt intent payload. |
| R | `composer.rs::tests::escape_consumers_keep_completion_and_wizard_priority` | Escape priority table. |
| R | `composer.rs::tests::wizard_single_select_auto_advances_and_completes` | Wizard flow (auto-advance, answers). |
| R | `composer.rs::tests::async_question_selection_waits_for_send_and_uses_index_ids` | Async question ids are indices; selection does not auto-send. |
| R | `composer.rs::tests::async_question_command_id_is_stable_and_bounded` | Idempotency key: stable, bounded, distinct per chat/request/amendment. |
| R | `composer.rs::tests::async_question_stays_visible_after_later_assistant_text_until_resolved` | Pending async question lookup survives later assistant entries. |
| R | `composer.rs::tests::unattended_answer_hold_requires_applied_receipt` | Receipt-driven hold: only Applied clears the countdown; retry clears its own error. |
| R | `composer.rs::tests::async_question_draft_survives_navigation_and_amendment` | Async answer draft persistence across routes. |
| R | `composer.rs::tests::pending_skip_readback_clears_only_its_own_banner` | Answer-later sends nothing and keeps the prompt; failure banners clear per key. Name is stale (see follow-ups). |
| R | `composer.rs::tests::uncertain_async_answer_retries_exact_command_without_touching_prompt` | Idempotent retry of the exact command envelope after lost replies. |
| R | `composer.rs::tests::wizard_multi_select_toggles_and_stays` | Multi-select toggling. |
| R | `composer.rs::tests::wizard_number_keys_and_bounds` | Number keys and bounds on `Wizard`. |
| R | `composer.rs::tests::wizard_typed_answer_overrides_and_back_pages` | Typed answer overrides and back paging. |
| R | `composer.rs::tests::pending_input_detection` | Pending input lookup incl. aborted entries and steer echoes (user forensics regressions). |
| R | `composer.rs::appshot_rebase_tests::appshot_only_draft_counts_as_content_and_removal_clears_it` | Appshot staging counts as content; removal clears it (entity-level). |
| R | `composer.rs::appshot_rebase_tests::failed_send_restores_complete_appshots_without_duplicates` | Failed send restores full appshots without duplicating fresh ones. |
| R | `composer.rs::appshot_rebase_tests::remote_queue_removal_recovers_both_appshot_drafts` | Remote queue removal merges both appshot drafts and text. |
| R | `composer_dock.rs::tests::panel_handoff_hides_background_during_geometry_switch_in_both_sidebar_states` | Dock choreography: dissolve stays opaque through the horizontal switch in both sidebar states. |
| R | `composer_dock.rs::tests::panel_exit_retains_source_transcript_width_only_until_handoff_ends` | Exit-flash regression: transcript keeps the source column width only during the handoff. |
| R | `composer_dock.rs::tests::panel_return_sizes_while_hidden_and_finishes_controls_with_input` | Panel return choreography timing. |
| R | `composer_dock.rs::tests::measured_dock_retargets_without_a_first_frame_jump` | Rendered gpui: prepaint glide starts from painted bounds on idle click, reversal and resize. |
| R | `composer_dock.rs::tests::idle_time_is_not_consumed_by_a_new_target` | Phase/visual state (not prepaint position): idle time is not animation time; reversal keeps state. |
| R | `composer_dock.rs::tests::choreography_is_direction_specific_and_selectors_never_duplicate` | Direction-specific schedules; selectors and footer never both visible. |
| R | `composer_dock.rs::tests::reversal_preserves_position_and_velocity` | Critically damped glide keeps position/velocity on retarget. |
| R | `composer_dock.rs::tests::normal_dock_is_monotone_and_frame_rate_independent` | Glide monotone and frame-rate independent. |
| R | `composer_dock.rs::tests::initial_and_reduced_motion_frames_snap` | First frame and reduced motion snap. |
| R | `composer_dock/panel_handoff.rs::tests::geometry_switch_is_hidden_in_both_directions` | PanelHandoff opacity: fully hidden across the switch window, both directions. |
| R | `composer_dock/panel_handoff.rs::tests::reversal_preserves_opacity_and_reduced_motion_cancels` | Reversal continuity and reduced-motion cancel. |
| R | `composer_dock/panel_handoff.rs::tests::ordinary_resizing_and_same_column_navigation_do_not_fade` | Negative contract: resizes and same-column navigation never fade. |
| R | `context_usage.rs::tests::missing_usage_is_distinct_from_zero_and_overflow` | User-visible context card copy: unknown vs zero vs overflow vs unknown window. |
| R | `context_usage.rs::tests::token_counts_are_grouped_by_thousands` | Digit grouping and exact detail copy. |
| R | `edge_fade.rs::tests::painted_label_retains_scroll_fade_across_overflow_transitions` | Rendered gpui paint-scope contract: nested label fades inherit and never leak. |
| R | `edge_fade.rs::tests::label_fade_enters_continuously_and_settles_at_one_band` | Continuity of the label fade ramp (no 1px cutoff pop). |
| R | `history.rs::tests::history_search_matches_fuzzy_subject_terms_and_sha_prefix` | Replays engine `luna_engine::repos::git_history_matches` (fuzzy subject, case-insensitive SHA prefix, negative). Engine only tests the Unicode case; retained as the only proof and filed as a cross-lane move. |
| R | `history.rs::tests::history_search_keeps_unicode_engine_result_visible` | UI must not drop engine search results in `recompute_view`. |
| R | `history.rs::tests::history_search_focus_survives_the_composer_fallback` | TestApp focus regression: search input keeps focus despite the composer focus fallback; idle dismiss does not collapse a non-empty query. |
| R | `history.rs::tests::search_compaction_connects_matches_across_hidden_commits` | History graph/layout pure function with a real input space; only proof. |
| R | `history.rs::tests::search_compaction_handles_a_twenty_thousand_commit_gap` | Stack safety of the UI copy of compaction (engine has its own copy and test; duplicate production code filed as follow-up). |
| R | `history.rs::tests::collapsing_a_branch_contracts_linear_parents_and_keeps_junctions` | History graph/layout pure function with a real input space; only proof. |
| R | `history.rs::tests::transition_rows_fold_old_commits_beside_their_stable_anchor` | History graph/layout pure function with a real input space; only proof. |
| R | `history.rs::tests::transition_rows_expand_new_commits_in_their_final_order` | History graph/layout pure function with a real input space; only proof. |
| R | `history.rs::tests::list_splice_preserves_the_unchanged_prefix_around_a_fold` | History graph/layout pure function with a real input space; only proof. |
| R | `history.rs::tests::removed_scroll_anchor_moves_to_the_next_surviving_commit` | History graph/layout pure function with a real input space; only proof. |
| R | `history.rs::tests::branch_fold_keys_keep_local_and_remote_identity` | Fold key format distinguishes local and remote branches. |
| R | `history.rs::tests::author_avatar_fallback_uses_the_first_visible_initial` | Avatar fallback copy. |
| R | `history.rs::tests::github_avatar_payload_decodes_into_a_gpui_image` | Avatar payload decode and rejection. |
| R | `history.rs::tests::graph_splits_and_rejoins_merge_lanes` | History graph/layout pure function with a real input space; only proof. |
| R | `history.rs::tests::appending_older_commits_preserves_the_loaded_prefix_layout` | Paging stability: loaded prefix layout unchanged by older pages. |
| R | `history.rs::tests::graph_palette_only_reduces_saturation` | Design contract: graph palette keeps hue/lightness/alpha. Weak (restates formula) but the only proof. |
| R | `history.rs::tests::graph_hover_detects_vertical_and_curved_paths` | History graph/layout pure function with a real input space; only proof. |
| R | `history.rs::tests::graph_hover_prefers_the_node_and_ignores_empty_space` | History graph/layout pure function with a real input space; only proof. |
| R | `history.rs::tests::responsive_graph_keeps_natural_spacing_when_it_fits` | History graph/layout pure function with a real input space; only proof. |
| R | `history.rs::tests::responsive_graph_compresses_lanes_to_preserve_commit_space` | History graph/layout pure function with a real input space; only proof. |
| R | `history.rs::tests::narrow_commit_space_switches_to_a_compact_rail_with_hysteresis` | History graph/layout pure function with a real input space; only proof. |
| R | `history.rs::tests::compact_graph_keeps_each_rows_color_as_its_hover_identity` | History graph/layout pure function with a real input space; only proof. |
| R | `history.rs::tests::graph_geometry_morph_converges_lanes_before_entering_the_compact_rail` | History graph/layout pure function with a real input space; only proof. |
| R | `history.rs::tests::graph_geometry_morph_expands_the_rail_from_its_compact_start` | History graph/layout pure function with a real input space; only proof. |
| R | `history.rs::tests::compressed_graph_hit_testing_uses_the_fitted_lane_positions` | History graph/layout pure function with a real input space; only proof. |
| R | `history.rs::tests::responsive_graph_geometry_ignores_sub_step_resize_jitter` | History graph/layout pure function with a real input space; only proof. |
| R | `history.rs::tests::responsive_graph_lanes_snap_to_device_pixels` | History graph/layout pure function with a real input space; only proof. |
| R | `history.rs::tests::ref_badges_expand_with_the_available_width` | Ref badge fitting table. |
| R | `history.rs::tests::ref_badges_preserve_overflow_when_the_first_badge_is_too_wide` | Ref badge overflow edge. |
| R | `history.rs::tests::ref_area_preserves_subject_space_and_caps_at_forty_five_percent` | Ref area width formula. |
| R | `history.rs::tests::commit_divider_resizes_the_first_visible_fixed_column` | Column resize math. |
| R | `history.rs::tests::interior_column_divider_preserves_width_and_clamps_both_sides` | Column resize math with clamps. |
| R | `history.rs::tests::visible_columns_follow_persisted_order_and_skip_hidden_entries` | Persisted column order/visibility contract. |
| R | `history.rs::tests::reordering_visible_columns_preserves_hidden_columns` | Persisted reorder keeps hidden columns. |
| R | `history.rs::tests::reorder_drop_index_respects_uneven_column_widths` | Drop index math. |
| R | `history.rs::tests::ref_tooltip_describes_each_reference_kind` | User-visible tooltip copy per ref kind. |
| R | `image_media.rs::tests::svg_text_survives_bundled_mono_font_selection` | Font fallback regression: SVG text rasterizes with bundled mono. |
| R | `image_media.rs::tests::svg_text_survives_an_unavailable_font` | Font fallback regression: missing/virtual families still render text. |
| C | `image_media.rs::tests::svg_is_bounded_and_external_resources_are_removed` | Duplicate of stronger owners. Detects: `file://` image href stripped; 99999px SVG decodes; garbage PNG rejected. Absorbed by `tests::svg_scripts_html_and_embedded_or_external_images_never_reach_gpui` (file/network/data image hrefs and `<image` stripped), `tests::large_svgs_keep_the_complete_viewbox_at_bounded_resolutions` (40000px SVGs decode bounded); the garbage-PNG row was carried into `tests::encoded_input_and_decode_dimensions_are_bounded`. Unlocks 14 test lines. |
| R | `image_media.rs::tests::large_svgs_keep_the_complete_viewbox_at_bounded_resolutions` | Raster budget, full viewBox, cache reuse and fallback for huge SVGs (pixel probes). |
| R | `image_media.rs::tests::extreme_aspect_ratios_stay_inside_the_pixel_budget` | `raster_size` budget at extreme aspect ratios. |
| R | `image_media.rs::tests::encoded_input_and_decode_dimensions_are_bounded` | Keeper: encoded byte cap, 4096px dimension cap, and now undecodable bytes rejected (row carried from the consolidated test). |
| R | `image_media.rs::tests::svg_scripts_html_and_embedded_or_external_images_never_reach_gpui` | Security: sanitized SVG drops scripts, foreignObject, network/file/data images. |
| R | `image_media.rs::tests::animated_images_are_flattened_to_one_bounded_frame` | Animated input flattened to one static PNG frame (workspace preview path). |
| R | `image_viewer.rs::tests::fit_preserves_aspect_ratio_and_never_upscales` | Viewer geometry math. |
| R | `image_viewer.rs::tests::zoom_keeps_the_cursor_over_the_same_image_point` | Anchored zoom invariant. |
| R | `image_viewer.rs::tests::pan_zoom_and_resize_stay_bounded` | Scale/pan clamps incl. NaN. |
| R | `image_viewer.rs::tests::wheel_requires_control_and_normalizes_lines` | Wheel without control passes through; line normalization and reversal (rendered test covers only control+pixel zoom-in). |
| R | `image_viewer.rs::tests::pinch_accumulates_native_deltas_and_a_drag_does_not_click` | Pinch accumulation and the 2px drag threshold (rendered test covers the 50px drag only). |
| R | `image_viewer.rs::tests::rendered_lightbox_consumes_zoom_and_drag_but_allows_click_and_escape` | Linux headless lightbox boundary: zoom/drag consumed, cached siblings untouched, click/Escape close. Keeper; runs on the ubuntu CI job. |
| R | `links.rs::tests::luna_link_round_trips_reserved_characters` | Deep-link codec round trip (reserved characters). |
| R | `links.rs::tests::malformed_or_foreign_links_are_rejected` | Deep-link parser rejects foreign/malformed input. |
| R | `links.rs::tests::local_workspace_locator_waits_for_device_identity` | Privacy contract: locator needs device identity, differs per device. |
| R | `links.rs::tests::synced_workspace_locator_waits_for_signed_in_identity` | Locator requires a signed-in org identity. |
| R | `links.rs::tests::codex_link_is_exact_and_unverified_harnesses_are_omitted` | Exact Codex URL bytes; unverified harnesses omitted. |
| D | `loaders.rs::tests::mark_stagger_follows_flight_axis` | Crate-local replay of a shared helper. Detects: `mark_cell_stagger` ordering and range. Remaining proof: `luna_proto::motion::tests::the_mark_sweeps_from_tail_to_head` (strict superset: same tail/head, exact tail value, per-cell range and phase). Unlocks the whole test module and the test-only `MARK_SPREAD` re-export (`rg` shows no other user). |
| R | `markdown/link_destination.rs::tests::disclosure_fits_small_viewports_and_keeps_the_complete_destination` | Viewport limits and lossless breakable URL presentation. |
| R | `markdown/link_interaction.rs::tests::selection_drags_and_secondary_clicks_do_not_navigate` | Activation predicate: drags, right clicks and double clicks never navigate. |
| R | `markdown/link_interaction.rs::tests::all_actions_keep_the_destination` | Copy copies the original destination; handlers receive target/action/session unchanged. |
| R | `markdown/link_interaction.rs::rendered_tests::escape_dismisses_focused_destination` | Linux headless rendered boundary (CI ubuntu job) through `render_tree`; uses the `#[cfg(test)]` `TOOLTIP_BOUNDS`/`selection_test_snapshot` hooks (retained seams). Escape dismisses the destination card. |
| R | `markdown/link_interaction.rs::rendered_tests::focused_destination_does_not_block_other_controls` | Linux headless rendered boundary (CI ubuntu job) through `render_tree`; uses the `#[cfg(test)]` `TOOLTIP_BOUNDS`/`selection_test_snapshot` hooks (retained seams). Destination card never swallows other controls' clicks. |
| R | `markdown/link_interaction.rs::rendered_tests::context_menu_cancels_visible_and_pending_hover_tooltips` | Linux headless rendered boundary (CI ubuntu job) through `render_tree`; uses the `#[cfg(test)]` `TOOLTIP_BOUNDS`/`selection_test_snapshot` hooks (retained seams). Menu hides visible and pending tooltips; menu row toggles the default destination. |
| R | `markdown/link_interaction.rs::rendered_tests::keyboard_visits_each_range_and_opens_the_link_menu` | Linux headless rendered boundary (CI ubuntu job) through `render_tree`; uses the `#[cfg(test)]` `TOOLTIP_BOUNDS`/`selection_test_snapshot` hooks (retained seams). Keyboard activation per range and menu External action. |
| R | `markdown/link_interaction.rs::rendered_tests::rendered_truncation_resizes_and_selects_the_original_url` | Linux headless rendered boundary (CI ubuntu job) through `render_tree`; uses the `#[cfg(test)]` `TOOLTIP_BOUNDS`/`selection_test_snapshot` hooks (retained seams). Truncation re-measures on resize, selection copies the original URL, tables too. |
| R | `markdown/link_presentation.rs::tests::truncation_preserves_graphemes_destinations_and_original_selection` | Offset-map math: grapheme-safe truncation keeps destination and original offsets. |
| R | `markdown/link_presentation.rs::tests::partial_and_cross_block_copies_map_back_to_source` | Offset-map math for partial and cross-block copies. |
| R | `markdown/link_presentation.rs::tests::widening_and_streaming_recompute_presentation_without_stale_offsets` | Recompute on widen/stream without stale offsets. |
| R | `markdown/link_presentation.rs::tests::styled_link_runs_are_trimmed_without_corrupting_offsets` | Run trimming keeps style and exact cover. |
| R | `markdown/links.rs::tests::internal_external_and_unsupported_platform_routes` | Routing decision table for Internal/External/Primary. |
| R | `markdown/links.rs::tests::rejected_targets_never_fall_back` | Security: unsafe/malformed URLs are Rejected in both embedded states. Only proof of `browser::model::transcript_address` rejection set. |
| R | `markdown/links.rs::tests::labels_and_original_destinations_survive_normalization` | Label/original preserved, navigation normalized. |
| D | `markdown/links.rs::tests::markdown_and_autolinks_share_validation` | Name promises more than the input exercises: the test calls `LinkTarget::new` itself, so it cannot detect a renderer that skips validation. Remaining proof: `parser::tests::bare_urls_autolink` and `parser::tests::autolink_leaves_non_urls_alone` (link runs), `links::tests::internal_external_and_unsupported_platform_routes` (https navigation Ok), rendered `link_interaction::rendered_tests::keyboard_visits_each_range_and_opens_the_link_menu` (parsed links reach activation). Unlocks 15 test lines. |
| R | `markdown/mend.rs::tests::balanced_text_needs_nothing` | Pure mend function table (one category per test); single owner of `close_hanging`'s input space. Parser display-tree tests cover the integration. |
| R | `markdown/mend.rs::tests::bold_and_italic_close` | Pure mend function table (one category per test); single owner of `close_hanging`'s input space. Parser display-tree tests cover the integration. |
| R | `markdown/mend.rs::tests::half_streamed_closers_complete` | Pure mend function table (one category per test); single owner of `close_hanging`'s input space. Parser display-tree tests cover the integration. |
| R | `markdown/mend.rs::tests::nested_closers_come_innermost_first` | Pure mend function table (one category per test); single owner of `close_hanging`'s input space. Parser display-tree tests cover the integration. |
| R | `markdown/mend.rs::tests::bare_openers_stay_literal_until_content` | Pure mend function table (one category per test); single owner of `close_hanging`'s input space. Parser display-tree tests cover the integration. |
| R | `markdown/mend.rs::tests::closers_insert_before_trailing_whitespace` | Pure mend function table (one category per test); single owner of `close_hanging`'s input space. Parser display-tree tests cover the integration. |
| R | `markdown/mend.rs::tests::intraword_and_escapes_are_literal` | Pure mend function table (one category per test); single owner of `close_hanging`'s input space. Parser display-tree tests cover the integration. |
| R | `markdown/mend.rs::tests::list_markers_are_not_openers` | Pure mend function table (one category per test); single owner of `close_hanging`'s input space. Parser display-tree tests cover the integration. |
| R | `markdown/mend.rs::tests::strikethrough_closes` | Pure mend function table (one category per test); single owner of `close_hanging`'s input space. Parser display-tree tests cover the integration. |
| R | `markdown/mend.rs::tests::inline_code_closes_and_shields_markers` | Pure mend function table (one category per test); single owner of `close_hanging`'s input space. Parser display-tree tests cover the integration. |
| R | `markdown/mend.rs::tests::links_mend_to_pending_sentinel` | Pure mend function table (one category per test); single owner of `close_hanging`'s input space. Parser display-tree tests cover the integration. |
| R | `markdown/mend.rs::tests::link_urls_allow_nested_parens` | Pure mend function table (one category per test); single owner of `close_hanging`'s input space. Parser display-tree tests cover the integration. |
| R | `markdown/mend.rs::tests::emphasis_inside_link_text_closes_inside` | Pure mend function table (one category per test); single owner of `close_hanging`'s input space. Parser display-tree tests cover the integration. |
| R | `markdown/mend.rs::tests::emphasis_unclosed_in_completed_bracket_is_dropped` | Pure mend function table (one category per test); single owner of `close_hanging`'s input space. Parser display-tree tests cover the integration. |
| R | `markdown/mend.rs::tests::setext_partials_get_zero_width_space` | Pure mend function table (one category per test); single owner of `close_hanging`'s input space. Parser display-tree tests cover the integration. |
| R | `markdown/mermaid.rs::tests::corpus_renders_through_gpui_in_both_themes` | E2E corpus (scripts/fixtures/markdown-preview/*.mmd) through mermaid and GPUI rasterizer in four themes; deterministic output; optional artifacts via LUNA_MERMAID_ARTIFACTS. |
| R | `markdown/mermaid.rs::tests::malformed_and_oversized_diagrams_are_recoverable` | Error recovery and size bound. |
| R | `markdown/parser.rs::tests::task_ranges_survive_nested_lists_unicode_crlf_and_streaming` | Task marker source ranges (unicode, CRLF, nested, quoted, fenced) and char-by-char streaming parity. |
| R | `markdown/parser.rs::tests::display_snapshots_share_stable_blocks_without_mutating_old_frames` | Display snapshots share Arc blocks and are immutable across append/reset. |
| R | `markdown/parser.rs::tests::incremental_matches_full_on_streamed_corpora` | Keeper: incremental/full parity over CORPORA at chunk sizes 1..64. Now also streams the closing-quote STORY (absorbs two consolidated regressions). |
| R | `markdown/parser.rs::tests::appends_keep_committed_blocks_identical` | Streaming stability: committed blocks never change across appends (over CORPORA, now incl. STORY). |
| R | `markdown/parser.rs::tests::incremental_matches_full_with_link_definitions` | Link-definition full-reparse fallback keeps parity. |
| R | `markdown/parser.rs::tests::set_text_appends_or_resets` | `set_text` append vs reset branches. |
| R | `markdown/parser.rs::tests::block_structure_basics` | Heading/paragraph/code block model and nested emphasis runs. |
| R | `markdown/parser.rs::tests::nested_lists_and_tight_items` | List model with implicit paragraphs. |
| R | `markdown/parser.rs::tests::tables_parse_header_and_rows` | Table model and default alignment. |
| R | `markdown/parser.rs::tests::tables_parse_column_alignment` | Explicit column alignment. |
| D | `markdown/parser.rs::tests::links_carry_urls` | Duplicate. Detects: a markdown link run carries its text and destination. Remaining proof: `tests::autolink_leaves_non_urls_alone` asserts `[https://shown.dev](https://real.dev)` yields text `https://shown.dev` with destination `https://real.dev`; `image_model_tests::images_preserve_alt_title_position_links_and_empty_alt` covers linked images. Unlocks 13 test lines. |
| R | `markdown/parser.rs::tests::bare_urls_autolink` | GFM autolink extension (punctuation, parens, emphasis). |
| R | `markdown/parser.rs::tests::autolink_leaves_non_urls_alone` | Autolink negatives; keeper for markdown link text/destination. |
| R | `markdown/parser.rs::tests::top_level_ranges_are_stable_anchors` | Top-level ranges are ordered source anchors. |
| R | `markdown/parser.rs::tests::display_tree_styles_hanging_bold_immediately` | Display tree mends while canonical stays literal. |
| R | `markdown/parser.rs::tests::display_tree_converges_to_canonical_when_balanced` | Balanced text: display == canonical == full parse. |
| R | `markdown/parser.rs::tests::display_tree_never_leaks_streaming_urls` | Security/UX: streaming URLs never render; pending sentinel destination. |
| R | `markdown/parser.rs::tests::display_tree_leaves_code_blocks_alone` | Code blocks are never mended. |
| R | `markdown/parser.rs::tests::display_tree_suppresses_setext_flicker` | Setext flicker suppression at the parser boundary. |
| R | `markdown/parser.rs::tests::display_tree_prefix_matches_canonical_across_streams` | Display prefix identical to canonical across streams (render caches). |
| R | `markdown/parser.rs::tests::empty_and_whitespace_sources` | Empty inputs produce empty trees. |
| R | `markdown/parser.rs::closing_quote_blocks::full_parse_keeps_trailing_quote_in_block` | Absolute anchor for the closing-quote report text (two blocks, quote kept). STORY made `pub(super)` so CORPORA can stream it. |
| C | `markdown/parser.rs::closing_quote_blocks::streamed_boundary_at_quote_adds_no_block` | Consolidated into `tests::incremental_matches_full_on_streamed_corpora`: STORY added to CORPORA; chunk size 1 streams through the exact `powerless.`\|`"` boundary and asserts full-tree equality with `parse_full` (stronger than block count + range end); absolute shape kept by `closing_quote_blocks::full_parse_keeps_trailing_quote_in_block`. Diagnostic from 005f35af (the bug was in GPUI wrapping). Unlocks 14 test lines. |
| C | `markdown/parser.rs::closing_quote_blocks::streamed_small_chunks_match_full_parse` | Consolidated into `tests::incremental_matches_full_on_streamed_corpora` (STORY at chunk size 7 among others, full-tree equality instead of range-only). Unlocks 14 test lines. |
| R | `markdown/parser.rs::image_model_tests::images_preserve_alt_title_position_links_and_empty_alt` | Inline image model (alt, title, link, empty alt, position). |
| R | `markdown/render.rs::tests::code_block_lines_participate_in_text_selection` | Rendered gpui: code lines (incl. blank) join cross-element selection. |
| R | `markdown/render.rs::tests::the_default_code_font_size_reproduces_the_historical_code_block_size` | Default/config contract tying markdown code size to the shared code font default. |
| R | `markdown/render.rs::tests::code_block_indices_include_nested_quotes_and_lists` | Code block index assignment in nested structures. |
| R | `markdown/render.rs::tests::sole_workspace_link_gets_a_file_path` | File decoration resolution. |
| R | `markdown/render.rs::tests::direct_file_decoration_excludes_mixed_and_external_links` | File decoration negatives. |
| R | `markdown/render.rs::tests::standalone_recognized_filename_gets_a_decorative_identity` | Plain filename decoration. |
| R | `markdown/render.rs::tests::plain_file_decoration_rejects_prose_code_and_unknown_dotted_tokens` | Plain filename negatives. |
| R | `markdown/render.rs::tests::hard_break_file_list_resolves_every_recognized_line` | Hard-break file lists. |
| R | `markdown/render.rs::tests::range_starting_at_soft_wrap_includes_first_glyph` | Selection rect math at a soft wrap (GPUI affinity). |
| R | `markdown/render.rs::tests::range_crossing_soft_wrap_includes_first_continuation_glyph` | Selection rect math across a soft wrap. |
| D | `markdown/render.rs::tests::code_line_runs_cover_exactly` | Duplicate of the table keeper. Detects: `runs_for_syntax_line` exact cover, mono font, some colored run. Remaining proof: `tests::affected_language_roles_flow_through_markdown_paint_only` (same three assertions per kind over 4 languages x 2 themes); Rust span kinds owned by `crates/syntax/tests/quality.rs::rust_reference_span_snapshot`. Unlocks 21 test lines. |
| D | `markdown/render.rs::tests::tree_sitter_runs_are_rich_and_paint_only` | Duplicate of the table keeper. Detects: exact cover, mono font, keyword/macro/number colors for one Rust line. Remaining proof: `tests::affected_language_roles_flow_through_markdown_paint_only` (cover/font/`token_color(kind)` per kind); macro/number spans: `crates/syntax/tests/quality.rs::rust_reference_span_snapshot`; palette: `theme::tests::syntax_palette_resolves_every_kind_on_code_and_diff_backgrounds`. Unlocks 21 test lines. |
| R | `markdown/render.rs::tests::affected_language_roles_flow_through_markdown_paint_only` | Keeper: table over ts/tsx/kotlin/dockerfile x dark/light for span kinds and paint-only runs. |
| R | `markdown/render.rs::tests::code_line_runs_with_no_tokens_are_one_plain_run` | No-span edge: one plain run. |
| R | `markdown/render.rs::tests::flatten_collects_and_merges_inline_code_ranges` | Inline code wash ranges merge; tint without run background. |
| R | `markdown/render.rs::tests::code_palette_is_colored_and_shared` | Design contract: code palette is colored (not monochrome); theme test checks contrast only. |
| R | `markdown/render.rs::tests::flatten_runs_maps_links_and_styles` | Flattening maps links, monochrome underline, semibold. |
| R | `markdown/render.rs::tests::table_columns_floor_and_padding` | Table geometry math. |
| R | `markdown/render.rs::tests::table_columns_are_content_proportional_not_equal` | Table geometry proportionality. |
| R | `markdown/render.rs::tests::table_header_flattens_at_weight_700` | Header weight contract. |
| R | `markdown/render.rs::tests::adjacent_same_link_runs_merge_into_one_range` | Link range merging across style runs. |
| R | `markdown/render.rs::tests::viewport_cache_releases_offscreen_paint_data` | Memory: offscreen paint data released. |
| R | `markdown/render.rs::tests::style_generation_change_invalidates_cached_runs` | Cache invalidation on style generation change. |
| R | `markdown/selection.rs::tests::spans_within_one_element` | Pure selection resolver (direction-normalized). |
| R | `markdown/selection.rs::tests::spans_across_elements_cover_middles_whole` | Cross-element spans cover middles. |
| R | `markdown/selection.rs::tests::spans_across_empty_elements_preserve_blank_lines` | Blank code lines keep their newline in copies. |
| R | `markdown/selection.rs::tests::drag_lifecycle_and_copy_joins` | Global selection lifecycle and copy join. |
| R | `markdown/selection.rs::tests::drag_survives_forward_virtualization` | Forward drag survives the anchor scrolling out. |
| R | `markdown/selection.rs::tests::drag_survives_backward_virtualization` | Backward drag survives virtualization. |
| R | `markdown/selection.rs::tests::empty_click_clears_on_release` | Empty click clears. |
| R | `markdown/selection.rs::tests::double_click_span` | Double-click immediate span. |
| R | `markdown/selection.rs::tests::word_ranges` | Word range table incl. unicode snapping. |
| R | `markdown/veil.rs::tests::first_text_fades_and_settles_once` | Veil lifecycle: fade once, prune, never re-animate. |
| R | `markdown/veil.rs::tests::appended_chunks_fade_concurrently_and_independently` | Rolling chunk fades. |
| R | `markdown/veil.rs::tests::fade_constants_match_mugen_fade_painter` | Cross-implementation contract with mugen FadePainter (duration clamp, EMA, boost). Constant self-asserts are redundant but the function asserts are the only proof. |
| R | `markdown/veil.rs::tests::seeded_row_adopts_existing_text_without_fading` | Attach baseline: no full-reply fade on return; post-seed elements fade from empty. |
| D | `markdown/veil.rs::tests::default_row_veil_fades_first_text` | Duplicate. Detects: a default `RowVeil` fades its first chunk from 0. Remaining proof: `tests::row_veil_tracks_elements_independently` (default RowVeil is fading after first advance), `tests::seeded_row_adopts_existing_text_without_fading` (post-seed `(0..n, 0.0)` span shape), `tests::first_text_fades_and_settles_once` (ElemVeil first span `(0..5, 0.0)`). Unlocks 8 test lines. |
| R | `markdown/veil.rs::tests::faded_text_never_reanimates_on_append` | Only the appended suffix veils. |
| R | `markdown/veil.rs::tests::non_append_rewrite_keeps_prefix_and_reveils_tail` | Markdown resolution rewrite keeps prefix. |
| R | `markdown/veil.rs::tests::common_prefix_respects_char_boundaries` | UTF-8 safety. |
| R | `markdown/veil.rs::tests::veil_opacity_curve_endpoints` | Opacity curve contract. |
| R | `markdown/veil.rs::tests::apply_veil_preserves_length_and_fonts` | Paint-only run splitting keeps cover/fonts. |
| R | `markdown/veil.rs::tests::apply_veil_without_spans_is_identity` | Settled frames are byte-identical. |
| R | `markdown/veil.rs::tests::apply_veil_fades_decorations_too` | Decorations fade. |
| R | `markdown/veil.rs::tests::slice_spans_shifts_to_local_offsets` | Per-line span slicing. |
| R | `markdown/veil.rs::tests::row_veil_tracks_elements_independently` | Keeper for default RowVeil per-element fading. |
| R | `syntax_cache.rs::tests::key_depends_on_content_language_and_query_not_theme` | Cache key contract: content and language participate (wrong-language/stale spans). Name over-promises (query/theme are not varied). |
| F | `syntax_cache.rs::tests::cache_reuses_neutral_documents` | Kept the reuse contract (insert then `get` returns the same `Arc`); dropped assertions on `len()` and `stats().hits/misses`, which only test-only counters consumed. Unlocks removal of `#[cfg(test)] len/is_empty`, `stats()`, `SyntaxCacheStats` and the `hits`/`misses` fields (`rg`: no non-test caller in crates/ apps/ examples). |
| D | `syntax_cache.rs::tests::retained_bytes_measure_materialized_spans_not_source_text` | Negative control that passes for an unrelated reason: `retained_bytes > source.len()` (10 bytes) holds from `size_of::<HighlightedDocument>()` alone, so no change to span accounting can fail it. No remaining contract proof (byte-budget eviction is untested; follow-up). Unlocks `stats().retained_bytes` use. |
| R | `transcript.rs::tests::jump_button_stays_available_when_scrolling_down_until_near_bottom` | Jump button hysteresis. |
| R | `transcript.rs::tests::departing_transcript_is_retained_only_until_hidden` | Route-exit retention lifecycle. |
| R | `transcript.rs::tests::tool_groups_stay_closed_on_populated_chat_attach` | Tool-group reveal/disclosure regression through the real `receive_transcript_update` reducer and `Transcript::sync`. History never flashes open. |
| R | `transcript.rs::tests::tool_groups_stay_closed_after_rapid_new_chat_navigation` | Tool-group reveal/disclosure regression through the real `receive_transcript_update` reducer and `Transcript::sync`. Interrupted close tween never replays. |
| R | `transcript.rs::tests::tool_group_revisit_skips_batched_history_but_animates_live_arrivals` | Tool-group reveal/disclosure regression through the real `receive_transcript_update` reducer and `Transcript::sync`. Backfill batches do not animate; live arrivals do. |
| R | `transcript.rs::tests::tool_group_replay_cutoff_survives_coalesced_live_updates` | Tool-group reveal/disclosure regression through the real `receive_transcript_update` reducer and `Transcript::sync`. Coalesced live tail is not swallowed by replay. |
| R | `transcript.rs::tests::tool_group_interleaved_history_preserves_live_animation_epochs` | Tool-group reveal/disclosure regression through the real `receive_transcript_update` reducer and `Transcript::sync`. Interleaved history keeps live epochs. |
| R | `transcript.rs::tests::tool_group_first_live_arrival_after_empty_replay_animates` | Tool-group reveal/disclosure regression through the real `receive_transcript_update` reducer and `Transcript::sync`. Empty replay then live arrival animates. |
| R | `transcript.rs::tests::tool_group_live_reset_does_not_become_history` | Tool-group reveal/disclosure regression through the real `receive_transcript_update` reducer and `Transcript::sync`. A live Reset frame is not history. |
| R | `transcript.rs::tests::tool_group_navigation_keeps_user_pins_and_new_arrivals` | Tool-group reveal/disclosure regression through the real `receive_transcript_update` reducer and `Transcript::sync`. User pins survive navigation. |
| R | `transcript.rs::tests::resizing_details_does_not_restart_group_disclosure` | Disclosure progress independent of resize. |
| R | `transcript.rs::tests::connector_intersection_is_tessellated_only_once` | Path tessellation (overlap painted once). |
| R | `transcript.rs::tests::file_badge_icon_well_counter_shades_each_appearance` | Only proof of `file_icons::well_bg` contrast in both appearances (owner file_icons.rs has none; cross-lane move suggested). |
| R | `transcript.rs::tests::file_badge_icon_well_uses_more_coverage_on_frost` | Only proof of frosted `well_bg` coverage (cross-lane move suggested). |
| R | `transcript.rs::tests::selection_scroll_ramps_at_viewport_edges` | Selection autoscroll ramp. |
| R | `transcript.rs::tests::live_row_parse_work_is_bounded_per_commit` | Perf contract: per-commit parse work O(tail) through `parse_for_row`; handoff/cache outcomes. |
| R | `transcript.rs::tests::stationary_spring_does_not_keep_requesting_frames` | Keeper: idle spring requests no frames; growth glides monotonically without overshoot and lands exactly. |
| R | `transcript.rs::tests::estimated_height_growth_at_the_bottom_cannot_keep_spring_awake` | Regression: estimate growth kept the redraw loop alive. Edited: reads `spring.target_vel` field instead of the removed `#[cfg(test)]` accessor. |
| R | `transcript.rs::tests::spring_converges_to_a_fixed_target` | Cold-start convergence, exact landing, idle. |
| D | `transcript.rs::tests::spring_never_overshoots_or_oscillates` | Duplicate invocation. Detects: no overshoot, no backwards motion, exact landing. Remaining proof: `tests::stationary_spring_does_not_keep_requesting_frames` asserts `next >= pos && next <= target` every frame and `pos == target`; `tests::spring_converges_to_a_fixed_target` covers a cold start. Unlocks 16 test lines. |
| R | `transcript.rs::tests::spring_feed_forward_tracks_constant_growth` | Feed-forward glide tracks stream growth. Edited: field read replaces `target_vel()` accessor. |
| R | `transcript.rs::tests::spring_feed_forward_resets_when_target_shrinks` | Estimate resets on collapse. Edited: field read replaces accessor. |
| R | `transcript.rs::tests::spring_catchup_frames_glide_instead_of_teleporting` | Catch-up sub-stepping. |
| R | `transcript.rs::tests::restick_is_direction_aware` | Restick table. |
| R | `transcript.rs::tests::only_a_stream_at_the_bottom_gets_a_hard_end_anchor` | Hard end anchor only at the bottom while streaming. |
| R | `transcript.rs::tests::viewport_anchor_tracks_a_stable_row_across_replay` | Viewport/runway model regression (pure state). |
| R | `transcript.rs::tests::viewport_anchor_has_entry_and_index_fallbacks` | Viewport/runway model regression (pure state). |
| R | `transcript.rs::tests::optimistic_echo_cannot_consume_a_historical_viewport_before_replay` | Viewport/runway model regression (pure state). |
| R | `transcript.rs::tests::saved_viewport_preserves_and_releases_an_active_turn_runway` | Viewport/runway model regression (pure state). |
| R | `transcript.rs::tests::own_turn_waits_for_its_first_echo_then_retires_if_it_disappears` | Viewport/runway model regression (pure state). |
| R | `transcript.rs::tests::queued_turn_waits_for_its_bubble_without_replacing_the_live_turn` | Viewport/runway model regression (pure state). Edited: `pending.items.len()` replaces the removed `#[cfg(test)] len()`. |
| R | `transcript.rs::tests::newest_materialized_queue_turn_owns_a_batched_transcript_frame` | Viewport/runway model regression (pure state). Edited: `pending.items.len()` replaces the removed accessor. |
| R | `transcript.rs::tests::restored_viewport_discards_a_failed_optimistic_turn` | Viewport/runway model regression (pure state). |
| R | `transcript.rs::tests::pinned_viewports_follow_tail_and_the_cache_is_bounded` | Viewport/runway model regression (pure state). Cache bound. Edited: `cache.by_chat.len()` replaces the removed `#[cfg(test)] len()`. |
| R | `transcript.rs::tests::reopening_the_oldest_cached_chat_protects_it_from_the_next_eviction` | Viewport/runway model regression (pure state). LRU touch. |
| R | `transcript.rs::tests::viewport_finalization_waits_for_current_generation_and_stable_layout` | Viewport/runway model regression (pure state). |
| R | `transcript.rs::tests::generated_image_owner_candidates_and_same_length_corrections` | Generated image owner order and row versioning over the shared fixture `tests/fixtures/generated-images.json`. |
| R | `transcript.rs::tests::generated_image_click_opens_lightbox_and_escape_restores_focus` | Rendered gpui: click opens the shared lightbox, Escape restores focus. |
| R | `transcript.rs::tests::generated_image_fixture_uses_cache_and_retries` | Transcript attachment_state through the real attachment cache: loaded/loading/error, no fetch on repaint, protection keys. |
| R | `transcript.rs::tests::generated_image_builds_one_row_outside_tools_and_copy` | Row building: image row outside tool groups and copy text. |
| R | `transcript.rs::tests::reasoning_joins_the_tool_group_accordion` | Row building: thoughts join tool groups; summary copy. |
| R | `transcript.rs::tests::live_thought_streams_open_and_settles_closed` | Live thought auto-open and settle. |
| R | `transcript.rs::tests::codex_summary_paragraphs_render_as_separate_styled_lines` | Thought lines for Codex summaries. |
| R | `transcript.rs::tests::thought_wrap_is_word_aware_and_bounded` | Thought wrap bounds. |
| R | `transcript.rs::tests::thought_markdown_styles_instead_of_literal_markers` | User report: thought markdown styled, no literal markers. |
| R | `transcript.rs::tests::thought_blocks_flatten_structurally` | Thought block flattening. |
| R | `transcript.rs::tests::live_entry_splits_per_block_with_id_continuity` | Row ids stable across live/complete flip. |
| R | `transcript.rs::tests::live_commit_changes_only_tail_row_versions` | Only the tail row resplices per commit. |
| R | `transcript.rs::tests::split_sibling_gaps_match_live_internal_spacing` | Gap continuity at completion. |
| R | `transcript.rs::tests::consecutive_tools_fold_into_groups_between_text` | Grouping and ids. |
| R | `transcript.rs::tests::agent_calls_split_out_of_ordinary_tool_groups` | Keeper: agent calls split out; a lone agent group does not collapse. |
| R | `transcript.rs::tests::stray_subagent_ref_on_a_run_chip_stays_an_ordinary_tool` | Legacy docs: stray subagent refs stay ordinary tools. |
| R | `transcript.rs::tests::child_control_table_only_advertises_an_existing_transcript` | Child action support table and inspection copy. |
| D | `transcript.rs::tests::lone_completed_agent_stays_uncollapsed` | Duplicate. Detects: a lone agent group does not collapse and a complete entry does not auto-open. Remaining proof: `tests::agent_calls_split_out_of_ordinary_tool_groups` (single-agent group `rows[2]`: `!tool_group_collapses`, `is_agent_tool`) and `tests::trailing_group_auto_opens_only_while_streaming` (complete trailing group `!auto_open`). Unlocks 18 test lines. |
| R | `transcript.rs::tests::pre_spawn_agent_name_is_enough_to_split` | Agent genus by name before subagent_ref. |
| R | `transcript.rs::tests::trailing_group_auto_opens_only_while_streaming` | Keeper for auto_open rules. |
| R | `transcript.rs::tests::user_rows_and_echo_versions` | Pending echo version flip. |
| R | `transcript.rs::tests::user_fold_scroll::conversation_width_reflows_streaming_text_without_restarting_animations` | Linux headless rendered regression (`user_fold_scroll`, CI ubuntu): frame-by-frame transcript geometry under the real list/scroll handlers. |
| R | `transcript.rs::tests::user_fold_scroll::replay_keeps_unpainted_opening_text_seeded_and_new_text_live` | Linux headless rendered regression (`user_fold_scroll`, CI ubuntu): frame-by-frame transcript geometry under the real list/scroll handlers. |
| R | `transcript.rs::tests::user_fold_scroll::replay_text_prefix_is_visible_while_coalesced_live_suffix_fades` | Linux headless rendered regression (`user_fold_scroll`, CI ubuntu): frame-by-frame transcript geometry under the real list/scroll handlers. |
| R | `transcript.rs::tests::user_fold_scroll::replay_growth_snaps_only_when_following_the_tail` | Linux headless rendered regression (`user_fold_scroll`, CI ubuntu): frame-by-frame transcript geometry under the real list/scroll handlers. |
| R | `transcript.rs::tests::user_fold_scroll::runway_short_chat_glides_from_its_bottom_aligned_position` | Linux headless rendered regression (`user_fold_scroll`, CI ubuntu): frame-by-frame transcript geometry under the real list/scroll handlers. |
| R | `transcript.rs::tests::user_fold_scroll::runway_append_consumes_space_before_the_first_paint` | Linux headless rendered regression (`user_fold_scroll`, CI ubuntu): frame-by-frame transcript geometry under the real list/scroll handlers. |
| R | `transcript.rs::tests::user_fold_scroll::selecting_live_tail_keeps_anchor_visible_during_a_stream_burst` | Linux headless rendered regression (`user_fold_scroll`, CI ubuntu): frame-by-frame transcript geometry under the real list/scroll handlers. |
| R | `transcript.rs::tests::user_fold_scroll::active_reply_text_selection_survives_streaming_and_completion` | Linux headless rendered regression (`user_fold_scroll`, CI ubuntu): frame-by-frame transcript geometry under the real list/scroll handlers. |
| R | `transcript.rs::tests::user_fold_scroll::runway_first_echo_starts_a_glide_in_an_empty_chat` | Linux headless rendered regression (`user_fold_scroll`, CI ubuntu): frame-by-frame transcript geometry under the real list/scroll handlers. |
| R | `transcript.rs::tests::user_fold_scroll::runway_real_stream_consumes_reservation_then_follows_until_completion` | Linux headless rendered regression (`user_fold_scroll`, CI ubuntu): frame-by-frame transcript geometry under the real list/scroll handlers. |
| R | `transcript.rs::tests::user_fold_scroll::runway_wheel_down_cannot_enter_a_temporary_gap_or_reverse` | Linux headless rendered regression (`user_fold_scroll`, CI ubuntu): frame-by-frame transcript geometry under the real list/scroll handlers. |
| R | `transcript.rs::tests::user_fold_scroll::runway_background_burst_and_downward_input_reach_the_new_tail` | Linux headless rendered regression (`user_fold_scroll`, CI ubuntu): frame-by-frame transcript geometry under the real list/scroll handlers. |
| R | `transcript.rs::tests::user_fold_scroll::runway_second_send_and_steer_keep_the_previous_viewport_until_echo` | Linux headless rendered regression (`user_fold_scroll`, CI ubuntu): frame-by-frame transcript geometry under the real list/scroll handlers. |
| R | `transcript.rs::tests::user_fold_scroll::runway_user_scroll_up_stays_released_when_output_overflows` | Linux headless rendered regression (`user_fold_scroll`, CI ubuntu): frame-by-frame transcript geometry under the real list/scroll handlers. |
| R | `transcript.rs::tests::user_fold_scroll::runway_resizes_in_the_same_layout_without_a_provisional_gap` | Linux headless rendered regression (`user_fold_scroll`, CI ubuntu): frame-by-frame transcript geometry under the real list/scroll handlers. |
| R | `transcript.rs::tests::user_fold_scroll::runway_direct_jump_to_an_unmeasured_tail_does_not_reserve_unknown_rows` | Linux headless rendered regression (`user_fold_scroll`, CI ubuntu): frame-by-frame transcript geometry under the real list/scroll handlers. |
| R | `transcript.rs::tests::user_fold_scroll::runway_wheel_down_at_the_end_keeps_following_when_streaming_overflows` | Linux headless rendered regression (`user_fold_scroll`, CI ubuntu): frame-by-frame transcript geometry under the real list/scroll handlers. |
| R | `transcript.rs::tests::user_fold_scroll::runway_down_then_up_before_overflow_preserves_the_user_viewport` | Linux headless rendered regression (`user_fold_scroll`, CI ubuntu): frame-by-frame transcript geometry under the real list/scroll handlers. |
| R | `transcript.rs::tests::user_fold_scroll::runway_absorbs_tail_shrinkage_in_the_same_layout` | Linux headless rendered regression (`user_fold_scroll`, CI ubuntu): frame-by-frame transcript geometry under the real list/scroll handlers. |
| R | `transcript.rs::tests::user_fold_scroll::send_glide_never_crosses_the_prompt_during_remeasurement` | Linux headless rendered regression (`user_fold_scroll`, CI ubuntu): frame-by-frame transcript geometry under the real list/scroll handlers. |
| R | `transcript.rs::tests::user_fold_scroll::background_overflow_retires_hold_before_the_tail_is_measured` | Linux headless rendered regression (`user_fold_scroll`, CI ubuntu): frame-by-frame transcript geometry under the real list/scroll handlers. |
| R | `transcript.rs::tests::user_fold_scroll::wheel_down_releases_stale_hold_before_a_background_frame_can_run` | Linux headless rendered regression (`user_fold_scroll`, CI ubuntu): frame-by-frame transcript geometry under the real list/scroll handlers. |
| R | `transcript.rs::tests::user_fold_scroll::expanding_streaming_prompt_does_not_retire_its_runway` | Linux headless rendered regression (`user_fold_scroll`, CI ubuntu): frame-by-frame transcript geometry under the real list/scroll handlers. |
| R | `transcript.rs::tests::user_fold_scroll::folding_releases_sent_turn_hold_without_removing_reservation` | Linux headless rendered regression (`user_fold_scroll`, CI ubuntu): frame-by-frame transcript geometry under the real list/scroll handlers. Keeper for prompt fold/expand scroll behaviour. |
| R | `transcript.rs::tests::user_fold_scroll::navigation_cancels_fold_compensation_before_queued_frame` | Linux headless rendered regression (`user_fold_scroll`, CI ubuntu): frame-by-frame transcript geometry under the real list/scroll handlers. |
| R | `transcript.rs::tests::long_prompts_collapse_and_short_ones_do_not` | Collapse threshold table. |
| R | `transcript.rs::tests::user_resize_duration_scales_with_distance_and_stays_bounded` | Fold animation duration/curve. |
| D | `transcript.rs::tests::expanding_a_prompt_is_not_a_row_change` | Cannot fail for its named reason: it calls `rows_for_entry` twice on identical input without toggling anything (fold state is not an input), so it only re-proves determinism. Remaining proof: fold state is structurally render-local; row id/version stability is owned by `tests::user_rows_and_echo_versions` and `tests::live_entry_splits_per_block_with_id_continuity`; fold scroll behaviour by `user_fold_scroll::folding_releases_sent_turn_hold_without_removing_reservation`. Unlocks 15 test lines. |
| R | `transcript.rs::tests::user_rows_split_attachment_refs_from_text` | Row-level boundary for attachment trailer parsing (text, copy text, image-only). |
| R | `transcript.rs::tests::user_rows_project_file_mentions_into_chips` | Row-level boundary for mention projection; version from raw text. |
| R | `transcript.rs::tests::diff_rows_appends_and_middle_edits` | Row diff splice table. |
| R | `transcript.rs::tests::diff_handles_live_to_split_growth` | Live-to-complete splice. |
| R | `transcript.rs::tests::tool_diff_builds_real_hunks_with_context_and_numbers` | Tool diff hunks/line numbers, added files, output cap. |
| R | `transcript.rs::tests::tool_group_summaries` | User-visible group summary copy. |
| R | `transcript.rs::tests::subagent_tab_titles` | Subagent tab title derivation table. |
| R | `transcript.rs::tests::tool_chip_labels_per_kind` | Chip label/detail copy per tool kind incl. computer-use evidence. |
| R | `transcript.rs::tests::file_action_badges_show_only_the_file_name` | Badge filename extraction (POSIX/Windows/dirs). |
| R | `transcript.rs::tests::multiline_command_flattens_to_one_chip_line` | User report: multi-line commands flatten to one chip line. |
| R | `transcript.rs::tests::call_block_carries_the_full_invocation` | Invocation block formatting per kind. |
| R | `transcript.rs::tests::timestamp_strip_lands_on_the_last_settled_row` | Timestamp format and placement; copy text only when settled. |
| R | `transcript.rs::tests::message_copy_keeps_authored_text_and_excludes_tool_traces` | Copy contract excludes tool traces. |
| R | `transcript.rs::tests::single_line_collapses_all_whitespace_runs` | Whitespace collapsing helper. |
| R | `transcript.rs::tests::chips_height_is_analytic` | Chip height math. |
| R | `transcript.rs::tests::tool_row_reveal_honors_delay_easing_and_reduced_motion` | Reveal timing. |
| R | `transcript.rs::tests::tool_connector_draws_continuously_before_revealing_the_branch` | Connector reveal timing. |
| R | `transcript.rs::tests::tool_branch_reveal_tracks_distance_through_the_bend` | Branch path arc-length reveal. |
| R | `transcript.rs::tests::tool_title_shimmer_crosses_the_title_without_a_loop_seam` | Shimmer seamless loop. |
| R | `transcript.rs::tests::elapsed_formatting` | Elapsed copy. |
| R | `transcript.rs::tests::sending_bridge_holds_until_the_turn_outdates_the_send` | Sending bridge predicate table. |
| R | `transcript.rs::tests::empty_text_parts_produce_no_rows` | Empty parts produce no rows. |
| R | `typography.rs::tests::wire_values_round_trip_and_unknown_falls_back` | Persisted settings wire values; unknown falls back. |
| R | `typography.rs::tests::unavailable_family_resolves_to_geist` | Fallback resolution. |
| R | `typography.rs::tests::unavailable_code_and_terminal_families_stay_monospaced` | Mono fallback for code/terminal. |
| R | `typography.rs::tests::advance_check_separates_bundled_mono_from_bundled_sans` | Fixed-width detection over bundled faces. |
| R | `typography.rs::tests::terminal_catalog_drops_proportional_families` | Terminal catalog contract. |
| R | `typography.rs::tests::persisted_proportional_terminal_family_falls_back` | Migration: legacy proportional terminal pick falls back. |
| R | `typography.rs::tests::catalog_keeps_bundled_and_virtual_choices_first` | Catalog order. |
| R | `typography.rs::tests::legacy_optional_families_become_installed_choices` | Migration of legacy wire values. |
| D | `typography.rs::tests::latin_metric_filter_requires_the_glyph_gpui_measures` | Name promises more than the input exercises: asserts the bundled Geist face has an `m` glyph and never calls `installed_families_with_latin_metrics` (the filter under the name). No credible regression can fail it. No remaining proof needed beyond `tests::bundled_families_have_required_static_faces` (bundled faces pinned); the system-font filter is unreachable without enumerating host fonts (follow-up). Unlocks 5 test lines. |
| R | `typography.rs::tests::ui_font_sizes_have_stable_labels_and_normalize` | Size labels/normalization and rem math. |
| R | `typography.rs::tests::bundled_families_have_required_static_faces` | Package contract: bundled static faces and family metadata. |

## Layer plan

Contract keepers for the deleted and consolidated tests:

| Contract | Keeper | Retired here |
| --- | --- | --- |
| Luna mark stagger (tail leads, sweep window) | `crates/proto/src/motion.rs::tests::the_mark_sweeps_from_tail_to_head` | `loaders::tests::mark_stagger_follows_flight_axis` (whole module) |
| Syntax document cache reuse | `syntax_cache::tests::cache_reuses_neutral_documents` (F: reuse only) | `retained_bytes_measure_materialized_spans_not_source_text` |
| SVG sanitization and bounds | `image_media::tests::svg_scripts_html_and_embedded_or_external_images_never_reach_gpui`, `large_svgs_keep_the_complete_viewbox_at_bounded_resolutions`, `encoded_input_and_decode_dimensions_are_bounded` (absorbed the undecodable-bytes row) | `svg_is_bounded_and_external_resources_are_removed` |
| Appshot context stripped from displayed user text | `attachments::tests::duplicate_or_invalid_appshot_metadata_stays_an_ordinary_attachment` | `appshot_context_is_hidden_but_image_remains` |
| Diff comment citation bytes | `comments::tests::comments_append_as_located_bullets` + `a_renamed_file_cites_the_side_the_line_lives_in` | `an_unrenamed_file_cites_its_only_path_on_both_sides` |
| Link runs from markdown and autolinks | `markdown::parser::tests::autolink_leaves_non_urls_alone`, `bare_urls_autolink`; routing: `markdown::links::tests::*`; rendered: `link_interaction::rendered_tests::keyboard_visits_each_range_and_opens_the_link_menu` | `markdown::links::tests::markdown_and_autolinks_share_validation`, `markdown::parser::tests::links_carry_urls` |
| Incremental/full parse parity (closing-quote report text) | `markdown::parser::tests::incremental_matches_full_on_streamed_corpora` (CORPORA now includes `closing_quote_blocks::STORY`) + `closing_quote_blocks::full_parse_keeps_trailing_quote_in_block` | `streamed_boundary_at_quote_adds_no_block`, `streamed_small_chunks_match_full_parse` |
| Paint-only syntax runs | `markdown::render::tests::affected_language_roles_flow_through_markdown_paint_only` (+ `crates/syntax/tests/quality.rs`, `theme::tests::syntax_palette_resolves_every_kind_on_code_and_diff_backgrounds`) | `code_line_runs_cover_exactly`, `tree_sitter_runs_are_rich_and_paint_only` |
| Default RowVeil fades first text | `markdown::veil::tests::row_veil_tracks_elements_independently`, `seeded_row_adopts_existing_text_without_fading` | `default_row_veil_fades_first_text` |
| Bundled font faces | `typography::tests::bundled_families_have_required_static_faces` | `latin_metric_filter_requires_the_glyph_gpui_measures` (did not exercise the filter) |
| Enter keymap bindings | `composer::tests::message_enter_bindings_cover_both_platform_modifiers` (exact vectors) | `message_enter_adds_only_the_fixed_opposite_modifier_binding` |
| Send button mode with staged comments | `composer::tests::staged_comments_alone_are_content` + `send_button_morph` | `a_comment_only_stage_queues_during_a_live_run` |
| Stick-to-bottom spring monotone, no overshoot | `transcript::tests::stationary_spring_does_not_keep_requesting_frames` + `spring_converges_to_a_fixed_target` | `spring_never_overshoots_or_oscillates` |
| Agent groups never collapse; auto-open only while streaming | `transcript::tests::agent_calls_split_out_of_ordinary_tool_groups` + `trailing_group_auto_opens_only_while_streaming` | `lone_completed_agent_stays_uncollapsed` |
| Prompt fold is render-local | structural (no fold input to `rows_for_entry`); scroll behaviour: `transcript::tests::user_fold_scroll::folding_releases_sent_turn_hold_without_removing_reservation` | `expanding_a_prompt_is_not_a_row_change` |

**Retired files and layers:** none. No integration-test file in this scope exists, and no file was deleted. The redundant layer here is within-module duplication: a second invocation of a contract that its own table or rendered test already owns. There is one crate-local replay of a helper owned in `luna_proto`.

**Assertions carried into keepers:**
- `image_media::tests::encoded_input_and_decode_dimensions_are_bounded` gained `decode_image("image/png", b"not an image")` is an error.
- `markdown::parser::tests::CORPORA` gained the closing-quote STORY. This adds it to `incremental_matches_full_on_streamed_corpora`, `appends_keep_committed_blocks_identical` and `display_tree_prefix_matches_canonical_across_streams`.

**Test-only production seams removed (all inside this manifest; `rg` across `crates/`, `apps/` and `crates/ui/examples` found no other references):**
- `syntax_cache.rs`: `pub struct SyntaxCacheStats`, `SyntaxHighlightCache::stats()`, the `hits`/`misses` fields and their counting in `get`, and `#[cfg(test)] len()`/`is_empty()`.
- `transcript.rs`: `#[cfg(test)] StickSpring::target_vel()`, `#[cfg(test)] PendingQueuedTurns::len()` and `#[cfg(test)] SavedViewportCache::len()`. Seven retained tests now read the private fields from the child `tests` module.
- `loaders.rs`: the `MARK_SPREAD` re-export, which only the deleted test used.

**Seams retained, with the reason:**
- `composer.rs` `#[cfg(test)] layout_rebuilds`: the shaping-cache keeper `layout_cache_reuses_resize_frames_and_invalidates_text_inputs` counts rebuilds through it.
- `markdown/render.rs` `selection_test_bounds`/`selection_test_snapshot` and the `link_interaction.rs` `TOOLTIP_BOUNDS` hook: the rendered keepers depend on them.
- `image_viewer.rs` `test_scale()`: `files/image_preview.rs` tests use it (another lane).
- `typography.rs` `FontAvailability::all()/without()`: `settings/appearance.rs` tests use them (another lane), and the pure fallback keepers need them.
- `markdown/selection.rs` `test_state_lock()`: test infrastructure that serializes process-global selection state, not a behaviour seam.

## Cross-lane requests

1. **Engine lane (`crates/engine/src/repos.rs`):** `history::tests::history_search_matches_fuzzy_subject_terms_and_sha_prefix` tests the engine's `luna_engine::repos::git_history_matches` from the UI crate. It covers fuzzy subject terms, the case-insensitive SHA prefix, and a negative case. The engine tests only `git_history_matches_unicode_case_insensitively`. Please add those three cases there. After that, the UI test can be deleted as a crate-local replay. It stays R until then.
2. **Proto lane:** keep `crates/proto/src/motion.rs::tests::the_mark_sweeps_from_tail_to_head`. It is now the sole owner of the mark stagger contract.
3. **Syntax and theme lanes:** keep `crates/syntax/tests/quality.rs::rust_reference_span_snapshot` and `crates/ui/src/theme.rs::tests::syntax_palette_resolves_every_kind_on_code_and_diff_backgrounds`. The two deleted render tests also relied on them.
4. **`badges.rs` owner (L06/L07):** `badges::tests::a_comment_only_send_keeps_its_stand_in_body` asserts the same bytes as `comments::tests::comment_only_send_gets_a_body`. This lane kept the `comments.rs` codec owner. Either may go, but not both.
5. **`file_icons.rs` owner:** `transcript::tests::file_badge_icon_well_counter_shades_each_appearance` and `file_badge_icon_well_uses_more_coverage_on_frost` are the only proof of `file_icons::well_bg`. Optionally move them into `file_icons.rs`. They stay in place, so no edit is required.
6. **`appshots.rs` owner:** `appshots::tests::shot()` must stay `pub(crate)`. Tests in `attachments.rs` and `composer.rs` use it.
7. **`state.rs` owner:** composer and attachments keepers depend on `EngineHandle::from_test_client` and `AppState::set_test_engine`. Do not remove these seams without moving those keepers.

**Workflow routing changes needed:** none. No CI filter names a test, module or target that moved or disappeared.

## Possible product defects

None found. No retained assertion looks like it encodes a bug. The structural risks below are follow-ups, not failures:

- The production code holds two independent copies of history search compaction: `luna_engine::repos::compact_history_commits` and `luna_ui::history::compact_commits_to_visible`. Each has its own 20,000-commit test, and they can diverge silently.

## Follow-ups

- `SyntaxHighlightCache` eviction (`MAX_DOCUMENTS` and the `MAX_RETAINED_BYTES` budget) has no test. Add one owner test at the cache boundary. The deleted `retained_bytes` test never exercised it.
- Several tests restate one-line helpers and are the only proof of their policy. Each should be replaced by a boundary test, after which the helper test can go:
  - `composer::tests::queued_submit_does_not_publish_an_optimistic_transcript_echo`: a real queued submit during a live run should leave `pending_echoes` empty.
  - `interrupt_tracking_is_idempotent_per_chat` and `interrupt_tracking_releases_only_settled_chats` (thin `HashSet` wrappers).
  - `composer_surface_radius_tracks_the_dock_morph`.
  - `history::tests::graph_palette_only_reduces_saturation`.
- `composer::tests::pending_skip_readback_clears_only_its_own_banner` has a stale name, since "answer later" no longer sends. Its fake RPC also carries an unreachable `EXECUTE_TASK_COMMAND` receipt branch. Rename it and trim the fake.
- `syntax_cache::tests::key_depends_on_content_language_and_query_not_theme` promises "query" and "not theme" but varies neither.
- `#[cfg(test)] layout_rebuilds` in `composer.rs` could be replaced by the existing `perf_counters` instrumentation, which would retire the seam.
- `typography`'s system-font Latin-metric filter (`installed_families_with_latin_metrics`) is untested. Without host font enumeration it is hard to reach.
- The Linux-gated rendered keepers (`user_fold_scroll`, `link_interaction::rendered_tests`, the lightbox and three composer input tests) never run on macOS developer machines. Only the ubuntu CI job covers them.
- `auto_grow_math` and `veil::fade_constants_match_mugen_fade_painter` carry redundant constant self-asserts next to their real formula asserts. They are cosmetic, so they were left in place.
