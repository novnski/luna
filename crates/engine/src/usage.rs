//! L16 local historical usage and normalized limit snapshots. Historical
//! parser/cache behavior follows T3 Code's MIT transcript contracts, adapted
//! to Rust. Explicit synthetic fixture settings isolate native limit probes.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use zeron_harness::codex::native::CodexQuotaRead;
use zeron_proto::{
    CostQuality, LimitAvailability, USAGE_CONTRACT_VERSION, UsageBucket, UsageLimitSnapshot,
    UsagePriceRate, UsageProvider, UsageRequest, UsageSnapshot, UsageSource, UsageSourceStatus,
    UsageSummary, UsageTokens,
};

const CACHE_VERSION: u32 = 2;
const MAX_FILES: usize = 10_000;
// Codex rollouts can exceed 1 GiB. The scanner streams them and checkpoints
// completed prefixes, so bound total file size without rejecting real history.
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_SCAN_TIME: Duration = Duration::from_secs(4);
const LITELLM_RATES_URL: &str =
    "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";
const RATES_TTL_MS: i64 = 24 * 60 * 60 * 1000;
const SYNTHETIC_MARKER: &str = ".luna-usage-synthetic";
const SYNTHETIC_USAGE_VARIABLES: &[&str] = &[
    "LUNA_USAGE_SYNTHETIC_CODEX_HOME",
    "LUNA_USAGE_SYNTHETIC_CLAUDE_HOME",
    "LUNA_USAGE_SYNTHETIC_CACHE_DIR",
    "LUNA_USAGE_SYNTHETIC_PRICES_JSON",
    "LUNA_USAGE_SYNTHETIC_LIMITS_JSON",
];

fn synthetic_mode_with(configured: impl Fn(&str) -> bool) -> bool {
    SYNTHETIC_USAGE_VARIABLES
        .iter()
        .any(|name| configured(name))
}

/// An explicit fixture setting is an isolation boundary even when its path is
/// missing or malformed. Never fall through to a live account in that case.
pub fn synthetic_mode_requested() -> bool {
    synthetic_mode_with(|name| std::env::var_os(name).is_some())
}

pub fn live_codex_limit(read: CodexQuotaRead) -> UsageLimitSnapshot {
    let windows = read
        .windows
        .into_iter()
        .map(|window| {
            let base = match window.duration_minutes {
                Some(300) => "Five-hour".into(),
                Some(10_080) => "Weekly".into(),
                Some(minutes) if minutes % 60 == 0 => format!("{}-hour", minutes / 60),
                Some(minutes) => format!("{minutes}-minute"),
                None if window.position == "primary" => "Primary window".into(),
                None => "Secondary window".into(),
            };
            let label = if window.limit_id == "codex" {
                base
            } else {
                format!(
                    "{} · {base}",
                    window.limit_name.as_deref().unwrap_or(&window.limit_id)
                )
            };
            zeron_proto::UsageLimitWindow {
                id: format!("{}/{}", window.limit_id, window.position),
                label,
                used_percent: window.used_percent,
                resets_at_ms: window
                    .resets_at_seconds
                    .and_then(|seconds| seconds.checked_mul(1000)),
                duration_minutes: window.duration_minutes,
            }
        })
        .collect();
    UsageLimitSnapshot {
        provider: UsageProvider::Codex,
        verified_account_id: read.verified_account_id,
        account_label: Some("Selected Codex account".into()),
        plan: read.plan,
        checked_at_ms: Some(read.checked_at_ms),
        availability: LimitAvailability::Ready,
        windows,
        reset_credits: read.reset_credits,
        detail: Some(
            "Read-only Codex app-server account/rateLimits/read · selected machine".into(),
        ),
    }
}

/// T3's Claude `get_usage` mapping over the same read-only OAuth usage body
/// already consumed by LUNA Accounts. Omitted windows stay omitted.
pub fn live_claude_limit(
    body: &Value,
    verified_account_id: Option<String>,
    plan: Option<String>,
) -> Option<UsageLimitSnapshot> {
    let mut windows = Vec::new();
    let mut push_window = |id: String, label: String, duration_minutes: u64, row: &Value| {
        let Some(percent) = row.get("utilization").and_then(Value::as_f64) else {
            return;
        };
        if !percent.is_finite() || percent < 0.0 {
            return;
        }
        windows.push(zeron_proto::UsageLimitWindow {
            id,
            label,
            used_percent: percent.min(100.0) as f32,
            resets_at_ms: row
                .get("resets_at")
                .and_then(Value::as_str)
                .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                .map(|value| value.timestamp_millis()),
            duration_minutes: Some(duration_minutes),
        });
    };
    for (id, label, duration) in [
        ("five_hour", "Five-hour", 300),
        ("seven_day", "Weekly", 10_080),
    ] {
        if let Some(row) = body.get(id) {
            push_window(id.into(), label.into(), duration, row);
        }
    }
    if let Some(scoped) = body.get("model_scoped").and_then(Value::as_array) {
        for row in scoped {
            let Some(name) = row.get("display_name").and_then(Value::as_str) else {
                continue;
            };
            if name.trim().is_empty() {
                continue;
            }
            let slug = name
                .chars()
                .map(|ch| {
                    if ch.is_ascii_alphanumeric() {
                        ch.to_ascii_lowercase()
                    } else {
                        '_'
                    }
                })
                .collect::<String>();
            push_window(
                format!("seven_day_{}", slug.trim_matches('_')),
                format!("Weekly · {name}"),
                10_080,
                row,
            );
        }
    }
    (!windows.is_empty()).then_some(UsageLimitSnapshot {
        provider: UsageProvider::Claude,
        verified_account_id,
        account_label: Some("Selected Claude account".into()),
        plan,
        checked_at_ms: Some(now_ms()),
        availability: LimitAvailability::Ready,
        windows,
        reset_credits: None,
        detail: Some("Read-only Claude usage endpoint · selected machine".into()),
    })
}

#[derive(Clone, Serialize, Deserialize)]
struct Record {
    at_ms: i64,
    model: String,
    session: String,
    tokens: UsageTokens,
    cost_usd: Option<f64>,
    dedupe: Option<String>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct CodexState {
    model: String,
    session: String,
    last_signature: Option<String>,
    saw_meta: bool,
    suppress_fork_copy: bool,
    fork_anchor_ms: i64,
}

#[derive(Clone, Serialize, Deserialize)]
struct CachedFile {
    size: u64,
    modified_ns: u128,
    complete_offset: u64,
    head_guard: String,
    tail_guard: String,
    records: Vec<Record>,
    codex: CodexState,
}

#[derive(Default, Serialize, Deserialize)]
struct ScanCache {
    version: u32,
    files: HashMap<String, CachedFile>,
}

#[derive(Serialize, Deserialize)]
struct LiveRatesCache {
    fetched_at_ms: i64,
    rates: HashMap<String, UsagePriceRate>,
}

fn price_cost(rate: UsagePriceRate, tokens: UsageTokens) -> f64 {
    (tokens.uncached_input as f64 * rate.input_per_million
        + tokens.cached_input as f64 * rate.cached_per_million
        + tokens.cache_creation as f64 * rate.creation_per_million
        + tokens.output as f64 * rate.output_per_million)
        / 1_000_000.0
}

fn modeled_cache_savings(rate: UsagePriceRate, tokens: UsageTokens) -> f64 {
    tokens.cached_input as f64 * (rate.input_per_million - rate.cached_per_million).max(0.0)
        / 1_000_000.0
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn int(value: Option<&Value>) -> u64 {
    value.and_then(Value::as_u64).unwrap_or(0)
}

fn timestamp(value: Option<&Value>) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value?.as_str()?)
        .ok()
        .map(|at| at.timestamp_millis())
}

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn parse_claude(line: &str) -> Option<Record> {
    let row: Value = serde_json::from_str(line).ok()?;
    if row.get("type")?.as_str()? != "assistant" {
        return None;
    }
    let message = row.get("message")?;
    let usage = message.get("usage")?;
    let model = message.get("model")?.as_str()?.to_owned();
    if model.is_empty() {
        return None;
    }
    let message_id = message.get("id").and_then(Value::as_str);
    let request_id = row.get("requestId").and_then(Value::as_str);
    let dedupe = (message_id.is_some() || request_id.is_some())
        .then(|| format!("{}:{}", message_id.unwrap_or(""), request_id.unwrap_or("")));
    let cost_usd = row
        .get("costUSD")
        .and_then(Value::as_f64)
        .filter(|cost| cost.is_finite() && *cost >= 0.0);
    Some(Record {
        at_ms: timestamp(row.get("timestamp"))?,
        model,
        session: row
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        tokens: UsageTokens {
            uncached_input: int(usage.get("input_tokens")),
            cached_input: int(usage.get("cache_read_input_tokens")),
            cache_creation: int(usage.get("cache_creation_input_tokens")),
            output: int(usage.get("output_tokens")),
            reasoning: 0,
        },
        cost_usd,
        dedupe,
    })
}

fn parse_codex(line: &str, state: &mut CodexState) -> Option<Record> {
    let row: Value = serde_json::from_str(line).ok()?;
    let payload = row.get("payload")?;
    match row.get("type").and_then(Value::as_str) {
        Some("session_meta") => {
            if !state.saw_meta {
                state.saw_meta = true;
                state.session = payload
                    .get("id")
                    .or_else(|| payload.get("session_id"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let forked = payload.get("forked_from_id").is_some()
                    || payload
                        .pointer("/source/subagent/thread_spawn/parent_thread_id")
                        .is_some();
                if forked && let Some(at) = timestamp(row.get("timestamp")) {
                    state.suppress_fork_copy = true;
                    state.fork_anchor_ms = at;
                }
            }
            return None;
        }
        Some("turn_context") => {
            if let Some(model) = payload.get("model").and_then(Value::as_str) {
                state.model = model.to_owned();
            }
            return None;
        }
        _ => {}
    }
    if payload.get("type")?.as_str()? != "token_count" {
        return None;
    }
    let usage = payload.pointer("/info/last_token_usage")?;
    let at_ms = timestamp(row.get("timestamp"))?;
    if state.model.is_empty() {
        return None;
    }
    let signature = serde_json::to_string(usage).ok()?;
    if state.last_signature.as_deref() == Some(&signature) {
        return None;
    }
    state.last_signature = Some(signature);
    if state.suppress_fork_copy {
        if at_ms.saturating_sub(state.fork_anchor_ms) < 1_000 {
            state.fork_anchor_ms = at_ms;
            return None;
        }
        state.suppress_fork_copy = false;
    }
    let input = int(usage.get("input_tokens"));
    let cached = int(usage.get("cached_input_tokens"));
    let creation = int(usage.get("cache_write_input_tokens"));
    let output = int(usage.get("output_tokens"));
    let tokens = UsageTokens {
        uncached_input: input.saturating_sub(cached).saturating_sub(creation),
        cached_input: cached,
        cache_creation: creation,
        output,
        reasoning: int(usage.get("reasoning_output_tokens")).min(output),
    };
    (tokens.total() > 0).then(|| Record {
        at_ms,
        model: state.model.clone(),
        session: state.session.clone(),
        tokens,
        cost_usd: None,
        dedupe: None,
    })
}

fn modified_ms(metadata: &fs::Metadata) -> u128 {
    metadata
        .modified()
        .ok()
        .and_then(|at| at.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_millis())
}

fn modified_ns(metadata: &fs::Metadata) -> u128 {
    metadata
        .modified()
        .ok()
        .and_then(|at| at.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_nanos())
}

fn guard(path: &Path, offset: u64) -> std::io::Result<(String, String)> {
    let mut file = File::open(path)?;
    let mut head = vec![0; offset.min(64) as usize];
    file.read_exact(&mut head)?;
    let mut tail = vec![0; offset.min(64) as usize];
    file.seek(SeekFrom::Start(offset.saturating_sub(64)))?;
    file.read_exact(&mut tail)?;
    Ok((hash(&head), hash(&tail)))
}

fn scan_file(
    path: &Path,
    provider: UsageProvider,
    previous: Option<&CachedFile>,
    deadline: Instant,
) -> std::io::Result<(CachedFile, bool)> {
    if provider == UsageProvider::OpenCodeGo {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "OpenCode history has no verified synthetic transcript adapter",
        ));
    }
    let metadata = fs::metadata(path)?;
    if metadata.len() > MAX_FILE_BYTES {
        return Err(std::io::Error::other("file exceeds bounded scan size"));
    }
    let size = metadata.len();
    let modified = modified_ns(&metadata);
    let old = previous.filter(|old| {
        old.complete_offset <= size
            && guard(path, old.complete_offset)
                .is_ok_and(|(head, tail)| head == old.head_guard && tail == old.tail_guard)
    });
    if let Some(old) = old
        && modified > 0
        && old.size == size
        && old.modified_ns == modified
        && old.complete_offset == size
    {
        return Ok((old.clone(), true));
    }
    // A same-size rewrite must replay from byte zero even if the short guard
    // happened to match. Only actual growth can safely resume at the offset.
    let mut result = old
        .filter(|old| {
            size > old.size
                || (size == old.size && old.complete_offset < size && old.modified_ns == modified)
        })
        .cloned()
        .unwrap_or_else(|| CachedFile {
            size: 0,
            modified_ns: 0,
            complete_offset: 0,
            head_guard: String::new(),
            tail_guard: String::new(),
            records: Vec::new(),
            codex: CodexState::default(),
        });
    let mut reader = BufReader::new(File::open(path)?);
    reader.seek(SeekFrom::Start(result.complete_offset))?;
    let mut line = String::new();
    loop {
        if Instant::now() >= deadline {
            // Persist the completed prefix. A subsequent refresh resumes from
            // this byte offset instead of repeating a large cold file.
            break;
        }
        line.clear();
        let start = reader.stream_position()?;
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        if !line.ends_with('\n') {
            // A writer may still be appending the final JSONL row.
            reader.seek(SeekFrom::Start(start))?;
            break;
        }
        result.complete_offset = reader.stream_position()?;
        let likely_usage = match provider {
            UsageProvider::Claude => line.contains("\"usage\""),
            UsageProvider::Codex => {
                line.contains("\"token_count\"")
                    || line.contains("\"turn_context\"")
                    || line.contains("\"session_meta\"")
            }
            UsageProvider::OpenCodeGo => false,
        };
        if !likely_usage {
            continue;
        }
        let record = match provider {
            UsageProvider::Claude => parse_claude(&line),
            UsageProvider::Codex => parse_codex(&line, &mut result.codex),
            UsageProvider::OpenCodeGo => None,
        };
        if let Some(record) = record {
            result.records.push(record);
        }
    }
    result.size = size;
    result.modified_ns = modified;
    (result.head_guard, result.tail_guard) = guard(path, result.complete_offset)?;
    Ok((result, false))
}

fn candidate_files(root: &Path, cutoff_ms: i64, deadline: Instant) -> (Vec<PathBuf>, bool) {
    let mut result = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0_usize)];
    let mut partial = false;
    while let Some((directory, depth)) = stack.pop() {
        if result.len() >= MAX_FILES || Instant::now() >= deadline {
            partial = true;
            break;
        }
        let entries = match fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(_) => {
                partial = true;
                continue;
            }
        };
        for entry in entries {
            if result.len() >= MAX_FILES || Instant::now() >= deadline {
                partial = true;
                break;
            }
            let Ok(entry) = entry else {
                partial = true;
                continue;
            };
            let Ok(meta) = entry.path().symlink_metadata() else {
                partial = true;
                continue;
            };
            if meta.file_type().is_symlink() {
                continue;
            }
            if meta.is_dir() && depth < 12 {
                stack.push((entry.path(), depth + 1));
            } else if meta.is_file()
                && entry.path().extension().is_some_and(|ext| ext == "jsonl")
                && modified_ms(&meta) as i64 >= cutoff_ms
            {
                result.push(entry.path());
            }
        }
    }
    result.sort();
    (result, partial)
}

fn actual_provider_homes() -> Vec<PathBuf> {
    let mut homes = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        homes.push(home.join(".codex"));
        homes.push(home.join(".claude"));
        homes.push(home.join(".config/opencode"));
        homes.push(home.join(".local/share/opencode"));
        homes.push(home.join(".cache/opencode"));
    }
    for variable in [
        "CODEX_HOME",
        "CLAUDE_CONFIG_DIR",
        "OPENCODE_CONFIG_DIR",
        "OPENCODE_HOME",
    ] {
        if let Some(path) = std::env::var_os(variable) {
            let path = PathBuf::from(path);
            if path.is_absolute() {
                homes.push(path);
            } else if let Ok(cwd) = std::env::current_dir() {
                homes.push(cwd.join(path));
            }
        }
    }
    for (variable, child) in [
        ("XDG_CONFIG_HOME", "opencode"),
        ("XDG_DATA_HOME", "opencode"),
        ("XDG_CACHE_HOME", "opencode"),
    ] {
        if let Some(path) = std::env::var_os(variable) {
            homes.push(PathBuf::from(path).join(child));
        }
    }
    homes
}

fn paths_overlap(left: &Path, right: &Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

fn safe_fixture_root_with_homes(
    value: Option<std::ffi::OsString>,
    actual_homes: &[PathBuf],
) -> Result<Option<PathBuf>, String> {
    let Some(value) = value else { return Ok(None) };
    let supplied = PathBuf::from(value);
    if !supplied.is_absolute() {
        return Err("Synthetic provider home must be an absolute path".into());
    }
    // Reject obvious overlap before accessing metadata of a candidate that
    // could itself be a real provider store.
    if actual_homes
        .iter()
        .any(|actual| paths_overlap(&supplied, actual))
    {
        return Err("Actual provider homes are excluded from L16 validation".into());
    }
    if !supplied.exists() {
        return Ok(Some(supplied));
    }
    let root = supplied
        .canonicalize()
        .map_err(|_| "Synthetic provider home is unreadable")?;
    // Resolve symlink aliases. Only path metadata is read here, never a
    // provider directory listing or transcript.
    if actual_homes.iter().any(|actual| {
        let actual = actual.canonicalize().unwrap_or_else(|_| actual.clone());
        paths_overlap(&root, &actual)
    }) {
        return Err("Actual provider homes are excluded from L16 validation".into());
    }
    if !root.join(SYNTHETIC_MARKER).is_file() {
        return Err("Synthetic provider home lacks its explicit marker".into());
    }
    Ok(Some(root))
}

fn safe_fixture_root(value: Option<std::ffi::OsString>) -> Result<Option<PathBuf>, String> {
    safe_fixture_root_with_homes(value, &actual_provider_homes())
}

fn fingerprint(provider: UsageProvider, root: &Path) -> String {
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    let volume = {
        #[cfg(unix)]
        {
            fs::metadata(root).map_or(0, |meta| meta.dev())
        }
        #[cfg(not(unix))]
        {
            0_u64
        }
    };
    hash(
        format!(
            "{}|{}|{}|{:?}",
            gethostname::gethostname().to_string_lossy(),
            root.display(),
            volume,
            provider
        )
        .as_bytes(),
    )
}

fn cache_path() -> Option<PathBuf> {
    let directory = std::env::var_os("LUNA_USAGE_SYNTHETIC_CACHE_DIR")?;
    let root = safe_fixture_root(Some(directory)).ok()??;
    for variable in [
        "LUNA_USAGE_SYNTHETIC_CODEX_HOME",
        "LUNA_USAGE_SYNTHETIC_CLAUDE_HOME",
    ] {
        if std::env::var_os(variable).is_some_and(|provider_home| {
            let provider_home = PathBuf::from(provider_home);
            provider_home.is_absolute()
                && root.starts_with(provider_home.canonicalize().unwrap_or(provider_home))
        }) {
            return None;
        }
    }
    let path = root.join("usage-scan-v2.json");
    safe_cache_file(&path).then_some(path)
}

fn safe_cache_file(path: &Path) -> bool {
    match path.symlink_metadata() {
        Ok(metadata) => metadata.file_type().is_file(),
        Err(error) => error.kind() == std::io::ErrorKind::NotFound,
    }
}

fn write_cache_bytes(path: &Path, bytes: &[u8]) {
    let Some(parent) = path.parent() else { return };
    if !safe_cache_file(path) {
        return;
    }
    let Ok(mut temporary) = tempfile::NamedTempFile::new_in(parent) else {
        return;
    };
    if temporary.write_all(bytes).is_ok() && temporary.as_file().sync_all().is_ok() {
        let _ = temporary.persist(path);
    }
}

fn synthetic_file(variable: &str) -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var_os(variable)?);
    let root = path.parent()?;
    // An explicit fixture file must sit directly in a marked synthetic folder.
    // This prevents a path into a native provider home from becoming a probe.
    let root = safe_fixture_root(Some(root.as_os_str().to_owned())).ok()??;
    let candidate = root.join(path.file_name()?);
    candidate
        .symlink_metadata()
        .is_ok_and(|meta| meta.file_type().is_file())
        .then_some(candidate)
}

fn prices() -> HashMap<String, (UsagePriceRate, CostQuality)> {
    synthetic_file("LUNA_USAGE_SYNTHETIC_PRICES_JSON")
        .and_then(|path| fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice::<HashMap<String, UsagePriceRate>>(&bytes).ok())
        .unwrap_or_default()
        .into_iter()
        .filter(|(model, rate)| !model.is_empty() && rate.valid())
        .map(|(model, rate)| (model, (rate, CostQuality::ModelPriced)))
        .collect()
}

fn parse_litellm_rates(document: &Value) -> HashMap<String, UsagePriceRate> {
    let mut rates = HashMap::new();
    let Some(entries) = document.as_object() else {
        return rates;
    };
    for (name, raw) in entries {
        let key = name.trim().to_ascii_lowercase();
        if key.is_empty() {
            continue;
        }
        let finite = |field: &str| {
            raw.get(field)
                .and_then(Value::as_f64)
                .filter(|value| value.is_finite() && *value >= 0.0)
        };
        let (Some(input), Some(output)) = (
            finite("input_cost_per_token"),
            finite("output_cost_per_token"),
        ) else {
            continue;
        };
        let rate = UsagePriceRate {
            input_per_million: input * 1_000_000.0,
            cached_per_million: finite("cache_read_input_token_cost").unwrap_or(input)
                * 1_000_000.0,
            creation_per_million: finite("cache_creation_input_token_cost").unwrap_or(input)
                * 1_000_000.0,
            output_per_million: output * 1_000_000.0,
        };
        if rate.valid() {
            rates.insert(key, rate);
        }
    }
    // T3 aliases a bare model only when all qualified entries agree on price.
    let mut aliases: HashMap<String, Option<UsagePriceRate>> = HashMap::new();
    for (key, rate) in &rates {
        let Some((_, bare)) = key.rsplit_once('/') else {
            continue;
        };
        if bare.is_empty() || rates.contains_key(bare) {
            continue;
        }
        aliases
            .entry(bare.to_owned())
            .and_modify(|held| {
                if held.is_some_and(|held| held != *rate) {
                    *held = None;
                }
            })
            .or_insert(Some(*rate));
    }
    rates.extend(
        aliases
            .into_iter()
            .filter_map(|(name, rate)| rate.map(|rate| (name, rate))),
    );
    rates
}

fn read_live_rates_cache(path: &Path) -> Option<LiveRatesCache> {
    let cache = fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<LiveRatesCache>(&bytes).ok())?;
    (cache.fetched_at_ms > 0 && !cache.rates.is_empty()).then_some(cache)
}

/// T3's 24-hour LiteLLM refresh, with the last on-disk copy retained on error.
/// The fixed public URL is the only network source; provider homes are untouched.
pub async fn refresh_live_prices(data_dir: &Path) {
    static REFRESH_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    let _guard = REFRESH_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let Some(path) = live_cache_file(data_dir, "usage-model-rates.json") else {
        return;
    };
    if read_live_rates_cache(&path)
        .is_some_and(|cache| now_ms().saturating_sub(cache.fetched_at_ms) < RATES_TTL_MS)
    {
        return;
    }
    let Ok(client) = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
    else {
        return;
    };
    let Ok(response) = client.get(LITELLM_RATES_URL).send().await else {
        return;
    };
    if !response.status().is_success()
        || response
            .content_length()
            .is_some_and(|len| len > 16 * 1024 * 1024)
    {
        return;
    }
    let Ok(bytes) = response.bytes().await else {
        return;
    };
    if bytes.len() > 16 * 1024 * 1024 {
        return;
    }
    let Ok(document) = serde_json::from_slice::<Value>(&bytes) else {
        return;
    };
    let rates = parse_litellm_rates(&document);
    if rates.is_empty() {
        return;
    }
    let Ok(encoded) = serde_json::to_vec(&LiveRatesCache {
        fetched_at_ms: now_ms(),
        rates,
    }) else {
        return;
    };
    write_cache_bytes(&path, &encoded);
}

fn live_prices(data_dir: &Path) -> HashMap<String, (UsagePriceRate, CostQuality)> {
    live_cache_file(data_dir, "usage-model-rates.json")
        .as_deref()
        .and_then(read_live_rates_cache)
        .map(|cache| {
            cache
                .rates
                .into_iter()
                .map(|(model, rate)| (model, (rate, CostQuality::ModelPriced)))
                .collect()
        })
        .unwrap_or_default()
}

fn live_pricing_warning(data_dir: &Path) -> Option<String> {
    let cached = live_cache_file(data_dir, "usage-model-rates.json")
        .as_deref()
        .and_then(read_live_rates_cache);
    match cached {
        None => Some(
            "LiteLLM model prices unavailable; unknown historical costs remain unpriced".into(),
        ),
        Some(cache) if now_ms().saturating_sub(cache.fetched_at_ms) >= RATES_TTL_MS => Some(
            "Using older cached LiteLLM model prices; refresh did not obtain a current table"
                .into(),
        ),
        Some(_) => None,
    }
}

fn lookup_price<'a>(
    prices: &'a HashMap<String, (UsagePriceRate, CostQuality)>,
    model: &str,
) -> Option<&'a (UsagePriceRate, CostQuality)> {
    if let Some(exact) = prices.get(model.trim()) {
        return Some(exact);
    }
    let normalized = model.trim().to_ascii_lowercase();
    let key = normalized.split('[').next().unwrap_or("");
    let bare = key.rsplit('/').next().unwrap_or("");
    if [
        "<synthetic>",
        "synthetic",
        "opus",
        "sonnet",
        "haiku",
        "fable",
    ]
    .contains(&bare)
    {
        return None;
    }
    prices.get(key)
}

fn load_cache(path: Option<&Path>) -> ScanCache {
    path.filter(|path| safe_cache_file(path))
        .and_then(|path| fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice::<ScanCache>(&bytes).ok())
        .filter(|cache| cache.version == CACHE_VERSION)
        .unwrap_or_default()
}

fn save_cache(path: Option<&Path>, cache: &ScanCache) {
    let Some(path) = path else { return };
    let Ok(bytes) = serde_json::to_vec(cache) else {
        return;
    };
    write_cache_bytes(path, &bytes);
}

fn add_source(
    provider: UsageProvider,
    variable: &str,
    cutoff_ms: i64,
    deadline: Instant,
    cache: &mut ScanCache,
    all: &mut Vec<(UsageProvider, Record)>,
) -> UsageSource {
    let configured = safe_fixture_root(std::env::var_os(variable));
    add_source_configured(provider, configured, cutoff_ms, deadline, cache, all)
}

fn add_source_configured(
    provider: UsageProvider,
    configured: Result<Option<PathBuf>, String>,
    cutoff_ms: i64,
    deadline: Instant,
    cache: &mut ScanCache,
    all: &mut Vec<(UsageProvider, Record)>,
) -> UsageSource {
    let mut source = UsageSource {
        provider,
        environment_id: "local-synthetic".into(),
        fingerprint: String::new(),
        status: UsageSourceStatus::Unsupported,
        distinct_sessions: 0,
        files_scanned: 0,
        files_cached: 0,
        failed_files: 0,
        detail: Some("No isolated synthetic provider home configured".into()),
    };
    let root = match configured {
        Ok(Some(root)) => root,
        Ok(None) => return source,
        Err(reason) => {
            source.detail = Some(reason);
            return source;
        }
    };
    if !root.exists() {
        source.status = UsageSourceStatus::Missing;
        source.detail = Some("Configured synthetic home is missing".into());
        return source;
    }
    source.fingerprint = fingerprint(provider, &root);
    source.status = UsageSourceStatus::Ready;
    source.detail = None;
    let (files, partial) = candidate_files(&root, cutoff_ms - 86_400_000, deadline);
    if partial {
        source.status = UsageSourceStatus::Partial;
        source.detail = Some("Scan time or file bound reached".into());
    }
    let mut sessions = HashSet::new();
    for path in files {
        if Instant::now() >= deadline {
            source.status = UsageSourceStatus::Partial;
            source.detail = Some("Scan time bound reached".into());
            break;
        }
        let key = format!("{:?}:{}", provider, path.display());
        match scan_file(&path, provider, cache.files.get(&key), deadline) {
            Ok((scanned, warm)) => {
                source.files_scanned += (!warm) as u64;
                source.files_cached += warm as u64;
                if scanned.complete_offset < scanned.size {
                    source.status = UsageSourceStatus::Partial;
                    source.detail = Some("Transcript scan paused at its time bound".into());
                }
                for record in &scanned.records {
                    if record.at_ms >= cutoff_ms {
                        if !record.session.is_empty() {
                            sessions.insert(record.session.clone());
                        }
                        all.push((provider, record.clone()));
                    }
                }
                cache.files.insert(key, scanned);
            }
            Err(_) => {
                source.failed_files += 1;
                source.status = UsageSourceStatus::Partial;
                source.detail = Some("One or more transcript files could not be read".into());
            }
        }
    }
    source.distinct_sessions = sessions.len() as u64;
    source
}

fn add_live_source(
    provider: UsageProvider,
    home: Option<&Path>,
    directory: &str,
    cutoff_ms: i64,
    deadline: Instant,
    cache: &mut ScanCache,
    all: &mut Vec<(UsageProvider, Record)>,
) -> UsageSource {
    let root = home.map(|home| {
        let root = home.join(directory);
        root.canonicalize().unwrap_or(root)
    });
    let mut source = add_source_configured(provider, Ok(root), cutoff_ms, deadline, cache, all);
    source.environment_id = "local".into();
    if source.status == UsageSourceStatus::Unsupported {
        source.detail = Some("Selected provider home is unavailable".into());
    } else if source.status == UsageSourceStatus::Missing {
        source.detail = Some("Provider transcript directory is missing".into());
    }
    source
}

fn live_cache_file(data_dir: &Path, name: &str) -> Option<PathBuf> {
    // The scan cache belongs to LUNA, never inside a provider's private home.
    let root = data_dir.canonicalize().ok()?;
    if actual_provider_homes().iter().any(|home| {
        let home = home.canonicalize().unwrap_or_else(|_| home.clone());
        paths_overlap(&root, &home)
    }) {
        return None;
    }
    let path = root.join(name);
    safe_cache_file(&path).then_some(path)
}

fn live_cache_path(data_dir: &Path) -> Option<PathBuf> {
    live_cache_file(data_dir, "usage-scan-v2.json")
}

fn unsupported_limit(provider: UsageProvider) -> UsageLimitSnapshot {
    UsageLimitSnapshot {
        provider,
        verified_account_id: None,
        account_label: None,
        plan: None,
        checked_at_ms: None,
        availability: LimitAvailability::Unsupported,
        windows: Vec::new(),
        reset_credits: None,
        detail: Some(match provider {
            UsageProvider::OpenCodeGo => "OpenCode Go limits require a marked, verified synthetic Go fixture; external OpenCode servers and other OpenCode/Zen spending are unsupported",
            _ => "Live account probes are disabled for isolated L16 validation",
        }.into()),
    }
}

fn with_unsupported_limits(mut items: Vec<UsageLimitSnapshot>) -> Vec<UsageLimitSnapshot> {
    for provider in [
        UsageProvider::Codex,
        UsageProvider::OpenCodeGo,
        UsageProvider::Claude,
    ] {
        if !items.iter().any(|item| item.provider == provider) {
            items.push(unsupported_limit(provider));
        }
    }
    items
}

fn normalize_limit_fixture(mut items: Vec<UsageLimitSnapshot>) -> Option<Vec<UsageLimitSnapshot>> {
    for item in &mut items {
        if item.availability != LimitAvailability::Ready || item.checked_at_ms.is_none() {
            return None;
        }
        if item.windows.iter().any(|window| {
            !window.used_percent.is_finite() || !(0.0..=100.0).contains(&window.used_percent)
        }) {
            return None;
        }
        if item.provider == UsageProvider::OpenCodeGo {
            if item
                .verified_account_id
                .as_deref()
                .is_none_or(|id| id.trim().is_empty())
                || item
                    .plan
                    .as_deref()
                    .is_none_or(|plan| plan.trim().is_empty())
                || item.windows.is_empty()
                || item
                    .windows
                    .iter()
                    .any(|window| window.duration_minutes == Some(0))
            {
                return None;
            }
            let mut kinds = HashSet::new();
            for window in &mut item.windows {
                let kind = window.id.to_ascii_lowercase().replace(['-', '_'], "");
                let (id, label) = match kind.as_str() {
                    "session" | "fivehour" => ("fiveHour", "Five-hour"),
                    "weekly" => ("weekly", "Weekly"),
                    "monthly" => ("monthly", "Monthly"),
                    _ => return None,
                };
                if !kinds.insert(id) {
                    return None;
                }
                window.id = id.into();
                window.label = label.into();
            }
            // A marked fixture is evidence for UI behavior, not a live login.
            item.reset_credits = None;
            item.detail =
                Some("Marked synthetic OpenCode Go quota fixture · no live account probe".into());
        }
    }
    Some(items)
}

fn default_limits() -> Vec<UsageLimitSnapshot> {
    [
        UsageProvider::Codex,
        UsageProvider::OpenCodeGo,
        UsageProvider::Claude,
    ]
    .into_iter()
    .map(unsupported_limit)
    .collect()
}

fn limits_from_fixture(path: &Path, last_good: Option<&Path>) -> Vec<UsageLimitSnapshot> {
    let last_good = last_good.filter(|path| safe_cache_file(path));
    let parsed = fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Vec<UsageLimitSnapshot>>(&bytes).ok())
        .and_then(normalize_limit_fixture);
    if let Some(items) = parsed {
        if let Some(path) = last_good
            && let Ok(bytes) = serde_json::to_vec(&items)
        {
            write_cache_bytes(path, &bytes);
        }
        return with_unsupported_limits(items);
    }
    if let Some(items) = last_good
        .and_then(|path| fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice::<Vec<UsageLimitSnapshot>>(&bytes).ok())
        .and_then(normalize_limit_fixture)
    {
        return with_unsupported_limits(
            items
                .into_iter()
                .map(|mut item| {
                    item.availability = LimitAvailability::ProbeFailed;
                    item.detail =
                        Some("Synthetic limit refresh failed; showing last good values".into());
                    item
                })
                .collect(),
        );
    }
    default_limits()
        .into_iter()
        .map(|mut item| {
            if item.provider != UsageProvider::OpenCodeGo {
                item.availability = LimitAvailability::ProbeFailed;
                item.detail =
                    Some("Marked synthetic limit fixture could not be read or validated".into());
            }
            item
        })
        .collect()
}

fn limits() -> Vec<UsageLimitSnapshot> {
    let Some(path) = synthetic_file("LUNA_USAGE_SYNTHETIC_LIMITS_JSON") else {
        return default_limits();
    };
    let last_good = cache_path().map(|path| path.with_file_name("usage-limits-last-good.json"));
    limits_from_fixture(&path, last_good.as_deref())
}

fn aggregate_records(
    records: Vec<(UsageProvider, Record)>,
    sources: &[UsageSource],
    days: u32,
    prices: &HashMap<String, (UsagePriceRate, CostQuality)>,
) -> Vec<UsageBucket> {
    let mut seen = HashSet::new();
    let mut buckets: BTreeMap<(i64, u8, String), (UsageBucket, HashSet<String>)> = BTreeMap::new();
    for (provider, record) in records {
        let Some(source) = sources.iter().find(|source| source.provider == provider) else {
            continue;
        };
        if let Some(key) = &record.dedupe
            && !seen.insert((source.fingerprint.clone(), provider as u8, key.clone()))
        {
            continue;
        }
        let width = if days == 1 { 3_600_000 } else { 86_400_000 };
        let start = record.at_ms.div_euclid(width) * width;
        let key = (start, provider as u8, record.model.clone());
        let (bucket, sessions) = buckets.entry(key).or_insert_with(|| {
            (
                UsageBucket {
                    start_ms: start,
                    provider,
                    model: record.model.clone(),
                    tokens: UsageTokens::default(),
                    records: 0,
                    sessions: 0,
                    api_equivalent_cost_usd: Some(0.0),
                    cache_savings_usd: Some(0.0),
                    cost_quality: CostQuality::ProviderReported,
                },
                HashSet::new(),
            )
        });
        bucket.tokens.add(record.tokens);
        bucket.records += 1;
        let rate = lookup_price(prices, &record.model).copied();
        let savings = if record.tokens.cached_input == 0 {
            Some(0.0)
        } else {
            rate.map(|(rate, _)| modeled_cache_savings(rate, record.tokens))
        };
        bucket.cache_savings_usd = bucket
            .cache_savings_usd
            .and_then(|known| savings.map(|saved| known + saved));
        if !record.session.is_empty() {
            sessions.insert(record.session);
        }
        // Match T3: an exact user override wins even over provider-reported
        // cost; otherwise the provider's own amount beats model estimates.
        let priced = match rate {
            Some((price, CostQuality::UserOverride)) => {
                Some((price_cost(price, record.tokens), CostQuality::UserOverride))
            }
            _ => record
                .cost_usd
                .map(|cost| (cost, CostQuality::ProviderReported))
                .or_else(|| {
                    rate.map(|(price, quality)| (price_cost(price, record.tokens), quality))
                }),
        };
        if let Some((cost, quality)) = priced {
            if let Some(total) = &mut bucket.api_equivalent_cost_usd {
                *total += cost;
            }
            if bucket.records == 1 {
                bucket.cost_quality = quality;
            } else if bucket.cost_quality != quality {
                bucket.cost_quality = CostQuality::Mixed;
            }
        } else {
            bucket.api_equivalent_cost_usd = None;
            bucket.cost_quality =
                if bucket.records == 1 || bucket.cost_quality == CostQuality::Unpriced {
                    CostQuality::Unpriced
                } else {
                    CostQuality::Mixed
                };
        }
    }
    buckets
        .into_values()
        .map(|(mut bucket, sessions)| {
            bucket.sessions = sessions.len() as u64;
            bucket
        })
        .collect()
}

/// Reads only marked, explicitly configured synthetic homes. Never resolves
/// CODEX_HOME/Claude home as a source and never invokes a provider account.
pub fn snapshot(request: UsageRequest) -> UsageSnapshot {
    snapshot_impl(request, None, None)
}

/// Read selected provider transcripts without touching their credentials or
/// writing into their homes. The cache is stored only in LUNA's data dir.
pub fn snapshot_live(
    request: UsageRequest,
    codex_home: Option<PathBuf>,
    claude_home: PathBuf,
    data_dir: PathBuf,
    enabled: Vec<UsageProvider>,
) -> UsageSnapshot {
    snapshot_impl(
        request,
        Some((codex_home, claude_home, data_dir)),
        Some(enabled),
    )
}

fn snapshot_impl(
    request: UsageRequest,
    live: Option<(Option<PathBuf>, PathBuf, PathBuf)>,
    enabled: Option<Vec<UsageProvider>>,
) -> UsageSnapshot {
    static SCAN_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let _guard = SCAN_LOCK.get_or_init(|| Mutex::new(())).lock();
    let started = Instant::now();
    let read_at = now_ms();
    let days = if [1, 7, 30, 90].contains(&request.days) {
        request.days
    } else {
        30
    };
    let cutoff = read_at - i64::from(days) * 86_400_000;
    let cache_file = live
        .as_ref()
        .map_or_else(cache_path, |(_, _, data_dir)| live_cache_path(data_dir));
    let mut cache = load_cache(cache_file.as_deref());
    let mut records = Vec::new();
    let deadline = started + MAX_SCAN_TIME;
    let (codex, claude) = if let Some((codex_home, claude_home, _)) = live.as_ref() {
        (
            add_live_source(
                UsageProvider::Codex,
                codex_home.as_deref().filter(|_| {
                    enabled
                        .as_ref()
                        .is_none_or(|items| items.contains(&UsageProvider::Codex))
                }),
                "sessions",
                cutoff,
                deadline,
                &mut cache,
                &mut records,
            ),
            add_live_source(
                UsageProvider::Claude,
                enabled
                    .as_ref()
                    .is_none_or(|items| items.contains(&UsageProvider::Claude))
                    .then_some(claude_home.as_path()),
                "projects",
                cutoff,
                Instant::now() + MAX_SCAN_TIME,
                &mut cache,
                &mut records,
            ),
        )
    } else {
        (
            add_source(
                UsageProvider::Codex,
                "LUNA_USAGE_SYNTHETIC_CODEX_HOME",
                cutoff,
                deadline,
                &mut cache,
                &mut records,
            ),
            add_source(
                UsageProvider::Claude,
                "LUNA_USAGE_SYNTHETIC_CLAUDE_HOME",
                cutoff,
                Instant::now() + MAX_SCAN_TIME,
                &mut cache,
                &mut records,
            ),
        )
    };
    let sources: Vec<_> = vec![
        codex,
        claude,
        UsageSource {
            provider: UsageProvider::OpenCodeGo,
            environment_id: if live.is_some() { "local" } else { "local-synthetic" }.into(),
            fingerprint: String::new(),
            status: UsageSourceStatus::Unsupported,
            distinct_sessions: 0,
            files_scanned: 0,
            files_cached: 0,
            failed_files: 0,
            detail: Some("OpenCode historical session usage is not in the T3/BB usage adapters; Go subscription limits are separate".into()),
        },
    ]
    .into_iter()
    .filter(|source| {
        enabled
            .as_ref()
            .is_none_or(|items| items.contains(&source.provider))
    })
    .collect();
    // Longest supported window plus one day of boundary slack.
    let retention = read_at - 91 * 86_400_000;
    cache.files.retain(|_, file| {
        (file.modified_ns / 1_000_000) as i64 >= retention
            || file.records.iter().any(|record| record.at_ms >= retention)
    });
    cache.version = CACHE_VERSION;
    save_cache(cache_file.as_deref(), &cache);

    let mut price_rates = live
        .as_ref()
        .map_or_else(prices, |(_, _, data_dir)| live_prices(data_dir));
    for (model, rate) in request.price_overrides {
        if !model.trim().is_empty() && model.len() <= 200 && rate.valid() {
            price_rates.insert(model, (rate, CostQuality::UserOverride));
        }
    }
    let buckets = aggregate_records(records, &sources, days, &price_rates);
    let warnings = live
        .as_ref()
        .filter(|_| {
            sources.iter().any(|source| {
                matches!(
                    source.provider,
                    UsageProvider::Codex | UsageProvider::Claude
                )
            })
        })
        .and_then(|(_, _, data_dir)| live_pricing_warning(data_dir))
        .into_iter()
        .collect();
    UsageSnapshot {
        summary: UsageSummary {
            contract_version: USAGE_CONTRACT_VERSION,
            read_at_ms: read_at,
            time_zone: "UTC".into(),
            days,
            resolution: if days == 1 { "hour" } else { "day" }.into(),
            buckets,
            partial: sources
                .iter()
                .any(|source| source.status == UsageSourceStatus::Partial),
            sources,
            scan_duration_ms: started.elapsed().as_millis() as u64,
            warnings,
        },
        limits: limits()
            .into_iter()
            .filter(|limit| {
                enabled
                    .as_ref()
                    .is_none_or(|items| items.contains(&limit.provider))
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn any_explicit_usage_fixture_blocks_live_fallback() {
        assert!(!synthetic_mode_with(|_| false));
        for variable in SYNTHETIC_USAGE_VARIABLES {
            assert!(synthetic_mode_with(|name| name == *variable), "{variable}");
        }
    }

    #[test]
    fn live_codex_mapping_does_not_invent_a_primary_or_reset() {
        let read = CodexQuotaRead {
            verified_account_id: Some("codex:synthetic-hash".into()),
            plan: Some("pro".into()),
            checked_at_ms: 500_000,
            windows: vec![
                zeron_harness::codex::native::CodexQuotaWindow {
                    limit_id: "codex".into(),
                    limit_name: None,
                    position: "secondary",
                    used_percent: 42.0,
                    duration_minutes: Some(10_080),
                    resets_at_seconds: None,
                },
                zeron_harness::codex::native::CodexQuotaWindow {
                    limit_id: "codex_other".into(),
                    limit_name: Some("Other model".into()),
                    position: "primary",
                    used_percent: 12.0,
                    duration_minutes: Some(60),
                    resets_at_seconds: Some(1780000000),
                },
            ],
            reset_credits: None,
        };
        let limit = live_codex_limit(read);
        assert_eq!(
            limit.verified_account_id.as_deref(),
            Some("codex:synthetic-hash")
        );
        assert_eq!(limit.windows.len(), 2);
        assert_eq!(limit.windows[0].id, "codex/secondary");
        assert_eq!(limit.windows[0].label, "Weekly");
        assert_eq!(limit.windows[0].resets_at_ms, None);
        assert_eq!(limit.windows[1].id, "codex_other/primary");
        assert_eq!(limit.windows[1].resets_at_ms, Some(1_780_000_000_000));
        assert_eq!(limit.reset_credits, None);
    }

    #[test]
    fn claude_usage_maps_only_reported_account_and_model_scoped_windows() {
        let body = serde_json::json!({
            "five_hour": {"utilization": 31.5, "resets_at": "2026-09-23T20:00:00Z"},
            "seven_day": null,
            "model_scoped": [
                {"display_name": "Fable 5", "utilization": 112.0, "resets_at": null},
                {"display_name": "Unknown", "utilization": null, "resets_at": null}
            ]
        });
        let limit =
            live_claude_limit(&body, Some("claude:fake".into()), Some("Max".into())).unwrap();
        assert_eq!(limit.windows.len(), 2);
        assert_eq!(limit.windows[0].id, "five_hour");
        assert_eq!(limit.windows[0].used_percent, 31.5);
        assert_eq!(limit.windows[1].id, "seven_day_fable_5");
        assert_eq!(limit.windows[1].used_percent, 100.0);
        assert_eq!(limit.windows[1].resets_at_ms, None);
        assert!(live_claude_limit(&serde_json::json!({"five_hour": null}), None, None).is_none());
    }

    #[test]
    fn litellm_prices_require_complete_rates_and_unambiguous_aliases() {
        let document = serde_json::json!({
            "vendor-a/model-one": {
                "input_cost_per_token": 0.000001,
                "output_cost_per_token": 0.000002,
                "cache_read_input_token_cost": 0.0000005
            },
            "vendor-b/model-one": {
                "input_cost_per_token": 0.000001,
                "output_cost_per_token": 0.000002,
                "cache_read_input_token_cost": 0.0000005
            },
            "vendor-a/model-two": {
                "input_cost_per_token": 0.000001,
                "output_cost_per_token": 0.000002
            },
            "vendor-b/model-two": {
                "input_cost_per_token": 0.000003,
                "output_cost_per_token": 0.000004
            },
            "incomplete": { "input_cost_per_token": 0.000001 }
        });
        let rates = parse_litellm_rates(&document);
        assert_eq!(rates["model-one"].input_per_million, 1.0);
        assert_eq!(rates["model-one"].cached_per_million, 0.5);
        assert!(!rates.contains_key("model-two"));
        assert!(!rates.contains_key("incomplete"));
        let prices = rates
            .into_iter()
            .map(|(name, rate)| (name, (rate, CostQuality::ModelPriced)))
            .collect();
        assert!(lookup_price(&prices, "MODEL-ONE[1m]").is_some());
        assert!(lookup_price(&prices, "model-two").is_none());
    }

    #[test]
    fn exact_price_override_wins_over_provider_reported_cost() {
        let source = UsageSource {
            provider: UsageProvider::Claude,
            environment_id: "fake".into(),
            fingerprint: "fake-home".into(),
            status: UsageSourceStatus::Ready,
            distinct_sessions: 1,
            files_scanned: 1,
            files_cached: 0,
            failed_files: 0,
            detail: None,
        };
        let record = Record {
            at_ms: now_ms(),
            model: "fake-model".into(),
            session: "fake-session".into(),
            tokens: UsageTokens {
                uncached_input: 1_000_000,
                ..UsageTokens::default()
            },
            cost_usd: Some(99.0),
            dedupe: None,
        };
        let prices = HashMap::from([(
            "fake-model".into(),
            (
                UsagePriceRate {
                    input_per_million: 1.0,
                    cached_per_million: 1.0,
                    creation_per_million: 1.0,
                    output_per_million: 1.0,
                },
                CostQuality::UserOverride,
            ),
        )]);
        let buckets =
            aggregate_records(vec![(UsageProvider::Claude, record)], &[source], 7, &prices);
        assert_eq!(buckets[0].api_equivalent_cost_usd, Some(1.0));
        assert_eq!(buckets[0].cost_quality, CostQuality::UserOverride);
    }

    #[test]
    fn claude_repeated_blocks_have_same_dedupe_identity() {
        let line = r#"{"type":"assistant","timestamp":"2026-09-22T00:00:00Z","sessionId":"s","requestId":"r","message":{"id":"m","model":"claude-test","usage":{"input_tokens":2,"cache_read_input_tokens":3,"cache_creation_input_tokens":4,"output_tokens":5}}}"#;
        let record = parse_claude(line).unwrap();
        assert_eq!(record.dedupe.as_deref(), Some("m:r"));
        assert_eq!(record.tokens.total(), 14);
    }

    #[test]
    fn codex_cached_and_reasoning_are_not_added_twice() {
        let mut state = CodexState::default();
        let _ = parse_codex(
            r#"{"type":"session_meta","payload":{"id":"s"}}"#,
            &mut state,
        );
        let _ = parse_codex(
            r#"{"type":"turn_context","payload":{"model":"gpt-test"}}"#,
            &mut state,
        );
        let line = r#"{"type":"event_msg","timestamp":"2026-09-22T00:00:00Z","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":10,"cached_input_tokens":4,"output_tokens":8,"reasoning_output_tokens":3}}}}"#;
        let record = parse_codex(line, &mut state).unwrap();
        assert_eq!(record.tokens.total(), 18);
        assert_eq!(record.tokens.reasoning, 3);
        assert!(parse_codex(line, &mut state).is_none());
    }

    #[test]
    fn append_and_rewrite_keep_correct_records() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.jsonl");
        let line = |id: &str| {
            format!(
                r#"{{"type":"assistant","timestamp":"2026-09-22T00:00:00Z","requestId":"{id}","message":{{"id":"{id}","model":"claude-test","usage":{{"input_tokens":2}}}}}}"#
            )
        };
        fs::write(&path, format!("{}\n", line("one"))).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        let (first, warm) = scan_file(&path, UsageProvider::Claude, None, deadline).unwrap();
        assert!(!warm);
        assert_eq!(first.records.len(), 1);
        fs::write(&path, format!("{}\n", line("two"))).unwrap();
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(2)))
            .unwrap();
        let (same_size, _) =
            scan_file(&path, UsageProvider::Claude, Some(&first), deadline).unwrap();
        assert_eq!(same_size.records.len(), 1);
        assert_eq!(same_size.records[0].dedupe.as_deref(), Some("two:two"));
        fs::write(&path, format!("{}\n{}\n", line("two"), line("three"))).unwrap();
        let (append, _) =
            scan_file(&path, UsageProvider::Claude, Some(&same_size), deadline).unwrap();
        assert_eq!(append.records.len(), 2);
        fs::write(&path, format!("{}\n", line("other"))).unwrap();
        let (rewrite, _) =
            scan_file(&path, UsageProvider::Claude, Some(&append), deadline).unwrap();
        assert_eq!(rewrite.records.len(), 1);
        assert_eq!(rewrite.records[0].dedupe.as_deref(), Some("other:other"));
    }

    #[test]
    fn timed_out_file_resumes_its_saved_prefix() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("fake.jsonl");
        let line = r#"{"type":"assistant","timestamp":"2026-09-22T00:00:00Z","sessionId":"fake","message":{"id":"fake","model":"fake-model","usage":{"input_tokens":2}}}"#;
        fs::write(&path, format!("{line}\n{line}\n")).unwrap();
        let (paused, _) = scan_file(&path, UsageProvider::Claude, None, Instant::now()).unwrap();
        assert_eq!(paused.complete_offset, 0);
        assert_eq!(paused.size, fs::metadata(&path).unwrap().len());
        let (resumed, warm) = scan_file(
            &path,
            UsageProvider::Claude,
            Some(&paused),
            Instant::now() + Duration::from_secs(2),
        )
        .unwrap();
        assert!(!warm);
        assert_eq!(resumed.complete_offset, resumed.size);
        assert_eq!(resumed.records.len(), 2);
    }

    #[test]
    fn fork_copy_burst_is_excluded_but_later_work_counts() {
        let mut state = CodexState::default();
        let _ = parse_codex(
            r#"{"type":"session_meta","timestamp":"2026-09-22T00:00:00Z","payload":{"id":"child","forked_from_id":"parent"}}"#,
            &mut state,
        );
        let _ = parse_codex(
            r#"{"type":"turn_context","payload":{"model":"gpt-test"}}"#,
            &mut state,
        );
        let token = |timestamp: &str, count| {
            format!(
                r#"{{"type":"event_msg","timestamp":"{timestamp}","payload":{{"type":"token_count","info":{{"last_token_usage":{{"input_tokens":{count},"output_tokens":1}}}}}}}}"#
            )
        };
        assert!(parse_codex(&token("2026-09-22T00:00:00.100Z", 4), &mut state).is_none());
        assert_eq!(
            parse_codex(&token("2026-09-22T00:00:05Z", 5), &mut state)
                .unwrap()
                .session,
            "child"
        );
    }

    #[test]
    fn synthetic_marker_is_required() {
        let dir = tempfile::tempdir().unwrap();
        assert!(safe_fixture_root(Some(dir.path().as_os_str().to_owned())).is_err());
        fs::write(dir.path().join(SYNTHETIC_MARKER), b"synthetic").unwrap();
        assert_eq!(
            safe_fixture_root(Some(dir.path().as_os_str().to_owned())).unwrap(),
            Some(dir.path().canonicalize().unwrap())
        );
    }

    #[test]
    fn marked_root_cannot_contain_a_real_provider_home() {
        let candidate = tempfile::tempdir().unwrap();
        let private = candidate.path().join("custom-codex-home");
        fs::create_dir(&private).unwrap();
        fs::write(candidate.path().join(SYNTHETIC_MARKER), b"synthetic").unwrap();
        fs::write(private.join("private.jsonl"), b"must never be scanned").unwrap();
        let result = safe_fixture_root_with_homes(
            Some(candidate.path().as_os_str().to_owned()),
            &[private.clone()],
        );
        assert!(result.is_err());
        let mut cache = ScanCache::default();
        let mut records = Vec::new();
        let source = add_source_configured(
            UsageProvider::Codex,
            result,
            0,
            Instant::now() + Duration::from_secs(1),
            &mut cache,
            &mut records,
        );
        assert_eq!(source.status, UsageSourceStatus::Unsupported);
        assert_eq!(source.files_scanned, 0);
        assert!(records.is_empty() && cache.files.is_empty());
        let nested = private.join("synthetic");
        fs::create_dir(&nested).unwrap();
        fs::write(nested.join(SYNTHETIC_MARKER), b"synthetic").unwrap();
        assert!(
            safe_fixture_root_with_homes(Some(nested.as_os_str().to_owned()), &[private]).is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_alias_to_nested_real_home_is_rejected() {
        use std::os::unix::fs::symlink;
        let candidate = tempfile::tempdir().unwrap();
        let aliases = tempfile::tempdir().unwrap();
        let private = candidate.path().join("custom-codex-home");
        fs::create_dir(&private).unwrap();
        fs::write(candidate.path().join(SYNTHETIC_MARKER), b"synthetic").unwrap();
        let alias = aliases.path().join("provider-alias");
        symlink(&private, &alias).unwrap();
        let result =
            safe_fixture_root_with_homes(Some(candidate.path().as_os_str().to_owned()), &[alias]);
        assert!(result.is_err());
    }

    #[test]
    fn aggregation_keeps_tokens_and_unknown_cost_without_double_counting() {
        let claude = parse_claude(r#"{"type":"assistant","timestamp":"2026-09-22T00:00:00Z","sessionId":"one","requestId":"r","costUSD":0.2,"message":{"id":"m","model":"claude-test","usage":{"input_tokens":2,"cache_read_input_tokens":3,"cache_creation_input_tokens":4,"output_tokens":5}}}"#).unwrap();
        let codex = Record {
            at_ms: claude.at_ms,
            model: "gpt-test".into(),
            session: "two".into(),
            tokens: UsageTokens {
                uncached_input: 6,
                cached_input: 4,
                cache_creation: 0,
                output: 8,
                reasoning: 3,
            },
            cost_usd: None,
            dedupe: None,
        };
        let unpriced = Record {
            model: "unknown-model".into(),
            ..codex.clone()
        };
        let source = |provider, id: &str| UsageSource {
            provider,
            environment_id: "fixture".into(),
            fingerprint: id.into(),
            status: UsageSourceStatus::Ready,
            distinct_sessions: 1,
            files_scanned: 1,
            files_cached: 0,
            failed_files: 0,
            detail: None,
        };
        let rates = HashMap::from([(
            "gpt-test".into(),
            (
                UsagePriceRate {
                    input_per_million: 1.0,
                    cached_per_million: 1.0,
                    creation_per_million: 1.0,
                    output_per_million: 1.0,
                },
                CostQuality::ModelPriced,
            ),
        )]);
        let buckets = aggregate_records(
            vec![
                (UsageProvider::Claude, claude.clone()),
                (UsageProvider::Claude, claude),
                (UsageProvider::Codex, codex),
                (UsageProvider::Codex, unpriced),
            ],
            &[
                source(UsageProvider::Claude, "claude-home"),
                source(UsageProvider::Codex, "codex-home"),
            ],
            1,
            &rates,
        );
        assert_eq!(
            buckets
                .iter()
                .map(|bucket| bucket.tokens.total())
                .sum::<u64>(),
            50
        );
        assert_eq!(buckets.iter().map(|bucket| bucket.records).sum::<u64>(), 3);
        let claude = buckets
            .iter()
            .find(|bucket| bucket.model == "claude-test")
            .unwrap();
        assert_eq!(claude.sessions, 1);
        assert_eq!(claude.api_equivalent_cost_usd, Some(0.2));
        assert_eq!(claude.cost_quality, CostQuality::ProviderReported);
        let codex = buckets
            .iter()
            .find(|bucket| bucket.model == "gpt-test")
            .unwrap();
        assert_eq!(codex.tokens.reasoning, 3);
        assert!((codex.api_equivalent_cost_usd.unwrap() - 0.000018).abs() < 1e-10);
        assert_eq!(codex.cost_quality, CostQuality::ModelPriced);
        let unknown = buckets
            .iter()
            .find(|bucket| bucket.model == "unknown-model")
            .unwrap();
        assert_eq!(unknown.api_equivalent_cost_usd, None);
        assert_eq!(unknown.cost_quality, CostQuality::Unpriced);
    }

    #[test]
    fn user_rate_prices_exact_model_even_when_provider_reports_cost() {
        let tokens = UsageTokens {
            uncached_input: 1_000_000,
            cached_input: 1_000_000,
            ..UsageTokens::default()
        };
        let base = Record {
            at_ms: 0,
            model: "gpt-test".into(),
            session: "one".into(),
            tokens,
            cost_usd: None,
            dedupe: None,
        };
        let source = UsageSource {
            provider: UsageProvider::Codex,
            environment_id: "fixture".into(),
            fingerprint: "fixture".into(),
            status: UsageSourceStatus::Ready,
            distinct_sessions: 1,
            files_scanned: 1,
            files_cached: 0,
            failed_files: 0,
            detail: None,
        };
        let rates = HashMap::from([(
            "gpt-test".into(),
            (
                UsagePriceRate {
                    input_per_million: 2.0,
                    cached_per_million: 0.0,
                    creation_per_million: 0.0,
                    output_per_million: 0.0,
                },
                CostQuality::UserOverride,
            ),
        )]);
        let buckets = aggregate_records(
            vec![
                (UsageProvider::Codex, base.clone()),
                (
                    UsageProvider::Codex,
                    Record {
                        at_ms: 3_600_000,
                        cost_usd: Some(0.5),
                        ..base.clone()
                    },
                ),
                (
                    UsageProvider::Codex,
                    Record {
                        at_ms: 7_200_000,
                        model: "gpt-test-extra".into(),
                        ..base
                    },
                ),
            ],
            &[source],
            1,
            &rates,
        );
        assert_eq!(buckets[0].api_equivalent_cost_usd, Some(2.0));
        assert_eq!(buckets[0].cache_savings_usd, Some(2.0));
        assert_eq!(buckets[0].cost_quality, CostQuality::UserOverride);
        assert_eq!(buckets[1].api_equivalent_cost_usd, Some(2.0));
        assert_eq!(buckets[1].cache_savings_usd, Some(2.0));
        assert_eq!(buckets[1].cost_quality, CostQuality::UserOverride);
        assert_eq!(buckets[2].api_equivalent_cost_usd, None);
        assert_eq!(buckets[2].cache_savings_usd, None);
        assert_eq!(buckets[2].cost_quality, CostQuality::Unpriced);
    }

    #[test]
    fn opencode_go_fixture_accepts_only_named_windows_with_account_provenance() {
        let fixture = UsageLimitSnapshot {
            provider: UsageProvider::OpenCodeGo,
            verified_account_id: Some("fake-go-account".into()),
            account_label: Some("Synthetic Go account".into()),
            plan: Some("Go".into()),
            checked_at_ms: Some(1_000_000),
            availability: LimitAvailability::Ready,
            windows: ["session", "weekly", "monthly"]
                .into_iter()
                .map(|id| zeron_proto::UsageLimitWindow {
                    id: id.into(),
                    label: "fixture label".into(),
                    used_percent: 25.0,
                    resets_at_ms: Some(2_000_000),
                    duration_minutes: Some(300),
                })
                .collect(),
            reset_credits: Some(7),
            detail: None,
        };
        let encoded = serde_json::to_value(&fixture).unwrap();
        assert_eq!(encoded["provider"], "openCodeGo");
        let fixture: UsageLimitSnapshot = serde_json::from_value(encoded).unwrap();
        let normalized = normalize_limit_fixture(vec![fixture.clone()]).unwrap();
        assert_eq!(normalized[0].windows[0].id, "fiveHour");
        assert_eq!(normalized[0].windows[0].label, "Five-hour");
        assert_eq!(normalized[0].windows.len(), 3);
        assert_eq!(normalized[0].reset_credits, None);
        assert!(
            normalized[0]
                .detail
                .as_deref()
                .unwrap()
                .contains("synthetic")
        );
        let with_defaults = with_unsupported_limits(normalized);
        assert_eq!(with_defaults.len(), 3);
        assert!(
            with_defaults
                .iter()
                .any(|item| item.provider == UsageProvider::Codex
                    && item.availability == LimitAvailability::Unsupported)
        );

        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join(SYNTHETIC_MARKER), b"synthetic").unwrap();
        let path = root.path().join("limits.json");
        let last_good = root.path().join("usage-limits-last-good.json");
        fs::write(&path, serde_json::to_vec(&vec![fixture.clone()]).unwrap()).unwrap();
        let fresh = limits_from_fixture(&path, Some(&last_good));
        assert_eq!(fresh[0].availability, LimitAvailability::Ready);
        assert!(last_good.is_file());
        fs::write(&path, b"invalid synthetic refresh").unwrap();
        let stale = limits_from_fixture(&path, Some(&last_good));
        assert_eq!(stale[0].availability, LimitAvailability::ProbeFailed);
        assert_eq!(stale[0].windows.len(), 3);
        fs::remove_file(&last_good).unwrap();
        let unavailable = limits_from_fixture(&path, Some(&last_good));
        assert!(
            unavailable
                .iter()
                .any(|item| item.provider == UsageProvider::OpenCodeGo
                    && item.availability == LimitAvailability::Unsupported)
        );

        let mut invalid = fixture.clone();
        invalid.verified_account_id = None;
        assert!(normalize_limit_fixture(vec![invalid]).is_none());
        let mut invalid = fixture.clone();
        invalid.windows[0].id = "zen".into();
        assert!(normalize_limit_fixture(vec![invalid]).is_none());
        let mut invalid = fixture;
        invalid.windows[1].id = "five-hour".into();
        assert!(normalize_limit_fixture(vec![invalid]).is_none());
    }

    #[test]
    fn opencode_history_fails_before_opening_a_transcript() {
        let missing = Path::new("/does-not-exist/open-code.jsonl");
        let result = scan_file(
            missing,
            UsageProvider::OpenCodeGo,
            None,
            Instant::now() + Duration::from_secs(1),
        );
        assert_eq!(
            result.err().unwrap().kind(),
            std::io::ErrorKind::Unsupported
        );
    }

    #[test]
    fn explicit_live_homes_scan_and_resume_without_provider_writes() {
        let root = tempfile::tempdir().unwrap();
        let codex = root.path().join("codex");
        let claude = root.path().join("claude");
        let data = root.path().join("luna-data");
        fs::create_dir_all(codex.join("sessions")).unwrap();
        fs::create_dir_all(claude.join("projects")).unwrap();
        fs::create_dir_all(&data).unwrap();
        let timestamp = chrono::Utc::now().to_rfc3339();
        let codex_rows = format!(
            "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"fake-codex\"}}}}\n\
             {{\"type\":\"turn_context\",\"payload\":{{\"model\":\"fake-model\"}}}}\n\
             {{\"type\":\"event_msg\",\"timestamp\":\"{timestamp}\",\"payload\":{{\"type\":\"token_count\",\"info\":{{\"last_token_usage\":{{\"input_tokens\":12,\"cached_input_tokens\":2,\"output_tokens\":3}}}}}}}}\n"
        );
        fs::write(codex.join("sessions/fake.jsonl"), codex_rows).unwrap();
        let claude_row = format!(
            "{{\"type\":\"assistant\",\"timestamp\":\"{timestamp}\",\"sessionId\":\"fake-claude\",\"message\":{{\"id\":\"fake-message\",\"model\":\"fake-claude-model\",\"usage\":{{\"input_tokens\":4,\"output_tokens\":2}}}}}}\n"
        );
        fs::write(claude.join("projects/fake.jsonl"), claude_row).unwrap();
        let request = UsageRequest {
            days: 7,
            price_overrides: HashMap::new(),
        };
        let first = snapshot_live(
            request.clone(),
            Some(codex.clone()),
            claude.clone(),
            data.clone(),
            vec![
                UsageProvider::Codex,
                UsageProvider::Claude,
                UsageProvider::OpenCodeGo,
            ],
        );
        assert!(
            first.summary.sources[..2]
                .iter()
                .all(|source| source.status == UsageSourceStatus::Ready)
        );
        assert_eq!(
            first
                .summary
                .buckets
                .iter()
                .map(|bucket| bucket.tokens.total())
                .sum::<u64>(),
            21
        );
        assert!(data.join("usage-scan-v2.json").is_file());
        let second = snapshot_live(
            request.clone(),
            Some(codex.clone()),
            claude.clone(),
            data.clone(),
            vec![
                UsageProvider::Codex,
                UsageProvider::Claude,
                UsageProvider::OpenCodeGo,
            ],
        );
        assert!(
            second.summary.sources[..2]
                .iter()
                .all(|source| source.files_cached == 1)
        );
        assert_eq!(
            second
                .summary
                .buckets
                .iter()
                .map(|bucket| bucket.tokens.total())
                .sum::<u64>(),
            21
        );
        let codex_only = snapshot_live(
            request,
            Some(codex),
            claude,
            data,
            vec![UsageProvider::Codex],
        );
        assert_eq!(codex_only.summary.sources.len(), 1);
        assert_eq!(codex_only.summary.sources[0].provider, UsageProvider::Codex);
        assert!(
            codex_only
                .summary
                .buckets
                .iter()
                .all(|bucket| bucket.provider == UsageProvider::Codex)
        );
        assert_eq!(codex_only.limits.len(), 1);
        assert_eq!(codex_only.limits[0].provider, UsageProvider::Codex);
        let none_enabled = snapshot_live(
            UsageRequest {
                days: 7,
                price_overrides: HashMap::new(),
            },
            None,
            root.path().join("unread-claude-home"),
            root.path().join("luna-data"),
            vec![],
        );
        assert!(none_enabled.summary.sources.is_empty());
        assert!(none_enabled.summary.buckets.is_empty());
        assert!(none_enabled.summary.warnings.is_empty());
        assert!(none_enabled.limits.is_empty());
    }

    #[test]
    #[ignore = "explicit opt-in read-only provider transcript check"]
    fn selected_provider_history_readback() {
        if std::env::var_os("LUNA_USAGE_REAL_READBACK").as_deref()
            != Some(std::ffi::OsStr::new("1"))
        {
            return;
        }
        let home = PathBuf::from(std::env::var_os("HOME").expect("HOME"));
        let codex = std::env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".codex"));
        let claude = std::env::var_os("CLAUDE_CONFIG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".claude"));
        let data = tempfile::tempdir().unwrap();
        let request = UsageRequest {
            days: 7,
            price_overrides: HashMap::new(),
        };
        let snapshot = snapshot_live(
            request.clone(),
            Some(codex.clone()),
            claude.clone(),
            data.path().to_path_buf(),
            vec![
                UsageProvider::Codex,
                UsageProvider::Claude,
                UsageProvider::OpenCodeGo,
            ],
        );
        let tokens: u64 = snapshot
            .summary
            .buckets
            .iter()
            .map(|bucket| bucket.tokens.total())
            .sum();
        eprintln!(
            "History readback: {} buckets, {tokens} tokens, source statuses {:?}/{:?}, scanned {}/{}, failed {}/{}",
            snapshot.summary.buckets.len(),
            snapshot.summary.sources[0].status,
            snapshot.summary.sources[1].status,
            snapshot.summary.sources[0].files_scanned,
            snapshot.summary.sources[1].files_scanned,
            snapshot.summary.sources[0].failed_files,
            snapshot.summary.sources[1].failed_files
        );
        assert!(snapshot.summary.sources[0].status != UsageSourceStatus::Unsupported);
        assert!(tokens > 0);
        let resumed = snapshot_live(
            request,
            Some(codex),
            claude,
            data.path().to_path_buf(),
            vec![
                UsageProvider::Codex,
                UsageProvider::Claude,
                UsageProvider::OpenCodeGo,
            ],
        );
        eprintln!(
            "History second pass: {} cached files, {} newly scanned, {:?}",
            resumed.summary.sources[0].files_cached,
            resumed.summary.sources[0].files_scanned,
            resumed.summary.sources[0].status
        );
        assert!(resumed.summary.sources[0].files_cached > 0);
    }

    #[tokio::test]
    #[ignore = "explicit opt-in public LiteLLM price refresh"]
    async fn current_litellm_price_readback() {
        if std::env::var_os("LUNA_USAGE_REAL_READBACK").as_deref()
            != Some(std::ffi::OsStr::new("1"))
        {
            return;
        }
        let data = tempfile::tempdir().unwrap();
        refresh_live_prices(data.path()).await;
        let rates = live_prices(data.path());
        eprintln!("LiteLLM readback: {} priced model keys", rates.len());
        assert!(!rates.is_empty());
        assert!(data.path().join("usage-model-rates.json").is_file());
    }

    #[cfg(unix)]
    #[test]
    fn synthetic_cache_rejects_symlinked_files() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("fake-private-data.json");
        fs::write(&target, b"fixture only").unwrap();
        let link = root.path().join("usage-limits-last-good.json");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(!safe_cache_file(&link));
        assert!(load_cache(Some(&link)).files.is_empty());
    }
}
