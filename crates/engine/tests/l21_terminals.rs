//! Synthetic PTYs and isolated profile roots only; no user process discovery.

#![cfg(unix)]

use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use luna_engine::Terminals;
use luna_proto::{TerminalEvent, TerminalProcessState};

async fn until_output(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<TerminalEvent>,
    needle: &str,
) -> u64 {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut seen = Vec::new();
    loop {
        let event = tokio::time::timeout_at(deadline, rx.recv())
            .await
            .unwrap()
            .unwrap();
        if let TerminalEvent::Data { seq, data } = event {
            seen.extend(BASE64.decode(data).unwrap());
            if String::from_utf8_lossy(&seen).contains(needle) {
                return seq;
            }
        }
    }
}

fn send(terms: &Terminals, id: &str, command: &str) {
    terms
        .write(id, &BASE64.encode(format!("{command}\r")))
        .unwrap();
}

async fn until_exit(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<TerminalEvent>,
    expected_signal: &str,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let event = tokio::time::timeout_at(deadline, rx.recv())
            .await
            .unwrap()
            .unwrap();
        if let TerminalEvent::Exit { signal, .. } = event {
            assert_eq!(signal.as_deref(), Some(expected_signal));
            break;
        }
    }
    assert!(rx.recv().await.is_none());
}

#[tokio::test]
async fn detach_reconnect_close_and_saved_output() {
    let root = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let terms = Terminals::open_with_history(root.path(), "fixture-device").unwrap();
    let terminal = terms
        .open_with_shell_for_task(
            "task-a",
            cwd.path().to_str().unwrap(),
            80,
            24,
            Some("/bin/sh"),
        )
        .unwrap();
    let mut first = terms.subscribe(&terminal.id, None).unwrap();
    send(&terms, &terminal.id, "printf L21FIRST");
    let first_seq = until_output(&mut first, "L21FIRST").await;
    drop(first); // viewer detach must not kill the PTY
    assert_eq!(
        terms.list(Some("task-a")).unwrap()[0].state,
        TerminalProcessState::Running
    );

    let mut resumed = terms.subscribe(&terminal.id, Some(first_seq)).unwrap();
    send(&terms, &terminal.id, "printf L21SECOND");
    let second_seq = until_output(&mut resumed, "L21SECOND").await;
    assert!(second_seq > first_seq);
    let pid_path = cwd.path().join("synthetic-shell.pid");
    send(
        &terms,
        &terminal.id,
        &format!("echo $$ > {}", pid_path.display()),
    );
    let shell_pid = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(contents) = std::fs::read_to_string(&pid_path) {
                break contents.trim().parse::<i32>().unwrap();
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    terms.close(&terminal.id).unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let result = unsafe { libc::kill(shell_pid, 0) };
            if result == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("only the synthetic shell should terminate on close");
    let item = &terms.list(Some("task-a")).unwrap()[0];
    assert_eq!(item.state, TerminalProcessState::Stopped);
    assert_eq!(item.id, terminal.id);
    assert!(
        terms
            .write(&terminal.id, &BASE64.encode("whoami\r"))
            .is_err()
    );
    drop(terms);

    let restored = Terminals::open_with_history(root.path(), "fixture-device").unwrap();
    assert_eq!(
        restored.list(Some("task-a")).unwrap()[0].state,
        TerminalProcessState::Stopped
    );
    let mut output = restored.subscribe(&terminal.id, None).unwrap();
    let _ = until_output(&mut output, "L21FIRST").await;
    let _ = until_output(&mut output, "L21SECOND").await;
    until_exit(&mut output, "closed").await;
}

#[tokio::test]
async fn shutdown_interrupts_history_and_task_delete_is_scoped() {
    let root = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let terms = Terminals::open_with_history(root.path(), "fixture-device").unwrap();
    let path = cwd.path().to_str().unwrap();
    let first = terms
        .open_with_shell_for_task("task-a", path, 80, 24, Some("/bin/sh"))
        .unwrap();
    let second = terms
        .open_with_shell_for_task("task-b", path, 80, 24, Some("/bin/sh"))
        .unwrap();
    assert_eq!(terms.delete_task("task-a").unwrap(), 1);
    assert!(terms.list(Some("task-a")).unwrap().is_empty());
    assert_eq!(
        terms.list(Some("task-b")).unwrap()[0].state,
        TerminalProcessState::Running
    );
    assert!(terms.subscribe(&first.id, None).is_err());
    terms.shutdown();
    drop(terms);

    let restored = Terminals::open_with_history(root.path(), "fixture-device").unwrap();
    assert_eq!(
        restored.list(Some("task-b")).unwrap()[0].state,
        TerminalProcessState::Interrupted
    );
    assert!(restored.write(&second.id, &BASE64.encode("x")).is_err());
    let mut saved = restored.subscribe(&second.id, None).unwrap();
    until_exit(&mut saved, "engineExit").await;
}
