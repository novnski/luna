//! Terminals — PTY sessions owned by this device (feature-inventory §3.4; port of
//! luna's `terminals.ts` over `portable-pty`).
//!
//! - `open` spawns the user's login shell in the chat's cwd; `subscribe` replays a
//!   bounded 1MB window (resumable via `afterSeq`) then tails live output, batched
//!   at [`TERMINAL_OUTPUT_BATCH_MS`]; data rides base64 (PTY bytes ≠ UTF-8).
//! - Live shells survive subscriber detach until explicit close or engine exit.
//!   The optional profile-local SQLite store retains bounded output and
//!   terminal metadata across restarts, but never revives an old PTY.
//!   Exited in-memory sessions expire after 30 minutes; saved history has its
//!   own count and per-terminal output bounds.
//! - Ownership: M5 is single-user local — every IPC/relay caller is the device
//!   owner, so the per-user owner re-checks from luna's Router land with real
//!   multi-account auth in M6.

use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use portable_pty::PtySize;
use portable_pty::{CommandBuilder, native_pty_system};
mod history;
use tokio::sync::mpsc;

use luna_doc::TERMINAL_OUTPUT_BATCH_MS;
use luna_proto::{TerminalEvent, TerminalInventoryEntry, TerminalProcessState, TerminalSession};

use history::TerminalHistory;

use crate::{EngineError, new_id};

const MAX_TERMINALS: usize = 32;
const MAX_INPUT_BYTES: usize = 64 * 1024;
const MAX_REPLAY_BYTES: usize = 1024 * 1024;
const MAX_OUTPUT_FRAME_BYTES: usize = 32 * 1024;
const EXITED_TTL: Duration = Duration::from_secs(30 * 60);
const REAPER_INTERVAL: Duration = Duration::from_secs(60);

struct LiveTerminal {
    inventory: TerminalInventoryEntry,
    history: Option<Arc<TerminalHistory>>,
    process_id: Option<u32>,
    child_reaped: bool,
    reaped_exit_code: Option<i32>,
    master: Option<Box<dyn portable_pty::MasterPty + Send>>,
    writer: Option<Box<dyn Write + Send>>,
    killer: Box<dyn portable_pty::ChildKiller + Send + Sync>,
    subscribers: Vec<mpsc::UnboundedSender<TerminalEvent>>,
    replay: VecDeque<TerminalEvent>,
    replay_bytes: usize,
    seq: u64,
    last_active_at: std::time::Instant,
    exited: bool,
}

impl LiveTerminal {
    /// Stamp a seq, append to the bounded replay window, and fan out to live
    /// subscribers. On `Exit` the subscriber senders are dropped so every
    /// attached stream ends after delivering the event.
    fn emit(&mut self, event: TerminalEvent) {
        if self.exited {
            return;
        }
        self.last_active_at = std::time::Instant::now();
        self.inventory.last_seq = match &event {
            TerminalEvent::Data { seq, .. } | TerminalEvent::Exit { seq, .. } => *seq,
        };
        self.inventory.updated_at_ms = chrono::Utc::now().timestamp_millis();
        if let TerminalEvent::Exit { exit_code, .. } = &event {
            self.inventory.state = TerminalProcessState::Exited;
            self.inventory.exit_code = Some(*exit_code);
        }
        if let Some(history) = &self.history
            && let Err(error) = history.append(&self.inventory.id, &event)
        {
            tracing::error!(terminal = %self.inventory.id, %error, "terminal history write failed");
        }
        let bytes = match &event {
            TerminalEvent::Data { data, .. } => data.len(),
            TerminalEvent::Exit { .. } => 16,
        };
        self.replay.push_back(event.clone());
        self.replay_bytes += bytes;
        while self.replay_bytes > MAX_REPLAY_BYTES && self.replay.len() > 1 {
            if let Some(dropped) = self.replay.pop_front() {
                self.replay_bytes -= match &dropped {
                    TerminalEvent::Data { data, .. } => data.len(),
                    TerminalEvent::Exit { .. } => 16,
                };
            }
        }
        self.subscribers.retain(|tx| tx.send(event.clone()).is_ok());
        if matches!(event, TerminalEvent::Exit { .. }) {
            self.exited = true;
            self.subscribers.clear();
        }
    }

    fn next_seq(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }
}

struct TerminalsInner {
    sessions: Mutex<HashMap<String, Arc<Mutex<LiveTerminal>>>>,
    history: Option<Arc<TerminalHistory>>,
    device_id: String,
}

impl Drop for TerminalsInner {
    fn drop(&mut self) {
        let sessions = self
            .sessions
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .drain()
            .map(|(_, session)| session)
            .collect::<Vec<_>>();
        for session in sessions {
            dispose(&session, true, TerminalProcessState::Interrupted);
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Clone)]
pub struct Terminals {
    inner: Arc<TerminalsInner>,
}

impl Default for Terminals {
    fn default() -> Self {
        Self::new()
    }
}

fn clamp_size(cols: u16, rows: u16) -> PtySize {
    PtySize {
        cols: cols.clamp(2, 500),
        rows: rows.clamp(1, 300),
        pixel_width: 0,
        pixel_height: 0,
    }
}

/// The user's interactive shell: `$SHELL`, else the platform default.
fn selected_shell() -> String {
    std::env::var("SHELL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| {
            if cfg!(target_os = "macos") {
                "/bin/zsh".into()
            } else {
                "/bin/bash".into()
            }
        })
}

impl Terminals {
    /// Requires a tokio runtime (spawns the exited-session reaper).
    pub fn new() -> Self {
        Self::with_storage(None, String::new())
    }

    /// Open history in the engine's profile root. Previous running entries are
    /// marked interrupted because their PTY handles cannot survive this boot.
    pub fn open_with_history(root: &Path, device_id: &str) -> Result<Self, EngineError> {
        let history = TerminalHistory::open(root)
            .map_err(|error| EngineError::Other(format!("terminal history: {error}")))?;
        Ok(Self::with_storage(
            Some(Arc::new(history)),
            device_id.to_string(),
        ))
    }

    fn with_storage(history: Option<Arc<TerminalHistory>>, device_id: String) -> Self {
        let terminals = Self {
            inner: Arc::new(TerminalsInner {
                sessions: Mutex::new(HashMap::new()),
                history,
                device_id,
            }),
        };
        tokio::spawn(reaper_task(Arc::downgrade(&terminals.inner)));
        terminals
    }

    /// Open a login shell in `cwd`. The PTY outlives every subscriber; it dies on
    /// [`Self::close`], shell exit + TTL, or engine shutdown.
    pub fn open(&self, cwd: &str, cols: u16, rows: u16) -> Result<TerminalSession, EngineError> {
        self.open_for_task("", cwd, cols, rows)
    }

    pub fn open_for_task(
        &self,
        chat_id: &str,
        cwd: &str,
        cols: u16,
        rows: u16,
    ) -> Result<TerminalSession, EngineError> {
        self.open_with_shell_for_task(chat_id, cwd, cols, rows, None)
    }

    /// Explicit shell override (tests use `/bin/sh`).
    pub fn open_with_shell(
        &self,
        cwd: &str,
        cols: u16,
        rows: u16,
        shell: Option<&str>,
    ) -> Result<TerminalSession, EngineError> {
        self.open_with_shell_for_task("", cwd, cols, rows, shell)
    }

    pub fn open_with_shell_for_task(
        &self,
        chat_id: &str,
        cwd: &str,
        cols: u16,
        rows: u16,
        shell: Option<&str>,
    ) -> Result<TerminalSession, EngineError> {
        self.open_process_for_task(chat_id, cwd, cols, rows, shell, None)
    }

    /// Saved commands use a noninteractive POSIX shell, in an exact cwd. The
    /// user command cannot execute until its durable receipt authorizes the
    /// random handshake through the existing terminal input channel.
    pub(crate) fn open_command_for_task(
        &self,
        chat_id: &str,
        cwd: &str,
        command: &str,
        handshake: &str,
    ) -> Result<TerminalSession, EngineError> {
        self.open_process_for_task(
            chat_id,
            cwd,
            100,
            28,
            Some("/bin/sh"),
            Some((command, handshake)),
        )
    }

    pub(crate) fn process_id(&self, terminal_id: &str) -> Option<u32> {
        self.session(terminal_id)
            .ok()
            .and_then(|s| lock(&s).process_id)
    }

    fn open_process_for_task(
        &self,
        chat_id: &str,
        cwd: &str,
        cols: u16,
        rows: u16,
        shell: Option<&str>,
        command: Option<(&str, &str)>,
    ) -> Result<TerminalSession, EngineError> {
        if lock(&self.inner.sessions).len() >= MAX_TERMINALS {
            return Err(EngineError::Other(format!(
                "Too many open terminals (maximum {MAX_TERMINALS})"
            )));
        }
        if !std::fs::metadata(cwd).map(|m| m.is_dir()).unwrap_or(false) {
            return Err(EngineError::Other(
                "Session working directory is unavailable".into(),
            ));
        }

        let explicit_shell = shell.is_some();
        let shell = shell.map(str::to_string).unwrap_or_else(selected_shell);
        let shell_name = std::path::Path::new(&shell)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| shell.clone());

        let (master, mut child) = {
            let pty = native_pty_system();
            let pair = pty
                .openpty(clamp_size(cols, rows))
                .map_err(|e| EngineError::Other(format!("could not open a pty: {e}")))?;
            let mut cmd = CommandBuilder::new(&shell);
            if let Some((command, handshake)) = command {
                let quote = |text: &str| format!("'{}'", text.replace('\'', "'\"'\"'"));
                cmd.arg("-c");
                cmd.arg(format!("IFS= read -r luna_command_gate; [ \"$luna_command_gate\" = {} ] || exit 125; exec /bin/sh -c {}", quote(handshake), quote(command)));
            } else if !explicit_shell {
                cmd.arg("-l"); // login shell — the user's real PATH/profile
            } else {
                // Explicit shell overrides are fixture-only today. Keep them
                // inside the caller's temporary cwd rather than sourcing the
                // user's interactive profile or shell startup hooks.
                cmd.env("HOME", cwd);
                cmd.env("ENV", "/dev/null");
                cmd.env("BASH_ENV", "/dev/null");
            }
            cmd.cwd(cwd);
            cmd.env("TERM", "xterm-256color");
            cmd.env("COLORTERM", "truecolor");
            cmd.env("TERM_PROGRAM", "Luna");
            let child = pair
                .slave
                .spawn_command(cmd)
                .map_err(|e| EngineError::Other(format!("could not spawn {shell_name}: {e}")))?;
            drop(pair.slave);
            (pair.master, child)
        };
        let killer = child.clone_killer();
        let process_id = child.process_id();
        let reader = master.try_clone_reader().map_err(|e| {
            let _ = child.kill();
            EngineError::Other(format!("pty reader: {e}"))
        })?;
        let writer = master.take_writer().map_err(|e| {
            let _ = child.kill();
            EngineError::Other(format!("pty writer: {e}"))
        })?;

        let id = new_id();
        let inventory = TerminalInventoryEntry {
            id: id.clone(),
            chat_id: chat_id.to_string(),
            device_id: self.inner.device_id.clone(),
            cwd: cwd.to_string(),
            shell: shell_name.clone(),
            state: TerminalProcessState::Running,
            exit_code: None,
            last_seq: 0,
            updated_at_ms: chrono::Utc::now().timestamp_millis(),
        };
        if let Some(history) = &self.inner.history {
            history.create(&inventory).map_err(|error| {
                // The child was spawned already; never leave an untracked PTY.
                let _ = child.kill();
                EngineError::Other(format!("terminal history create failed: {error}"))
            })?;
        }
        let session = Arc::new(Mutex::new(LiveTerminal {
            inventory,
            history: self.inner.history.clone(),
            process_id,
            child_reaped: false,
            reaped_exit_code: None,
            master: Some(master),
            writer: Some(writer),
            killer,
            subscribers: Vec::new(),
            replay: VecDeque::new(),
            replay_bytes: 0,
            seq: 0,
            last_active_at: std::time::Instant::now(),
            exited: false,
        }));
        lock(&self.inner.sessions).insert(id.clone(), session.clone());

        // Raw PTY bytes: blocking reader thread → batcher task (12ms windows).
        let (raw_tx, raw_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let reader_thread = std::thread::Builder::new()
            .name(format!("pty-read-{id}"))
            .spawn(move || read_pty(reader, raw_tx))
            .map_err(|e| {
                lock(&self.inner.sessions).remove(&id);
                dispose(&session, true, TerminalProcessState::Stopped);
                EngineError::Other(format!("pty reader thread: {e}"))
            })?;
        drop(reader_thread);
        let wait = {
            let waiting_session = Arc::downgrade(&session);
            tokio::task::spawn_blocking(move || {
                loop {
                    let Some(session) = waiting_session.upgrade() else {
                        return child.wait();
                    };
                    // Reap under the same lock used by dispose. Until the child is
                    // reaped, its PID cannot be reused; after reaping, dispose sees
                    // child_reaped and cannot signal a different process group.
                    let mut live = lock(&session);
                    match child.try_wait() {
                        Ok(Some(status)) => {
                            live.child_reaped = true;
                            live.reaped_exit_code = Some(status.exit_code() as i32);
                            return Ok(status);
                        }
                        Err(error) => {
                            live.child_reaped = true;
                            return Err(error);
                        }
                        Ok(None) => {}
                    }
                    drop(live);
                    std::thread::sleep(Duration::from_millis(50));
                }
            })
        };
        tokio::spawn(pump_output(Arc::downgrade(&session), raw_rx, wait));

        Ok(TerminalSession {
            id,
            cwd: cwd.to_string(),
            shell: shell_name,
        })
    }

    fn session(&self, terminal_id: &str) -> Result<Arc<Mutex<LiveTerminal>>, EngineError> {
        lock(&self.inner.sessions)
            .get(terminal_id)
            .cloned()
            .ok_or_else(|| EngineError::Other("Terminal not found".into()))
    }

    /// Replay (from `after_seq`, bounded 1MB window) then live tail. The stream
    /// ends after `Exit`; detaching (dropping the stream) leaves the PTY running.
    pub fn subscribe(
        &self,
        terminal_id: &str,
        after_seq: Option<u64>,
    ) -> Result<mpsc::UnboundedReceiver<TerminalEvent>, EngineError> {
        let session = match self.session(terminal_id) {
            Ok(session) => session,
            Err(error) => {
                let Some(history) = &self.inner.history else {
                    return Err(error);
                };
                if !history.exists(terminal_id).map_err(history_error)? {
                    return Err(error);
                }
                let events = history
                    .replay(terminal_id, after_seq.unwrap_or(0))
                    .map_err(history_error)?;
                let (tx, rx) = mpsc::unbounded_channel();
                for event in events {
                    let _ = tx.send(event);
                }
                return Ok(rx);
            }
        };
        let mut session = lock(&session);
        session.last_active_at = std::time::Instant::now();
        let (tx, rx) = mpsc::unbounded_channel();
        let after = after_seq.unwrap_or(0);
        for event in &session.replay {
            let seq = match event {
                TerminalEvent::Data { seq, .. } | TerminalEvent::Exit { seq, .. } => *seq,
            };
            if seq > after {
                let _ = tx.send(event.clone());
            }
        }
        if !session.exited {
            session.subscribers.push(tx);
        }
        // On an exited session `tx` drops here: the stream ends after the replay.
        Ok(rx)
    }

    /// Write input bytes; `data` is base64 (matching `Data` events), with a plain
    /// UTF-8 fallback for lenient callers.
    pub fn write(&self, terminal_id: &str, data: &str) -> Result<(), EngineError> {
        let bytes = BASE64
            .decode(data)
            .unwrap_or_else(|_| data.as_bytes().to_vec());
        if bytes.len() > MAX_INPUT_BYTES {
            return Err(EngineError::Other("Terminal input is too large".into()));
        }
        let session = self.session(terminal_id)?;
        let mut session = lock(&session);
        if session.exited {
            return Err(EngineError::Other("Terminal has exited".into()));
        }
        session.last_active_at = std::time::Instant::now();
        let writer = session
            .writer
            .as_mut()
            .ok_or_else(|| EngineError::Other("Terminal has exited".into()))?;
        writer
            .write_all(&bytes)
            .and_then(|_| writer.flush())
            .map_err(|e| EngineError::Other(format!("Terminal write failed: {e}")))
    }

    pub fn resize(&self, terminal_id: &str, cols: u16, rows: u16) -> Result<(), EngineError> {
        let session = self.session(terminal_id)?;
        let mut session = lock(&session);
        session.last_active_at = std::time::Instant::now();
        if session.exited {
            return Ok(());
        }
        let Some(master) = session.master.as_ref() else {
            return Ok(());
        };
        master
            .resize(clamp_size(cols, rows))
            .map_err(|e| EngineError::Other(format!("Terminal resize failed: {e}")))
    }

    /// Kill the shell (if still running) and drop the session + replay buffer.
    pub fn close(&self, terminal_id: &str) -> Result<(), EngineError> {
        let session = lock(&self.inner.sessions)
            .remove(terminal_id)
            .ok_or_else(|| EngineError::Other("Terminal not found".into()))?;
        dispose(&session, true, TerminalProcessState::Stopped);
        Ok(())
    }

    /// Any live PTY (the reaper prunes exited ones) — restarts kill shells, so
    /// the auto-updater waits for none.
    pub fn any_open(&self) -> bool {
        lock(&self.inner.sessions)
            .values()
            .any(|session| !lock(session).exited)
    }

    pub fn list(&self, chat_id: Option<&str>) -> Result<Vec<TerminalInventoryEntry>, EngineError> {
        let mut entries = if let Some(history) = &self.inner.history {
            history.list(chat_id).map_err(history_error)?
        } else {
            Vec::new()
        };
        for session in lock(&self.inner.sessions).values() {
            let live = lock(session);
            if chat_id.is_some_and(|id| id != live.inventory.chat_id) {
                continue;
            }
            if let Some(entry) = entries
                .iter_mut()
                .find(|entry| entry.id == live.inventory.id)
            {
                *entry = live.inventory.clone();
            } else {
                entries.push(live.inventory.clone());
            }
        }
        entries.sort_by(|a, b| {
            b.updated_at_ms
                .cmp(&a.updated_at_ms)
                .then_with(|| b.id.cmp(&a.id))
        });
        Ok(entries)
    }

    /// Stop task-owned PTYs and remove their saved output after task deletion.
    pub fn delete_task(&self, chat_id: &str) -> Result<usize, EngineError> {
        let ids = self
            .list(Some(chat_id))?
            .into_iter()
            .map(|item| item.id)
            .collect::<Vec<_>>();
        for id in &ids {
            if let Some(session) = lock(&self.inner.sessions).remove(id) {
                dispose(&session, true, TerminalProcessState::Stopped);
            }
        }
        if let Some(history) = &self.inner.history {
            history.delete_task(chat_id).map_err(history_error)?;
        }
        Ok(ids.len())
    }

    /// Engine shutdown: kill every live shell.
    pub fn shutdown(&self) {
        let sessions: Vec<_> = lock(&self.inner.sessions).drain().map(|(_, s)| s).collect();
        for session in sessions {
            dispose(&session, true, TerminalProcessState::Interrupted);
        }
    }
}

fn history_error(error: rusqlite::Error) -> EngineError {
    EngineError::Other(format!("terminal history: {error}"))
}

fn dispose(session: &Arc<Mutex<LiveTerminal>>, kill: bool, state: TerminalProcessState) {
    let mut session = lock(session);
    let was_live = !session.exited;
    if was_live {
        let observed_exit = session.reaped_exit_code;
        let state = if observed_exit.is_some() {
            TerminalProcessState::Exited
        } else if session.child_reaped {
            TerminalProcessState::Interrupted
        } else {
            state
        };
        let seq = session.next_seq();
        let signal = match state {
            TerminalProcessState::Exited => None,
            TerminalProcessState::Stopped => Some("closed".to_string()),
            TerminalProcessState::Interrupted => Some("engineExit".to_string()),
            TerminalProcessState::Running => None,
        };
        session.emit(TerminalEvent::Exit {
            seq,
            exit_code: observed_exit.unwrap_or(-1),
            signal,
        });
        session.inventory.state = state;
        session.inventory.exit_code = observed_exit;
        session.inventory.updated_at_ms = chrono::Utc::now().timestamp_millis();
        if let Some(history) = &session.history
            && let Err(error) =
                history.set_state(&session.inventory.id, state, observed_exit, session.seq)
        {
            tracing::error!(terminal = %session.inventory.id, %error, "terminal history state write failed");
        }
    }
    session.subscribers.clear();
    #[cfg(unix)]
    if kill
        && was_live
        && !session.child_reaped
        && let Some(pid) = session.process_id
    {
        // portable-pty starts the shell in a new session/process group. Target
        // only that group; unrelated user processes are never enumerated.
        unsafe {
            if libc::getpgid(pid as i32) == pid as i32 {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
    }
    if kill
        && was_live
        && !session.child_reaped
        && let Err(err) = session.killer.kill()
    {
        tracing::debug!(error = %err, "terminal kill failed (already exited?)");
    }
}

/// Blocking PTY reader: forwards raw chunks until EOF. A closed PTY reads as an
/// error on some platforms (EIO on Linux once the shell exits) — both end the loop.
fn read_pty(mut reader: Box<dyn Read + Send>, tx: mpsc::UnboundedSender<Vec<u8>>) {
    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        }
    }
}

/// Batches raw chunks into `Data` events every [`TERMINAL_OUTPUT_BATCH_MS`].
/// Child wait and output reads are driven concurrently. Once the child is gone we close
/// the PTY handles, drain its final buffered output, then emit `Exit`.
/// Holds only a weak session handle so a closed terminal tears this task down.
async fn pump_output(
    session: Weak<Mutex<LiveTerminal>>,
    mut raw_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    mut wait: tokio::task::JoinHandle<Result<portable_pty::ExitStatus, std::io::Error>>,
) {
    let batch = Duration::from_millis(TERMINAL_OUTPUT_BATCH_MS);
    let emit = |buffer: Vec<u8>| -> bool {
        let Some(session) = session.upgrade() else {
            return false;
        };
        let mut session = lock(&session);
        for chunk in buffer.chunks(MAX_OUTPUT_FRAME_BYTES) {
            let seq = session.next_seq();
            session.emit(TerminalEvent::Data {
                seq,
                data: BASE64.encode(chunk),
            });
        }
        true
    };

    // Start a batch only when output arrives; idle terminals need no timer.
    let flush = tokio::time::sleep(batch);
    tokio::pin!(flush);
    let mut buffer = Vec::new();
    let mut raw_open = true;
    let mut exit_code = None;

    while raw_open || exit_code.is_none() {
        tokio::select! {
            chunk = raw_rx.recv(), if raw_open => match chunk {
                Some(chunk) => {
                    if buffer.is_empty() {
                        flush.as_mut().reset(tokio::time::Instant::now() + batch);
                    }
                    buffer.extend_from_slice(&chunk);
                },
                None => raw_open = false,
            },
            result = &mut wait, if exit_code.is_none() => {
                exit_code = Some(match result {
                    Ok(Ok(status)) => status.exit_code() as i32,
                    Ok(Err(err)) => {
                        tracing::debug!(error = %err, "terminal wait failed");
                        -1
                    }
                    Err(err) => {
                        tracing::debug!(error = %err, "terminal wait task failed");
                        -1
                    }
                });

                let Some(session) = session.upgrade() else {
                    return;
                };
                {
                    let (master, writer) = {
                        let mut session = lock(&session);
                        (session.master.take(), session.writer.take())
                    };
                    let _ = tokio::task::spawn_blocking(move || {
                        drop(writer);
                        drop(master);
                    }).await;
                }
            }
            _ = &mut flush, if !buffer.is_empty() => {
                if !emit(std::mem::take(&mut buffer)) {
                    return;
                }
            }
        }
    }

    if !buffer.is_empty() && !emit(buffer) {
        return;
    }
    if let Some(session) = session.upgrade() {
        let mut session = lock(&session);
        let seq = session.next_seq();
        session.emit(TerminalEvent::Exit {
            seq,
            exit_code: exit_code.unwrap_or(-1),
            signal: None,
        });
    }
}

/// Live shells never expire on idleness — a detached session is the user's running
/// process. Only EXITED sessions are swept after [`EXITED_TTL`]: they're inert
/// replay buffers held so a returning viewer can show the tail + exit status.
async fn reaper_task(inner: Weak<TerminalsInner>) {
    let mut tick = tokio::time::interval(REAPER_INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tick.tick().await; // consume the immediate first tick
    loop {
        tick.tick().await;
        let Some(inner) = inner.upgrade() else { break };
        let mut sessions = lock(&inner.sessions);
        sessions.retain(|_, session| {
            let session = lock(session);
            !(session.exited && session.last_active_at.elapsed() > EXITED_TTL)
        });
    }
}
