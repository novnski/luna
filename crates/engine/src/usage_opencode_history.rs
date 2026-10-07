//! OpenCode usage history for the Usage page (F-MOD-15), the same history
//! surface Codex and Claude use. Adapted from T3 Code's MIT
//! `apps/server/src/usage/opencodeUsageReader.ts` (@4ee6bfd5): a read-only
//! scan of OpenCode's local SQLite stores (`opencode.db` and channel copies
//! such as `opencode-dev.db`) plus pre-migration `storage/message/**.json`.
//!
//! Failure cases this reader covers:
//! - OCH-01 A busy or locked OpenCode database stalls the Usage page. Every
//!   connection is read-only with a 100 ms busy timeout; a busy store falls
//!   back to its last completed read and the source is marked partial.
//! - OCH-02 The reader writes to provider storage. Databases open with
//!   `SQLITE_OPEN_READ_ONLY` and never `CREATE`, so a missing file is never
//!   created and no pragma, migration or checkpoint is issued. A closed store
//!   (no `-wal`, `-shm` or `-journal`) opens immutable, so SQLite does not
//!   create WAL side files in the provider's directory either.
//! - OCH-03 Message text reaches LUNA memory or its cache. SQL selects only
//!   usage fields through `json_extract`; legacy files decode into a usage-only
//!   struct; the cache holds the shared usage `Record` alone.
//! - OCH-04 One malformed row aborts the whole query. Rows must pass
//!   `json_valid(data)` before any `json_extract`.
//! - OCH-05 The same message counts twice: v1 `message` and its v2 migration
//!   share an id, a migrated JSON file shadows its database row, or two data
//!   roots or channel databases hold one message. Records carry the
//!   `opencode:<message id>` dedupe key; database rows are pushed first and
//!   legacy files already present in a database are skipped.
//! - OCH-06 Forked sessions count copied history again. v2 forks copy rows
//!   with new ids and the original `time_created`; rows older than their fork
//!   session are excluded, matching `opencode stats` and LUNA's Codex fork
//!   suppression.
//! - OCH-07 Reasoning tokens are dropped or doubled. OpenCode reports them
//!   apart from output, so they are added to output and kept as its subset.
//! - OCH-08 Zero-cost subscription or unknown-rate rows show a provider $0.
//!   Only a positive finite cost is provider-reported; others are estimated by
//!   the shared price table like Codex and Claude records.
//! - OCH-09 A missing data directory is reported as a failure. No store found
//!   and no read error means `Missing`.
//! - OCH-10 A wider window after a narrower one shows only the first window.
//!   Database reads use the fixed retention window, independent of the
//!   requested period; the requested cutoff applies when records are added.
//! - OCH-11 Schema drift crashes or silently empties the source. Tables and
//!   columns are introspected; a store without either message table is partial.
//! - OCH-12 Symlink loops or huge legacy trees stall the scan. Symlinks are
//!   never followed and walks honor depth, file-count and time bounds.
//! - OCH-13 An unfinished read replaces good cached history. Reads cut short
//!   by the scan deadline are never cached.
//! - OCH-14 Rows without usage (user, model switches, a running compaction)
//!   count. v2 rows need `type = 'assistant'` or `type = 'compaction'`; v1
//!   and legacy rows need no other role.
//! - OCH-16 A compaction's own model request goes uncounted. OpenCode 2
//!   records it on the `compaction` row (`cost`, `tokens`, and `model` once
//!   completed), apart from any assistant row. Those rows count like
//!   assistant rows; a failed one without a model takes the session's latest
//!   earlier assistant model. Compactions migrated from v1 carry no usage
//!   (the v1 summary assistant row already counts), so nothing doubles.
//! - OCH-17 A QA app reads the user's store. Roots come from the storage
//!   environment of the OpenCode server LUNA would spawn
//!   (`luna_harness::opencode::OpencodeStorage`), so a profile wrapper's
//!   directories replace the user's, and a wrapper that cannot be evaluated
//!   makes the source unavailable instead of falling back to the home store.
//! - OCH-15 Duplicate or aliased roots count twice. Roots are canonicalized
//!   and deduplicated in order.

use std::collections::HashSet;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use luna_harness::opencode::OpencodeStorage;
use luna_proto::{UsageProvider, UsageSource, UsageSourceStatus, UsageTokens};
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags, ffi::ErrorCode};
use serde::Deserialize;
use serde_json::Value;

use super::{CachedFile, CodexState, MAX_FILES, Record, ScanCache, fingerprint, hash, modified_ns};

const PROVIDER: UsageProvider = UsageProvider::OpenCodeGo;
/// T3's comma-separated override for OpenCode data directories.
pub(super) const DATA_DIR_VARIABLE: &str = "OPENCODE_DATA_DIR";
pub(super) const BUSY_TIMEOUT: Duration = Duration::from_millis(100);
/// Pre-migration message files hold message metadata, not part text.
const MAX_LEGACY_FILE_BYTES: u64 = 4 * 1024 * 1024;
const DATABASE_GUARD: &str = "opencode-database";
const LEGACY_GUARD: &str = "opencode-legacy-message";

/// OPENCODE_DATA_DIR entries and the OpenCode data, config and database
/// directories the spawned server would use, for provider-home exclusion.
pub(super) fn configured_roots() -> Vec<PathBuf> {
    let storage = OpencodeStorage::current();
    let home = storage.home().ok().flatten().map(OsString::from);
    let cwd = std::env::current_dir().ok();
    let mut roots = split_roots(
        storage.var(DATA_DIR_VARIABLE).ok().flatten(),
        home.as_ref(),
        cwd.as_deref(),
    );
    roots.extend(storage.data_dir().ok().flatten());
    roots.extend(storage.config_dir().ok().flatten());
    roots.extend(storage.global_config_dir().ok().flatten());
    roots.extend(
        storage
            .database()
            .ok()
            .flatten()
            .and_then(|db| db.parent().map(Path::to_path_buf)),
    );
    roots
}

/// The OpenCode history roots the Usage page reads: those of the OpenCode
/// server LUNA would spawn now, so a QA app whose executable is a profile
/// wrapper reads the profile's store and never the user's (OCS-01).
pub(super) fn data_roots() -> Result<Vec<PathBuf>, String> {
    storage_roots(&OpencodeStorage::current())
}

/// T3's root resolution over the server's environment: OPENCODE_DATA_DIR
/// (comma list), otherwise the server's data root (`$XDG_DATA_HOME/opencode`,
/// else `~/.local/share/opencode`) plus the directory of an `OPENCODE_DB`
/// kept outside it. Canonical and deduplicated in order. Err when the
/// executable's wrapper moves storage in a way LUNA cannot evaluate.
pub(crate) fn storage_roots(storage: &OpencodeStorage) -> Result<Vec<PathBuf>, String> {
    let unknown = |_| {
        "OpenCode data directory could not be determined: the OpenCode executable changes its \
         storage environment in a way LUNA cannot evaluate"
            .to_owned()
    };
    let home = storage.home().map_err(unknown)?.map(OsString::from);
    let list = storage.var(DATA_DIR_VARIABLE).map_err(unknown)?;
    let mut roots = split_roots(list, home.as_ref(), std::env::current_dir().ok().as_deref());
    if roots.is_empty() {
        let data = storage.data_dir().map_err(unknown)?;
        roots.extend(data.clone());
        if let Some(parent) = storage
            .database()
            .map_err(unknown)?
            .filter(|db| {
                db.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(is_database_name)
            })
            .and_then(|db| db.parent().map(Path::to_path_buf))
            .filter(|parent| data.as_ref() != Some(parent))
        {
            roots.push(parent);
        }
    }
    let mut seen = HashSet::new();
    Ok(roots
        .into_iter()
        .map(|root| root.canonicalize().unwrap_or(root))
        .filter(|root| seen.insert(root.clone()))
        .collect())
}

fn split_roots(
    list: Option<OsString>,
    home: Option<&OsString>,
    cwd: Option<&Path>,
) -> Vec<PathBuf> {
    let Some(list) = list else {
        return Vec::new();
    };
    list.to_string_lossy()
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .filter_map(|entry| {
            let expanded = if entry == "~" {
                PathBuf::from(home?)
            } else if let Some(rest) = entry.strip_prefix("~/") {
                PathBuf::from(home?).join(rest)
            } else {
                PathBuf::from(entry)
            };
            if expanded.is_absolute() {
                Some(expanded)
            } else {
                cwd.map(|cwd| cwd.join(expanded))
            }
        })
        .collect()
}

#[derive(Default)]
struct Fields {
    id: String,
    session: String,
    role: Option<String>,
    model: String,
    created_ms: Option<f64>,
    cost: Option<f64>,
    input: Option<f64>,
    output: Option<f64>,
    reasoning: Option<f64>,
    cache_read: Option<f64>,
    cache_write: Option<f64>,
}

fn tokens(value: Option<f64>) -> u64 {
    value
        .filter(|value| value.is_finite() && *value > 0.0)
        .map_or(0, |value| value.trunc() as u64)
}

/// T3's `parseOpenCodeMessage`: OpenCode stores uncached input, cache reads,
/// cache writes, output and reasoning as separate counts.
fn usage_record(fields: Fields) -> Option<Record> {
    if fields
        .role
        .as_deref()
        .is_some_and(|role| role != "assistant")
        || fields.model.is_empty()
    {
        return None;
    }
    let at_ms = fields.created_ms.filter(|value| value.is_finite())? as i64;
    let reasoning = tokens(fields.reasoning);
    let usage = UsageTokens {
        uncached_input: tokens(fields.input),
        cached_input: tokens(fields.cache_read),
        cache_creation: tokens(fields.cache_write),
        output: tokens(fields.output).saturating_add(reasoning),
        reasoning,
    };
    (usage.total() > 0).then(|| Record {
        at_ms,
        model: fields.model,
        session: fields.session,
        tokens: usage,
        // OpenCode writes zero for models without a known rate, including
        // paid subscription models. Let the shared price table estimate them.
        cost_usd: fields.cost.filter(|cost| cost.is_finite() && *cost > 0.0),
        dedupe: (!fields.id.is_empty()).then(|| format!("opencode:{}", fields.id)),
        legacy_parser: false,
    })
}

enum ReadError {
    Busy,
    Schema,
    Other,
}

impl From<rusqlite::Error> for ReadError {
    fn from(error: rusqlite::Error) -> Self {
        match error.sqlite_error_code() {
            Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) => Self::Busy,
            _ => Self::Other,
        }
    }
}

fn sql_text(value: ValueRef<'_>) -> Option<String> {
    match value {
        ValueRef::Text(text) => Some(String::from_utf8_lossy(text).into_owned()),
        ValueRef::Null => None,
        // A present, non-string value is still not the expected string.
        _ => Some(String::new()),
    }
}

fn sql_number(value: ValueRef<'_>) -> Option<f64> {
    match value {
        ValueRef::Integer(value) => Some(value as f64),
        ValueRef::Real(value) => Some(value),
        _ => None,
    }
}

fn columns(db: &Connection, table: &str) -> Result<HashSet<String>, ReadError> {
    let mut statement = db.prepare(&format!("PRAGMA table_info({table})"))?;
    let names = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<HashSet<_>, _>>()?;
    Ok(names)
}

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Opens a store read-only. A read-only connection to a WAL database creates
/// `-wal`/`-shm` files when they are absent, which would write into the
/// provider's directory. OpenCode keeps them while it has the store open, so
/// without any writer side file the store is closed and is read as immutable.
pub(super) fn open_read_only(path: &Path) -> rusqlite::Result<Connection> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    if ["-wal", "-shm", "-journal"]
        .iter()
        .any(|suffix| sidecar(path, suffix).exists())
    {
        return Connection::open_with_flags(path, flags);
    }
    let mut uri = String::from("file:");
    for byte in path.as_os_str().as_encoded_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                uri.push(*byte as char)
            }
            _ => uri.push_str(&format!("%{byte:02X}")),
        }
    }
    uri.push_str("?mode=ro&immutable=1");
    Connection::open_with_flags(uri, flags | OpenFlags::SQLITE_OPEN_URI)
}

/// One complete or deadline-bounded read of a database's assistant usage.
fn read_database(
    path: &Path,
    since_ms: i64,
    deadline: Instant,
) -> Result<(Vec<Record>, bool), ReadError> {
    let db = open_read_only(path)?;
    // A busy live provider should fail this source promptly rather than
    // stalling the scan while SQLite waits for its writer.
    db.busy_timeout(BUSY_TIMEOUT)?;
    let tables = {
        let mut statement = db.prepare("SELECT name FROM sqlite_master WHERE type = 'table'")?;
        statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<HashSet<_>, _>>()?
    };
    if !tables.contains("session_message") && !tables.contains("message") {
        return Err(ReadError::Schema);
    }
    let forks = if tables.contains("session_v2") {
        let session = columns(&db, "session_v2")?;
        ["id", "fork_session_id", "time_created"]
            .iter()
            .all(|column| session.contains(*column))
    } else {
        false
    };
    let mut records = Vec::new();
    for table in ["session_message", "message"] {
        if !tables.contains(table) {
            continue;
        }
        let present = columns(&db, table)?;
        let v2 = table == "session_message";
        if !present.contains("data") || (v2 && !present.contains("type")) {
            return Err(ReadError::Schema);
        }
        let column = |name: &str| {
            if present.contains(name) {
                format!("m.{name}")
            } else {
                "NULL".to_owned()
            }
        };
        let timed = present.contains("time_created");
        let mut predicates = vec!["json_valid(m.data)".to_owned()];
        if v2 {
            predicates.push("m.type IN ('assistant', 'compaction')".into());
        }
        // OCH-16: a failed compaction has usage but no model; it ran on the
        // session's model, the latest earlier assistant row's.
        let fallback_model = if v2 && timed && present.contains("session_id") {
            "CASE WHEN m.type = 'compaction' AND json_extract(m.data, '$.model.id') IS NULL \
             THEN (SELECT json_extract(a.data, '$.model.id') FROM session_message a \
             WHERE a.session_id = m.session_id AND a.type = 'assistant' \
             AND a.time_created <= m.time_created AND json_valid(a.data) \
             ORDER BY a.time_created DESC LIMIT 1) END"
        } else {
            "NULL"
        };
        if timed {
            predicates.push("m.time_created >= ?1".into());
        }
        let mut join = String::new();
        if v2 && forks && timed && present.contains("session_id") {
            join = " LEFT JOIN session_v2 s ON s.id = m.session_id".into();
            predicates
                .push("(s.fork_session_id IS NULL OR m.time_created >= s.time_created)".into());
        }
        let sql = format!(
            "SELECT {}, {}, {}, \
             json_extract(m.data, '$.role'), json_extract(m.data, '$.model.id'), \
             json_extract(m.data, '$.model.modelID'), json_extract(m.data, '$.modelID'), \
             json_extract(m.data, '$.time.created'), json_extract(m.data, '$.cost'), \
             json_extract(m.data, '$.tokens.input'), json_extract(m.data, '$.tokens.output'), \
             json_extract(m.data, '$.tokens.reasoning'), \
             json_extract(m.data, '$.tokens.cache.read'), \
             json_extract(m.data, '$.tokens.cache.write'), \
             json_extract(m.data, '$.id'), json_extract(m.data, '$.sessionID'), \
             {fallback_model} \
             FROM {table} m{join} WHERE {}",
            column("id"),
            column("session_id"),
            column("time_created"),
            predicates.join(" AND "),
        );
        let mut statement = db.prepare(&sql)?;
        let mut rows = if timed {
            statement.query([since_ms])?
        } else {
            statement.query([])?
        };
        let mut count = 0_u64;
        while let Some(row) = rows.next()? {
            count += 1;
            if count.is_multiple_of(256) && Instant::now() >= deadline {
                return Ok((records, false));
            }
            let value = |index: usize| row.get_ref(index).unwrap_or(ValueRef::Null);
            let text = |index: usize| sql_text(value(index)).unwrap_or_default();
            let first_text = |indexes: &[usize]| {
                indexes
                    .iter()
                    .map(|index| text(*index))
                    .find(|text| !text.is_empty())
                    .unwrap_or_default()
            };
            let fields = Fields {
                id: first_text(&[0, 14]),
                session: first_text(&[1, 15]),
                role: sql_text(value(3)),
                model: first_text(&[4, 5, 6, 16]),
                created_ms: sql_number(value(7)).or_else(|| sql_number(value(2))),
                cost: sql_number(value(8)),
                input: sql_number(value(9)),
                output: sql_number(value(10)),
                reasoning: sql_number(value(11)),
                cache_read: sql_number(value(12)),
                cache_write: sql_number(value(13)),
            };
            records.extend(usage_record(fields));
        }
    }
    Ok((records, true))
}

/// Usage-only view of a pre-migration message file. Unknown fields, including
/// any text, are skipped by the decoder and never retained.
#[derive(Deserialize)]
struct LegacyMessage {
    id: Option<Value>,
    #[serde(rename = "sessionID")]
    session: Option<Value>,
    role: Option<Value>,
    model: Option<Value>,
    #[serde(rename = "modelID")]
    model_id: Option<Value>,
    time: Option<Value>,
    cost: Option<Value>,
    tokens: Option<Value>,
}

fn parse_legacy(bytes: &[u8], file_id: &str) -> Option<Record> {
    let message: LegacyMessage = serde_json::from_slice(bytes).ok()?;
    let text = |value: Option<&Value>| value.and_then(Value::as_str).unwrap_or("").to_owned();
    let tokens = message.tokens.as_ref();
    let number = |pointer: &str| tokens.and_then(|tokens| tokens.pointer(pointer)?.as_f64());
    let model = message.model.as_ref();
    let model = [
        text(model.and_then(|model| model.get("id"))),
        text(model.and_then(|model| model.get("modelID"))),
        text(message.model_id.as_ref()),
    ]
    .into_iter()
    .find(|model| !model.is_empty())
    .unwrap_or_default();
    let id = if file_id.is_empty() {
        text(message.id.as_ref())
    } else {
        file_id.to_owned()
    };
    usage_record(Fields {
        id,
        session: text(message.session.as_ref()),
        role: message
            .role
            .map(|role| role.as_str().unwrap_or("").to_owned()),
        model,
        created_ms: message
            .time
            .as_ref()
            .and_then(|time| time.get("created")?.as_f64()),
        cost: message.cost.as_ref().and_then(Value::as_f64),
        input: number("/input"),
        output: number("/output"),
        reasoning: number("/reasoning"),
        cache_read: number("/cache/read"),
        cache_write: number("/cache/write"),
    })
}

fn is_database_name(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".db") else {
        return false;
    };
    stem == "opencode"
        || stem.strip_prefix("opencode-").is_some_and(|channel| {
            !channel.is_empty()
                && channel
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
        })
}

fn is_database_path(root: &Path, path: &Path) -> bool {
    path.parent() == Some(root)
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(is_database_name)
}

/// Size and modification time of a database and its write-ahead log. A
/// committed write changes at least one of them.
fn database_signature(path: &Path) -> Option<(u64, u128, String)> {
    let metadata = fs::metadata(path).ok()?;
    let wal = fs::metadata(sidecar(path, "-wal"))
        .map(|meta| (meta.len(), modified_ns(&meta)))
        .unwrap_or((0, 0));
    Some((
        metadata.len(),
        modified_ns(&metadata),
        format!("wal:{}:{}", wal.0, wal.1),
    ))
}

fn databases(root: &Path) -> Result<Vec<PathBuf>, ()> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err(()),
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|_| ())?;
        // DirEntry file types do not follow symlinks.
        if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        if let Some(name) = entry.file_name().to_str()
            && is_database_name(name)
        {
            names.push(name.to_owned());
        }
    }
    names.sort_by(|left, right| {
        (left != "opencode.db")
            .cmp(&(right != "opencode.db"))
            .then_with(|| left.cmp(right))
    });
    Ok(names.into_iter().map(|name| root.join(name)).collect())
}

/// Legacy `storage/message/<session>/<message>.json` files modified since the
/// cutoff. Returns whether a bound or unreadable directory cut the walk short.
fn legacy_files(root: &Path, cutoff_ms: i64, deadline: Instant) -> (Vec<PathBuf>, bool) {
    let mut result = Vec::new();
    let mut partial = false;
    let mut stack = vec![(root.join("storage").join("message"), 0_usize)];
    while let Some((directory, depth)) = stack.pop() {
        if result.len() >= MAX_FILES || Instant::now() >= deadline {
            partial = true;
            break;
        }
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => {
                partial = true;
                continue;
            }
        };
        for entry in entries {
            if result.len() >= MAX_FILES {
                partial = true;
                break;
            }
            let Ok(entry) = entry else {
                partial = true;
                continue;
            };
            let Ok(kind) = entry.file_type() else {
                partial = true;
                continue;
            };
            let path = entry.path();
            if kind.is_dir() && depth < 4 {
                stack.push((path, depth + 1));
            } else if kind.is_file()
                && path.extension().is_some_and(|ext| ext == "json")
                && entry
                    .metadata()
                    .is_ok_and(|meta| super::modified_ms(&meta) as i64 >= cutoff_ms)
            {
                result.push(path);
            }
        }
    }
    result.sort();
    (result, partial)
}

fn cached_entry(
    size: u64,
    modified: u128,
    guard: String,
    kind: &str,
    records: Vec<Record>,
) -> CachedFile {
    CachedFile {
        size,
        modified_ns: modified,
        complete_offset: size,
        head_guard: guard,
        tail_guard: kind.to_owned(),
        records,
        codex: CodexState::default(),
        legacy_parser: false,
    }
}

#[derive(Default)]
struct Scan {
    found: bool,
    partial: Option<String>,
    files_scanned: u64,
    files_cached: u64,
    failed_files: u64,
}

fn push_records(records: &[Record], cutoff_ms: i64, all: &mut Vec<(UsageProvider, Record)>) {
    all.extend(
        records
            .iter()
            .filter(|record| record.at_ms >= cutoff_ms)
            .map(|record| (PROVIDER, record.clone())),
    );
}

fn scan_root(
    root: &Path,
    cutoff_ms: i64,
    since_ms: i64,
    deadline: Instant,
    cache: &mut ScanCache,
    all: &mut Vec<(UsageProvider, Record)>,
    scan: &mut Scan,
) {
    tracing::debug!(
        target: "luna_engine::usage",
        root = %root.display(),
        "reading OpenCode history root"
    );
    let mut processed = HashSet::new();
    let mut database_ids = HashSet::new();
    let paths = match databases(root) {
        Ok(paths) => paths,
        Err(()) => {
            scan.partial = Some("The OpenCode data directory could not be listed".into());
            Vec::new()
        }
    };
    for path in paths {
        scan.found = true;
        let key = format!("{PROVIDER:?}:{}", path.display());
        processed.insert(key.clone());
        let signature = database_signature(&path);
        let cached = cache.files.get(&key);
        if let (Some((size, modified, wal)), Some(cached)) = (signature.as_ref(), cached)
            && cached.tail_guard == DATABASE_GUARD
            && cached.size == *size
            && cached.modified_ns == *modified
            && *modified > 0
            && cached.head_guard == *wal
        {
            scan.files_cached += 1;
            database_ids.extend(
                cached
                    .records
                    .iter()
                    .filter_map(|record| record.dedupe.clone()),
            );
            push_records(&cached.records, cutoff_ms, all);
            continue;
        }
        let has_cached = cache.files.contains_key(&key);
        let (reason, failed) = match read_database(&path, since_ms, deadline) {
            Ok((records, true)) => {
                scan.files_scanned += 1;
                database_ids.extend(records.iter().filter_map(|record| record.dedupe.clone()));
                push_records(&records, cutoff_ms, all);
                if let Some((size, modified, wal)) = signature {
                    cache.files.insert(
                        key,
                        cached_entry(size, modified, wal, DATABASE_GUARD, records),
                    );
                }
                continue;
            }
            // Never cache an unfinished read, nor prefer it to a complete one.
            Ok((records, false)) => {
                scan.files_scanned += 1;
                if !has_cached {
                    database_ids.extend(records.iter().filter_map(|record| record.dedupe.clone()));
                    push_records(&records, cutoff_ms, all);
                }
                ("OpenCode history scan reached its time bound", false)
            }
            Err(ReadError::Busy) => (
                if has_cached {
                    "OpenCode database was busy; showing its last completed read"
                } else {
                    "OpenCode database was busy; its history will be read on a later refresh"
                },
                !has_cached,
            ),
            Err(ReadError::Schema) => {
                ("An OpenCode database has no recognized message table", true)
            }
            Err(ReadError::Other) => ("Some OpenCode history could not be read", true),
        };
        scan.partial = Some(reason.into());
        scan.failed_files += failed as u64;
        if let Some(cached) = cache.files.get(&key) {
            scan.files_cached += 1;
            database_ids.extend(
                cached
                    .records
                    .iter()
                    .filter_map(|record| record.dedupe.clone()),
            );
            push_records(&cached.records, cutoff_ms, all);
        }
    }

    // T3 slack: a file modified around local midnight may hold in-window rows.
    let (files, bounded) = legacy_files(root, cutoff_ms - 36 * 3_600_000, deadline);
    if bounded {
        scan.partial = Some("OpenCode legacy history scan reached its bound".into());
    }
    for path in files {
        scan.found = true;
        let id = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("")
            .to_owned();
        // Database records win over their old JSON copies.
        if !id.is_empty() && database_ids.contains(&format!("opencode:{id}")) {
            continue;
        }
        if Instant::now() >= deadline {
            scan.partial = Some("OpenCode legacy history scan reached its bound".into());
            break;
        }
        let key = format!("{PROVIDER:?}:{}", path.display());
        // A file removed since the listing keeps its cached history below.
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        processed.insert(key.clone());
        let modified = modified_ns(&metadata);
        if let Some(cached) = cache.files.get(&key).filter(|cached| {
            cached.tail_guard == LEGACY_GUARD
                && cached.size == metadata.len()
                && cached.modified_ns == modified
                && modified > 0
        }) {
            scan.files_cached += 1;
            push_records(&cached.records, cutoff_ms, all);
            continue;
        }
        if metadata.len() > MAX_LEGACY_FILE_BYTES {
            processed.remove(&key);
            scan.failed_files += 1;
            scan.partial = Some("Some OpenCode history could not be read".into());
            continue;
        }
        match fs::read(&path) {
            Ok(bytes) => {
                scan.files_scanned += 1;
                let records: Vec<Record> = parse_legacy(&bytes, &id).into_iter().collect();
                push_records(&records, cutoff_ms, all);
                cache.files.insert(
                    key,
                    cached_entry(
                        metadata.len(),
                        modified,
                        String::new(),
                        LEGACY_GUARD,
                        records,
                    ),
                );
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                processed.remove(&key);
            }
            Err(_) => {
                processed.remove(&key);
                scan.failed_files += 1;
                scan.partial = Some("Some OpenCode history could not be read".into());
            }
        }
    }

    // Like transcripts, keep previously read history whose store has gone
    // away or was not reached this time; the cache is pruned by record age.
    // Only this root's own stores count, so nested roots cannot repeat them.
    let prefix = format!("{PROVIDER:?}:");
    let legacy_root = root.join("storage").join("message");
    for (key, cached) in &cache.files {
        let Some(path) = key.strip_prefix(&prefix) else {
            continue;
        };
        let path = Path::new(path);
        if processed.contains(key)
            || !(is_database_path(root, path) || path.starts_with(&legacy_root))
        {
            continue;
        }
        if cached.tail_guard == LEGACY_GUARD
            && cached.records.iter().any(|record| {
                record
                    .dedupe
                    .as_ref()
                    .is_some_and(|key| database_ids.contains(key))
            })
        {
            continue;
        }
        scan.files_cached += 1;
        push_records(&cached.records, cutoff_ms, all);
    }
}

/// The OpenCode history source. `roots` is `Ok(empty)` when no data directory
/// is configured, and `Err` when a configured fixture was refused.
pub(super) fn add_source(
    roots: Result<Vec<PathBuf>, String>,
    cutoff_ms: i64,
    since_ms: i64,
    deadline: Instant,
    cache: &mut ScanCache,
    all: &mut Vec<(UsageProvider, Record)>,
) -> UsageSource {
    let mut source = UsageSource {
        provider: PROVIDER,
        environment_id: "local-synthetic".into(),
        fingerprint: String::new(),
        status: UsageSourceStatus::Unsupported,
        distinct_sessions: 0,
        files_scanned: 0,
        files_cached: 0,
        failed_files: 0,
        detail: Some("No isolated synthetic OpenCode data directory configured".into()),
    };
    let roots = match roots {
        Ok(roots) if !roots.is_empty() => roots,
        Ok(_) => return source,
        Err(reason) => {
            source.detail = Some(reason);
            return source;
        }
    };
    source.fingerprint = match roots.as_slice() {
        [root] => fingerprint(PROVIDER, root),
        roots => hash(
            roots
                .iter()
                .map(|root| fingerprint(PROVIDER, root))
                .collect::<Vec<_>>()
                .join("|")
                .as_bytes(),
        ),
    };
    let start = all.len();
    let mut scan = Scan::default();
    for root in &roots {
        scan_root(root, cutoff_ms, since_ms, deadline, cache, all, &mut scan);
    }
    let retained = all.len() > start;
    source.files_scanned = scan.files_scanned;
    source.files_cached = scan.files_cached;
    source.failed_files = scan.failed_files;
    source.distinct_sessions = all[start..]
        .iter()
        .filter(|(_, record)| !record.session.is_empty())
        .map(|(_, record)| record.session.as_str())
        .collect::<HashSet<_>>()
        .len() as u64;
    (source.status, source.detail) = match scan.partial {
        Some(reason) => (UsageSourceStatus::Partial, Some(reason)),
        None if !scan.found && retained => (
            UsageSourceStatus::Partial,
            Some("Cached history retained; OpenCode history stores are unavailable".into()),
        ),
        None if !scan.found => (
            UsageSourceStatus::Missing,
            Some("No OpenCode history database was found".into()),
        ),
        None => (UsageSourceStatus::Ready, None),
    };
    source
}
