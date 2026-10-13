//! End-to-end coverage for the workspace file RPC surface over the real
//! in-memory transport.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use luna_engine::{EngineCore, HarnessRegistry};
use luna_proto::{
    WorkspaceDirectoryPage, WorkspaceFileChanges, WorkspaceFileText, WriteWorkspaceFileOutcome,
};
use luna_rpc::methods;

async fn git(cwd: &Path, args: &[&str]) {
    let output = tokio::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@test")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@test")
        .output()
        .await
        .expect("git spawns");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

async fn init_repo(path: &Path) {
    std::fs::create_dir_all(path.join("src/nested")).expect("repo tree");
    git(path, &["init", "-b", "main"]).await;
    std::fs::write(path.join("README.md"), "hello\n").expect("readme");
    std::fs::write(path.join("src/lib.rs"), "pub fn answer() -> u8 { 42 }\n").expect("source");
    std::fs::write(path.join("src/nested/mod.rs"), "pub mod child;\n").expect("nested source");
    git(path, &["add", "."]).await;
    git(path, &["commit", "-m", "initial"]).await;
}

fn assemble(data_dir: &Path, device_id: &str) -> EngineCore {
    std::fs::create_dir_all(data_dir).expect("data dir");
    std::fs::write(data_dir.join("device-id"), device_id).expect("device id");
    EngineCore::assemble(
        data_dir,
        Arc::new(HarnessRegistry::new()),
        luna_proto::HarnessId::Mock,
        None,
    )
    .expect("engine assembles")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workspace_file_rpcs_list_search_read_write_and_watch() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = temp.path().join("repo");
    init_repo(&repo).await;
    let core = assemble(&temp.path().join("data"), "device-files");
    core.workspace
        .create_space(
            "space-files",
            &core.device_id,
            &repo.to_string_lossy(),
            None,
            true,
        )
        .expect("space");
    core.workspace
        .create_chat("chat-files", Some("space-files"), None, None, None)
        .expect("chat");
    let client = luna_rpc::memory_client(core.rpc_service());

    let root = client
        .call(
            methods::LIST_WORKSPACE_DIRECTORY,
            serde_json::json!({ "chatId": "chat-files" }),
        )
        .await
        .expect("list root");
    let root: WorkspaceDirectoryPage = serde_json::from_value(root).expect("typed root");
    assert_eq!(root.entries[0].path, "src");
    assert!(root.entries.iter().any(|entry| entry.path == "README.md"));
    assert!(!root.entries.iter().any(|entry| entry.path == ".git"));

    let nested = client
        .call(
            methods::LIST_WORKSPACE_DIRECTORY,
            serde_json::json!({ "chatId": "chat-files", "directory": "src" }),
        )
        .await
        .expect("list src");
    let nested: WorkspaceDirectoryPage = serde_json::from_value(nested).expect("typed src");
    assert!(
        nested
            .entries
            .iter()
            .any(|entry| entry.path == "src/lib.rs")
    );

    let matches = client
        .call(
            methods::SEARCH_WORKSPACE_FILES,
            serde_json::json!({ "chatId": "chat-files", "query": "lib" }),
        )
        .await
        .expect("search");
    assert_eq!(matches[0]["path"], "src/lib.rs");

    let read = client
        .call(
            methods::READ_WORKSPACE_FILE,
            serde_json::json!({ "chatId": "chat-files", "path": "README.md" }),
        )
        .await
        .expect("read");
    let read: WorkspaceFileText = serde_json::from_value(read).expect("typed read");
    assert_eq!(read.text.as_deref(), Some("hello\n"));
    let original_hash = read.content_hash.expect("content hash");
    let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="20"><rect width="20" height="20"/></svg>"#;
    std::fs::write(repo.join("example.svg"), svg).unwrap();
    let image = client.call(methods::READ_WORKSPACE_IMAGE, serde_json::json!({ "chatId": "chat-files", "path": "example.svg", "expectedCheckoutId": read.checkout_id, "offset": 0 })).await.expect("workspace image");
    assert_eq!(image["mimeType"], "image/svg+xml");
    assert_eq!(image["done"], true);
    assert_eq!(image["size"], svg.len());
    for (path, checkout) in [
        ("example.svg", "wrong-checkout"),
        ("../example.svg", read.checkout_id.as_str()),
        (".git/config", read.checkout_id.as_str()),
    ] {
        assert!(client.call(methods::READ_WORKSPACE_IMAGE, serde_json::json!({ "chatId": "chat-files", "path": path, "expectedCheckoutId": checkout, "offset": 0 })).await.is_err());
    }

    let mut watch = client
        .subscribe(
            methods::WATCH_WORKSPACE_FILES,
            serde_json::json!({ "chatId": "chat-files" }),
        )
        .await
        .expect("watch subscribe");
    let baseline = tokio::time::timeout(Duration::from_secs(3), watch.recv())
        .await
        .expect("baseline timeout")
        .expect("watch alive");
    let baseline: WorkspaceFileChanges = serde_json::from_value(baseline).expect("baseline shape");
    assert!(baseline.resync_required);

    let written = client
        .call(
            methods::WRITE_WORKSPACE_FILE,
            serde_json::json!({
                "chatId": "chat-files",
                "path": "README.md",
                "text": "updated\n",
                "expectedCheckoutId": read.checkout_id,
                "expectedContentHash": original_hash,
                "encoding": "utf8",
                "lineEnding": "lf",
            }),
        )
        .await
        .expect("write");
    let written: WriteWorkspaceFileOutcome =
        serde_json::from_value(written).expect("written shape");
    let new_hash = match written {
        WriteWorkspaceFileOutcome::Written { file } => file.content_hash,
        outcome => panic!("unexpected write outcome: {outcome:?}"),
    };
    assert_eq!(
        std::fs::read_to_string(repo.join("README.md")).unwrap(),
        "updated\n"
    );

    let conflict = client
        .call(
            methods::WRITE_WORKSPACE_FILE,
            serde_json::json!({
                "chatId": "chat-files",
                "path": "README.md",
                "text": "stale overwrite\n",
                "expectedCheckoutId": read.checkout_id,
                "expectedContentHash": "stale",
                "encoding": "utf8",
                "lineEnding": "lf",
            }),
        )
        .await
        .expect("conflict response");
    assert_eq!(conflict["status"], "conflict");
    assert_eq!(conflict["currentContentHash"], new_hash);
    assert_eq!(
        std::fs::read_to_string(repo.join("README.md")).unwrap(),
        "updated\n"
    );

    let concurrent_read = client
        .call(
            methods::READ_WORKSPACE_FILE,
            serde_json::json!({ "chatId": "chat-files", "path": "README.md" }),
        )
        .await
        .expect("read before concurrent writers");
    let concurrent_hash = concurrent_read["contentHash"]
        .as_str()
        .expect("concurrent hash")
        .to_string();
    let first_write = client.call(
        methods::WRITE_WORKSPACE_FILE,
        serde_json::json!({
            "chatId": "chat-files",
            "path": "README.md",
            "text": "concurrent\n",
            "expectedCheckoutId": read.checkout_id,
                "expectedContentHash": concurrent_hash.clone(),
            "encoding": "utf8",
            "lineEnding": "lf",
        }),
    );
    let second_write = client.call(
        methods::WRITE_WORKSPACE_FILE,
        serde_json::json!({
            "chatId": "chat-files",
            "path": "README.md",
            "text": "concurrent\n",
            "expectedCheckoutId": read.checkout_id,
                "expectedContentHash": concurrent_hash,
            "encoding": "utf8",
            "lineEnding": "lf",
        }),
    );
    let (first_write, second_write) = tokio::join!(first_write, second_write);
    let statuses = [
        first_write.expect("first concurrent writer")["status"]
            .as_str()
            .expect("first status")
            .to_string(),
        second_write.expect("second concurrent writer")["status"]
            .as_str()
            .expect("second status")
            .to_string(),
    ];
    assert_eq!(
        statuses
            .iter()
            .filter(|status| status.as_str() == "written")
            .count(),
        1
    );
    assert_eq!(
        statuses
            .iter()
            .filter(|status| status.as_str() == "conflict")
            .count(),
        1
    );

    std::fs::write(repo.join("created.txt"), "created\n").expect("external create");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let item = tokio::time::timeout_at(deadline, watch.recv())
            .await
            .expect("watch change timeout")
            .expect("watch alive");
        let item: WorkspaceFileChanges = serde_json::from_value(item).expect("change shape");
        if item
            .changes
            .iter()
            .any(|change| change.path == "created.txt")
        {
            break;
        }
    }

    assert!(
        client
            .call(
                methods::READ_WORKSPACE_FILE,
                serde_json::json!({ "chatId": "chat-files", "path": "../outside" }),
            )
            .await
            .is_err()
    );
    drop(watch);
    core.shutdown().await;
    core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workspace_file_rpcs_preserve_plain_folder_search_support() {
    let temp = tempfile::tempdir().expect("tempdir");
    let folder = temp.path().join("plain-folder");
    std::fs::create_dir_all(&folder).expect("plain folder");
    std::fs::write(folder.join("notes.txt"), "plain\n").expect("plain file");
    let core = assemble(&temp.path().join("plain-data"), "device-plain");
    core.workspace
        .create_space(
            "space-plain",
            &core.device_id,
            &folder.to_string_lossy(),
            None,
            false,
        )
        .expect("plain space");
    let client = luna_rpc::memory_client(core.rpc_service());

    let legacy = client
        .call(
            methods::SEARCH_FILES,
            serde_json::json!({ "spaceId": "space-plain", "query": "notes" }),
        )
        .await
        .expect("legacy SearchFiles on plain folder");
    assert_eq!(legacy[0]["path"], "notes.txt");
    let listing = client
        .call(
            methods::LIST_WORKSPACE_DIRECTORY,
            serde_json::json!({ "spaceId": "space-plain" }),
        )
        .await
        .expect("workspace listing on plain folder");
    assert!(
        listing["entries"]
            .as_array()
            .is_some_and(|entries| entries.iter().any(|entry| entry["path"] == "notes.txt"))
    );
    core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn write_rejects_changed_checkout_even_when_contents_match() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = temp.path().join("repo");
    let worktree = temp.path().join("worktree");
    init_repo(&repo).await;
    git(
        &repo,
        &["worktree", "add", "-b", "other", worktree.to_str().unwrap()],
    )
    .await;
    let core = assemble(&temp.path().join("data"), "device-files");
    core.workspace
        .create_space(
            "space-files",
            &core.device_id,
            repo.to_str().unwrap(),
            None,
            true,
        )
        .unwrap();
    core.workspace
        .create_chat("chat-files", Some("space-files"), None, None, None)
        .unwrap();
    let client = luna_rpc::memory_client(core.rpc_service());
    let read = client
        .call(
            methods::READ_WORKSPACE_FILE,
            serde_json::json!({
                "chatId": "chat-files", "path": "README.md"
            }),
        )
        .await
        .unwrap();
    assert!(!read["checkoutId"].as_str().unwrap().is_empty());
    core.workspace
        .set_chat_cwd("chat-files", worktree.to_str().unwrap())
        .unwrap();
    let other = client
        .call(
            methods::READ_WORKSPACE_FILE,
            serde_json::json!({
                "chatId": "chat-files", "path": "README.md"
            }),
        )
        .await
        .unwrap();
    assert_eq!(read["contentHash"], other["contentHash"]);
    assert_ne!(read["checkoutId"], other["checkoutId"]);
    let mut request = serde_json::json!({
        "chatId": "chat-files", "path": "README.md", "text": "pending original edit\n",
        "expectedContentHash": read["contentHash"], "expectedCheckoutId": read["checkoutId"],
        "encoding": "utf8", "lineEnding": "lf"
    });
    let error = client
        .call(methods::WRITE_WORKSPACE_FILE, request.clone())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("Workspace changed"), "{error}");
    request
        .as_object_mut()
        .unwrap()
        .remove("expectedCheckoutId");
    assert!(
        client
            .call(methods::WRITE_WORKSPACE_FILE, request.clone())
            .await
            .is_err()
    );
    assert_eq!(
        std::fs::read_to_string(repo.join("README.md")).unwrap(),
        "hello\n"
    );
    assert_eq!(
        std::fs::read_to_string(worktree.join("README.md")).unwrap(),
        "hello\n"
    );
    // Returning to the original checkout allows the preserved edit to be saved.
    core.workspace
        .set_chat_cwd("chat-files", repo.to_str().unwrap())
        .unwrap();
    request["expectedCheckoutId"] = read["checkoutId"].clone();
    let written = client
        .call(methods::WRITE_WORKSPACE_FILE, request)
        .await
        .unwrap();
    assert_eq!(written["status"], "written");
    assert_eq!(
        std::fs::read_to_string(repo.join("README.md")).unwrap(),
        "pending original edit\n"
    );
    assert_eq!(
        std::fs::read_to_string(worktree.join("README.md")).unwrap(),
        "hello\n"
    );
}

// ZI-05: bounded literal text search. Contract and measured budgets:
// docs/pending-tasks/zeron-upstream-improvements/zi-05/README.md.

use luna_proto::{WorkspaceTextSearchLimit, WorkspaceTextSearchResult, WorkspaceTextSearchStatus};

/// (path, 1-based line, byte start, byte end, 1-based character column).
type TextHit = (String, u32, u32, u32, u32);

const MAX_TEXT_FILE_BYTES: usize = 8 * 1024 * 1024;

/// Independent literal oracle written from the contract: a naive byte scan of
/// every eligible file, line by line, leftmost and non-overlapping. Shares no
/// code with the engine (no `ignore`, no `str::find`).
fn oracle(root: &Path, candidates: &[String], query: &str) -> Vec<TextHit> {
    let case_sensitive = query.chars().any(char::is_uppercase);
    let needle = query.as_bytes();
    let mut hits = Vec::new();
    for path in candidates {
        let full = root.join(path);
        let Ok(metadata) = std::fs::symlink_metadata(&full) else {
            continue;
        };
        let name = path.rsplit('/').next().unwrap();
        if !metadata.file_type().is_file()
            || metadata.len() as usize > MAX_TEXT_FILE_BYTES
            || (name.starts_with(".luna-save-") && name.ends_with(".tmp"))
        {
            continue;
        }
        let bytes = std::fs::read(&full).unwrap();
        if bytes.contains(&0) {
            continue;
        }
        let body = bytes
            .strip_prefix(&[0xef, 0xbb, 0xbf])
            .unwrap_or(bytes.as_slice());
        let Ok(text) = std::str::from_utf8(body) else {
            continue;
        };
        for (index, line) in text.split('\n').enumerate() {
            let line = line.strip_suffix('\r').unwrap_or(line).as_bytes();
            let mut at = 0;
            while at + needle.len() <= line.len() {
                let candidate = &line[at..at + needle.len()];
                let equal = if case_sensitive {
                    candidate == needle
                } else {
                    candidate.eq_ignore_ascii_case(needle)
                };
                if equal {
                    let column = std::str::from_utf8(&line[..at]).unwrap().chars().count() + 1;
                    hits.push((
                        path.clone(),
                        index as u32 + 1,
                        at as u32,
                        (at + needle.len()) as u32,
                        column as u32,
                    ));
                    at += needle.len();
                } else {
                    at += 1;
                }
            }
        }
    }
    hits.sort();
    hits
}

/// Git's own view of the non-ignored files: the external oracle for the
/// Files ignore semantics in a repository.
fn git_visible_files(root: &Path) -> Vec<String> {
    let output = std::process::Command::new("git")
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .current_dir(root)
        .output()
        .expect("git ls-files");
    assert!(output.status.success());
    output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| String::from_utf8(path.to_vec()).unwrap())
        .collect()
}

/// Every regular path under `root` without following symlinks or entering
/// `.git`: the oracle's view when ignored files are explicitly included.
fn all_files(root: &Path) -> Vec<String> {
    let mut files = Vec::new();
    let mut pending = vec![std::path::PathBuf::new()];
    while let Some(relative) = pending.pop() {
        for entry in std::fs::read_dir(root.join(&relative)).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = relative.join(&name);
            let kind = entry.file_type().unwrap();
            if kind.is_dir() && !name.eq_ignore_ascii_case(".git") {
                pending.push(path);
            } else if kind.is_file() {
                files.push(path.to_string_lossy().into_owned());
            }
        }
    }
    files
}

fn hits(result: &WorkspaceTextSearchResult) -> Vec<TextHit> {
    let mut hits: Vec<TextHit> = result
        .files
        .iter()
        .flat_map(|file| {
            file.matches.iter().map(|found| {
                (
                    file.path.clone(),
                    found.line,
                    found.byte_start,
                    found.byte_end,
                    found.column,
                )
            })
        })
        .collect();
    hits.sort();
    hits
}

async fn search_text(
    client: &luna_rpc::RpcClient,
    params: serde_json::Value,
) -> Result<WorkspaceTextSearchResult, luna_rpc::RpcError> {
    client
        .call(methods::SEARCH_WORKSPACE_TEXT, params)
        .await
        .map(|value| serde_json::from_value(value).expect("typed text search result"))
}

/// Each reported preview shows the matched bytes of the real line.
fn assert_previews_show_their_matches(root: &Path, result: &WorkspaceTextSearchResult) {
    for file in &result.files {
        let bytes = std::fs::read(root.join(&file.path)).unwrap();
        let body = bytes
            .strip_prefix(&[0xef, 0xbb, 0xbf])
            .unwrap_or(bytes.as_slice());
        let text = std::str::from_utf8(body).unwrap();
        for found in &file.matches {
            let line = text.split('\n').nth(found.line as usize - 1).unwrap();
            let line = line.strip_suffix('\r').unwrap_or(line);
            let matched = &line[found.byte_start as usize..found.byte_end as usize];
            let shown = &found.preview
                [found.preview_match_start as usize..found.preview_match_end as usize];
            assert!(
                matched.starts_with(shown) && !shown.is_empty(),
                "{}:{} preview {shown:?} vs {matched:?}",
                file.path,
                found.line
            );
            assert!(found.preview.len() <= 512, "unbounded preview");
        }
    }
}

/// Regressions caught: regex/shell interpretation of punctuation or `$(...)`;
/// trimmed or collapsed spaces and tabs; overlapping or missed repeats; byte
/// columns off after multibyte text, CRLF or a BOM; `.git`, ignored, binary,
/// non-UTF-8 or internal temp files searched; symlinks followed out of (or
/// within) the root; an answer cached past a deletion.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workspace_text_search_matches_an_independent_literal_oracle() {
    let temp = tempfile::tempdir().expect("tempdir");
    let base = temp.path().canonicalize().unwrap();
    let repo = base.join("repo");
    init_repo(&repo).await;
    let sentinel = base.join("shell-probe");
    let shell_query = format!("$(touch {})", sentinel.display());
    let unicode_dir = repo.join("src/unicode café");
    std::fs::create_dir_all(&unicode_dir).unwrap();
    std::fs::write(
        unicode_dir.join("naïve file.rs"),
        "// größe: Needle needle NEEDLE\nlet s = \"naïve 🦀 needle\";\tneedle\naaaa aa\n",
    )
    .unwrap();
    std::fs::create_dir_all(repo.join("docs")).unwrap();
    std::fs::write(
        repo.join("docs/notes.md"),
        "repeated needle needle needle\n  needle with two spaces\npunct a.b*c(d)[e]\\f{g}|h^$ needle\n",
    )
    .unwrap();
    std::fs::write(repo.join("crlf.txt"), "first needle\r\nsecond NEEDLE\r\n").unwrap();
    std::fs::write(repo.join("bom.txt"), "\u{feff}needle at the start\n").unwrap();
    std::fs::write(
        repo.join("long.min.js"),
        format!("{}needle{}", "x".repeat(100_000), "y".repeat(5_000)),
    )
    .unwrap();
    std::fs::write(repo.join("blob.bin"), b"needle\0binary").unwrap();
    std::fs::write(repo.join("latin1.txt"), b"needle \xe9t\xe9\n").unwrap();
    std::fs::write(repo.join("shell.txt"), format!("{shell_query}\n")).unwrap();
    std::fs::write(repo.join(".gitignore"), "ignored/\n*.log\n").unwrap();
    std::fs::create_dir_all(repo.join("ignored")).unwrap();
    std::fs::write(
        repo.join("ignored/secret.txt"),
        "needle in an ignored file\n",
    )
    .unwrap();
    std::fs::write(repo.join("debug.log"), "needle log\n").unwrap();
    std::fs::write(repo.join(".git/needle-probe"), "needle inside .git\n").unwrap();
    std::fs::write(repo.join(".luna-save-zi05.tmp"), "needle internal temp\n").unwrap();
    std::fs::write(repo.join("untracked.txt"), "an untracked needle\n").unwrap();
    std::fs::create_dir_all(base.join("outside")).unwrap();
    std::fs::write(base.join("outside/secret.txt"), "needle outside the root\n").unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(base.join("outside"), repo.join("escape")).unwrap();
        std::os::unix::fs::symlink(base.join("outside/secret.txt"), repo.join("link.txt")).unwrap();
        std::os::unix::fs::symlink(repo.join("docs/notes.md"), repo.join("alias.md")).unwrap();
    }
    git(&repo, &["add", "src", "docs", "crlf.txt", ".gitignore"]).await;
    git(&repo, &["commit", "-m", "text fixtures"]).await;

    let core = assemble(&temp.path().join("data"), "device-files");
    core.workspace
        .create_space(
            "space-files",
            &core.device_id,
            &repo.to_string_lossy(),
            None,
            true,
        )
        .expect("space");
    core.workspace
        .create_chat("chat-files", Some("space-files"), None, None, None)
        .expect("chat");
    let client = luna_rpc::memory_client(core.rpc_service());

    let visible = git_visible_files(&repo);
    for query in [
        "needle",
        "Needle",
        "a.b*c(d)[e]\\f{g}|h^$",
        "  needle",
        "\tneedle",
        "aa",
        "🦀 needle",
        "naïve",
        shell_query.as_str(),
    ] {
        let result = search_text(
            &client,
            serde_json::json!({ "chatId": "chat-files", "query": query }),
        )
        .await
        .unwrap_or_else(|error| panic!("{query:?}: {error}"));
        assert_eq!(result.query, query);
        assert_eq!(result.case_sensitive, query.chars().any(char::is_uppercase));
        assert_eq!(
            result.status,
            WorkspaceTextSearchStatus::Complete,
            "{query:?}: {result:?}"
        );
        assert!(result.limits.is_empty(), "{query:?}: {:?}", result.limits);
        let expected = oracle(&repo, &visible, query);
        assert!(!expected.is_empty(), "fixture has no {query:?}");
        assert_eq!(hits(&result), expected, "{query:?}");
        assert_eq!(result.match_count as usize, expected.len());
        assert_previews_show_their_matches(&repo, &result);
    }
    assert!(
        !sentinel.exists(),
        "a literal query executed a shell command"
    );

    let needle = search_text(
        &client,
        serde_json::json!({ "chatId": "chat-files", "query": "needle" }),
    )
    .await
    .unwrap();
    let paths: Vec<_> = needle.files.iter().map(|file| file.path.as_str()).collect();
    for absent in [
        "ignored/secret.txt",
        "debug.log",
        ".git/needle-probe",
        ".luna-save-zi05.tmp",
        "blob.bin",
        "latin1.txt",
        "link.txt",
        "alias.md",
        "escape/secret.txt",
    ] {
        assert!(!paths.contains(&absent), "{absent} was searched: {paths:?}");
    }
    assert!(needle.skipped_binary >= 2, "{needle:?}");
    let long = needle
        .files
        .iter()
        .find(|file| file.path == "long.min.js")
        .expect("long line match");
    assert_eq!(long.matches[0].byte_start, 100_000);
    assert!(long.matches[0].preview_truncated_before && long.matches[0].preview_truncated_after);
    let bom = needle
        .files
        .iter()
        .find(|file| file.path == "bom.txt")
        .unwrap();
    assert_eq!((bom.matches[0].line, bom.matches[0].byte_start), (1, 0));

    // The reported hash is the Files read hash: navigation can tell whether
    // the bytes it opens are the bytes that were searched.
    let read = client
        .call(
            methods::READ_WORKSPACE_FILE,
            serde_json::json!({ "chatId": "chat-files", "path": "crlf.txt" }),
        )
        .await
        .unwrap();
    let crlf = needle
        .files
        .iter()
        .find(|file| file.path == "crlf.txt")
        .unwrap();
    assert_eq!(read["contentHash"], crlf.content_hash.as_str());
    assert_eq!(read["checkoutId"], needle.checkout_id.as_str());
    std::fs::write(repo.join("crlf.txt"), "inserted\r\nfirst needle\r\n").unwrap();
    let changed = client
        .call(
            methods::READ_WORKSPACE_FILE,
            serde_json::json!({ "chatId": "chat-files", "path": "crlf.txt" }),
        )
        .await
        .unwrap();
    assert_ne!(changed["contentHash"], crlf.content_hash.as_str());
    std::fs::remove_file(repo.join("docs/notes.md")).unwrap();
    assert!(
        client
            .call(
                methods::READ_WORKSPACE_FILE,
                serde_json::json!({ "chatId": "chat-files", "path": "docs/notes.md" }),
            )
            .await
            .is_err()
    );
    let after = search_text(
        &client,
        serde_json::json!({ "chatId": "chat-files", "query": "needle" }),
    )
    .await
    .unwrap();
    assert_eq!(
        hits(&after),
        oracle(&repo, &git_visible_files(&repo), "needle")
    );
    assert!(!after.files.iter().any(|file| file.path == "docs/notes.md"));

    // Ignored files only with the Files surface's explicit "show all" scope;
    // `.git`, symlinks and internal temp files never.
    let all = search_text(
        &client,
        serde_json::json!({ "chatId": "chat-files", "query": "needle", "includeIgnored": true }),
    )
    .await
    .unwrap();
    assert_eq!(hits(&all), oracle(&repo, &all_files(&repo), "needle"));
    assert!(
        all.files
            .iter()
            .any(|file| file.path == "ignored/secret.txt")
    );
    assert!(!all.files.iter().any(|file| file.path.contains(".git/")
        || file.path == "link.txt"
        || file.path.starts_with("escape/")));
    core.shutdown().await;
}

/// Regressions caught: any budget, unreadable entry or skipped large text
/// file reported as a complete answer; a capped file losing its "more"
/// marker; binary files counted as coverage gaps.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workspace_text_search_reports_every_budget_as_partial() {
    let temp = tempfile::tempdir().expect("tempdir");
    let limits = temp.path().join("limits");
    let gaps = temp.path().join("gaps");
    std::fs::create_dir_all(&limits).unwrap();
    std::fs::create_dir_all(&gaps).unwrap();
    std::fs::write(
        limits.join("crowded.txt"),
        (0..25)
            .map(|line| format!("needle {line}\n"))
            .collect::<String>(),
    )
    .unwrap();
    for name in ["a.txt", "b.txt", "c.txt"] {
        std::fs::write(limits.join(name), "one needle\n").unwrap();
    }
    let mut large = "x".repeat(MAX_TEXT_FILE_BYTES);
    large.push_str("needle");
    std::fs::write(gaps.join("huge.txt"), large).unwrap();
    let mut binary = vec![0u8; MAX_TEXT_FILE_BYTES + 1024];
    binary.extend_from_slice(b"needle");
    std::fs::write(gaps.join("huge.bin"), binary).unwrap();
    std::fs::write(gaps.join("ok.txt"), "needle\n").unwrap();
    #[cfg(unix)]
    let locked = {
        use std::os::unix::fs::PermissionsExt;
        let path = gaps.join("locked.txt");
        std::fs::write(&path, "needle\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        // Root reads through permissions; the unreadable case is then moot.
        std::fs::read(&path).is_err()
    };
    #[cfg(not(unix))]
    let locked = false;

    let core = assemble(&temp.path().join("data"), "device-limits");
    for (id, path) in [("space-limits", &limits), ("space-gaps", &gaps)] {
        core.workspace
            .create_space(id, &core.device_id, &path.to_string_lossy(), None, false)
            .expect("plain space");
    }
    let client = luna_rpc::memory_client(core.rpc_service());
    let call = |params: serde_json::Value| search_text(&client, params);

    let absent = call(serde_json::json!({ "spaceId": "space-limits", "query": "zz-absent" }))
        .await
        .unwrap();
    assert_eq!(absent.status, WorkspaceTextSearchStatus::Complete);
    assert_eq!((absent.match_count, absent.files_scanned), (0, 4));

    let elapsed = call(serde_json::json!({
        "spaceId": "space-limits", "query": "zz-absent", "budget": { "maxElapsedMs": 0 },
    }))
    .await
    .unwrap();
    assert_eq!(elapsed.status, WorkspaceTextSearchStatus::Partial);
    assert_eq!(elapsed.limits, vec![WorkspaceTextSearchLimit::Elapsed]);
    assert_eq!((elapsed.match_count, elapsed.files_scanned), (0, 0));

    let crowded = call(serde_json::json!({ "spaceId": "space-limits", "query": "needle" }))
        .await
        .unwrap();
    assert_eq!(crowded.status, WorkspaceTextSearchStatus::Partial);
    assert_eq!(crowded.limits, vec![WorkspaceTextSearchLimit::FileMatches]);
    let capped = crowded
        .files
        .iter()
        .find(|file| file.path == "crowded.txt")
        .unwrap();
    assert_eq!(capped.matches.len(), 20);
    assert!(capped.matches_truncated);
    assert_eq!(crowded.match_count, 23);

    for (budget, limit) in [
        (
            serde_json::json!({ "limit": 2 }),
            WorkspaceTextSearchLimit::Results,
        ),
        (
            serde_json::json!({ "budget": { "maxFiles": 1 } }),
            WorkspaceTextSearchLimit::Files,
        ),
        (
            serde_json::json!({ "budget": { "maxBytes": 4 } }),
            WorkspaceTextSearchLimit::Bytes,
        ),
    ] {
        let mut params = serde_json::json!({ "spaceId": "space-limits", "query": "one needle" });
        params
            .as_object_mut()
            .unwrap()
            .extend(budget.as_object().unwrap().clone());
        let result = call(params).await.unwrap();
        assert_eq!(
            result.status,
            WorkspaceTextSearchStatus::Partial,
            "{limit:?}"
        );
        assert!(
            result.limits.contains(&limit),
            "{limit:?}: {:?}",
            result.limits
        );
        assert!(result.match_count < 3, "{limit:?} returned every match");
    }

    let gapped = call(serde_json::json!({ "spaceId": "space-gaps", "query": "needle" }))
        .await
        .unwrap();
    assert_eq!(gapped.status, WorkspaceTextSearchStatus::Partial);
    assert!(
        gapped
            .limits
            .contains(&WorkspaceTextSearchLimit::LargeFiles)
    );
    assert_eq!(gapped.skipped_large, 1);
    assert_eq!(gapped.skipped_binary, 1, "huge.bin is binary, not a gap");
    assert_eq!(
        gapped
            .limits
            .contains(&WorkspaceTextSearchLimit::Unreadable),
        locked
    );
    assert_eq!(
        gapped
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>(),
        vec!["ok.txt"]
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            gaps.join("locked.txt"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
    }
    core.shutdown().await;
}

/// Regressions caught: a late older generation scanning and answering as if
/// current; a superseded scan reported as a complete or partial answer;
/// unbounded session keys; malformed literal queries accepted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workspace_text_search_fences_generations_and_rejects_bad_requests() {
    let temp = tempfile::tempdir().expect("tempdir");
    let folder = temp.path().join("many");
    std::fs::create_dir_all(&folder).unwrap();
    let filler = "filler text without the literal\n".repeat(128);
    for index in 0..3_000 {
        let contents = if index % 300 == 0 {
            format!("{filler}needle {index}\n")
        } else {
            filler.clone()
        };
        std::fs::write(folder.join(format!("file-{index:04}.txt")), contents).unwrap();
    }
    let core = assemble(&temp.path().join("data"), "device-fence");
    core.workspace
        .create_space(
            "space-many",
            &core.device_id,
            &folder.to_string_lossy(),
            None,
            false,
        )
        .expect("plain space");
    let client = luna_rpc::memory_client(core.rpc_service());
    let request = |generation: u64| {
        serde_json::json!({
            "spaceId": "space-many", "query": "needle",
            "searchSession": "files-surface-a", "generation": generation,
        })
    };

    let current = search_text(&client, request(5)).await.unwrap();
    assert_eq!(current.status, WorkspaceTextSearchStatus::Complete);
    assert_eq!(current.match_count, 10);
    let late = search_text(&client, request(4)).await.unwrap();
    assert_eq!(late.status, WorkspaceTextSearchStatus::Superseded);
    assert_eq!((late.files_scanned, late.files.len()), (0, 0));
    assert_eq!(late.generation, 4);

    // A newer generation replaces a running scan. Whichever way the race
    // goes, the older answer is never a short "complete" or "partial" one.
    let older = {
        let client = luna_rpc::memory_client(core.rpc_service());
        tokio::spawn(async move { search_text(&client, request(10)).await })
    };
    tokio::task::yield_now().await;
    let newer = search_text(&client, request(11)).await.unwrap();
    assert_eq!(newer.status, WorkspaceTextSearchStatus::Complete);
    assert_eq!(newer.match_count, 10);
    let older = older.await.unwrap().unwrap();
    match older.status {
        WorkspaceTextSearchStatus::Superseded => assert!(older.files.is_empty()),
        WorkspaceTextSearchStatus::Complete => assert_eq!(hits(&older), hits(&newer)),
        other => panic!("superseded scan answered {other:?}: {older:?}"),
    }
    // Another session is independent of this one's generations.
    let other = search_text(
        &client,
        serde_json::json!({
            "spaceId": "space-many", "query": "needle",
            "searchSession": "files-surface-b", "generation": 1,
        }),
    )
    .await
    .unwrap();
    assert_eq!(other.status, WorkspaceTextSearchStatus::Complete);

    for params in [
        serde_json::json!({ "spaceId": "space-many", "query": "" }),
        serde_json::json!({ "spaceId": "space-many", "query": "two\nlines" }),
        serde_json::json!({ "spaceId": "space-many", "query": "carriage\rreturn" }),
        serde_json::json!({ "spaceId": "space-many", "query": "nul\u{0}" }),
        serde_json::json!({ "spaceId": "space-many", "query": "q".repeat(257) }),
        serde_json::json!({ "spaceId": "space-many", "query": "needle", "searchSession": "" }),
        serde_json::json!({
            "spaceId": "space-many", "query": "needle", "searchSession": "s".repeat(129),
        }),
    ] {
        let error = search_text(&client, params.clone()).await.unwrap_err();
        assert!(
            matches!(error, luna_rpc::RpcError::BadParams(_)),
            "{params}: {error:?}"
        );
    }
    core.shutdown().await;
}

/// Regressions caught: text search reading a chat or space that belongs to
/// another device, a missing chat, or a checkout path outside the project,
/// instead of the Files read authorization.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workspace_text_search_shares_files_read_authorization() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = temp.path().join("repo");
    init_repo(&repo).await;
    let outside = temp.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), "needle outside\n").unwrap();
    let core = assemble(&temp.path().join("data"), "device-auth");
    core.workspace
        .create_space(
            "space-local",
            &core.device_id,
            &repo.to_string_lossy(),
            None,
            true,
        )
        .unwrap();
    core.workspace
        .create_space(
            "space-other",
            "device-other",
            &outside.to_string_lossy(),
            None,
            false,
        )
        .unwrap();
    core.workspace
        .create_chat(
            "chat-other",
            Some("space-other"),
            Some("device-other"),
            None,
            None,
        )
        .unwrap();
    let client = luna_rpc::memory_client(core.rpc_service());
    for (params, expected) in [
        (
            serde_json::json!({ "chatId": "chat-other", "query": "needle" }),
            "another device",
        ),
        (
            serde_json::json!({ "spaceId": "space-other", "query": "needle" }),
            "another device",
        ),
        (
            serde_json::json!({ "chatId": "chat-missing", "query": "needle" }),
            "not found",
        ),
        (
            serde_json::json!({
                "spaceId": "space-local", "checkoutPath": outside.to_string_lossy(),
                "query": "needle",
            }),
            "not a workspace checkout",
        ),
    ] {
        let error = search_text(&client, params.clone()).await.unwrap_err();
        assert!(error.to_string().contains(expected), "{params}: {error}");
    }
    core.shutdown().await;
}
