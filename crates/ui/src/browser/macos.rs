//! AppKit boundary for the browser. Wry owns the native child and its UI
//! delegate; our navigation delegate supplies browser policy and state, and a
//! shared download delegate saves files. All callbacks enqueue events, never
//! re-enter GPUI. No page-to-engine IPC.
use super::model::{Download, PageState, Presentation, allowed_navigation};
use gpui::{Bounds, Pixels, Window};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{
    DefinedClass, MainThreadMarker, MainThreadOnly, Message, class, define_class, msg_send, sel,
};
use objc2_app_kit::{
    NSColor, NSEvent, NSEventMask, NSEventModifierFlags, NSView, NSWindowOrderingMode,
};
use objc2_foundation::{
    NSData, NSDictionary, NSError, NSHTTPURLResponse, NSKeyValueChangeKey,
    NSKeyValueObservingOptions, NSObject, NSObjectNSKeyValueObserverRegistration, NSObjectProtocol,
    NSString, NSURL, NSURLResponse,
};
use objc2_web_kit::{
    WKDownload, WKDownloadDelegate, WKNavigation, WKNavigationAction, WKNavigationActionPolicy,
    WKNavigationDelegate, WKNavigationResponse, WKNavigationResponsePolicy, WKWebView,
};
use std::{
    cell::{Cell, RefCell},
    path::{Path, PathBuf},
    rc::Rc,
};
use wry::{WebView, WebViewBuilderExtMacos, WebViewExtMacOS};

#[derive(Default)]
struct BrowserStore {
    store: Option<Retained<objc2_web_kit::WKWebsiteDataStore>>,
    preview_hosts: std::collections::BTreeSet<String>,
    /// Outlives individual tabs, so closing one does not orphan a download.
    downloads: Option<Retained<Downloads>>,
}
#[derive(Clone, Default)]
pub(super) struct BrowserData(Rc<RefCell<BrowserStore>>);
impl BrowserData {
    fn configuration(
        &self,
        mtm: MainThreadMarker,
    ) -> Retained<objc2_web_kit::WKWebViewConfiguration> {
        let mut data = self.0.borrow_mut();
        if data.store.is_none() {
            data.store =
                Some(unsafe { objc2_web_kit::WKWebsiteDataStore::nonPersistentDataStore(mtm) });
            if let Err(error) =
                configure_preview_proxy(data.store.as_ref().unwrap(), &data.preview_hosts)
            {
                tracing::warn!(%error, "preview hostname proxy unavailable");
            }
        }
        let configuration = unsafe { objc2_web_kit::WKWebViewConfiguration::new(mtm) };
        unsafe {
            configuration.setWebsiteDataStore(data.store.as_ref().unwrap());
        }
        configuration
    }
    fn downloads(&self, mtm: MainThreadMarker) -> Retained<Downloads> {
        self.0
            .borrow_mut()
            .downloads
            .get_or_insert_with(|| Downloads::new(mtm))
            .clone()
    }
    pub(super) fn register_preview(&self, address: &str) {
        let Ok(url) = url::Url::parse(address) else {
            return;
        };
        let Some(host) = url.host_str() else {
            return;
        };
        if url.scheme() != "http"
            || url.port() != Some(luna_proto::PREVIEW_PROXY_PORT)
            || !host.ends_with(".localhost")
        {
            return;
        }
        let mut data = self.0.borrow_mut();
        if !data.preview_hosts.insert(host.into()) {
            return;
        }
        if let Some(store) = &data.store {
            if let Err(error) = configure_preview_proxy(store, &data.preview_hosts) {
                tracing::warn!(%error, "preview hostname proxy unavailable");
            }
        }
    }
}

/// Network.framework objects are Objective-C OS objects. Resolve the newer API
/// dynamically so older systems can still open ordinary browser tabs. Match
/// only discovered/opened preview hostnames; other websites retain normal routing.
fn configure_preview_proxy(
    store: &objc2_web_kit::WKWebsiteDataStore,
    hosts: &std::collections::BTreeSet<String>,
) -> Result<(), String> {
    if hosts.is_empty() {
        return Ok(());
    }
    use objc2_foundation::{NSArray, NSObject};
    use std::ffi::{CStr, CString, c_char};
    unsafe {
        let supported: bool = msg_send![store, respondsToSelector: sel!(setProxyConfigurations:)];
        if !supported {
            return Err("Automatic preview hostnames require macOS 14 or later".into());
        }
        static NETWORK: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
        let handle = *NETWORK.get_or_init(|| {
            libc::dlopen(
                c"/System/Library/Frameworks/Network.framework/Network".as_ptr(),
                libc::RTLD_LAZY,
            ) as usize
        });
        let symbol = |name: &CStr| -> Result<*mut std::ffi::c_void, String> {
            let value = libc::dlsym(handle as *mut _, name.as_ptr());
            if handle == 0 || value.is_null() {
                Err("Preview proxy API unavailable".into())
            } else {
                Ok(value)
            }
        };
        let endpoint: unsafe extern "C" fn(*const c_char, *const c_char) -> *mut NSObject =
            std::mem::transmute(symbol(c"nw_endpoint_create_host")?);
        let proxy: unsafe extern "C" fn(*mut NSObject, *mut NSObject) -> *mut NSObject =
            std::mem::transmute(symbol(c"nw_proxy_config_create_http_connect")?);
        let match_domain: unsafe extern "C" fn(*mut NSObject, *const c_char) =
            std::mem::transmute(symbol(c"nw_proxy_config_add_match_domain")?);
        let endpoint = Retained::from_raw(endpoint(c"127.0.0.1".as_ptr(), c"7331".as_ptr()))
            .ok_or("Could not create preview endpoint")?;
        let config = Retained::from_raw(proxy(
            Retained::as_ptr(&endpoint) as *mut _,
            std::ptr::null_mut(),
        ))
        .ok_or("Could not create preview proxy")?;
        for host in hosts {
            let host = CString::new(host.as_str()).map_err(|_| "Invalid preview hostname")?;
            match_domain(Retained::as_ptr(&config) as *mut _, host.as_ptr());
        }
        let proxies = NSArray::arrayWithObject(&*config);
        let _: () = msg_send![store, setProxyConfigurations: &*proxies];
    }
    Ok(())
}

type Sender = tokio::sync::mpsc::Sender<NativeEvent>;
const OBSERVED: [&str; 6] = [
    "URL",
    "title",
    "loading",
    "canGoBack",
    "canGoForward",
    "underPageBackgroundColor",
];

pub(super) enum NativeEvent {
    Changed,
    Finished,
    NewTab(String),
    Key(gpui::Keystroke),
    Favicon { page: String, url: String },
    Download(Download),
}

struct ObserverState {
    tx: Sender,
    downloads: Retained<Downloads>,
    pending: Cell<bool>,
    error: RefCell<Option<String>>,
    requested_url: RefCell<Option<String>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "LunaBrowserObserver"]
    #[ivars = ObserverState]
    struct Observer;
    unsafe impl NSObjectProtocol for Observer {}
    impl Observer {
        #[unsafe(method(observeValueForKeyPath:ofObject:change:context:))]
        fn observe(&self, _key: Option<&NSString>, _object: Option<&AnyObject>, _change: Option<&NSDictionary<NSKeyValueChangeKey, AnyObject>>, _context: *mut std::ffi::c_void) {
            self.changed();
        }
    }
    unsafe impl WKNavigationDelegate for Observer {
        #[unsafe(method(webView:decidePolicyForNavigationAction:decisionHandler:))]
        fn policy(&self, _view: &WKWebView, action: &WKNavigationAction, decision: &block2::Block<dyn Fn(WKNavigationActionPolicy)>) {
            let url = unsafe { action.request().URL() }.and_then(|u| u.absoluteString()).map(|u| u.to_string()).unwrap_or_default();
            // A download attribute saves the file, including a page's own
            // Blob or data exports, which are never displayed as pages.
            if unsafe { action.shouldPerformDownload() } && allowed_download(&url) {
                decision.call((WKNavigationActionPolicy::Download,));
                return;
            }
            if allowed_navigation(&url) && unsafe { action.targetFrame() }.is_some_and(|frame| unsafe { frame.isMainFrame() }) {
                *self.ivars().requested_url.borrow_mut() = Some(url.clone());
            }
            decision.call((if allowed_navigation(&url) { WKNavigationActionPolicy::Allow } else { WKNavigationActionPolicy::Cancel },));
        }
        #[unsafe(method(webView:decidePolicyForNavigationResponse:decisionHandler:))]
        fn response(&self, _view: &WKWebView, response: &WKNavigationResponse, decision: &block2::Block<dyn Fn(WKNavigationResponsePolicy)>) {
            let displayable = unsafe { response.canShowMIMEType() };
            // Only the page itself becomes a download: a frame's unsupported
            // content must not save files merely because a page embedded it.
            let policy = if !unsafe { response.isForMainFrame() } {
                if displayable { WKNavigationResponsePolicy::Allow } else { WKNavigationResponsePolicy::Cancel }
            } else if displayable && !attachment(&*unsafe { response.response() }) {
                WKNavigationResponsePolicy::Allow
            } else {
                // The current page stays; so does its address.
                self.ivars().requested_url.borrow_mut().take();
                self.changed();
                WKNavigationResponsePolicy::Download
            };
            decision.call((policy,));
        }
        #[unsafe(method(webView:navigationAction:didBecomeDownload:))]
        fn action_download(&self, view: &WKWebView, _action: &WKNavigationAction, download: &WKDownload) {
            self.ivars().downloads.adopt(view, download, self.ivars().tx.clone());
        }
        #[unsafe(method(webView:navigationResponse:didBecomeDownload:))]
        fn response_download(&self, view: &WKWebView, _response: &WKNavigationResponse, download: &WKDownload) {
            self.ivars().downloads.adopt(view, download, self.ivars().tx.clone());
        }
        #[unsafe(method(webView:didStartProvisionalNavigation:))]
        fn start(&self, _view: &WKWebView, _navigation: Option<&WKNavigation>) {
            self.ivars().error.borrow_mut().take(); self.changed();
        }
        #[unsafe(method(webView:didCommitNavigation:))]
        fn commit(&self, _view: &WKWebView, _navigation: Option<&WKNavigation>) {
            self.ivars().requested_url.borrow_mut().take(); self.changed();
        }
        #[unsafe(method(webView:didFinishNavigation:))]
        fn finish(&self, _view: &WKWebView, _navigation: Option<&WKNavigation>) {
            self.changed(); let _ = self.ivars().tx.try_send(NativeEvent::Finished);
        }
        #[unsafe(method(webView:didFailProvisionalNavigation:withError:))]
        fn provisional_error(&self, _view: &WKWebView, _navigation: Option<&WKNavigation>, error: &NSError) {
            if interrupted(error) {
                self.changed();
            } else if error.code() != -999 {
                tracing::warn!(domain = %error.domain(), code = error.code(), "browser provisional navigation failed");
                self.fail("Check the address and make sure your server is running, then try again.");
            }
        }
        #[unsafe(method(webView:didFailNavigation:withError:))]
        fn navigation_error(&self, _view: &WKWebView, _navigation: Option<&WKNavigation>, error: &NSError) {
            if interrupted(error) {
                self.changed();
            } else if error.code() != -999 {
                tracing::warn!(domain = %error.domain(), code = error.code(), "browser navigation failed");
                self.fail("The connection was interrupted. Try loading this page again.");
            }
        }
        #[unsafe(method(webViewWebContentProcessDidTerminate:))]
        fn terminated(&self, _view: &WKWebView) {
            self.fail("The page stopped responding. Reload to continue.");
        }
    }
);

impl Observer {
    fn new(tx: Sender, downloads: Retained<Downloads>, mtm: MainThreadMarker) -> Retained<Self> {
        let object = mtm.alloc().set_ivars(ObserverState {
            tx,
            downloads,
            pending: Cell::new(false),
            error: RefCell::new(None),
            requested_url: RefCell::new(None),
        });
        unsafe { msg_send![super(object), init] }
    }
    fn changed(&self) {
        if !self.ivars().pending.replace(true) {
            if self.ivars().tx.try_send(NativeEvent::Changed).is_err() {
                self.ivars().pending.set(false);
            }
        }
    }
    fn fail(&self, message: &str) {
        *self.ivars().error.borrow_mut() = Some(message.into());
        self.changed();
    }
}

/// A response saved as a download ends its navigation with this error. The
/// current page stays in place, so it is not a load failure.
fn interrupted(error: &NSError) -> bool {
    // WebKitErrorFrameLoadInterruptedByPolicyChange
    error.code() == 102 && error.domain().to_string() == "WebKitErrorDomain"
}

/// Website addresses, plus the page's own Blob and data exports. These other
/// schemes are only ever saved, never navigated to.
fn allowed_download(address: &str) -> bool {
    allowed_navigation(address)
        || url::Url::parse(address).is_ok_and(|url| match url.scheme() {
            "data" => true,
            "blob" => matches!(url.origin(), url::Origin::Tuple(scheme, ..) if scheme == "http" || scheme == "https"),
            _ => false,
        })
}

fn attachment(response: &NSURLResponse) -> bool {
    response
        .downcast_ref::<NSHTTPURLResponse>()
        .and_then(|http| {
            http.valueForHTTPHeaderField(objc2_foundation::ns_string!("Content-Disposition"))
        })
        .is_some_and(|value| {
            value
                .to_string()
                .trim_start()
                .to_ascii_lowercase()
                .starts_with("attachment")
        })
}

struct ActiveDownload {
    download: Retained<WKDownload>,
    tx: Sender,
    name: String,
    page: Option<String>,
    path: Option<PathBuf>,
}

#[derive(Default)]
struct DownloadsState {
    active: RefCell<Vec<ActiveDownload>>,
}

// WebKit holds its download delegate weakly. One per browser context keeps
// downloads reporting to their tab and finishing after that tab closes.
define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "LunaBrowserDownloads"]
    #[ivars = DownloadsState]
    struct Downloads;
    unsafe impl NSObjectProtocol for Downloads {}
    unsafe impl WKDownloadDelegate for Downloads {
        #[unsafe(method(download:decideDestinationUsingResponse:suggestedFilename:completionHandler:))]
        fn destination(
            &self,
            download: &WKDownload,
            _response: &NSURLResponse,
            suggested: &NSString,
            completion: &block2::DynBlock<dyn Fn(*mut NSURL)>,
        ) {
            let Some(index) = self.position(download) else {
                completion.call((std::ptr::null_mut(),));
                return;
            };
            let path = downloads_directory()
                .and_then(|directory| destination(&directory, &suggested.to_string()));
            let mut active = self.ivars().active.borrow_mut();
            let Some(path) = path else {
                let entry = active.remove(index);
                drop(active);
                entry.report(Some("Your Downloads folder isn’t available.".into()));
                completion.call((std::ptr::null_mut(),));
                return;
            };
            let entry = &mut active[index];
            entry.name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            entry.report(None);
            let url = NSURL::fileURLWithPath_isDirectory(
                &NSString::from_str(&path.to_string_lossy()),
                false,
            );
            entry.path = Some(path);
            drop(active);
            completion.call((Retained::as_ptr(&url) as *mut NSURL,));
        }
        #[unsafe(method(downloadDidFinish:))]
        fn finished(&self, download: &WKDownload) {
            if let Some(index) = self.position(download) {
                let entry = self.ivars().active.borrow_mut().remove(index);
                if let Some(path) = &entry.path {
                    quarantine(path, &entry.download, entry.page.as_deref());
                }
                entry.report(None);
            }
        }
        #[unsafe(method(download:didFailWithError:resumeData:))]
        fn failed(&self, download: &WKDownload, error: &NSError, _resume: Option<&NSData>) {
            if let Some(index) = self.position(download) {
                tracing::warn!(domain = %error.domain(), code = error.code(), "browser download failed");
                let mut entry = self.ivars().active.borrow_mut().remove(index);
                entry.path = None;
                entry.report(Some(error.localizedDescription().to_string()));
            }
        }
    }
);

impl Downloads {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let object = mtm.alloc().set_ivars(DownloadsState::default());
        unsafe { msg_send![super(object), init] }
    }
    fn adopt(&self, view: &WKWebView, download: &WKDownload, tx: Sender) {
        let url = unsafe { download.originalRequest() }.and_then(|request| request.URL());
        let name = url
            .filter(|url| {
                url.absoluteString()
                    .is_some_and(|url| allowed_navigation(&url.to_string()))
            })
            .and_then(|url| url.lastPathComponent())
            .map(|name| name.to_string())
            .filter(|name| !name.is_empty() && name != "/")
            .unwrap_or_else(|| "download".into());
        let page = unsafe { view.URL() }
            .and_then(|url| url.absoluteString())
            .map(|url| url.to_string());
        unsafe { download.setDelegate(Some(ProtocolObject::from_ref(self))) };
        self.ivars().active.borrow_mut().push(ActiveDownload {
            download: download.retain(),
            tx,
            name,
            page,
            path: None,
        });
    }
    fn position(&self, download: &WKDownload) -> Option<usize> {
        self.ivars()
            .active
            .borrow()
            .iter()
            .position(|entry| std::ptr::eq(&*entry.download, download))
    }
}

impl ActiveDownload {
    fn report(&self, error: Option<String>) {
        let _ = self.tx.try_send(NativeEvent::Download(Download {
            name: self.name.clone(),
            path: if error.is_none() {
                self.path.clone()
            } else {
                None
            },
            error,
        }));
    }
}

fn downloads_directory() -> Option<PathBuf> {
    let manager = objc2_foundation::NSFileManager::defaultManager();
    let directory = manager
        .URLsForDirectory_inDomains(
            objc2_foundation::NSSearchPathDirectory::DownloadsDirectory,
            objc2_foundation::NSSearchPathDomainMask::UserDomainMask,
        )
        .firstObject()?
        .path()?;
    let directory = PathBuf::from(directory.to_string());
    std::fs::create_dir_all(&directory).ok()?;
    Some(directory)
}

/// A new file in `directory`. Never overwrites: a taken name gets a numbered
/// suffix. The suggested name cannot leave the directory or hide the file.
fn destination(directory: &Path, suggested: &str) -> Option<PathBuf> {
    let name: String = Path::new(suggested)
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default()
        .chars()
        .map(|c| if c.is_control() { '_' } else { c })
        .collect();
    let name = name.trim_start_matches(['.', ' ']);
    let name = if name.is_empty() { "download" } else { name };
    let (stem, extension) = match name.rfind('.') {
        Some(index) if index > 0 => name.split_at(index),
        _ => (name, ""),
    };
    (0..1000)
        .map(|n| {
            directory.join(if n == 0 {
                name.to_owned()
            } else {
                format!("{stem} ({n}){extension}")
            })
        })
        .find(|path| std::fs::symlink_metadata(path).is_err())
}

/// Mark website files as downloaded, so Gatekeeper checks them as it would
/// after any browser, unless WebKit already recorded the quarantine.
fn quarantine(path: &Path, download: &WKDownload, page: Option<&str>) {
    let file = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
    let key = unsafe { objc2_foundation::NSURLQuarantinePropertiesKey };
    let mut existing = None;
    if unsafe { file.getResourceValue_forKey_error(&mut existing, key) }.is_err()
        || existing.is_some()
    {
        return;
    }
    let source = unsafe { download.originalRequest() }
        .and_then(|request| request.URL())
        .filter(|url| {
            url.absoluteString()
                .is_some_and(|url| allowed_navigation(&url.to_string()))
        });
    let page = page
        .filter(|page| allowed_navigation(page))
        .and_then(|page| NSURL::URLWithString(&NSString::from_str(page)));
    let mut keys = vec![
        objc2_foundation::ns_string!("LSQuarantineAgentName"),
        objc2_foundation::ns_string!("LSQuarantineType"),
    ];
    let mut values: Vec<Retained<AnyObject>> = vec![
        Retained::into_super(Retained::into_super(NSString::from_str("LUNA"))),
        Retained::into_super(Retained::into_super(NSString::from_str(
            "LSQuarantineTypeWebDownload",
        ))),
    ];
    if let Some(source) = source {
        keys.push(objc2_foundation::ns_string!("LSQuarantineDataURL"));
        values.push(Retained::into_super(Retained::into_super(source)));
    }
    if let Some(page) = page {
        keys.push(objc2_foundation::ns_string!("LSQuarantineOriginURL"));
        values.push(Retained::into_super(Retained::into_super(page)));
    }
    let properties = NSDictionary::<NSString, AnyObject>::from_retained_objects(&keys, &values);
    let properties: &AnyObject = &properties;
    if let Err(error) = unsafe { file.setResourceValue_forKey_error(Some(properties), key) } {
        tracing::warn!(code = error.code(), "could not quarantine browser download");
    }
}

struct ClipState {
    dragging: Cell<bool>,
    region: Cell<objc2_foundation::NSRect>,
    resize_inset: Cell<f64>,
}

// Clips native content to GPUI's current paint mask. During app drags the
// entire native subtree passes pointer events back to GPUI, without hiding it.
define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "LunaBrowserClipView"]
    #[ivars = ClipState]
    struct BrowserClipView;
    impl BrowserClipView {
        #[unsafe(method(hitTest:))]
        fn hit_test(&self, point: objc2_foundation::NSPoint) -> *mut NSView {
            let region = self.ivars().region.get();
            if self.ivars().dragging.get()
                || point.x < region.origin.x + self.ivars().resize_inset.get() || point.x >= region.origin.x + region.size.width
                || point.y < region.origin.y || point.y >= region.origin.y + region.size.height
            { std::ptr::null_mut() }
            else { unsafe { msg_send![super(self), hitTest: point] } }
        }
    }
);

pub(super) struct NativePage(Rc<RefCell<Host>>);

pub(super) struct Host {
    web: WebView,
    data: BrowserData,
    view: Retained<WKWebView>,
    observer: Retained<Observer>,
    clip: Retained<BrowserClipView>,
    clip_mask: Retained<AnyObject>,
    parent: Retained<NSView>,
    visible_bounds: Option<Bounds<Pixels>>,
    background_color: Option<Retained<NSColor>>,
    monitor: Option<Retained<AnyObject>>,
    shortcuts: Rc<RefCell<Vec<String>>>,
    bounds: Option<Bounds<Pixels>>,
    presentation: Presentation,
    tx: Sender,
    #[cfg(feature = "browser-fixture")]
    visibility_changes: Cell<u64>,
}

impl NativePage {
    pub fn new(window: &Window, data: &BrowserData, tx: Sender) -> Result<Self, String> {
        let mtm = MainThreadMarker::new().ok_or("Browser must be created on the main thread")?;
        let new_tab = tx.clone();
        let web = wry::WebViewBuilder::new()
            .with_webview_configuration(data.configuration(mtm))
            .with_visible(false)
            .with_focused(false)
            .with_incognito(true)
            .with_new_window_req_handler(move |url, _| {
                if allowed_navigation(&url) {
                    let _ = new_tab.try_send(NativeEvent::NewTab(url));
                }
                wry::NewWindowResponse::Deny
            })
            .build_as_child(window)
            .map_err(|e| e.to_string())?;
        let view = Retained::into_super(web.webview());
        window
            .enable_scene_overlay()
            .map_err(|error| error.to_string())?;
        let parent = unsafe { view.superview() }.ok_or("Browser parent is missing")?;
        let clip: Retained<BrowserClipView> = unsafe {
            let object = mtm.alloc().set_ivars(ClipState {
                dragging: Cell::new(false),
                region: Cell::new(objc2_foundation::NSRect::ZERO),
                resize_inset: Cell::new(0.0),
            });
            msg_send![super(object), initWithFrame: objc2_foundation::NSRect::ZERO]
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
        // Keep every native tab beneath the shared GPUI overlay plane.
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

        let observer = Observer::new(tx.clone(), data.downloads(mtm), mtm);
        unsafe {
            view.setNavigationDelegate(Some(ProtocolObject::from_ref(&*observer)));
            for key in OBSERVED {
                view.addObserver_forKeyPath_options_context(
                    &observer,
                    &NSString::from_str(key),
                    NSKeyValueObservingOptions::empty(),
                    std::ptr::null_mut(),
                );
            }
        }
        let shortcuts = Rc::new(RefCell::new(Vec::<String>::new()));
        let monitor_view = view.clone();
        let monitor_tx = tx.clone();
        let monitor_shortcuts = shortcuts.clone();
        let callback = block2::RcBlock::new(move |event: std::ptr::NonNull<NSEvent>| {
            let e = unsafe { event.as_ref() };
            if monitor_view.isHidden()
                || !has_focus(&monitor_view)
                || e.window(mtm) != monitor_view.window()
            {
                return event.as_ptr();
            }
            let mods = e.modifierFlags();
            let key = e
                .charactersIgnoringModifiers()
                .map(|s| s.to_string().to_lowercase())
                .unwrap_or_default();
            let mut combo = String::new();
            if mods.contains(NSEventModifierFlags::Command) {
                combo.push_str("cmd-");
            }
            if mods.contains(NSEventModifierFlags::Control) {
                combo.push_str("ctrl-");
            }
            if mods.contains(NSEventModifierFlags::Option) {
                combo.push_str("alt-");
            }
            if mods.contains(NSEventModifierFlags::Shift) {
                combo.push_str("shift-");
            }
            combo.push_str(&key);
            let Ok(keystroke) = gpui::Keystroke::parse(&combo) else {
                return event.as_ptr();
            };
            let browser_key = matches!(
                combo.as_str(),
                "cmd-l" | "cmd-t" | "cmd-w" | "cmd-[" | "cmd-]" | "cmd-shift-r" | "cmd-k" | "cmd-,"
            );
            let app_key = monitor_shortcuts
                .borrow()
                .iter()
                .any(|s| gpui::Keystroke::parse(s).is_ok_and(|s| s == keystroke));
            if browser_key || app_key {
                if monitor_tx.try_send(NativeEvent::Key(keystroke)).is_ok() {
                    std::ptr::null_mut()
                } else {
                    event.as_ptr()
                }
            } else {
                event.as_ptr()
            }
        });
        let monitor = unsafe {
            NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &callback)
        };
        Ok(Self(Rc::new(RefCell::new(Host {
            data: data.clone(),
            web,
            view,
            observer,
            clip,
            clip_mask,
            parent,
            visible_bounds: None,
            background_color: None,
            monitor,
            shortcuts,
            bounds: None,
            presentation: Presentation::Hidden,
            #[cfg(feature = "browser-fixture")]
            visibility_changes: Cell::new(0),
            tx,
        }))))
    }
    pub fn handle(&self) -> Rc<RefCell<Host>> {
        self.0.clone()
    }
    pub fn focus_chrome(&self) {
        let _ = self.0.borrow().web.focus_parent();
    }
    pub fn set_shortcuts(&self, shortcuts: Vec<String>) {
        *self.0.borrow().shortcuts.borrow_mut() = shortcuts;
    }
    pub fn present(&mut self, presentation: Presentation) {
        self.0.borrow_mut().present(presentation);
    }
    pub fn load(&self, url: &str) -> Result<(), String> {
        let host = self.0.borrow();
        host.data.register_preview(url);
        host.observer.ivars().error.borrow_mut().take();
        *host.observer.ivars().requested_url.borrow_mut() = Some(url.into());
        host.web.load_url(url).map_err(|e| e.to_string())
    }
    pub fn reload(&self) {
        let host = self.0.borrow();
        host.observer.ivars().error.borrow_mut().take();
        // Retrying the requested URL also works after a failed provisional load.
        unsafe {
            host.view.reload();
        }
        host.observer.changed();
    }
    pub fn history(&self, forward: bool) {
        let host = self.0.borrow();
        unsafe {
            if forward {
                host.view.goForward();
            } else {
                host.view.goBack();
            }
        }
    }
    pub fn state(&self) -> PageState {
        let host = self.0.borrow();
        host.observer.ivars().pending.set(false);
        unsafe {
            PageState {
                url: host
                    .observer
                    .ivars()
                    .requested_url
                    .borrow()
                    .clone()
                    .or_else(|| {
                        host.view
                            .URL()
                            .and_then(|u| u.absoluteString())
                            .map(|u| u.to_string())
                    }),
                title: host.view.title().map(|s| s.to_string()).unwrap_or_default(),
                loading: host.view.isLoading(),
                can_back: host.view.canGoBack(),
                can_forward: host.view.canGoForward(),
                error: host.observer.ivars().error.borrow().clone(),
            }
        }
    }
    pub fn discover_favicon(&self, page: String) {
        let host = self.0.borrow();
        let tx = host.tx.clone();
        // Our navigation delegate owns readiness, so Wry's private pending-
        // script queue is intentionally bypassed. This runs only after finish.
        let completion = block2::RcBlock::new(move |value: *mut AnyObject, error: *mut NSError| {
            if !error.is_null() {
                return;
            }
            if let Some(url) =
                unsafe { value.as_ref() }.and_then(|value| value.downcast_ref::<NSString>())
            {
                let _ = tx.try_send(NativeEvent::Favicon {
                    page: page.clone(),
                    url: url.to_string(),
                });
            }
        });
        unsafe {
            host.view.evaluateJavaScript_completionHandler(
                &NSString::from_str("(() => { const link = document.querySelector('link[rel~=icon]'); return link ? link.href : new URL('/favicon.ico', location.href).href; })()"),
                Some(&completion),
            );
        }
    }
}

fn has_focus(view: &NSView) -> bool {
    view.window()
        .and_then(|w| w.firstResponder())
        .and_then(|r| r.downcast::<NSView>().ok())
        .is_some_and(|v| v.isDescendantOf(view))
}

impl Host {
    pub fn sync(
        &mut self,
        bounds: Bounds<Pixels>,
        mask: Bounds<Pixels>,
        dragging: bool,
        resize_inset: Pixels,
    ) {
        // WebKit may fill newly exposed tiles a frame after a viewport change.
        // Match its page background beneath those tiles instead of exposing
        // the application's dark window background at the resize edge.
        unsafe {
            let color = self.view.underPageBackgroundColor();
            let unchanged = self
                .background_color
                .as_ref()
                .is_some_and(|previous| msg_send![&**previous, isEqual: &*color]);
            if !unchanged {
                // WebKit's native backing otherwise uses controlBackgroundColor,
                // independently of the CSS page background. Exposed resize tiles
                // must use the page color while the remote content catches up.
                let _: () = msg_send![&*self.view, _setBackgroundColor: &*color];
                let cg_color = color.CGColor();
                let layer: *mut AnyObject = msg_send![&*self.clip, layer];
                let _: () = msg_send![layer, setBackgroundColor: &*cg_color];
                self.background_color = Some(color);
            }
        }
        let visible = bounds.intersect(&mask);
        self.clip.ivars().dragging.set(dragging);
        self.clip
            .ivars()
            .resize_inset
            .set(f64::from(f32::from(resize_inset)));
        if self.bounds != Some(bounds) || self.visible_bounds != Some(visible) {
            self.bounds = Some(bounds);
            self.visible_bounds = Some(visible);
            let x = f64::from(f32::from(visible.origin.x));
            let y = f64::from(f32::from(visible.origin.y));
            let width = f64::from(f32::from(visible.size.width)).max(0.);
            let height = f64::from(f32::from(visible.size.height)).max(0.);
            let y = if self.parent.isFlipped() {
                y
            } else {
                self.parent.bounds().size.height - y - height
            };
            let actions_disabled: bool = unsafe {
                let previous = msg_send![class!(CATransaction), disableActions];
                let _: () = msg_send![class!(CATransaction), setDisableActions: true];
                previous
            };
            // Keep the host stationary. Moving it while WebKit updates its
            // remote layer tree can combine the old origin with the new size.
            self.clip.setFrame(self.parent.bounds());
            let region = objc2_foundation::NSRect::new(
                objc2_foundation::NSPoint::new(x, y),
                objc2_foundation::NSSize::new(width, height),
            );
            self.clip.ivars().region.set(region);
            unsafe {
                let _: () = msg_send![&*self.clip_mask, setFrame: region];
            }
            // Wry's set_bounds rounds logical origins and sizes to whole points. GPUI
            // uses fractional points, so that leaves uncovered background
            // strips between the WKWebView and its clip after a resize.
            let web_height = f64::from(f32::from(bounds.size.height)).max(0.);
            let web_y = f64::from(f32::from(bounds.origin.y));
            let web_y = if self.parent.isFlipped() {
                web_y
            } else {
                self.parent.bounds().size.height - web_y - web_height
            };
            self.view.setFrame(objc2_foundation::NSRect::new(
                objc2_foundation::NSPoint::new(f64::from(f32::from(bounds.origin.x)), web_y),
                objc2_foundation::NSSize::new(
                    f64::from(f32::from(bounds.size.width)).max(0.),
                    web_height,
                ),
            ));
            unsafe {
                // Leave the implicit transaction open for the matching Metal
                // presentation. Committing here would expose native geometry early.
                let _: () = msg_send![class!(CATransaction), setDisableActions: actions_disabled];
            }
            #[cfg(feature = "browser-fixture")]
            unsafe {
                let layer: *mut AnyObject = msg_send![&*self.clip, layer];
                let frame: objc2_foundation::NSRect = msg_send![layer, frame];
                let presentation: *mut AnyObject = msg_send![layer, presentationLayer];
                let presented: objc2_foundation::NSRect = if presentation.is_null() {
                    frame
                } else {
                    msg_send![presentation, frame]
                };
                eprintln!(
                    "Browser geometry: bounds={bounds:?} mask={mask:?} clip={:?} layer={frame:?} presented={presented:?} web={:?} background={:?}",
                    self.clip.frame(),
                    self.view.frame(),
                    self.view.underPageBackgroundColor()
                );
            }
        }
        self.update_visibility();
    }
    fn update_visibility(&self) {
        let visible = self.presentation != Presentation::Hidden
            && self.observer.ivars().error.borrow().is_none()
            && self
                .visible_bounds
                .is_some_and(|b| f32::from(b.size.width) > 0. && f32::from(b.size.height) > 0.);
        if (!visible || self.presentation == Presentation::Passthrough) && has_focus(&self.view) {
            let _ = self.web.focus_parent();
        }
        if self.view.isHidden() == visible {
            #[cfg(feature = "browser-fixture")]
            self.visibility_changes
                .set(self.visibility_changes.get() + 1);
            let _ = self.web.set_visible(visible);
        }
        self.clip.setHidden(!visible);
    }
    fn present(&mut self, presentation: Presentation) {
        self.presentation = presentation;
        self.clip
            .ivars()
            .dragging
            .set(presentation == Presentation::Passthrough);
        self.update_visibility();
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
            for key in OBSERVED {
                self.view
                    .removeObserver_forKeyPath(&self.observer, &NSString::from_str(key));
            }
            self.view.setNavigationDelegate(None);
            self.view.stopLoading();
        }
        self.view.removeFromSuperview();
        self.clip.removeFromSuperview();
    }
}

#[cfg(feature = "browser-fixture")]
impl NativePage {
    pub fn fixture_move_cursor(&self, x: f64, y: f64) {
        #[link(name = "CoreGraphics", kind = "framework")]
        unsafe extern "C" {
            fn CGWarpMouseCursorPosition(point: objc2_foundation::NSPoint) -> i32;
        }
        let host = self.0.borrow();
        let window = host.view.window().unwrap();
        let height = host.parent.clone().bounds().size.height;
        let p = window.convertPointToScreen(objc2_foundation::NSPoint::new(x, height - y));
        let screen = objc2_app_kit::NSScreen::mainScreen(host.view.mtm()).unwrap();
        unsafe {
            CGWarpMouseCursorPosition(objc2_foundation::NSPoint::new(
                p.x,
                screen.frame().size.height - p.y,
            ));
        }
    }
    pub fn fixture_backdrop_layers(&self) -> String {
        unsafe fn describe(layer: *mut AnyObject, depth: usize, out: &mut String) {
            if layer.is_null() || depth > 8 {
                return;
            }
            unsafe {
                let desc: Retained<NSString> = msg_send![layer, description];
                out.push_str(&format!("{}{}\n", " ".repeat(depth), desc));
                let layers: *mut AnyObject = msg_send![layer, sublayers];
                if !layers.is_null() {
                    let count: usize = msg_send![layers, count];
                    for i in 0..count.min(30) {
                        let child: *mut AnyObject = msg_send![layers, objectAtIndex:i];
                        describe(child, depth + 1, out);
                    }
                }
            }
        }
        let host = self.0.borrow();
        let mut out = String::new();
        for view in host.parent.subviews() {
            if view.class().name() == c"GPUIBackdropView" {
                unsafe {
                    let layer: *mut AnyObject = msg_send![&*view, layer];
                    describe(layer, 0, &mut out);
                }
            }
        }
        out
    }
    pub fn fixture_page_hit(&self, x: f64, y: f64) -> bool {
        let host = self.0.borrow();
        let point = objc2_foundation::NSPoint::new(x, host.parent.bounds().size.height - y);
        host.parent
            .hitTest(point)
            .is_some_and(|hit| hit.isDescendantOf(&host.clip))
    }
    pub fn fixture_backdrops(&self) -> Vec<(f64, f64, f64, f64)> {
        let host = self.0.borrow();
        host.parent
            .subviews()
            .iter()
            .filter(|view| view.class().name() == c"GPUIBackdropView" && !view.isHidden())
            .map(|view| {
                let r = view.frame();
                (
                    r.origin.x,
                    host.parent.bounds().size.height - r.origin.y - r.size.height,
                    r.size.width,
                    r.size.height,
                )
            })
            .collect()
    }
    pub fn fixture_geometry(&self) -> (f32, f32, f32) {
        let host = self.0.borrow();
        (
            host.bounds.unwrap().size.width.into(),
            host.view.frame().size.width as f32,
            host.visible_bounds.unwrap().size.width.into(),
        )
    }
    pub fn fixture_origin(&self) -> (f32, f32) {
        let b = self.0.borrow().bounds.unwrap();
        (b.origin.x.into(), b.origin.y.into())
    }
    pub fn fixture_overlay_visible(&self) -> bool {
        let host = self.0.borrow();
        host.parent
            .clone()
            .subviews()
            .iter()
            .any(|v| v.class().name() == c"GPUIOverlayView" && !v.isHidden())
    }
    pub fn fixture_visibility_changes(&self) -> u64 {
        let host = self.0.borrow();
        host.visibility_changes.get()
    }
    pub fn fixture_focus(&self) {
        let _ = self.0.borrow().web.focus();
    }
    pub fn fixture_focused(&self) -> bool {
        has_focus(&self.0.borrow().view)
    }
    pub fn fixture_overlay_at(&self, x: f64, y: f64) -> bool {
        let host = self.0.borrow();
        let parent = host.parent.clone();
        let point = objc2_foundation::NSPoint::new(
            x,
            if parent.isFlipped() {
                y
            } else {
                parent.bounds().size.height - y
            },
        );
        parent
            .hitTest(point)
            .is_some_and(|hit| hit.class().name() == c"GPUIOverlayView")
    }
    pub fn fixture_click(&self, x: f64, y: f64) {
        self.fixture_move_cursor(x, y);
        let host = self.0.borrow();
        let parent = host.parent.clone();
        let window = host.view.window().unwrap();
        let point = objc2_foundation::NSPoint::new(x, parent.bounds().size.height - y);
        let app = objc2_app_kit::NSApplication::sharedApplication(host.view.mtm());
        for kind in [
            objc2_app_kit::NSEventType::LeftMouseDown,
            objc2_app_kit::NSEventType::LeftMouseUp,
        ] {
            let event = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(kind, point, NSEventModifierFlags::empty(), 0., window.windowNumber(), None, 0, 1, 1.).unwrap();
            app.postEvent_atStart(&event, false);
        }
    }
    pub fn fixture_visible(&self) -> bool {
        !self.0.borrow().view.isHidden()
    }
    pub fn fixture_eval(&self, script: &str) {
        unsafe {
            self.0
                .borrow()
                .view
                .evaluateJavaScript_completionHandler(&NSString::from_str(script), None);
        }
    }
}
