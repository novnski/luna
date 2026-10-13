//! IC-0 native feasibility fixture for isolated documents in a virtualized
//! GPUI transcript (macOS). Failure contract first:
//! docs/pending-tasks/interactive-chat-surfaces/ic-0/README.md ("prototype").
//!
//! A real GPUI window with two virtualized lists (a split) holds six visual
//! cards backed by `luna_ui::browser::isolated::IsolatedDocument` (five
//! isolated, one positive control). It records, with real WebKit and AppKit:
//! network/file/storage/popup/navigation/host-bridge refusals against local
//! TCP/UDP/file sentinels (the control must leak, or the probe is void),
//! clipping and hit testing under the list mask and GPUI's overlay plane,
//! focus entry and Escape, wheel routing, live-view budget/eviction/reload,
//! card resize without reload, split resize, hidden-view animation and
//! teardown. No engine, provider, network service or user data is used.
//!
//!     cargo build --locked -p luna-ui --example isolated-document-fixture --features browser-fixture
//!     scripts/run-macos-browser-fixture.sh target/debug/examples/isolated-document-fixture <out-dir>
//!
//! Writes <out-dir>/result.json and PNG captures; exits non-zero when any
//! hard check fails. Observations (wheel chaining, hidden frame rate) are
//! recorded findings, not pass/fail gates.

#[cfg(not(target_os = "macos"))]
fn main() -> anyhow::Result<()> {
    anyhow::bail!("the IC-0 isolated document fixture requires macOS WebKit")
}

#[cfg(target_os = "macos")]
fn main() -> anyhow::Result<()> {
    macos::main()
}

#[cfg(target_os = "macos")]
mod macos {
    use gpui::{
        AnyElement, App, AppContext, AsyncApp, Bounds, Context, ListAlignment, ListOffset,
        ListState, Pixels, Render, Window, WindowBounds, WindowHandle, WindowOptions, anchored,
        canvas, deferred, div, list, point, prelude::*, px, relative, rgb, size,
    };
    use luna_ui::browser::isolated::{
        DocumentEvent, IsolatedDocument, Isolation, PreparedResource, live_views,
    };
    use serde_json::{Value, json};
    use std::{
        cell::RefCell,
        collections::BTreeMap,
        io::{Read, Write},
        path::{Path, PathBuf},
        rc::Rc,
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        time::{Duration, Instant},
    };

    const HEADER: f32 = 40.;
    const ROW: f32 = 56.;
    const CARD: f32 = 320.;
    const EXPANDED: f32 = 560.;
    const BUDGET: usize = 3;
    const LEFT_ROWS: usize = 400;
    const RIGHT_ROWS: usize = 120;
    const PNG_1X1: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f,
        0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0xfc,
        0xcf, 0xc0, 0x50, 0x0f, 0x00, 0x04, 0x85, 0x01, 0x80, 0x84, 0xa9, 0x8c, 0x21, 0x00, 0x00,
        0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];

    /// The untrusted document. `__ID__`, `__TITLE__` and `__TALL__` are
    /// replaced per card. It never names the host's sentinels: the fixture
    /// sends them in a `probe` message after load.
    const DOCUMENT: &str = r#"<!doctype html><meta charset="utf-8"><title>__TITLE__</title>
<style>
body{margin:0;font:13px -apple-system,system-ui;background:#f4f1ea;color:#263d35}
main{padding:12px}h3{margin:0 0 8px;font:600 15px Georgia}
#chart{display:block;width:100%;height:120px}
#tall{height:__TALL__px;background:linear-gradient(#f4f1ea,#cfd8c8)}
</style>
<main><h3>__TITLE__</h3><canvas id="chart"></canvas>
<input id="slider" type="range" min="1" max="20" value="8"> <span id="value">8</span>
<img id="pixel" alt="" width="8" height="8" src="luna-artifact://localhost/__ID__/res/pixel.png">
<div id="tall"></div></main>
<script>
(() => {
  const parentWindow = window.parent;
  const isTop = parentWindow === window;
  const send = (type, data) => {
    if (isTop) {
      if (window.webkit && window.webkit.messageHandlers.lunaShell)
        window.webkit.messageHandlers.lunaShell.postMessage(JSON.stringify({ token: null, kind: type, data }));
    } else parentWindow.postMessage({ type, data }, '*');
  };
  let frames = 0; const tick = () => { frames++; requestAnimationFrame(tick); }; requestAnimationFrame(tick);
  const canvas = document.getElementById('chart'); const slider = document.getElementById('slider');
  const draw = () => {
    const ratio = devicePixelRatio || 1; const w = canvas.width = Math.max(1, canvas.clientWidth * ratio);
    const h = canvas.height = 120 * ratio; const ctx = canvas.getContext('2d'); const n = Number(slider.value);
    ctx.clearRect(0, 0, w, h);
    for (let i = 0; i < n; i++) { const bh = h * (0.2 + 0.8 * Math.abs(Math.sin(i + n)));
      ctx.fillStyle = `hsl(${150 + i * 9} 35% 40%)`; ctx.fillRect(i * w / n + 2, h - bh, w / n - 4, bh); }
    document.getElementById('value').textContent = String(n);
  };
  slider.addEventListener('input', draw); addEventListener('resize', draw); draw();
  let lastScroll = 0;
  addEventListener('scroll', () => { const y = Math.round(scrollY);
    if (Math.abs(y - lastScroll) >= 10) { lastScroll = y; send('scroll', { y }); } }, { passive: true });
  addEventListener('keydown', (e) => { if (e.key === 'Escape') send('escape', { cooperative: true }); });
  const wait = (ms) => new Promise((ok) => setTimeout(ok, ms));
  async function probes(cfg) {
    const r = {};
    const attempt = async (name, run) => {
      try { r[name] = await Promise.race([run(), wait(4000).then(() => 'timeout')]); }
      catch (e) { r[name] = 'threw:' + ((e && e.name) || String(e)); }
    };
    await attempt('fetch', () => fetch(cfg.sentinel + '/fetch', { mode: 'no-cors' }).then(() => 'resolved', () => 'rejected'));
    await attempt('xhr', () => new Promise((ok) => { const x = new XMLHttpRequest(); x.onload = () => ok('loaded');
      x.onerror = () => ok('error'); x.open('GET', cfg.sentinel + '/xhr'); x.send(); }));
    await attempt('image', () => new Promise((ok) => { const i = new Image(); i.onload = () => ok('loaded');
      i.onerror = () => ok('error'); i.src = cfg.sentinel + '/image.png'; }));
    await attempt('cssImage', () => new Promise((ok) => { const d = document.createElement('div');
      d.style.cssText = 'width:4px;height:4px;background-image:url(' + cfg.sentinel + '/css.png)';
      document.body.append(d); setTimeout(() => ok('applied'), 800); }));
    await attempt('beacon', async () => String(navigator.sendBeacon(cfg.sentinel + '/beacon', 'x')));
    await attempt('websocket', () => new Promise((ok) => { const s = new WebSocket(cfg.ws + '/ws');
      s.onopen = () => ok('open'); s.onerror = () => ok('error'); }));
    await attempt('fileImage', () => new Promise((ok) => { const i = new Image(); i.onload = () => ok('loaded');
      i.onerror = () => ok('error'); i.src = cfg.fileUrl; }));
    await attempt('fileFetch', () => fetch(cfg.fileUrl).then((x) => x.text()).then((t) => 'read:' + t.length, () => 'rejected'));
    await attempt('cookie', async () => { document.cookie = 'ic0=1; path=/'; return 'cookie:' + document.cookie; });
    await attempt('localStorage', async () => { localStorage.setItem('ic0', '1'); return 'stored'; });
    await attempt('indexedDB', () => new Promise((ok) => { const q = indexedDB.open('ic0');
      q.onsuccess = () => ok('opened'); q.onerror = () => ok('error'); }));
    await attempt('popup', async () => (window.open(cfg.sentinel + '/popup') ? 'opened' : 'blocked'));
    await attempt('parentDocument', async () => (isTop ? 'self' : String(Boolean(parentWindow.document))));
    await attempt('hostBridgeDirect', async () => {
      const h = window.webkit && window.webkit.messageHandlers && window.webkit.messageHandlers.lunaShell;
      if (!h) return 'absent';
      h.postMessage(JSON.stringify({ token: 'forged', kind: 'forged', data: { forged: true } })); return 'posted';
    });
    await attempt('wryIpc', async () => typeof window.ipc);
    await attempt('download', async () => { const a = document.createElement('a'); a.href = 'data:text/plain,ic0';
      a.download = 'ic0.txt'; document.body.append(a); a.click(); return 'clicked'; });
    await attempt('webrtc', async () => {
      if (typeof RTCPeerConnection !== 'function') return 'unavailable';
      const pc = new RTCPeerConnection({ iceServers: [{ urls: 'stun:127.0.0.1:' + cfg.udpPort }] });
      pc.createDataChannel('ic0'); await pc.setLocalDescription(await pc.createOffer());
      await wait(2500); const state = pc.iceGatheringState; pc.close(); return 'gathered:' + state;
    });
    await attempt('permittedResource', async () => (document.getElementById('pixel').naturalWidth > 0 ? 'loaded' : 'missing'));
    return r;
  }
  function navigationProbes(cfg) {
    try { const f = document.createElement('form'); f.method = 'post'; f.action = cfg.sentinel + '/form';
      document.body.append(f); f.submit(); } catch (e) {}
    setTimeout(() => { try { const a = document.createElement('a'); a.href = cfg.sentinel + '/blank';
      a.target = '_blank'; document.body.append(a); a.click(); } catch (e) {} }, 300);
    setTimeout(() => { try { window.top.location.href = cfg.sentinel + '/top'; } catch (e) {} }, 600);
    setTimeout(() => { try { location.href = cfg.sentinel + '/self'; } catch (e) {} }, 900);
  }
  addEventListener('message', async (event) => {
    if (event.source !== parentWindow) return;
    const m = event.data || {};
    if (m.type === 'report') send('frames', { count: frames });
    if (m.type === 'size') send('size', { width: innerWidth, height: innerHeight });
    if (m.type === 'probe') send('probe', await probes(m.data));
    if (m.type === 'navprobe') navigationProbes(m.data);
  });
  addEventListener('load', () => send('loaded', { pixel: document.getElementById('pixel').naturalWidth }));
})();
</script>"#;

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Pane {
        Left,
        Right,
    }

    struct Slot {
        id: &'static str,
        pane: Pane,
        row: usize,
        isolation: Isolation,
        html: String,
        card: f32,
        doc: Option<IsolatedDocument>,
        wanted_frame: u64,
        geometry: Option<(Bounds<Pixels>, Bounds<Pixels>)>,
        creations: u32,
        loads: u32,
        error: Option<String>,
        events: Vec<DocumentEvent>,
    }

    #[derive(Default)]
    struct Registry {
        frame: u64,
        slots: Vec<Slot>,
        max_live: usize,
        evictions: u32,
        refusals: u32,
    }

    impl Registry {
        fn slot(&self, id: &str) -> &Slot {
            self.slots
                .iter()
                .find(|slot| slot.id == id)
                .expect("known slot")
        }
        fn slot_mut(&mut self, id: &str) -> &mut Slot {
            self.slots
                .iter_mut()
                .find(|slot| slot.id == id)
                .expect("known slot")
        }
        fn at(&self, pane: Pane, row: usize) -> Option<(&'static str, f32)> {
            self.slots
                .iter()
                .find(|slot| slot.pane == pane && slot.row == row)
                .map(|slot| (slot.id, slot.card))
        }
        fn live(&self) -> usize {
            self.slots.iter().filter(|slot| slot.doc.is_some()).count()
        }
        fn want(&mut self, id: &str, bounds: Bounds<Pixels>, mask: Bounds<Pixels>) {
            let frame = self.frame;
            let slot = self.slot_mut(id);
            slot.wanted_frame = frame;
            slot.geometry = Some((bounds, mask));
        }
        fn present(&mut self, id: &str) {
            let slot = self.slot_mut(id);
            if let (Some(doc), Some((bounds, mask))) = (&slot.doc, slot.geometry) {
                doc.sync(bounds, mask);
            }
        }
        /// Runs after every row's present: anything not painted this frame is
        /// off screen and must not stay visible (no orphan native views).
        fn sweep(&mut self, frame: u64) {
            for slot in &self.slots {
                if slot.wanted_frame != frame
                    && let Some(doc) = &slot.doc
                {
                    doc.hide();
                }
            }
        }
        fn drain(&mut self) {
            for slot in &mut self.slots {
                if let Some(doc) = &slot.doc {
                    for event in doc.take_events() {
                        if matches!(&event, DocumentEvent::Accepted { kind, .. } if kind == "loaded")
                        {
                            slot.loads += 1;
                        }
                        slot.events.push(event);
                    }
                }
            }
        }
        /// Slots painted in the latest frame without a live view, evicting
        /// the least recently painted off-screen view over budget.
        fn plan(&mut self) -> Vec<usize> {
            let frame = self.frame;
            let mut create = Vec::new();
            for index in 0..self.slots.len() {
                if self.slots[index].doc.is_some()
                    || self.slots[index].wanted_frame != frame
                    || self.slots[index].error.is_some()
                {
                    continue;
                }
                while self.live() + create.len() >= BUDGET {
                    let victim = self
                        .slots
                        .iter()
                        .enumerate()
                        .filter(|(_, slot)| slot.doc.is_some() && slot.wanted_frame != frame)
                        .min_by_key(|(_, slot)| slot.wanted_frame)
                        .map(|(victim, _)| victim);
                    let Some(victim) = victim else { break };
                    self.slots[victim].doc = None;
                    self.evictions += 1;
                }
                if self.live() + create.len() >= BUDGET {
                    self.refusals += 1;
                    continue;
                }
                create.push(index);
            }
            create
        }
    }

    struct Harness {
        registry: Rc<RefCell<Registry>>,
        left: ListState,
        right: ListState,
        split: f32,
        menu_open: bool,
        _reconcile: gpui::Task<()>,
    }

    impl Harness {
        fn new(
            registry: Rc<RefCell<Registry>>,
            window: &mut Window,
            cx: &mut Context<Self>,
        ) -> Self {
            let reconcile = cx.spawn_in(window, async move |this, cx| {
                loop {
                    cx.background_executor()
                        .timer(Duration::from_millis(40))
                        .await;
                    let alive = this.update_in(cx, |this, window, cx| {
                        let create = {
                            let mut registry = this.registry.borrow_mut();
                            registry.drain();
                            registry.plan()
                        };
                        // Native creation happens with the registry released.
                        for index in &create {
                            let (id, html, isolation) = {
                                let registry = this.registry.borrow();
                                let slot = &registry.slots[*index];
                                (slot.id, slot.html.clone(), slot.isolation)
                            };
                            let resources = vec![PreparedResource {
                                name: "pixel.png".into(),
                                mime: "image/png".into(),
                                bytes: PNG_1X1.to_vec(),
                            }];
                            let created =
                                IsolatedDocument::new(window, id, &html, resources, isolation);
                            let mut registry = this.registry.borrow_mut();
                            let slot = &mut registry.slots[*index];
                            match created {
                                Ok(doc) => {
                                    slot.doc = Some(doc);
                                    slot.creations += 1;
                                }
                                Err(error) => slot.error = Some(error),
                            }
                            let live = registry.live();
                            registry.max_live = registry.max_live.max(live);
                        }
                        if !create.is_empty() {
                            cx.notify();
                        }
                    });
                    if alive.is_err() {
                        break;
                    }
                }
            });
            Self {
                registry,
                left: ListState::new(LEFT_ROWS, ListAlignment::Top, px(400.)),
                right: ListState::new(RIGHT_ROWS, ListAlignment::Top, px(400.)),
                split: 0.6,
                menu_open: false,
                _reconcile: reconcile,
            }
        }
    }

    fn rows(
        pane: Pane,
        registry: Rc<RefCell<Registry>>,
    ) -> impl FnMut(usize, &mut Window, &mut App) -> AnyElement + 'static {
        move |ix, _, _| {
            let card = registry.borrow().at(pane, ix);
            let Some((id, height)) = card else {
                return div()
                    .w_full()
                    .h(px(ROW))
                    .px(px(12.))
                    .flex()
                    .items_center()
                    .border_b_1()
                    .border_color(rgb(0x2a2d33))
                    .child(format!("Message {ix}: an ordinary transcript row"))
                    .into_any_element();
            };
            let registry = registry.clone();
            div()
                .w_full()
                .h(px(height))
                .p(px(8.))
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(div().h(px(18.)).child(format!("Visual card {id}")))
                .child(
                    div().flex_1().min_h_0().bg(rgb(0x2c3038)).child(
                        canvas(
                            |_, _, _| (),
                            move |bounds, _, window, _| {
                                let mask = window.content_mask().bounds;
                                registry.borrow_mut().want(id, bounds, mask);
                                let registry = registry.clone();
                                window.on_present(move || registry.borrow_mut().present(id));
                            },
                        )
                        .size_full(),
                    ),
                )
                .into_any_element()
        }
    }

    impl Render for Harness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let frame = {
                let mut registry = self.registry.borrow_mut();
                registry.frame += 1;
                registry.frame
            };
            let sweep = self.registry.clone();
            div()
                .size_full()
                .flex()
                .flex_col()
                .font_family("Helvetica")
                .text_size(px(13.))
                .bg(rgb(0x17191d))
                .text_color(rgb(0xe8e9ed))
                .child(
                    div()
                        .h(px(HEADER))
                        .flex_none()
                        .px(px(12.))
                        .flex()
                        .items_center()
                        .bg(rgb(0x22252b))
                        .child("IC-0 isolated document fixture"),
                )
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .flex()
                        .flex_row()
                        .child(
                            div()
                                .w(relative(self.split))
                                .h_full()
                                .overflow_hidden()
                                .child(
                                    list(
                                        self.left.clone(),
                                        rows(Pane::Left, self.registry.clone()),
                                    )
                                    .size_full(),
                                ),
                        )
                        .child(div().w(px(1.)).h_full().bg(rgb(0x565b67)))
                        .child(
                            div().flex_1().h_full().overflow_hidden().child(
                                list(self.right.clone(), rows(Pane::Right, self.registry.clone()))
                                    .size_full(),
                            ),
                        ),
                )
                .when(self.menu_open, |root| {
                    root.child(deferred(
                        anchored()
                            .position(point(px(120.), px(HEADER + 40.)))
                            .child(
                                div()
                                    .w(px(360.))
                                    .h(px(240.))
                                    .p(px(12.))
                                    .bg(rgb(0x2c3038))
                                    .border_1()
                                    .border_color(rgb(0x565b67))
                                    .occlude()
                                    .child("Menu overlay above a live document"),
                            ),
                    ))
                })
                .child(
                    canvas(
                        |_, _, _| (),
                        move |_, _, window, _| {
                            let sweep = sweep.clone();
                            window.on_present(move || sweep.borrow_mut().sweep(frame));
                        },
                    )
                    .absolute()
                    .top_0()
                    .left_0()
                    .w(px(1.))
                    .h(px(1.)),
                )
        }
    }

    struct Sentinel {
        tcp_port: u16,
        hits: Arc<Mutex<Vec<String>>>,
        udp: BTreeMap<&'static str, (u16, Arc<AtomicUsize>)>,
    }

    impl Sentinel {
        fn start(udp_for: &[&'static str]) -> anyhow::Result<Self> {
            let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
            let tcp_port = listener.local_addr()?.port();
            let hits = Arc::new(Mutex::new(Vec::new()));
            let record = hits.clone();
            std::thread::spawn(move || {
                for mut stream in listener.incoming().flatten() {
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                    let mut request = [0; 4096];
                    let n = stream.read(&mut request).unwrap_or(0);
                    let request = String::from_utf8_lossy(&request[..n]);
                    let line = request.lines().next().unwrap_or_default().to_owned();
                    if let Ok(mut hits) = record.lock() {
                        hits.push(line.clone());
                    }
                    let body: &[u8] = if line.contains(".png") { PNG_1X1 } else { b"" };
                    let status = if body.is_empty() {
                        "204 No Content"
                    } else {
                        "200 OK"
                    };
                    let _ = write!(
                        stream,
                        "HTTP/1.1 {status}\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(body);
                }
            });
            let mut udp = BTreeMap::new();
            for id in udp_for {
                let socket = std::net::UdpSocket::bind("127.0.0.1:0")?;
                let port = socket.local_addr()?.port();
                let count = Arc::new(AtomicUsize::new(0));
                let counter = count.clone();
                std::thread::spawn(move || {
                    let mut buffer = [0; 2048];
                    while socket.recv_from(&mut buffer).is_ok() {
                        counter.fetch_add(1, Ordering::SeqCst);
                    }
                });
                udp.insert(*id, (port, count));
            }
            Ok(Self {
                tcp_port,
                hits,
                udp,
            })
        }
        fn hits_for(&self, id: &str) -> Vec<String> {
            let prefix = format!(" /{id}/");
            self.hits
                .lock()
                .map(|hits| {
                    hits.iter()
                        .filter(|line| line.contains(&prefix))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default()
        }
        fn udp_for(&self, id: &str) -> usize {
            self.udp
                .get(id)
                .map(|(_, count)| count.load(Ordering::SeqCst))
                .unwrap_or(0)
        }
        fn config(&self, id: &str, file_url: &str) -> Value {
            json!({
                "sentinel": format!("http://127.0.0.1:{}/{id}", self.tcp_port),
                "ws": format!("ws://127.0.0.1:{}/{id}", self.tcp_port),
                "udpPort": self.udp.get(id).map(|(port, _)| *port).unwrap_or(9),
                "fileUrl": file_url,
            })
        }
    }

    #[derive(Default)]
    struct Report {
        checks: Vec<Value>,
        observations: BTreeMap<String, Value>,
    }

    impl Report {
        fn check(&mut self, id: &str, pass: bool, detail: Value) {
            eprintln!("IC-0 fixture {} {id}", if pass { "PASS" } else { "FAIL" });
            self.checks
                .push(json!({"id": id, "pass": pass, "detail": detail}));
        }
        fn observe(&mut self, id: &str, value: Value) {
            self.observations.insert(id.into(), value);
        }
        fn failed(&self) -> Vec<String> {
            self.checks
                .iter()
                .filter(|check| check["pass"] != json!(true))
                .filter_map(|check| check["id"].as_str().map(str::to_owned))
                .collect()
        }
    }

    async fn pause(cx: &mut AsyncApp, ms: u64) {
        cx.background_executor()
            .timer(Duration::from_millis(ms))
            .await;
    }

    async fn wait_for(cx: &mut AsyncApp, ms: u64, condition: impl Fn() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_millis(ms);
        loop {
            if condition() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            pause(cx, 25).await;
        }
    }

    fn capture(directory: &Path, name: &str) -> Value {
        let app = objc2_app_kit::NSApplication::sharedApplication(
            objc2::MainThreadMarker::new().expect("main thread"),
        );
        let Some(window) = app.keyWindow().or_else(|| app.mainWindow()) else {
            return json!({"name": name, "error": "no window"});
        };
        let path = directory.join(format!("{name}.png"));
        let status = std::process::Command::new("/usr/sbin/screencapture")
            .args(["-x", "-o", "-l", &window.windowNumber().to_string()])
            .arg(&path)
            .status();
        json!({"name": name, "ok": status.map(|s| s.success()).unwrap_or(false)})
    }

    fn accepted<'a>(events: &'a [DocumentEvent], wanted: &str) -> Vec<&'a Value> {
        events
            .iter()
            .filter_map(|event| match event {
                DocumentEvent::Accepted { kind, data } if kind == wanted => Some(data),
                _ => None,
            })
            .collect()
    }

    fn event_json(event: &DocumentEvent) -> Value {
        match event {
            DocumentEvent::Accepted { kind, data } => {
                // Only probe results are kept; other payloads are counters.
                let data = if kind == "probe" {
                    data.clone()
                } else {
                    Value::Null
                };
                json!({"accepted": kind, "data": data})
            }
            DocumentEvent::Refused {
                main_frame,
                origin,
                reason,
            } => {
                json!({"refused": reason, "mainFrame": main_frame, "origin": origin})
            }
            DocumentEvent::Navigation {
                url,
                main_frame,
                allowed,
            } => {
                json!({"navigation": url, "mainFrame": main_frame, "allowed": allowed})
            }
            DocumentEvent::NewWindow { url } => json!({"newWindow": url}),
            DocumentEvent::Download { url } => json!({"download": url}),
            DocumentEvent::EscapedFocus => json!({"escapedFocus": true}),
            DocumentEvent::Terminated => json!({"terminated": true}),
        }
    }

    fn bounds_json(bounds: Option<Bounds<Pixels>>) -> Value {
        bounds.map_or(Value::Null, |b| {
            json!([
                f32::from(b.origin.x),
                f32::from(b.origin.y),
                f32::from(b.size.width),
                f32::from(b.size.height)
            ])
        })
    }

    fn not_success(value: &Value) -> bool {
        let text = value.as_str().unwrap_or("missing");
        text.starts_with("threw:")
            || text == "error"
            || text == "rejected"
            || text == "timeout"
            || text == "blocked"
            || text == "cookie:"
            || text == "missing"
    }

    pub fn main() -> anyhow::Result<()> {
        let output = PathBuf::from(
            std::env::args()
                .nth(1)
                .unwrap_or_else(|| "/tmp/luna-ic0-isolated-document".into()),
        );
        std::fs::create_dir_all(&output)?;
        let temp = tempfile::tempdir()?;
        let file_sentinel = temp.path().join("ic0-file-sentinel.png");
        std::fs::write(&file_sentinel, PNG_1X1)?;
        let file_url = format!("file://{}", file_sentinel.display());
        let sentinel = Rc::new(Sentinel::start(&["doc-a", "doc-control"])?);
        let document = |id: &str, title: &str, tall: u32| {
            DOCUMENT
                .replace("__ID__", id)
                .replace("__TITLE__", title)
                .replace("__TALL__", &tall.to_string())
        };
        let slot = |id: &'static str, pane, row, isolation, html: String| Slot {
            id,
            pane,
            row,
            isolation,
            html,
            card: CARD,
            doc: None,
            wanted_frame: 0,
            geometry: None,
            creations: 0,
            loads: 0,
            error: None,
            events: Vec::new(),
        };
        let registry = Rc::new(RefCell::new(Registry {
            slots: vec![
                slot(
                    "doc-a",
                    Pane::Left,
                    3,
                    Isolation::Isolated,
                    document("doc-a", "Isolated chart", 900),
                ),
                slot(
                    "doc-b",
                    Pane::Left,
                    120,
                    Isolation::Isolated,
                    document("doc-b", "Short card", 0),
                ),
                slot(
                    "doc-c",
                    Pane::Left,
                    240,
                    Isolation::Isolated,
                    document("doc-c", "Third card", 900),
                ),
                slot(
                    "doc-d",
                    Pane::Left,
                    360,
                    Isolation::Isolated,
                    document("doc-d", "Fourth card", 900),
                ),
                slot(
                    "doc-control",
                    Pane::Right,
                    2,
                    Isolation::Control,
                    document("doc-control", "Positive control", 900),
                ),
                slot(
                    "doc-e",
                    Pane::Right,
                    60,
                    Isolation::Isolated,
                    document("doc-e", "Right pane card", 0),
                ),
            ],
            ..Default::default()
        }));
        let failure = Rc::new(RefCell::new(None::<String>));
        let result = failure.clone();
        gpui_platform::application().run(move |cx: &mut App| {
            let window: WindowHandle<Harness> = cx
                .open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                            point(px(40.), px(40.)),
                            size(px(1100.), px(760.)),
                        ))),
                        ..Default::default()
                    },
                    |window, cx| cx.new(|cx| Harness::new(registry.clone(), window, cx)),
                )
                .expect("fixture window");
            cx.activate(true);
            let registry = registry.clone();
            cx.spawn(async move |cx| {
                let mut report = Report::default();
                let run: anyhow::Result<()> = scenario(
                    cx,
                    window,
                    &registry,
                    &sentinel,
                    &file_url,
                    &output,
                    &mut report,
                )
                .await;
                if let Err(error) = &run {
                    report.check("scenario.completed", false, json!(format!("{error:#}")));
                }
                // Teardown: dropping every document must remove every native view.
                for slot in registry.borrow_mut().slots.iter_mut() {
                    slot.doc = None;
                }
                pause(cx, 300).await;
                report.check(
                    "teardown.no-native-views",
                    live_views() == 0,
                    json!({"liveViews": live_views()}),
                );
                let command = |program: &str, args: &[&str]| {
                    std::process::Command::new(program)
                        .args(args)
                        .output()
                        .ok()
                        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
                };
                let macos = command("/usr/bin/sw_vers", &["-productVersion"]);
                let hardware = command("/usr/sbin/sysctl", &["-n", "hw.model"]);
                let environment = json!({"macos": macos, "hardware": hardware, "budget": BUDGET});
                let events: BTreeMap<&str, Vec<Value>> = registry
                    .borrow()
                    .slots
                    .iter()
                    .map(|slot| (slot.id, slot.events.iter().map(event_json).collect()))
                    .collect();
                let failed = report.failed();
                let message =
                    (!failed.is_empty()).then(|| format!("failed checks: {}", failed.join(", ")));
                let record = json!({
                    "evidenceKind": "ic0-isolated-document-fixture",
                    "passed": message.is_none(),
                    "failed": failed,
                    "environment": environment,
                    "checks": report.checks,
                    "observations": report.observations,
                    "events": events,
                });
                let _ = std::fs::write(
                    output.join("result.json"),
                    serde_json::to_string_pretty(&record).unwrap_or_default(),
                );
                if message.is_some() {
                    *result.borrow_mut() = message;
                }
                let _ = window.update(cx, |_, window, _| window.remove_window());
                pause(cx, 100).await;
                let _ = cx.update(|cx| cx.quit());
            })
            .detach();
        });
        if let Some(error) = failure.borrow_mut().take() {
            anyhow::bail!(error);
        }
        Ok(())
    }

    fn doc_point(registry: &Rc<RefCell<Registry>>, id: &str) -> Option<(f64, f64, Bounds<Pixels>)> {
        let registry = registry.borrow();
        let doc = registry.slot(id).doc.as_ref()?;
        let visible = doc.visible_bounds()?;
        let center = visible.center();
        Some((
            f64::from(f32::from(center.x)),
            f64::from(f32::from(center.y)),
            visible,
        ))
    }

    fn scroll(
        window: WindowHandle<Harness>,
        cx: &mut AsyncApp,
        pane: Pane,
        item_ix: usize,
        offset: f32,
    ) -> anyhow::Result<()> {
        window.update(cx, |harness, _, cx| {
            let state = if pane == Pane::Left {
                &harness.left
            } else {
                &harness.right
            };
            state.scroll_to(ListOffset {
                item_ix,
                offset_in_item: px(offset),
            });
            cx.notify();
        })
    }

    fn with_doc<R>(
        registry: &Rc<RefCell<Registry>>,
        id: &str,
        f: impl FnOnce(&IsolatedDocument) -> R,
    ) -> Option<R> {
        let registry = registry.borrow();
        registry.slot(id).doc.as_ref().map(f)
    }

    fn list_top(
        window: WindowHandle<Harness>,
        cx: &mut AsyncApp,
        pane: Pane,
    ) -> anyhow::Result<(usize, f32)> {
        window.update(cx, |harness, _, _| {
            let state = if pane == Pane::Left {
                &harness.left
            } else {
                &harness.right
            };
            let top = state.logical_scroll_top();
            (top.item_ix, f32::from(top.offset_in_item))
        })
    }

    async fn frames(
        cx: &mut AsyncApp,
        registry: &Rc<RefCell<Registry>>,
        id: &str,
    ) -> Option<(f64, Instant)> {
        let before = accepted(&registry.borrow().slot(id).events, "frames").len();
        with_doc(registry, id, |doc| doc.send(&json!({"type": "report"})))?;
        let arrived = wait_for(cx, 3000, || {
            accepted(&registry.borrow().slot(id).events, "frames").len() > before
        })
        .await;
        let at = Instant::now();
        if !arrived {
            return None;
        }
        let registry = registry.borrow();
        let events = &registry.slot(id).events;
        accepted(events, "frames")
            .last()
            .and_then(|data| data["count"].as_f64())
            .map(|count| (count, at))
    }

    #[allow(clippy::too_many_arguments)]
    async fn scenario(
        cx: &mut AsyncApp,
        window: WindowHandle<Harness>,
        registry: &Rc<RefCell<Registry>>,
        sentinel: &Sentinel,
        file_url: &str,
        output: &Path,
        report: &mut Report,
    ) -> anyhow::Result<()> {
        let mut captures = Vec::new();
        // 1. Load: shell + document + prepared resource through the scheme.
        let loaded = wait_for(cx, 20_000, || {
            let registry = registry.borrow();
            registry.slot("doc-a").loads >= 1 && registry.slot("doc-control").loads >= 1
        })
        .await;
        let served = with_doc(registry, "doc-a", |doc| doc.served()).unwrap_or_default();
        let errors: BTreeMap<&str, String> = registry
            .borrow()
            .slots
            .iter()
            .filter_map(|slot| slot.error.clone().map(|error| (slot.id, error)))
            .collect();
        report.check(
            "load.isolated-and-control",
            loaded,
            json!({"served": served, "errors": errors}),
        );
        for path in [
            "200 /doc-a/shell.html",
            "200 /doc-a/document.html",
            "200 /doc-a/res/pixel.png",
        ] {
            report.check(
                &format!("load.served {path}"),
                served.iter().any(|line| line == path),
                Value::Null,
            );
        }
        anyhow::ensure!(loaded, "documents did not load");
        pause(cx, 300).await;
        captures.push(capture(output, "01-loaded"));

        // 2. Isolation probes in the isolated view and the positive control.
        for id in ["doc-a", "doc-control"] {
            let config = sentinel.config(id, file_url);
            with_doc(registry, id, |doc| {
                doc.send(&json!({"type": "probe", "data": config}))
            });
        }
        let probed = wait_for(cx, 40_000, || {
            let registry = registry.borrow();
            ["doc-a", "doc-control"]
                .iter()
                .all(|id| !accepted(&registry.slot(id).events, "probe").is_empty())
        })
        .await;
        report.check("isolation.probes-returned", probed, Value::Null);
        for id in ["doc-a", "doc-control"] {
            let config = sentinel.config(id, file_url);
            with_doc(registry, id, |doc| {
                doc.send(&json!({"type": "navprobe", "data": config}))
            });
        }
        pause(cx, 3000).await;
        let (isolated, control) = {
            let registry = registry.borrow();
            let first = |id: &str| {
                accepted(&registry.slot(id).events, "probe")
                    .first()
                    .map(|v| (*v).clone())
                    .unwrap_or(Value::Null)
            };
            (first("doc-a"), first("doc-control"))
        };
        let isolated_hits = sentinel.hits_for("doc-a");
        let control_hits = sentinel.hits_for("doc-control");
        report.observe("isolation.isolatedResults", isolated.clone());
        report.observe("isolation.controlResults", control.clone());
        report.observe("isolation.sentinel", json!({"isolatedTcp": isolated_hits, "controlTcp": control_hits,
            "isolatedUdp": sentinel.udp_for("doc-a"), "controlUdp": sentinel.udp_for("doc-control")}));
        report.check(
            "isolation.positive-control-leaks",
            !control_hits.is_empty(),
            json!({"controlTcp": control_hits.len()}),
        );
        report.check("isolation.no-network", isolated_hits.is_empty() && sentinel.udp_for("doc-a") == 0,
            json!({"tcp": isolated_hits, "udp": sentinel.udp_for("doc-a"), "webrtc": isolated["webrtc"]}));
        for key in ["cookie", "localStorage", "indexedDB"] {
            report.check(
                &format!("isolation.no-storage {key}"),
                not_success(&isolated[key]),
                isolated[key].clone(),
            );
        }
        report.check("isolation.no-file", isolated["fileImage"] != json!("loaded")
            && !isolated["fileFetch"].as_str().unwrap_or("").starts_with("read:"),
            json!({"image": isolated["fileImage"], "fetch": isolated["fileFetch"], "controlImage": control["fileImage"]}));
        report.check(
            "isolation.no-parent-access",
            isolated["parentDocument"]
                .as_str()
                .is_some_and(|v| v.starts_with("threw:")),
            isolated["parentDocument"].clone(),
        );
        report.check(
            "isolation.no-wry-ipc",
            isolated["wryIpc"] == json!("undefined"),
            isolated["wryIpc"].clone(),
        );
        report.check(
            "isolation.permitted-resource-loads",
            isolated["permittedResource"] == json!("loaded"),
            isolated["permittedResource"].clone(),
        );
        {
            let registry = registry.borrow();
            let events = &registry.slot("doc-a").events;
            let child_refusals = events
                .iter()
                .filter(|e| {
                    matches!(
                        e,
                        DocumentEvent::Refused {
                            main_frame: false,
                            ..
                        }
                    )
                })
                .count();
            let forged = !accepted(events, "forged").is_empty();
            report.check("isolation.host-bridge-refuses-child", !forged
                && (isolated["hostBridgeDirect"] == json!("absent") || child_refusals > 0),
                json!({"direct": isolated["hostBridgeDirect"], "childRefusals": child_refusals, "forgedAccepted": forged}));
            let allowed: Vec<String> = events
                .iter()
                .filter_map(|e| match e {
                    DocumentEvent::Navigation {
                        url,
                        main_frame,
                        allowed: true,
                    } => Some(format!(
                        "{}{url}",
                        if *main_frame { "main " } else { "sub " }
                    )),
                    _ => None,
                })
                .collect();
            let expected = [
                "main luna-artifact://localhost/doc-a/shell.html",
                "sub luna-artifact://localhost/doc-a/document.html",
            ];
            report.check(
                "isolation.navigation-allowlist",
                allowed.iter().all(|a| expected.contains(&a.as_str())),
                json!({"allowed": allowed}),
            );
            let windows = events
                .iter()
                .filter(|e| matches!(e, DocumentEvent::NewWindow { .. }))
                .count();
            let downloads = events
                .iter()
                .filter(|e| matches!(e, DocumentEvent::Download { .. }))
                .count();
            report.check(
                "isolation.no-popup-or-download",
                windows == 0 && downloads == 0 && isolated["popup"] != json!("opened"),
                json!({"newWindows": windows, "downloads": downloads, "popup": isolated["popup"]}),
            );
            let control_events = &registry.slot("doc-control").events;
            report.observe(
                "isolation.controlNavigationsAllowed",
                json!(
                    control_events
                        .iter()
                        .filter(|e| matches!(e, DocumentEvent::Navigation { allowed: true, .. }))
                        .count()
                ),
            );
        }
        captures.push(capture(output, "02-probed"));

        // 3. Clipping: scroll doc-a partly under the list's top edge.
        scroll(window, cx, Pane::Left, 3, 150.)?;
        pause(cx, 400).await;
        let geometry = with_doc(registry, "doc-a", |doc| {
            (doc.visible(), doc.bounds(), doc.visible_bounds())
        })
        .unwrap_or((false, None, None));
        let (visible, bounds, clipped) = geometry;
        let mut clip_ok = visible;
        let mut detail =
            json!({"bounds": bounds_json(bounds), "visibleBounds": bounds_json(clipped)});
        if let (Some(bounds), Some(clipped)) = (bounds, clipped) {
            let x = f64::from(f32::from(clipped.center().x));
            let above = f64::from(HEADER - 10.);
            let inside = f64::from(f32::from(clipped.origin.y) + 20.);
            let under_edge = f32::from(bounds.origin.y) < HEADER - 10.;
            let hit_above = with_doc(registry, "doc-a", |doc| doc.hit(x, above)).unwrap_or(true);
            let hit_inside = with_doc(registry, "doc-a", |doc| doc.hit(x, inside)).unwrap_or(false);
            clip_ok &= under_edge
                && f32::from(clipped.origin.y) >= HEADER - 0.5
                && !hit_above
                && hit_inside;
            detail["hitAboveListEdge"] = json!(hit_above);
            detail["hitInsideVisible"] = json!(hit_inside);
            detail["documentStartsAboveEdge"] = json!(under_edge);
        } else {
            clip_ok = false;
        }
        report.check("clip.list-mask-and-hit-testing", clip_ok, detail);
        captures.push(capture(output, "03-clipped"));
        scroll(window, cx, Pane::Left, 3, 0.)?;
        pause(cx, 400).await;

        // 4. Overlay plane above the live document.
        window.update(cx, |harness, _, cx| {
            harness.menu_open = true;
            cx.notify();
        })?;
        pause(cx, 400).await;
        let point_a = doc_point(registry, "doc-a");
        let (over, still_visible) = point_a
            .map(|(x, y, _)| {
                (
                    with_doc(registry, "doc-a", |doc| {
                        doc.overlay_at(x.min(400.), y.min(f64::from(HEADER) + 200.))
                    })
                    .unwrap_or(false),
                    with_doc(registry, "doc-a", |doc| doc.visible()).unwrap_or(false),
                )
            })
            .unwrap_or((false, false));
        captures.push(capture(output, "04-overlay"));
        window.update(cx, |harness, _, cx| {
            harness.menu_open = false;
            cx.notify();
        })?;
        pause(cx, 400).await;
        let restored = point_a.is_some_and(|(x, y, _)| {
            with_doc(registry, "doc-a", |doc| {
                !doc.overlay_at(x, y) && doc.hit(x, y)
            })
            .unwrap_or(false)
        });
        report.check("overlay.menu-above-document", over && still_visible && restored,
            json!({"overlayIntercepts": over, "documentStayedVisible": still_visible, "hitRestored": restored}));

        // 5. Focus enters by click and leaves by Escape, whatever the page does.
        if let Some((x, y, _)) = doc_point(registry, "doc-a") {
            with_doc(registry, "doc-a", |doc| doc.click(x, y));
            let entered = wait_for(cx, 3000, || {
                with_doc(registry, "doc-a", |doc| doc.focused()).unwrap_or(false)
            })
            .await;
            with_doc(registry, "doc-a", |doc| doc.press_escape());
            let escaped = wait_for(cx, 3000, || {
                let registry = registry.borrow();
                let slot = registry.slot("doc-a");
                slot.events
                    .iter()
                    .any(|e| matches!(e, DocumentEvent::EscapedFocus))
                    && !slot.doc.as_ref().is_some_and(|doc| doc.focused())
            })
            .await;
            report.check(
                "focus.enter-and-escape",
                entered && escaped,
                json!({"entered": entered, "escaped": escaped}),
            );
        } else {
            report.check(
                "focus.enter-and-escape",
                false,
                json!("doc-a not on screen"),
            );
        }

        // 6. Expand the card: same live view, no reload, scroll anchor kept.
        let before = {
            let registry = registry.borrow();
            let slot = registry.slot("doc-a");
            (
                slot.loads,
                slot.creations,
                slot.doc
                    .as_ref()
                    .and_then(|d| d.bounds())
                    .map(|b| f32::from(b.size.height)),
            )
        };
        let anchor_before = list_top(window, cx, Pane::Left)?;
        window.update(cx, |harness, _, cx| {
            harness.registry.borrow_mut().slot_mut("doc-a").card = EXPANDED;
            harness.left.splice(3..4, 1);
            cx.notify();
        })?;
        pause(cx, 500).await;
        with_doc(registry, "doc-a", |doc| doc.send(&json!({"type": "size"})));
        let sized = wait_for(cx, 3000, || {
            !accepted(&registry.borrow().slot("doc-a").events, "size").is_empty()
        })
        .await;
        let after = {
            let registry = registry.borrow();
            let slot = registry.slot("doc-a");
            let inner = accepted(&slot.events, "size")
                .last()
                .and_then(|d| d["height"].as_f64());
            (
                slot.loads,
                slot.creations,
                slot.doc
                    .as_ref()
                    .and_then(|d| d.bounds())
                    .map(|b| f32::from(b.size.height)),
                inner,
            )
        };
        let anchor_after = list_top(window, cx, Pane::Left)?;
        let grew = matches!((before.2, after.2), (Some(a), Some(b)) if (b - a - (EXPANDED - CARD)).abs() < 2.);
        let reflowed = matches!((after.2, after.3), (Some(native), Some(css)) if (f64::from(native) - css).abs() < 2.);
        report.check("expand.same-view-no-reload", grew && before.0 == after.0 && before.1 == after.1 && sized && reflowed
            && anchor_before.0 == anchor_after.0,
            json!({"heightBefore": before.2, "heightAfter": after.2, "cssHeight": after.3, "loads": [before.0, after.0],
                   "creations": [before.1, after.1], "anchor": [anchor_before, anchor_after]}));

        // 7. Wheel routing: positive control over a plain row, then over a
        // scrollable document and a document that fits its card.
        scroll(window, cx, Pane::Left, 3, 0.)?;
        pause(cx, 400).await;
        // Below the expanded card at the top of the left list: an ordinary row.
        let plain = (330.0_f64, f64::from(HEADER + EXPANDED + 20.));
        let top_before = list_top(window, cx, Pane::Left)?;
        let wheel = with_doc(registry, "doc-a", |doc| {
            doc.scroll_wheel(plain.0, plain.1, -240)
        })
        .unwrap_or(Err("doc-a missing".into()));
        pause(cx, 600).await;
        let top_after = list_top(window, cx, Pane::Left)?;
        let delivered = wheel.is_ok() && top_before != top_after;
        report.check(
            "wheel.synthetic-delivery-control",
            delivered,
            json!({"post": wheel.err(), "before": top_before, "after": top_after}),
        );
        scroll(window, cx, Pane::Left, 3, 0.)?;
        pause(cx, 400).await;
        let mut wheel_observations = BTreeMap::new();
        for (id, row) in [("doc-a", 3usize), ("doc-b", 120)] {
            scroll(window, cx, Pane::Left, row, 0.)?;
            let shown = wait_for(cx, 8000, || {
                with_doc(registry, id, |doc| doc.visible()).unwrap_or(false)
                    && registry.borrow().slot(id).loads >= 1
            })
            .await;
            pause(cx, 300).await;
            let Some((x, y, _)) = doc_point(registry, id).filter(|_| shown) else {
                wheel_observations.insert(id, json!({"error": "not on screen"}));
                continue;
            };
            let scrolls_before = accepted(&registry.borrow().slot(id).events, "scroll").len();
            let top_before = list_top(window, cx, Pane::Left)?;
            let _ = with_doc(registry, id, |doc| doc.scroll_wheel(x, y, -240));
            pause(cx, 700).await;
            let top_after = list_top(window, cx, Pane::Left)?;
            let scrolls =
                accepted(&registry.borrow().slot(id).events, "scroll").len() - scrolls_before;
            wheel_observations.insert(
                id,
                json!({"documentScrolled": scrolls > 0, "transcriptMoved": top_before != top_after,
                "before": top_before, "after": top_after}),
            );
        }
        report.observe("wheel.overDocuments", json!(wheel_observations));

        // 8. Lifecycle: detach off screen, evict over budget, recreate.
        let creations_before = registry.borrow().slot("doc-a").creations;
        for row in [200usize, 240, 360, 3] {
            scroll(window, cx, Pane::Left, row, 0.)?;
            pause(cx, 700).await;
            let registry_ref = registry.borrow();
            let frame = registry_ref.frame;
            let orphans: Vec<&str> = registry_ref
                .slots
                .iter()
                .filter(|slot| {
                    slot.wanted_frame != frame && slot.doc.as_ref().is_some_and(|doc| doc.visible())
                })
                .map(|slot| slot.id)
                .collect();
            report.check(
                &format!("lifecycle.no-orphan-views at row {row}"),
                orphans.is_empty() && registry_ref.live() <= BUDGET,
                json!({"orphans": orphans, "live": registry_ref.live()}),
            );
        }
        let reloaded = wait_for(cx, 8000, || {
            let registry = registry.borrow();
            let slot = registry.slot("doc-a");
            slot.doc.as_ref().is_some_and(|doc| doc.visible())
                && slot.loads as usize >= slot.creations as usize
        })
        .await;
        {
            let registry = registry.borrow();
            let slot = registry.slot("doc-a");
            report.check("lifecycle.budget-and-reload", registry.max_live <= BUDGET && reloaded,
                json!({"maxLive": registry.max_live, "evictions": registry.evictions, "refusals": registry.refusals,
                       "docACreations": [creations_before, slot.creations], "docALoads": slot.loads}));
        }

        // 9. Hidden-view work: rAF frames while scrolled away but still live.
        pause(cx, 500).await;
        let visible_start = frames(cx, registry, "doc-a").await;
        pause(cx, 2000).await;
        let visible_end = frames(cx, registry, "doc-a").await;
        let creations = registry.borrow().slot("doc-a").creations;
        scroll(window, cx, Pane::Left, 200, 0.)?;
        pause(cx, 2000).await;
        scroll(window, cx, Pane::Left, 3, 0.)?;
        pause(cx, 150).await;
        let hidden_end = frames(cx, registry, "doc-a").await;
        let still_same_view = registry.borrow().slot("doc-a").creations == creations;
        let rate = |a: Option<(f64, Instant)>, b: Option<(f64, Instant)>| match (a, b) {
            (Some((c1, t1)), Some((c2, t2))) if t2 > t1 => {
                Some((c2 - c1) / (t2 - t1).as_secs_f64())
            }
            _ => None,
        };
        let visible_rate = rate(visible_start, visible_end);
        let hidden_rate = if still_same_view {
            rate(visible_end, hidden_end)
        } else {
            None
        };
        let rss = std::process::Command::new("/bin/ps")
            .args(["-o", "rss=", "-p", &std::process::id().to_string()])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned());
        report.observe(
            "performance.animationFramesPerSecond",
            json!({
                "visible": visible_rate,
                "hiddenIncludingTransitions": hidden_rate,
                "hiddenViewDestroyed": !still_same_view,
                "uiProcessRssKiB": rss,
            }),
        );

        // 10. Split resize: right-pane views follow; panes never overlap.
        window.update(cx, |harness, _, cx| {
            harness.split = 0.45;
            cx.notify();
        })?;
        pause(cx, 500).await;
        let width = window.update(cx, |_, window, _| f32::from(window.viewport_size().width))?;
        let split_x = width * 0.45;
        let (left_right_edge, right_left_edge) = {
            let registry = registry.borrow();
            let edge = |id: &str| {
                registry
                    .slot(id)
                    .doc
                    .as_ref()
                    .and_then(|d| d.visible_bounds())
                    .filter(|_| registry.slot(id).doc.as_ref().is_some_and(|d| d.visible()))
            };
            (
                edge("doc-a").map(|b| f32::from(b.origin.x + b.size.width)),
                edge("doc-control").map(|b| f32::from(b.origin.x)),
            )
        };
        report.check("split.panes-clip-independently",
            left_right_edge.is_some_and(|e| e <= split_x + 1.) && right_left_edge.is_some_and(|e| e >= split_x - 1.),
            json!({"splitX": split_x, "leftDocRight": left_right_edge, "rightDocLeft": right_left_edge}));
        captures.push(capture(output, "05-split"));
        report.observe("captures", json!(captures));
        Ok(())
    }
}
