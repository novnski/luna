//! Live probe for OC-07/OC-08 against a real OpenCode 2 binary: turn
//! correlation and receipts, native mid-turn steering, and SSE-drop
//! recovery. Prints every event (receipts included) and exits 0 only when
//! the mode's acceptance holds.
//!
//!     opencode_steer_probe <opencode-exe> <provider/model> <mode>
//!
//! Modes:
//! - `plain`: one turn; expects prompt accepted, inbox enqueued + delivered
//!   for the turn's own caller id, Done Completed with text.
//! - `steer`: a turn running a slow shell command; on its first tool call
//!   the probe sends an explicit steer (RequireActiveTurn) asking for the
//!   word KIWI. Expects Accepted, a `Steered` boundary carrying the steer's
//!   LUNA id (its inbox delivery), KIWI in the reply and Done Completed.
//! - `stop-steer`: as `steer`, but Stop is pressed as soon as the steer is
//!   enqueued (it waits for the running tool's step boundary). Expects Done
//!   Interrupted, the steer withdrawn from the inbox and no `Steered`.
//! - `drop`: the harness talks to the server through an in-probe proxy that
//!   injects the Basic credential; on the first tool call the proxy cuts
//!   the SSE stream and holds `/api/event` reconnects (no response) for
//!   `PROBE_GAP_MS` (default 20000) so later frames land in the gap.
//!   Expects a reconnect receipt, the tool result, the reply text exactly
//!   once and Done Completed. (With approvals in Ask mode the gap swallows
//!   the shell permission ask: the reconnect resync must re-show it.)
//! - `drop-late`: as `drop`, but the cut follows the first tool RESULT, so
//!   the final reply and the execution terminal are durable frames lost in
//!   the gap: the reconnect must detect the gap and read them back.
//!
//! The probe owns the `opencode serve --stdio --port 0` child in `drop`
//! mode (the harness is attached through the proxy); otherwise the harness
//! spawns it. Run it under an isolated HOME/XDG (the evidence runner).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use base64::Engine as _;
use futures::StreamExt;
use luna_harness::{
    CancellationToken, Harness, OpencodeHarness, RunControls, SteerAcceptance, SteerDelivery,
    SteerMessage,
};
use luna_proto::{AgentEvent, DoneStatus, RunRequest, SandboxLevel};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot, watch};

const STEER_ID: &str = "probe-steer-1";
const FIRST_ID: &str = "probe-first-1";

#[tokio::main]
async fn main() {
    let exe = std::env::args().nth(1).expect("arg 1: opencode executable");
    let model = std::env::args().nth(2).expect("arg 2: provider/model");
    let mode = std::env::args().nth(3).unwrap_or_else(|| "plain".into());
    let workspace = tempfile::tempdir().expect("isolated probe workspace");
    let cwd = workspace.path().to_str().expect("UTF-8 path").to_owned();
    let prompt = match mode.as_str() {
        "plain" => "Reply with exactly: PONG".to_owned(),
        "steer" | "stop-steer" => {
            "Use the shell tool to run `sleep 6 && echo TICK`, then reply with one short \
              sentence that includes the word TICK."
                .to_owned()
        }
        _ => "Use the shell tool to run `sleep 2 && echo TICK`, then reply with one short \
              sentence that includes the word TICK."
            .to_owned(),
    };
    let mut model_options = serde_json::Map::new();
    // The engine's first-message id, as the engine sends it.
    model_options.insert("lunaClientUserMessageId".into(), FIRST_ID.into());
    let request = RunRequest {
        review: None,
        goal: None,
        input_items: Vec::new(),
        prompt,
        harness: None,
        provider_account_id: None,
        model: Some(model),
        reasoning: None,
        model_options,
        cwd: cwd.clone(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: true,
        permission_mode: luna_proto::PermissionMode::Full,
        parent_permission_ceiling: None,
        environment_permission_ceiling: None,
        permission_policy: None,
        attachments: Vec::new(),
        resume: None,
        worktree: None,
    };

    let (cut, _cut_rx) = watch::channel(false);
    let mut owned_server: Option<tokio::process::Child> = None;
    let harness = if mode.starts_with("drop") {
        let (child, upstream, auth) = spawn_server(&exe, &cwd).await;
        owned_server = Some(child);
        let gap = std::env::var("PROBE_GAP_MS")
            .ok()
            .and_then(|ms| ms.parse().ok())
            .unwrap_or(20_000u64);
        let proxy = start_proxy(upstream, auth, cut.clone(), Duration::from_millis(gap)).await;
        eprintln!("--- proxy {proxy} (SSE gap {gap} ms on the first tool call)");
        OpencodeHarness::new().with_base_url(proxy)
    } else {
        OpencodeHarness::new().with_executable(exe)
    };

    let (steer_tx, steering) = mpsc::channel(8);
    let interrupt = CancellationToken::new();
    let started = Instant::now();
    let mut stream = harness
        .run(
            request,
            RunControls {
                request_input: Box::new(|_| panic!("probe must not ask for input")),
                steering,
                interrupt: interrupt.clone(),
                invoke_luna_tool: None,
            },
        )
        .await
        .expect("run starts");

    let mut text = String::new();
    let mut tool_calls = 0u32;
    let mut tool_results = 0u32;
    let mut steered_exact = false;
    let mut steer_acceptance: Option<oneshot::Receiver<SteerAcceptance>> = None;
    let mut receipts: Vec<String> = Vec::new();
    let mut reconnected = false;
    let status = loop {
        if let Some(rx) = steer_acceptance.as_mut()
            && let Ok(result) = rx.try_recv()
        {
            eprintln!("STEER-ACCEPTANCE {result:?} after {:?}", started.elapsed());
            receipts.push(format!("steer-acceptance:{result:?}"));
            steer_acceptance = None;
        }
        let ev = match tokio::time::timeout(Duration::from_secs(180), stream.next()).await {
            Ok(Some(ev)) => ev,
            Ok(None) => break None,
            Err(_) => {
                eprintln!("--- timed out");
                std::process::exit(2);
            }
        };
        match ev {
            Ok(AgentEvent::Done { status, error, .. }) => {
                if let Some(error) = error {
                    eprintln!("ERR {error}");
                }
                break Some(status);
            }
            Ok(AgentEvent::TextDelta { text: delta }) => {
                text.push_str(&delta);
                eprintln!("TXT {delta}");
            }
            Ok(AgentEvent::ReasoningDelta { text }) => eprintln!("THK {}", text.trim_end()),
            Ok(AgentEvent::ToolCall { id, call }) => {
                tool_calls += 1;
                eprintln!("TOOL {id} {call:?}");
                if tool_calls == 1 && (mode == "steer" || mode == "stop-steer") {
                    let (tx, rx) = oneshot::channel();
                    steer_acceptance = Some(rx);
                    eprintln!("--- sending steer {STEER_ID}");
                    steer_tx
                        .send(SteerMessage {
                            prompt: "Also end your reply with the word KIWI.".into(),
                            attachments: Vec::new(),
                            input_items: Vec::new(),
                            message_id: Some(STEER_ID.into()),
                            delivery: SteerDelivery::RequireActiveTurn,
                            acceptance: Some(tx),
                            goal: None,
                        })
                        .await
                        .expect("steering mailbox open");
                }
                if tool_calls == 1 && mode == "drop" {
                    eprintln!("--- cutting the SSE stream");
                    let _ = cut.send(true);
                }
            }
            Ok(AgentEvent::ToolResult {
                id,
                is_error,
                output,
                ..
            }) => {
                tool_results += 1;
                if tool_results == 1 && mode == "drop-late" {
                    eprintln!("--- cutting the SSE stream after the tool result");
                    let _ = cut.send(true);
                }
                eprintln!(
                    "RESULT {id} error={is_error} {:?}",
                    output
                        .as_deref()
                        .map(|o| o.chars().take(80).collect::<String>())
                );
            }
            Ok(AgentEvent::Steered {
                user_message_id, ..
            }) => {
                eprintln!("STEERED user_message_id={user_message_id:?}");
                steered_exact |= user_message_id.as_deref() == Some(STEER_ID);
            }
            Ok(AgentEvent::ProviderEvidence {
                kind,
                native_id,
                payload,
                ..
            }) => {
                eprintln!("RECEIPT {kind} {native_id:?} {payload}");
                reconnected |= kind == "native:bus.reconnected";
                if mode == "stop-steer"
                    && kind == "native:inbox.enqueued"
                    && payload.get("owner").and_then(|v| v.as_str()) == Some("steer")
                {
                    // The steer waits for the running tool's step boundary:
                    // Stop now must withdraw it, never deliver it later.
                    eprintln!("--- pressing Stop with the steer pending");
                    interrupt.cancel();
                }
                receipts.push(format!(
                    "{kind}:{}:{}",
                    payload.get("owner").and_then(|v| v.as_str()).unwrap_or("-"),
                    payload
                        .get("receipt")
                        .and_then(|v| v.as_str())
                        .unwrap_or("-")
                ));
            }
            Ok(AgentEvent::Error { message }) => eprintln!("CHIP {message}"),
            Ok(AgentEvent::ProviderNotice { notice }) => eprintln!("NOTICE {notice:?}"),
            Ok(other) => eprintln!("EV {other:?}"),
            Err(e) => eprintln!("ERR {e}"),
        }
    };
    eprintln!(
        "--- done: {status:?} text={text:?} tools={tool_calls} results={tool_results} \
         steered_exact={steered_exact} reconnected={reconnected} ({:?})",
        started.elapsed()
    );
    let drained = tokio::time::timeout(Duration::from_secs(15), async {
        while stream.next().await.is_some() {}
    })
    .await;
    eprintln!("--- stream closed after teardown: {}", drained.is_ok());
    eprintln!("--- receipts: {receipts:?}");
    if let Some(mut child) = owned_server.take() {
        drop(child.stdin.take());
        let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
        let _ = child.kill().await;
    }
    let own = |kind: &str, receipt: &str| {
        receipts
            .iter()
            .any(|r| r == &format!("native:{kind}:turn:{receipt}"))
    };
    let completed = status == Some(DoneStatus::Completed) && !text.trim().is_empty();
    let ok = match mode.as_str() {
        "plain" => {
            completed
                && receipts.iter().any(|r| r.starts_with("native:prompt:"))
                && own("inbox.enqueued", "accepted")
                && own("inbox.delivered", "recorded")
        }
        "steer" => {
            completed
                && steered_exact
                && receipts.iter().any(|r| r == "steer-acceptance:Accepted")
                && text.contains("KIWI")
        }
        "stop-steer" => {
            status == Some(DoneStatus::Interrupted)
                && !steered_exact
                && receipts.iter().any(|r| r == "native:inbox.withdraw:-:-")
        }
        "drop" | "drop-late" => {
            completed && reconnected && tool_results >= 1 && text.matches("TICK").count() <= 2
        }
        other => {
            eprintln!("unknown mode {other}");
            false
        }
    };
    eprintln!("--- acceptance {}", if ok { "PASS" } else { "FAIL" });
    if !ok {
        std::process::exit(1);
    }
}

/// `opencode serve --stdio --port 0` with a per-run password, under the
/// caller's (isolated) HOME/XDG. Returns the child, its URL and the Basic
/// credential.
async fn spawn_server(exe: &str, cwd: &str) -> (tokio::process::Child, String, String) {
    let password = format!("probe-{}", std::process::id());
    let mut child = tokio::process::Command::new(exe)
        .args(["serve", "--stdio", "--port", "0"])
        .current_dir(cwd)
        .env("OPENCODE_PASSWORD", &password)
        .env("OPENCODE_DISABLE_AUTOUPDATE", "1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn opencode serve");
    let stdout = child.stdout.take().expect("stdout");
    let mut lines = tokio::io::BufReader::new(stdout).lines();
    let line = tokio::time::timeout(Duration::from_secs(60), lines.next_line())
        .await
        .expect("url line in time")
        .expect("read url line")
        .expect("url line");
    let url: serde_json::Value = serde_json::from_str(&line).expect("url JSON");
    let url = url["url"]
        .as_str()
        .expect("url")
        .trim_end_matches('/')
        .to_owned();
    let auth = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("opencode:{password}"))
    );
    (child, url, auth)
}

/// A one-request-per-connection HTTP proxy that injects the Basic
/// credential. `/api/event` streams are cut when `cut` fires, and new ones
/// are held unanswered for `gap` afterwards.
async fn start_proxy(
    upstream: String,
    auth: String,
    cut: watch::Sender<bool>,
    gap: Duration,
) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind proxy");
    let addr = listener.local_addr().expect("proxy addr");
    let upstream_addr = upstream.trim_start_matches("http://").to_owned();
    let refusing = Arc::new(AtomicBool::new(false));
    let fired = Arc::new(AtomicBool::new(false));
    tokio::spawn(async move {
        loop {
            let Ok((client, _)) = listener.accept().await else {
                return;
            };
            let upstream_addr = upstream_addr.clone();
            let auth = auth.clone();
            let cut = cut.subscribe();
            let refusing = refusing.clone();
            let fired = fired.clone();
            tokio::spawn(async move {
                let _ = proxy_one(client, &upstream_addr, &auth, cut, refusing, fired, gap).await;
            });
        }
    });
    format!("http://{addr}")
}

async fn proxy_one(
    mut client: TcpStream,
    upstream_addr: &str,
    auth: &str,
    mut cut: watch::Receiver<bool>,
    refusing: Arc<AtomicBool>,
    fired: Arc<AtomicBool>,
    gap: Duration,
) -> std::io::Result<()> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    let head_end = loop {
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
        let n = client.read(&mut chunk).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.split("\r\n");
    let start = lines.next().unwrap_or_default().to_owned();
    let is_event = start
        .split_whitespace()
        .nth(1)
        .is_some_and(|p| p.starts_with("/api/event"));
    if is_event && refusing.load(Ordering::SeqCst) {
        // Park the reconnect (no response yet) until the gap is over, so
        // the gap's length does not depend on the bus's retry budget.
        eprintln!("--- proxy: holding an /api/event reconnect until the gap ends");
        while refusing.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    let mut rewritten = format!("{start}\r\n");
    let mut content_length = 0usize;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("authorization:") || lower.starts_with("connection:") {
            continue;
        }
        if let Some(value) = lower.strip_prefix("content-length:") {
            content_length = value.trim().parse().unwrap_or(0);
        }
        rewritten.push_str(line);
        rewritten.push_str("\r\n");
    }
    rewritten.push_str(&format!(
        "authorization: {auth}\r\nconnection: close\r\n\r\n"
    ));
    while buf.len() < head_end + content_length {
        let n = client.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let mut upstream = TcpStream::connect(upstream_addr).await?;
    upstream.write_all(rewritten.as_bytes()).await?;
    upstream.write_all(&buf[head_end..]).await?;
    if !is_event {
        tokio::io::copy(&mut upstream, &mut client).await?;
        return Ok(());
    }
    // An SSE stream: pass bytes through until the cut fires (once).
    let mut piece = [0u8; 8192];
    loop {
        tokio::select! {
            n = upstream.read(&mut piece) => {
                let n = n?;
                if n == 0 {
                    return Ok(());
                }
                client.write_all(&piece[..n]).await?;
            }
            _ = cut.changed(), if !fired.load(Ordering::SeqCst) => {
                fired.store(true, Ordering::SeqCst);
                refusing.store(true, Ordering::SeqCst);
                eprintln!("--- proxy: SSE cut; refusing reconnects for {gap:?}");
                let refusing = refusing.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(gap).await;
                    refusing.store(false, Ordering::SeqCst);
                    eprintln!("--- proxy: gap over");
                });
                return Ok(());
            }
        }
    }
}
