//! Failure-first integration: production PR service -> real git + fake gh subprocess.
//! Single test owns environment; no network or hosted writes occur.
#![cfg(unix)]
use luna_engine::{
    Repos, git_actions::GitActions, source_control::pull_requests::GitHubPullRequests,
};
use luna_proto::{GitPrCreateOutcome, GitPrCreateRequest};
use std::path::Path;

fn git(root: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}
fn mode(root: &Path, value: &str) {
    std::fs::write(root.join("mode"), value).unwrap();
}
fn writes(root: &Path) -> usize {
    std::fs::read_to_string(root.join("writes.jsonl"))
        .unwrap_or_default()
        .lines()
        .count()
}
fn reset(root: &Path, journal: &Path) {
    for name in ["pr.json", "writes.jsonl"] {
        let _ = std::fs::remove_file(root.join(name));
    }
    // Disposable fixture only, never a product profile.
    if journal.exists() {
        std::fs::remove_dir_all(journal).unwrap();
    }
    mode(root, "success");
}
#[tokio::test]
async fn draft_pr_subprocess_acceptance() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let checkout = root.join("checkout");
    let journal = root.join("data/github-pr-attempts");
    std::fs::create_dir_all(&checkout).unwrap();
    git(&checkout, &["init", "-q", "-b", "main"]);
    git(
        &checkout,
        &[
            "-c",
            "user.name=QA",
            "-c",
            "user.email=qa@example.invalid",
            "commit",
            "--allow-empty",
            "-qm",
            "Initial",
        ],
    );
    git(&checkout, &["checkout", "-qb", "feature/pr"]);
    git(
        &checkout,
        &["remote", "add", "origin", "git@github.com:acme/luna.git"],
    );
    let sha = git(&checkout, &["rev-parse", "HEAD"]);
    std::fs::write(root.join("sha"), &sha).unwrap();
    git(
        &checkout,
        &["update-ref", "refs/remotes/origin/feature/pr", &sha],
    );
    git(&checkout, &["config", "branch.feature/pr.remote", "origin"]);
    git(
        &checkout,
        &["config", "branch.feature/pr.merge", "refs/heads/feature/pr"],
    );
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/fixtures/github-pr")
        .canonicalize()
        .unwrap();
    let old_path = std::env::var("PATH").unwrap();
    // SAFETY: sole test in this isolated binary; no provider processes running.
    unsafe {
        std::env::set_var("PATH", format!("{}:{old_path}", fixture.display()));
        std::env::set_var("LUNA_NO_LOGIN_SHELL", "1");
        std::env::set_var("LUNA_PR_FIXTURE", root);
    }
    let actions = GitActions::new(Repos::new(&root.join("data"), "fixture"));
    let service = GitHubPullRequests::new(actions.clone(), journal.clone());
    let cwd = checkout.to_str().unwrap();
    mode(root, "auth");
    assert!(
        service
            .preview(cwd)
            .await
            .unwrap_err()
            .contains("authenticated")
    );
    assert_eq!(writes(root), 0);
    mode(root, "invalid");
    assert!(service.preview(cwd).await.is_err());
    mode(root, "oversized");
    assert!(service.preview(cwd).await.is_err());
    mode(root, "success");
    git(
        &checkout,
        &["config", "--unset", "branch.feature/pr.remote"],
    );
    assert!(
        service
            .preview(cwd)
            .await
            .unwrap_err()
            .contains("Push branch first")
    );
    git(&checkout, &["config", "branch.feature/pr.remote", "origin"]);
    git(
        &checkout,
        &[
            "remote",
            "set-url",
            "--push",
            "origin",
            "git@github.com:other/luna.git",
        ],
    );
    assert!(service.preview(cwd).await.unwrap_err().contains("differ"));
    git(
        &checkout,
        &[
            "remote",
            "set-url",
            "--push",
            "origin",
            "git@github.com:acme/luna.git",
        ],
    );
    mode(root, "diverged");
    assert!(
        service
            .preview(cwd)
            .await
            .unwrap_err()
            .contains("Push branch first")
    );
    mode(root, "success");
    let preview = service.preview(cwd).await.unwrap();
    let mut request = GitPrCreateRequest {
        preview,
        reconcile_only: false,
        title: "Fixture draft".into(),
        body: "First line\n\nSecond line: `literal` $HOME".into(),
        base: "main".into(),
        head: "feature/pr".into(),
    };
    let mut wrong = request.clone();
    wrong.head = "wrong-head".into();
    assert_eq!(
        service.create(wrong).await.unwrap().outcome,
        GitPrCreateOutcome::Rejected
    );
    let mut wrong = request.clone();
    wrong.base = "feature/pr".into();
    assert_eq!(
        service.create(wrong).await.unwrap().outcome,
        GitPrCreateOutcome::Rejected
    );
    git(
        &checkout,
        &["config", "branch.feature/pr.merge", "refs/heads/other"],
    );
    assert_eq!(
        service.create(request.clone()).await.unwrap().outcome,
        GitPrCreateOutcome::Rejected
    );
    git(
        &checkout,
        &["config", "branch.feature/pr.merge", "refs/heads/feature/pr"],
    );
    git(
        &checkout,
        &[
            "remote",
            "set-url",
            "--push",
            "origin",
            "git@github.com:other/luna.git",
        ],
    );
    assert_eq!(
        service.create(request.clone()).await.unwrap().outcome,
        GitPrCreateOutcome::Rejected
    );
    git(
        &checkout,
        &[
            "remote",
            "set-url",
            "--push",
            "origin",
            "git@github.com:acme/luna.git",
        ],
    );
    assert_eq!(writes(root), 0);
    let (a, b) = tokio::join!(
        service.create(request.clone()),
        service.create(request.clone())
    );
    let a = a.unwrap();
    let b = b.unwrap();
    let outcomes = [a.outcome, b.outcome];
    assert_eq!(
        outcomes
            .iter()
            .filter(|o| **o == GitPrCreateOutcome::Created)
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|o| **o == GitPrCreateOutcome::Existing)
            .count(),
        1
    );
    assert_eq!(writes(root), 1);
    let details = a.details.unwrap();
    assert!(details.is_draft);
    assert_eq!(details.summary.number, 42);
    assert_eq!(details.body, request.body);
    assert_eq!(details.checks.len(), 2);
    let body: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(root.join("writes.jsonl")).unwrap()).unwrap();
    assert_eq!(body["draft"], true);
    assert_eq!(body["body"], request.body);
    assert_eq!(body["head"], request.head);
    assert_eq!(body["base"], request.base);
    let mut reused = request.clone();
    reused.title = "Different payload under the same operation ID".into();
    assert_eq!(
        service.create(reused).await.unwrap().outcome,
        GitPrCreateOutcome::Rejected
    );
    assert_eq!(writes(root), 1);
    // A repeated intent cannot become a new write when the prior PR is terminal.
    for (state, expected_number) in [("CLOSED", 43), ("MERGED", 44)] {
        let mut pr: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("pr.json")).unwrap()).unwrap();
        pr["state"] = state.into();
        std::fs::write(root.join("pr.json"), serde_json::to_vec(&pr).unwrap()).unwrap();
        let before = writes(root);
        assert_eq!(
            service.create(request.clone()).await.unwrap().outcome,
            GitPrCreateOutcome::Existing
        );
        assert_eq!(writes(root), before);
        let mut renewed = request.clone();
        renewed.preview = service.preview(cwd).await.unwrap();
        let (left, right) = tokio::join!(
            service.create(renewed.clone()),
            service.create(renewed.clone())
        );
        assert_eq!(writes(root), before + 1);
        assert_eq!(
            left.unwrap().details.unwrap().summary.number,
            expected_number
        );
        assert_eq!(
            right.unwrap().details.unwrap().summary.number,
            expected_number
        );
        let reopened = GitHubPullRequests::new(actions.clone(), journal.clone());
        assert_eq!(
            reopened.create(renewed).await.unwrap().outcome,
            GitPrCreateOutcome::Existing
        );
        assert_eq!(writes(root), before + 1);
    }
    let history_path = std::fs::read_dir(&journal)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|e| e == "json"))
        .unwrap();
    let history: serde_json::Value =
        serde_json::from_slice(&std::fs::read(history_path).unwrap()).unwrap();
    assert_eq!(history["attempts"].as_array().unwrap().len(), 3);
    assert_eq!(history["attempts"][0]["confirmed"]["state"], "closed");
    assert_eq!(history["attempts"][1]["confirmed"]["state"], "merged");
    // Failure-first review regression: known operations are read-only even when
    // local/published SHA advances after their PR has closed.
    git(
        &checkout,
        &[
            "-c",
            "user.name=QA",
            "-c",
            "user.email=qa@example.invalid",
            "commit",
            "--allow-empty",
            "-qm",
            "Advance reused branch",
        ],
    );
    let advanced = git(&checkout, &["rev-parse", "HEAD"]);
    std::fs::write(root.join("sha"), &advanced).unwrap();
    git(
        &checkout,
        &["update-ref", "refs/remotes/origin/feature/pr", &advanced],
    );
    let replay = service.create(request.clone()).await.unwrap();
    assert_eq!(replay.outcome, GitPrCreateOutcome::Existing);
    assert_eq!(replay.details.unwrap().summary.number, 42);
    assert_eq!(writes(root), 3);
    let mut stale_new = request.clone();
    stale_new.preview.operation_id = uuid::Uuid::new_v4().to_string();
    assert_eq!(
        service.create(stale_new).await.unwrap().outcome,
        GitPrCreateOutcome::Rejected
    );
    assert_eq!(writes(root), 3);
    request.preview = service.preview(cwd).await.unwrap();
    reset(root, &journal);
    mode(root, "lost");
    assert_eq!(
        service.create(request.clone()).await.unwrap().outcome,
        GitPrCreateOutcome::Existing
    );
    assert_eq!(writes(root), 1);
    reset(root, &journal);
    mode(root, "reject");
    assert_eq!(
        service.create(request.clone()).await.unwrap().outcome,
        GitPrCreateOutcome::Uncertain
    );
    mode(root, "auth");
    assert_eq!(
        service.create(request.clone()).await.unwrap().outcome,
        GitPrCreateOutcome::Uncertain
    );
    mode(root, "success");
    let restarted = GitHubPullRequests::new(actions, journal.clone());
    assert_eq!(
        restarted.create(request.clone()).await.unwrap().outcome,
        GitPrCreateOutcome::Uncertain
    );
    assert_eq!(writes(root), 1);
    // Restore-like copy of the journal cannot authorize another write.
    let copy = root.join("restored/github-pr-attempts");
    std::fs::create_dir_all(&copy).unwrap();
    for entry in std::fs::read_dir(&journal).unwrap() {
        let e = entry.unwrap();
        if e.path().extension().is_some_and(|e| e == "json") {
            std::fs::copy(e.path(), copy.join(e.file_name())).unwrap();
        }
    }
    let restored = GitHubPullRequests::new(
        GitActions::new(Repos::new(&root.join("restored"), "restored")),
        copy,
    );
    assert_eq!(
        restored.create(request.clone()).await.unwrap().outcome,
        GitPrCreateOutcome::Uncertain
    );
    assert_eq!(writes(root), 1);
    reset(root, &journal);
    mode(root, "wait");
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_secs(3),
            restarted.create(request.clone())
        )
        .await
        .is_err()
    );
    mode(root, "success");
    assert_eq!(
        restarted.create(request.clone()).await.unwrap().outcome,
        GitPrCreateOutcome::Existing
    );
    assert_eq!(writes(root), 1);
    // A different base/fork/repository cannot satisfy reconciliation.
    let mut pr: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("pr.json")).unwrap()).unwrap();
    pr["headRepositoryOwner"]["login"] = "someone-else".into();
    pr["isCrossRepository"] = true.into();
    std::fs::write(root.join("pr.json"), serde_json::to_vec(&pr).unwrap()).unwrap();
    assert_eq!(
        restarted.create(request.clone()).await.unwrap().outcome,
        GitPrCreateOutcome::Uncertain
    );
    assert_eq!(writes(root), 1);
    // Malformed persisted state must fail closed.
    std::fs::remove_file(root.join("pr.json")).unwrap();
    for entry in std::fs::read_dir(&journal).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "json") {
            std::fs::write(path, "broken").unwrap();
        }
    }
    assert_eq!(
        restarted.create(request).await.unwrap().outcome,
        GitPrCreateOutcome::Rejected
    );
    assert_eq!(writes(root), 1);
    let calls = std::fs::read_to_string(root.join("calls.jsonl")).unwrap();
    assert!(!calls.contains("\"push\""));
    assert!(!calls.contains("\"merge\""));
    if let Ok(artifact) = std::env::var("LUNA_PR_ARTIFACT_DIR") {
        let out = Path::new(&artifact);
        std::fs::create_dir_all(out).unwrap();
        std::fs::write(out.join("calls.jsonl"), calls).unwrap();
        std::fs::write(out.join("result.json"), serde_json::json!({"passed":true,"hostedWrites":0,"cases":["auth","malformed","output cap","no upstream","divergent remote","remote head mismatch","edited head","same base","changed upstream","changed remote","concurrent dedupe","draft success","detail checks","lost response reconciliation","uncertain restart","restored journal","cancelled write","wrong fork","malformed journal"]}).to_string()).unwrap();
    }
}
