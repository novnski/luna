//! The shipped POSIX remote helper against a disposable HOME and dummy binary.

#![cfg(unix)]

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};

use sha2::{Digest, Sha256};
use tempfile::TempDir;

const SCRIPT: &str = include_str!("../src/ssh_remote.sh");

fn run(home: &std::path::Path, mode: &str, sha: &str, owner: &str) -> std::process::Output {
    let mut child = Command::new("sh")
        .args([
            "-s",
            "--",
            mode,
            "0.2.73",
            sha,
            "37655",
            "37654",
            "test-key",
            owner,
            "30303030-3030-4030-8030-303030303030",
            &"0".repeat(64),
            "development",
        ])
        .env("HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(SCRIPT.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn staged_checksum_version_reuse_and_owned_stop() {
    let home = TempDir::new().unwrap();
    let base = home.path().join(".luna/runtime/versions/0.2.73");
    let owner = "b2a7a5aa-caae-4f70-8851-322eaa830df5";
    let other = "21bb8a5d-64d6-43a4-b781-8c55e7f3f040";
    let dummy = "#!/bin/sh\nif [ \"$1\" = '--version' ]; then echo 'luna-headless 0.2.73'; exit; fi\nwhile :; do sleep 1; done\n".to_owned();
    let sha = format!("{:x}", Sha256::digest(dummy.as_bytes()));
    assert_eq!(
        String::from_utf8(run(home.path(), "inspect", &sha, owner).stdout)
            .unwrap()
            .trim(),
        "missing"
    );
    assert!(
        !base.exists(),
        "inspection must not create a runtime directory"
    );
    fs::create_dir_all(&base).unwrap();
    let stage = base.join(format!(".incoming-{owner}"));
    fs::write(&stage, &dummy).unwrap();
    let bad = run(home.path(), "install", &"0".repeat(64), owner);
    assert!(!bad.status.success());
    assert!(String::from_utf8_lossy(&bad.stderr).contains("checksum mismatch"));
    assert!(!base.join("luna-headless").exists());
    fs::write(&stage, &dummy).unwrap();
    let installed = run(home.path(), "install", &sha, owner);
    assert!(
        installed.status.success(),
        "{}",
        String::from_utf8_lossy(&installed.stderr)
    );
    assert_eq!(
        fs::metadata(base.join("luna-headless"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        String::from_utf8(run(home.path(), "inspect", &sha, owner).stdout)
            .unwrap()
            .trim(),
        "installed"
    );
    let mismatch = run(home.path(), "inspect", &"1".repeat(64), owner);
    assert!(!mismatch.status.success());
    assert!(String::from_utf8_lossy(&mismatch.stderr).contains("version or checksum mismatch"));

    let started = run(home.path(), "start", &sha, owner);
    assert!(
        started.status.success(),
        "{}",
        String::from_utf8_lossy(&started.stderr)
    );
    assert!(String::from_utf8_lossy(&started.stdout).contains("kind=owned"));
    let reused = run(home.path(), "start", &sha, other);
    assert!(
        reused.status.success(),
        "{}",
        String::from_utf8_lossy(&reused.stderr)
    );
    assert!(String::from_utf8_lossy(&reused.stdout).contains("kind=reused"));
    let refused = run(home.path(), "stop", &sha, other);
    assert!(!refused.status.success());
    assert!(home.path().join(".luna/ssh-launch/test-key/owner").exists());
    let stamp_path = home.path().join(".luna/ssh-launch/test-key/stamp");
    let original_stamp = fs::read_to_string(&stamp_path).unwrap();
    fs::write(&stamp_path, "a different process start\n").unwrap();
    let replaced_pid = run(home.path(), "stop", &sha, owner);
    assert!(
        !replaced_pid.status.success(),
        "PID reuse must not be killed"
    );
    assert!(home.path().join(".luna/ssh-launch/test-key/owner").exists());
    fs::write(&stamp_path, original_stamp).unwrap();
    let stopped = run(home.path(), "stop", &sha, owner);
    assert!(
        stopped.status.success(),
        "{}",
        String::from_utf8_lossy(&stopped.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&stopped.stdout).trim(), "stopped");
    assert!(!home.path().join(".luna/ssh-launch/test-key/owner").exists());
    fs::write(base.join("luna-headless"), format!("{dummy}# modified\n")).unwrap();
    let tampered = run(home.path(), "inspect", &sha, owner);
    assert!(!tampered.status.success());
    assert!(String::from_utf8_lossy(&tampered.stderr).contains("version or checksum mismatch"));
}
