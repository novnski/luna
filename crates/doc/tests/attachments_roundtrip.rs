//! Round 17 probe: RunRequest.attachments must survive the command ledger's
//! loro round trip.

use luna_doc::{SessionCommandEntry, SessionCommandPayload, SessionCommandStatus, SessionDoc};

#[test]
fn run_request_attachments_survive_command_round_trip() {
    let doc = SessionDoc::init("chat-1").unwrap();
    let request = luna_proto::RunRequest {
        review: None,
        prompt: "p".into(),
        harness: None,
        provider_account_id: None,
        model: None,
        reasoning: None,
        model_options: Default::default(),
        cwd: "/tmp".into(),
        sandbox: luna_proto::SandboxLevel::WorkspaceWrite,
        auto_approve: true,
        permission_mode: luna_proto::PermissionMode::Ask,
        parent_permission_ceiling: None,
        environment_permission_ceiling: None,
        permission_policy: None,
        attachments: vec!["/tmp/a.png".into()],
        worktree: None,
        resume: None,
    };
    doc.queue_command(&SessionCommandEntry {
        id: "c1".into(),
        payload: SessionCommandPayload::Run {
            request,
            message_id: "m1".into(),
        },
        issued_by: "d".into(),
        issued_at: 1,
        based_on: None,
        expires_at: None,
        status: SessionCommandStatus::Pending,
        resolution: None,
    })
    .unwrap();
    match &doc.read_commands().unwrap()[0].payload {
        SessionCommandPayload::Run { request, .. } => {
            assert_eq!(request.attachments, vec!["/tmp/a.png".to_string()]);
        }
        other => panic!("unexpected payload {other:?}"),
    }
}
