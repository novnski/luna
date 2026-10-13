//! LUNA portable extension host (CORE-07).
//!
//! One process per portable instance, started and supervised by the engine
//! like any worker. It speaks worker protocol 1.0 on stdio, compiles exactly
//! one core Wasm module (portable profile 1.0: no components, no WASI) and
//! links only `luna_broker_v1`:
//!
//! - `call(request_ptr, request_len, answer_ptr, answer_capacity) -> i32`
//!   forwards one broker request, `{"op", "params"}`, to the engine and
//!   writes its answer, `{"ok", "result" | "error"}`, into guest memory. It
//!   returns the answer's length, `-1` when the answer does not fit, `-2` for
//!   an unreadable request and `-3` when the engine is gone.
//! - `log(level, ptr, len)` sends one bounded log line (at most 256 a call).
//!
//! A module exports `memory`, `luna_alloc(len) -> ptr` and one function per
//! operation, `luna.op/<name>(params_ptr, params_len) -> i64`, returning the
//! packed `(ptr << 32) | len` of its UTF-8 JSON answer, `{"result": ...}` or
//! `{"error": {"code", "message"}}`. Every call runs in a fresh store: a
//! 16 MiB memory limit, a fuel budget with async yields and the caller's
//! deadline. Cancelling a call drops it, and with it any host call it waits
//! on; the engine is told with `hostCancel`. The bytes compiled are the bytes
//! hashed and reported in the hello.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt as _, BufReader};
use tokio::sync::{mpsc, oneshot};
use wasmtime::{
    Caller, Config, Engine, ExternType, InstancePre, Linker, Module, Store, StoreLimits,
    StoreLimitsBuilder, ValType,
};

const FRAME_LIMIT: usize = 64 * 1024;
const MAX_MODULE_BYTES: u64 = 8 * 1024 * 1024;
const GUEST_MEMORY_CEILING: u64 = 16 * 1024 * 1024;
const FUEL_CEILING: u64 = 2_000_000_000;
/// CORE-00: yield to the executor every 10,000 units of fuel, so deadlines
/// and cancellation interrupt running guest code.
const FUEL_YIELD_INTERVAL: u64 = 10_000;
const MAX_WASM_STACK: usize = 512 * 1024;
const MAX_TABLE_ELEMENTS: usize = 10_000;
const DEFAULT_DEADLINE_MS: u64 = 30_000;
/// The engine's timer owns the deadline and cancels the call; this host
/// stops a guest itself only this long after it, should no cancel arrive.
const DEADLINE_BACKSTOP: Duration = Duration::from_millis(250);
const MAX_DEADLINE_MS: u64 = 120_000;
const OP_PREFIX: &str = "luna.op/";
const BROKER_MODULE: &str = "luna_broker_v1";
const MAX_LOGS_PER_CALL: u32 = 256;
const LOG_LIMIT: usize = 2 * 1024;
/// A broker request or a guest answer must leave room for its frame.
const PAYLOAD_LIMIT: usize = 60 * 1024;
const OUTBOUND_QUEUE: usize = 256;
const RUNTIME: &str = "wasmtime 49.0.2";

const EXIT_USAGE: i32 = 2;
const EXIT_MODULE_REFUSED: i32 = 3;
const EXIT_PROTOCOL: i32 = 4;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn fail(code: i32, message: impl std::fmt::Display) -> ! {
    eprintln!("luna-extension-host: {message}");
    std::process::exit(code);
}

fn bounded(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

/// One newline-terminated frame, never buffering more than [`FRAME_LIMIT`].
async fn read_frame<R: AsyncBufRead + Unpin>(reader: &mut R) -> Result<Option<Vec<u8>>, String> {
    let mut frame = Vec::new();
    loop {
        let available = reader.fill_buf().await.map_err(|error| error.to_string())?;
        if available.is_empty() {
            return Ok(None);
        }
        if let Some(end) = available.iter().position(|byte| *byte == b'\n') {
            if frame.len() + end > FRAME_LIMIT {
                return Err("a frame is larger than 64 KiB".into());
            }
            frame.extend_from_slice(&available[..end]);
            reader.consume(end + 1);
            return Ok(Some(frame));
        }
        let length = available.len();
        if frame.len() + length > FRAME_LIMIT {
            return Err("a frame is larger than 64 KiB".into());
        }
        frame.extend_from_slice(available);
        reader.consume(length);
    }
}

/// The engine side of the connection.
struct Host {
    out: mpsc::Sender<Vec<u8>>,
    /// Host calls waiting for the engine's `hostResult`.
    host_calls: Mutex<HashMap<String, oneshot::Sender<Value>>>,
    /// Running calls; whoever removes an entry answers it.
    calls: Mutex<HashMap<String, tokio::task::AbortHandle>>,
    next_host_call: AtomicU64,
}

fn encode(frame: &Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(frame).unwrap_or_default();
    if bytes.len() > FRAME_LIMIT {
        let id = frame.get("id").cloned().unwrap_or(Value::Null);
        bytes = serde_json::to_vec(&json!({
            "type": "response",
            "id": id,
            "ok": false,
            "error": {"code": "tooLarge", "message": "the answer is larger than one frame"},
        }))
        .unwrap_or_default();
    }
    bytes.push(b'\n');
    bytes
}

impl Host {
    async fn send(&self, frame: Value) -> bool {
        self.out.send(encode(&frame)).await.is_ok()
    }

    fn send_now(&self, frame: Value) {
        let _ = self.out.try_send(encode(&frame));
    }

    /// Forward one broker request and wait for the engine's answer; `None`
    /// when the engine is gone. Dropping the future withdraws the request.
    async fn host_call(self: &Arc<Self>, op: String, params: Value) -> Option<Value> {
        let id = format!("h{}", self.next_host_call.fetch_add(1, Ordering::Relaxed));
        let (sender, receiver) = oneshot::channel();
        lock(&self.host_calls).insert(id.clone(), sender);
        let _pending = PendingHostCall {
            host: self.clone(),
            id: id.clone(),
        };
        if !self
            .send(json!({"type": "hostCall", "id": id, "op": op, "params": params}))
            .await
        {
            return None;
        }
        receiver.await.ok()
    }

    /// The `response` frame for one finished call, unless it was cancelled.
    async fn answer(&self, id: &str, answer: Result<Value, (String, String)>) {
        if lock(&self.calls).remove(id).is_none() {
            return;
        }
        let frame = match answer {
            Ok(result) => json!({"type": "response", "id": id, "ok": true, "result": result}),
            Err((code, message)) => json!({
                "type": "response",
                "id": id,
                "ok": false,
                "error": {"code": code, "message": bounded(&message, 1024)},
            }),
        };
        self.send(frame).await;
    }
}

/// Withdraws a host call that is dropped before its answer arrives.
struct PendingHostCall {
    host: Arc<Host>,
    id: String,
}

impl Drop for PendingHostCall {
    fn drop(&mut self) {
        if lock(&self.host.host_calls).remove(&self.id).is_some() {
            self.host
                .send_now(json!({"type": "hostCancel", "id": self.id}));
        }
    }
}

struct GuestState {
    limits: StoreLimits,
    host: Arc<Host>,
    logs: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GuestHostCall {
    op: String,
    #[serde(default)]
    params: Value,
}

struct Program {
    engine: Engine,
    pre: InstancePre<GuestState>,
    host: Arc<Host>,
    guest_memory: u64,
    fuel: u64,
}

fn link(engine: &Engine) -> Linker<GuestState> {
    let mut linker: Linker<GuestState> = Linker::new(engine);
    let defined = linker.func_wrap_async(
        BROKER_MODULE,
        "call",
        |mut caller: Caller<'_, GuestState>,
         (request_ptr, request_len, answer_ptr, answer_capacity): (i32, i32, i32, i32)| {
            Box::new(async move {
                let Some(memory) = caller
                    .get_export("memory")
                    .and_then(|export| export.into_memory())
                else {
                    return -2;
                };
                let request_len = request_len as u32 as usize;
                if request_len > PAYLOAD_LIMIT {
                    return -2;
                }
                let mut request = vec![0_u8; request_len];
                if memory
                    .read(&caller, request_ptr as u32 as usize, &mut request)
                    .is_err()
                {
                    return -2;
                }
                let Ok(request) = serde_json::from_slice::<GuestHostCall>(&request) else {
                    return -2;
                };
                let host = caller.data().host.clone();
                let Some(answer) = host.host_call(request.op, request.params).await else {
                    return -3;
                };
                let Ok(bytes) = serde_json::to_vec(&answer) else {
                    return -2;
                };
                if bytes.len() > answer_capacity as u32 as usize {
                    return -1;
                }
                if memory
                    .write(&mut caller, answer_ptr as u32 as usize, &bytes)
                    .is_err()
                {
                    return -2;
                }
                bytes.len() as i32
            })
        },
    );
    if let Err(error) = defined {
        fail(EXIT_MODULE_REFUSED, format!("broker import: {error}"));
    }
    let defined = linker.func_wrap(
        BROKER_MODULE,
        "log",
        |mut caller: Caller<'_, GuestState>, level: i32, ptr: i32, len: i32| {
            if caller.data().logs >= MAX_LOGS_PER_CALL {
                return;
            }
            caller.data_mut().logs += 1;
            let Some(memory) = caller
                .get_export("memory")
                .and_then(|export| export.into_memory())
            else {
                return;
            };
            let len = (len as u32 as usize).min(LOG_LIMIT);
            let mut bytes = vec![0_u8; len];
            if memory
                .read(&caller, ptr as u32 as usize, &mut bytes)
                .is_err()
            {
                return;
            }
            let level = match level {
                0 => "debug",
                1 => "info",
                2 => "warn",
                _ => "error",
            };
            caller.data().host.send_now(json!({
                "type": "log",
                "level": level,
                "message": String::from_utf8_lossy(&bytes),
            }));
        },
    );
    if let Err(error) = defined {
        fail(EXIT_MODULE_REFUSED, format!("broker import: {error}"));
    }
    linker
}

type CallError = (String, String);

fn trapped(error: impl std::fmt::Display) -> CallError {
    ("trapped".into(), bounded(&error.to_string(), 1024))
}

fn bad_result(message: impl Into<String>) -> CallError {
    ("badResult".into(), message.into())
}

/// Run one operation in a fresh store.
async fn invoke(program: &Program, method: &str, params: &Value) -> Result<Value, CallError> {
    let mut store = Store::new(
        &program.engine,
        GuestState {
            limits: StoreLimitsBuilder::new()
                .memory_size(program.guest_memory as usize)
                .memories(1)
                .instances(1)
                .tables(1)
                .table_elements(MAX_TABLE_ELEMENTS)
                .build(),
            host: program.host.clone(),
            logs: 0,
        },
    );
    store.limiter(|state| &mut state.limits);
    store.set_fuel(program.fuel).map_err(trapped)?;
    store
        .fuel_async_yield_interval(Some(FUEL_YIELD_INTERVAL))
        .map_err(trapped)?;
    let instance = program
        .pre
        .instantiate_async(&mut store)
        .await
        .map_err(trapped)?;
    let memory = instance
        .get_memory(&mut store, "memory")
        .ok_or_else(|| bad_result("the module exports no memory"))?;
    let input = serde_json::to_vec(params).map_err(|error| bad_result(error.to_string()))?;
    let alloc = instance
        .get_typed_func::<i32, i32>(&mut store, "luna_alloc")
        .map_err(trapped)?;
    let pointer = alloc
        .call_async(&mut store, input.len() as i32)
        .await
        .map_err(trapped)?;
    if pointer <= 0 {
        return Err(bad_result(
            "luna_alloc returned no memory for the parameters",
        ));
    }
    memory
        .write(&mut store, pointer as u32 as usize, &input)
        .map_err(|_| bad_result("luna_alloc returned memory outside the module"))?;
    let function = instance
        .get_typed_func::<(i32, i32), i64>(&mut store, &format!("{OP_PREFIX}{method}"))
        .map_err(trapped)?;
    let packed = function
        .call_async(&mut store, (pointer, input.len() as i32))
        .await
        .map_err(trapped)? as u64;
    let (address, length) = ((packed >> 32) as usize, (packed & 0xffff_ffff) as usize);
    if length > PAYLOAD_LIMIT {
        return Err((
            "tooLarge".into(),
            "the answer is larger than one frame".into(),
        ));
    }
    let mut answer = vec![0_u8; length];
    memory
        .read(&store, address, &mut answer)
        .map_err(|_| bad_result("the answer points outside the module's memory"))?;
    let answer: Value = serde_json::from_slice(&answer)
        .map_err(|error| bad_result(format!("the answer is not JSON: {error}")))?;
    if let Some(result) = answer.get("result") {
        return Ok(result.clone());
    }
    if let Some(error) = answer.get("error") {
        let code = error
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or("failed");
        let message = error.get("message").and_then(Value::as_str).unwrap_or("");
        return Err((bounded(code, 64), bounded(message, 1024)));
    }
    Err(bad_result("the answer has neither result nor error"))
}

async fn run_call(
    program: Arc<Program>,
    id: String,
    method: String,
    params: Value,
    deadline: Duration,
) {
    let backstop = deadline + DEADLINE_BACKSTOP;
    let answer = match tokio::time::timeout(backstop, invoke(&program, &method, &params)).await {
        Ok(answer) => answer,
        Err(_) => Err((
            "deadlineExceeded".into(),
            format!("the call ran past its {} ms deadline", deadline.as_millis()),
        )),
    };
    program.host.answer(&id, answer).await;
}

/// The operations a module offers, after checking its required exports.
fn operations(module: &Module) -> Result<Vec<String>, String> {
    let mut operations = Vec::new();
    let mut memory = false;
    let mut alloc = false;
    for export in module.exports() {
        let name = export.name();
        match export.ty() {
            ExternType::Memory(_) if name == "memory" => memory = true,
            ExternType::Func(func) if name == "luna_alloc" => {
                let params: Vec<ValType> = func.params().collect();
                let results: Vec<ValType> = func.results().collect();
                if params.len() != 1
                    || !params[0].is_i32()
                    || results.len() != 1
                    || !results[0].is_i32()
                {
                    return Err("luna_alloc must be (i32) -> i32".into());
                }
                alloc = true;
            }
            ExternType::Func(func) if name.starts_with(OP_PREFIX) => {
                let params: Vec<ValType> = func.params().collect();
                let results: Vec<ValType> = func.results().collect();
                if params.len() != 2
                    || !params.iter().all(ValType::is_i32)
                    || results.len() != 1
                    || !results[0].is_i64()
                {
                    return Err(format!("{name} must be (i32, i32) -> i64"));
                }
                let operation = &name[OP_PREFIX.len()..];
                if operation.is_empty() || operation.len() > 128 {
                    return Err(format!("{name} is not a valid operation name"));
                }
                operations.push(operation.to_owned());
            }
            _ => {}
        }
    }
    if !memory {
        return Err("the module must export its memory as `memory`".into());
    }
    if !alloc && !operations.is_empty() {
        return Err("the module must export luna_alloc".into());
    }
    if operations.len() > 256 {
        return Err("the module exports more than 256 operations".into());
    }
    Ok(operations)
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    let mut args = std::env::args().skip(1);
    let module_path = match (args.next().as_deref(), args.next(), args.next()) {
        (Some("--module"), Some(path), None) => path,
        _ => fail(EXIT_USAGE, "usage: luna-extension-host --module <path>"),
    };
    let size = std::fs::metadata(&module_path)
        .map(|metadata| metadata.len())
        .unwrap_or_else(|error| fail(EXIT_MODULE_REFUSED, format!("{module_path}: {error}")));
    if size > MAX_MODULE_BYTES {
        fail(EXIT_MODULE_REFUSED, "the module is larger than 8 MiB");
    }
    let bytes = std::fs::read(&module_path)
        .unwrap_or_else(|error| fail(EXIT_MODULE_REFUSED, format!("{module_path}: {error}")));
    // Hash exactly the bytes that will be compiled.
    let module_sha256: String = Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();

    let mut stdin = BufReader::new(tokio::io::stdin());
    let hello = match read_frame(&mut stdin).await {
        Ok(Some(frame)) => serde_json::from_slice::<Value>(&frame)
            .unwrap_or_else(|error| fail(EXIT_PROTOCOL, format!("the hello is not JSON: {error}"))),
        Ok(None) => std::process::exit(0),
        Err(error) => fail(EXIT_PROTOCOL, error),
    };
    if hello.get("type").and_then(Value::as_str) != Some("hello")
        || hello.pointer("/protocol/major").and_then(Value::as_u64) != Some(1)
    {
        fail(EXIT_PROTOCOL, "the first frame is not a protocol 1 hello");
    }
    let hello_id = hello.get("id").cloned().unwrap_or(Value::Null);
    let extension_id = hello
        .pointer("/instance/extensionId")
        .cloned()
        .unwrap_or(Value::Null);
    let package_hash = hello
        .pointer("/instance/packageHash")
        .cloned()
        .unwrap_or(Value::Null);
    let requested = |pointer: &str, ceiling: u64| {
        hello
            .pointer(pointer)
            .and_then(Value::as_u64)
            .filter(|value| *value > 0)
            .map_or(ceiling, |value| value.min(ceiling))
    };
    let guest_memory = requested("/portable/guestMemoryBytes", GUEST_MEMORY_CEILING);
    let fuel = requested("/portable/fuelPerCall", FUEL_CEILING);

    let mut config = Config::new();
    config.consume_fuel(true);
    config.max_wasm_stack(MAX_WASM_STACK);
    let engine = Engine::new(&config).unwrap_or_else(|error| {
        fail(
            EXIT_MODULE_REFUSED,
            format!("the runtime could not start: {error}"),
        )
    });
    let module = Module::from_binary(&engine, &bytes).unwrap_or_else(|error| {
        fail(
            EXIT_MODULE_REFUSED,
            format!("the module is invalid: {error}"),
        )
    });
    let imports: Vec<String> = module
        .imports()
        .map(|import| format!("{}::{}", import.module(), import.name()))
        .collect();
    let operations = operations(&module).unwrap_or_else(|error| fail(EXIT_MODULE_REFUSED, error));
    let linker = link(&engine);
    // Unknown imports (WASI, anything but luna_broker_v1) fail here.
    let pre = linker.instantiate_pre(&module).unwrap_or_else(|error| {
        fail(
            EXIT_MODULE_REFUSED,
            format!("the module is refused: {error}"),
        )
    });

    let (out, mut frames) = mpsc::channel::<Vec<u8>>(OUTBOUND_QUEUE);
    tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(frame) = frames.recv().await {
            if stdout.write_all(&frame).await.is_err() || stdout.flush().await.is_err() {
                std::process::exit(0);
            }
        }
    });
    let host = Arc::new(Host {
        out,
        host_calls: Mutex::new(HashMap::new()),
        calls: Mutex::new(HashMap::new()),
        next_host_call: AtomicU64::new(1),
    });
    let program = Arc::new(Program {
        engine,
        pre,
        host: host.clone(),
        guest_memory,
        fuel,
    });
    host.send(json!({
        "type": "hello",
        "id": hello_id,
        "protocol": {"major": 1, "minor": 0},
        "extensionId": extension_id,
        "packageHash": package_hash,
        "operations": operations,
        "portable": {
            "profile": {"major": 1, "minor": 0},
            "runtime": RUNTIME,
            "moduleSha256": module_sha256,
            "imports": imports,
        },
    }))
    .await;

    loop {
        let frame = match read_frame(&mut stdin).await {
            Ok(Some(frame)) => frame,
            Ok(None) => break,
            Err(error) => fail(EXIT_PROTOCOL, error),
        };
        let Ok(frame) = serde_json::from_slice::<Value>(&frame) else {
            fail(EXIT_PROTOCOL, "a frame from the engine is not JSON");
        };
        let id = frame
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_default();
        match frame.get("type").and_then(Value::as_str) {
            Some("call") => {
                let method = frame
                    .get("method")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                if !operations.contains(&method) {
                    host.send(json!({
                        "type": "response",
                        "id": id,
                        "ok": false,
                        "error": {
                            "code": "unknownOperation",
                            "message": format!("this module has no operation {}", bounded(&method, 128)),
                        },
                    }))
                    .await;
                    continue;
                }
                let params = frame.get("params").cloned().unwrap_or(Value::Null);
                let deadline = Duration::from_millis(
                    frame
                        .get("deadlineMs")
                        .and_then(Value::as_u64)
                        .unwrap_or(DEFAULT_DEADLINE_MS)
                        .clamp(1, MAX_DEADLINE_MS),
                );
                let mut calls = lock(&host.calls);
                let task = tokio::spawn(run_call(
                    program.clone(),
                    id.clone(),
                    method,
                    params,
                    deadline,
                ));
                calls.insert(id, task.abort_handle());
            }
            Some("cancel") => {
                let task = lock(&host.calls).remove(&id);
                if let Some(task) = task {
                    task.abort();
                    host.send(json!({
                        "type": "response",
                        "id": id,
                        "ok": false,
                        "error": {"code": "cancelled", "message": "cancelled by the host"},
                    }))
                    .await;
                }
            }
            Some("hostResult") => {
                let waiting = lock(&host.host_calls).remove(&id);
                if let Some(waiting) = waiting {
                    let mut answer = frame;
                    if let Some(object) = answer.as_object_mut() {
                        object.remove("type");
                        object.remove("id");
                    }
                    let _ = waiting.send(answer);
                }
            }
            Some("shutdown") => break,
            // Frames from a newer minor are ignored.
            _ => {}
        }
    }
}
