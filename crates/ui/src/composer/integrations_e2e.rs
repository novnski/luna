//! Corrective GPUI composer/editor E2E. Written before the repair; fixture-only.
use super::*;
use luna_proto::ComposerInput as NativeInput;

/// Drive actual editor/composer methods. The caller owns the one GUI slot and
/// writes the returned readbacks alongside a native render image.
pub fn run_integrations_picker_e2e(
    composer: Entity<Composer>, window: &mut Window, cx: &mut App,
) -> anyhow::Result<serde_json::Value> {
    let item = NativeInput::Mention { name: "QA 名 (one)".into(), path: "plugin://opaque@qa".into() };
    let raw = integrations::native_link(&item);
    let mut steps = Vec::new();
    let observe = |label: &str, composer: &Entity<Composer>, cx: &mut App| {
        composer.update(cx, |this, cx| {
            this.reconcile_native_selections(cx);
            serde_json::json!({"step": label, "draftId": this.draft_id_for(&this.current_key),
                "engineGeneration": this.state.read(cx).engine_generation(),
                "text": this.input.read(cx).text(), "inputItems": this.input_items.get(&this.current_key).cloned().unwrap_or_default(),
                "editorInputItems": this.input.read(cx).native_input_items()})
        })
    };
    composer.update(cx, |this, cx| {
        this.ensure_native_editor_owner(cx);
        this.input.update(cx, |input, cx| {
            input.set_text("@QA", cx);
            input.replace_native_mention(0..3, &item, cx);
        });
    });
    steps.push(observe("selected", &composer, cx));
    anyhow::ensure!(steps.last().unwrap()["inputItems"].as_array().unwrap().len() == 1, "picker selection was lost");
    // A real editor edit before the chip shifts its byte anchor.
    composer.update(cx, |this, cx| this.input.update(cx, |input, cx| input.replace_text_in_range(Some(0..0), "Prefix 🧭 ", window, cx)));
    steps.push(observe("edited-prefix", &composer, cx));
    anyhow::ensure!(steps.last().unwrap()["inputItems"].as_array().unwrap().len() == 1, "anchor was lost by preceding Unicode edit");
    // Select all through the editor's UTF16 contract, then delete.
    composer.update(cx, |this, cx| this.input.update(cx, |input, cx| {
        let length = input.text().encode_utf16().count();
        input.replace_text_in_range(Some(0..length), "", window, cx);
    }));
    steps.push(observe("deleted", &composer, cx));
    anyhow::ensure!(steps.last().unwrap()["inputItems"].as_array().unwrap().is_empty(), "deletion retained identity");
    composer.update(cx, |this, cx| this.input.update(cx, |input, cx| input.undo(&Undo, window, cx)));
    steps.push(observe("undo", &composer, cx));
    anyhow::ensure!(steps.last().unwrap()["inputItems"].as_array().unwrap().len() == 1, "real Undo did not restore identity");
    composer.update(cx, |this, cx| this.input.update(cx, |input, cx| input.redo(&Redo, window, cx)));
    steps.push(observe("redo", &composer, cx));
    anyhow::ensure!(steps.last().unwrap()["inputItems"].as_array().unwrap().is_empty(), "real Redo did not remove identity");
    // Exact text insertion is the same ElementInputHandler used by plain paste.
    composer.update(cx, |this, cx| this.input.update(cx, |input, cx| input.replace_text_in_range(None, &raw, window, cx)));
    steps.push(observe("plain-pasted-historical-link", &composer, cx));
    anyhow::ensure!(steps.last().unwrap()["inputItems"].as_array().unwrap().is_empty(), "raw paste minted a historical native identity");
    // Only a trusted durable envelope may rehydrate the pasted projection.
    composer.update(cx, |this, cx| this.restore_native_input_envelope(raw.clone(), vec![item.clone()], cx));
    steps.push(observe("trusted-envelope-restored", &composer, cx));
    anyhow::ensure!(steps.last().unwrap()["inputItems"].as_array().unwrap().len() == 1, "trusted draft identity was lost");
    let old_id = composer.read(cx).draft_id_for("");
    composer.update(cx, |this, cx| {
        this.canvas_draft_id = uuid::Uuid::new_v4().to_string();
        this.reset_native_picker_owner(cx);
        this.input_items.clear();
        this.set_input_text_without_draft_edit(raw.clone(), cx);
        this.ensure_native_editor_owner(cx);
    });
    steps.push(observe("new-canvas-raw-link", &composer, cx));
    anyhow::ensure!(steps.last().unwrap()["draftId"].as_str() != Some(old_id.as_str()), "canvas owner was not changed");
    anyhow::ensure!(steps.last().unwrap()["inputItems"].as_array().unwrap().is_empty(), "fresh canvas minted old identity");
    // Same draft ID with a new engine generation is also a different owner.
    composer.update(cx, |this, cx| {
        this.restore_native_input_envelope(raw.clone(), vec![item.clone()], cx);
        let owner = integrations::NativeEditorOwner { engine_generation: this.state.read(cx).engine_generation().wrapping_add(1), draft_id: this.draft_id_for(&this.current_key) };
        this.input.update(cx, |input, _| input.bind_native_owner(owner));
    });
    steps.push(observe("new-engine-owner", &composer, cx));
    anyhow::ensure!(steps.last().unwrap()["inputItems"].as_array().unwrap().is_empty(), "engine owner retained old identity");
    // Seed the production popup for semantic/disabled native row readback.
    composer.update(cx, |this, cx| this.fixture_native_picker_accessibility(cx));
    Ok(serde_json::json!({"evidenceKind": "synthetic-native-gpui-editor-composer", "passed": true, "steps": steps}))
}
