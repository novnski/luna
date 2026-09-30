//! Synthetic native GPUI E2E for the installed integration picker.
//!
//! Scenarios come from `revision-r2-failure-scenarios.md`, recorded before the
//! repair. Every step drives production editor, composer, popup and key
//! binding paths inside its own window update, so GPUI flushes the same
//! subscriptions (`Edited` -> `on_input_edited` -> reconcile) the app uses.
//! Accessibility is read back from GPUI's real AccessKit tree for the rendered
//! production popup. There is no engine, RPC, provider or LUNA app here; the
//! catalog is synthetic and seeded through `fixture_seed_native_catalog`.
use super::*;
use gpui::{AnyWindowHandle, AsyncApp, KeyDownEvent, KeyUpEvent, Keystroke, PlatformInput};
use luna_proto::{ComposerCatalogSlice, ComposerInput as NativeInput};
use serde_json::{Value, json};
use std::path::Path;

impl Composer {
    /// Seed the production popup with a synthetic catalog. Catalog refreshes
    /// become local no-ops (no engine exists) until the next picker reset.
    fn fixture_seed_native_catalog(&mut self, catalog: ComposerCatalog, cx: &mut Context<Self>) {
        self.integrations.fixture_catalog = true;
        self.integrations.catalog = Some(catalog);
        self.integrations.loading = false;
        self.integrations.error = None;
        self.update_native_rows();
        self.sync_mention_controls(cx);
        cx.notify();
    }

    /// Production semantics for the open popup, as rendering exposes them.
    fn fixture_native_picker_accessibility(&self) -> Value {
        let rows: Vec<Value> = (0..self.mention.native_results.len())
            .filter_map(|index| {
                let semantics = self.native_row_semantics(index)?;
                let entry = &self.mention.native_results[index];
                Some(json!({
                    "index": self.mention.results.len() + index,
                    "kind": kind_singular(entry.kind),
                    "path": entry.path,
                    "label": semantics.label,
                    "description": semantics.description,
                    "enabled": semantics.enabled,
                }))
            })
            .collect();
        json!({
            "active": self.mention.active,
            "itemCount": self.mention_item_count(),
            "retryIndex": self.native_retry_index(),
            "notice": self.native_catalog_notice().map(|notice| notice.to_string()),
            "pickerGeneration": self.integrations.generation,
            "forceRefresh": self.integrations.force_refresh,
            "rows": rows,
        })
    }
}

fn catalog() -> ComposerCatalog {
    let entry = |kind, id: &str, name: &str, path: &str| ComposerCatalogEntry {
        kind,
        id: id.into(),
        name: name.into(),
        path: path.into(),
        description: None,
    };
    let ready = |kind| ComposerCatalogSlice {
        kind,
        status: Status::Ready,
        disabled_count: 0,
        detail: None,
    };
    ComposerCatalog {
        scope: "fixture-scope".into(),
        fetched_at_ms: chrono::Utc::now().timestamp_millis(),
        ttl_ms: 300_000,
        stale: false,
        entries: vec![
            entry(
                Kind::Plugin,
                "opaque@qa",
                "QA 名 (one)",
                "plugin://opaque@qa",
            ),
            entry(Kind::App, "docs-app", "Docs", "app://docs-app"),
            entry(Kind::Plugin, "docs@team", "Docs", "plugin://docs@team"),
            entry(
                Kind::Plugin,
                "docs@personal",
                "Docs",
                "plugin://docs@personal",
            ),
            entry(
                Kind::Skill,
                "review",
                "Review (PR)",
                "/Users/qa/.codex/skills/review/SKILL.md",
            ),
        ],
        slices: vec![ready(Kind::Plugin), ready(Kind::App), ready(Kind::Skill)],
    }
}

fn observe(label: &str, composer: &Entity<Composer>, cx: &App) -> Value {
    let this = composer.read(cx);
    let input = this.input.read(cx);
    json!({
        "step": label,
        "currentKey": this.current_key,
        "draftId": this.draft_id_for(&this.current_key),
        "engineGeneration": this.state.read(cx).engine_generation(),
        "text": input.text(),
        "inputItems": this.input_items.get(&this.current_key).cloned().unwrap_or_default(),
        "anchors": input.native_spans.iter().map(|span| json!({
            "start": span.range.start,
            "end": span.range.end,
            "path": identity(&span.item).1,
        })).collect::<Vec<_>>(),
        "undoDepth": input.undo_stack.len(),
        "redoDepth": input.redo_stack.len(),
        "suppressNextDraftEdit": this.suppress_next_draft_edit,
    })
}

fn items(step: &Value) -> usize {
    step["inputItems"].as_array().map_or(0, Vec::len)
}

fn anchors(step: &Value) -> Vec<(u64, u64)> {
    step["anchors"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|anchor| {
            (
                anchor["start"].as_u64().unwrap_or_default(),
                anchor["end"].as_u64().unwrap_or_default(),
            )
        })
        .collect()
}

/// One production step in its own window update; effects flush afterwards.
fn act(
    window: AnyWindowHandle,
    cx: &mut AsyncApp,
    f: impl FnOnce(&mut Window, &mut App),
) -> anyhow::Result<()> {
    window.update(cx, |_, window, cx| f(window, cx))
}

fn input_edit(
    composer: &Entity<Composer>,
    window: AnyWindowHandle,
    cx: &mut AsyncApp,
    f: impl FnOnce(&mut ComposerInput, &mut Window, &mut Context<ComposerInput>),
) -> anyhow::Result<()> {
    let input = cx.update(|cx| composer.read(cx).input.clone());
    act(window, cx, |window, cx| {
        input.update(cx, |input, cx| f(input, window, cx))
    })
}

fn utf16(text: &str, range: (u64, u64)) -> Range<usize> {
    let start = text[..range.0 as usize].encode_utf16().count();
    let end = text[..range.1 as usize].encode_utf16().count();
    start..end
}

fn keystroke(stroke: &str) -> anyhow::Result<Keystroke> {
    Keystroke::parse(stroke).map_err(|error| anyhow::anyhow!("keystroke {stroke}: {error:?}"))
}

/// A key typed while the composer input holds focus, through its bindings.
fn key(window: AnyWindowHandle, cx: &mut AsyncApp, stroke: &str) -> anyhow::Result<()> {
    let keystroke = keystroke(stroke)?;
    act(window, cx, |window, cx| {
        window.dispatch_keystroke(keystroke, cx);
    })
}

/// Enter/Space press and release on a focused control: GPUI's keyboard click.
fn key_press(window: AnyWindowHandle, cx: &mut AsyncApp, stroke: &str) -> anyhow::Result<()> {
    let keystroke = keystroke(stroke)?;
    let release = keystroke.clone();
    act(window, cx, |window, cx| {
        window.dispatch_event(
            PlatformInput::KeyDown(KeyDownEvent {
                keystroke,
                is_held: false,
                prefer_character_input: false,
            }),
            cx,
        );
    })?;
    act(window, cx, |window, cx| {
        window.dispatch_event(PlatformInput::KeyUp(KeyUpEvent { keystroke: release }), cx);
    })
}

fn draw(window: AnyWindowHandle, cx: &mut AsyncApp) -> anyhow::Result<()> {
    act(window, cx, |window, cx| {
        window.draw(cx).clear();
    })
}

fn capture(window: AnyWindowHandle, cx: &mut AsyncApp, path: &Path) -> anyhow::Result<()> {
    window.update(cx, |_, window, cx| -> anyhow::Result<()> {
        window.draw(cx).clear();
        window.render_to_image()?.save(path)?;
        Ok(())
    })?
}

/// AccessKit's macOS subclassing adapter activates on the first AX query to
/// the window's content view. A headless fixture has no screen reader, so it
/// issues that query itself; the next frame builds the real a11y tree.
#[cfg(target_os = "macos")]
fn activate_accessibility(window: &Window) -> anyhow::Result<()> {
    use objc2::runtime::AnyObject;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let handle = HasWindowHandle::window_handle(window)
        .map_err(|error| anyhow::anyhow!("window handle unavailable: {error}"))?;
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        anyhow::bail!("fixture window is not an AppKit window");
    };
    let view = appkit.ns_view.as_ptr().cast::<AnyObject>();
    // SAFETY: the view belongs to this live window on the main thread.
    let _children: *mut AnyObject = unsafe { objc2::msg_send![view, accessibilityChildren] };
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn activate_accessibility(_: &Window) -> anyhow::Result<()> {
    anyhow::bail!("AccessKit activation readback is implemented for macOS only")
}

fn a11y_tree(window: AnyWindowHandle, cx: &mut AsyncApp) -> anyhow::Result<Value> {
    draw(window, cx)?;
    draw(window, cx)?;
    let json = window
        .update(cx, |_, window, _| window.debug_a11y_tree_json())?
        .ok_or_else(|| anyhow::anyhow!("AccessKit tree was not built (activation failed)"))?;
    Ok(serde_json::from_str(&json)?)
}

/// `(node key, aria)` for every node with `role`.
fn nodes_with_role<'a>(tree: &'a Value, role: &str) -> Vec<(&'a str, &'a Value)> {
    tree["nodes"]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(_, node)| node["aria"]["role"] == role)
        .map(|(id, node)| (id.as_str(), &node["aria"]))
        .collect()
}

fn has_action(aria: &Value, action: &str) -> bool {
    aria["on_action"]
        .as_array()
        .is_some_and(|actions| actions.iter().any(|value| value == action))
}

fn native_option_nodes(tree: &Value) -> Vec<(&str, &Value)> {
    nodes_with_role(tree, "ListBoxOption")
        .into_iter()
        .filter(|(_, aria)| {
            aria["label"].as_str().is_some_and(|label| {
                ["Plugin ", "App ", "Skill "]
                    .iter()
                    .any(|kind| label.starts_with(kind))
            })
        })
        .collect()
}

fn retry_nodes(tree: &Value) -> Vec<(&str, &Value)> {
    nodes_with_role(tree, "Button")
        .into_iter()
        .filter(|(_, aria)| aria["label"] == "Retry native integrations discovery")
        .collect()
}

/// Plain text insertion through the same handler paste and typing use.
fn paste(
    text: String,
) -> impl FnOnce(&mut ComposerInput, &mut Window, &mut Context<ComposerInput>) {
    move |input, window, cx| input.replace_text_in_range(None, &text, window, cx)
}

/// A trusted durable restore, as native-inputs performs it.
fn restore(
    composer: &Entity<Composer>,
    text: String,
    items: Vec<NativeInput>,
) -> impl FnOnce(&mut Window, &mut App) {
    let composer = composer.clone();
    move |_, cx| {
        composer.update(cx, |this, cx| {
            this.restore_native_input_envelope(text, items, cx)
        })
    }
}

fn popup(composer: &Entity<Composer>, cx: &mut AsyncApp) -> Value {
    cx.update(|cx| composer.read(cx).fixture_native_picker_accessibility())
}

/// Drive the editor/provenance scenarios, then the popup accessibility ones.
/// Writes PNG captures into `output`; returns the JSON evidence document.
pub async fn run_integrations_picker_e2e(
    composer: Entity<Composer>,
    state: Entity<AppState>,
    window: AnyWindowHandle,
    output: &Path,
    cx: &mut AsyncApp,
) -> anyhow::Result<Value> {
    let item = NativeInput::Mention {
        name: "QA 名 (one)".into(),
        path: "plugin://opaque@qa".into(),
    };
    let skill = NativeInput::Skill {
        name: "Review (PR)".into(),
        path: "/Users/qa/.codex/skills/review/SKILL.md".into(),
    };
    let raw = native_link(&item);
    let envelope = format!("Use {raw} please");
    let mut steps: Vec<Value> = Vec::new();
    macro_rules! record {
        ($label:expr) => {{
            let step = cx.update(|cx| observe($label, &composer, cx));
            steps.push(step.clone());
            step
        }};
    }

    // Record the first engine generation and give the input keyboard focus.
    act(window, cx, |window, cx| {
        state.update(cx, |_, cx| cx.notify());
        let focus = composer.read(cx).input.focus_handle(cx);
        window.focus(&focus, cx);
    })?;
    act(window, cx, |_, cx| {
        composer.update(cx, |this, cx| {
            this.fixture_seed_native_catalog(catalog(), cx)
        })
    })?;

    // Picker acceptance through the real `@` token and MentionAccept event.
    input_edit(&composer, window, cx, paste("Ask @QA".into()))?;
    let opened = popup(&composer, cx);
    anyhow::ensure!(
        opened["rows"]
            .as_array()
            .is_some_and(|rows| rows.len() == 1),
        "@QA popup rows: {opened}"
    );
    input_edit(&composer, window, cx, |_, _, cx| {
        cx.emit(ComposerInputEvent::MentionAccept)
    })?;
    let selected = record!("selected");
    anyhow::ensure!(
        items(&selected) == 1 && anchors(&selected).len() == 1,
        "picker selection was lost: {selected}"
    );
    let selected_anchor = anchors(&selected)[0];

    // Case 3: edits on either side shift or keep the anchor and identity.
    input_edit(&composer, window, cx, |input, window, cx| {
        input.replace_text_in_range(Some(0..0), "Prefix 🧭 ", window, cx)
    })?;
    let prefixed = record!("edited-prefix");
    let shift = "Prefix 🧭 ".len() as u64;
    anyhow::ensure!(
        items(&prefixed) == 1
            && anchors(&prefixed) == vec![(selected_anchor.0 + shift, selected_anchor.1 + shift)],
        "preceding Unicode edit did not shift the anchor: {prefixed}"
    );
    input_edit(&composer, window, cx, |input, window, cx| {
        let end = input.text().encode_utf16().count();
        input.replace_text_in_range(Some(end..end), " thanks ", window, cx)
    })?;
    let suffixed = record!("edited-suffix");
    anyhow::ensure!(
        items(&suffixed) == 1 && anchors(&suffixed) == anchors(&prefixed),
        "suffix edit moved the anchor: {suffixed}"
    );

    // Case 3: inserting another matching raw link cannot mint another anchor.
    input_edit(&composer, window, cx, paste(raw.clone()))?;
    let duplicated = record!("raw-duplicate-pasted");
    anyhow::ensure!(
        items(&duplicated) == 1 && anchors(&duplicated) == anchors(&prefixed),
        "pasted duplicate link minted an anchor: {duplicated}"
    );

    // Case 1: delete exactly the selected chip; the pasted raw copy stays text.
    let chip = anchors(&duplicated)[0];
    input_edit(&composer, window, cx, move |input, window, cx| {
        let range = utf16(input.text(), chip);
        input.replace_text_in_range(Some(range), "", window, cx)
    })?;
    let deleted = record!("chip-deleted-raw-copy-remains");
    anyhow::ensure!(
        items(&deleted) == 0
            && anchors(&deleted).is_empty()
            && deleted["text"]
                .as_str()
                .is_some_and(|text| text.contains(&raw)),
        "deleted chip identity survived through its raw copy: {deleted}"
    );

    // Case 2: real Undo restores the exact anchor; real Redo removes it.
    input_edit(&composer, window, cx, |input, window, cx| {
        input.undo(&Undo, window, cx)
    })?;
    let undone = record!("undo");
    anyhow::ensure!(
        items(&undone) == 1 && anchors(&undone) == vec![chip],
        "Undo did not restore the typed anchor: {undone}"
    );
    input_edit(&composer, window, cx, |input, window, cx| {
        input.redo(&Redo, window, cx)
    })?;
    let redone = record!("redo");
    anyhow::ensure!(
        items(&redone) == 0 && anchors(&redone).is_empty(),
        "Redo did not remove the typed anchor: {redone}"
    );

    // Case 1: plain paste of the historical link after deletion stays text.
    input_edit(&composer, window, cx, paste(raw.clone()))?;
    let pasted = record!("plain-pasted-historical-link");
    anyhow::ensure!(
        items(&pasted) == 0,
        "raw paste minted a historical identity: {pasted}"
    );

    // Case 5: a trusted envelope anchors in place and starts a new document.
    act(
        window,
        cx,
        restore(&composer, envelope.clone(), vec![item.clone()]),
    )?;
    let restored = record!("trusted-envelope-restored");
    anyhow::ensure!(
        items(&restored) == 1
            && restored["text"] == envelope.as_str()
            && restored["undoDepth"] == 0,
        "trusted restore did not anchor in place: {restored}"
    );

    // Case 3: replacing any part of an anchor removes it.
    let anchor = anchors(&restored)[0];
    input_edit(&composer, window, cx, move |input, window, cx| {
        let range = utf16(input.text(), (anchor.0 + 2, anchor.0 + 3));
        input.replace_text_in_range(Some(range), "x", window, cx)
    })?;
    let partial = record!("edit-inside-anchor");
    anyhow::ensure!(
        items(&partial) == 0,
        "edit inside an anchor kept its identity: {partial}"
    );

    // Case 5: a trusted envelope missing its link appends a visible chip.
    act(
        window,
        cx,
        restore(&composer, "No link here".into(), vec![skill.clone()]),
    )?;
    let appended = record!("trusted-envelope-appended-link");
    anyhow::ensure!(
        items(&appended) == 1
            && appended["text"]
                .as_str()
                .is_some_and(|text| text.ends_with(&native_link(&skill))),
        "missing envelope link was not appended visibly: {appended}"
    );

    // Case 4: IME before an anchor shifts it, IME over it removes it, and
    // Undo restores the typed pre-composition snapshot.
    act(
        window,
        cx,
        restore(&composer, envelope.clone(), vec![item.clone()]),
    )?;
    let before_ime = record!("ime-baseline");
    input_edit(&composer, window, cx, |input, window, cx| {
        input.replace_and_mark_text_in_range(Some(0..0), "か", None, window, cx)
    })?;
    input_edit(&composer, window, cx, |input, window, cx| {
        input.replace_text_in_range(None, "漢", window, cx)
    })?;
    let ime_before = record!("ime-commit-before-anchor");
    let ime_shift = "漢".len() as u64;
    let base = anchors(&before_ime)[0];
    anyhow::ensure!(
        items(&ime_before) == 1
            && anchors(&ime_before) == vec![(base.0 + ime_shift, base.1 + ime_shift)],
        "IME commit before the anchor did not shift it: {ime_before}"
    );
    let over = anchors(&ime_before)[0];
    input_edit(&composer, window, cx, move |input, window, cx| {
        let range = utf16(input.text(), over);
        input.replace_and_mark_text_in_range(Some(range), "こ", None, window, cx)
    })?;
    let ime_marked = record!("ime-marked-over-anchor");
    input_edit(&composer, window, cx, |input, window, cx| {
        input.replace_text_in_range(None, "個", window, cx)
    })?;
    let ime_replaced = record!("ime-committed-over-anchor");
    anyhow::ensure!(
        items(&ime_marked) == 0 && items(&ime_replaced) == 0,
        "IME replacement kept the replaced identity: {ime_replaced}"
    );
    input_edit(&composer, window, cx, |input, window, cx| {
        input.undo(&Undo, window, cx)
    })?;
    let ime_undo = record!("ime-undo");
    anyhow::ensure!(
        items(&ime_undo) == 1 && anchors(&ime_undo) == vec![over],
        "IME Undo did not restore the typed pre-composition snapshot: {ime_undo}"
    );

    // Case 5: programmatic raw set_text (history recall, new draft) never mints.
    let raw_text = raw.clone();
    act(window, cx, |_, cx| {
        composer.update(cx, |this, cx| {
            this.set_input_text_without_draft_edit(raw_text, cx)
        })
    })?;
    let programmatic = record!("programmatic-set-text");
    anyhow::ensure!(
        items(&programmatic) == 0,
        "programmatic set_text minted an identity: {programmatic}"
    );

    // Case 10: native-inputs' trusted assembly (text, typed map, hydrate).
    let hydrate_text = envelope.clone();
    let hydrate_item = item.clone();
    act(window, cx, |_, cx| {
        composer.update(cx, |this, cx| {
            this.set_input_text_without_draft_edit(hydrate_text, cx);
            let key = this.current_key.clone();
            this.input_items.insert(key, vec![hydrate_item]);
            this.hydrate_native_input_envelope(cx);
        })
    })?;
    let hydrated = record!("trusted-hydrate");
    anyhow::ensure!(
        items(&hydrated) == 1 && hydrated["suppressNextDraftEdit"] == false,
        "hydration lost identity or left a stale draft-edit suppression: {hydrated}"
    );

    // Case 5: result insertion keeps prefix chips and never mints its links.
    let appended_result = format!("{envelope}\n\nResult: {}", native_link(&skill));
    input_edit(&composer, window, cx, move |input, _, cx| {
        input.set_text_keeping_native_prefix(appended_result, cx)
    })?;
    let result_insert = record!("result-insertion");
    anyhow::ensure!(
        items(&result_insert) == 1 && anchors(&result_insert) == anchors(&hydrated),
        "result insertion changed native identities: {result_insert}"
    );

    // Case 6: navigation parks exact anchors per owner; another task cannot
    // use them, and returning to the byte-identical draft restores them.
    let canvas_anchor = anchors(&result_insert);
    act(window, cx, |_, cx| {
        state.update(cx, |state, cx| {
            state.selected_chat = Some("fixture-chat-b".into());
            cx.notify();
        })
    })?;
    let other = record!("navigated-to-other-task");
    anyhow::ensure!(
        other["currentKey"] == "fixture-chat-b" && items(&other) == 0 && other["undoDepth"] == 0,
        "another task inherited canvas identity or history: {other}"
    );
    input_edit(&composer, window, cx, paste(raw.clone()))?;
    let other_paste = record!("other-task-raw-link");
    anyhow::ensure!(
        items(&other_paste) == 0,
        "other task minted identity from raw text: {other_paste}"
    );
    act(window, cx, |_, cx| {
        state.update(cx, |state, cx| {
            state.selected_chat = None;
            cx.notify();
        })
    })?;
    let back = record!("navigated-back-to-canvas");
    anyhow::ensure!(
        back["currentKey"] == "" && items(&back) == 1 && anchors(&back) == canvas_anchor,
        "exact canvas anchors did not return: {back}"
    );

    // Case 10: a trusted background restore into another task (for example a
    // failed send while the canvas is open) hydrates when that task opens.
    let background = format!("Retry {} later", native_link(&skill));
    let background_text = background.clone();
    let background_item = skill.clone();
    act(window, cx, |_, cx| {
        composer.update(cx, |this, _| {
            this.drafts.insert("fixture-chat-c".into(), background_text);
            this.input_items
                .insert("fixture-chat-c".into(), vec![background_item]);
        })
    })?;
    act(window, cx, |_, cx| {
        state.update(cx, |state, cx| {
            state.selected_chat = Some("fixture-chat-c".into());
            cx.notify();
        })
    })?;
    let background_opened = record!("background-restore-opened");
    anyhow::ensure!(
        items(&background_opened) == 1
            && anchors(&background_opened).len() == 1
            && background_opened["text"] == background.as_str(),
        "trusted background envelope was not hydrated on open: {background_opened}"
    );
    act(window, cx, |_, cx| {
        state.update(cx, |state, cx| {
            state.selected_chat = None;
            cx.notify();
        })
    })?;
    let back_again = record!("navigated-back-to-canvas-again");
    anyhow::ensure!(
        items(&back_again) == 1 && anchors(&back_again) == canvas_anchor,
        "canvas anchors changed across a second round trip: {back_again}"
    );

    // Case 6: a new canvas UUID under the same empty key is a new owner.
    let old_draft = back["draftId"].as_str().unwrap_or_default().to_owned();
    let canvas_text = envelope.clone();
    act(window, cx, |_, cx| {
        composer.update(cx, |this, cx| {
            this.canvas_draft_id = uuid::Uuid::new_v4().to_string();
            this.set_input_text_without_draft_edit(canvas_text, cx);
        })
    })?;
    let new_canvas = record!("new-canvas-same-empty-key");
    anyhow::ensure!(
        new_canvas["draftId"] != old_draft.as_str()
            && items(&new_canvas) == 0
            && new_canvas["undoDepth"] == 0,
        "new canvas reused old canvas authority: {new_canvas}"
    );

    // Case 7: the real runtime/profile replacement resets picker state.
    act(
        window,
        cx,
        restore(&composer, envelope.clone(), vec![item.clone()]),
    )?;
    act(window, cx, |_, cx| {
        composer.update(cx, |this, cx| {
            this.integrations.send_verified = true;
            this.integrations.force_refresh = true;
            this.park_native_editor(cx);
        })
    })?;
    let before_reset = cx.update(|cx| composer.read(cx).integrations.generation);
    act(window, cx, |_, cx| {
        state.update(cx, |state, cx| {
            state.prepare_runtime_replacement(cx);
            cx.notify();
        })
    })?;
    let reset = record!("profile-engine-reset");
    let picker_reset = cx.update(|cx| {
        let picker = &composer.read(cx).integrations;
        json!({
            "generationAdvanced": picker.generation != before_reset,
            "sendVerified": picker.send_verified,
            "forceRefresh": picker.force_refresh,
            "parked": picker.parked.len(),
            "catalog": picker.catalog.is_some(),
            "sendTask": picker.send_task.is_some(),
            "catalogTask": picker.task.is_some(),
        })
    });
    anyhow::ensure!(
        items(&reset) == 0
            && reset["undoDepth"] == 0
            && reset["engineGeneration"] != restored["engineGeneration"]
            && picker_reset
                == json!({"generationAdvanced": true, "sendVerified": false, "forceRefresh": false,
                    "parked": 0, "catalog": false, "sendTask": false, "catalogTask": false}),
        "engine/profile reset retained picker state: {reset} {picker_reset}"
    );
    input_edit(&composer, window, cx, paste(raw.clone()))?;
    let after_reset = record!("post-reset-raw-link");
    anyhow::ensure!(
        items(&after_reset) == 0,
        "post-reset raw link minted identity: {after_reset}"
    );
    capture(window, cx, &output.join("editor-composer.png"))?;

    // Cases 8 and 9: accessibility of the production popup via AccessKit.
    act(window, cx, |window, cx| {
        composer.update(cx, |this, cx| {
            this.set_input_text_without_draft_edit(String::new(), cx);
            this.fixture_seed_native_catalog(catalog(), cx);
        });
        let focus = composer.read(cx).input.focus_handle(cx);
        window.focus(&focus, cx);
    })?;
    input_edit(&composer, window, cx, paste("@Docs".into()))?;
    let activation = window.update(cx, |_, window, _| {
        activate_accessibility(window).map_err(|error| error.to_string())
    })?;
    if let Err(error) = activation {
        anyhow::bail!("AccessKit activation failed: {error}");
    }
    let ready_tree = a11y_tree(window, cx)?;
    let ready_model = popup(&composer, cx);
    capture(window, cx, &output.join("popup-ready.png"))?;
    let options = native_option_nodes(&ready_tree);
    let labels: Vec<&str> = options
        .iter()
        .filter_map(|(_, aria)| aria["label"].as_str())
        .collect();
    let mut unique = labels.clone();
    unique.sort_unstable();
    unique.dedup();
    anyhow::ensure!(
        options.len() == 3
            && unique.len() == 3
            && labels.contains(&"App Docs")
            && labels.contains(&"Plugin Docs (docs@team)")
            && labels.contains(&"Plugin Docs (docs@personal)"),
        "duplicate native labels are not distinguishable in AccessKit: {labels:?}"
    );
    anyhow::ensure!(
        options.iter().all(|(_, aria)| has_action(aria, "Click")
            && aria["description"]
                .as_str()
                .is_some_and(|text| text.contains("://"))),
        "native options lack Click or exact identity: {options:?}"
    );
    anyhow::ensure!(
        options
            .iter()
            .filter(|(_, aria)| aria["selected"] == true)
            .count()
            == 1,
        "exactly one native option must be selected: {options:?}"
    );
    anyhow::ensure!(
        !nodes_with_role(&ready_tree, "ListBox").is_empty() && retry_nodes(&ready_tree).is_empty(),
        "listbox missing, or Retry shown for a ready catalog"
    );

    // Arrow keys through the production key bindings move the selection.
    key(window, cx, "down")?;
    let moved = popup(&composer, cx);
    anyhow::ensure!(
        moved["active"] == 1,
        "Down did not move the native selection: {moved}"
    );

    // Stale last-good rows: visible, disabled, and Retry reachable.
    act(window, cx, |_, cx| {
        composer.update(cx, |this, cx| {
            let mut stale = catalog();
            stale.stale = true;
            this.fixture_seed_native_catalog(stale, cx);
            this.mention.active = Some(0);
            this.sync_mention_controls(cx);
        })
    })?;
    let stale_tree = a11y_tree(window, cx)?;
    let stale_model = popup(&composer, cx);
    capture(window, cx, &output.join("popup-stale-retry.png"))?;
    let stale_options = native_option_nodes(&stale_tree);
    anyhow::ensure!(
        stale_options.len() == 3
            && stale_options
                .iter()
                .all(|(_, aria)| !has_action(aria, "Click")
                    && aria["description"]
                        .as_str()
                        .is_some_and(|text| text.contains("Unavailable"))),
        "stale native rows remain activatable: {stale_options:?}"
    );
    let retry = retry_nodes(&stale_tree);
    anyhow::ensure!(
        retry.len() == 1 && has_action(retry[0].1, "Click") && has_action(retry[0].1, "Focus"),
        "Retry lacks Button role, label, Click or Focus: {retry:?}"
    );
    anyhow::ensure!(
        !nodes_with_role(&stale_tree, "Status").is_empty(),
        "catalog status notice is not exposed"
    );

    // Enter on a disabled row changes nothing.
    let text_before = cx.update(|cx| composer.read(cx).input.read(cx).text().to_owned());
    key(window, cx, "enter")?;
    let disabled_enter = record!("enter-on-disabled-row");
    anyhow::ensure!(
        items(&disabled_enter) == 0 && disabled_enter["text"] == text_before.as_str(),
        "Enter activated a disabled native row: {disabled_enter}"
    );

    // Up from the first row wraps to Retry; Enter runs the production retry.
    key(window, cx, "up")?;
    let on_retry = popup(&composer, cx);
    anyhow::ensure!(
        !on_retry["retryIndex"].is_null() && on_retry["active"] == on_retry["retryIndex"],
        "Retry is not keyboard reachable from the composer: {on_retry}"
    );
    key(window, cx, "enter")?;
    let retried = popup(&composer, cx);
    anyhow::ensure!(
        retried["pickerGeneration"] != on_retry["pickerGeneration"]
            && retried["forceRefresh"] == true,
        "Enter on Retry did not force a new discovery: {retried}"
    );

    // Retry as its own focus target (Tab stop): keyboard click with Enter.
    act(window, cx, |window, cx| {
        let focus = composer.update(cx, |this, cx| {
            this.integrations.force_refresh = false;
            this.retry_focus_handle(cx)
        });
        window.focus(&focus, cx);
    })?;
    let focused_tree = a11y_tree(window, cx)?;
    let focused_retry = focused_tree["gpui_focus"].as_str().is_some_and(|focus| {
        retry_nodes(&focused_tree)
            .iter()
            .any(|(id, _)| *id == focus)
    });
    anyhow::ensure!(
        focused_retry,
        "focused Retry is not the AccessKit focus node"
    );
    let before_click = cx.update(|cx| composer.read(cx).integrations.generation);
    key_press(window, cx, "enter")?;
    let clicked = cx.update(|cx| {
        let picker = &composer.read(cx).integrations;
        picker.generation != before_click && picker.force_refresh
    });
    anyhow::ensure!(
        clicked,
        "keyboard activation of the focused Retry did not retry"
    );

    Ok(json!({
        "evidenceKind": "synthetic-native-gpui-editor-composer-popup",
        "providerProof": false,
        "passed": true,
        "steps": steps,
        "accessibility": {
            "readyModel": ready_model,
            "staleModel": stale_model,
            "keyboard": {
                "downMovedTo": moved["active"],
                "retryIndex": on_retry["retryIndex"],
                "enterOnRetryForcedRefresh": true,
                "focusedRetryKeyboardClick": clicked,
            },
            "readyTree": ready_tree,
            "staleTree": stale_tree,
            "focusedRetryTree": focused_tree,
        },
    }))
}
