//! Headless GPUI fixture (CC-04): a held image row with steer intent while a
//! turn is live. Claude Code and Codex steer it without interrupting; a
//! provider without native image steering (OpenCode) still offers only
//! "Send now". Drives the production queue resolver and renders the
//! production queue panel inside the production composer. No engine,
//! provider, RPC or LUNA app; the harness catalog is synthetic.
use super::*;
use gpui::{AnyWindowHandle, AsyncApp, Entity};
use luna_engine::registry::HarnessDescriptor;
use luna_proto::{HarnessId, SteeringMode};
use serde_json::{Value, json};
use std::path::Path;

const CHAT: &str = "cc04-queue-fixture";
const DEVICE: &str = "cc04-fixture-device";

fn descriptor(id: HarnessId, name: &str) -> HarnessDescriptor {
    HarnessDescriptor {
        id,
        name: name.into(),
        supports_steering: true,
        steering_mode: SteeringMode::StepBoundary,
        reasoning_levels: Vec::new(),
        installed: true,
        enabled: Some(true),
    }
}

fn row(id: &str, harness: HarnessId, text: &str, image: &str) -> anyhow::Result<QueuedMessage> {
    Ok(QueuedMessage {
        input_items: Vec::new(),
        id: id.into(),
        text: text.into(),
        attachments: vec![image.into()],
        hold_for_turn_end: true,
        follow_up_behavior: luna_proto::FollowUpBehavior::Steer,
        hold_reason: None,
        issued_by: DEVICE.into(),
        issued_at: 1_790_000_000_000,
        edited_at: None,
        revision: 1,
        execution: Some(serde_json::from_value(json!({
            "harness": harness, "model": null, "reasoning": null, "sandbox": "workspace-write"
        }))?),
        status: QueueMessageStatus::Held,
        delivery_gate: None,
    })
}

fn capture(window: AnyWindowHandle, cx: &mut AsyncApp, path: &Path) -> anyhow::Result<()> {
    window.update(cx, |_, window, cx| -> anyhow::Result<()> {
        window.draw(cx).clear();
        window.render_to_image()?.save(path)?;
        Ok(())
    })?
}

/// Seeds a live Claude task, then resolves and renders one held image row per
/// provider. Writes `queue-<provider>.png`; returns the JSON evidence.
pub async fn run_queue_image_steer_e2e(
    composer: Entity<Composer>,
    state: Entity<crate::state::AppState>,
    window: AnyWindowHandle,
    image: &str,
    output: &Path,
    cx: &mut AsyncApp,
) -> anyhow::Result<Value> {
    state.update(cx, |s, cx| -> anyhow::Result<()> {
        s.local_device_id = Some(DEVICE.into());
        s.devices = vec![serde_json::from_value(json!({
            "id": DEVICE, "name": "This device", "platform": std::env::consts::OS,
            "lastSeenAt": null,
            "capabilities": [luna_proto::capabilities::MESSAGE_QUEUE_ACTIONS_V1],
        }))?];
        s.chats = vec![serde_json::from_value(json!({
            "id": CHAT, "deviceId": DEVICE, "title": "Claude image steering",
            "archived": false, "createdAt": "2026-10-01T00:00:00Z",
            "config": {"harness": "claude-code", "model": null, "reasoning": null, "sandbox": "workspace-write"},
        }))?];
        s.selected_chat = Some(CHAT.into());
        cx.notify();
        Ok(())
    })?;
    composer.update(cx, |c, cx| {
        c.pickers().update(cx, |p, _| {
            p.fixture_set_harnesses(vec![
                descriptor(HarnessId::ClaudeCode, "Claude Code"),
                descriptor(HarnessId::Codex, "Codex"),
                descriptor(HarnessId::Opencode, "OpenCode"),
            ])
        })
    });

    let mut rows = Vec::new();
    for (harness, slug, expected) in [
        (
            HarnessId::ClaudeCode,
            "claude",
            QueuePrimaryAction::SteerNow,
        ),
        (HarnessId::Codex, "codex", QueuePrimaryAction::SteerNow),
        (HarnessId::Opencode, "opencode", QueuePrimaryAction::SendNow),
    ] {
        let item = row(
            &format!("{slug}-image-row"),
            harness,
            "Also check this screenshot",
            image,
        )?;
        state.update(cx, |s, cx| {
            let now = chrono::Utc::now();
            s.sessions = vec![luna_proto::Session {
                last_completed_turn: None,
                chat_id: CHAT.into(),
                device_id: DEVICE.into(),
                status: luna_proto::SessionStatus::Working,
                started_at: Some(now),
                updated_at: now,
                recovery: None,
            }];
            s.queue = vec![item.clone()];
            cx.notify();
        });
        let resolution = composer.update(cx, |c, cx| {
            c.resolve_queued_primary(CHAT, &item, false, true, cx)
        });
        // Let the queue panel finish its appear animation before capturing.
        cx.background_executor()
            .timer(std::time::Duration::from_millis(800))
            .await;
        let path = output.join(format!("queue-{slug}.png"));
        capture(window, cx, &path)?;
        anyhow::ensure!(
            resolution.action == expected && resolution.blocked.is_none(),
            "{slug}: expected {:?}, resolved {:?} blocked {:?}",
            expected,
            resolution.action,
            resolution.blocked
        );
        rows.push(json!({
            "provider": slug,
            "action": resolution.action.label(),
            "tooltip": resolution.tooltip(),
            "blocked": resolution.blocked.map(|b| format!("{b:?}")),
            "png": path.file_name().and_then(|n| n.to_str()),
        }));
    }
    Ok(json!({"chat": CHAT, "rows": rows, "realProvider": false, "engine": false}))
}
