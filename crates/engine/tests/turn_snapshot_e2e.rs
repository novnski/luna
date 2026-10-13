//! ZI-01: Latest turn snapshots stay out of the checkout's Git database, are
//! bounded, die with their turn, and live exactly as long as their readers.
//!
//! Every case drives the engine's public turn hook (a real dispatched turn, or
//! the same `note_turn_start` hook sessions call) and the `GetCheckoutDiff` /
//! `GetCheckoutFileDiffText` RPCs over the memory transport, in a disposable
//! repository under a temp dir — never a real checkout. The oracle is the
//! `git` CLI hashing the same working tree into its own bare object database.
//! Manifests are byte hashes of the checkout's object files, index file and
//! refs, taken with read-only Git commands.
//!
//! With `LUNA_ZI01_ARTIFACT_DIR` set, each case writes
//! `snapshot-e2e-<case>.json` there (manifest digests, expected/actual patch
//! hashes, omission counts, process receipts, durations).

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use luna_engine::{CheckoutDiffSync, EngineCore, HarnessRegistry};
use luna_harness::mock::MockHarness;
use luna_proto::{
    AgentEvent, CheckoutDiff, CheckoutFileDiffText, DoneStatus, HarnessId, RunRequest,
    SandboxLevel, SessionStatus,
};
use luna_rpc::{RpcClient, methods};
use serde_json::json;
use sha2::{Digest, Sha256};

const CHAT: &str = "chat-zi01";
const PATCH_ARGS: &[&str] = &[
    "--no-ext-diff",
    "--no-color",
    "--find-renames",
    "--unified=3",
];
const UNAVAILABLE: &str = "Latest turn is unavailable";
/// Content long enough for Git's rename detection to pair the move.
const MOVED: &str = "first line of a file that moves\nsecond line\nthird line\nfourth line\n";

// ---------------------------------------------------------------------------
// Git fixtures and the independent oracle
// ---------------------------------------------------------------------------

/// A `git` command isolated from any repository the test process inherits.
fn git_command(cwd: &Path) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new("git");
    cmd.current_dir(cwd);
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    ] {
        cmd.env_remove(name);
    }
    cmd.env("GIT_AUTHOR_NAME", "fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .env("GIT_TERMINAL_PROMPT", "0");
    cmd
}

async fn run(mut cmd: tokio::process::Command, what: &str) -> Vec<u8> {
    let output = cmd.output().await.expect("git spawns");
    assert!(
        output.status.success(),
        "{what} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

async fn git_raw(cwd: &Path, args: &[&str]) -> Vec<u8> {
    let mut cmd = git_command(cwd);
    cmd.args(args);
    run(cmd, &format!("git {args:?}")).await
}

async fn git(cwd: &Path, args: &[&str]) -> String {
    String::from_utf8(git_raw(cwd, args).await)
        .expect("utf-8 git output")
        .trim()
        .to_string()
}

fn write(root: &Path, path: &str, content: &str) {
    let full = root.join(path);
    std::fs::create_dir_all(full.parent().expect("parent")).expect("fixture dir");
    std::fs::write(full, content).expect("fixture file");
}

fn append(root: &Path, path: &str, content: &str) {
    let full = root.join(path);
    let mut text = std::fs::read_to_string(&full).expect("fixture file");
    text.push_str(content);
    std::fs::write(full, text).expect("fixture file");
}

#[derive(Clone, Copy, Debug)]
enum Layout {
    Ordinary,
    Unborn,
    LinkedWorktree,
}

impl Layout {
    /// Tracked files the fixture force-added under an ignore rule.
    fn forced(self) -> &'static [&'static str] {
        match self {
            Layout::Unborn => &[],
            Layout::Ordinary | Layout::LinkedWorktree => &["vendor/keep.log"],
        }
    }
}

struct Fixture {
    _tmp: tempfile::TempDir,
    base: PathBuf,
    /// Engine data root. Its name holds a space and, on Unix, the `:` Git's
    /// alternates list must quote.
    data: PathBuf,
    /// The checkout the chat works in (the linked worktree for that layout).
    checkout: PathBuf,
    /// Bare oracle repository: its own object database.
    oracle: PathBuf,
    /// A folder outside any Git checkout.
    plain: PathBuf,
}

fn awkward(name: &str) -> String {
    if cfg!(unix) {
        format!("{name}: dir")
    } else {
        format!("{name} dir")
    }
}

async fn fixture(layout: Layout) -> Fixture {
    let tmp = tempfile::tempdir().expect("tempdir");
    let base = tmp.path().canonicalize().expect("canonical tempdir");
    let data = base.join(awkward("data"));
    let checkouts = base.join(awkward("repos"));
    let main = checkouts.join("main repo");
    std::fs::create_dir_all(&main).expect("main repo dir");
    git(&main, &["init", "-q", "-b", "main"]).await;
    let oracle = base.join("oracle.git");
    git(
        &base,
        &["init", "-q", "--bare", oracle.to_str().expect("utf-8")],
    )
    .await;
    let plain = base.join("plain folder");
    std::fs::create_dir_all(&plain).expect("plain dir");
    // The non-checkout case must never resolve to an enclosing real checkout.
    let mut probe = git_command(&plain);
    probe.args(["rev-parse", "--show-toplevel"]);
    assert!(
        !probe.output().await.expect("git spawns").status.success(),
        "the temp dir must be outside any Git checkout"
    );

    let checkout = match layout {
        Layout::Unborn => {
            write(&main, ".gitignore", "*.log\n");
            write(&main, "staged.txt", "staged before the turn\n");
            git(&main, &["add", "staged.txt"]).await;
            main.clone()
        }
        Layout::Ordinary | Layout::LinkedWorktree => {
            write(&main, ".gitignore", "*.log\n");
            write(&main, "a.txt", "one\ntwo\n");
            write(&main, "staged.txt", "base\n");
            write(&main, "gone.txt", "remove me\n");
            write(&main, "moved.txt", MOVED);
            write(&main, "dir with space/file.txt", "spaced\n");
            write(&main, "vendor/keep.log", "tracked but ignored\n");
            git(&main, &["add", "-A"]).await;
            git(&main, &["add", "-f", "vendor/keep.log"]).await;
            git(
                &main,
                &["-c", "commit.gpgsign=false", "commit", "-q", "-m", "base"],
            )
            .await;
            let checkout = if matches!(layout, Layout::LinkedWorktree) {
                let linked = checkouts.join("linked worktree");
                let target = linked.to_str().expect("utf-8").to_string();
                git(&main, &["worktree", "add", "-q", "-b", "feature", &target]).await;
                linked
            } else {
                main.clone()
            };
            // Staged and unstaged work from before the turn.
            write(&checkout, "staged.txt", "staged before the turn\n");
            git(&checkout, &["add", "staged.txt"]).await;
            write(&checkout, "a.txt", "one\ntwo\npre-turn\n");
            checkout
        }
    };
    // Untracked and ignored state from before the turn.
    write(&checkout, "pre.txt", "before the turn\n");
    write(&checkout, "notes/todo.md", "- item\n");
    write(&checkout, "debug.log", "ignored noise\n");
    Fixture {
        _tmp: tmp,
        base,
        data,
        checkout,
        oracle,
        plain,
    }
}

/// What an agent turn does to the checkout.
async fn apply_turn_edits(layout: Layout, checkout: &Path, stage: bool) {
    append(checkout, "pre.txt", "edited in turn\n");
    write(checkout, "turn.txt", "made this turn\n");
    write(checkout, "debug2.log", "still ignored\n");
    match layout {
        Layout::Unborn => append(checkout, "staged.txt", "unstaged turn edit\n"),
        Layout::Ordinary | Layout::LinkedWorktree => {
            append(checkout, "a.txt", "turn edit\n");
            std::fs::remove_file(checkout.join("gone.txt")).expect("delete tracked file");
            std::fs::rename(checkout.join("moved.txt"), checkout.join("renamed.txt"))
                .expect("rename tracked file");
            write(
                checkout,
                "dir with space/file.txt",
                "spaced\nchanged in turn\n",
            );
            append(
                checkout,
                "vendor/keep.log",
                "tracked change under an ignore rule\n",
            );
        }
    }
    if stage {
        git(checkout, &["add", "turn.txt"]).await;
    }
}

/// The oracle's tree for `worktree`: tracked and unignored untracked files
/// plus the fixture's known force-added files, hashed through a fresh index
/// into the oracle's own database, leaving out `omit` (Git pathspec units).
async fn oracle_tree(fx: &Fixture, forced: &[&str], omit: &[&str]) -> String {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let index = fx.oracle.join(format!(
        "oracle-index-{}",
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let mut add = git_command(&fx.checkout);
    add.arg("--git-dir")
        .arg(&fx.oracle)
        .arg("--work-tree")
        .arg(&fx.checkout)
        .env("GIT_INDEX_FILE", &index)
        .args(["add", "-A", "--", "."]);
    for unit in omit {
        add.arg(format!(":(top,exclude,literal){unit}"));
    }
    run(add, "oracle add").await;
    let present: Vec<&str> = forced
        .iter()
        .copied()
        .filter(|path| fx.checkout.join(path).exists())
        .collect();
    if !present.is_empty() {
        let mut force = git_command(&fx.checkout);
        force
            .arg("--git-dir")
            .arg(&fx.oracle)
            .arg("--work-tree")
            .arg(&fx.checkout)
            .env("GIT_INDEX_FILE", &index)
            .args(["add", "-f", "--"])
            .args(&present);
        run(force, "oracle add -f").await;
    }
    let mut write_tree = git_command(&fx.checkout);
    write_tree
        .arg("--git-dir")
        .arg(&fx.oracle)
        .env("GIT_INDEX_FILE", &index)
        .arg("write-tree");
    let tree = String::from_utf8(run(write_tree, "oracle write-tree").await)
        .expect("utf-8")
        .trim()
        .to_string();
    let _ = std::fs::remove_file(&index);
    tree
}

async fn oracle_diff(fx: &Fixture, from: &str, to: &str, args: &[&str]) -> Vec<u8> {
    let mut cmd = git_command(&fx.base);
    cmd.arg("--git-dir")
        .arg(&fx.oracle)
        .arg("diff")
        .args(args)
        .args([from, to, "--"]);
    run(cmd, "oracle diff").await
}

async fn oracle_blob(fx: &Fixture, tree: &str, path: &str) -> String {
    let spec = format!("{tree}:{path}");
    let mut cmd = git_command(&fx.base);
    cmd.arg("--git-dir")
        .arg(&fx.oracle)
        .args(["cat-file", "blob", spec.as_str()]);
    String::from_utf8(run(cmd, "oracle cat-file").await).expect("utf-8 blob")
}

/// The oracle's expectation for a Latest turn between two trees.
struct Expected {
    patch: String,
    files: Vec<(String, Option<String>, char)>,
    additions: u32,
    deletions: u32,
}

async fn expected(fx: &Fixture, from: &str, to: &str) -> Expected {
    let patch = String::from_utf8(oracle_diff(fx, from, to, PATCH_ARGS).await).expect("utf-8");
    let raw = oracle_diff(fx, from, to, &["--name-status", "-z", "--find-renames"]).await;
    let fields: Vec<String> = raw
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty())
        .map(|field| String::from_utf8_lossy(field).into_owned())
        .collect();
    let mut files = Vec::new();
    let mut i = 0;
    while i < fields.len() {
        let code = fields[i].chars().next().expect("status letter");
        if code == 'R' || code == 'C' {
            files.push((fields[i + 2].clone(), Some(fields[i + 1].clone()), code));
            i += 3;
        } else {
            files.push((fields[i + 1].clone(), None, code));
            i += 2;
        }
    }
    files.sort();
    let stat =
        String::from_utf8(oracle_diff(fx, from, to, &["--shortstat", "--find-renames"]).await)
            .expect("utf-8");
    let count = |word: &str| -> u32 {
        stat.split(',')
            .find(|part| part.contains(word))
            .and_then(|part| part.split_whitespace().next())
            .and_then(|number| number.parse().ok())
            .unwrap_or(0)
    };
    Expected {
        patch,
        files,
        additions: count("insertion"),
        deletions: count("deletion"),
    }
}

fn actual_files(diff: &CheckoutDiff) -> Vec<(String, Option<String>, char)> {
    let mut files: Vec<_> = diff
        .files
        .iter()
        .map(|file| {
            let code = match file.status.as_str() {
                "added" => 'A',
                "deleted" => 'D',
                "renamed" => 'R',
                "copied" => 'C',
                _ => 'M',
            };
            (file.path.clone(), file.old_path.clone(), code)
        })
        .collect();
    files.sort();
    files
}

fn assert_matches_oracle(diff: &CheckoutDiff, want: &Expected, marker: Option<&str>) {
    let mut patch = want.patch.clone();
    if let Some(marker) = marker {
        patch.push_str(marker);
    }
    assert_eq!(
        diff.patch, patch,
        "Latest turn patch differs from the Git oracle"
    );
    assert_eq!(actual_files(diff), want.files);
    assert_eq!(
        (diff.additions, diff.deletions),
        (want.additions, want.deletions)
    );
}

// ---------------------------------------------------------------------------
// Checkout manifests
// ---------------------------------------------------------------------------

fn hex(bytes: impl AsRef<[u8]>) -> String {
    bytes.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Debug, PartialEq, Eq)]
struct Manifest {
    /// Every file under the checkout's object directory → sha256 of its bytes.
    objects: BTreeMap<String, String>,
    /// `for-each-ref` plus the symbolic HEAD.
    refs: String,
    /// sha256 of the real index file's bytes.
    index: String,
    /// `ls-files -s`: the staged entries.
    entries: String,
    /// Read-only `status --porcelain`.
    status: String,
}

impl Manifest {
    fn digest(&self) -> String {
        let mut hasher = Sha256::new();
        for (path, hash) in &self.objects {
            hasher.update(path.as_bytes());
            hasher.update(hash.as_bytes());
        }
        for part in [&self.refs, &self.index, &self.entries, &self.status] {
            hasher.update([0u8]);
            hasher.update(part.as_bytes());
        }
        hex(hasher.finalize())
    }

    fn summary(&self) -> serde_json::Value {
        json!({
            "objectFiles": self.objects.len(),
            "index": self.index,
            "digest": self.digest(),
        })
    }
}

fn collect_files(dir: &Path, base: &Path, out: &mut BTreeMap<String, String>) {
    for entry in std::fs::read_dir(dir).expect("read objects dir").flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, base, out);
        } else {
            let relative = path.strip_prefix(base).expect("under objects dir");
            let bytes = std::fs::read(&path).expect("read object file");
            out.insert(
                relative.to_string_lossy().replace('\\', "/"),
                hex(Sha256::digest(&bytes)),
            );
        }
    }
}

async fn manifest(checkout: &Path) -> Manifest {
    let objects_dir = PathBuf::from(
        git(
            checkout,
            &[
                "rev-parse",
                "--path-format=absolute",
                "--git-path",
                "objects",
            ],
        )
        .await,
    );
    let index = PathBuf::from(
        git(
            checkout,
            &["rev-parse", "--path-format=absolute", "--git-path", "index"],
        )
        .await,
    );
    let mut objects = BTreeMap::new();
    collect_files(&objects_dir, &objects_dir, &mut objects);
    // One line per ref, then HEAD. An unborn repository has no refs: its
    // empty listing must not become an empty line.
    let listed = git(
        checkout,
        &["for-each-ref", "--format=%(refname) %(objectname)"],
    )
    .await;
    let head = format!(
        "HEAD -> {}",
        git(checkout, &["symbolic-ref", "-q", "HEAD"]).await
    );
    let refs = listed
        .lines()
        .filter(|line| !line.is_empty())
        .chain(std::iter::once(head.as_str()))
        .collect::<Vec<_>>()
        .join("\n");
    let index = match std::fs::read(&index) {
        Ok(bytes) => hex(Sha256::digest(&bytes)),
        Err(_) => "absent".into(),
    };
    Manifest {
        objects,
        refs,
        index,
        entries: git(checkout, &["ls-files", "-s"]).await,
        status: git(
            checkout,
            &[
                "--no-optional-locks",
                "status",
                "--porcelain",
                "--untracked-files=all",
            ],
        )
        .await,
    }
}

/// The checkout's object database, index and refs are exactly as before.
fn assert_untouched(before: &Manifest, after: &Manifest, what: &str) {
    assert_eq!(
        after.objects, before.objects,
        "{what}: checkout object files changed"
    );
    assert_eq!(after.refs, before.refs, "{what}: refs changed");
    assert_eq!(after.index, before.index, "{what}: index bytes changed");
    assert_eq!(
        after.entries, before.entries,
        "{what}: staged entries changed"
    );
}

fn without_checkpoint_refs(refs: &str) -> Vec<&str> {
    refs.lines()
        .filter(|line| !line.is_empty() && !line.starts_with("refs/luna/checkpoints/"))
        .collect()
}

/// Object files added since `before` that no `refs/luna/checkpoints/*` ref
/// reaches. A real turn records File restore checkpoints there by design;
/// anything else would be app pollution.
async fn unexplained_new_objects(
    checkout: &Path,
    before: &Manifest,
    after: &Manifest,
) -> Vec<String> {
    let added: Vec<&String> = after
        .objects
        .keys()
        .filter(|path| !before.objects.contains_key(*path))
        .collect();
    if added.is_empty() {
        return Vec::new();
    }
    let refs = git(
        checkout,
        &[
            "for-each-ref",
            "--format=%(refname)",
            "refs/luna/checkpoints/",
        ],
    )
    .await;
    let reachable: HashSet<String> = if refs.is_empty() {
        HashSet::new()
    } else {
        let mut args = vec!["rev-list", "--objects", "--no-object-names"];
        args.extend(refs.lines());
        git(checkout, &args)
            .await
            .lines()
            .map(str::to_string)
            .collect()
    };
    added
        .into_iter()
        .filter(|path| {
            // Loose objects only (`ab/cdef…`); a new pack or index is itself unexplained.
            let oid = path
                .split_once('/')
                .filter(|(fan, rest)| fan.len() == 2 && rest.len() >= 38)
                .map(|(fan, rest)| format!("{fan}{rest}"));
            !oid.is_some_and(|oid| reachable.contains(&oid))
        })
        .cloned()
        .collect()
}

// ---------------------------------------------------------------------------
// Engine, RPC and store observation
// ---------------------------------------------------------------------------

fn assemble(data: &Path, cwd: &Path) -> EngineCore {
    std::fs::create_dir_all(data).expect("data dir");
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(MockHarness {
        script: vec![
            AgentEvent::SessionStarted {
                harness: HarnessId::Mock,
                model: "mock-1".into(),
                tools: vec![],
                cwd: cwd.to_string_lossy().into_owned(),
                session_id: "zi01-session".into(),
                assistant_message_id: "zi01-assistant".into(),
            },
            AgentEvent::TextDelta {
                text: "turn ran".into(),
            },
            AgentEvent::Done {
                status: DoneStatus::Completed,
                result: None,
                error: None,
                session_id: Some("zi01-session".into()),
            },
        ],
    }));
    EngineCore::assemble(data, Arc::new(registry), HarnessId::Mock, None)
        .expect("engine core assembles")
}

fn run_request(cwd: &Path) -> RunRequest {
    RunRequest {
        review: None,
        goal: None,
        input_items: Vec::new(),
        prompt: "change the files".into(),
        harness: None,
        provider_account_id: None,
        model: None,
        reasoning: None,
        model_options: Default::default(),
        cwd: cwd.to_string_lossy().into_owned(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: true,
        permission_mode: luna_proto::PermissionMode::Ask,
        parent_permission_ceiling: None,
        environment_permission_ceiling: None,
        permission_policy: None,
        attachments: Vec::new(),
        worktree: None,
        resume: None,
    }
}

async fn wait_until(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !ready() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Settled after a completed turn: Done, its checkpoint and the Idle status.
async fn wait_turn_completed(core: &EngineCore) {
    wait_until("the mock turn to complete", || {
        core.sessions.session_status(CHAT).is_some_and(|session| {
            session.status == SessionStatus::Idle && session.last_completed_turn.is_some()
        })
    })
    .await;
}

async fn turn_diff(client: &RpcClient, cwd: &Path) -> Result<CheckoutDiff, String> {
    client
        .call(
            methods::GET_CHECKOUT_DIFF,
            json!({ "cwd": cwd, "mode": "turn", "chatId": CHAT }),
        )
        .await
        .map(|value| serde_json::from_value(value).expect("typed CheckoutDiff"))
        .map_err(|error| error.to_string())
}

async fn turn_file_text(
    client: &RpcClient,
    diff: &CheckoutDiff,
    cwd: &Path,
    path: &str,
) -> CheckoutFileDiffText {
    let value = client
        .call(
            methods::GET_CHECKOUT_FILE_DIFF_TEXT,
            json!({
                "checkoutId": diff.checkout_id,
                "cwd": cwd,
                "path": path,
                "mode": "turn",
                "chatId": CHAT,
                "diffChecksum": diff.checksum,
            }),
        )
        .await
        .expect("GetCheckoutFileDiffText");
    serde_json::from_value(value).expect("typed CheckoutFileDiffText")
}

/// Snapshot stores on disk: `{data}/turn-snapshots/{run}/{store}`.
fn stores(data: &Path) -> Vec<PathBuf> {
    let mut stores = Vec::new();
    let Ok(runs) = std::fs::read_dir(data.join("turn-snapshots")) else {
        return stores;
    };
    for run in runs.flatten() {
        if let Ok(entries) = std::fs::read_dir(run.path()) {
            stores.extend(entries.flatten().map(|entry| entry.path()));
        }
    }
    stores.sort();
    stores
}

fn sha(text: &str) -> String {
    hex(Sha256::digest(text.as_bytes()))
}

fn write_artifact(case: &str, value: serde_json::Value) {
    let Some(dir) = std::env::var_os("LUNA_ZI01_ARTIFACT_DIR") else {
        return;
    };
    let dir = PathBuf::from(dir);
    std::fs::create_dir_all(&dir).expect("artifact dir");
    std::fs::write(
        dir.join(format!("snapshot-e2e-{case}.json")),
        serde_json::to_vec_pretty(&value).expect("artifact json"),
    )
    .expect("write artifact");
}

// ---------------------------------------------------------------------------
// Real turns: oracle equality and an untouched checkout
// ---------------------------------------------------------------------------

/// A real dispatched turn, then Latest turn reads, in one checkout layout.
/// Catches: snapshot blobs/trees written into the checkout's database (by
/// the turn-start capture or by every read), private objects read from the
/// checkout alone, pre-turn staged/unstaged/untracked state reappearing, and
/// tracked-but-ignored changes silently missing.
async fn latest_turn_case(layout: Layout, case: &str) {
    let fx = fixture(layout).await;
    let core = assemble(&fx.data, &fx.checkout);
    let client = luna_rpc::memory_client(core.rpc_service());
    let m0 = manifest(&fx.checkout).await;

    let started = Instant::now();
    core.sessions
        .dispatch(CHAT, HarnessId::Mock, run_request(&fx.checkout), None)
        .await
        .expect("dispatch a mock turn");
    wait_turn_completed(&core).await;
    let clean = turn_diff(&client, &fx.checkout)
        .await
        .expect("Latest turn after a turn that changed nothing");
    let cold_ms = started.elapsed().as_millis() as u64;
    assert!(
        clean.files.is_empty() && clean.patch.is_empty() && !clean.truncated,
        "pre-turn staged, unstaged and untracked state reappeared: {clean:?}"
    );

    // The turn also recorded File restore checkpoints (reachable private
    // refs, by design). Nothing else may have been written.
    let m1 = manifest(&fx.checkout).await;
    assert_eq!(m1.entries, m0.entries, "staged entries changed");
    assert_eq!(m1.status, m0.status, "working tree changed");
    assert_eq!(
        without_checkpoint_refs(&m1.refs),
        without_checkpoint_refs(&m0.refs)
    );
    assert_eq!(
        unexplained_new_objects(&fx.checkout, &m0, &m1).await,
        Vec::<String>::new(),
        "objects outside checkpoint refs were written into the checkout"
    );

    let t0 = oracle_tree(&fx, layout.forced(), &[]).await;
    apply_turn_edits(layout, &fx.checkout, true).await;
    let m2 = manifest(&fx.checkout).await;
    let t1 = oracle_tree(&fx, layout.forced(), &[]).await;
    let want = expected(&fx, &t0, &t1).await;

    let started = Instant::now();
    let turn = turn_diff(&client, &fx.checkout).await.expect("Latest turn");
    let warm_ms = started.elapsed().as_millis() as u64;
    assert!(!turn.truncated, "nothing was over budget: {turn:?}");
    assert_matches_oracle(&turn, &want, None);

    // Old side through the turn's object context; new side from the working
    // tree. (The turn-hook case below reads a blob only a private store holds.)
    let pre = turn_file_text(&client, &turn, &fx.checkout, "pre.txt").await;
    assert!(!pre.stale, "{pre:?}");
    assert_eq!(
        pre.old_text.as_deref(),
        Some(oracle_blob(&fx, &t0, "pre.txt").await.as_str())
    );
    let current = std::fs::read_to_string(fx.checkout.join("pre.txt")).expect("pre.txt");
    assert_eq!(pre.new_text.as_deref(), Some(current.as_str()));

    let m3 = manifest(&fx.checkout).await;
    assert_untouched(&m2, &m3, "Latest turn reads");
    assert_eq!(
        m3.status, m2.status,
        "Latest turn reads changed the working tree"
    );

    // Comparison stores die with their read; the turn's base stays until
    // shutdown releases it.
    wait_until("comparison stores removed", || stores(&fx.data).len() == 1).await;
    core.shutdown().await;
    wait_until("base store released at shutdown", || {
        stores(&fx.data).is_empty()
    })
    .await;

    write_artifact(
        case,
        json!({
            "case": case,
            "layout": format!("{layout:?}"),
            "manifests": {
                "beforeTurn": m0.summary(),
                "afterTurn": m1.summary(),
                "beforeReads": m2.summary(),
                "afterReads": m3.summary(),
            },
            "oracle": { "turnStartTree": t0, "currentTree": t1 },
            "patch": { "expectedSha256": sha(&want.patch), "actualSha256": sha(&turn.patch) },
            "files": turn.files.len(),
            "durationsMs": { "turnToFirstRead": cold_ms, "secondRead": warm_ms },
        }),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ordinary_checkout_latest_turn_matches_oracle_and_leaves_checkout_untouched() {
    latest_turn_case(Layout::Ordinary, "ordinary").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unborn_head_latest_turn_matches_oracle_and_leaves_checkout_untouched() {
    latest_turn_case(Layout::Unborn, "unborn").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn linked_worktree_latest_turn_matches_oracle_and_leaves_checkout_untouched() {
    latest_turn_case(Layout::LinkedWorktree, "linked-worktree").await;
}

// ---------------------------------------------------------------------------
// Turn hook alone: exact manifests, crash collection, sweep ownership
// ---------------------------------------------------------------------------

/// The turn hook sessions call, without a provider run (so no checkpoint
/// writes): every byte of the linked worktree's common object database,
/// index and refs is unchanged across capture and reads. A crashed earlier
/// run's store is collected at assembly; a second sync starting on the same
/// data root (an in-process restart) sweeps again without touching this
/// run's live store; another data root is never swept.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn turn_hook_writes_only_private_stores_and_collects_only_stale_runs() {
    let layout = Layout::LinkedWorktree;
    let fx = fixture(layout).await;
    let crashed = fx.data.join("turn-snapshots/1-1/crashed-store");
    write(&crashed, "objects/ab/cdef", "left by a killed engine");
    let foreign = fx.base.join("other data/turn-snapshots/1-1/other-store");
    write(&foreign, "objects/ab/cdef", "another data root");
    let m0 = manifest(&fx.checkout).await;

    let core = assemble(&fx.data, &fx.checkout);
    wait_until("crashed run collected", || !crashed.exists()).await;
    let client = luna_rpc::memory_client(core.rpc_service());
    core.diff_sync
        .note_turn_start(CHAT, &fx.checkout.to_string_lossy());
    let clean = turn_diff(&client, &fx.checkout).await.expect("first read");
    assert!(clean.files.is_empty() && !clean.truncated, "{clean:?}");
    wait_until("only the base store remains", || {
        stores(&fx.data).len() == 1
    })
    .await;
    let live = stores(&fx.data);

    let late = fx.data.join("turn-snapshots/2-2");
    std::fs::create_dir_all(&late).expect("late stale run");
    let second = CheckoutDiffSync::start(
        core.repos.clone(),
        core.workspace.clone(),
        &core.device_id,
        None,
    );
    wait_until("second sweep ran", || !late.exists()).await;
    assert_eq!(stores(&fx.data), live, "this run's live store was swept");
    second.shutdown().await;
    assert!(foreign.exists(), "another data root's store was swept");

    let t0 = oracle_tree(&fx, layout.forced(), &[]).await;
    apply_turn_edits(layout, &fx.checkout, false).await;
    let t1 = oracle_tree(&fx, layout.forced(), &[]).await;
    let want = expected(&fx, &t0, &t1).await;
    let turn = turn_diff(&client, &fx.checkout).await.expect("Latest turn");
    assert_matches_oracle(&turn, &want, None);
    let renamed = turn_file_text(&client, &turn, &fx.checkout, "renamed.txt").await;
    assert!(!renamed.stale, "{renamed:?}");
    assert_eq!(renamed.old_text.as_deref(), Some(MOVED));
    assert_eq!(renamed.new_text.as_deref(), Some(MOVED));
    // pre.txt was untracked at turn start: only the base's private store
    // holds its old blob (the manifest below proves the checkout never did).
    let pre = turn_file_text(&client, &turn, &fx.checkout, "pre.txt").await;
    assert!(!pre.stale, "{pre:?}");
    assert_eq!(
        pre.old_text.as_deref(),
        Some(oracle_blob(&fx, &t0, "pre.txt").await.as_str())
    );

    let m1 = manifest(&fx.checkout).await;
    assert_untouched(&m0, &m1, "turn hook capture and reads");
    core.shutdown().await;
    write_artifact(
        "turn-hook-worktree",
        json!({
            "manifests": { "before": m0.summary(), "after": m1.summary() },
            "patch": { "expectedSha256": sha(&want.patch), "actualSha256": sha(&turn.patch) },
            "crashedStoreCollected": !crashed.exists(),
            "foreignStoreKept": foreign.exists(),
        }),
    );
}

// ---------------------------------------------------------------------------
// Budgets: consistent omissions and an honest partial result
// ---------------------------------------------------------------------------

fn write_many(root: &Path, dir: &str, range: std::ops::Range<usize>) {
    let dir = root.join(dir);
    std::fs::create_dir_all(&dir).expect("bulk dir");
    for i in range {
        std::fs::write(dir.join(format!("f{i}.txt")), format!("{i}\n")).expect("bulk file");
    }
}

/// Catches: unbounded hashing of an untracked dataset/build tree; a directory
/// crossing the budget mid-turn (or shrinking under it) fabricating mass
/// additions or deletions; an omission presented as a complete result; small
/// new source files hidden along with the large units.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn untracked_budget_omits_consistently_and_reports_partial() {
    let fx = fixture(Layout::Ordinary).await;
    // At turn start: one oversized file and one directory over the count
    // budget are omitted; `gen/` fits.
    std::fs::write(fx.checkout.join("big.bin"), vec![b'x'; 8 * 1024 * 1024 + 1]).expect("big file");
    write_many(&fx.checkout, "huge", 0..10_001);
    write_many(&fx.checkout, "gen", 0..10);
    let m0 = manifest(&fx.checkout).await;
    // The oracle leaves out the same units on both sides, including `gen/`,
    // which crosses the budget during the turn and so keeps its base state.
    let omit = ["big.bin", "huge/", "gen/"];
    let t0 = oracle_tree(&fx, Layout::Ordinary.forced(), &omit).await;

    let core = assemble(&fx.data, &fx.checkout);
    let client = luna_rpc::memory_client(core.rpc_service());
    let started = Instant::now();
    core.diff_sync
        .note_turn_start(CHAT, &fx.checkout.to_string_lossy());
    let clean = turn_diff(&client, &fx.checkout).await.expect("first read");
    let cold_ms = started.elapsed().as_millis() as u64;
    assert!(clean.files.is_empty(), "{:?}", clean.files);
    assert!(
        clean.truncated,
        "omitted units must make the result partial"
    );
    assert!(
        clean
            .patch
            .contains("# Luna turn snapshot omitted 2 path(s) over the snapshot budget"),
        "{}",
        clean.patch
    );

    // The turn: `gen/` grows past the budget (and an existing file in it
    // changes), `huge/` shrinks under it, the oversized file changes, and a
    // small source file plus a tracked edit are the real work.
    write_many(&fx.checkout, "gen", 10..10_001);
    append(&fx.checkout, "gen/f0.txt", "changed while over budget\n");
    for i in 0..6_000 {
        std::fs::remove_file(fx.checkout.join(format!("huge/f{i}.txt"))).expect("shrink huge");
    }
    let mut big = std::fs::read(fx.checkout.join("big.bin")).expect("big file");
    big.extend_from_slice(b"more");
    std::fs::write(fx.checkout.join("big.bin"), big).expect("big file");
    write(&fx.checkout, "src/new.rs", "pub fn added_this_turn() {}\n");
    append(&fx.checkout, "a.txt", "turn edit\n");

    let t1 = oracle_tree(&fx, Layout::Ordinary.forced(), &omit).await;
    let want = expected(&fx, &t0, &t1).await;
    let started = Instant::now();
    let turn = turn_diff(&client, &fx.checkout).await.expect("Latest turn");
    let warm_ms = started.elapsed().as_millis() as u64;
    assert!(turn.truncated);
    assert_matches_oracle(
        &turn,
        &want,
        Some("# Luna turn snapshot omitted 3 path(s) over the snapshot budget\n"),
    );
    assert_eq!(
        actual_files(&turn)
            .into_iter()
            .map(|(path, _, code)| (path, code))
            .collect::<Vec<_>>(),
        vec![("a.txt".to_string(), 'M'), ("src/new.rs".to_string(), 'A')],
    );
    let source = turn_file_text(&client, &turn, &fx.checkout, "src/new.rs").await;
    assert_eq!(source.old_text, None);
    assert_eq!(
        source.new_text.as_deref(),
        Some("pub fn added_this_turn() {}\n")
    );

    let m1 = manifest(&fx.checkout).await;
    assert_untouched(&m0, &m1, "budgeted capture and reads");
    core.shutdown().await;
    write_artifact(
        "untracked-budget",
        json!({
            "omittedAtStart": 2,
            "omittedInComparison": 3,
            "manifests": { "before": m0.summary(), "after": m1.summary() },
            "patch": { "expectedSha256": sha(&want.patch), "actualSha256": sha(&turn.patch) },
            "durationsMs": { "turnStartToFirstRead": cold_ms, "comparisonRead": warm_ms },
        }),
    );
}

// ---------------------------------------------------------------------------
// Cancellation, stale generations and reader lifetime (controlled filters)
// ---------------------------------------------------------------------------

/// A clean filter for `*.gate` files that records its pid and blocks until a
/// release file exists, plus a required `*.fail` filter that always fails.
/// Attributes live in `$GIT_DIR/info/attributes`, outside the working tree.
#[cfg(unix)]
struct Gate {
    release: PathBuf,
    pids: PathBuf,
}

#[cfg(unix)]
impl Gate {
    async fn install(fx: &Fixture) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let dir = fx.base.join("gate");
        std::fs::create_dir_all(&dir).expect("gate dir");
        let release = dir.join("release");
        let pids = dir.join("pids");
        let script = dir.join("gate.sh");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\necho $$ >> '{}'\nwhile [ ! -e '{}' ]; do sleep 0.05; done\nexec cat\n",
                pids.display(),
                release.display()
            ),
        )
        .expect("gate script");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
            .expect("gate script mode");
        let clean = format!("'{}'", script.display());
        git(&fx.checkout, &["config", "filter.gate.clean", &clean]).await;
        git(&fx.checkout, &["config", "filter.fail.clean", "false"]).await;
        git(&fx.checkout, &["config", "filter.fail.required", "true"]).await;
        let attributes = PathBuf::from(
            git(
                &fx.checkout,
                &[
                    "rev-parse",
                    "--path-format=absolute",
                    "--git-path",
                    "info/attributes",
                ],
            )
            .await,
        );
        std::fs::create_dir_all(attributes.parent().expect("info dir")).expect("info dir");
        std::fs::write(&attributes, "*.gate filter=gate\n*.fail filter=fail\n")
            .expect("attributes");
        Self { release, pids }
    }

    fn open(&self) {
        std::fs::write(&self.release, "").expect("open gate");
    }

    fn close(&self) {
        let _ = std::fs::remove_file(&self.release);
    }

    fn pids(&self) -> Vec<i32> {
        std::fs::read_to_string(&self.pids)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| line.trim().parse().ok())
            .collect()
    }

    async fn wait_for_pids(&self, count: usize) -> Vec<i32> {
        wait_until(&format!("{count} filter invocations"), || {
            self.pids().len() >= count
        })
        .await;
        self.pids()
    }
}

#[cfg(unix)]
fn alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

/// Catches: a failed capture, a cwd outside Git or a slower superseded
/// capture leaving (or later publishing) an earlier turn's base for the
/// newer turn; a superseded capture's hung clean filter surviving.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn turn_starts_never_answer_with_a_prior_base() {
    let fx = fixture(Layout::Ordinary).await;
    let gate = Gate::install(&fx).await;
    let m0 = manifest(&fx.checkout).await;
    let core = assemble(&fx.data, &fx.checkout);
    let client = luna_rpc::memory_client(core.rpc_service());
    let cwd = fx.checkout.to_string_lossy().into_owned();
    gate.open();

    core.diff_sync.note_turn_start(CHAT, &cwd);
    turn_diff(&client, &fx.checkout).await.expect("first base");
    write(
        &fx.checkout,
        "after-first.txt",
        "after the first turn started\n",
    );

    // The next turn starts outside any checkout.
    core.diff_sync
        .note_turn_start(CHAT, &fx.plain.to_string_lossy());
    let error = turn_diff(&client, &fx.checkout)
        .await
        .expect_err("must not fall back to the first turn");
    assert!(error.contains(UNAVAILABLE), "{error}");

    // A good turn, then one whose capture fails.
    core.diff_sync.note_turn_start(CHAT, &cwd);
    turn_diff(&client, &fx.checkout)
        .await
        .expect("good base again");
    write(&fx.checkout, "broken.fail", "its clean filter fails\n");
    core.diff_sync.note_turn_start(CHAT, &cwd);
    let error = turn_diff(&client, &fx.checkout)
        .await
        .expect_err("a failed capture must not leave the previous base");
    assert!(error.contains(UNAVAILABLE), "{error}");
    std::fs::remove_file(fx.checkout.join("broken.fail")).expect("remove failing file");

    // Overlapping starts: the first capture hangs in its clean filter and the
    // second supersedes it. Once released, a surviving first capture would
    // finish last and overwrite the newer base.
    gate.close();
    let before = gate.pids().len();
    write(&fx.checkout, "slow.gate", "slow\n");
    core.diff_sync.note_turn_start(CHAT, &cwd);
    let hung = gate.wait_for_pids(before + 1).await[before];
    std::fs::remove_file(fx.checkout.join("slow.gate")).expect("remove slow file");
    write(&fx.checkout, "between.txt", "between the two starts\n");
    let superseded = Instant::now();
    core.diff_sync.note_turn_start(CHAT, &cwd);
    wait_until("superseded capture's filter killed", || !alive(hung)).await;
    let killed_ms = superseded.elapsed().as_millis() as u64;
    gate.open();
    let base = turn_diff(&client, &fx.checkout).await.expect("second base");
    assert!(
        base.files.is_empty(),
        "between.txt belongs to the newest base: {base:?}"
    );
    write(&fx.checkout, "after.txt", "after the second start\n");
    let turn = turn_diff(&client, &fx.checkout).await.expect("Latest turn");
    assert_eq!(
        actual_files(&turn),
        vec![("after.txt".to_string(), None, 'A')],
        "the newest turn's base must answer"
    );

    let m1 = manifest(&fx.checkout).await;
    assert_untouched(&m0, &m1, "turn starts");
    core.shutdown().await;
    write_artifact(
        "stale-generations",
        json!({
            "supersededFilterPid": hung,
            "supersededFilterExited": !alive(hung),
            "killWithinMs": killed_ms,
            "manifests": { "before": m0.summary(), "after": m1.summary() },
        }),
    );
}

/// Catches: a hung Git child or clean filter surviving engine shutdown, its
/// half-written store left on disk, and a restart that cannot capture again.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hung_capture_dies_with_shutdown_and_restart_recovers() {
    let fx = fixture(Layout::Ordinary).await;
    let gate = Gate::install(&fx).await;
    write(&fx.checkout, "slow.gate", "slow\n");
    let m0 = manifest(&fx.checkout).await;

    let core = assemble(&fx.data, &fx.checkout);
    let cwd = fx.checkout.to_string_lossy().into_owned();
    core.diff_sync.note_turn_start(CHAT, &cwd);
    let hung = gate.wait_for_pids(1).await[0];
    assert!(alive(hung));
    let stopped = Instant::now();
    core.shutdown().await;
    wait_until("hung filter killed by shutdown", || !alive(hung)).await;
    let killed_ms = stopped.elapsed().as_millis() as u64;
    wait_until("cancelled capture's store removed", || {
        stores(&fx.data).is_empty()
    })
    .await;
    drop(core);

    gate.open();
    let core = assemble(&fx.data, &fx.checkout);
    let client = luna_rpc::memory_client(core.rpc_service());
    core.diff_sync.note_turn_start(CHAT, &cwd);
    let clean = turn_diff(&client, &fx.checkout)
        .await
        .expect("capture after restart");
    assert!(clean.files.is_empty(), "{clean:?}");
    write(&fx.checkout, "slow.gate", "slow\nchanged after restart\n");
    let turn = turn_diff(&client, &fx.checkout)
        .await
        .expect("Latest turn after restart");
    assert_eq!(
        actual_files(&turn),
        vec![("slow.gate".to_string(), None, 'M')]
    );

    let m1 = manifest(&fx.checkout).await;
    assert_untouched(&m0, &m1, "hung capture, shutdown and restart");
    core.shutdown().await;
    write_artifact(
        "hung-filter-shutdown",
        json!({
            "hungFilterPid": hung,
            "hungFilterExited": !alive(hung),
            "killWithinMs": killed_ms,
            "manifests": { "before": m0.summary(), "after": m1.summary() },
        }),
    );
}

/// Catches: a base store deleted while a Latest turn read still uses it
/// (the next turn replaced it), or kept after that read ends.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reader_keeps_a_replaced_base_until_it_finishes() {
    let fx = fixture(Layout::Ordinary).await;
    let gate = Gate::install(&fx).await;
    gate.open();
    write(&fx.checkout, "slow.gate", "slow\n");
    let m0 = manifest(&fx.checkout).await;
    let core = assemble(&fx.data, &fx.checkout);
    let client = Arc::new(luna_rpc::memory_client(core.rpc_service()));
    let cwd = fx.checkout.to_string_lossy().into_owned();

    core.diff_sync.note_turn_start(CHAT, &cwd);
    turn_diff(&client, &fx.checkout).await.expect("first base");
    wait_until("first read's comparison store removed", || {
        stores(&fx.data).len() == 1
    })
    .await;
    let first_base = stores(&fx.data).remove(0);

    append(&fx.checkout, "a.txt", "turn edit\n");
    gate.close();
    let before = gate.pids().len();
    let reader = tokio::spawn({
        let client = client.clone();
        let checkout = fx.checkout.clone();
        async move { turn_diff(&client, &checkout).await }
    });
    gate.wait_for_pids(before + 1).await; // the reader's comparison is inside the filter
    core.diff_sync.note_turn_start(CHAT, &cwd); // the next turn replaces the held base
    gate.wait_for_pids(before + 2).await; // the new base capture is blocked too
    assert!(
        first_base.exists(),
        "a replaced base was deleted while its reader ran"
    );

    gate.open();
    let held = reader
        .await
        .expect("reader task")
        .expect("the reader finishes against the base it started with");
    assert_eq!(actual_files(&held), vec![("a.txt".to_string(), None, 'M')]);
    wait_until("replaced base released after its reader", || {
        !first_base.exists()
    })
    .await;
    let fresh = turn_diff(&client, &fx.checkout).await.expect("new base");
    assert!(
        fresh.files.is_empty(),
        "the new turn's base includes the edit: {fresh:?}"
    );
    wait_until("one retained store", || stores(&fx.data).len() == 1).await;

    let m1 = manifest(&fx.checkout).await;
    assert_untouched(&m0, &m1, "held reader");
    core.shutdown().await;
    write_artifact(
        "held-reader",
        json!({
            "replacedBaseSurvivedReader": true,
            "replacedBaseReleased": !first_base.exists(),
            "manifests": { "before": m0.summary(), "after": m1.summary() },
        }),
    );
}
