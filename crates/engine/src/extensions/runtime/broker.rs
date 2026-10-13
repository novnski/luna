//! The capability broker: the only LUNA authority a worker can reach.
//!
//! The principal (instance, package, version, grants) and the target (one
//! task's checkout, or none) are bound by the server when the instance
//! starts. A broker request carries only an operation and its parameters;
//! parameters refuse unknown fields, so a worker cannot name another task,
//! instance or grant. Each operation names the capability it needs.
//!
//! Workspace paths are relative to the bound task's checkout. Every component
//! is opened with `openat` and `O_NOFOLLOW`, so neither `..`, an absolute
//! path, nor a symbolic link (even one swapped in during the call) leaves the
//! checkout. A hard link that already exists inside the checkout is followed
//! like any file, as the checkout's owner made it.
//!
//! The `data.*` operations (CORE-19) use the same confined path walk below
//! the instance's data folder: the profile's current data generation for
//! its extension, or, for a migration instance, the copy being migrated.
//! The folder is bound by the server at start, like the task.
//!
//! These checks bound what LUNA brokers. A native worker keeps its own OS
//! access regardless; see `ExtensionIsolation`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use luna_proto::contract::ExtensionId;
use luna_proto::{
    EXTENSION_BROKER_OPERATIONS, ExtensionRuntimeKind, ExtensionTarget, ExtensionTrust,
    extension_broker_operation,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::WorkspaceHost;

/// Largest chunk one `workspace.read` returns (it must fit in one frame
/// after base64 encoding).
const READ_CHUNK: u32 = 32 * 1024;
/// Largest `workspace.write` payload.
const WRITE_LIMIT: usize = 32 * 1024;
const PATH_LIMIT: usize = 1024;
const SEGMENT_LIMIT: usize = 255;
const DEPTH_LIMIT: usize = 64;
const TASK_LIST_LIMIT: usize = 64;
const TITLE_LIMIT: usize = 80;

/// Who is calling: bound by the server at start, never by the worker.
pub(super) struct Principal {
    pub instance_id: String,
    pub extension_id: ExtensionId,
    pub version: String,
    pub package_hash: String,
    pub runtime: ExtensionRuntimeKind,
    /// Sorted, unique capability ids granted for this exact package hash.
    pub grants: Vec<String>,
    pub target: BoundTarget,
    /// The extension's data folder in this profile, when the package
    /// declares data (CORE-19).
    pub data_root: Option<PathBuf>,
    revoked: AtomicBool,
}

impl Principal {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        instance_id: String,
        extension_id: ExtensionId,
        version: String,
        package_hash: String,
        runtime: ExtensionRuntimeKind,
        grants: Vec<String>,
        target: BoundTarget,
        data_root: Option<PathBuf>,
    ) -> Self {
        Self {
            instance_id,
            extension_id,
            version,
            package_hash,
            runtime,
            grants,
            target,
            data_root,
            revoked: AtomicBool::new(false),
        }
    }

    pub fn trust(&self) -> ExtensionTrust {
        self.runtime.trust()
    }

    pub fn granted(&self, capability: &str) -> bool {
        self.grants.iter().any(|grant| grant == capability)
    }

    /// Withdraw the principal's authority; every later broker call fails.
    pub fn revoke(&self) {
        self.revoked.store(true, Ordering::SeqCst);
    }

    pub fn is_revoked(&self) -> bool {
        self.revoked.load(Ordering::SeqCst)
    }

    pub fn target_view(&self) -> ExtensionTarget {
        match &self.target {
            BoundTarget::None => ExtensionTarget::None,
            BoundTarget::Task { task_id, .. } => ExtensionTarget::Task {
                task_id: task_id.clone(),
            },
        }
    }
}

/// A target resolved by the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BoundTarget {
    None,
    Task {
        task_id: String,
        /// The task's checkout, canonical when the instance started.
        root: PathBuf,
    },
}

/// A refused or failed broker operation, as the worker sees it.
#[derive(Debug, Clone, Serialize)]
pub(super) struct BrokerError {
    pub code: &'static str,
    pub message: String,
}

impl BrokerError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn busy(message: impl Into<String>) -> Self {
        Self::new("busy", message)
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new("badRequest", message)
    }

    pub fn timeout(message: impl Into<String>) -> Self {
        Self::new("timeout", message)
    }

    fn outside(message: impl Into<String>) -> Self {
        Self::new("outsideTarget", message)
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self::new("notFound", message)
    }

    fn failed(message: impl Into<String>) -> Self {
        Self::new("failed", message)
    }

    /// Whether the broker refused authority (as opposed to a failed or busy
    /// operation).
    pub fn is_refusal(&self) -> bool {
        matches!(
            self.code,
            "denied"
                | "outsideTarget"
                | "badRequest"
                | "unknownOperation"
                | "noTarget"
                | "noData"
                | "targetChanged"
                | "revoked"
        )
    }
}

#[derive(Clone)]
pub(super) struct Broker {
    /// Shared by every clone. Released when the runtime shuts down, so no
    /// instance record or late task keeps the engine's registry alive.
    workspace: Arc<Mutex<Option<WorkspaceHost>>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReadParams {
    path: String,
    #[serde(default)]
    offset: u64,
    #[serde(default)]
    max_bytes: Option<u32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WriteParams {
    path: String,
    /// Base64 file contents.
    data: String,
    /// Create the file when it does not exist.
    #[serde(default)]
    create: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoParams {}

fn strict<T: serde::de::DeserializeOwned>(op: &str, params: Value) -> Result<T, BrokerError> {
    let params = if params.is_null() {
        Value::Object(Default::default())
    } else {
        params
    };
    serde_json::from_value(params)
        .map_err(|error| BrokerError::bad_request(format!("{op}: {error}")))
}

impl Broker {
    pub fn new(workspace: Option<WorkspaceHost>) -> Self {
        Self {
            workspace: Arc::new(Mutex::new(workspace)),
        }
    }

    fn workspace(&self) -> Result<WorkspaceHost, String> {
        self.workspace
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .ok_or_else(|| "tasks are unavailable on this engine".to_string())
    }

    /// Drop the registry handle; later task and workspace calls fail.
    pub fn release(&self) {
        self.workspace
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
    }

    /// Resolve the owner's chosen task into a target, at start.
    pub fn bind_task(&self, task_id: &str) -> Result<BoundTarget, String> {
        let workspace = self.workspace()?;
        let chat = workspace
            .chat(task_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("task {task_id} does not exist"))?;
        if chat.device_id != workspace.device_id() {
            return Err(format!(
                "task {task_id} runs on another device; start the extension there"
            ));
        }
        let cwd = chat
            .cwd
            .filter(|cwd| Path::new(cwd).is_absolute())
            .ok_or_else(|| format!("task {task_id} has no local checkout"))?;
        let root = Path::new(&cwd)
            .canonicalize()
            .map_err(|error| format!("task {task_id}'s checkout is unavailable: {error}"))?;
        if !root.is_dir() {
            return Err(format!("task {task_id}'s checkout is not a folder"));
        }
        Ok(BoundTarget::Task {
            task_id: task_id.to_owned(),
            root,
        })
    }

    /// The bound task's checkout, re-checked: the task still exists on this
    /// device and still has the same canonical checkout.
    fn task_root(&self, principal: &Principal) -> Result<PathBuf, BrokerError> {
        let BoundTarget::Task { task_id, root } = &principal.target else {
            return Err(BrokerError::new(
                "noTarget",
                "this instance has no task; workspace operations need one",
            ));
        };
        let changed =
            |why: &str| BrokerError::new("targetChanged", format!("task {task_id} {why}"));
        let workspace = self.workspace().map_err(BrokerError::failed)?;
        let chat = workspace
            .chat(task_id)
            .map_err(|error| BrokerError::failed(error.to_string()))?
            .ok_or_else(|| changed("no longer exists"))?;
        if chat.device_id != workspace.device_id() {
            return Err(changed("moved to another device"));
        }
        let current = chat
            .cwd
            .map(|cwd| Path::new(&cwd).canonicalize())
            .transpose()
            .map_err(|_| changed("has an unavailable checkout"))?;
        if current.as_deref() != Some(root.as_path()) {
            return Err(changed("now uses a different checkout"));
        }
        Ok(root.clone())
    }

    /// The instance's data folder, bound at start.
    fn data_root(principal: &Principal) -> Result<PathBuf, BrokerError> {
        principal.data_root.clone().ok_or_else(|| {
            BrokerError::new(
                "noData",
                "this package declares no data, so its instances have no data folder",
            )
        })
    }

    /// Run one broker operation for `principal`.
    pub async fn handle(
        &self,
        principal: Arc<Principal>,
        op: String,
        params: Value,
    ) -> Result<Value, BrokerError> {
        if principal.is_revoked() {
            return Err(BrokerError::new(
                "revoked",
                "this instance's authority was withdrawn",
            ));
        }
        let Some(operation) = extension_broker_operation(&op) else {
            return Err(BrokerError::new(
                "unknownOperation",
                format!(
                    "{} is not a broker operation",
                    super::protocol::bounded_text(&op, super::protocol::NAME_LIMIT)
                ),
            ));
        };
        if let Some(capability) = operation.capability
            && !principal.granted(capability)
        {
            return Err(BrokerError::new(
                "denied",
                format!("{op} needs {capability}, which this instance was not granted"),
            ));
        }
        match operation.name {
            "host.info" => {
                let NoParams {} = strict(&op, params)?;
                Ok(json!({
                    "instanceId": principal.instance_id,
                    "extensionId": principal.extension_id,
                    "version": principal.version,
                    "packageHash": principal.package_hash,
                    "trust": principal.trust(),
                    "grants": principal.grants,
                    "target": principal.target_view(),
                    "operations": EXTENSION_BROKER_OPERATIONS
                        .iter()
                        .map(|operation| operation.name)
                        .collect::<Vec<_>>(),
                }))
            }
            "workspace.read" => {
                let params: ReadParams = strict(&op, params)?;
                let root = self.task_root(&principal)?;
                let max = params.max_bytes.unwrap_or(READ_CHUNK).min(READ_CHUNK);
                run_blocking(move || confined::read(&root, &params.path, params.offset, max)).await
            }
            "workspace.write" => {
                let params: WriteParams = strict(&op, params)?;
                if params.data.len() > WRITE_LIMIT.div_ceil(3) * 4 + 4 {
                    return Err(BrokerError::new(
                        "tooLarge",
                        format!("workspace.write takes at most {} KiB", WRITE_LIMIT / 1024),
                    ));
                }
                let bytes = BASE64.decode(params.data.as_bytes()).map_err(|error| {
                    BrokerError::bad_request(format!("data is not base64: {error}"))
                })?;
                if bytes.len() > WRITE_LIMIT {
                    return Err(BrokerError::new(
                        "tooLarge",
                        format!("workspace.write takes at most {} KiB", WRITE_LIMIT / 1024),
                    ));
                }
                let root = self.task_root(&principal)?;
                run_blocking(move || confined::write(&root, &params.path, &bytes, params.create))
                    .await
            }
            "data.read" => {
                let params: ReadParams = strict(&op, params)?;
                let root = Self::data_root(&principal)?;
                let max = params.max_bytes.unwrap_or(READ_CHUNK).min(READ_CHUNK);
                run_blocking(move || confined::read(&root, &params.path, params.offset, max)).await
            }
            "data.write" => {
                let params: WriteParams = strict(&op, params)?;
                if params.data.len() > WRITE_LIMIT.div_ceil(3) * 4 + 4 {
                    return Err(BrokerError::new(
                        "tooLarge",
                        format!("data.write takes at most {} KiB", WRITE_LIMIT / 1024),
                    ));
                }
                let bytes = BASE64.decode(params.data.as_bytes()).map_err(|error| {
                    BrokerError::bad_request(format!("data is not base64: {error}"))
                })?;
                if bytes.len() > WRITE_LIMIT {
                    return Err(BrokerError::new(
                        "tooLarge",
                        format!("data.write takes at most {} KiB", WRITE_LIMIT / 1024),
                    ));
                }
                let root = Self::data_root(&principal)?;
                run_blocking(move || confined::write(&root, &params.path, &bytes, params.create))
                    .await
            }
            "tasks.list" => {
                let NoParams {} = strict(&op, params)?;
                let workspace = self.workspace().map_err(BrokerError::failed)?;
                let device = workspace.device_id().to_owned();
                let chats = workspace
                    .read_chats()
                    .map_err(|error| BrokerError::failed(error.to_string()))?;
                let local: Vec<_> = chats
                    .into_iter()
                    .filter(|chat| chat.device_id == device)
                    .collect();
                let truncated = local.len() > TASK_LIST_LIMIT;
                let tasks: Vec<Value> = local
                    .into_iter()
                    .take(TASK_LIST_LIMIT)
                    .map(|chat| {
                        json!({
                            "id": chat.id,
                            "title": chat.title.map(|title| super::protocol::bounded_text(&title, TITLE_LIMIT)),
                            "projectId": chat.space_id,
                            "archived": chat.archived,
                        })
                    })
                    .collect();
                Ok(json!({"tasks": tasks, "truncated": truncated}))
            }
            other => Err(BrokerError::new(
                "unknownOperation",
                format!("{other} is not implemented by this host"),
            )),
        }
    }
}

async fn run_blocking(
    work: impl FnOnce() -> Result<Value, BrokerError> + Send + 'static,
) -> Result<Value, BrokerError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| BrokerError::failed(format!("the operation stopped: {error}")))?
}

/// Split a workspace path into canonical components.
fn components(path: &str) -> Result<Vec<String>, BrokerError> {
    if path.is_empty() || path.len() > PATH_LIMIT {
        return Err(BrokerError::bad_request(format!(
            "a path is 1 to {PATH_LIMIT} bytes"
        )));
    }
    if path.starts_with('/') {
        return Err(BrokerError::outside(format!(
            "{path} is absolute; paths are relative to the task's checkout"
        )));
    }
    if path.contains('\0') || path.contains('\\') {
        return Err(BrokerError::bad_request(format!(
            "{path:?} must use `/` and contain no NUL"
        )));
    }
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() > DEPTH_LIMIT {
        return Err(BrokerError::bad_request(format!(
            "{path} is nested deeper than {DEPTH_LIMIT} levels"
        )));
    }
    if parts.contains(&"..") {
        return Err(BrokerError::outside(format!(
            "{path} leaves the task's checkout"
        )));
    }
    for part in &parts {
        if part.is_empty() || *part == "." {
            return Err(BrokerError::bad_request(format!("{path} is not canonical")));
        }
        if part.len() > SEGMENT_LIMIT {
            return Err(BrokerError::bad_request(format!(
                "a path segment is at most {SEGMENT_LIMIT} bytes"
            )));
        }
    }
    Ok(parts.into_iter().map(str::to_owned).collect())
}

#[cfg(unix)]
mod confined {
    use std::ffi::{CStr, CString};
    use std::fs::File;
    use std::io::{self, Write as _};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::fs::FileExt as _;
    use std::path::Path;

    use super::{BASE64, BrokerError, components};
    use base64::Engine as _;
    use serde_json::{Value, json};

    enum Kind {
        File { mode: libc::mode_t },
        Dir,
        Link,
        Other,
    }

    fn name(segment: &str) -> Result<CString, BrokerError> {
        CString::new(segment).map_err(|_| BrokerError::bad_request("a path contains NUL"))
    }

    fn os_error(what: &str, error: io::Error) -> BrokerError {
        if error.kind() == io::ErrorKind::NotFound {
            BrokerError::not_found(format!("{what} does not exist"))
        } else if error.raw_os_error() == Some(libc::ELOOP) {
            BrokerError::outside(format!(
                "{what} is a link; the broker does not follow links"
            ))
        } else {
            BrokerError::failed(format!("{what}: {error}"))
        }
    }

    /// What `name` is inside `dir`, without following a link.
    fn kind(dir: &OwnedFd, name: &CStr, label: &str) -> Result<Option<Kind>, BrokerError> {
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: `dir` is an open descriptor, `name` is NUL-terminated and
        // `stat` is a valid out-pointer.
        let result = unsafe {
            libc::fstatat(
                dir.as_raw_fd(),
                name.as_ptr(),
                &mut stat,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if result != 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::NotFound {
                return Ok(None);
            }
            return Err(os_error(label, error));
        }
        let format = stat.st_mode & libc::S_IFMT;
        Ok(Some(if format == libc::S_IFREG {
            Kind::File {
                mode: stat.st_mode & 0o7777,
            }
        } else if format == libc::S_IFDIR {
            Kind::Dir
        } else if format == libc::S_IFLNK {
            Kind::Link
        } else {
            Kind::Other
        }))
    }

    fn open_at(
        dir: &OwnedFd,
        name: &CStr,
        flags: libc::c_int,
        label: &str,
    ) -> Result<OwnedFd, BrokerError> {
        // SAFETY: `dir` is open and `name` is NUL-terminated. The mode
        // argument is only read with `O_CREAT`.
        let fd =
            unsafe { libc::openat(dir.as_raw_fd(), name.as_ptr(), flags, 0o600 as libc::c_uint) };
        if fd < 0 {
            return Err(os_error(label, io::Error::last_os_error()));
        }
        // SAFETY: `fd` was just returned by `openat` and is owned here.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    /// Open every folder in `segments` below `root`, refusing links.
    fn open_folders(root: &Path, segments: &[String]) -> Result<OwnedFd, BrokerError> {
        let mut dir = OwnedFd::from(File::open(root).map_err(|error| {
            BrokerError::failed(format!("the checkout is unavailable: {error}"))
        })?);
        let mut walked = String::new();
        for segment in segments {
            if !walked.is_empty() {
                walked.push('/');
            }
            walked.push_str(segment);
            let segment_name = name(segment)?;
            match kind(&dir, &segment_name, &walked)? {
                None => return Err(BrokerError::not_found(format!("{walked} does not exist"))),
                Some(Kind::Link) => {
                    return Err(BrokerError::outside(format!(
                        "{walked} is a link; the broker does not follow links"
                    )));
                }
                Some(Kind::Dir) => {}
                Some(_) => {
                    return Err(BrokerError::bad_request(format!(
                        "{walked} is not a folder"
                    )));
                }
            }
            dir = open_at(
                &dir,
                &segment_name,
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                &walked,
            )?;
        }
        Ok(dir)
    }

    pub(super) fn read(
        root: &Path,
        path: &str,
        offset: u64,
        max: u32,
    ) -> Result<Value, BrokerError> {
        let segments = components(path)?;
        let (folders, last) = segments.split_at(segments.len() - 1);
        let dir = open_folders(root, folders)?;
        let file_name = name(&last[0])?;
        match kind(&dir, &file_name, path)? {
            None => return Err(BrokerError::not_found(format!("{path} does not exist"))),
            Some(Kind::Link) => {
                return Err(BrokerError::outside(format!(
                    "{path} is a link; the broker does not follow links"
                )));
            }
            Some(Kind::File { .. }) => {}
            Some(_) => {
                return Err(BrokerError::bad_request(format!(
                    "{path} is not a regular file"
                )));
            }
        }
        // Non-blocking, so a FIFO swapped in cannot stall the call.
        let file = File::from(open_at(
            &dir,
            &file_name,
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            path,
        )?);
        let metadata = file
            .metadata()
            .map_err(|error| BrokerError::failed(format!("{path}: {error}")))?;
        if !metadata.is_file() {
            return Err(BrokerError::bad_request(format!(
                "{path} is not a regular file"
            )));
        }
        let size = metadata.len();
        if offset > size {
            return Err(BrokerError::bad_request(format!(
                "offset {offset} is past the end of {path} ({size} bytes)"
            )));
        }
        let wanted = (size - offset).min(u64::from(max)) as usize;
        let mut buffer = vec![0_u8; wanted];
        let mut filled = 0;
        while filled < wanted {
            let read = file
                .read_at(&mut buffer[filled..], offset + filled as u64)
                .map_err(|error| BrokerError::failed(format!("{path}: {error}")))?;
            if read == 0 {
                break;
            }
            filled += read;
        }
        buffer.truncate(filled);
        Ok(json!({
            "path": path,
            "size": size,
            "offset": offset,
            "data": BASE64.encode(&buffer),
            "eof": offset + filled as u64 >= size,
        }))
    }

    pub(super) fn write(
        root: &Path,
        path: &str,
        bytes: &[u8],
        create: bool,
    ) -> Result<Value, BrokerError> {
        let segments = components(path)?;
        let (folders, last) = segments.split_at(segments.len() - 1);
        let dir = open_folders(root, folders)?;
        let file_name = name(&last[0])?;
        let mode = match kind(&dir, &file_name, path)? {
            None if !create => {
                return Err(BrokerError::not_found(format!(
                    "{path} does not exist; pass create to make it"
                )));
            }
            None => 0o644,
            Some(Kind::File { mode }) => mode,
            Some(Kind::Link) => {
                return Err(BrokerError::outside(format!(
                    "{path} is a link; the broker does not follow links"
                )));
            }
            Some(_) => {
                return Err(BrokerError::bad_request(format!(
                    "{path} is not a regular file"
                )));
            }
        };
        let temp = format!(".luna-extension-{}.tmp", uuid::Uuid::new_v4());
        let temp_name = name(&temp)?;
        let mut file = File::from(open_at(
            &dir,
            &temp_name,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            path,
        )?);
        let result = (|| -> io::Result<()> {
            file.write_all(bytes)?;
            // SAFETY: `file` is an open descriptor owned here.
            if unsafe { libc::fchmod(file.as_raw_fd(), mode) } != 0 {
                return Err(io::Error::last_os_error());
            }
            file.sync_all()?;
            // SAFETY: both names are NUL-terminated and relative to `dir`.
            if unsafe {
                libc::renameat(
                    dir.as_raw_fd(),
                    temp_name.as_ptr(),
                    dir.as_raw_fd(),
                    file_name.as_ptr(),
                )
            } != 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        })();
        if let Err(error) = result {
            // SAFETY: as above; removing our own temp file.
            unsafe {
                libc::unlinkat(dir.as_raw_fd(), temp_name.as_ptr(), 0);
            }
            return Err(BrokerError::failed(format!(
                "{path} could not be written: {error}"
            )));
        }
        Ok(json!({"path": path, "size": bytes.len()}))
    }
}

#[cfg(not(unix))]
mod confined {
    use std::path::Path;

    use super::{BrokerError, components};
    use serde_json::Value;

    pub(super) fn read(_: &Path, path: &str, _: u64, _: u32) -> Result<Value, BrokerError> {
        components(path)?;
        Err(BrokerError::failed(
            "workspace access is not supported on this platform",
        ))
    }

    pub(super) fn write(_: &Path, path: &str, _: &[u8], _: bool) -> Result<Value, BrokerError> {
        components(path)?;
        Err(BrokerError::failed(
            "workspace access is not supported on this platform",
        ))
    }
}
