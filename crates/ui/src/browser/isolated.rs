//! IC-0 prototype: one isolated, offline document view on macOS WebKit.
//!
//! Off the default build. It compiles only with the `browser-fixture`
//! feature and is driven by `examples/isolated-document-fixture.rs`; IC-1
//! owns promotion into the transcript. The ordinary browser (`macos.rs`) is
//! untouched and keeps its own policy.
//!
//! Isolation is layered at browser boundaries, never by replacing page APIs:
//! 1. A fresh nonpersistent website data store per view: no cookies, storage
//!    or cache shared with the browser, other documents or LUNA.
//! 2. A `luna-artifact` scheme handler serves only this view's in-memory
//!    shell, document and prepared resources; everything else is 404. Each
//!    HTML response carries a Content-Security-Policy header (offline:
//!    `connect-src 'none'`, no frames/workers/forms/base).
//! 3. The document runs in an opaque-origin `<iframe sandbox="allow-scripts">`
//!    inside a minimal trusted shell: no same-origin, popups, forms, modals,
//!    downloads or top navigation.
//! 4. The navigation delegate allows exactly the shell (main frame) and the
//!    document (subframe); every other navigation is cancelled and recorded.
//! 5. New windows, downloads and media permission requests are denied.
//! 6. The only native IPC is one script-message handler. It accepts a message
//!    only from the main frame, the shell's own URL and origin, carrying the
//!    per-view bridge token injected into the main frame alone. Child-frame
//!    posts are refused and recorded, never forwarded.
//!
//! `Isolation::Control` is the positive control for the fixture's probes:
//! the same document as the top-level page without CSP, sandbox or shell, so
//! a probe that cannot see a leak there is not evidence of isolation.
//!
//! All callbacks enqueue [`DocumentEvent`]s; nothing re-enters GPUI.
use gpui::{Bounds, Pixels, Window};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, class, define_class, msg_send};
use objc2_app_kit::{
    NSApplication, NSColor, NSEvent, NSEventMask, NSEventModifierFlags, NSEventType, NSView,
    NSWindowOrderingMode,
};
use objc2_foundation::{NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString};
use objc2_web_kit::{
    WKNavigationAction, WKNavigationActionPolicy, WKNavigationDelegate, WKNavigationResponse,
    WKNavigationResponsePolicy, WKScriptMessage, WKScriptMessageHandler, WKUserContentController,
    WKUserScript, WKUserScriptInjectionTime, WKWebView,
};
use serde_json::Value;
use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use wry::{WebView, WebViewBuilderExtMacos, WebViewExtMacOS};

pub const SCHEME: &str = "luna-artifact";
const ORIGIN: &str = "luna-artifact://localhost";
const HANDLER: &str = "lunaShell";
const MAX_MESSAGE_BYTES: usize = 64 * 1024;
const ESCAPE_KEY_CODE: u16 = 53;
const SHELL_CSP: &str = "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; frame-src luna-artifact:; base-uri 'none'; form-action 'none'";
const DOCUMENT_CSP: &str = "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data: luna-artifact:; font-src data:; media-src data:; connect-src 'none'; frame-src 'none'; worker-src 'none'; form-action 'none'; base-uri 'none'";
const SHELL_SCRIPT: &str = r#"(() => {
  const token = window.__lunaBridgeToken;
  delete window.__lunaBridgeToken;
  const frame = document.getElementById('document');
  const kinds = new Set(['loaded', 'probe', 'frames', 'scroll', 'size', 'escape']);
  const post = (kind, data) => window.webkit.messageHandlers.lunaShell.postMessage(
    JSON.stringify({ token, kind, data: data === undefined ? null : data }));
  window.addEventListener('message', (event) => {
    if (event.source !== frame.contentWindow) { post('foreignMessage', null); return; }
    const message = event.data;
    if (!message || typeof message !== 'object' || !kinds.has(message.type)) return;
    post(message.type, message.data === undefined ? null : message.data);
  });
  Object.defineProperty(window, '__lunaShell', {
    value: Object.freeze({ send(message) { frame.contentWindow.postMessage(message, '*'); } }),
  });
  post('shellReady', null);
})();"#;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Isolation {
    Isolated,
    /// Positive control: top-level document, no CSP, sandbox or shell.
    Control,
}

/// One prepared resource the document may load by
/// `luna-artifact://localhost/<id>/res/<name>`.
#[derive(Clone, Debug)]
pub struct PreparedResource {
    pub name: String,
    pub mime: String,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DocumentEvent {
    /// A validated shell message (Control: any main-frame message).
    Accepted {
        kind: String,
        data: Value,
    },
    /// A script message that failed validation; never forwarded.
    Refused {
        main_frame: bool,
        origin: String,
        reason: &'static str,
    },
    Navigation {
        url: String,
        main_frame: bool,
        allowed: bool,
    },
    NewWindow {
        url: String,
    },
    Download {
        url: String,
    },
    /// Escape moved first responder from the document back to GPUI.
    EscapedFocus,
    Terminated,
}

type Events = Rc<RefCell<Vec<DocumentEvent>>>;

static LIVE_VIEWS: AtomicUsize = AtomicUsize::new(0);

/// Native document hosts currently alive in this process (teardown evidence).
pub fn live_views() -> usize {
    LIVE_VIEWS.load(Ordering::SeqCst)
}

struct Assets {
    id: String,
    isolation: Isolation,
    shell: Vec<u8>,
    document: Vec<u8>,
    resources: Vec<PreparedResource>,
}

impl Assets {
    fn route(&self, host: &str, path: &str) -> (u16, String, Vec<u8>, Option<&'static str>) {
        let missing = (404, "text/plain".to_owned(), Vec::new(), None);
        if host != "localhost" {
            return missing;
        }
        let Some(rest) = path
            .strip_prefix('/')
            .and_then(|path| path.strip_prefix(self.id.as_str()))
            .and_then(|path| path.strip_prefix('/'))
        else {
            return missing;
        };
        let html = "text/html; charset=utf-8".to_owned();
        match rest {
            "shell.html" if self.isolation == Isolation::Isolated => {
                (200, html, self.shell.clone(), Some(SHELL_CSP))
            }
            "document.html" => (
                200,
                html,
                self.document.clone(),
                (self.isolation == Isolation::Isolated).then_some(DOCUMENT_CSP),
            ),
            _ => match rest
                .strip_prefix("res/")
                .and_then(|name| self.resources.iter().find(|resource| resource.name == name))
            {
                Some(resource) => (200, resource.mime.clone(), resource.bytes.clone(), None),
                None => missing,
            },
        }
    }

    fn respond(
        &self,
        request: &wry::http::Request<Vec<u8>>,
        served: &Mutex<Vec<String>>,
    ) -> wry::http::Response<Cow<'static, [u8]>> {
        let uri = request.uri();
        let path = uri.path().to_owned();
        let (status, mime, body, csp) = self.route(uri.host().unwrap_or_default(), &path);
        if let Ok(mut served) = served.lock()
            && served.len() < 1024
        {
            served.push(format!("{status} {path}"));
        }
        let mut builder = wry::http::Response::builder()
            .status(status)
            .header("Content-Type", mime)
            .header("Cache-Control", "no-store")
            .header("X-Content-Type-Options", "nosniff");
        if let Some(csp) = csp {
            builder = builder.header("Content-Security-Policy", csp);
        }
        builder
            .body(Cow::Owned(body))
            .unwrap_or_else(|_| wry::http::Response::new(Cow::Borrowed(&[][..])))
    }
}

fn shell_html(document_url: &str) -> String {
    format!(
        "<!doctype html><meta charset=\"utf-8\"><style>html,body{{margin:0;height:100%;overflow:hidden;background:transparent}}iframe{{display:block;border:0;width:100%;height:100%}}</style><iframe id=\"document\" sandbox=\"allow-scripts\" referrerpolicy=\"no-referrer\" src=\"{document_url}\"></iframe><script>{script}</script>",
        script = SHELL_SCRIPT
    )
}

struct ObserverState {
    events: Events,
    isolation: Isolation,
    shell_url: String,
    document_url: String,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "LunaIsolatedDocumentObserver"]
    #[ivars = ObserverState]
    struct Observer;
    unsafe impl NSObjectProtocol for Observer {}
    unsafe impl WKNavigationDelegate for Observer {
        #[unsafe(method(webView:decidePolicyForNavigationAction:decisionHandler:))]
        fn policy(
            &self,
            _view: &WKWebView,
            action: &WKNavigationAction,
            decision: &block2::Block<dyn Fn(WKNavigationActionPolicy)>,
        ) {
            let url = unsafe { action.request().URL() }
                .and_then(|u| u.absoluteString())
                .map(|u| u.to_string())
                .unwrap_or_default();
            let main_frame =
                unsafe { action.targetFrame() }.is_some_and(|frame| unsafe { frame.isMainFrame() });
            let allowed = self.allows(&url, main_frame);
            self.ivars()
                .events
                .borrow_mut()
                .push(DocumentEvent::Navigation {
                    url,
                    main_frame,
                    allowed,
                });
            decision.call((if allowed {
                WKNavigationActionPolicy::Allow
            } else {
                WKNavigationActionPolicy::Cancel
            },));
        }
        #[unsafe(method(webView:decidePolicyForNavigationResponse:decisionHandler:))]
        fn response(
            &self,
            _view: &WKWebView,
            response: &WKNavigationResponse,
            decision: &block2::Block<dyn Fn(WKNavigationResponsePolicy)>,
        ) {
            let url = unsafe { response.response() }
                .URL()
                .and_then(|u| u.absoluteString())
                .map(|u| u.to_string())
                .unwrap_or_default();
            let allowed = self.ivars().isolation == Isolation::Control
                || url.starts_with(&format!("{ORIGIN}/"));
            decision.call((if allowed {
                WKNavigationResponsePolicy::Allow
            } else {
                WKNavigationResponsePolicy::Cancel
            },));
        }
        #[unsafe(method(webViewWebContentProcessDidTerminate:))]
        fn terminated(&self, _view: &WKWebView) {
            self.ivars()
                .events
                .borrow_mut()
                .push(DocumentEvent::Terminated);
        }
    }
);

impl Observer {
    fn new(state: ObserverState, mtm: MainThreadMarker) -> Retained<Self> {
        let object = mtm.alloc::<Self>().set_ivars(state);
        unsafe { msg_send![super(object), init] }
    }

    fn allows(&self, url: &str, main_frame: bool) -> bool {
        let state = self.ivars();
        match state.isolation {
            Isolation::Control => true,
            Isolation::Isolated if main_frame => url == state.shell_url,
            Isolation::Isolated => url == state.document_url,
        }
    }
}

struct BridgeState {
    events: Events,
    isolation: Isolation,
    token: String,
    entry_url: String,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "LunaIsolatedDocumentBridge"]
    #[ivars = BridgeState]
    struct Bridge;
    unsafe impl NSObjectProtocol for Bridge {}
    unsafe impl WKScriptMessageHandler for Bridge {
        #[unsafe(method(userContentController:didReceiveScriptMessage:))]
        fn received(&self, _controller: &WKUserContentController, message: &WKScriptMessage) {
            self.accept(message);
        }
    }
);

impl Bridge {
    fn new(state: BridgeState, mtm: MainThreadMarker) -> Retained<Self> {
        let object = mtm.alloc::<Self>().set_ivars(state);
        unsafe { msg_send![super(object), init] }
    }

    /// Validate frame, origin, URL, size, shape and token before anything is
    /// enqueued. A refusal records only the frame kind, origin and reason.
    fn accept(&self, message: &WKScriptMessage) {
        let state = self.ivars();
        let frame = unsafe { message.frameInfo() };
        let main_frame = unsafe { frame.isMainFrame() };
        let origin = unsafe { frame.securityOrigin() };
        let origin = format!("{}://{}", unsafe { origin.protocol() }, unsafe {
            origin.host()
        });
        let frame_url = unsafe { frame.request() }
            .URL()
            .and_then(|url| url.absoluteString())
            .map(|url| url.to_string())
            .unwrap_or_default();
        let body = unsafe { message.body() };
        let text = body.downcast_ref::<NSString>().map(|text| text.to_string());
        let refuse = |reason: &'static str| {
            state.events.borrow_mut().push(DocumentEvent::Refused {
                main_frame,
                origin: origin.clone(),
                reason,
            });
        };
        if !main_frame {
            return refuse("child frame");
        }
        if origin != ORIGIN || frame_url != state.entry_url {
            return refuse("unexpected origin");
        }
        let Some(value) = text
            .filter(|text| text.len() <= MAX_MESSAGE_BYTES)
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        else {
            return refuse("malformed");
        };
        let Some(kind) = value
            .get("kind")
            .and_then(Value::as_str)
            .filter(|kind| !kind.is_empty() && kind.len() <= 64)
        else {
            return refuse("malformed");
        };
        if state.isolation == Isolation::Isolated
            && value.get("token").and_then(Value::as_str) != Some(state.token.as_str())
        {
            return refuse("bad token");
        }
        let data = value.get("data").cloned().unwrap_or(Value::Null);
        state.events.borrow_mut().push(DocumentEvent::Accepted {
            kind: kind.to_owned(),
            data,
        });
    }
}

struct ClipState {
    region: Cell<NSRect>,
}

// Same clipping as the browser's `LunaBrowserClipView` (IC-1: share one
// implementation): hits outside GPUI's current paint mask fall through.
define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "LunaIsolatedDocumentClipView"]
    #[ivars = ClipState]
    struct ClipView;
    impl ClipView {
        #[unsafe(method(hitTest:))]
        fn hit_test(&self, point: NSPoint) -> *mut NSView {
            let region = self.ivars().region.get();
            if point.x < region.origin.x || point.x >= region.origin.x + region.size.width
                || point.y < region.origin.y || point.y >= region.origin.y + region.size.height
            { std::ptr::null_mut() }
            else { unsafe { msg_send![super(self), hitTest: point] } }
        }
    }
);

/// Opaque `CGEvent` so `+[NSEvent eventWithCGEvent:]` gets its exact encoding.
/// Only ever named behind a pointer.
#[allow(dead_code)]
#[repr(C)]
struct CGEventOpaque {
    _private: [u8; 0],
}

unsafe impl objc2::encode::RefEncode for CGEventOpaque {
    const ENCODING_REF: objc2::encode::Encoding =
        objc2::encode::Encoding::Pointer(&objc2::encode::Encoding::Struct("__CGEvent", &[]));
}

struct Host {
    web: WebView,
    view: Retained<WKWebView>,
    /// WebKit holds its navigation delegate weakly.
    _observer: Retained<Observer>,
    controller: Retained<objc2_web_kit::WKUserContentController>,
    clip: Retained<ClipView>,
    clip_mask: Retained<AnyObject>,
    parent: Retained<NSView>,
    monitor: Option<Retained<AnyObject>>,
    bounds: Option<Bounds<Pixels>>,
    visible_bounds: Option<Bounds<Pixels>>,
    shown: bool,
    isolation: Isolation,
    events: Events,
    served: Arc<Mutex<Vec<String>>>,
}

/// One live isolated document view. Dropping it removes the native view.
pub struct IsolatedDocument(Rc<RefCell<Host>>);

impl IsolatedDocument {
    /// `id` names the document's scoped origin path (`[A-Za-z0-9-]`, ≤ 64).
    pub fn new(
        window: &Window,
        id: &str,
        document: &str,
        resources: Vec<PreparedResource>,
        isolation: Isolation,
    ) -> Result<Self, String> {
        let mtm = MainThreadMarker::new().ok_or("Isolated documents need the main thread")?;
        if id.is_empty()
            || id.len() > 64
            || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err("invalid document id".into());
        }
        let token = uuid::Uuid::new_v4().simple().to_string();
        let shell_url = format!("{ORIGIN}/{id}/shell.html");
        let document_url = format!("{ORIGIN}/{id}/document.html");
        let entry_url = match isolation {
            Isolation::Isolated => shell_url.clone(),
            Isolation::Control => document_url.clone(),
        };
        let events: Events = Rc::default();
        let served = Arc::new(Mutex::new(Vec::new()));
        let assets = Arc::new(Assets {
            id: id.to_owned(),
            isolation,
            shell: shell_html(&document_url).into_bytes(),
            document: document.as_bytes().to_vec(),
            resources,
        });

        let configuration = unsafe { objc2_web_kit::WKWebViewConfiguration::new(mtm) };
        let controller = unsafe {
            configuration.setWebsiteDataStore(
                &objc2_web_kit::WKWebsiteDataStore::nonPersistentDataStore(mtm),
            );
            configuration.userContentController()
        };
        let bridge = Bridge::new(
            BridgeState {
                events: events.clone(),
                isolation,
                token: token.clone(),
                entry_url: entry_url.clone(),
            },
            mtm,
        );
        unsafe {
            controller.addScriptMessageHandler_name(
                ProtocolObject::from_ref(&*bridge),
                &NSString::from_str(HANDLER),
            );
            if isolation == Isolation::Isolated {
                let script = WKUserScript::initWithSource_injectionTime_forMainFrameOnly(
                    mtm.alloc::<WKUserScript>(),
                    &NSString::from_str(&format!("window.__lunaBridgeToken = '{token}';")),
                    WKUserScriptInjectionTime::AtDocumentStart,
                    true,
                );
                controller.addUserScript(&script);
            }
        }

        let new_window = events.clone();
        let download = events.clone();
        let handler_assets = assets.clone();
        let handler_served = served.clone();
        let web = wry::WebViewBuilder::new()
            .with_webview_configuration(configuration)
            .with_visible(false)
            .with_focused(false)
            .with_custom_protocol(SCHEME.into(), move |_, request| {
                handler_assets.respond(&request, &handler_served)
            })
            .with_new_window_req_handler(move |url, _| {
                new_window
                    .borrow_mut()
                    .push(DocumentEvent::NewWindow { url });
                wry::NewWindowResponse::Deny
            })
            .with_download_started_handler(move |url, _| {
                download.borrow_mut().push(DocumentEvent::Download { url });
                false
            })
            .with_permission_handler(|_| wry::PermissionResponse::Deny)
            .build_as_child(window)
            .map_err(|e| e.to_string())?;
        let view = Retained::into_super(web.webview());
        window
            .enable_scene_overlay()
            .map_err(|error| error.to_string())?;
        let parent = unsafe { view.superview() }.ok_or("Isolated document parent is missing")?;
        let clip: Retained<ClipView> = unsafe {
            let object = mtm.alloc::<ClipView>().set_ivars(ClipState {
                region: Cell::new(NSRect::ZERO),
            });
            msg_send![super(object), initWithFrame: NSRect::ZERO]
        };
        clip.setWantsLayer(true);
        clip.setAutoresizesSubviews(false);
        let clip_mask: Retained<AnyObject> = unsafe { msg_send![class!(CALayer), new] };
        unsafe {
            let color = NSColor::blackColor().CGColor();
            let _: () = msg_send![&*clip_mask, setBackgroundColor: &*color];
            let layer: *mut AnyObject = msg_send![&*clip, layer];
            let _: () = msg_send![layer, setMasksToBounds: true];
            let _: () = msg_send![layer, setMask: &*clip_mask];
        }
        clip.setHidden(true);
        parent.addSubview(&clip);
        // Beneath GPUI's overlay plane, like browser tabs: menus stay on top.
        for sibling in parent.subviews() {
            if sibling.class().name() == c"GPUIOverlayView" {
                parent.addSubview_positioned_relativeTo(
                    &clip,
                    NSWindowOrderingMode::Below,
                    Some(&sibling),
                );
                break;
            }
        }
        clip.addSubview(&view);
        let observer = Observer::new(
            ObserverState {
                events: events.clone(),
                isolation,
                shell_url,
                document_url,
            },
            mtm,
        );
        unsafe {
            view.setNavigationDelegate(Some(ProtocolObject::from_ref(&*observer)));
        }

        // Escape always returns focus to GPUI, whatever the page does with
        // keyboard events. It is consumed only while this view has focus.
        let monitor_view = view.clone();
        let monitor_parent = parent.clone();
        let monitor_events = events.clone();
        let callback = block2::RcBlock::new(move |event: std::ptr::NonNull<NSEvent>| {
            let e = unsafe { event.as_ref() };
            if e.keyCode() != ESCAPE_KEY_CODE
                || monitor_view.isHidden()
                || !has_focus(&monitor_view)
                || e.window(mtm) != monitor_view.window()
            {
                return event.as_ptr();
            }
            if let Some(window) = monitor_view.window() {
                window.makeFirstResponder(Some(&monitor_parent));
            }
            monitor_events
                .borrow_mut()
                .push(DocumentEvent::EscapedFocus);
            std::ptr::null_mut()
        });
        let monitor = unsafe {
            NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &callback)
        };
        // The controller retains the handler; Drop removes it by name.
        drop(bridge);
        LIVE_VIEWS.fetch_add(1, Ordering::SeqCst);
        // Constructed before the first load: any failure from here on is
        // cleaned up by `Host::drop`, never left in the view hierarchy.
        let document = Self(Rc::new(RefCell::new(Host {
            web,
            view,
            _observer: observer,
            controller,
            clip,
            clip_mask,
            parent,
            monitor,
            bounds: None,
            visible_bounds: None,
            shown: false,
            isolation,
            events,
            served,
        })));
        let loaded = document.0.borrow().web.load_url(&entry_url);
        loaded.map_err(|e| e.to_string())?;
        Ok(document)
    }

    pub fn isolation(&self) -> Isolation {
        self.0.borrow().isolation
    }

    /// Place and show the view at GPUI `bounds`, clipped to `mask`. Call from
    /// `Window::on_present` so native geometry follows the presented frame.
    pub fn sync(&self, bounds: Bounds<Pixels>, mask: Bounds<Pixels>) {
        let mut host = self.0.borrow_mut();
        host.shown = true;
        host.place(bounds, mask);
        host.update_visibility();
    }

    /// Hide without destroying (off-screen row, collapsed card).
    pub fn hide(&self) {
        let mut host = self.0.borrow_mut();
        if host.shown {
            host.shown = false;
            host.update_visibility();
        }
    }

    pub fn visible(&self) -> bool {
        let host = self.0.borrow();
        !host.view.isHidden() && !host.clip.isHidden()
    }

    pub fn bounds(&self) -> Option<Bounds<Pixels>> {
        self.0.borrow().bounds
    }

    pub fn visible_bounds(&self) -> Option<Bounds<Pixels>> {
        self.0.borrow().visible_bounds
    }

    pub fn take_events(&self) -> Vec<DocumentEvent> {
        std::mem::take(&mut *self.0.borrow().events.borrow_mut())
    }

    /// `"<status> <path>"` for every scheme request this view made.
    pub fn served(&self) -> Vec<String> {
        self.0
            .borrow()
            .served
            .lock()
            .map(|served| served.clone())
            .unwrap_or_default()
    }

    /// Deliver a host message to the document through the shell (Control:
    /// directly to the top-level page).
    pub fn send(&self, message: &Value) {
        let json = message.to_string();
        let script = match self.0.borrow().isolation {
            Isolation::Isolated => {
                format!("window.__lunaShell && window.__lunaShell.send({json})")
            }
            Isolation::Control => format!("window.postMessage({json}, '*')"),
        };
        self.eval(&script);
    }

    fn eval(&self, script: &str) {
        unsafe {
            self.0
                .borrow()
                .view
                .evaluateJavaScript_completionHandler(&NSString::from_str(script), None);
        }
    }

    pub fn focus(&self) {
        let _ = self.0.borrow().web.focus();
    }

    pub fn focused(&self) -> bool {
        has_focus(&self.0.borrow().view)
    }

    /// Whether a window point (GPUI top-left coordinates) hits this view.
    pub fn hit(&self, x: f64, y: f64) -> bool {
        let host = self.0.borrow();
        host.parent
            .hitTest(host.point(x, y))
            .is_some_and(|hit| hit.isDescendantOf(&host.clip))
    }

    /// Whether a window point hits GPUI's overlay plane (menus, prompts).
    pub fn overlay_at(&self, x: f64, y: f64) -> bool {
        let host = self.0.borrow();
        host.parent
            .hitTest(host.point(x, y))
            .is_some_and(|hit| hit.class().name() == c"GPUIOverlayView")
    }

    /// Native left click through AppKit's own dispatch and hit testing.
    pub fn click(&self, x: f64, y: f64) {
        let host = self.0.borrow();
        host.warp_cursor(x, y);
        let Some(window) = host.view.window() else {
            return;
        };
        let point = host.point(x, y);
        let app = NSApplication::sharedApplication(host.view.mtm());
        for kind in [NSEventType::LeftMouseDown, NSEventType::LeftMouseUp] {
            if let Some(event) = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(kind, point, NSEventModifierFlags::empty(), 0., window.windowNumber(), None, 0, 1, 1.) {
                app.postEvent_atStart(&event, false);
            }
        }
    }

    /// Native Escape key-down into the window's event queue.
    pub fn press_escape(&self) {
        let host = self.0.borrow();
        let Some(window) = host.view.window() else {
            return;
        };
        let escape = NSString::from_str("\u{1b}");
        let app = NSApplication::sharedApplication(host.view.mtm());
        if let Some(event) = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(NSEventType::KeyDown, NSPoint::new(0., 0.), NSEventModifierFlags::empty(), 0., window.windowNumber(), None, &escape, &escape, false, ESCAPE_KEY_CODE) {
            app.postEvent_atStart(&event, false);
        }
    }

    /// A real pixel scroll-wheel event at a window point, routed by AppKit
    /// (WebKit first if the point hits this view, otherwise GPUI).
    pub fn scroll_wheel(&self, x: f64, y: f64, delta_y: i32) -> Result<(), String> {
        use core_graphics::event::{CGEvent, ScrollEventUnit};
        use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
        use foreign_types::ForeignType;
        let host = self.0.borrow();
        host.warp_cursor(x, y);
        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .map_err(|_| "no CoreGraphics event source")?;
        let event = CGEvent::new_scroll_event(source, ScrollEventUnit::PIXEL, 1, delta_y, 0, 0)
            .map_err(|_| "could not create a scroll event")?;
        let raw = event.as_ptr() as *mut CGEventOpaque;
        let ns: Option<Retained<NSEvent>> =
            unsafe { msg_send![class!(NSEvent), eventWithCGEvent: raw] };
        let ns = ns.ok_or("NSEvent could not wrap the scroll event")?;
        NSApplication::sharedApplication(host.view.mtm()).postEvent_atStart(&ns, false);
        Ok(())
    }
}

fn has_focus(view: &NSView) -> bool {
    view.window()
        .and_then(|w| w.firstResponder())
        .and_then(|r| r.downcast::<NSView>().ok())
        .is_some_and(|v| v.isDescendantOf(view))
}

impl Host {
    fn point(&self, x: f64, y: f64) -> NSPoint {
        NSPoint::new(
            x,
            if self.parent.isFlipped() {
                y
            } else {
                self.parent.bounds().size.height - y
            },
        )
    }

    fn warp_cursor(&self, x: f64, y: f64) {
        #[link(name = "CoreGraphics", kind = "framework")]
        unsafe extern "C" {
            fn CGWarpMouseCursorPosition(point: NSPoint) -> i32;
        }
        let Some(window) = self.view.window() else {
            return;
        };
        let height = self.parent.bounds().size.height;
        let p = window.convertPointToScreen(NSPoint::new(x, height - y));
        let Some(screen) = objc2_app_kit::NSScreen::mainScreen(self.view.mtm()) else {
            return;
        };
        unsafe {
            CGWarpMouseCursorPosition(NSPoint::new(p.x, screen.frame().size.height - p.y));
        }
    }

    fn place(&mut self, bounds: Bounds<Pixels>, mask: Bounds<Pixels>) {
        let visible = bounds.intersect(&mask);
        if self.bounds == Some(bounds) && self.visible_bounds == Some(visible) {
            return;
        }
        self.bounds = Some(bounds);
        self.visible_bounds = Some(visible);
        let flip = |y: f64, height: f64| {
            if self.parent.isFlipped() {
                y
            } else {
                self.parent.bounds().size.height - y - height
            }
        };
        let width = f64::from(f32::from(visible.size.width)).max(0.);
        let height = f64::from(f32::from(visible.size.height)).max(0.);
        let region = NSRect::new(
            NSPoint::new(
                f64::from(f32::from(visible.origin.x)),
                flip(f64::from(f32::from(visible.origin.y)), height),
            ),
            NSSize::new(width, height),
        );
        let web_height = f64::from(f32::from(bounds.size.height)).max(0.);
        let web = NSRect::new(
            NSPoint::new(
                f64::from(f32::from(bounds.origin.x)),
                flip(f64::from(f32::from(bounds.origin.y)), web_height),
            ),
            NSSize::new(f64::from(f32::from(bounds.size.width)).max(0.), web_height),
        );
        let actions_disabled: bool = unsafe {
            let previous = msg_send![class!(CATransaction), disableActions];
            let _: () = msg_send![class!(CATransaction), setDisableActions: true];
            previous
        };
        self.clip.setFrame(self.parent.bounds());
        self.clip.ivars().region.set(region);
        unsafe {
            let _: () = msg_send![&*self.clip_mask, setFrame: region];
        }
        self.view.setFrame(web);
        unsafe {
            let _: () = msg_send![class!(CATransaction), setDisableActions: actions_disabled];
        }
    }

    fn update_visibility(&self) {
        let visible = self.shown
            && self
                .visible_bounds
                .is_some_and(|b| f32::from(b.size.width) > 0. && f32::from(b.size.height) > 0.);
        if !visible && has_focus(&self.view) {
            let _ = self.web.focus_parent();
        }
        if self.view.isHidden() == visible {
            let _ = self.web.set_visible(visible);
        }
        self.clip.setHidden(!visible);
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        if has_focus(&self.view) {
            let _ = self.web.focus_parent();
        }
        unsafe {
            if let Some(monitor) = self.monitor.take() {
                NSEvent::removeMonitor(&monitor);
            }
            self.controller
                .removeScriptMessageHandlerForName(&NSString::from_str(HANDLER));
            self.view.setNavigationDelegate(None);
            self.view.stopLoading();
        }
        self.view.removeFromSuperview();
        self.clip.removeFromSuperview();
        LIVE_VIEWS.fetch_sub(1, Ordering::SeqCst);
    }
}
