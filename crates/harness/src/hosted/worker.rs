//! The worker side of provider profile 2.0: one supervised process that
//! hosts one adapter instance and serves it over worker protocol 1.0 on its
//! stdio (JSON lines, at most 64 KiB per frame).
//!
//! - Exactly one answer per call; a cancelled call is answered `cancelled`
//!   at once and its late answer is suppressed.
//! - Every job runs in its own task. Its reports go to one queue, in the
//!   order the adapter produced them, and leave only when the host
//!   acknowledges them with a later `after`. While more than 8 MiB is
//!   unacknowledged, a run's event stream waits (backpressure, never drops).
//! - Questions, LUNA tool calls and progress reports wait for the host's
//!   `provider.job.reply`; fork and rewind progress block the adapter until
//!   the host confirmed it, exactly like the in-process reporter does.
//! - Shutdown (frame or end of input) interrupts every run and exits; the
//!   supervisor ends the process group, every `opencode serve` included.
//!
//! A native worker is not sandboxed: it has the user's account access.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use futures::StreamExt;
use futures::future::BoxFuture;
use luna_proto::{
    AgentEvent, ComposerInput, GoalStartRequest, PermissionMode, RunRequest, RuntimeDelivery,
    UserInputAnswer, UserInputQuestion,
};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot, watch};

use super::{
    CHILD_STOP, DESCRIBE, EVENTS, FRAGMENT_CHUNK_BYTES, FRAGMENT_THRESHOLD_BYTES, FRAME_BYTES,
    JOB_CANCEL, JOB_REPLY, JOB_START, MAX_UPLOAD_BYTES, OPERATIONS, PAGE_BYTES, PROFILE_MAJOR,
    PROFILE_MINOR, RUN_STEER, RUN_STOP, UPLOAD, decode_command_scope, decode_compact_request,
    decode_error, decode_fork_request, decode_history_scope, decode_mcp_request,
    decode_rewind_request, decode_steer_delivery, encode_error, encode_fork_progress,
    encode_fork_result, encode_history_turns, encode_mcp_reply, encode_rewind_outcome,
    encode_session_mcp, encode_steer_acceptance, encode_task_stop, encode_tool_context, op,
    rewind_request_written, split_text, steer_dropped,
};
use crate::{
    CancellationToken, Harness, HarnessError, LunaToolExecutor, LunaToolInvocation,
    NativeRewindProgress, ProviderForkProgress, RunControls, SteerMessage,
};

/// Answer a long poll this long before the host's deadline.
const DEADLINE_MARGIN: Duration = Duration::from_millis(500);
/// Unacknowledged bytes above which a run's event stream waits.
const HIGH_WATER_BYTES: usize = 8 * 1024 * 1024;
const BACKPRESSURE_POLL: Duration = Duration::from_millis(20);
/// Steer messages buffered between the host's call and the adapter.
const STEER_BUFFER: usize = 32;
const MAX_JOBS: usize = 64;
const MAX_UPLOADS: usize = 16;
const MAX_UPLOAD_PARTS: usize = 65_536;
const UPLOAD_TTL: Duration = Duration::from_secs(300);
/// How long a fork or rewind progress report waits for the host.
const PROGRESS_ACK_WAIT: Duration = Duration::from_secs(60);
const SHUTDOWN_GRACE: Duration = Duration::from_millis(300);
const MESSAGE_LIMIT: usize = 2000;

type Probe<T> = Arc<dyn Fn() -> T + Send + Sync>;
/// The resolved program for `describe`: its path and version.
type ExecutableProbe = Probe<Option<(String, Option<String>)>>;
type InputRequester =
    Box<dyn Fn(Vec<UserInputQuestion>) -> oneshot::Receiver<Vec<UserInputAnswer>> + Send + Sync>;
type CallError = (&'static str, String);

/// What one hosted worker serves.
pub struct WorkerSpec {
    /// The provider contribution id, exactly as the package declares it.
    pub provider_id: String,
    pub harness: Arc<dyn Harness>,
    /// The program the adapter resolves (path, version) for `describe`.
    pub executable: Option<ExecutableProbe>,
    /// Shown when the adapter's own `installed()` probe is false.
    pub unavailable_reason: String,
    /// Adapter state the host carries from one run's worker to the next.
    pub state: Option<StateHooks>,
}

/// Export and import of state an adapter keeps between runs in one
/// process (OpenCode: its LUNA-owned session grants). The value is a JSON
/// object whose top-level keys are independent entries.
pub struct StateHooks {
    pub export: Probe<Value>,
    pub import: Arc<dyn Fn(&Value) + Send + Sync>,
}

/// Serve `spec` on this process's stdio until the host ends the instance.
pub fn run(spec: WorkerSpec) -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("hosted provider worker: no async runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    let result = runtime.block_on(serve(spec));
    runtime.shutdown_timeout(Duration::from_secs(1));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("hosted provider worker: {message}");
            ExitCode::FAILURE
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn clip(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

fn invalid(error: String) -> HarnessError {
    HarnessError::Protocol(format!("the request is invalid: {error}"))
}

fn to_json<T: Serialize>(value: &T) -> Result<Value, HarnessError> {
    serde_json::to_value(value)
        .map_err(|error| HarnessError::Protocol(format!("a result could not be encoded: {error}")))
}

fn text_field(params: &Value, key: &str) -> Result<String, CallError> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| ("badRequest", format!("`{key}` must be a string")))
}

fn flag(params: &Value, key: &str) -> bool {
    params.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn parsed<T: serde::de::DeserializeOwned + Default>(
    params: &Value,
    key: &str,
) -> Result<T, CallError> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(T::default()),
        Some(value) => serde_json::from_value(value.clone())
            .map_err(|error| ("badRequest", format!("`{key}` is invalid: {error}"))),
    }
}

enum Frame {
    Line(Vec<u8>),
    End,
    TooLarge,
}

/// One newline-terminated frame, never buffering more than a frame.
async fn read_frame<R: AsyncBufRead + Unpin>(reader: &mut R) -> std::io::Result<Frame> {
    let mut frame = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(Frame::End);
        }
        if let Some(end) = available.iter().position(|byte| *byte == b'\n') {
            if frame.len() + end > FRAME_BYTES {
                return Ok(Frame::TooLarge);
            }
            frame.extend_from_slice(&available[..end]);
            reader.consume(end + 1);
            return Ok(Frame::Line(frame));
        }
        let length = available.len();
        if frame.len() + length > FRAME_BYTES {
            return Ok(Frame::TooLarge);
        }
        frame.extend_from_slice(available);
        reader.consume(length);
    }
}

async fn write_frames(mut frames: mpsc::UnboundedReceiver<Vec<u8>>) {
    let mut stdout = tokio::io::stdout();
    while let Some(frame) = frames.recv().await {
        if stdout.write_all(&frame).await.is_err() || stdout.flush().await.is_err() {
            return;
        }
    }
}

fn send_line(out: &mpsc::UnboundedSender<Vec<u8>>, frame: &Value) {
    if let Ok(mut bytes) = serde_json::to_vec(frame) {
        bytes.push(b'\n');
        let _ = out.send(bytes);
    }
}

/// The handshake, then calls until shutdown or the end of input.
pub async fn serve(spec: WorkerSpec) -> Result<(), String> {
    let (out, frames) = mpsc::unbounded_channel::<Vec<u8>>();
    let writer = tokio::spawn(write_frames(frames));
    let mut input = BufReader::new(tokio::io::stdin());
    let first = match read_frame(&mut input).await {
        Ok(Frame::Line(frame)) => frame,
        Ok(Frame::End) => return Ok(()),
        Ok(Frame::TooLarge) => return Err("the hello frame is larger than a frame".into()),
        Err(error) => return Err(format!("the hello frame could not be read: {error}")),
    };
    let hello: Value = serde_json::from_slice(&first)
        .map_err(|error| format!("the hello frame is invalid: {error}"))?;
    if hello.get("type").and_then(Value::as_str) != Some("hello") {
        return Err("the first frame was not a hello".into());
    }
    let major = hello.pointer("/protocol/major").and_then(Value::as_u64);
    if major != Some(1) {
        return Err(format!(
            "LUNA speaks worker protocol {major:?}; this worker speaks 1"
        ));
    }
    send_line(
        &out,
        &json!({
            "type": "hello",
            "id": hello.get("id").cloned().unwrap_or(Value::Null),
            "protocol": {"major": 1, "minor": 0},
            "extensionId": hello.pointer("/instance/extensionId").cloned().unwrap_or(Value::Null),
            "packageHash": hello.pointer("/instance/packageHash").cloned().unwrap_or(Value::Null),
            "operations": OPERATIONS,
        }),
    );
    let host = Arc::new(Host::new(spec, out.clone()));
    // The end of input, an oversized frame or a read error ends the worker.
    while let Ok(Frame::Line(bytes)) = read_frame(&mut input).await {
        let Ok(frame) = serde_json::from_slice::<Value>(&bytes) else {
            continue;
        };
        match frame.get("type").and_then(Value::as_str) {
            Some("call") => host.clone().call(frame),
            Some("cancel") => {
                if let Some(id) = frame.get("id").and_then(Value::as_str) {
                    host.respond(id, Err(("cancelled", "the call was cancelled".into())));
                }
            }
            Some("shutdown") => break,
            // `hostResult`: this profile makes no host calls.
            _ => {}
        }
    }
    host.shutdown().await;
    drop(host);
    drop(out);
    let _ = tokio::time::timeout(Duration::from_millis(500), writer).await;
    Ok(())
}

struct Queued {
    seq: u64,
    json: String,
}

#[derive(Default)]
struct Queue {
    /// The last seq handed out.
    next: u64,
    items: VecDeque<Queued>,
    bytes: usize,
}

impl Queue {
    fn push(&mut self, mut item: Value) {
        self.next += 1;
        if let Value::Object(map) = &mut item {
            map.insert("seq".into(), json!(self.next));
        }
        let json = item.to_string();
        self.bytes += json.len();
        self.items.push_back(Queued {
            seq: self.next,
            json,
        });
    }
}

struct Upload {
    count: usize,
    parts: Vec<String>,
    bytes: usize,
    started: Instant,
}

/// One job: its task, and what it waits on from the host.
struct Job {
    id: String,
    interrupt: CancellationToken,
    steering: Mutex<Option<mpsc::Sender<SteerMessage>>>,
    inputs: Mutex<HashMap<String, oneshot::Sender<Vec<UserInputAnswer>>>>,
    tools: Mutex<HashMap<String, oneshot::Sender<Result<Value, HarnessError>>>>,
    acks: Mutex<HashMap<String, std::sync::mpsc::SyncSender<Result<(), HarnessError>>>>,
    task: Mutex<Option<tokio::task::AbortHandle>>,
    finished: AtomicBool,
}

impl Job {
    fn new(id: String) -> Self {
        Self {
            id,
            interrupt: CancellationToken::new(),
            steering: Mutex::new(None),
            inputs: Mutex::new(HashMap::new()),
            tools: Mutex::new(HashMap::new()),
            acks: Mutex::new(HashMap::new()),
            task: Mutex::new(None),
            finished: AtomicBool::new(false),
        }
    }
}

struct Host {
    provider_id: String,
    harness: Arc<dyn Harness>,
    executable: Option<ExecutableProbe>,
    unavailable_reason: String,
    state: Option<StateHooks>,
    out: mpsc::UnboundedSender<Vec<u8>>,
    /// Calls not answered yet.
    pending: Mutex<HashSet<String>>,
    queue: Mutex<Queue>,
    /// The last seq queued, for long polls.
    latest: watch::Sender<u64>,
    jobs: Mutex<HashMap<String, Arc<Job>>>,
    uploads: Mutex<HashMap<String, Upload>>,
}

impl Host {
    fn new(spec: WorkerSpec, out: mpsc::UnboundedSender<Vec<u8>>) -> Self {
        Self {
            provider_id: spec.provider_id,
            harness: spec.harness,
            executable: spec.executable,
            unavailable_reason: spec.unavailable_reason,
            state: spec.state,
            out,
            pending: Mutex::new(HashSet::new()),
            queue: Mutex::new(Queue::default()),
            latest: watch::channel(0).0,
            jobs: Mutex::new(HashMap::new()),
            uploads: Mutex::new(HashMap::new()),
        }
    }

    /// The one answer to call `id`; nothing once it was answered.
    fn respond(&self, id: &str, outcome: Result<String, CallError>) {
        if !lock(&self.pending).remove(id) {
            return;
        }
        let id_json = serde_json::to_string(id).unwrap_or_else(|_| "\"\"".into());
        let frame = match outcome {
            Ok(result) if result.len() + id_json.len() + 64 <= FRAME_BYTES => {
                format!(
                    "{{\"type\":\"response\",\"id\":{id_json},\"ok\":true,\"result\":{result}}}"
                )
            }
            Ok(_) => error_frame(id, "tooLarge", "the answer is larger than a protocol frame"),
            Err((code, message)) => error_frame(id, code, &message),
        };
        let mut bytes = frame.into_bytes();
        bytes.push(b'\n');
        let _ = self.out.send(bytes);
    }

    fn call(self: Arc<Self>, frame: Value) {
        let Some(id) = frame.get("id").and_then(Value::as_str).map(str::to_owned) else {
            return;
        };
        let method = frame
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let params = frame.get("params").cloned().unwrap_or(Value::Null);
        let deadline = Duration::from_millis(
            frame
                .get("deadlineMs")
                .and_then(Value::as_u64)
                .unwrap_or(30_000),
        );
        lock(&self.pending).insert(id.clone());
        tokio::spawn(async move {
            self.handle(id, method, params, deadline).await;
        });
    }

    async fn handle(
        self: Arc<Self>,
        id: String,
        method: String,
        params: Value,
        deadline: Duration,
    ) {
        let params = if method == UPLOAD {
            Ok(params)
        } else {
            self.resolve_upload(params)
        };
        let params = match params {
            Ok(params) => params,
            Err(message) => return self.respond(&id, Err(("badRequest", message))),
        };
        if method == EVENTS {
            let page = self.events(&params, deadline).await;
            return self.respond(&id, Ok(page));
        }
        let outcome = match method.as_str() {
            DESCRIBE => Ok(self.describe().await),
            UPLOAD => self.upload(&params),
            JOB_START => self.start_job(&params),
            JOB_CANCEL => Ok(self.cancel_job(&params)),
            JOB_REPLY => self.reply(&params),
            RUN_STOP => Ok(self.stop_run(&params)),
            RUN_STEER => self.steer(&params),
            CHILD_STOP => self.child_stop(&params).await,
            other => Err((
                "unknownOperation",
                format!("{other} is not a provider profile 2.0 operation"),
            )),
        };
        self.respond(&id, outcome.map(|value| value.to_string()));
    }

    // ---- describe ----------------------------------------------------------

    async fn describe(self: &Arc<Self>) -> Value {
        let host = self.clone();
        // The executable probe runs the CLI's own `--version`.
        let probed = tokio::task::spawn_blocking(move || {
            let available = host.harness.installed();
            let executable = host.executable.as_ref().and_then(|probe| probe());
            let delivery = host.harness.luna_runtime_delivery();
            (available, executable, delivery)
        })
        .await;
        let (available, executable, delivery) =
            probed.unwrap_or((false, None, RuntimeDelivery::Unsupported));
        let mut described = json!({
            "profile": {"major": PROFILE_MAJOR, "minor": PROFILE_MINOR},
            "providerId": self.provider_id,
            "available": available,
            "runtimeDelivery": delivery,
        });
        if !available {
            described["unavailableReason"] = json!(self.unavailable_reason);
        }
        if let Some((path, version)) = executable {
            described["executable"] = json!({"path": path, "version": version});
        }
        described
    }

    // ---- the event queue ---------------------------------------------------

    fn push(&self, item: Value) {
        let Ok(body) = serde_json::to_string(&item) else {
            return;
        };
        let mut queue = lock(&self.queue);
        if body.len() > FRAGMENT_THRESHOLD_BYTES {
            let parts = split_text(&body, FRAGMENT_CHUNK_BYTES);
            let count = parts.len();
            for (index, part) in parts.into_iter().enumerate() {
                queue.push(json!({
                    "type": "fragment",
                    "index": index,
                    "count": count,
                    "data": part,
                }));
            }
        } else {
            queue.push(item);
        }
        let last = queue.next;
        drop(queue);
        self.latest.send_replace(last);
    }

    /// [`Self::push`] once the host has caught up below the high-water mark.
    async fn push_wait(&self, item: Value) {
        loop {
            let full = lock(&self.queue).bytes >= HIGH_WATER_BYTES;
            if !full {
                break;
            }
            tokio::time::sleep(BACKPRESSURE_POLL).await;
        }
        self.push(item);
    }

    fn acknowledge(&self, after: u64) {
        let mut queue = lock(&self.queue);
        while queue.items.front().is_some_and(|item| item.seq <= after) {
            if let Some(item) = queue.items.pop_front() {
                queue.bytes -= item.json.len();
            }
        }
    }

    fn page(&self, after: u64) -> String {
        let queue = lock(&self.queue);
        let mut page = String::from("{\"events\":[");
        let mut used = 0usize;
        for (index, item) in queue
            .items
            .iter()
            .filter(|item| item.seq > after)
            .enumerate()
        {
            if index > 0 && used + item.json.len() + 1 > PAGE_BYTES {
                break;
            }
            if index > 0 {
                page.push(',');
            }
            page.push_str(&item.json);
            used += item.json.len() + 1;
        }
        page.push_str("]}");
        page
    }

    async fn events(&self, params: &Value, deadline: Duration) -> String {
        let after = params.get("after").and_then(Value::as_u64).unwrap_or(0);
        let wait = Duration::from_millis(params.get("waitMs").and_then(Value::as_u64).unwrap_or(0))
            .min(deadline.saturating_sub(DEADLINE_MARGIN));
        self.acknowledge(after);
        let mut latest = self.latest.subscribe();
        let ready = *latest.borrow_and_update() > after;
        if !ready && !wait.is_zero() {
            let _ = tokio::time::timeout(wait, latest.wait_for(|seq| *seq > after)).await;
        }
        self.page(after)
    }

    // ---- uploads -----------------------------------------------------------

    fn upload(&self, params: &Value) -> Result<Value, CallError> {
        let id = text_field(params, "uploadId")?;
        let data = text_field(params, "data")?;
        let index = params
            .get("index")
            .and_then(Value::as_u64)
            .ok_or(("badRequest", "`index` must be a number".to_owned()))?
            as usize;
        let count = params
            .get("count")
            .and_then(Value::as_u64)
            .ok_or(("badRequest", "`count` must be a number".to_owned()))?
            as usize;
        if count == 0 || count > MAX_UPLOAD_PARTS || index >= count {
            return Err(("badRequest", "an upload part is out of range".into()));
        }
        let mut uploads = lock(&self.uploads);
        uploads.retain(|_, upload| upload.started.elapsed() < UPLOAD_TTL);
        if index == 0 {
            if uploads.len() >= MAX_UPLOADS && !uploads.contains_key(&id) {
                return Err(("busy", format!("at most {MAX_UPLOADS} uploads at once")));
            }
            uploads.insert(
                id.clone(),
                Upload {
                    count,
                    parts: Vec::new(),
                    bytes: 0,
                    started: Instant::now(),
                },
            );
        }
        let valid = uploads.get(&id).is_some_and(|upload| {
            upload.count == count
                && upload.parts.len() == index
                && upload.bytes + data.len() <= MAX_UPLOAD_BYTES
        });
        if !valid {
            uploads.remove(&id);
            return Err((
                "badRequest",
                "the upload part is out of order or too large".into(),
            ));
        }
        if let Some(upload) = uploads.get_mut(&id) {
            upload.bytes += data.len();
            upload.parts.push(data);
        }
        Ok(json!({"received": index + 1}))
    }

    /// `{"upload": id}` → the uploaded value.
    fn resolve_upload(&self, params: Value) -> Result<Value, String> {
        let id = match params.as_object() {
            Some(map) if map.len() == 1 => {
                map.get("upload").and_then(Value::as_str).map(str::to_owned)
            }
            _ => None,
        };
        let Some(id) = id else {
            return Ok(params);
        };
        let upload = lock(&self.uploads)
            .remove(&id)
            .ok_or_else(|| format!("upload {id} is unknown or expired"))?;
        if upload.parts.len() != upload.count {
            return Err(format!("upload {id} is incomplete"));
        }
        serde_json::from_str(&upload.parts.concat())
            .map_err(|error| format!("upload {id} is not JSON: {error}"))
    }

    // ---- jobs --------------------------------------------------------------

    fn job_of(&self, params: &Value) -> Option<Arc<Job>> {
        let id = params.get("job").and_then(Value::as_str)?;
        lock(&self.jobs).get(id).cloned()
    }

    fn start_job(self: &Arc<Self>, params: &Value) -> Result<Value, CallError> {
        let id = params
            .get("job")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty() && id.len() <= 128)
            .ok_or(("badRequest", "a job needs an id".to_owned()))?
            .to_owned();
        let operation = params
            .get("op")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if !op::ALL.contains(&operation.as_str()) {
            return Err((
                "rejected",
                format!("{operation} is not a provider profile 2.0 job"),
            ));
        }
        let job = Arc::new(Job::new(id.clone()));
        {
            let mut jobs = lock(&self.jobs);
            if jobs.contains_key(&id) {
                return Err(("rejected", format!("job {id} already exists")));
            }
            if jobs.len() >= MAX_JOBS {
                return Err((
                    "rejected",
                    format!("this worker already runs {MAX_JOBS} jobs"),
                ));
            }
            jobs.insert(id, job.clone());
        }
        let job_params = params.get("params").cloned().unwrap_or(Value::Null);
        let host = self.clone();
        let running = job.clone();
        let task = tokio::spawn(async move {
            let outcome = host.run_op(&running, &operation, job_params).await;
            host.finish(&running, outcome);
        });
        *lock(&job.task) = Some(task.abort_handle());
        Ok(json!({}))
    }

    /// The job's single terminal report.
    fn finish(&self, job: &Job, outcome: Result<Value, HarnessError>) {
        if job.finished.swap(true, Ordering::SeqCst) {
            return;
        }
        let item = match outcome {
            Ok(result) => json!({"type": "done", "job": job.id, "ok": true, "result": result}),
            Err(error) => {
                json!({"type": "done", "job": job.id, "ok": false, "error": encode_error(&error)})
            }
        };
        lock(&job.inputs).clear();
        lock(&job.tools).clear();
        lock(&job.acks).clear();
        *lock(&job.steering) = None;
        self.push(item);
        lock(&self.jobs).remove(&job.id);
    }

    fn cancel_job(&self, params: &Value) -> Value {
        let Some(job) = self.job_of(params) else {
            return json!({"cancelled": false});
        };
        job.interrupt.cancel();
        if let Some(task) = lock(&job.task).take() {
            // Dropping the job's future drops the adapter's run and with it
            // the server process it owns.
            task.abort();
        }
        self.finish(
            &job,
            Err(HarnessError::Protocol("LUNA cancelled the job".into())),
        );
        json!({"cancelled": true})
    }

    fn stop_run(&self, params: &Value) -> Value {
        match self.job_of(params) {
            Some(job) => {
                job.interrupt.cancel();
                json!({"stopping": true})
            }
            None => json!({"stopping": false}),
        }
    }

    fn reply(&self, params: &Value) -> Result<Value, CallError> {
        let job = self
            .job_of(params)
            .ok_or(("unknownJob", "the job has ended".to_owned()))?;
        let id = text_field(params, "id")?;
        let ok = flag(params, "ok");
        match params.get("kind").and_then(Value::as_str) {
            Some("input") => {
                let answers = if flag(params, "cancelled") {
                    None
                } else {
                    Some(parsed::<Vec<UserInputAnswer>>(params, "answers")?)
                };
                let sender = lock(&job.inputs).remove(&id).ok_or((
                    "unknownRequest",
                    "the question is no longer pending".to_owned(),
                ))?;
                // A dismissed question drops the sender, as LUNA does.
                if let Some(answers) = answers {
                    let _ = sender.send(answers);
                }
            }
            Some("tool") => {
                let result = if ok {
                    Ok(params.get("result").cloned().unwrap_or(Value::Null))
                } else {
                    Err(decode_error(params.get("error").unwrap_or(&Value::Null)))
                };
                let sender = lock(&job.tools).remove(&id).ok_or((
                    "unknownRequest",
                    "the tool call is no longer pending".to_owned(),
                ))?;
                let _ = sender.send(result);
            }
            Some("progress") => {
                let result = if ok {
                    Ok(())
                } else {
                    Err(decode_error(params.get("error").unwrap_or(&Value::Null)))
                };
                let sender = lock(&job.acks).remove(&id).ok_or((
                    "unknownRequest",
                    "the progress report is no longer pending".to_owned(),
                ))?;
                let _ = sender.try_send(result);
            }
            _ => return Err(("badRequest", "a reply needs a known kind".into())),
        }
        Ok(json!({}))
    }

    fn steer(self: &Arc<Self>, params: &Value) -> Result<Value, CallError> {
        let job = self
            .job_of(params)
            .ok_or(("unknownJob", "the run has ended".to_owned()))?;
        let steer = text_field(params, "steer")?;
        let prompt = text_field(params, "prompt")?;
        let attachments = parsed::<Vec<String>>(params, "attachments")?;
        let input_items = parsed::<Vec<ComposerInput>>(params, "inputItems")?;
        let goal = parsed::<Option<GoalStartRequest>>(params, "goal")?;
        let message_id = params
            .get("messageId")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let delivery = decode_steer_delivery(params.get("delivery").unwrap_or(&Value::Null))
            .map_err(|error| ("badRequest", error))?;
        let (acceptance, receiver) = if flag(params, "acceptance") {
            let (sender, receiver) = oneshot::channel();
            (Some(sender), Some(receiver))
        } else {
            (None, None)
        };
        let message = SteerMessage {
            prompt,
            attachments,
            input_items,
            message_id,
            delivery,
            acceptance,
            goal,
        };
        let sender = lock(&job.steering).clone();
        let delivered = sender.is_some_and(|sender| sender.try_send(message).is_ok());
        if delivered && let Some(receiver) = receiver {
            let host = self.clone();
            let job_id = job.id.clone();
            tokio::spawn(async move {
                let acceptance = match receiver.await {
                    Ok(acceptance) => encode_steer_acceptance(&acceptance),
                    Err(_) => steer_dropped(),
                };
                host.push(json!({
                    "type": "steerAck",
                    "job": job_id,
                    "steer": steer,
                    "acceptance": acceptance,
                }));
            });
        }
        Ok(json!({"delivered": delivered}))
    }

    async fn child_stop(&self, params: &Value) -> Result<Value, CallError> {
        let session = text_field(params, "sessionId")?;
        let child = text_field(params, "childId")?;
        Ok(
            match self.harness.stop_native_child(&session, &child).await {
                Ok(stop) => json!({"ok": true, "stop": encode_task_stop(&stop)}),
                Err(error) => json!({"ok": false, "error": encode_error(&error)}),
            },
        )
    }

    /// Report fork or rewind progress and wait for the host's answer.
    fn await_ack(&self, job: &Job, progress: Value) -> Result<(), HarnessError> {
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let ack = uuid::Uuid::new_v4().to_string();
        lock(&job.acks).insert(ack.clone(), sender);
        self.push(json!({"type": "progress", "job": job.id, "progress": progress, "ack": ack}));
        let wait = || receiver.recv_timeout(PROGRESS_ACK_WAIT);
        let answer = match tokio::runtime::Handle::try_current() {
            Ok(_) => tokio::task::block_in_place(wait),
            Err(_) => wait(),
        };
        answer.unwrap_or_else(|_| {
            Err(HarnessError::Protocol(
                "LUNA did not confirm the progress report".into(),
            ))
        })
    }

    async fn run_op(
        self: &Arc<Self>,
        job: &Arc<Job>,
        operation: &str,
        params: Value,
    ) -> Result<Value, HarnessError> {
        let harness = self.harness.clone();
        match operation {
            op::MODELS => {
                let models = if flag(&params, "live") {
                    harness.models_live().await?
                } else {
                    harness.models().await?
                };
                Ok(json!({"models": to_json(&models)?}))
            }
            op::RUN => self.run_job(job, params).await,
            op::TITLE => self.title_job(job, params).await,
            op::COMMANDS => {
                let commands = match params.get("scope").filter(|scope| !scope.is_null()) {
                    Some(scope) => {
                        let scope = decode_command_scope(scope).map_err(invalid)?;
                        harness.commands_in(&scope).await?
                    }
                    None => harness.commands().await?,
                };
                Ok(json!({"commands": to_json(&commands)?}))
            }
            op::EXTENSIONS => {
                let cwd = params.get("cwd").and_then(Value::as_str).map(PathBuf::from);
                let mode: PermissionMode =
                    serde_json::from_value(params.get("mode").cloned().unwrap_or(Value::Null))
                        .map_err(|error| invalid(error.to_string()))?;
                let view = harness
                    .native_extensions(cwd.as_deref(), mode, flag(&params, "refresh"))
                    .await?;
                Ok(json!({"view": to_json(&view)?}))
            }
            op::MCP => {
                let request = decode_mcp_request(params.get("request").unwrap_or(&Value::Null))
                    .map_err(invalid)?;
                let reply = harness.mcp_control(request).await?;
                Ok(json!({"reply": encode_mcp_reply(&reply)}))
            }
            op::FORK => {
                let request = decode_fork_request(params.get("request").unwrap_or(&Value::Null))
                    .map_err(invalid)?;
                let host = self.clone();
                let reporting = job.clone();
                let reporter = move |progress: ProviderForkProgress| -> Result<(), HarnessError> {
                    host.await_ack(&reporting, encode_fork_progress(&progress))
                };
                let result = harness.fork_session_staged(request, &reporter).await?;
                Ok(json!({"result": encode_fork_result(&result)}))
            }
            op::HISTORY => {
                let scope = decode_history_scope(params.get("scope").unwrap_or(&Value::Null))
                    .map_err(invalid)?;
                let turns = harness.native_history_turns(scope).await?;
                Ok(json!({"turns": encode_history_turns(&turns)}))
            }
            op::REWIND => {
                let request = decode_rewind_request(params.get("request").unwrap_or(&Value::Null))
                    .map_err(invalid)?;
                let host = self.clone();
                let reporting = job.clone();
                let reporter = move |_progress: NativeRewindProgress| {
                    let _ = host.await_ack(&reporting, rewind_request_written());
                };
                let outcome = harness
                    .rewind_native_conversation(request, &reporter)
                    .await?;
                Ok(json!({"outcome": encode_rewind_outcome(&outcome)}))
            }
            op::COMPACT => {
                let request = decode_compact_request(params.get("request").unwrap_or(&Value::Null))
                    .map_err(invalid)?;
                harness.compact_context(request).await?;
                Ok(json!({}))
            }
            other => Err(HarnessError::Protocol(format!(
                "{other} is not a provider profile 2.0 job"
            ))),
        }
    }

    async fn title_job(&self, job: &Arc<Job>, params: Value) -> Result<Value, HarnessError> {
        let request: RunRequest =
            serde_json::from_value(params.get("request").cloned().unwrap_or(Value::Null))
                .map_err(|error| invalid(error.to_string()))?;
        let (_steer, steering) = mpsc::channel::<SteerMessage>(1);
        let request_input: InputRequester = Box::new(|_questions: Vec<UserInputQuestion>| {
            oneshot::channel::<Vec<UserInputAnswer>>().1
        });
        let controls = RunControls {
            request_input,
            steering,
            interrupt: job.interrupt.clone(),
            invoke_luna_tool: None,
        };
        let mut stream = self.harness.run_title(request, controls).await?;
        let mut events = Vec::new();
        while let Some(item) = stream.next().await {
            events.push(item?);
        }
        Ok(json!({"events": to_json(&events)?}))
    }

    fn input_requester(self: &Arc<Self>, job: &Arc<Job>) -> InputRequester {
        let host = self.clone();
        let job = job.clone();
        Box::new(move |questions: Vec<UserInputQuestion>| {
            let (sender, receiver) = oneshot::channel();
            let request = uuid::Uuid::new_v4().to_string();
            // MCP launch URLs never serialize; they travel beside the questions.
            let launch_urls: Vec<Value> = questions
                .iter()
                .map(|question| {
                    question
                        .mcp
                        .as_ref()
                        .and_then(|mcp| mcp.launch_url.clone())
                        .map_or(Value::Null, Value::String)
                })
                .collect();
            if let Ok(encoded) = serde_json::to_value(&questions) {
                lock(&job.inputs).insert(request.clone(), sender);
                host.push(json!({
                    "type": "input",
                    "job": job.id,
                    "request": request,
                    "questions": encoded,
                    "launchUrls": launch_urls,
                }));
            }
            receiver
        })
    }

    fn tool_executor(self: &Arc<Self>, job: &Arc<Job>) -> LunaToolExecutor {
        let host = self.clone();
        let job = job.clone();
        Arc::new(
            move |invocation: LunaToolInvocation| -> BoxFuture<'static, Result<Value, HarnessError>> {
                let (sender, receiver) = oneshot::channel();
                let call = uuid::Uuid::new_v4().to_string();
                lock(&job.tools).insert(call.clone(), sender);
                host.push(json!({
                    "type": "tool",
                    "job": job.id,
                    "call": call,
                    "name": invocation.name,
                    "arguments": invocation.arguments,
                    "context": encode_tool_context(&invocation.context),
                }));
                Box::pin(async move {
                    receiver.await.unwrap_or_else(|_| {
                        Err(HarnessError::Protocol(
                            "the run ended before LUNA answered the tool call".into(),
                        ))
                    })
                })
            },
        )
    }

    fn observe(&self, job: &Job, session: &str, observed: &mut Option<i64>) {
        let Some(observation) = self.harness.session_mcp_observation(session) else {
            return;
        };
        if *observed == Some(observation.observed_at_ms) {
            return;
        }
        *observed = Some(observation.observed_at_ms);
        self.push(json!({
            "type": "observation",
            "job": job.id,
            "mcp": encode_session_mcp(&observation),
        }));
    }

    async fn run_job(
        self: &Arc<Self>,
        job: &Arc<Job>,
        params: Value,
    ) -> Result<Value, HarnessError> {
        let request: RunRequest =
            serde_json::from_value(params.get("request").cloned().unwrap_or(Value::Null))
                .map_err(|error| invalid(error.to_string()))?;
        if let (Some(hooks), Some(state)) = (
            &self.state,
            params.get("state").filter(|state| !state.is_null()),
        ) {
            (hooks.import)(state);
        }
        let (steer_sender, steering) = mpsc::channel::<SteerMessage>(STEER_BUFFER);
        *lock(&job.steering) = Some(steer_sender);
        let invoke_luna_tool = flag(&params, "lunaTools").then(|| self.tool_executor(job));
        let controls = RunControls {
            request_input: self.input_requester(job),
            steering,
            interrupt: job.interrupt.clone(),
            invoke_luna_tool,
        };
        let mut stream = self.harness.run(request, controls).await?;
        self.push(json!({"type": "started", "job": job.id}));
        let mut session: Option<String> = None;
        let mut observed: Option<i64> = None;
        while let Some(item) = stream.next().await {
            match item {
                Ok(event) => {
                    if session.is_none()
                        && let AgentEvent::SessionStarted { session_id, .. } = &event
                    {
                        session = Some(session_id.clone());
                    }
                    match serde_json::to_value(&event) {
                        Ok(encoded) => {
                            self.push_wait(
                                json!({"type": "agent", "job": job.id, "event": encoded}),
                            )
                            .await
                        }
                        Err(error) => {
                            let error = HarnessError::Protocol(format!(
                                "an event could not be encoded: {error}"
                            ));
                            self.push(json!({
                                "type": "streamError",
                                "job": job.id,
                                "error": encode_error(&error),
                            }));
                        }
                    }
                    if let Some(session) = &session {
                        self.observe(job, session, &mut observed);
                    }
                }
                Err(error) => self.push(json!({
                    "type": "streamError",
                    "job": job.id,
                    "error": encode_error(&error),
                })),
            }
        }
        *lock(&job.steering) = None;
        let state = self.state.as_ref().map(|hooks| (hooks.export)());
        Ok(json!({
            "state": state,
            "runtimeDelivery": self.harness.luna_runtime_delivery(),
        }))
    }

    async fn shutdown(&self) {
        let jobs: Vec<Arc<Job>> = lock(&self.jobs).values().cloned().collect();
        for job in &jobs {
            job.interrupt.cancel();
        }
        if !jobs.is_empty() {
            tokio::time::sleep(SHUTDOWN_GRACE).await;
        }
    }
}

fn error_frame(id: &str, code: &str, message: &str) -> String {
    json!({
        "type": "response",
        "id": id,
        "ok": false,
        "error": {"code": code, "message": clip(message, MESSAGE_LIMIT)},
    })
    .to_string()
}
