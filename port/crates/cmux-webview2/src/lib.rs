//! Windows WebView2 hosting for the cmux browser surface (M5.1).
//!
//! One [`BrowserHost`] fronts one browser surface: it starts environment
//! and controller creation asynchronously (completions dispatch on the egui
//! frame thread's own message pump, so no modal wait ever blocks the frame
//! loop), applies pane bounds and visibility per frame via
//! [`BrowserHost::tick`], and exposes navigation verbs plus a polled
//! [`BrowserSnapshot`] (source/title/back-forward state).
//!
//! Threading contract: every method must be called on the thread that owns
//! the parent window's message pump (the egui frame thread). IPC handlers
//! already run there, so this crate has no marshaling.
//!
//! Units: [`TickParams::bounds`] is in egui points, which equal DIPs; the
//! host multiplies by `dpi_scale` through `ICoreWebView2Controller3::
//! SetRasterizationScale` so rendering lands on physical pixels (per the
//! WebView2 reference: `Bounds` are relative to the parent HWND and
//! `RasterizationScale` scales the rendered content; runtime-verified in
//! the M5.1 smoke).

use std::collections::{BTreeSet, VecDeque};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use webview2_com::FocusChangedEventHandler;
use webview2_com::Microsoft::Web::WebView2::Win32::{
    CreateCoreWebView2EnvironmentWithOptions, ICoreWebView2,
    ICoreWebView2AcceleratorKeyPressedEventArgs, ICoreWebView2Controller, ICoreWebView2Controller2,
    ICoreWebView2Controller3, ICoreWebView2ControllerOptions3,
    ICoreWebView2CreateCoreWebView2ControllerCompletedHandler, ICoreWebView2Environment,
    ICoreWebView2Environment10, ICoreWebView2EnvironmentOptions, COREWEBVIEW2_COLOR,
    COREWEBVIEW2_KEY_EVENT_KIND, COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN,
    COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN, COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC,
};
use webview2_com::{
    AcceleratorKeyPressedEventHandler, CallDevToolsProtocolMethodCompletedHandler,
    CoreWebView2EnvironmentOptions, CreateCoreWebView2ControllerCompletedHandler,
    CreateCoreWebView2EnvironmentCompletedHandler,
};
use windows::core::{IUnknown, Interface, BOOL, HSTRING, PCWSTR};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL};

/// What a host needs at start time plus its per-frame inputs.
#[derive(Clone, Debug)]
pub struct BrowserSpec {
    /// The pane id this host serves. Router events (accelerator chords,
    /// native GotFocus) carry it so the shell can attribute them.
    pub surface_id: String,
    /// Per-surface user data folder (port decision: one folder per surface,
    /// `%LOCALAPPDATA%\cmux\webview2\<surface-id>`). The folder is left on
    /// disk after close — documented residual in ADR-0006.
    pub user_data_dir: PathBuf,
    /// First URL navigated once the controller is wired.
    pub start_url: String,
    /// What pages with no defined background paint below their content,
    /// `0xRRGGBB` opaque: the shell's resolved viewport fill (the design's
    /// `#0c0e14` token), applied as the WebView2 default background through
    /// the controller options at creation (plus a wire-time put backstop).
    /// Live-verified on Evergreen 156.0.4314.42: a transparent-background
    /// document paints the token; the bare `about:blank` leaf keeps
    /// Chromium's theme gray regardless — the blank document is
    /// special-cased upstream, so this only fixes real pages.
    pub default_background: u32,
}

impl BrowserSpec {
    pub fn new(
        surface_id: impl Into<String>,
        user_data_dir: PathBuf,
        start_url: impl Into<String>,
        default_background: u32,
    ) -> Self {
        Self {
            surface_id: surface_id.into(),
            user_data_dir,
            start_url: start_url.into(),
            default_background,
        }
    }
}

/// Everything the frame loop knows about one browser surface.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BrowserSnapshot {
    /// Current document URL (`ICoreWebView2.Source`); the desired start
    /// URL before the first navigation lands.
    pub source: String,
    /// Current document title (empty before first load).
    pub title: String,
    pub can_go_back: bool,
    pub can_go_forward: bool,
}

/// Lifecycle phase of one host. Mirrored into IPC replies as strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrowserPhase {
    /// Environment/controller creation requested, not finished.
    Creating,
    /// Fully wired: navigable, bounds apply, snapshot is live.
    Ready,
    /// Permanent failure for this surface (runtime missing, creation
    /// error). The pane stays until closed.
    Failed,
    /// `Close` was requested: the controller is released.
    Closed,
}

impl BrowserPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            BrowserPhase::Creating => "creating",
            BrowserPhase::Ready => "ready",
            BrowserPhase::Failed => "failed",
            BrowserPhase::Closed => "closed",
        }
    }
}

/// A tiny anchor-free rect in points (avoids an egui dependency here).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    /// Left edge in points (egui point space == DIPs).
    pub left: f64,
    /// Top edge in points.
    pub top: f64,
    /// Right edge in points (exclusive).
    pub right: f64,
    /// Bottom edge in points (exclusive).
    pub bottom: f64,
}

impl Rect {
    pub fn new(left: f64, top: f64, right: f64, bottom: f64) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    pub fn width(&self) -> f64 {
        (self.right - self.left).max(0.0)
    }

    pub fn height(&self) -> f64 {
        (self.bottom - self.top).max(0.0)
    }
}

/// Per-frame inputs computed by the shell from layout/user state.
#[derive(Clone, Copy, Debug)]
pub struct TickParams {
    /// Pane rect in points inside the window's client area (`None` while
    /// the surface has no visible leaf this frame).
    pub bounds: Option<Rect>,
    /// DPI scale of the parent window (96-DPI base == 1.0).
    pub dpi_scale: f64,
    /// Whether the surface's leaf is in the active tree this frame.
    pub visible: bool,
    /// Screen-space top-left of the parent window this frame; a change
    /// triggers `NotifyParentWindowPositionChanged` (accessibility).
    pub window_pos: Option<(i32, i32)>,
    /// Whether the browser surface is the model-focused one.
    pub focused: bool,
}

/// Conversion used by `tick` and unit-tested on its own.
pub(crate) fn rect_to_win(rect: Rect, scale: f64) -> RECT {
    let to_i32 = |v: f64| -> i32 {
        let scaled = v * scale;
        if scaled >= 0.0 {
            (scaled + 0.5) as i32
        } else {
            (scaled - 0.5) as i32
        }
    };
    RECT {
        left: to_i32(rect.left),
        top: to_i32(rect.top),
        right: to_i32(rect.left + rect.width()),
        bottom: to_i32(rect.top + rect.height()),
    }
}

/// Parked controller completion: `(Err(...), controller?)`. An `Rc` — the
/// completion dispatches onto the owning thread's pump, so the slot never
/// crosses threads (the closure type carries no `Send` bound either).
type ControllerSlot = Rc<
    Mutex<
        Option<(
            Option<windows::core::Error>,
            Option<ICoreWebView2Controller>,
        )>,
    >,
>;

/// What stage the host is in.
enum HostPhase {
    EnvRequested,
    /// Controller creation kicked; completion parked in `controller_slot`.
    ControllerRequested,
    Wired {
        controller: ICoreWebView2Controller,
        webview: ICoreWebView2,
    },
    Failed(String),
    Closed,
}

struct HostState {
    phase: HostPhase,
    parent_hwnd: HWND,
    spec: BrowserSpec,
    accel_router: Option<AccelRouter>,
    /// URLs queued while not yet wired (the start URL, and any navigation
    /// requested during creation), in arrival order.
    pending_urls: VecDeque<String>,
    controller_slot: ControllerSlot,
    applied_bounds: Option<(Rect, f64)>,
    applied_visible: Option<bool>,
    applied_focused: bool,
    last_window_pos: Option<(i32, i32)>,
    snapshot: BrowserSnapshot,
    last_poll: Instant,
    /// Rate limiter for slow-COM-tick diagnostics (`SLOW_TICK_LOG`).
    last_slow_log: Option<Instant>,
}

/// History verb fan for [`BrowserHost::history`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryVerb {
    Back,
    Forward,
    Reload,
}

/// A wake handle the completion handlers call when an async result
/// arrives while the frame loop is idle, so the parked reply drains on a
/// prompt frame instead of waiting for the next display-driven one.
pub type FrameWakeFn = Arc<dyn Fn() + Send + Sync>;

/// One finished DevTools protocol call. The caller-minted `id` echoes the
/// [`BrowserHost::call_cdp`] argument; `outcome` carries the method's raw
/// result JSON, or the protocol error text. Completions arrive via the
/// frame thread's message pump and drain through
/// [`BrowserHost::take_cdp_completions`] on the next frame.
#[derive(Clone, Debug)]
pub struct CdpCompletion {
    pub id: u64,
    pub outcome: Result<String, String>,
    /// Wall time from invoke to completion, measured host-side (the
    /// measurement probe behind ADR-0006's non-blocking budget; 0 when the
    /// call was not tracked, e.g. after a synchronous re-registration).
    pub round_trip: Duration,
}

/// Per-call wall-clock starts for the completion latency probe. Purely
/// bookkeeping: unknown ids measure as zero and are logged untracked.
#[derive(Default)]
struct CdpTracker {
    starts: std::collections::HashMap<u64, Instant>,
}

impl CdpTracker {
    fn mark(&mut self, id: u64) {
        self.starts.insert(id, Instant::now());
    }

    /// Takes a started call's elapsed time, removing the entry (each call
    /// completes at most once per protocol contract).
    fn take(&mut self, id: u64) -> Option<Duration> {
        self.starts.remove(&id).map(|started| started.elapsed())
    }
}

/// Handle for one browser surface. Cheap to clone; lives in the pane's
/// browser body; call purely from the frame thread.
#[derive(Clone)]
pub struct BrowserHost {
    state: Rc<Mutex<Option<HostState>>>,
    seq: Arc<AtomicU64>,
    /// Finished DevTools completions the handlers push and the shell
    /// drains per frame. Host-level (not inside `HostState`) so the
    /// completion closure can capture cheap clones without contending the
    /// state lock.
    cdp_outbox: Rc<Mutex<Vec<CdpCompletion>>>,
    cdp_tracker: Rc<Mutex<CdpTracker>>,
    wake: FrameWakeFn,
}

impl BrowserHost {
    /// Starts environment creation. Must be called on the frame thread.
    /// `wake` fires (from any of this host's completion handlers) when an
    /// async result lands while the frame loop is idle.
    pub fn start(
        spec: BrowserSpec,
        parent_hwnd: isize,
        accel: Option<&AccelRouter>,
        wake: FrameWakeFn,
    ) -> Self {
        // COM apartment: the egui thread may already be STA (WGL does not
        // initialize COM). Outcomes that are explicitly fine to continue
        // past: S_FALSE (already initialized as STA) and RPC_E_CHANGED_MODE
        // (a library chose MTA; WebView2 objects are agile). Any genuinely
        // broken COM surfaces later in environment creation anyway.
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }

        let host = Self {
            state: Rc::new(Mutex::new(Some(HostState {
                phase: HostPhase::EnvRequested,
                parent_hwnd: HWND(parent_hwnd as *mut core::ffi::c_void),
                spec: spec.clone(),
                accel_router: accel.cloned(),
                pending_urls: VecDeque::from([spec.start_url.clone()]),
                controller_slot: Rc::new(Mutex::new(None)),
                applied_bounds: None,
                applied_visible: None,
                applied_focused: false,
                last_window_pos: None,
                snapshot: BrowserSnapshot {
                    source: spec.start_url.clone(),
                    ..BrowserSnapshot::default()
                },
                last_poll: Instant::now(),
                last_slow_log: None,
            }))),
            seq: Arc::new(AtomicU64::new(0)),
            cdp_outbox: Rc::new(Mutex::new(Vec::new())),
            cdp_tracker: Rc::new(Mutex::new(CdpTracker::default())),
            wake,
        };

        // Default options: Evergreen channel search (recorded in ADR-0006:
        // per-surface environment → one browser process tree per surface).
        let options: ICoreWebView2EnvironmentOptions =
            CoreWebView2EnvironmentOptions::default().into();

        let slot = Rc::clone(&host.state);
        let handler = CreateCoreWebView2EnvironmentCompletedHandler::create(Box::new(
            move |result, environment| {
                let mut guard = match slot.lock() {
                    Ok(g) => g,
                    Err(_) => return Ok(()),
                };
                let Some(state) = guard.as_mut() else {
                    return Ok(());
                };
                match (result, environment) {
                    (Ok(()), Some(environment)) => {
                        if let Err(error) = request_controller(state, &environment) {
                            state.phase = HostPhase::Failed(error);
                        }
                    }
                    (Err(error), _) => state.phase = HostPhase::Failed(error.to_string()),
                    (Ok(()), None) => {
                        state.phase =
                            HostPhase::Failed("WebView2 returned no environment".to_string())
                    }
                }
                Ok(())
            },
        ));

        let (udf, udf_buf) = pcw(&spec.user_data_dir.to_string_lossy());
        let result = unsafe {
            CreateCoreWebView2EnvironmentWithOptions(
                PCWSTR::null(), // browser executable folder: Evergreen default
                udf,
                &options,
                &handler,
            )
        };
        drop(udf_buf);
        if let Err(error) = result {
            if let Ok(mut guard) = host.state.lock() {
                if let Some(state) = guard.as_mut() {
                    state.phase = HostPhase::Failed(error.to_string());
                }
            }
        }
        host
    }

    /// Advances creation stages and applies this frame's bounds/visibility.
    /// Must be called on the frame thread; returns the polled state.
    pub fn tick(&self, params: &TickParams) -> (BrowserPhase, BrowserSnapshot) {
        let Ok(mut guard) = self.state.lock() else {
            return (BrowserPhase::Failed, BrowserSnapshot::default());
        };
        self.seq.fetch_add(1, Ordering::Relaxed);
        let Some(state) = guard.as_mut() else {
            return (BrowserPhase::Closed, BrowserSnapshot::default());
        };

        // Promote a parked controller completion, then wire once.
        if matches!(state.phase, HostPhase::ControllerRequested) {
            if let Some(parked) = state
                .controller_slot
                .lock()
                .ok()
                .and_then(|mut slot| slot.take())
            {
                match (parked.0, parked.1) {
                    (None, Some(controller)) => match wire_controller(state, &controller) {
                        Ok(webview) => {
                            state.phase = HostPhase::Wired {
                                controller,
                                webview,
                            };
                        }
                        Err(message) => state.phase = HostPhase::Failed(message),
                    },
                    (Some(error), _) => state.phase = HostPhase::Failed(error.to_string()),
                    (None, None) => {
                        state.phase =
                            HostPhase::Failed("WebView2 returned no controller".to_string())
                    }
                }
            }
        }

        finish_tick(state, params)
    }

    /// Snapshot without advancing anything.
    pub fn peek(&self) -> (BrowserPhase, BrowserSnapshot) {
        let Ok(guard) = self.state.lock() else {
            return (BrowserPhase::Failed, BrowserSnapshot::default());
        };
        let Some(state) = guard.as_ref() else {
            return (BrowserPhase::Closed, BrowserSnapshot::default());
        };
        (phase_of(state), state.snapshot.clone())
    }

    /// Failure description, when the host has failed permanently.
    pub fn failure(&self) -> Option<String> {
        let guard = self.state.lock().ok()?;
        match &guard.as_ref()?.phase {
            HostPhase::Failed(text) => Some(text.clone()),
            _ => None,
        }
    }

    /// Navigates immediately when wired, otherwise queues until ready.
    pub fn navigate(&self, url: String) -> Result<(), String> {
        let Ok(mut guard) = self.state.lock() else {
            return Err("browser host unavailable".to_string());
        };
        let Some(state) = guard.as_mut() else {
            return Err("browser host closed".to_string());
        };
        match &state.phase {
            HostPhase::Wired { webview, .. } => unsafe {
                webview
                    .Navigate(&HSTRING::from(url.as_str()))
                    .map_err(|e| e.to_string())
            },
            HostPhase::Failed(message) => Err(format!("browser host failed: {message}")),
            HostPhase::Closed => Err("browser host closed".to_string()),
            HostPhase::EnvRequested | HostPhase::ControllerRequested => {
                if state.pending_urls.len() >= PENDING_URL_LIMIT {
                    return Err("too many navigations queued while loading".to_string());
                }
                state.pending_urls.push_back(url);
                Ok(())
            }
        }
    }

    pub fn go_back(&self) {
        self.with_webview(|webview| unsafe {
            let _ = webview.GoBack();
        });
    }

    pub fn go_forward(&self) {
        self.with_webview(|webview| unsafe {
            let _ = webview.GoForward();
        });
    }

    pub fn reload(&self) {
        self.with_webview(|webview| unsafe {
            let _ = webview.Reload();
        });
    }

    /// History verb fan: back/forward are no-ops at history end;
    /// reload re-navigates the current document.
    #[track_caller]
    pub fn history(&self, verb: HistoryVerb) {
        self.with_webview(|webview| unsafe {
            match verb {
                HistoryVerb::Back => {
                    let _ = webview.GoBack();
                }
                HistoryVerb::Forward => {
                    let _ = webview.GoForward();
                }
                HistoryVerb::Reload => {
                    let _ = webview.Reload();
                }
            }
        });
    }

    /// Issues one DevTools protocol method against the wired webview.
    /// `id` is caller-minted and echoed in the completion; `method` is the
    /// protocol name (e.g. `Runtime.evaluate`), `params_json` the CDP
    /// parameters object as JSON. The invoke itself is asynchronous under
    /// the protocol contract: it returns once the call is handed to the
    /// browser process, and the completion dispatches on the frame
    /// thread's message pump (parked into the host's outbox, consumed
    /// through [`BrowserHost::take_cdp_completions`]). Never blocks the
    /// frame loop; a synchronous transport failure reports `Err` with the
    /// COM error text. Errors map at the shell, which owns the verbs.
    pub fn call_cdp(&self, id: u64, method: &str, params_json: &str) -> Result<(), String> {
        let webview = {
            let guard = self
                .state
                .lock()
                .map_err(|_| "browser host unavailable".to_string())?;
            let Some(state) = guard.as_ref() else {
                return Err("browser host closed".to_string());
            };
            match &state.phase {
                HostPhase::Wired { webview, .. } => webview.clone(),
                HostPhase::Failed(message) => {
                    return Err(format!("browser host failed: {message}"))
                }
                HostPhase::Closed => return Err("browser host closed".to_string()),
                HostPhase::EnvRequested | HostPhase::ControllerRequested => {
                    return Err(
                        "browser surface is still initializing; call after it is ready".to_string(),
                    )
                }
            }
        };

        // The invoke may still register the call before it fails at a
        // later stage, so the tracker records the start here either way
        // and the synchronous-failure path removes it again.
        if let Ok(mut tracker) = self.cdp_tracker.lock() {
            tracker.mark(id);
        }

        let outbox = Rc::clone(&self.cdp_outbox);
        let tracker = Rc::clone(&self.cdp_tracker);
        let wake = Arc::clone(&self.wake);
        // Completed-callback closure shape (`CompletedClosure<HRESULT,
        // PCWSTR>`): the error code already converts to `Result<()>`, the
        // JSON payload arrives as an owned `String`.
        let handler = CallDevToolsProtocolMethodCompletedHandler::create(Box::new(
            move |error_code: windows::core::Result<()>, return_json: String| {
                let round_trip = tracker
                    .lock()
                    .ok()
                    .and_then(|mut tracker| tracker.take(id))
                    .unwrap_or_default();
                if round_trip > CDP_SLOW_LOG {
                    eprintln!("cmux-webview2: slow cdp completion {round_trip:?} (call {id})");
                }
                let outcome = match error_code {
                    Ok(()) => Ok(return_json),
                    Err(error) => Err(error.to_string()),
                };
                let pushed = outbox.lock().ok().is_some_and(|mut sink| {
                    sink.push(CdpCompletion {
                        id,
                        outcome,
                        round_trip,
                    });
                    true
                });
                if pushed {
                    // Prompt frame so a parked reply drains even from an
                    // idle pump (the handler runs on the frame thread).
                    wake();
                }
                Ok(())
            },
        ));

        let (method_ptr, method_buf) = pcw(method);
        let (params_ptr, params_buf) = pcw(params_json);
        let result =
            unsafe { webview.CallDevToolsProtocolMethod(method_ptr, params_ptr, &handler) };
        drop(method_buf);
        drop(params_buf);
        if result.is_err() {
            // Nothing will complete: release the tracked start to keep the
            // probe's bookkeeping leak-free.
            if let Ok(mut tracker) = self.cdp_tracker.lock() {
                let _ = tracker.take(id);
            }
        }
        result.map_err(|error| error.to_string())
    }

    /// Drains the completion outbox (arrival order). Called per frame by
    /// the shell right after each host tick.
    pub fn take_cdp_completions(&self) -> Vec<CdpCompletion> {
        self.cdp_outbox
            .lock()
            .map(|mut sink| std::mem::take(&mut *sink))
            .unwrap_or_default()
    }

    /// Closes the surface: controller `Close` + release all refs. `Close`
    /// is synchronous and never fires `beforeunload`; the user data folder
    /// is intentionally kept on disk (documented residual).
    pub fn close(&self) {
        if let Ok(mut guard) = self.state.lock() {
            if let Some(state) = guard.as_mut() {
                if let HostPhase::Wired { controller, .. } = &state.phase {
                    unsafe {
                        let _ = controller.Close();
                    }
                }
                state.phase = HostPhase::Closed;
            }
            *guard = None;
        }
    }

    /// The spec's user data folder (for diagnostics/logging).
    pub fn user_data_dir(&self) -> PathBuf {
        self.state
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|s| s.spec.user_data_dir.clone()))
            .unwrap_or_default()
    }

    fn with_webview(&self, f: impl FnOnce(&ICoreWebView2)) {
        if let Ok(guard) = self.state.lock() {
            if let Some(state) = guard.as_ref() {
                if let HostPhase::Wired { webview, .. } = &state.phase {
                    f(webview);
                }
            }
        }
    }
}

/// Cap on navigations queued during creation (protects against runaway
/// callers while the browser process is still spawning).
const PENDING_URL_LIMIT: usize = 8;

/// The measurement probe threshold for per-frame COM work: ticks that
/// exceed it log once per host per second (ADR-0006's frame-budget
/// evidence; healthy polls stay far below).
const SLOW_TICK_LOG: Duration = Duration::from_millis(5);

/// The completion latency threshold for the same probe (`call_cdp` round
/// trips; the harness budget lives at the request plane, 400 ms).
const CDP_SLOW_LOG: Duration = Duration::from_millis(100);

fn phase_of(state: &HostState) -> BrowserPhase {
    match state.phase {
        HostPhase::EnvRequested | HostPhase::ControllerRequested => BrowserPhase::Creating,
        HostPhase::Wired { .. } => BrowserPhase::Ready,
        HostPhase::Failed(_) => BrowserPhase::Failed,
        HostPhase::Closed => BrowserPhase::Closed,
    }
}

/// Kicks controller creation off an obtained environment. The completion
/// parks into `state.controller_slot`; the next tick promotes it. This keeps
/// creation non-blocking for the frame loop while staying single-threaded.
///
/// The default background color is set through the controller options at
/// creation time (upstream issue 4389's dark-mode base-paint guidance;
/// `Environment10` carries the options factory; on an older runtime the
/// plain call remains and the wire-time put in [`wire_controller`] applies).
/// Which of the two paths the runtime actually honors is not attributable
/// from capture evidence alone (both were active in the probe) — the
/// records below describe the observed paint, not the mechanism.
fn request_controller(
    state: &mut HostState,
    environment: &ICoreWebView2Environment,
) -> Result<(), String> {
    let slot: ControllerSlot = Rc::clone(&state.controller_slot);
    let handler = CreateCoreWebView2ControllerCompletedHandler::create(Box::new(
        move |result, controller| {
            if let Ok(mut bound) = slot.lock() {
                *bound = Some((result.err(), controller));
            }
            Ok(())
        },
    ));

    let environment10 = environment.cast::<ICoreWebView2Environment10>();
    let requested = match &environment10 {
        Ok(environment10) => request_controller_with_options(state, environment10, &handler)
            .or_else(|error| {
                log_once(&format!(
                    "controller with options failed ({error}); plain call"
                ));
                request_controller_plain(state, environment, &handler)
            }),
        Err(_) => request_controller_plain(state, environment, &handler),
    };
    requested.map_err(|e| e.to_string())?;
    state.phase = HostPhase::ControllerRequested;
    Ok(())
}

/// The options carrier: creates the controller with the design background
/// color initialized before the WebView has ever painted.
fn request_controller_with_options(
    state: &HostState,
    environment10: &ICoreWebView2Environment10,
    handler: &ICoreWebView2CreateCoreWebView2ControllerCompletedHandler,
) -> Result<(), windows::core::Error> {
    let options = unsafe { environment10.CreateCoreWebView2ControllerOptions()? };
    if let Ok(options3) = options.cast::<ICoreWebView2ControllerOptions3>() {
        let color = corewebview2_color(state.spec.default_background);
        if let Err(error) = unsafe { options3.SetDefaultBackgroundColor(color) } {
            log_once(&format!("default background rejected at options: {error}"));
        }
    } else {
        log_once("controller options carry no background color interface");
    }
    unsafe {
        environment10.CreateCoreWebView2ControllerWithOptions(
            state.parent_hwnd,
            &options,
            handler,
        )?;
    }
    Ok(())
}

/// The plain (pre-Environment10) controller request.
fn request_controller_plain(
    state: &HostState,
    environment: &ICoreWebView2Environment,
    handler: &ICoreWebView2CreateCoreWebView2ControllerCompletedHandler,
) -> Result<(), windows::core::Error> {
    unsafe { environment.CreateCoreWebView2Controller(state.parent_hwnd, handler) }
}

/// Wires a freshly created controller: acquires the webview, registers the
/// accelerator router on the controller, and clears apply-state so the next
/// tick pushes bounds/visibility/queued URLs.
fn wire_controller(
    state: &mut HostState,
    controller: &ICoreWebView2Controller,
) -> Result<ICoreWebView2, String> {
    let webview = unsafe { controller.CoreWebView2().map_err(|e| e.to_string())? };

    // Settings seam (M5.2 will revisit): keep every browser default; this
    // acquisition documents the extension point without pinning the type.
    let _ = unsafe { webview.Settings() };

    // The default document background (the shell's viewport fill): a
    // page with no defined background paints it below its content.
    // Kept as a backstop alongside the creation-time options put; if the
    // `ICoreWebView2Controller2` interface is absent (an ancient runtime),
    // keep the runtime default and say so once.
    match controller.cast::<ICoreWebView2Controller2>() {
        Ok(controller2) => {
            let color = corewebview2_color(state.spec.default_background);
            if let Err(error) = unsafe { controller2.SetDefaultBackgroundColor(color) } {
                log_once(&format!("default background rejected: {error}"));
            }
        }
        Err(_) => log_once("default background not applied: ICoreWebView2Controller2 unavailable"),
    }

    // Accelerator routing: app chords while the webview is focused must not
    // be swallowed by the browser. The documented WebView2 pattern applies:
    // intercept via AcceleratorKeyPressed + Handled(true), enqueue, and let
    // the shell act on the next frame. Native focus reports route the same
    // way, so a click INTO the covered child updates model focus (M5.2
    // focus unification).
    install_accelerator_router(state, controller)?;
    install_focus_router(state, controller)?;

    state.applied_bounds = None;
    state.applied_visible = None;

    Ok(webview)
}

/// The OS keyboard state as the accelerator contract needs it: the COM
/// callback runs on the UI thread inside input processing, so `GetKeyState`
/// reads the exact state of the event being dispatched (asynchronous
/// `GetAsyncKeyState` would be wrong here).
fn ctrl_held() -> bool {
    unsafe { (GetKeyState(VK_CONTROL.0 as i32) as u16 & 0x8000) != 0 }
}

/// 0xRRGGBB (the shell's token form) packed into an opaque
/// [`COREWEBVIEW2_COLOR`]: the alpha stays 255 because an opaque default
/// background is supported by every Evergreen runtime, while transparent
/// backgrounds carry a feature floor the port does not require.
fn corewebview2_color(rgb: u32) -> COREWEBVIEW2_COLOR {
    COREWEBVIEW2_COLOR {
        A: 255,
        R: ((rgb >> 16) & 0xff) as u8,
        G: ((rgb >> 8) & 0xff) as u8,
        B: (rgb & 0xff) as u8,
    }
}

fn install_accelerator_router(
    state: &HostState,
    controller: &ICoreWebView2Controller,
) -> Result<(), String> {
    let Some(router) = state.accel_router.clone() else {
        return Ok(()); // no router: unmanaged chords default to the browser
    };
    let handler = AcceleratorKeyPressedEventHandler::create(Box::new(
        move |_sender: Option<ICoreWebView2Controller>,
              args: Option<ICoreWebView2AcceleratorKeyPressedEventArgs>|
              -> windows::core::Result<()> {
            let Some(args) = args else {
                return Ok(());
            };
            let mut kind = COREWEBVIEW2_KEY_EVENT_KIND::default();
            unsafe { args.KeyEventKind(&mut kind)? };
            // Unfiltered diagnosability (log_once dedups): distinguishes a
            // never-firing registration from an event with an unexpected
            // key/kind shape.
            let mut key: u32 = 0;
            unsafe { args.VirtualKey(&mut key)? };
            log_once(&format!("accelerator event: kind={kind:?} VK=0x{key:02X}"));
            if (kind == COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN
                || kind == COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN)
                && router.routes_as_chord(ctrl_held(), key)
            {
                // Mark handled so the browser does not consume it; the
                // shell drains and acts on the next frame.
                unsafe { args.SetHandled(true)? };
                router.received(key);
            }
            Ok(())
        },
    ));
    let mut token: i64 = 0;
    let result = unsafe { controller.add_AcceleratorKeyPressed(&handler, &mut token) };
    result.map_err(|e| e.to_string())?;
    Ok(())
}

/// Native-focus routing (M5.2 unification): the controller confirms when
/// the webview child actually gained OS keyboard focus — including the
/// cases the shell cannot see (a click landing on the covering Chromium
/// HWND, alt-tab back into the app with the child last-focused). Wiring
/// `GotFocus` (not `LostFocus`: blur never deselects the pane) feeds model
/// focus the truth of where the keyboard went.
fn install_focus_router(
    state: &HostState,
    controller: &ICoreWebView2Controller,
) -> Result<(), String> {
    let Some(router) = state.accel_router.clone() else {
        return Ok(());
    };
    let surface_id = state.spec.surface_id.clone();
    let handler = FocusChangedEventHandler::create(Box::new(
        move |_sender: Option<ICoreWebView2Controller>, _args: Option<IUnknown>| {
            router.received_focus(&surface_id);
            Ok(())
        },
    ));
    let mut token: i64 = 0;
    let result = unsafe { controller.add_GotFocus(&handler, &mut token) };
    result.map_err(|e| e.to_string())?;
    Ok(())
}

fn finish_tick(state: &mut HostState, params: &TickParams) -> (BrowserPhase, BrowserSnapshot) {
    let started = Instant::now();
    if let HostPhase::Wired {
        controller,
        webview,
        ..
    } = &mut state.phase
    {
        // Rasterization scale tracks DPI; bounds are DIPs scaled to physical
        // pixels by the controller's rasterization hook.
        if let Ok(c3) = controller.cast::<ICoreWebView2Controller3>() {
            unsafe {
                let _ = c3.SetRasterizationScale(params.dpi_scale);
            }
        }

        if let Some(bounds) = params.bounds {
            let changed = match state.applied_bounds {
                Some((applied, applied_scale)) => {
                    applied != bounds || applied_scale != params.dpi_scale
                }
                None => true,
            };
            if changed {
                let rect = rect_to_win(bounds, params.dpi_scale);
                unsafe {
                    if let Err(error) = controller.SetBounds(rect) {
                        log_once(&format!("SetBounds: {error}"));
                    }
                }
                state.applied_bounds = Some((bounds, params.dpi_scale));
            }
        }

        if state.applied_visible != Some(params.visible) {
            unsafe {
                let _ = controller.SetIsVisible(params.visible);
            }
            state.applied_visible = Some(params.visible);
        }

        if params.focused && !state.applied_focused {
            unsafe {
                let _ = controller.MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC);
            }
            state.applied_focused = true;
        } else if !params.focused {
            state.applied_focused = false;
        }

        if let Some(window_pos) = params.window_pos {
            if state.last_window_pos != Some(window_pos) {
                unsafe {
                    let _ = controller.NotifyParentWindowPositionChanged();
                }
                state.last_window_pos = Some(window_pos);
            }
        }

        // Apply queued URLs once wired, in order.
        while let Some(url) = state.pending_urls.pop_front() {
            unsafe {
                if let Err(error) = webview.Navigate(&HSTRING::from(url.as_str())) {
                    log_once(&format!("navigate: {error}"));
                }
            }
        }

        // Poll title/source/canGo* at a modest cadence (these are agile COM
        // calls into the browser process; throttled to 20 ms).
        if state.last_poll.elapsed() > Duration::from_millis(20) {
            state.last_poll = Instant::now();
            unsafe {
                poll_into_snapshot(webview, &mut state.snapshot);
            }
        }
    }

    // Measurement probe (ADR-0006 Amendment 3): per-frame COM tick cost,
    // logged once per host per second when it exceeds the threshold.
    let tick = started.elapsed();
    if tick > SLOW_TICK_LOG
        && state
            .last_slow_log
            .is_none_or(|last| last.elapsed() > Duration::from_secs(1))
    {
        state.last_slow_log = Some(Instant::now());
        eprintln!(
            "cmux-webview2: slow com tick {:?} (surface {})",
            tick, state.spec.surface_id
        );
    }

    (phase_of(state), state.snapshot.clone())
}

/// # Safety
/// Webview COM calls on the frame thread only ( upheld by contract).
unsafe fn poll_into_snapshot(webview: &ICoreWebView2, snapshot: &mut BrowserSnapshot) {
    let mut pw: windows::core::PWSTR = windows::core::PWSTR::null();
    if webview.Source(&mut pw).is_ok() {
        let url = webview2_com::take_pwstr(pw);
        if !url.is_empty() {
            snapshot.source = url;
        }
    }
    let mut pw: windows::core::PWSTR = windows::core::PWSTR::null();
    if webview.DocumentTitle(&mut pw).is_ok() {
        snapshot.title = webview2_com::take_pwstr(pw);
    }
    let mut back = BOOL::default();
    let mut forward = BOOL::default();
    if webview.CanGoBack(&mut back).is_ok() {
        snapshot.can_go_back = back.as_bool();
    }
    if webview.CanGoForward(&mut forward).is_ok() {
        snapshot.can_go_forward = forward.as_bool();
    }
}

/// De-duplicates log spam for per-frame failures.
fn log_once(message: &str) {
    static DEDUP: Mutex<Option<String>> = Mutex::new(None);
    if let Ok(mut cell) = DEDUP.lock() {
        let already = cell.as_deref() == Some(message);
        if !already {
            eprintln!("cmux-webview2: {message}");
            *cell = Some(message.to_string());
        }
    }
}

/// Encodes a UTF-16 string for a `PCWSTR` call; the returned buffer must
/// outlive the call (`(ptr, buffer)`).
fn pcw(s: &str) -> (PCWSTR, Vec<u16>) {
    let mut buf: Vec<u16> = s.encode_utf16().collect();
    buf.push(0);
    (PCWSTR::from_raw(buf.as_ptr()), buf)
}

// ---------------------------------------------------------------------------
// Accelerator router: intercepts app chords while a webview owns focus.
// ---------------------------------------------------------------------------

/// Routes accelerator keys pressed while a webview is focused back into the
/// app. The shell builds ONE shared router and passes it to every
/// [`BrowserHost::start`]; the frame loop drains queued chords each frame.
#[derive(Clone)]
pub struct AccelRouter {
    inner: Arc<RouterInner>,
}

impl std::fmt::Debug for AccelRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AccelRouter")
    }
}

struct RouterInner {
    /// Virtual keys (`VK_*`) the app wants while a webview is focused.
    keys: Mutex<BTreeSet<u32>>,
    queue: Mutex<VecDeque<u32>>,
    /// Pane ids whose webview confirmed native keyboard focus
    /// (controller `GotFocus`), drained by the shell each frame.
    focus_queue: Mutex<VecDeque<String>>,
    wake: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
}

impl AccelRouter {
    /// Builds a router whose key set starts as `keys` (`VK_*` codes).
    pub fn new(keys: BTreeSet<u32>) -> Self {
        Self {
            inner: Arc::new(RouterInner {
                keys: Mutex::new(keys),
                queue: Mutex::new(VecDeque::new()),
                focus_queue: Mutex::new(VecDeque::new()),
                wake: Mutex::new(None),
            }),
        }
    }

    /// Installs a wake-up consulted after enqueue (usually
    /// `egui::Context::request_repaint`); call once from the shell.
    pub fn set_wake(&self, wake: Box<dyn Fn() + Send + Sync>) {
        if let Ok(mut wake_slot) = self.inner.wake.lock() {
            *wake_slot = Some(wake);
        }
    }

    /// Adds virtual keys after construction (binding reload etc.).
    pub fn add_keys(&self, keys: impl IntoIterator<Item = u32>) {
        if let Ok(mut guard) = self.inner.keys.lock() {
            guard.extend(keys);
        }
    }

    /// Drains queued chords (virtual keys, in arrival order).
    pub fn take_chords(&self) -> Vec<u32> {
        self.inner
            .queue
            .lock()
            .map(|mut q| std::mem::take(&mut *q).into_iter().collect())
            .unwrap_or_default()
    }

    fn key_match(&self, virtual_key: u32) -> bool {
        self.inner
            .keys
            .lock()
            .map(|keys| keys.contains(&virtual_key))
            .unwrap_or(false)
    }

    /// Capture decision for one accelerator event. A registered key routes
    /// ONLY while Ctrl is actually held (M5.2 modifier gate): `shift+digit`
    /// types into the page, and `alt+digit` is a chord the register does
    /// not own — with the gate (ctrl held) the browser keeps both instead
    /// of the shell silently switching workspaces. `GotFocus`-driven
    /// capture is separate (`received_focus`).
    pub fn routes_as_chord(&self, ctrl_held: bool, virtual_key: u32) -> bool {
        ctrl_held && self.key_match(virtual_key)
    }

    /// Drains the pane ids whose webviews reported native focus gain
    /// (controller `GotFocus`), in arrival order.
    pub fn take_focus_gained(&self) -> Vec<String> {
        self.inner
            .focus_queue
            .lock()
            .map(|mut q| std::mem::take(&mut *q).into_iter().collect())
            .unwrap_or_default()
    }

    fn received(&self, virtual_key: u32) {
        log_once(&format!(
            "accelerator captured: VK 0x{virtual_key:02X} (waiting for the frame drain)"
        ));
        if let Ok(mut guard) = self.inner.queue.lock() {
            guard.push_back(virtual_key);
        }
        self.wake_frame();
    }

    /// Records a native `GotFocus` for `surface_id` (called from the COM
    /// callback on the UI thread; wakes the frame for the drain).
    fn received_focus(&self, surface_id: &str) {
        log_once(&format!(
            "accelerator captured: focus -> {surface_id} (webview gained native focus)"
        ));
        if let Ok(mut guard) = self.inner.focus_queue.lock() {
            guard.push_back(surface_id.to_owned());
        }
        self.wake_frame();
    }

    fn wake_frame(&self) {
        if let Ok(wake) = self.inner.wake.lock() {
            if let Some(wake) = wake.as_ref() {
                wake();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_viewport_token_packs_opaque_into_the_webview_color() {
        let color = corewebview2_color(0x0C0E14);
        assert_eq!(color.A, 255);
        assert_eq!(color.R, 0x0C);
        assert_eq!(color.G, 0x0E);
        assert_eq!(color.B, 0x14);
        // The white custom-channel guard: byte order must survive the pack.
        let warm = corewebview2_color(0xFFC42F);
        assert_eq!((warm.R, warm.G, warm.B), (0xFF, 0xC4, 0x2F));
    }

    #[test]
    fn a_dip_rect_rounds_to_device_pixels_at_scale() {
        let rect = Rect::new(10.0, 20.0, 110.0, 70.0);
        let win = rect_to_win(rect, 1.25);
        assert_eq!(win.left, 13); // 10 * 1.25 = 12.5 rounds up
        assert_eq!(win.top, 25); // 20 * 1.25
        assert_eq!(win.right, 138); // 110 * 1.25 = 137.5 rounds up
        assert_eq!(win.bottom, 88); // (20 + 50) * 1.25 = 87.5 rounds up
    }

    #[test]
    fn a_dip_rect_stays_stable_at_scale_one_with_halfpoints() {
        let rect = Rect::new(0.5, 0.5, 3.0, 3.0);
        let win = rect_to_win(rect, 1.0);
        assert_eq!((win.left, win.top, win.right, win.bottom), (1, 1, 3, 3));
    }

    #[test]
    fn an_accel_router_enqueues_only_matching_keys_and_drains_in_order() {
        let router = AccelRouter::new(BTreeSet::from([0x31, 0x32, 0x39]));
        assert!(router.key_match(0x31)); // VK_1
        assert!(!router.key_match(0x41)); // VK_A never routed
        router.received(0x32);
        router.received(0x39);
        router.received(0x31);
        assert_eq!(router.take_chords(), vec![0x32, 0x39, 0x31]);
        assert!(router.take_chords().is_empty());
    }

    #[test]
    fn a_chord_routes_only_while_ctrl_is_actually_held() {
        // M5.2 modifier gate: `shift+2` types `@` into the page and
        // `alt+2` stays a browser accelerator; only a Ctrl-confirmed press
        // of a registered key re-enters the shell's action stream.
        let router = AccelRouter::new(BTreeSet::from([0x31, 0x32]));
        assert!(router.routes_as_chord(true, 0x32));
        assert!(!router.routes_as_chord(false, 0x32)); // shift/alt digit
        assert!(!router.routes_as_chord(false, 0x31)); // plain pressed digit
        assert!(!router.routes_as_chord(true, 0x70)); // F1: registered set only
    }

    #[test]
    fn native_focus_gains_drain_per_surface_in_order() {
        let router = AccelRouter::new(BTreeSet::from([0x31]));
        router.received_focus("p3");
        router.received_focus("p7");
        router.received_focus("p3");
        assert_eq!(
            router.take_focus_gained(),
            vec!["p3".to_owned(), "p7".to_owned(), "p3".to_owned()]
        );
        assert!(router.take_focus_gained().is_empty());
    }

    #[test]
    fn cdp_probe_tracks_one_wall_clock_per_call() {
        // The completion latency probe: a marked call measures exactly
        // once (the entry is consumed), an unknown id measures as none,
        // and re-marking replaces the start (the synchronous-failure
        // cleanup path after a re-issue).
        let mut tracker = CdpTracker::default();
        assert!(tracker.take(7).is_none());
        tracker.mark(7);
        assert!(tracker.take(7).is_some());
        assert!(tracker.take(7).is_none());
        tracker.mark(7);
        tracker.mark(7);
        assert!(tracker.take(7).is_some());
        assert!(tracker.take(7).is_none());
        // Independent ids never interfere.
        tracker.mark(1);
        tracker.mark(2);
        assert!(tracker.take(1).is_some() && tracker.take(2).is_some());
    }
}
