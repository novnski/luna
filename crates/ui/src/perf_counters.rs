//! Opt-in measurement counters (`LUNA_PERF_COUNTERS=1`) for the workbench
//! baseline (harness evolution slice 6). They observe the current behavior
//! and never change it: when off, each call site costs one cached boolean
//! check; when on, they record calls, items, bytes and wall time per metric
//! and scope on the UI thread. `scope` names the view: `main:<chat>` or
//! `pane:<chat>`. See docs/design/harness-evolution/slice-6-ui.md.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Counters are on for this process (read once).
pub fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("LUNA_PERF_COUNTERS").is_ok_and(|v| !v.is_empty() && v != "0")
    })
}

/// Layout probes (painted control bounds for native QA) are a separate,
/// costlier layer than the counters: a canvas, a name and a map insert per
/// probed element per paint. They start off so measured phases match a
/// baseline that never had them; a fixture turns them on after measuring.
/// `LUNA_PERF_MEASURE_LAYOUT_PROBES=1` (diagnostic) keeps them on from
/// startup to compare one binary both ways.
static PROBES: OnceLock<AtomicBool> = OnceLock::new();

fn probes_flag() -> &'static AtomicBool {
    PROBES.get_or_init(|| {
        AtomicBool::new(
            std::env::var("LUNA_PERF_MEASURE_LAYOUT_PROBES")
                .is_ok_and(|v| !v.is_empty() && v != "0"),
        )
    })
}

/// Layout probes are painted (counters on and probes switched on).
pub fn probes_enabled() -> bool {
    enabled() && probes_flag().load(Ordering::Relaxed)
}

/// Turn layout probes on or off; views add or drop them on their next
/// render (a capture forces that render).
pub fn set_probes_enabled(on: bool) {
    probes_flag().store(on, Ordering::Relaxed);
}

#[derive(Default, Clone)]
struct Stat {
    calls: u64,
    items: u64,
    bytes: u64,
    total_ns: u128,
    max_ns: u64,
}

/// Work noted by free functions during one owning call (a transcript sync),
/// attributed to that call's scope when it flushes.
#[derive(Default)]
struct Pending {
    fingerprints: u64,
    fingerprint_bytes: u64,
    hits: u64,
    misses: u64,
    rows_rebuilt: u64,
    streaming_rebuilt: u64,
}

/// What one painted row showed.
#[derive(Clone)]
struct RenderedRow {
    version: u64,
    kind: &'static str,
    text: String,
    frame: u64,
}

const ROWS_PER_SCOPE: usize = 2048;

#[derive(Default)]
struct Rows {
    by_id: HashMap<String, RenderedRow>,
    order: VecDeque<String>,
}

thread_local! {
    static STATS: RefCell<BTreeMap<(&'static str, String), Stat>> = RefCell::default();
    static PENDING: RefCell<Pending> = RefCell::default();
    static ROWS: RefCell<HashMap<String, Rows>> = RefCell::default();
    static FRAME: RefCell<u64> = const { RefCell::new(0) };
    /// Probe bounds with the frame that painted them.
    static BOUNDS: RefCell<HashMap<String, ([f32; 4], u64)>> = RefCell::default();
}

/// A probed element's painted bounds (x, y, width, height), recorded while
/// counters are on so a native fixture can aim real pointer input.
pub fn record_bounds(name: &str, bounds: gpui::Bounds<gpui::Pixels>) {
    if probes_enabled() {
        let frame = FRAME.with(|f| *f.borrow());
        BOUNDS.with(|map| {
            map.borrow_mut().insert(
                name.to_string(),
                (
                    [
                        f32::from(bounds.origin.x),
                        f32::from(bounds.origin.y),
                        f32::from(bounds.size.width),
                        f32::from(bounds.size.height),
                    ],
                    frame,
                ),
            );
        });
    }
}

/// Forget probes whose name starts with `prefix` (before re-probing).
pub fn clear_bounds(prefix: &str) {
    BOUNDS.with(|map| map.borrow_mut().retain(|name, _| !name.starts_with(prefix)));
}

/// The last painted bounds of probe `name`.
pub fn bounds(name: &str) -> Option<[f32; 4]> {
    BOUNDS.with(|map| map.borrow().get(name).map(|(bounds, _)| *bounds))
}

/// The frame number `next_frame` last started.
pub fn current_frame() -> u64 {
    FRAME.with(|f| *f.borrow())
}

/// Probes painted since the last [`clear_bounds`] covering them, whose names
/// start with `prefix`. A capture clears first and then forces the probed
/// views to re-render (cached views replay old paints without re-recording),
/// so an element not painted since (hidden, moved into an overflow menu)
/// never reports a stale rectangle.
pub fn probes(prefix: &str) -> Vec<(String, [f32; 4])> {
    BOUNDS.with(|map| {
        let mut out: Vec<(String, [f32; 4])> = map
            .borrow()
            .iter()
            .filter(|(name, _)| name.starts_with(prefix))
            .map(|(name, (bounds, _))| (name.clone(), *bounds))
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    })
}

/// A layout-neutral probe (absolute, filling its positioned parent) that
/// records the parent's painted bounds as `name`. Attach only while
/// [`probes_enabled`].
pub fn probe(name: String) -> gpui::AnyElement {
    use gpui::{IntoElement as _, Styled as _};
    gpui::canvas(
        move |bounds, _, _| record_bounds(&name, bounds),
        |_, _, _, _| {},
    )
    .absolute()
    .inset_0()
    .into_any_element()
}

fn add(metric: &'static str, scope: &str, items: u64, bytes: u64, elapsed: Option<Duration>) {
    STATS.with(|stats| {
        let mut stats = stats.borrow_mut();
        let stat = stats.entry((metric, scope.to_string())).or_default();
        stat.calls += 1;
        stat.items += items;
        stat.bytes += bytes;
        if let Some(elapsed) = elapsed {
            let ns = elapsed.as_nanos();
            stat.total_ns += ns;
            stat.max_ns = stat.max_ns.max(ns.min(u64::MAX as u128) as u64);
        }
    });
}

/// Count one occurrence of `metric` (no timing).
pub fn count(metric: &'static str, scope: &str, items: u64, bytes: u64) {
    if enabled() {
        add(metric, scope, items, bytes, None);
    }
}

/// Times a call from creation to drop.
pub struct Timer {
    metric: &'static str,
    scope: String,
    start: Instant,
    items: u64,
    flush: bool,
}

impl Timer {
    /// `None` (no work at all) while counters are off.
    pub fn start(metric: &'static str, scope: impl FnOnce() -> String) -> Option<Self> {
        enabled().then(|| Self {
            metric,
            scope: scope(),
            start: Instant::now(),
            items: 0,
            flush: false,
        })
    }

    /// On drop, also attribute work noted by free functions meanwhile.
    pub fn with_pending(mut self) -> Self {
        self.flush = true;
        self
    }

    pub fn scope(&self) -> &str {
        &self.scope
    }

    pub fn items(&mut self, items: u64) {
        self.items += items;
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        if self.flush {
            flush_pending(&self.scope);
        }
        add(
            self.metric,
            &self.scope,
            self.items,
            0,
            Some(self.start.elapsed()),
        );
    }
}

/// `entry_fingerprint` hashed `bytes` of input.
pub fn note_fingerprint(bytes: usize) {
    if enabled() {
        PENDING.with(|p| {
            let mut p = p.borrow_mut();
            p.fingerprints += 1;
            p.fingerprint_bytes += bytes as u64;
        });
    }
}

/// `rows_for` reused cached rows (`hit`) or rebuilt `rows` rows.
pub fn note_rows(hit: bool, streaming: bool, rows: usize) {
    if enabled() {
        PENDING.with(|p| {
            let mut p = p.borrow_mut();
            if hit {
                p.hits += 1;
            } else if streaming {
                p.streaming_rebuilt += rows as u64;
            } else {
                p.misses += 1;
                p.rows_rebuilt += rows as u64;
            }
        });
    }
}

/// Attribute the noted work to `scope` (one transcript sync).
pub fn flush_pending(scope: &str) {
    if !enabled() {
        return;
    }
    let pending = PENDING.with(|p| std::mem::take(&mut *p.borrow_mut()));
    add(
        "transcript.fingerprint",
        scope,
        pending.fingerprints,
        pending.fingerprint_bytes,
        None,
    );
    add("transcript.row_cache_hit", scope, pending.hits, 0, None);
    add("transcript.row_cache_miss", scope, pending.misses, 0, None);
    add(
        "transcript.rows_rebuilt",
        scope,
        pending.rows_rebuilt,
        0,
        None,
    );
    add(
        "transcript.streaming_rebuilt",
        scope,
        pending.streaming_rebuilt,
        0,
        None,
    );
}

/// A new window frame began (the fixture calls this from its frame hook), so
/// rendered rows can be grouped by the frame that painted them.
pub fn next_frame() -> u64 {
    FRAME.with(|f| {
        let mut f = f.borrow_mut();
        *f += 1;
        *f
    })
}

/// The painter received this row in the current frame.
pub fn record_row(scope: &str, id: &str, version: u64, kind: &'static str, text: String) {
    if !enabled() {
        return;
    }
    let frame = FRAME.with(|f| *f.borrow());
    ROWS.with(|rows| {
        let mut rows = rows.borrow_mut();
        let rows = rows.entry(scope.to_string()).or_default();
        if rows
            .by_id
            .insert(
                id.to_string(),
                RenderedRow {
                    version,
                    kind,
                    text,
                    frame,
                },
            )
            .is_none()
        {
            rows.order.push_back(id.to_string());
            while rows.order.len() > ROWS_PER_SCOPE {
                if let Some(old) = rows.order.pop_front() {
                    rows.by_id.remove(&old);
                }
            }
        }
    });
}

/// Every counter as JSON: `{metric: {scope: {calls, items, bytes, totalMs,
/// maxMs, meanUs}}}`.
pub fn snapshot() -> serde_json::Value {
    STATS.with(|stats| {
        let mut out = serde_json::Map::new();
        for ((metric, scope), stat) in stats.borrow().iter() {
            let entry = out
                .entry(metric.to_string())
                .or_insert_with(|| serde_json::Value::Object(Default::default()));
            let total_ms = stat.total_ns as f64 / 1e6;
            entry[scope.as_str()] = serde_json::json!({
                "calls": stat.calls,
                "items": stat.items,
                "bytes": stat.bytes,
                "totalMs": total_ms,
                "maxMs": stat.max_ns as f64 / 1e6,
                "meanUs": if stat.calls == 0 { 0.0 } else { total_ms * 1000.0 / stat.calls as f64 },
            });
        }
        serde_json::Value::Object(out)
    })
}

/// Rendered rows per scope: `{scope: [{id, version, kind, text, frame}]}`
/// in first-painted order.
pub fn rendered_rows() -> serde_json::Value {
    ROWS.with(|rows| {
        let mut out = serde_json::Map::new();
        for (scope, rows) in rows.borrow().iter() {
            let list: Vec<serde_json::Value> = rows
                .order
                .iter()
                .filter_map(|id| {
                    rows.by_id.get(id).map(|row| {
                        serde_json::json!({
                            "id": id,
                            "version": row.version,
                            "kind": row.kind,
                            "text": row.text,
                            "frame": row.frame,
                        })
                    })
                })
                .collect();
            out.insert(scope.clone(), serde_json::Value::Array(list));
        }
        serde_json::Value::Object(out)
    })
}

/// Forget rendered rows only (counters keep accumulating).
pub fn reset_rows() {
    ROWS.with(|r| r.borrow_mut().clear());
}

/// Start a new phase: clear counters, pending work and rendered rows.
pub fn reset() {
    STATS.with(|s| s.borrow_mut().clear());
    PENDING.with(|p| *p.borrow_mut() = Pending::default());
    ROWS.with(|r| r.borrow_mut().clear());
}
