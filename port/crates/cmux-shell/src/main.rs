//! The M1/M2 slice host: a real Windows shell inside a real window, now
//! with split panes.
//!
//! Architecture (ADRs 0002/0003/0004/0007): ConPTY via `portable-pty`
//! (_cmux-pty-port_), VT emulation by `libghostty-vt` (through `egui_tty`),
//! rendering by egui/eframe, split-pane geometry by `cmux-layout` (ported
//! from cmux's CmuxPanes) through the pure `panes::PaneTree` model.
//! The terminal widget answers ConPTY's startup probes on the pty writer
//! (reply hook), which our own ConPTY probes verified as required.
//!
//! Pane lifecycle: a watcher thread observes each pane's child (ConPTY gives
//! no EOF on child exit). When a pane's shell exits â€” e.g. the user types
//! `exit` â€” that pane closes immediately (owner product decision
//! 2026-10-17; the earlier M1.1 notice row with New shell/Close pane is
//! gone, a manual restart affordance is deferred to the config/menu
//! milestone). The exit reason is logged to stderr. Closing the last pane
//! closes the window.
//!
//! Visual separation is renderer-side chrome (`panes::gutter_inset` + border
//! strokes): panes share a gutter (`PANE_GUTTER`) on sides that touch a
//! neighbor, stay full-bleed against the window frame, and are stroked
//! after painting â€” brighter on the focused pane. Colors come from the
//! egui theme's selection/widget strokes until the theme/config milestone.
//!
//! Keybindings and per-app settings come from the config file (`cmux-config`,
//! ADR-0008): `%USERPROFILE%\.config\cmux\cmux.json`, mirroring upstream's
//! `~/.config/cmux/cmux.json` (override via `CMUX_CONFIG_FILE`; a template is
//! written on first run). It carries the font magnification percent, pane
//! gutter and resize step, an optional custom shell, and `shortcuts.bindings`
//! overrides (upstream `ShortcutAction` ids; the built-in stroke defaults are
//! the previously accepted provisional set). Binding events are consumed
//! before the terminals see them.
//!
//! Automated smoke modes: `CMUX_SHELL_AUTO_EXIT_MS=<ms>` closes the window
//! after the delay; `CMUX_SHELL_AUTO_SPLIT_MS=<ms>` performs one split
//! right at the delay and one split down 1.5 s later (machine-checked
//! split lifecycle; splitting/typing/editing is still a human rubric).
//! `CMUX_SHELL_RESTORE_SMOKE=1` restores the pane tree from the session
//! snapshot and auto-exits without saving (asserting a restore must not
//! rewrite the store). The one-shot smoke (`CMUX_SHELL_SPAWN_ONESHOT=1`)
//! restores and saves nothing. `CMUX_SESSION_FILE` relocates the snapshot
//! store (machine tests; default `%APPDATA%\cmux\session.json`).

use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::{mpsc, Arc, Mutex},
    time::{Duration, Instant},
};

use cmux_config::{Binding, Config};
use cmux_layout::{Orientation, PaneCycleNavigator, PixelRect, ResizeDirection, SplitDirection};
use cmux_pty_port::PtySpec;
// The element-verb wire types ride through the whole automation plane
// (builders, selectors, replies); everything else stays fully qualified.
use cmux_ipc::{ElementTarget, GetField, IsState, KeyHalf, LoadState};
use cmux_workspaces::{
    AppSessionSnapshot, LoadOutcome, PaneLayoutSnapshot, PanelSnapshot, SessionPaths,
    SplitLayoutSnapshot, TabManagerSnapshot, WindowSnapshot, WorkspaceLayoutSnapshot,
    WorkspaceSnapshot,
};
use egui::epaint::{CornerRadius, StrokeKind};
use egui::{Key, Modifiers};
use egui_tty::{Terminal, TerminalStyle};

use cmux_shell::panes::{
    gutter_inset, mint_pane_id, mint_workspace_id, seed_pane_ids, seed_workspace_ids, PaneLeaf,
    PaneTree, TreeOutline,
};
use cmux_shell::policy::{self, DeliveryContext, PolicyEffects};
use cmux_shell::shell_session::{start_shell, PaneEvent, WatchedStatus};
use cmux_shell::turns::{TurnEvent, TurnPending};

/// Wakes the frame loop from shell-reader threads. An event (OSC
/// notification or turn boundary) reaching a channel while the frame loop
/// is idle must wake it, or the loop's deadline sweep — the only thing
/// that observes a pending turn — never runs and the delivery is
/// silently lost (defect found 2026-10-19: a perfect stream with no frame
/// wake produced no turn delivery until the app was brought to the
/// foreground). The egui context is installed on the first frame; panes
/// spawned before the window exists (restore's first pane) wake nothing
/// until then, and the frames that follow drain whatever queued up.
struct FrameWake {
    context: Mutex<Option<egui::Context>>,
}

impl FrameWake {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            context: Mutex::new(None),
        })
    }

    /// Installs the running context; idempotent (a later frame may simply
    /// overwrite with its own clone of the same context).
    fn install(&self, context: egui::Context) {
        *self.context.lock().unwrap() = Some(context);
    }

    /// Wakes the frame loop from any thread (no-op before the first frame
    /// installs the context). The IPC server thread uses this so an
    /// app-plane call answered without user input does not wait for a
    /// keystroke or repaint.
    fn wake_now(self: &Arc<Self>) {
        if let Some(context) = self.context.lock().unwrap().as_ref() {
            context.request_repaint();
        }
    }

    /// The closure handed to `start_shell`; egui coalesces repaint
    /// requests, so a wake per chunk is cheap.
    fn as_wake(self: &Arc<Self>) -> cmux_shell::shell_session::WakeFn {
        let cell = Arc::clone(self);
        Arc::new(move || {
            if let Some(context) = cell.context.lock().unwrap().as_ref() {
                context.request_repaint();
            }
        })
    }
}

/// One M4.1 app-plane call handed from the server thread into the frame
/// loop. The server thread parks on `reply` for at most
/// [`APP_REPLY_BUDGET`]; the frame loop's `serve_ipc_calls` completes the
/// round trip on the UI thread where the whole model mutates safely.
struct IpcCallMessage {
    call: cmux_ipc::AppCall,
    reply: mpsc::Sender<Result<serde_json::Value, cmux_ipc::RequestProblem>>,
}

/// How long the server thread waits for the app to answer an app-plane
/// call. Kept under the client's [`cmux_ipc::DEFAULT_RESPONSE_BUDGET`]
/// (500 ms) so the client reads either the result or the structured
/// `timeout` error, never a transport deadline.
const APP_REPLY_BUDGET: Duration = Duration::from_millis(400);

/// The extra time a `browser.wait` call's server thread parks beyond its
/// deadline — upstream's CLI uses the same formula (`waitMs + 8 s`) so the
/// app handler reports the wait's real outcome instead of the socket
/// layer dying first.
const WAIT_REPLY_HEADROOM_MS: u64 = 8_000;

/// What a browser-automation call executes on the page (M5.2 P0 subset of
/// upstream's agent-browser action list; the plan shape mirrors upstream's
/// "key/mouse plans": pure intent, no page-query state in Rust beyond the
/// ref table itself).
#[derive(Clone, Debug, PartialEq)]
enum CdpKind {
    /// `browser.eval`: one `Runtime.evaluate`, reply carries the value.
    Eval { expression: String },
    /// `browser.snapshot`: the walker expression, reply carries text+refs
    /// and installs the refs table.
    Snapshot,
    /// `browser.click`/`browser.dblclick`: fresh-center evaluate, then a
    /// mouse press+release at those coordinates.
    ClickMouse {
        target: ElementTarget,
        click_count: u32,
    },
    /// `browser.type`: focus evaluate (`el.focus()`), then one
    /// `Input.insertText` (documented divergence from upstream's per-key
    /// stream).
    TypeText { target: ElementTarget, text: String },
    /// M5.3: the uniform one-evaluate element verbs (fill / focus / check
    /// / uncheck / select / scroll-into-view / get.* / is.*): the JS is
    /// built from the resolved target selector, one `Runtime.evaluate`
    /// runs it, and the shape step turns the JSON into the reply.
    ElementJs {
        target: ElementTarget,
        js: ElementJs,
    },
    /// M5.3: `browser.scroll` — one `window.scrollBy`; the reply carries
    /// the resulting offsets.
    ScrollBy { dx: f64, dy: f64 },
    /// M5.3: `browser.hover` — fresh-center evaluate, then one
    /// `Input.dispatchMouseEvent` `mouseMoved` (no press, button none).
    HoverMouse { target: ElementTarget },
    /// M5.3: `browser.screenshot` — one `Page.captureScreenshot` (viewport
    /// PNG); the completion's base64 decodes to a file under `%TEMP%`.
    Screenshot,
    /// M5.4: `browser.press` / `browser.keydown` / `browser.keyup` — the
    /// raw key token, resolved to a [`KeyPlan`] during prepare (unknown
    /// vocabulary answers `invalid_params` there). `press` sends the
    /// down+up pair, the other two a single half.
    Key { half: KeyHalf, token: String },
    /// M5.4: `browser.wait` — poll the predicate evaluate until every
    /// slot is true or the deadline elapses; the deadline and waited-ms
    /// anchor live on the pending.
    Wait { poll: WaitPoll, timeout_ms: u32 },
}

/// The one-evaluate element verbs' JS plan (M5.3; each maps to one
/// expression builder + one reply parser, sharing the ref-XOR-selector
/// target machinery with the M5.2 verbs).
#[derive(Clone, Debug, PartialEq)]
enum ElementJs {
    Fill {
        text: String,
    },
    Focus,
    Check {
        checked: bool,
    },
    SelectOption {
        value: String,
    },
    ScrollIntoView,
    GetDom {
        field: GetField,
        attr: Option<String>,
        property: Option<String>,
    },
    IsState {
        state: IsState,
    },
}

/// The next action a completion escalates to (the pending call's state
/// machine; `Final` shapes the reply from the completed call — the last
/// planned call always maps to it).
#[derive(Clone, Debug, PartialEq)]
enum CdpStep {
    Final,
    /// The completed call is a fresh-center evaluate: parse (x, y), then
    /// dispatch `mousePressed` there.
    AfterCenter {
        click_count: u32,
    },
    /// The completed call is `mousePressed`: dispatch `mouseReleased`.
    AfterPress {
        x: f64,
        y: f64,
        click_count: u32,
    },
    /// The completed call is the focus evaluate: `Input.insertText` next.
    AfterFocus {
        text: String,
    },
    /// The completed call is a hover's fresh-center evaluate: parse
    /// (x, y), then just move the mouse there (no press; M5.3).
    AfterHoverCenter,
    /// The completed call is a key-press's keyDown event: dispatch the
    /// prepared keyUp event next (M5.4).
    AfterKeyDown {
        up: serde_json::Value,
    },
}

/// One in-flight automation call (M5.2): issued from the serve pass,
/// completed on a later frame's pump, answered through the parked reply
/// channel exactly like the sync verbs (the server thread's budget —
/// per-call since M5.4's waits — still bounds the round trip).
struct PendingCdp {
    id: u64,
    surface_id: String,
    kind: CdpKind,
    step: CdpStep,
    /// `browser.wait` only: the instant a further poll may not continue
    /// past (the predicate's deadline; `None` for every other kind).
    deadline: Option<Instant>,
    /// The first issue of this call: waits compute `waited_ms` from it.
    first_issued: Instant,
    /// The sweep's freshness anchor: refreshed every re-issue so a long
    /// wait is never swept while its polls keep landing.
    last_activity: Instant,
    reply: mpsc::Sender<Result<serde_json::Value, cmux_ipc::RequestProblem>>,
}

// ---------------------------------------------------------------------------
// M5.2 browser automation (the agent-browser-port P0 subset over CDP)
// ---------------------------------------------------------------------------

/// A validated initial automation call: resolve + shape before any COM.
struct CdpPrep {
    surface_id: String,
    host: cmux_webview2::BrowserHost,
    method: &'static str,
    params: String,
    first_step: CdpStep,
}

/// The `browser.snapshot` walker: one `Runtime.evaluate` building the
/// visible text lines plus ephemeral `@eN` refs for interactive elements,
/// each with the CSS selector that re-resolves it. Original port code
/// (upstream parity is semantic, not a copied tree); refs are
/// snapshot-local and any navigation clears them (upstream object
/// semantics: refs are ephemeral).
const SNAPSHOT_WALKER_JS: &str = r#"(function () {
  var REFS = [];
  var LINES = [];
  function esc(value) { return String(value).replace(/\s+/g, ' ').trim(); }
  function selectorFor(el) {
    var parts = [];
    var cur = el;
    while (cur && cur.nodeType === 1 && cur !== document.documentElement) {
      if (cur.id) { parts.unshift('#' + CSS.escape(cur.id)); break; }
      var tag = cur.tagName.toLowerCase();
      var nth = 1;
      var sib = cur;
      while ((sib = sib.previousElementSibling)) {
        if (sib.tagName === cur.tagName) nth++;
      }
      parts.unshift(tag + ':nth-of-type(' + nth + ')');
      cur = cur.parentElement;
    }
    if (!parts.length) { parts.push(el.tagName.toLowerCase()); }
    return parts.join(' > ');
  }
  function labelFor(el) {
    var tag = el.tagName.toLowerCase();
    var text = el.getAttribute && (el.getAttribute('title') || el.getAttribute('aria-label') || el.getAttribute('placeholder'));
    if (!text && (tag === 'input' || tag === 'textarea')) { text = el.value; }
    if (!text) { text = esc(el.textContent); }
    return text.slice(0, 120);
  }
  function describeFor(el) {
    var tag = el.tagName.toLowerCase();
    var role = el.getAttribute && el.getAttribute('role');
    var kinds = { a: 'link', button: 'button', input: 'input', textarea: 'textbox', select: 'combobox', summary: 'summary' };
    var kind = role || kinds[tag] || tag;
    var text = labelFor(el);
    return text ? kind + ' "' + text + '"' : kind;
  }
  function interactive(el) {
    var tag = el.tagName.toLowerCase();
    if (tag === 'a' && el.hasAttribute('href')) { return true; }
    if (tag === 'button' || tag === 'input' || tag === 'textarea' || tag === 'select' || tag === 'summary') { return true; }
    if (el.isContentEditable) { return true; }
    if (el.hasAttribute('role')) {
      var roles = ['button', 'link', 'checkbox', 'radio', 'textbox', 'combobox', 'tab', 'switch', 'menuitem'];
      if (roles.includes(el.getAttribute('role'))) { return true; }
    }
    if (el.hasAttribute('tabindex') && el.tabIndex >= 0) { return true; }
    return false;
  }
  function walk(node, depth) {
    if (node.nodeType === 3) {
      var text = esc(node.textContent);
      if (text) { LINES.push('  '.repeat(depth) + '- ' + text); }
      return;
    }
    if (node.nodeType !== 1) { return; }
    var tag = node.tagName.toLowerCase();
    if (tag === 'script' || tag === 'style' || tag === 'noscript' || tag === 'template' || tag === 'svg') { return; }
    var style = window.getComputedStyle(node);
    if (style.display === 'none' || style.visibility === 'hidden') { return; }
    if (interactive(node)) {
      var rect = node.getBoundingClientRect();
      REFS.push({
        ref: '@e' + (REFS.length + 1),
        selector: selectorFor(node),
        tag: tag,
        label: labelFor(node),
        center: { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 }
      });
      LINES.push('  '.repeat(depth) + '- ' + describeFor(node) + ' [@e' + REFS.length + ']');
      return;
    }
    var children = node.childNodes;
    for (var i = 0; i < children.length; i++) { walk(children[i], depth + 1); }
  }
  walk(document.body, 0);
  return {
    title: document.title,
    url: location.href,
    text: LINES.join('\n'),
    refs: REFS
  };
})()"#;

/// A `Runtime.evaluate` parameter object for the walker and one-off page
/// queries (returnByValue so plain JSON comes back; userGesture lets the
/// page react as if a user drove it).
fn cdp_evaluate_params(expression: String) -> serde_json::Value {
    serde_json::json!({
        "expression": expression,
        "returnByValue": true,
        "userGesture": true,
    })
}

/// The fresh-center evaluate for a ref's selector (click/dblclick phase 1).
/// The selector is JSON-encoded into the expression: a JSON string literal
/// is valid JavaScript, so page-provided ids can never break out.
fn center_evaluate_js(selector: &str) -> String {
    let literal = serde_json::to_string(selector).unwrap_or_else(|_| "''".to_owned());
    format!(
        "(function () {{ var el = document.querySelector({literal}); if (!el) {{ return null; }} \
         var r = el.getBoundingClientRect(); \
         return {{ x: r.left + r.width / 2, y: r.top + r.height / 2 }}; }})()"
    )
}

/// The focus evaluate for `browser.type` phase 1: focusing the element is
/// what insertText needs (an `el.focus()` beats a full synthetic click:
/// one CDP call, same keyboard outcome).
fn focus_evaluate_js(selector: &str) -> String {
    let literal = serde_json::to_string(selector).unwrap_or_else(|_| "''".to_owned());
    format!(
        "(function () {{ var el = document.querySelector({literal}); if (!el) {{ return null; }} \
         el.focus(); return true; }})()"
    )
}

/// The default `browser.get.styles` property set (documented; one test
/// locks the names). A request's `property` parameter replaces this list.
const DEFAULT_STYLE_PROPS: [&str; 6] = [
    "display",
    "visibility",
    "color",
    "background-color",
    "font-size",
    "opacity",
];

/// `browser.fill`: replace the element's value (or text content for
/// contenteditable hosts) and dispatch `input`+`change` — a whole-value
/// replace like upstream's `fill`, not keystrokes. The prototype's own
/// `value` setter runs first so tracked forms (the React-style guard)
/// observe the write.
fn fill_evaluate_js(selector: &str, text: &str) -> String {
    let selector_literal = serde_json::to_string(selector).unwrap_or_else(|_| "''".to_owned());
    let text_literal = serde_json::to_string(text).unwrap_or_else(|_| "''".to_owned());
    format!(
        "(function () {{ var el = document.querySelector({selector_literal}); \
         if (!el) {{ return null; }} \
         var EVENT = new Event('input', {{ bubbles: true }}), CH = new Event('change', {{ bubbles: true }}); \
         if (el.isContentEditable) {{ el.textContent = {text_literal}; el.dispatchEvent(EVENT); \
         el.dispatchEvent(CH); return true; }} \
         var desc = Object.getOwnPropertyDescriptor(Object.getPrototypeOf(el) || {{}}, 'value'); \
         if (desc && desc.set) {{ desc.set.call(el, {text_literal}); }} else {{ el.value = {text_literal}; }} \
         el.dispatchEvent(EVENT); el.dispatchEvent(CH); return true; }})()"
    )
}

/// `browser.check` / `browser.uncheck`: guard the input type (checkbox or
/// radio only), set `checked`, dispatch `change`. Sentinels (strings)
/// let the shell answer structured `invalid_state` messages.
fn check_evaluate_js(selector: &str, checked: bool) -> String {
    let selector_literal = serde_json::to_string(selector).unwrap_or_else(|_| "''".to_owned());
    let checked_literal = if checked { "true" } else { "false" };
    format!(
        "(function () {{ var el = document.querySelector({selector_literal}); \
         if (!el) {{ return null; }} \
         if (!el.type || (el.type !== 'checkbox' && el.type !== 'radio')) {{ return 'not-checkbox'; }} \
         if (el.checked === {checked_literal}) {{ return true; }} \
         el.checked = {checked_literal}; el.dispatchEvent(new Event('change', {{ bubbles: true }})); \
         return true; }})()"
    )
}

/// `browser.select`: match an `<option>` by value or visible label, then
/// enroll it exactly the way a user change would.
fn select_evaluate_js(selector: &str, value: &str) -> String {
    let selector_literal = serde_json::to_string(selector).unwrap_or_else(|_| "''".to_owned());
    let value_literal = serde_json::to_string(value).unwrap_or_else(|_| "''".to_owned());
    format!(
        "(function () {{ var el = document.querySelector({selector_literal}); \
         if (!el) {{ return null; }} \
         if (!el.tagName || el.tagName.toLowerCase() !== 'select') {{ return 'not-select'; }} \
         var target = null; \
         for (var i = 0; i < el.options.length; i++) {{ \
         var opt = el.options[i]; \
         if (opt.value === {value_literal} || opt.label === {value_literal}) {{ target = opt; break; }} }} \
         if (!target) {{ return 'no-option'; }} \
         el.value = target.value; \
         el.dispatchEvent(new Event('change', {{ bubbles: true }})); return true; }})()"
    )
}

/// `browser.scroll_into_view`: center the element in the viewport.
fn scroll_into_view_evaluate_js(selector: &str) -> String {
    let literal = serde_json::to_string(selector).unwrap_or_else(|_| "''".to_owned());
    format!(
        "(function () {{ var el = document.querySelector({literal}); \
         if (!el) {{ return null; }} \
         el.scrollIntoView({{ block: 'center', inline: 'nearest' }}); return true; }})()"
    )
}

/// `browser.get.*` DOM reads (except count, below): one querySelector,
/// then the field-specific read. Reads return plain JSON values (a
/// `DOMRect` never serializes through `returnByValue`, so `box` builds a
/// plain `{x,y,width,height}`).
fn dom_field_evaluate_js(
    selector: &str,
    field: GetField,
    attr: Option<&str>,
    property: Option<&str>,
) -> String {
    let literal = serde_json::to_string(selector).unwrap_or_else(|_| "''".to_owned());
    let head = format!(
        "(function () {{ var el = document.querySelector({literal}); if (!el) {{ return null; }} "
    );
    let body = match field {
        GetField::Text => "return String(el.textContent); })()".to_owned(),
        GetField::Html => "return String(el.outerHTML); })()".to_owned(),
        GetField::Value => "return ('value' in el) ? String(el.value) : null; })()".to_owned(),
        GetField::Attr => {
            let attr_literal =
                serde_json::to_string(attr.unwrap_or_default()).unwrap_or_else(|_| "''".to_owned());
            format!("return el.getAttribute({attr_literal}); }})()")
        }
        GetField::Box => "var r = el.getBoundingClientRect(); \
             return { x: r.x, y: r.y, width: r.width, height: r.height }; })()"
            .to_owned(),
        GetField::Styles => {
            let names = match property {
                Some(one) => vec![one],
                None => DEFAULT_STYLE_PROPS.to_vec(),
            };
            let names_literal = serde_json::to_string(&names).unwrap_or_else(|_| "[]".to_owned());
            format!(
                "var cs = window.getComputedStyle(el); var out = {{}}; \
                 var names = {names_literal}; \
                 for (var i = 0; i < names.length; i++) {{ \
                 out[names[i]] = cs.getPropertyValue(names[i]); }} return out; }})()"
            )
        }
        GetField::Count => count_evaluate_js(selector),
    };
    format!("{head} {body}")
}

/// `browser.get.count` — how many elements the selector matches; a count
/// of 0 is a valid reply, so the element-null contract does not apply.
fn count_evaluate_js(selector: &str) -> String {
    let literal = serde_json::to_string(selector).unwrap_or_else(|_| "''".to_owned());
    format!("document.querySelectorAll({literal}).length")
}

/// `browser.is.*` probes: null when the element is gone (the shell reads
/// it as `false`, upstream contrast-free semantics), else the probe
/// object keyed by the state.
fn is_state_evaluate_js(selector: &str, state: IsState) -> String {
    let literal = serde_json::to_string(selector).unwrap_or_else(|_| "''".to_owned());
    let probe = match state {
        IsState::Visible => {
            "return { visible: r.width > 0 && r.height > 0 \
             && cs.display !== 'none' && cs.visibility !== 'hidden' }; })()"
        }
        IsState::Enabled => {
            "return { enabled: el.disabled !== true && \
             el.getAttribute('aria-disabled') !== 'true' }; })()"
        }
        IsState::Checked => "return { checked: el.checked === true }; })()",
    };
    format!(
        "(function () {{ var el = document.querySelector({literal}); \
         if (!el) {{ return null; }} \
         var r = el.getBoundingClientRect(); var cs = window.getComputedStyle(el); {probe}"
    )
}

/// `browser.scroll`: one `window.scrollBy`, then report the resulting
/// offsets (the reply shape's `x`/`y`).
fn scroll_evaluate_js(dx: f64, dy: f64) -> String {
    format!(
        "(function () {{ window.scrollBy({dx}, {dy}); \
         return {{ x: window.scrollX, y: window.scrollY }}; }})()"
    )
}

// ---------------------------------------------------------------------------
// M5.4 key synthesis (browser.press / keydown / keyup)
// ---------------------------------------------------------------------------

/// CDP `Input.dispatchKeyEvent`'s modifier bits (`modifiers` field).
const KEY_MODIFIER_ALT: u8 = 1;
/// CDP `Input.dispatchKeyEvent`'s modifier bits (`modifiers` field).
const KEY_MODIFIER_CONTROL: u8 = 2;
/// CDP `Input.dispatchKeyEvent`'s modifier bits (`modifiers` field).
const KEY_MODIFIER_META: u8 = 4;
/// CDP `Input.dispatchKeyEvent`'s modifier bits (`modifiers` field).
const KEY_MODIFIER_SHIFT: u8 = 8;

/// One accepted key of the port's Playwright-style vocabulary:
///
/// - `name`: the normalized token callers use (case-insensitive; also the
///   `KeyboardEvent.key` page code sees for non-printing keys),
/// - `vk`: the Windows virtual-key code Chromium maps the event through,
/// - `label`: the CDP `key` field — the page's `e.key` value (a named key's
///   name, or the unshifted character for printing keys),
/// - `text`/`shift`: the US-layout character the key produces unshifted and
///   with Shift (printing keys only).
#[derive(Debug)]
struct KeyRow {
    name: &'static str,
    vk: u16,
    label: &'static str,
    text: Option<&'static str>,
    shift: Option<&'static str>,
}

/// The lower-case spelling alias table (the caller's convenience names).
const KEY_ALIASES: [(&str, &str); 9] = [
    ("return", "Enter"),
    ("esc", "Escape"),
    ("spacebar", "Space"),
    ("left", "ArrowLeft"),
    ("right", "ArrowRight"),
    ("up", "ArrowUp"),
    ("down", "ArrowDown"),
    ("del", "Delete"),
    ("backtick", "Backquote"),
];

/// Builds the key vocabulary table. Named keys, the modifier keys
/// themselves and US punctuation are written out; letters, digits and the
/// F-key rows are generated (same row shape, no typo surface).
fn key_table() -> &'static [KeyRow] {
    static TABLE: std::sync::OnceLock<Vec<KeyRow>> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        let mut rows: Vec<KeyRow> = vec![
            KeyRow {
                name: "Enter",
                vk: 0x0D,
                label: "Enter",
                text: Some("\r"),
                shift: None,
            },
            KeyRow {
                name: "Tab",
                vk: 0x09,
                label: "Tab",
                text: None,
                shift: None,
            },
            KeyRow {
                name: "Space",
                vk: 0x20,
                label: " ",
                text: Some(" "),
                shift: None,
            },
            KeyRow {
                name: "Escape",
                vk: 0x1B,
                label: "Escape",
                text: None,
                shift: None,
            },
            KeyRow {
                name: "Backspace",
                vk: 0x08,
                label: "Backspace",
                text: None,
                shift: None,
            },
            KeyRow {
                name: "Delete",
                vk: 0x2E,
                label: "Delete",
                text: None,
                shift: None,
            },
            KeyRow {
                name: "Insert",
                vk: 0x2D,
                label: "Insert",
                text: None,
                shift: None,
            },
            KeyRow {
                name: "Home",
                vk: 0x24,
                label: "Home",
                text: None,
                shift: None,
            },
            KeyRow {
                name: "End",
                vk: 0x23,
                label: "End",
                text: None,
                shift: None,
            },
            KeyRow {
                name: "PageUp",
                vk: 0x21,
                label: "PageUp",
                text: None,
                shift: None,
            },
            KeyRow {
                name: "PageDown",
                vk: 0x22,
                label: "PageDown",
                text: None,
                shift: None,
            },
            KeyRow {
                name: "ArrowLeft",
                vk: 0x25,
                label: "ArrowLeft",
                text: None,
                shift: None,
            },
            KeyRow {
                name: "ArrowUp",
                vk: 0x26,
                label: "ArrowUp",
                text: None,
                shift: None,
            },
            KeyRow {
                name: "ArrowRight",
                vk: 0x27,
                label: "ArrowRight",
                text: None,
                shift: None,
            },
            KeyRow {
                name: "ArrowDown",
                vk: 0x28,
                label: "ArrowDown",
                text: None,
                shift: None,
            },
        ];
        // The four modifier keys are also pressable on their own (an
        // advanced caller's bare keyDown/keyup; Chromium's `key` names).
        #[rustfmt::skip]
        let modifier_rows = [
            ("Shift", 0x10u16), ("Control", 0x11), ("Alt", 0x12), ("Meta", 0x5B),
        ];
        for (name, vk) in modifier_rows {
            rows.push(KeyRow {
                name,
                vk,
                label: name,
                text: None,
                shift: None,
            });
        }
        // Letters (VK 0x41..): pages read `e.key` "a", shifted reads "A".
        for index in 0..26usize {
            let (lower, upper) = ((b'a' + index as u8) as char, (b'A' + index as u8) as char);
            let lower: String = lower.to_string();
            let upper: String = upper.to_string();
            let name: &'static str = Box::leak(lower.into_boxed_str());
            let shift: &'static str = Box::leak(upper.into_boxed_str());
            rows.push(KeyRow {
                name,
                vk: 0x41 + index as u16,
                label: name,
                text: Some(name),
                shift: Some(shift),
            });
        }
        // Digits (VK 0x30..): US shift row printed above them (Shift+0
        // is ')' ... Shift+9 is '(').
        let digit_shifts = [')', '!', '@', '#', '$', '%', '^', '&', '*', '('];
        for (index, shifted) in digit_shifts.into_iter().enumerate() {
            let name: String = char::from(b'0' + index as u8).to_string();
            let name: &'static str = Box::leak(name.into_boxed_str());
            let shift: String = shifted.to_string();
            let shift: &'static str = Box::leak(shift.into_boxed_str());
            rows.push(KeyRow {
                name,
                vk: 0x30 + index as u16,
                label: name,
                text: Some(name),
                shift: Some(shift),
            });
        }
        // F1..F12.
        for index in 0..12usize {
            let name: String = format!("F{}", index + 1);
            let name: &'static str = Box::leak(name.into_boxed_str());
            rows.push(KeyRow {
                name,
                vk: 0x70 + index as u16,
                label: name,
                text: None,
                shift: None,
            });
        }
        // US punctuation rows (name = the vocabulary id, label = the
        // unshifted character). Kept explicit: the VKs and shift partners
        // are desktop-layout facts, not derivable.
        #[rustfmt::skip]
        let punctuation = [
            ("Semicolon", 0xBAu16, ";", ":"),
            ("Equal", 0xBB, "=", "+"),
            ("Comma", 0xBC, ",", "<"),
            ("Minus", 0xBD, "-", "_"),
            ("Period", 0xBE, ".", ">"),
            ("Slash", 0xBF, "/", "?"),
            ("Backquote", 0xC0, "`", "~"),
            ("BracketLeft", 0xDB, "[", "{"),
            ("Backslash", 0xDC, "\\", "|"),
            ("BracketRight", 0xDD, "]", "}"),
            ("Quote", 0xDE, "'", "\""),
        ];
        for (name, vk, unshifted, shifted) in punctuation {
            rows.push(KeyRow {
                name,
                vk,
                label: unshifted,
                text: Some(unshifted),
                shift: Some(shifted),
            });
        }
        rows
    })
}

/// Resolves a single key-name part to its table row (usize index into
/// [`key_table`]) plus whether the part forces Shift (spelling a letter
/// or a shifted punctuation character directly, e.g. `K` or `%`).
fn key_row_lookup(part: &str) -> Result<(usize, bool), cmux_ipc::RequestProblem> {
    let table = key_table();
    let not_known = || {
        cmux_ipc::RequestProblem::invalid_params(format!(
            "params.key '{part}' is not a known key (named keys like Enter, Tab, Escape, \
             ArrowLeft, F1..F12; letters, digits and US punctuation characters; modifier \
             chains Shift+Control+Alt+Meta, e.g. 'Control+a')"
        ))
    };
    // Multi-character parts are names only.
    let mut chars = part.chars();
    if let (Some(single), None) = (chars.next(), chars.next()) {
        // A row whose name or unshifted label is that one character:
        // letters, digits, plain punctuation figures, the space key, or
        // '+' itself.
        if let Some(index) = table.iter().position(|row| {
            (row.name.len() == 1 && row.name.starts_with(single))
                || (row.label.len() == 1 && row.label.starts_with(single))
        }) {
            return Ok((index, false));
        }
        if let Some(index) = table
            .iter()
            .position(|row| row.name == "Space" && single.is_whitespace())
        {
            return Ok((index, false));
        }
        // Shifted punctuation characters (e.g. `%` = Shift+5, `+` = the
        // Equal row's shifted partner, `K` = Shift+k).
        let shifted_char: String = single.to_string();
        if let Some(index) = table
            .iter()
            .position(|row| row.shift == Some(shifted_char.as_str()))
        {
            return Ok((index, true));
        }
        return Err(not_known());
    }
    // The spelled names: alias first, then a case-insensitive row name.
    let lower = part.to_ascii_lowercase();
    let canonical = KEY_ALIASES
        .iter()
        .find(|(alias, _)| *alias == lower.as_str())
        .map(|(_, canonical)| *canonical)
        .unwrap_or(part);
    table
        .iter()
        .position(|row| row.name.eq_ignore_ascii_case(canonical))
        .map(|index| (index, false))
        .ok_or_else(not_known)
}

/// The validated key plan: Chromium's `key` label, the Windows VK, CDP's
/// modifier bits, and the resolved text. A plan is pure data; unit tests
/// lock the whole mapping.
#[derive(Clone, Debug, PartialEq)]
struct KeyPlan {
    key: String,
    vk: u16,
    modifiers: u8,
    text: Option<String>,
}

/// Resolves a caller key token (`Enter`, `a`, `Control+Shift+K`, `+`) to
/// its dispatch plan; unknown vocabulary is a structured `invalid_params`
/// listing the accepted families.
fn key_plan_of(token: &str) -> Result<KeyPlan, cmux_ipc::RequestProblem> {
    let trimmed = token.trim();
    // Three special spells disambiguated before splitting: the space key
    // itself (trim would eat it), and '+' (which is also the chain
    // separator, spelled through the Equal row instead).
    let token_parts: Vec<&str> = if trimmed.is_empty() {
        if token == " " {
            vec![" "]
        } else {
            return Err(cmux_ipc::RequestProblem::invalid_params(
                "params.key must not be empty",
            ));
        }
    } else if trimmed == "+" {
        vec!["+"]
    } else {
        trimmed.split('+').collect()
    };
    // split_last: the key part plus everything before it (the modifier
    // chain); an empty token cannot happen (trimmed is non-empty), but a
    // malformed split still answers the structured error.
    let Some((key_part, modifiers_parts)) = token_parts.split_last() else {
        return Err(cmux_ipc::RequestProblem::invalid_params(format!(
            "params.key '{token}' has an empty key part (spell the plus key as '+')"
        )));
    };
    let mut modifiers = 0u8;
    for part in modifiers_parts {
        let part = part.trim();
        match part.to_ascii_lowercase().as_str() {
            "shift" => modifiers |= KEY_MODIFIER_SHIFT,
            "control" | "ctrl" => modifiers |= KEY_MODIFIER_CONTROL,
            "alt" | "option" => modifiers |= KEY_MODIFIER_ALT,
            "meta" | "cmd" | "command" => modifiers |= KEY_MODIFIER_META,
            "" => {
                return Err(cmux_ipc::RequestProblem::invalid_params(format!(
                    "params.key '{token}' has an empty part (spell the plus key as '+')"
                )))
            }
            other => {
                return Err(cmux_ipc::RequestProblem::invalid_params(format!(
                    "params.key modifier '{other}' is not one of Shift, Control, Alt, Meta"
                )))
            }
        }
    }
    // Names tolerate slop; a single character is a literal figure (the
    // space key " " must never be trimmed away).
    let key_part = if key_part.len() > 1 {
        key_part.trim()
    } else {
        key_part
    };
    if key_part.is_empty() {
        return Err(cmux_ipc::RequestProblem::invalid_params(format!(
            "params.key '{token}' has an empty key part (spell the plus key as '+')"
        )));
    }
    let (row_index, extra_shift) = key_row_lookup(key_part)?;
    if extra_shift {
        modifiers |= KEY_MODIFIER_SHIFT;
    }
    let row = &key_table()[row_index];
    // Chromium-honest char synthesis: the text field rides only when no
    // Ctrl/Alt/Meta is engaged (their chars never land in the page) —
    // Shift alone picks the shifted character.
    let text = if modifiers & !KEY_MODIFIER_SHIFT != 0 {
        None
    } else if modifiers & KEY_MODIFIER_SHIFT != 0 {
        row.shift.or(row.text)
    } else {
        row.text
    };
    Ok(KeyPlan {
        key: row.label.to_owned(),
        vk: row.vk,
        modifiers,
        text: text.map(|text| text.to_owned()),
    })
}

/// The `Input.dispatchKeyEvent` event for one key half: a down event
/// carries the resolved text (`keyDown`) or is a bare raw event
/// (`rawKeyDown` — cursor/editing keys like Enter? no: Enter carries
/// text; Backspace/arrows are raw), the up half never carries text
/// (Chromium's protocol mapping as the automation layer uses it).
fn key_dispatch_params(plan: &KeyPlan, down: bool) -> serde_json::Value {
    let event_type = if !down {
        "keyUp"
    } else if plan.text.is_some() {
        "keyDown"
    } else {
        "rawKeyDown"
    };
    let mut event = serde_json::json!({
        "type": event_type,
        "modifiers": plan.modifiers,
        "key": plan.key,
        "windowsVirtualKeyCode": plan.vk,
    });
    if down {
        if let Some(text) = &plan.text {
            event["text"] = serde_json::json!(text);
        }
    }
    event
}

// ---------------------------------------------------------------------------
// M5.4 wait polling (browser.wait)
// ---------------------------------------------------------------------------

/// The `browser.wait` predicate set: every `Some` member must become true
/// (AND semantics; one member = the plain upstream wait). Slot order is
/// fixed — selector, text_contains, url_contains, load_state, function —
/// the same order [`wait_evaluate_js`] builds.
#[derive(Clone, Debug, PartialEq)]
struct WaitPoll {
    selector: Option<String>,
    text_contains: Option<String>,
    url_contains: Option<String>,
    load: Option<LoadState>,
    function: Option<String>,
}

/// The combined wait predicate evaluate: one expression evaluating every
/// given slot into a JSON array, in the documented order. Strings ride as
/// JSON literals (never code); the caller's JS function is embedded
/// verbatim — `browser.wait`'s `function` predicate is automation input
/// at the same trust level as `browser.eval` (the client's own page).
fn wait_evaluate_js(poll: &WaitPoll) -> String {
    let mut slots: Vec<String> = Vec::new();
    if let Some(selector) = &poll.selector {
        let literal = serde_json::to_string(selector).unwrap_or_else(|_| "''".to_owned());
        slots.push(format!("!!document.querySelector({literal})"));
    }
    if let Some(text) = &poll.text_contains {
        let literal = serde_json::to_string(text).unwrap_or_else(|_| "''".to_owned());
        slots.push(format!(
            "String((document.body || document.documentElement).textContent).indexOf({literal}) !== -1"
        ));
    }
    if let Some(url) = &poll.url_contains {
        let literal = serde_json::to_string(url).unwrap_or_else(|_| "''".to_owned());
        slots.push(format!("location.href.indexOf({literal}) !== -1"));
    }
    if let Some(state) = &poll.load {
        let expression = match state {
            LoadState::Load => "document.readyState === 'complete'",
            LoadState::DomContentLoaded => "document.readyState !== 'loading'",
        };
        slots.push(expression.to_owned());
    }
    if let Some(function) = &poll.function {
        slots.push(format!("!!({function})"));
    }
    format!("(function () {{ return [{}]; }})()", slots.join(", "))
}

/// JS truthiness over the JSON-backed values the poll evaluate returns
/// (the `function` slot): `null`, `false`, `0`, `""` are not yet; objects
/// and arrays are always truthy.
fn value_is_truthy(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Bool(flag) => *flag,
        serde_json::Value::Null => false,
        serde_json::Value::Number(number) => number.as_f64().is_some_and(|value| value != 0.0),
        serde_json::Value::String(text) => !text.is_empty(),
        _ => true,
    }
}

/// One predicate slot: the strict slots (selector/text/url/load_state)
/// must be boolean, the caller-function slot is truthy-tested like JS.
/// A missing or non-boolean strict slot means the page reply was
/// malformed (never silently forever-false).
fn wait_slot_truthy(
    slots: &[serde_json::Value],
    index: usize,
    what: &'static str,
    strict: bool,
) -> Result<bool, cmux_ipc::RequestProblem> {
    let slot = slots.get(index).ok_or_else(|| {
        cmux_ipc::RequestProblem::invalid_state(format!(
            "the wait predicate reply was malformed (missing the {what} slot)"
        ))
    })?;
    match slot {
        serde_json::Value::Bool(flag) => Ok(*flag),
        serde_json::Value::Null => {
            if strict {
                Err(cmux_ipc::RequestProblem::invalid_state(format!(
                    "the wait predicate reply was malformed (the {what} slot was absent)"
                )))
            } else {
                Ok(false)
            }
        }
        _ if !strict => Ok(value_is_truthy(slot)),
        _ => Err(cmux_ipc::RequestProblem::invalid_state(format!(
            "the wait predicate reply was malformed (the {what} slot was not a boolean)"
        ))),
    }
}

/// Reads one completed wait-poll evaluate: `Ok(true)` when every given
/// predicate holds; `Ok(false)` keeps polling; a malformed reply (and a
/// slot-count mismatch — the defensive tail) fails the wait fast.
fn parse_wait_slots(
    raw: &serde_json::Value,
    poll: &WaitPoll,
) -> Result<bool, cmux_ipc::RequestProblem> {
    let value = evaluate_value_of(raw);
    let slots = value.as_array().ok_or_else(|| {
        cmux_ipc::RequestProblem::invalid_state(
            "the wait predicate reply was malformed (expected the slots array)".to_owned(),
        )
    })?;
    let malformed = || {
        cmux_ipc::RequestProblem::invalid_state(
            "the wait predicate reply was malformed (slot count mismatch)".to_owned(),
        )
    };
    let mut index = 0usize;
    let mut all_true = true;
    if poll.selector.is_some() {
        if !wait_slot_truthy(slots, index, "selector", true)? {
            all_true = false;
        }
        index += 1;
    }
    if poll.text_contains.is_some() {
        if !wait_slot_truthy(slots, index, "text_contains", true)? {
            all_true = false;
        }
        index += 1;
    }
    if poll.url_contains.is_some() {
        if !wait_slot_truthy(slots, index, "url_contains", true)? {
            all_true = false;
        }
        index += 1;
    }
    if poll.load.is_some() {
        if !wait_slot_truthy(slots, index, "load_state", true)? {
            all_true = false;
        }
        index += 1;
    }
    if poll.function.is_some() {
        if !wait_slot_truthy(slots, index, "function", false)? {
            all_true = false;
        }
        index += 1;
    }
    if index != slots.len() {
        return Err(malformed());
    }
    Ok(all_true)
}

/// Builds the one-evaluate expression for an M5.3 element verb (the
/// `prepare_cdp_call` dispatch; unit-tested for JSON-literal embedding).
fn element_expression_of(plan: &ElementJs, selector: &str) -> String {
    match plan {
        ElementJs::Fill { text } => fill_evaluate_js(selector, text),
        ElementJs::Focus => focus_evaluate_js(selector),
        ElementJs::Check { checked } => check_evaluate_js(selector, *checked),
        ElementJs::SelectOption { value } => select_evaluate_js(selector, value),
        ElementJs::ScrollIntoView => scroll_into_view_evaluate_js(selector),
        ElementJs::GetDom {
            field: GetField::Count,
            ..
        } => count_evaluate_js(selector),
        ElementJs::GetDom {
            field,
            attr,
            property,
        } => dom_field_evaluate_js(selector, *field, attr.as_deref(), property.as_deref()),
        ElementJs::IsState { state } => is_state_evaluate_js(selector, *state),
    }
}

/// Parses one M5.3 action evaluate (fill/focus/check/uncheck/select/
/// scroll-into-view): `true` = ok, the not-a-target sentinel = a
/// structured `invalid_state`, any other value = the element is gone.
fn parse_element_action(
    raw: &serde_json::Value,
    plan: &ElementJs,
) -> Result<(), cmux_ipc::RequestProblem> {
    match evaluate_value_of(raw) {
        serde_json::Value::Bool(true) => Ok(()),
        serde_json::Value::String(sentinel) => match (sentinel.as_str(), plan) {
            ("not-checkbox", _) => Err(cmux_ipc::RequestProblem::invalid_state(
                "the referenced element is not a checkbox or radio input".to_owned(),
            )),
            ("not-select", _) => Err(cmux_ipc::RequestProblem::invalid_state(
                "the referenced element is not a <select>".to_owned(),
            )),
            ("no-option", _) => Err(cmux_ipc::RequestProblem::invalid_state(
                "no <option> matches the requested value".to_owned(),
            )),
            _ => Err(cmux_ipc::RequestProblem::invalid_state(
                "the referenced element is gone (snapshot again)".to_owned(),
            )),
        },
        _ => Err(cmux_ipc::RequestProblem::invalid_state(
            "the referenced element is gone (snapshot again)".to_owned(),
        )),
    }
}

/// Parses one M5.3 state probe: the JS returns null when the element is
/// gone (read as `false`, upstream contrast-free semantics) or the probe
/// object keyed by the state.
fn parse_is_state(raw: &serde_json::Value, state: IsState) -> bool {
    let value = evaluate_value_of(raw);
    let key = match state {
        IsState::Visible => "visible",
        IsState::Enabled => "enabled",
        IsState::Checked => "checked",
    };
    value
        .get(key)
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

/// `browser.scroll` reply offsets (`scrollX`/`scrollY`); absent members
/// read as 0 (a plain non-scrolling page reports 0 too).
fn parse_scroll_reply(raw: &serde_json::Value) -> (f64, f64) {
    let value = evaluate_value_of(raw);
    (
        value
            .get("x")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0),
        value
            .get("y")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0),
    )
}

/// The `Page.captureScreenshot` base64 payload, decoded; a reply without
/// `data` is a structured `invalid_state` (the protocol answered but gave
/// nothing to write).
fn screenshot_bytes_of(raw: &serde_json::Value) -> Result<Vec<u8>, cmux_ipc::RequestProblem> {
    use base64::Engine as _;
    let data = raw
        .get("data")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            cmux_ipc::RequestProblem::invalid_state(
                "the screenshot reply carried no PNG data".to_owned(),
            )
        })?;
    base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|error| {
            cmux_ipc::RequestProblem::invalid_state(format!(
                "the screenshot reply was not valid base64: {error}"
            ))
        })
}

/// The screenshot staging directory (upstream persists page PNGs the app
/// side; `%TEMP%` keeps them out of the app's own appdata home).
fn screenshot_dir() -> std::path::PathBuf {
    std::env::temp_dir().join("cmux-screenshots")
}

/// The surface-id fragment of a screenshot filename: the ids are port
/// minted (`pane-<n>`); anything unexpected collapses to `pane` so a
/// hostile-looking id can never shape the path.
fn screenshot_surface_fragment(surface_id: &str) -> String {
    let clean: String = surface_id
        .chars()
        .take(40)
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if clean.is_empty() {
        "pane".to_owned()
    } else {
        clean
    }
}

/// The lean M5.3 action reply: one `surface_id` plus the consumed target
/// echo (`{"ref": …}` or `{"selector": …}` — whatever the caller sent).
fn lean_target_reply(surface_id: &str, target: &ElementTarget) -> serde_json::Value {
    let mut reply = serde_json::json!({ "surface_id": surface_id });
    if let (Some(fields), Some(echoes)) = (reply.as_object_mut(), target.target_json().as_object())
    {
        for (key, value) in echoes {
            fields.insert(key.clone(), value.clone());
        }
    }
    reply
}

/// Decodes a completed CDP outcome. Malformed JSON and protocol-reported
/// exceptions (`Runtime.evaluate`'s exceptionDetails) surface as
/// structured problems; otherwise the raw method-response object returns.
fn decode_cdp_outcome(
    outcome: &Result<String, String>,
) -> Result<serde_json::Value, cmux_ipc::RequestProblem> {
    let text = match outcome {
        Ok(text) => text,
        Err(error) => {
            return Err(cmux_ipc::RequestProblem::invalid_state(format!(
                "browser cdp call failed: {error}"
            )))
        }
    };
    let raw: serde_json::Value = serde_json::from_str(text).map_err(|error| {
        cmux_ipc::RequestProblem::invalid_state(format!(
            "browser cdp reply was not valid JSON: {error}"
        ))
    })?;
    if let Some(details) = raw.get("exceptionDetails") {
        let message = details
            .get("exception")
            .and_then(|exception| exception.get("description"))
            .and_then(serde_json::Value::as_str)
            .filter(|text| !text.is_empty())
            .or_else(|| details.get("text").and_then(serde_json::Value::as_str))
            .unwrap_or("page evaluation failed");
        return Err(cmux_ipc::RequestProblem::invalid_state(format!(
            "browser eval failed: {message}"
        )));
    }
    Ok(raw)
}

/// The JavaScript value of a `Runtime.evaluate` response (the protocol
/// nests it under `result.value`).
fn evaluate_value_of(raw: &serde_json::Value) -> serde_json::Value {
    raw.get("result")
        .and_then(|result| result.get("value"))
        .cloned()
        .unwrap_or(serde_json::Value::Null)
}

/// The fresh center (CSS pixels) from the center evaluate; a miss means
/// the element vanished since the snapshot (stale ref semantics).
fn parse_center_cdp(raw: &serde_json::Value) -> Result<(f64, f64), cmux_ipc::RequestProblem> {
    let value = evaluate_value_of(raw);
    let x = value.get("x").and_then(serde_json::Value::as_f64);
    let y = value.get("y").and_then(serde_json::Value::as_f64);
    match (x, y) {
        (Some(x), Some(y)) => Ok((x, y)),
        _ => Err(cmux_ipc::RequestProblem::invalid_state(
            "the referenced element has no box any more (it changed since the snapshot; snapshot again)"
                .to_owned(),
        )),
    }
}

/// Which automation verb an app request executes, or `None` for the
/// synchronous verbs.
fn cdp_kind_of(request: &cmux_ipc::AppRequest) -> Option<(Option<String>, CdpKind)> {
    use cmux_ipc::AppRequest as Req;
    match request {
        Req::BrowserSnapshot { surface_id } => Some((surface_id.clone(), CdpKind::Snapshot)),
        Req::BrowserEval {
            surface_id,
            expression,
        } => Some((
            surface_id.clone(),
            CdpKind::Eval {
                expression: expression.clone(),
            },
        )),
        Req::BrowserClick { surface_id, target } => Some((
            surface_id.clone(),
            CdpKind::ClickMouse {
                target: target.clone(),
                click_count: 1,
            },
        )),
        Req::BrowserDblclick { surface_id, target } => Some((
            surface_id.clone(),
            CdpKind::ClickMouse {
                target: target.clone(),
                click_count: 2,
            },
        )),
        Req::BrowserType {
            surface_id,
            target,
            text,
        } => Some((
            surface_id.clone(),
            CdpKind::TypeText {
                target: target.clone(),
                text: text.clone(),
            },
        )),
        // M5.3: the deferred-element family (one evaluate each).
        Req::BrowserFill {
            surface_id,
            target,
            text,
        } => Some((
            surface_id.clone(),
            CdpKind::ElementJs {
                target: target.clone(),
                js: ElementJs::Fill { text: text.clone() },
            },
        )),
        Req::BrowserFocus { surface_id, target } => Some((
            surface_id.clone(),
            CdpKind::ElementJs {
                target: target.clone(),
                js: ElementJs::Focus,
            },
        )),
        Req::BrowserCheck {
            surface_id,
            target,
            checked,
        } => Some((
            surface_id.clone(),
            CdpKind::ElementJs {
                target: target.clone(),
                js: ElementJs::Check { checked: *checked },
            },
        )),
        Req::BrowserSelect {
            surface_id,
            target,
            value,
        } => Some((
            surface_id.clone(),
            CdpKind::ElementJs {
                target: target.clone(),
                js: ElementJs::SelectOption {
                    value: value.clone(),
                },
            },
        )),
        Req::BrowserScrollIntoView { surface_id, target } => Some((
            surface_id.clone(),
            CdpKind::ElementJs {
                target: target.clone(),
                js: ElementJs::ScrollIntoView,
            },
        )),
        Req::BrowserHover { surface_id, target } => Some((
            surface_id.clone(),
            CdpKind::HoverMouse {
                target: target.clone(),
            },
        )),
        Req::BrowserGet {
            surface_id,
            target,
            field,
            attr,
            property,
        } => Some((
            surface_id.clone(),
            CdpKind::ElementJs {
                target: target.clone(),
                js: ElementJs::GetDom {
                    field: *field,
                    attr: attr.clone(),
                    property: property.clone(),
                },
            },
        )),
        Req::BrowserIs {
            surface_id,
            target,
            state,
        } => Some((
            surface_id.clone(),
            CdpKind::ElementJs {
                target: target.clone(),
                js: ElementJs::IsState { state: *state },
            },
        )),
        Req::BrowserScroll { surface_id, dx, dy } => Some((
            surface_id.clone(),
            CdpKind::ScrollBy {
                dx: dx.unwrap_or(0.0),
                dy: dy.unwrap_or(0.0),
            },
        )),
        Req::BrowserScreenshot { surface_id } => Some((surface_id.clone(), CdpKind::Screenshot)),
        // M5.4: the key family and the wait family. The key token is
        // validated in prepare (its plan build can fail); the wait rides
        // its predicate set plus the deadline size.
        Req::BrowserKey {
            surface_id,
            half,
            key,
        } => Some((
            surface_id.clone(),
            CdpKind::Key {
                half: *half,
                token: key.clone(),
            },
        )),
        Req::BrowserWait {
            surface_id,
            selector,
            text_contains,
            url_contains,
            load_state,
            function,
            timeout_ms,
        } => Some((
            surface_id.clone(),
            CdpKind::Wait {
                poll: WaitPoll {
                    selector: selector.clone(),
                    text_contains: text_contains.clone(),
                    url_contains: url_contains.clone(),
                    load: *load_state,
                    function: function.clone(),
                },
                timeout_ms: *timeout_ms,
            },
        )),
        _ => None,
    }
}

/// Answers a pending automation call; a closed reply window (the server
/// thread already sent its structured timeout) only logs.
fn reply_cdp(pending: &PendingCdp, result: Result<serde_json::Value, cmux_ipc::RequestProblem>) {
    if pending.reply.send(result).is_err() {
        eprintln!(
            "cmux-shell: cdp call {} reply discarded (reply window closed)",
            pending.surface_id
        );
    }
}

/// Pane border rounding (tuned per-theme at the config/theme milestone).
const PANE_ROUNDING: u8 = 4;

/// Workspace strip geometry (provisional slice values; upstream's sidebar
/// is user-resizable and themed â€” resize/theming arrive later).
/// The design aside (node `1:399`) is 256 wide at x=0
/// (`figma-design-reference.md` section 4); the 216 default was a legacy
/// value — corrected to the design width in design-fidelity round 1
/// (2026-10-26).
const SIDEBAR_WIDTH: f32 = 256.0;
/// The strip never exceeds this share of the window.
const SIDEBAR_MAX_FRACTION: f32 = 0.4;
/// Height of one workspace row in the strip.
const SIDEBAR_ROW_HEIGHT: f32 = 30.0;

/// Actions queued by keybindings, the sidebar, or smoke scheduling;
/// drained per frame.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Action {
    SplitFocused(SplitDirection),
    CloseFocused,
    /// The sidebar's per-tab close button (`at` = absolute workspace
    /// index at click time).
    CloseWorkspace {
        at: usize,
    },
    /// Keyboard `closeTab` (upstream âŒ˜W): closes the active workspace.
    CloseActiveWorkspace,
    /// New workspace tab (upstream `newTab` â€” a tab IS a workspace).
    NewWorkspace,
    /// Absolute workspace index (the sidebar click; digits map 1â†’0).
    SelectWorkspace(usize),
    CycleWorkspace {
        forward: bool,
    },
    ToggleSidebar,
    Cycle {
        forward: bool,
    },
    /// Equalize every split in the window (config id `equalizeSplits`).
    EqualizeAll,
    /// Nudge the focused pane's controlling divider one `resizeStep`
    /// (config ids `resizePane{Left,Right,Up,Down}`).
    Resize(ResizeDirection),
    /// Design lane 3 (row 13, node 1:512): open/advance the command
    /// palette — the mid-row bar's Ctrl+K path (default stroke
    /// `ctrl+k`, config id `commandPalette`).
    CommandPalette,
    /// Sidebar drag-reorder commit (owner round 9): move the workspace
    /// at `from` so it lands at index `to` in the reordered list. Both
    /// indices are paint-frame snapshots; the drain applies them after
    /// painting, mirroring the select/close rule.
    ReorderWorkspace {
        from: usize,
        to: usize,
    },
    /// Keyboard reorder of the ACTIVE workspace (upstream action ids
    /// `moveWorkspaceUp`/`moveWorkspaceDown`; unbound by default,
    /// rebindable like `equalizeSplits`).
    MoveActiveWorkspace {
        up: bool,
    },
}

/// The shell command for this run: the configured `terminal.shell` if set,
/// else the Windows default; a one-shot `cmd /c exit` under
/// `CMUX_SHELL_SPAWN_ONESHOT=1` takes precedence so the automated smoke
/// stays deterministic.
fn shell_spec(settings: &RuntimeSettings) -> PtySpec {
    if std::env::var("CMUX_SHELL_SPAWN_ONESHOT").as_deref() == Ok("1") {
        return PtySpec::new("cmd.exe").arg("/c").arg("exit");
    }
    let base = match &settings.shell {
        Some(shell) => {
            let mut spec = PtySpec::new(shell.program.as_str());
            for arg in &shell.args {
                spec = spec.arg(arg.as_str());
            }
            spec
        }
        None => PtySpec::windows_default_shell(),
    };
    // No forced `TERM`/`COLORTERM`: PowerShell's host treats a set `TERM`
    // as a raw-VT-input signal, which breaks Ctrl+C semantics in
    // PSReadLine under ConPTY (M4.3 evidence — Windows Terminal parity is
    // the no-TERM behavior: interrupt + `^C` echo).
    base
}

/// The M6 SSH pane command (ADR-0014): the Windows OpenSSH client with
/// OpenSSH-standard argv ordering — port/identity/config options before
/// the destination, the remote command after (OpenSSH joins trailing
/// argv into the remote shell line, so one joined remote string is
/// equivalent to upstream's joined `initial_command` handoff). No forced
/// TTY flags: this is upstream's plain-interactive fallback shape.
fn ssh_spec(
    destination: &str,
    port: Option<u16>,
    identity_file: Option<&str>,
    options: &[String],
    initial_command: Option<&str>,
) -> PtySpec {
    let mut spec = PtySpec::new("ssh");
    if let Some(port) = port {
        spec = spec.arg("-p").arg(port.to_string());
    }
    if let Some(identity_file) = identity_file {
        spec = spec.arg("-i").arg(identity_file);
    }
    for option in options {
        spec = spec.arg("-o").arg(option.as_str());
    }
    spec = spec.arg(destination);
    if let Some(initial_command) = initial_command {
        spec = spec.arg(initial_command);
    }
    spec
}
/// Injects the hook-identity environment into a pane spec (M3.3 slice A):
/// `CMUX_SURFACE_ID` (the pane id), `CMUX_WORKSPACE_ID`, and the control
/// pipe the pane's shell (and agents it runs) can address. When IPC is
/// disabled (`ipc_pipe` empty) the extras are skipped.
fn identity_spec(spec: PtySpec, pane_id: &str, workspace_index: usize, ipc_pipe: &str) -> PtySpec {
    let disable = std::env::var("CMUX_CLAUDE_HOOKS_DISABLED").ok();
    let pane_path = std::env::var("PATH").ok();
    let shim = claude_shim_for_panes(ipc_pipe.is_empty(), disable.as_deref());
    identity_spec_with(
        spec,
        pane_id,
        workspace_index,
        ipc_pipe,
        disable.as_deref(),
        pane_path.as_deref(),
        shim.as_ref(),
    )
}

/// The pure identity injection: the routed surface/workspace ids, the pipe
/// when IPC is on, the claude-hooks disable value when the environment
/// carries one (tested without process-global env mutation), and â€” for
/// enabled panes â€” the PATH shim slot for typed claude (M3.3c).
fn identity_spec_with(
    spec: PtySpec,
    pane_id: &str,
    workspace_index: usize,
    ipc_pipe: &str,
    claude_hooks_disabled: Option<&str>,
    pane_path: Option<&str>,
    shim: Option<&ClaudeShim>,
) -> PtySpec {
    let mut spec = spec
        .env("CMUX_SURFACE_ID", pane_id)
        .env("CMUX_WORKSPACE_ID", format!("ws-{}", workspace_index + 1));
    if !ipc_pipe.is_empty() {
        spec = spec.env("CMUX_SOCKET_PATH", ipc_pipe);
    }
    // Upstream's per-agent toggle arrives on the app's environment (the
    // port has no per-agent settings UI yet); forwarding it makes the
    // spawn gates and the in-pane hook commands observe the same value
    // (upstream `setManagedEnvironmentValue`).
    if let Some(value) = claude_hooks_disabled.filter(|v| !v.trim().is_empty()) {
        spec = spec.env("CMUX_CLAUDE_HOOKS_DISABLED", value);
    }
    if let Some(shim) = shim {
        // Typed-claude transport (M3.3c): the shim dir leads the pane
        // PATH, so `claude` typed in any pane shell resolves to the shim
        // and reaches the `hooks claude launch` verb (upstream's
        // `agentCommandShims` PATH approach).
        spec = spec.env(
            "PATH",
            cmux_hooks::claude::path_with_shim(pane_path, &shim.dir),
        );
    } else if let Some(path) = pane_path.map(str::trim).filter(|v| !v.is_empty()) {
        // The child env's PATH entry is composed from the default env
        // block on Windows when un-overridden (verified in the M3.3c
        // smoke: app-side PATH mutations do not reach panes without this),
        // so the pane PATH is always routed explicitly to the app's live
        // PATH. Other variables propagate unmodified.
        spec = spec.env("PATH", path);
    }
    spec
}

/// Whether `pane_id` is the focused surface of the workspace at
/// `workspace_index`. A regression guard (M3.3): "focused" is NOT "any
/// leaf in the active workspace" â€” the comparison runs against that
/// workspace's focused pane id, which the bug-fix tests lock down.
fn surface_focused(
    workspace_focused: &str,
    pane_id: &str,
    workspace_index: usize,
    active_workspace: usize,
) -> bool {
    workspace_index == active_workspace && workspace_focused == pane_id
}

/// M3.3b: when a pane's program is the claude CLI *by explicit path*, spawn
/// it attached to a generated settings document (`claude --settings <file>`)
/// â€” the port's spawn-time form of upstream's wrapper transport. A *bare*
/// `claude` program is treated as a PATH resolution and left to the typed-
/// claude shim (M3.3c), which delivers the same doc â€” injecting here too
/// would stack two `--settings` flags.
///
/// Eligibility (see [`claude_injection_eligible`]) keeps plain-claude
/// behavior everywhere the doc would be inert: IPC off, the upstream
/// disable variable, a restored pane that already carries `--settings`
/// (its spec replays with the still-valid content-addressed doc), and a
/// missing CLI executable beside this app â€” the port does not bundle a
/// CLI yet, so the doc's hook commands need that sibling. Write failures
/// degrade to plain claude with a log line; nothing ever blocks or crashes
/// the spawn for an integration's sake.
fn maybe_inject_claude_settings(spec: PtySpec) -> PtySpec {
    if !claude_injection_eligible(&spec) {
        return spec;
    }
    let (Some(cli), Some(dir)) = (cli_exe_for_pane_docs(), cmux_hooks::claude::settings_dir())
    else {
        eprintln!(
            "cmux-shell: cannot resolve the CLI or claude-settings location; spawning claude without hooks"
        );
        return spec;
    };
    match cmux_hooks::claude::write_settings_file(&cli, &dir) {
        Ok(path) => {
            eprintln!("cmux-shell: injecting claude hooks via {}", path.display());
            spec.arg("--settings")
                .arg(path.to_string_lossy().into_owned())
        }
        Err(error) => {
            eprintln!(
                "cmux-shell: writing the claude settings doc failed ({error}); spawning claude without hooks"
            );
            spec
        }
    }
}

/// The pure-logic half of the injection gate. The program must be the
/// claude CLI, the pane must carry the control pipe (identity
/// injection already happened â€” an IPC-off pane gets no doc), the upstream
/// per-agent toggle `CMUX_CLAUDE_HOOKS_DISABLED=1` suppresses the feature
/// exactly like upstream's shell guard (`!= 1` semantics), and a spec that
/// already carries `--settings` is left alone.
fn claude_injection_eligible(spec: &PtySpec) -> bool {
    if !cmux_hooks::claude::is_claude_program(spec.program()) {
        return false;
    }
    // A bare `claude` (no directory) is a PATH resolution: the typed-claude
    // shim covers it (M3.3c). Only explicit paths are injected at spawn.
    let explicit_path = std::path::Path::new(spec.program())
        .parent()
        .is_some_and(|parent| !parent.as_os_str().is_empty());
    if !explicit_path {
        return false;
    }
    if spec.env_value("CMUX_SOCKET_PATH").is_none() {
        return false;
    }
    if disabled_claude_hooks_value(spec.env_value("CMUX_CLAUDE_HOOKS_DISABLED")) {
        return false;
    }
    !spec.args().iter().any(|arg| arg == "--settings")
}

/// The shared disable-var semantics for every claude-hooks gate (the CLI's
/// `env_trimmed` contract): `1` after trimming disables, anything else is on.
fn disabled_claude_hooks_value(value: Option<&str>) -> bool {
    value.map(str::trim) == Some("1")
}

/// The CLI executable the doc's hook commands point at: the sibling
/// `cmux.exe` of this app (the port has no bundled-CLI channel yet; dev
/// builds put both exes in the same target dir).
fn cli_exe_for_pane_docs() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let candidate = exe.parent()?.join("cmux.exe");
    candidate.is_file().then_some(candidate)
}

/// The PATH shim for *typed* claude (M3.3c): one shared `claude.cmd` that
/// delegates to `cmux.exe hooks claude launch` (one dir, not upstream's
/// per-surface uuid dirs â€” the port's pane ids are deterministic, so a
/// shared dir needs no lifecycle management; divergence documented).
struct ClaudeShim {
    dir: std::path::PathBuf,
    #[allow(dead_code)]
    cmux_bin: std::path::PathBuf,
}

/// Builds and installs the pane shim, applying the same gates as the
/// spawn-time doc injection: IPC on, disable var not set, CLI beside the
/// app resolvable. Any failure means panes keep their plain PATH (typed
/// claude stays plain) with a log line.
fn claude_shim_for_panes(ipc_off: bool, disabled: Option<&str>) -> Option<ClaudeShim> {
    if ipc_off || disabled_claude_hooks_value(disabled) {
        return None;
    }
    let cmux_bin = cli_exe_for_pane_docs()?;
    let dir = cmux_hooks::claude::shim_dir(&cmux_hooks::claude::settings_dir()?);
    if let Err(error) = cmux_hooks::claude::install_shim(&dir, &cmux_bin) {
        eprintln!("cmux-shell: claude shim install failed ({error}); typed claude stays plain");
        return None;
    }
    Some(ClaudeShim { dir, cmux_bin })
}

/// How an admitted hook subcommand surfaces (M3.3b). Upstream delivers
/// lifecycle/attention events through the notification policy and keeps
/// the tool-loop feed route in the agent journal (no ring, no banner); the
/// port mirrors that split until the journal feed ships: `feed` and
/// `pre-tool-use` log their disposition but touch nothing visual. An
/// unrecognized subcommand stays deliverable â€” it is by definition rare.
fn subcommand_is_deliverable(subcommand: &str) -> bool {
    !matches!(subcommand, "feed" | "pre-tool-use")
}

/// A turn boundary awaiting its dispatch debounce, tracked pane-side
/// between the detector's announcement and the frame that delivers it.
struct PendingTurn {
    pending: TurnPending,
    deadline: Instant,
    /// The pane's last visible terminal rows, frozen when the boundary
    /// was seen — the active card's inline block (lane 5). Freezing at
    /// capture keeps the paint pass read-only and flicker-free; a
    /// superseding boundary re-captures.
    tail: Vec<String>,
}

/// One pane: its tab identity, laid-out rectangle, and the body it drives —
/// a PTY terminal (M0+) or a WebView2 browser surface (M5.1).
/// `panel_ids` keeps the leaf's full restored tab list (the strip
/// itself renders from M2.4b) so a save round-trips panels the UI cannot
/// show yet; it always contains `id` (that pane is the visible one).
struct Pane {
    id: String,
    /// Tab membership of the leaf (visible one = `id`); `[id]` after a
    /// fresh spawn.
    panel_ids: Vec<String>,
    frame: PixelRect,
    body: PaneBody,
    /// An app-known display title: authoritative for panes the app
    /// titled itself (M6: SSH destinations, `--name`) — a child-set OSC
    /// title is only the no-override fallback (Windows ssh.exe titles
    /// itself with its image path before any connection). `None`
    /// everywhere else.
    title_override: Option<String>,
    /// Lane 4 (row 12): the spawn's tree-leader pid, the membership root
    /// the services sampler walks (None for browser panes and when
    /// portable-pty could not report a pid).
    tree_pid: Option<u32>,
}

/// The per-pane lane: terminal PTY plumbing or the deferred browser host.
enum PaneBody {
    Terminal(Box<TerminalLane>),
    Browser(PaneBrowser),
    /// Design lane 5: the agent activity stream (the design pane 1:244's
    /// card surface). It has no child process — it renders its
    /// workspace's retained agent events; the lane carries view state
    /// only. Closes by user action, like a browser pane.
    Agent(AgentActivityPane),
}

/// Everything a PTY-backed terminal pane keeps alive.
struct TerminalLane {
    terminal: Terminal,
    status_receiver: Option<mpsc::Receiver<WatchedStatus>>,
    /// Shell events arriving from this pane (M3.1 OSC notifications
    /// and M3.4 turn boundaries); drained per frame into the shared
    /// arrival policy.
    notify_receiver: Option<mpsc::Receiver<Vec<PaneEvent>>>,
    /// M3.4: turn boundaries awaiting their debounce, keyed by agent
    /// id (one slot per agent; a newer identifier replaces an older
    /// one).
    pending_turns: HashMap<&'static str, PendingTurn>,
    /// Highest turn identifier delivered per agent — the exactly-once
    /// high-water mark.
    delivered_turns: HashMap<&'static str, u64>,
}

/// The browser lane: URL, host lifecycle, and the polled snapshot.
struct PaneBrowser {
    /// Desired first page; queued into the host at wire time.
    start_url: String,
    /// The WebView2 host, created on the frame thread once the app
    /// window exists (restore plus IPC creation happen before any
    /// such window is guaranteed).
    host: Option<cmux_webview2::BrowserHost>,
    /// Latest polled snapshot for rows/titles/identify.
    snapshot: cmux_webview2::BrowserSnapshot,
    phase: cmux_webview2::BrowserPhase,
    /// Failure text mirrored from the host (drives the in-pane overlay).
    failure: Option<String>,
    /// The snapshot's element refs (`@eN` → CSS selector), surface-local
    /// and replaced by every snapshot; any navigation clears them (the
    /// upstream semantics: refs are ephemeral).
    refs: Vec<BrowserRef>,
}

/// The agent activity stream pane's view state (design lane 5). Event
/// data lives in the workspace's `agent_cards::CardStore` slot; the lane
/// carries only per-pane view state (scroll anchoring).
struct AgentActivityPane {
    /// How far the stream viewport sits above its bottom anchor, in
    /// points. 0 pins the newest card at the stream's bottom edge; wheel
    /// input raises it and painting clamps to the content height.
    scroll_from_bottom: f32,
}

/// One `browser.snapshot` ref: the canonical `@eN` id and the CSS selector
/// that re-resolves the element (unique chain, built by the walker).
#[derive(Clone, Debug, PartialEq)]
struct BrowserRef {
    id: String,
    selector: String,
}

impl PaneBody {
    fn is_browser(&self) -> bool {
        matches!(self, PaneBody::Browser(_))
    }
}

/// Folds a child OSC title into a friendly pane title (Windows
/// adaptation, recorded in the port docs): Windows PowerShell 5.1 titles
/// its console with the executable image path
/// (`C:\WINDOWS\system32\WindowsPowerShell\v1.0\powershell.exe` — the
/// same OS behavior that drives Windows Terminal's bare-path titles),
/// and that path overflowed every title consumer: the breadcrumb chip,
/// the sidebar row, and the status-bar marker run (which collided with
/// the status bar's right cluster; the marker run retired in owner
/// round 8, the fold remains for the chip and the sidebar row). A title
/// that parses as an absolute
/// Windows path (drive-letter or UNC) whose LAST segment ends in `.exe`
/// (case-insensitive) folds to the image stem; every other title passes
/// through untouched. App-known titles (`title_override` — an SSH
/// destination or `--name`) never reach this fold: they are pane
/// identities, not child chatter.
fn friendly_osc_title(title: &str) -> String {
    let bytes = title.as_bytes();
    let drive_absolute = bytes.len() >= 3
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
        && bytes[0].is_ascii_alphabetic();
    if !(drive_absolute || title.starts_with("\\\\")) {
        return title.to_owned();
    }
    let last_segment = title.rsplit(['\\', '/']).next().unwrap_or("");
    let cut = last_segment.len().checked_sub(4);
    match cut {
        Some(at) if last_segment[at..].eq_ignore_ascii_case(".exe") => {
            let stem = &last_segment[..at];
            if stem.is_empty() {
                title.to_owned()
            } else {
                stem.to_owned()
            }
        }
        _ => title.to_owned(),
    }
}

impl Pane {
    fn terminal_mut(&mut self) -> Option<&mut Terminal> {
        match &mut self.body {
            PaneBody::Terminal(terminal_lane) => Some(&mut terminal_lane.terminal),
            PaneBody::Browser(_) => None,
            PaneBody::Agent(_) => None,
        }
    }

    /// A row/sidebar/identify title: an app-known title (SSH destination,
    /// `--name`) is the pane's identity and wins; a child-set OSC title is
    /// only the no-override fallback (Windows ssh.exe titles itself with
    /// its image path before any connection, which would otherwise mask
    /// the destination — M6 lane evidence 2026-10-23), and it folds a
    /// Windows image-path title to its `.exe` stem via
    /// [`friendly_osc_title`] (design round 1, 2026-10-26). Browser panes
    /// show the page title once loaded.
    fn display_title(&self) -> Option<String> {
        match &self.body {
            PaneBody::Terminal(terminal_lane) => self.title_override.clone().or_else(|| {
                terminal_lane
                    .terminal
                    .title()
                    .map(|title| friendly_osc_title(&title))
            }),
            PaneBody::Browser(browser_lane) => {
                let title = browser_lane.snapshot.title.trim();
                (!title.is_empty()).then(|| title.to_owned())
            }
            PaneBody::Agent(_) => Some("Agent activity".to_owned()),
        }
    }

    /// The browser lane when this pane drives a webview.
    fn browser_lane_mut(&mut self) -> Option<&mut PaneBrowser> {
        match &mut self.body {
            PaneBody::Browser(browser_lane) => Some(browser_lane),
            PaneBody::Terminal(_) => None,
            PaneBody::Agent(_) => None,
        }
    }
}

impl PaneLeaf for Pane {
    fn pane_id(&self) -> &str {
        &self.id
    }

    fn pane_frame(&self) -> PixelRect {
        self.frame
    }

    fn set_pane_frame(&mut self, frame: PixelRect) {
        self.frame = frame;
    }
}

/// Spawns a fresh interactive shell pane; new panes take the keyboard
/// (egui_tty's pending-focus flag is consumed by the next frame that acts
/// on it).
fn new_pane(id: String, spec: PtySpec, wake: &Arc<FrameWake>) -> anyhow::Result<Pane> {
    new_pane_with_focus(id, spec, true, wake)
}

/// Restore variant: only the pane that should hold the keyboard requests
/// focus (a freshly restored tree must not hand it to the last pane to
/// render).
fn new_pane_with_focus(
    id: String,
    spec: PtySpec,
    focus: bool,
    wake: &Arc<FrameWake>,
) -> anyhow::Result<Pane> {
    let spec = maybe_inject_claude_settings(spec);
    let handoff = start_shell(spec, wake.as_wake())?;
    let mut terminal = Terminal::new(handoff.stream).map_err(|e| anyhow::anyhow!("{e:#}"))?;
    terminal = terminal.with_label(id.clone());
    if focus {
        terminal.request_focus();
    }
    Ok(Pane {
        panel_ids: vec![id.clone()],
        id,
        frame: PixelRect::default(),
        body: PaneBody::Terminal(Box::new(TerminalLane {
            terminal,
            status_receiver: Some(handoff.status),
            notify_receiver: Some(handoff.events),
            pending_turns: HashMap::new(),
            delivered_turns: HashMap::new(),
        })),
        title_override: None,
        tree_pid: handoff.tree_pid,
    })
}

/// A browser pane defers its WebView2 host: `restore` and IPC creation run
/// before the app window exists, so the host is built by the frame loop's
/// first sync pass (the pane's URL already sits in the lane).
fn new_browser_pane(id: String, start_url: String, _wake: &Arc<FrameWake>) -> anyhow::Result<Pane> {
    Ok(Pane {
        panel_ids: vec![id.clone()],
        id,
        frame: PixelRect::default(),
        body: PaneBody::Browser(PaneBrowser {
            start_url,
            host: None,
            snapshot: cmux_webview2::BrowserSnapshot::default(),
            phase: cmux_webview2::BrowserPhase::Creating,
            failure: None,
            refs: Vec::new(),
        }),
        title_override: None,
        tree_pid: None,
    })
}

/// An agent activity pane has no child process: it aggregates its
/// workspace's agent events (design lane 5) and closes only by user
/// action or with its workspace.
fn new_agent_pane(id: String, _wake: &Arc<FrameWake>) -> anyhow::Result<Pane> {
    Ok(Pane {
        panel_ids: vec![id.clone()],
        id,
        frame: PixelRect::default(),
        body: PaneBody::Agent(AgentActivityPane {
            scroll_from_bottom: 0.0,
        }),
        title_override: None,
        tree_pid: None,
    })
}

/// `browser.open_split` default page (owner round 8, 2026-10-27: the
/// recommended option of figma sec 20's residual — a token-painted
/// landing instead of bare `about:blank`): the fresh browser leaf shows
/// the resolved viewport fill the moment it exists instead of Edge's
/// theme-derived gray blank document (which the runtime special-cases
/// against every `DefaultBackgroundColor` mechanism — sec 20's ladder).
/// A data URL keeps the page title-less and content-less, so pane
/// titles and identify facts behave like the M5.1 blank; the URL is
/// percent-encoded (`#` -> `%23`) and re-derives from the theme, so a
/// configured `theme.background` moves the home with the chrome.
/// Restored leaves keep their saved URL (including older
/// `about:blank` ones): only NEW leaves land on the token home.
fn browser_home_url(viewport_background: u32) -> String {
    format!(
        "data:text/html,<html><body style=\"background:%23\
         {viewport_background:06x};margin:0;height:100%25;\
         overflow:hidden\"></body></html>"
    )
}

/// Per-surface WebView2 user data folder: `cmux\webview2\<id>` under the
/// given base. Kept on disk after close (documented residual in ADR-0006).
fn browser_user_data_dir(base: &std::path::Path, id: &str) -> std::path::PathBuf {
    base.join("cmux").join("webview2").join(id)
}

/// The local-application-data base for browser state (`%LOCALAPPDATA%`,
/// falling back to the temp dir when the env var is missing).
fn user_data_base() -> std::path::PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(std::path::PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(std::env::temp_dir)
}

/// Maps an intercepted virtual key to its app action. M5.1 routes the
/// workspace digit chords (`ctrl+1..ctrl+9`, upstream `nextSidebarTab`
/// family: digits map 1→0 like the bindings table); other keys stay with
/// the browser.
fn action_for_accel_key(key: u32) -> Option<Action> {
    let digit = (key as u8).checked_sub(b'1')?;
    if digit > 8 {
        return None;
    }
    Some(Action::SelectWorkspace(usize::from(digit)))
}

/// One vertical tab. Upstream names Workspace tabs exactly that way
/// (`newTab` renders "New Workspace"; `nextSidebarTab` renders "Next
/// Workspace"): a tab owns a whole workspace â€” its split tree and focused
/// pane. The shells of inactive tabs keep running; their terminals are
/// kept alive with `poll()` (no rendering) so switching never loses
/// output. (Note: a hidden pane's PTY size stays stale until the tab is
/// shown â€” resize while hidden is a recorded slice limitation.)
/// `id` is the workspace's stable identity on the automation surface
/// (M4.1 `workspace.*` calls address it; minted `w{n}`, carried across
/// session saves).
struct Workspace {
    id: String,
    tree: PaneTree<Pane>,
    focused: String,
    /// The user's rename for this workspace (owner round 9, 2026-10-27):
    /// set by the sidebar's inline editor, carried across saves (the
    /// snapshot's `customTitle` key — the DTO field existed since ADR-0009
    /// but nothing wrote it). `preferred_name` gives it precedence over
    /// the live shell title.
    custom_title: Option<String>,
}

/// The workspace list backing the sidebar, with the active index.
struct Workspaces {
    items: Vec<Workspace>,
    active: usize,
}

impl Workspaces {
    fn new_single(tree: PaneTree<Pane>, id: String, focused: String) -> Self {
        Self {
            items: vec![Workspace {
                id,
                tree,
                focused,
                custom_title: None,
            }],
            active: 0,
        }
    }

    fn active_workspace(&self) -> &Workspace {
        &self.items[self.active]
    }

    fn active_workspace_mut(&mut self) -> &mut Workspace {
        let index = self.active;
        &mut self.items[index]
    }

    /// Upstream names a workspace after the running process
    /// (`processTitle`); the port derives it live from the focused pane's
    /// shell title (OSC), falling back to a positional name while the
    /// shell has none. The user's rename (`set_custom_title`) takes
    /// precedence over both ([`cmux_shell::sidebar::preferred_name`]).
    fn name_of(&self, index: usize) -> String {
        let workspace = &self.items[index];
        let title = workspace
            .tree
            .leaf(&workspace.focused)
            .and_then(Pane::display_title);
        cmux_shell::sidebar::preferred_name(
            workspace.custom_title.as_deref(),
            title,
            format!("shell {}", index + 1),
        )
    }

    /// Writes the user's rename for the workspace at `index` (owner round
    /// 9): `Some` sets the custom title, `None` clears it back to the
    /// live shell title. The sidebar editor only commits through
    /// [`cmux_shell::sidebar::rename_commit`]'s policy, so an empty edit
    /// never reaches here as a clear.
    fn set_custom_title(&mut self, index: usize, title: Option<String>) {
        if index < self.items.len() {
            self.items[index].custom_title = title;
        }
    }

    /// Switches the active workspace; `false` when the index is out of
    /// range (no-op, like upstream's out-of-range digit select).
    fn select(&mut self, index: usize) -> bool {
        if index < self.items.len() && index != self.active {
            self.active = index;
            true
        } else {
            false
        }
    }

    fn cycle(&mut self, forward: bool) {
        let count = self.items.len();
        if count <= 1 {
            return;
        }
        self.active = if forward {
            (self.active + 1) % count
        } else {
            (self.active + count - 1) % count
        };
    }

    /// Removes the workspace at `index`. Returns `true` when the list
    /// has emptied (the caller must close the window). The selection
    /// follows: prefer the same slot when a tab before it was removed,
    /// otherwise clamp.
    fn close(&mut self, index: usize) -> bool {
        if self.items.len() <= 1 {
            return true;
        }
        self.items.remove(index);
        // The selection follows: prefer the same slot when a tab before it
        // was removed, otherwise clamp (unit-tested via the pure helper).
        self.active = adjusted_active_after_close(self.items.len(), self.active, index);
        false
    }

    /// Appends a workspace; `focus` mirrors upstream's SSH open handoff
    /// (ADR-0014) and the create path: a focused open activates it, while
    /// `--no-focus` leaves the caller's active slot alone. (The restore
    /// path builds its item vec directly and applies the saved index.)
    fn push_with_focus(&mut self, workspace: Workspace, focus: bool) {
        self.items.push(workspace);
        if focus {
            self.active = self.items.len() - 1;
        }
    }

    /// Moves the workspace at `from` to land at index `to` (owner round 9
    /// drag-reorder; the pure slot math lives in
    /// [`cmux_shell::sidebar::reorder_target`], the caller filters no-ops
    /// so nothing shifts). `to` is the FINAL index in the reordered list.
    /// The active selection follows its workspace by id, whatever the
    /// slots do around it.
    fn reorder(&mut self, from: usize, to: usize) {
        if from >= self.items.len() || to >= self.items.len() {
            return;
        }
        let active_id = self.items[self.active].id.clone();
        let moved = self.items.remove(from);
        self.items.insert(to, moved);
        if let Some(new_index) = self.items.iter().position(|other| other.id == active_id) {
            self.active = new_index;
        }
    }
}

/// Adjusted active index after removing `removed`, mirroring
/// [`Workspaces::close`]'s selection-follow rule (unit-tested without
/// live panes). A removal index equal to the active slot keeps the same
/// slot â€” the next tab slides into it.
fn adjusted_active_after_close(len_after: usize, active_before: usize, removed: usize) -> usize {
    if len_after == 0 {
        return 0;
    }
    if removed < active_before {
        active_before - 1
    } else if active_before >= len_after {
        len_after - 1
    } else {
        active_before
    }
}

/// One active sidebar drag (owner round 9): identity, not geometry — the
/// frame reads the pointer from egui and reruns the pure planner each
/// pass, so only the dragged row's index/id is held across frames. The
/// session opens on the source row's first decisively-dragging frame
/// (egui resolves click-vs-drag first: >6pt move, >0.8s hold, or dragging
/// out of the row) and closes on the button release.
struct SidebarDrag {
    from: usize,
    id: String,
}

/// One inline-rename session (owner round 9): the editor's state at open
/// and its once-per-session focus handoff. `baseline` is the auto-derived
/// display name at editor open (the rename policy's live-name input);
/// `custom_open` is the workspace's custom title at open (`None` = never
/// user-renamed, the policy's user-title branch input).
struct SidebarRename {
    index: usize,
    id: String,
    buffer: String,
    baseline: Option<String>,
    custom_open: Option<String>,
    requested_focus: bool,
}

// ---------------------------------------------------------------------------
// M4.1 automation row shapes (pure, unit-mirrored). Upstream's harness
// resolves ids from these rows (`index`/`id` on every list), so the keys
// stay stable; the port's extras (`pane_id`, `workspace_id`…) are
// additive.
// ---------------------------------------------------------------------------

/// One `workspace.list` row. `title` follows the port's `name_of` rule
/// (live shell title, positional fallback).
fn workspace_row(index: usize, id: &str, title: &str, selected: bool) -> serde_json::Value {
    serde_json::json!({
        "index": index,
        "id": id,
        "title": title,
        "selected": selected,
    })
}

/// One `surface.list` row. The port maps panes 1:1 to surfaces, so
/// `pane_id` repeats `id`; `type` is `terminal` or `browser` (upstream's
/// `surface_type`).
fn surface_row(
    index: usize,
    id: &str,
    workspace_id: &str,
    surface_type: &str,
    title: Option<String>,
    focused: bool,
) -> serde_json::Value {
    serde_json::json!({
        "index": index,
        "id": id,
        "pane_id": id,
        "workspace_id": workspace_id,
        "title": title,
        "type": surface_type,
        "focused": focused,
    })
}

/// One `pane.list` row (1:1 with surfaces: one terminal per pane).
fn pane_row(index: usize, id: &str, workspace_id: &str, focused: bool) -> serde_json::Value {
    serde_json::json!({
        "index": index,
        "id": id,
        "workspace_id": workspace_id,
        "surface_ids": [id],
        "surface_count": 1,
        "selected_surface_id": id,
        "focused": focused,
    })
}

struct App {
    workspaces: Workspaces,
    actions: Vec<Action>,
    runtime: RuntimeSettings,
    /// Where the session snapshot lives; `None` disables saving (one-shot
    /// and restore smokes).
    session_paths: Option<SessionPaths>,
    /// Whether the vertical workspace (tab) strip is shown.
    sidebar_visible: bool,
    /// cmux's window keyboard focus, refreshed each frame (M3.2a: the
    /// arrival policy's `isAppFocused` input).
    is_app_focused: bool,
    /// Best-effort Windows toast delivery for `effects.desktop`
    /// (M3.2b). Worker-threaded; failures only log.
    toast: cmux_toast::ToastDelivery,
    /// Unread desktop-notification count per workspace, index-aligned
    /// with `workspaces.items` (M3.1 ring; upstream `isRead` semantics â€”
    /// viewing a workspace is its mark-read). Not persisted.
    workspace_alerts: Vec<usize>,
    /// Design lane 5 (agent activity stream): retained event cards per
    /// workspace, index-aligned like `workspace_alerts`. Ingest sites are
    /// the OSC notification decisions and turn deliveries; the stream
    /// pane reads its workspace's slot live. Bounded retention
    /// (`agent_cards::CARDS_MAX_PER_WORKSPACE`); not persisted.
    agent_cards: cmux_shell::agent_cards::CardStore,
    /// Hook events arrived over the control pipe this run (M3.3 slice A).
    /// The pipe server pushes into it; poll_children drains per frame and
    /// feeds the same arrival policy as OSC notifications.
    hook_receiver: mpsc::Receiver<cmux_ipc::EnqueueRequest>,
    /// M4.1 app-plane calls: the pipe server parks one request per call;
    /// `serve_ipc_calls` (poll_children drain) executes on the UI thread
    /// and completes the reply inside the server thread's budget.
    ipc_calls: mpsc::Receiver<IpcCallMessage>,
    /// The control pipe this app instance serves ("" when IPC is disabled
    /// by `CMUX_IPC_DISABLED=1`); injected into pane environments so hooks
    /// spawned inside a pane reach the right app.
    ipc_pipe: String,
    /// Highest hidden-sidebar-row count already logged (frame-to-frame
    /// log throttle); `None` when everything currently fits.
    sidebar_overflow_reported: Option<usize>,
    /// Sidebar scroll offset in content px (owner round 9): rows +
    /// services paint shifted by this much inside the clipped region
    /// below the plus row; 0 when everything fits (the pre-scroll paint
    /// geometry is byte-identical).
    sidebar_scroll: f32,
    /// Reveal hint (owner round 9): the active row's index to scroll
    /// into view (upstream's `shouldScrollSelectedWorkspace` policy) —
    /// set when the selection moves (click shortcuts are excluded: a
    /// clicked row is on-screen), consumed by the next draw pass.
    sidebar_scroll_reveal: Option<usize>,
    /// Live drag-reorder session (owner round 9): the dragged row's
    /// index and id while egui reports the row dragged; `None` between
    /// drags. The commit is an `Action::ReorderWorkspace` applied after
    /// painting (indices snapshotted at drag stop).
    sidebar_drag: Option<SidebarDrag>,
    /// Live inline-rename session (owner round 9):
    /// `buffer` is the editor text, `baseline` the auto-derived name at
    /// editor start (the rename-policy comparison), `custom_open` the
    /// workspace's custom title at open (`None` = never renamed),
    /// `requested_focus` the once-per-session focus handoff to the edit
    /// widget.
    sidebar_rename: Option<SidebarRename>,
    /// Status bar machine sample (lane 2): held CPU% and RAM-in-use,
    /// refreshed on the ~0.75s cadence; `None` until (or unless) host
    /// probes succeed. Non-Windows targets keep `None` and the `–`
    /// placeholders.
    status_state: Option<StatusState>,
    pending_window_close: bool,
    auto_exit_ms: Option<u64>,
    auto_split_ms: Option<u64>,
    started: Instant,
    split_stage: u8,
    /// Frame-wake cell shared with every pane's shell-reader threads
    /// (`FrameWake`); the context is installed on the first frame.
    frame_wake: Arc<FrameWake>,
    /// Host window facts for WebView2 hosting (M5.1): the top-level HWND,
    /// its DPI scale, and its screen position. `platform_hwnd` is `None`
    /// until the first frame with a native window.
    platform_hwnd: Option<isize>,
    dpi_scale: f64,
    window_pos: Option<(i32, i32)>,
    /// Shared chord router forwarded to every browser host (M5.1): app
    /// accelerator keys pressed while a webview has native focus are
    /// intercepted before the browser and re-entered as app actions.
    accel_router: cmux_webview2::AccelRouter,
    accel_wake_set: bool,
    /// Egui's focused pane id from the previous frame (`None` initially);
    /// the model-focus reconcile only follows an egui focus change against
    /// this, so standing terminal focus never steals model focus back from
    /// a browser leaf (M5.1 focus rule).
    last_egui_focus: Option<String>,
    /// M5.2 automation calls waiting on a WebView2 completion, oldest
    /// first. The serve pass enqueues; the sync pass promotes completions
    /// (usually the same or next frame) and answers the parked reply.
    pending_cdp: Vec<PendingCdp>,
    /// Caller-facing ids for automation calls, minted on the frame thread
    /// (unique per app, echoed by every completion).
    next_cdp_id: u64,
    // Owner round 4 (2026-10-26): the git sampler fields retired with
    // the sub-header chips; the `cmux_shell::git_state` parsing lib
    // stays public with its tests.
    /// Design lane 4 (row 12): the services segment's display rows per
    /// workspace, sampled off-thread on the same ~2s cadence as the git
    /// lane. Keys are workspace positions — a sample is at most one
    /// cadence stale after a close shifts them.
    service_rows: std::collections::BTreeMap<usize, Vec<cmux_shell::port_state::PortRow>>,
    services_requested_at: Option<Instant>,
    service_requests: Option<std::sync::mpsc::Sender<Vec<(usize, u32)>>>,
    service_results: mpsc::Receiver<Vec<(usize, Vec<cmux_shell::port_state::PortRow>)>>,
    /// Design lane 3 palette (row 13, node 1:512): modal state. Rows are
    /// built once on open (the config read happens there, not per
    /// frame); a pane closing while the palette is up re-resolves the
    /// entry at activation (vanishing rows read as their error).
    palette_open: bool,
    palette_query: String,
    palette_rows: Vec<PaletteEntry>,
    palette_selected: usize,
    /// The pane id a cross-workspace palette jump focuses once its
    /// SelectWorkspace action has switched the active workspace.
    pending_jump: Option<String>,
}

impl App {
    /// Sets the active workspace's focused pane and tells egui_tty to hand
    /// the keyboard to it (click-to-focus needs no here). A browser pane
    /// has no egui widget to focus; its native focus follows the frame
    /// loop's sync pass.
    fn set_active_focus(&mut self, id: String) {
        let index = self.workspaces.active;
        // Focus transitions are user-driven and low-frequency; a line per
        // transition keeps misrouted keyboard input diagnosable (M5.1
        // webview interplay).
        eprintln!("cmux-shell: focus -> {id} (workspace {index})");
        let tree = &mut self.workspaces.active_workspace_mut().tree;
        if let Some(pane) = tree.leaf_mut(&id) {
            if let Some(terminal) = pane.terminal_mut() {
                terminal.request_focus();
            }
        }
        self.workspaces.active_workspace_mut().focused = id;
    }

    /// Keyboard goes to the (possibly restored) active workspace's focused
    /// pane.
    fn settle_focus(&mut self) {
        let focused = self.workspaces.active_workspace().focused.clone();
        self.set_active_focus(focused);
    }

    /// Keybindings: parse and consume before pane widgets see them, so a
    /// binding never leaks into the shell. The table comes from the config
    /// file (`RuntimeSettings::bindings`); matching uses egui's own
    /// `matches_exact` semantics for the command/ctrl distinctions.
    fn process_bindings(&mut self, ui: &mut egui::Ui) {
        let mut actions: Vec<Action> = Vec::new();
        let mut consumed: Vec<egui::Event> = Vec::new();

        ui.ctx().input(|input| {
            for event in &input.events {
                let egui::Event::Key {
                    key,
                    modifiers,
                    pressed: true,
                    repeat: false,
                    ..
                } = event
                else {
                    continue;
                };
                if let Some((_, action)) = self
                    .runtime
                    .bindings
                    .iter()
                    .find(|(combo, _)| combo.key == *key && modifiers.matches_exact(combo.mods))
                {
                    actions.push(*action);
                    consumed.push(event.clone());
                }
            }
        });

        if !consumed.is_empty() {
            ui.ctx()
                .input_mut(|input| input.events.retain(|event| !consumed.contains(event)));
        }
        self.actions.extend(actions);
    }

    /// Smoke scheduling: auto splits then auto exit, driven by env vars.
    fn process_schedule(&mut self) {
        let elapsed_ms = self.started.elapsed().as_millis() as u64;
        if let Some(split_ms) = self.auto_split_ms {
            if elapsed_ms >= split_ms && self.split_stage < 1 {
                self.split_stage = 1;
                self.actions
                    .push(Action::SplitFocused(SplitDirection::Right));
                eprintln!("cmux-shell smoke: split-right action queued");
            }
            if elapsed_ms >= split_ms + 1500 && self.split_stage < 2 {
                self.split_stage = 2;
                self.actions
                    .push(Action::SplitFocused(SplitDirection::Down));
                eprintln!("cmux-shell smoke: split-down action queued");
            }
        }
        if let Some(exit_ms) = self.auto_exit_ms {
            if elapsed_ms >= exit_ms && !self.pending_window_close {
                eprintln!("cmux-shell smoke: window close queued");
            }
            if elapsed_ms >= exit_ms {
                self.pending_window_close = true;
            }
        }
    }

    /// Polls every pane's child watcher exactly once per frame, across all
    /// workspaces (inactive tabs keep running); when a pane's shell has
    /// exited, the pane closes (product decision 2026-10-17 per owner
    /// request: typing `exit` just closes; a workspace's last pane closes
    /// the workspace; the last workspace closing closes the window). The
    /// exit reason goes to stderr at pane granularity â€” no terminal output
    /// is logged; the receiver is not preserved.
    fn poll_children(&mut self, ctx: &egui::Context) {
        // Collect exits first: applying a close can remove a workspace
        // (its last pane), which would invalidate indices mid-iteration.
        let mut exits: Vec<(usize, String)> = Vec::new();
        // Workspace count read before the mutable walk (the drain below
        // resizes the alert list against it); the active slot feeds the
        // arrival policy's focus gates.
        let workspace_count = self.workspaces.items.len();
        let active_workspace = self.workspaces.active;
        for (index, workspace) in self.workspaces.items.iter_mut().enumerate() {
            for id in workspace.tree.ordered_leaf_ids() {
                let Some(pane) = workspace.tree.leaf_mut(&id) else {
                    continue;
                };
                // Browser panes carry no PTY lanes: no OSC notifications,
                // turn boundaries, or exit watcher to drain (M5.1).
                let PaneBody::Terminal(lane) = &mut pane.body else {
                    continue;
                };
                // M3.1 + M3.2a + M3.4: drain this pane's shell events
                // (OSC desktop notifications and turn boundaries) and
                // resolve each through the arrival policy (upstream's
                // disposition + effects gates). The port realizes ring
                // (unread) and the record log so far; banner/sound/flash/
                // reorder land with the toast slice. Bodies stay out of
                // logs â€” only the workspace, disposition, and title echo.
                if let Some(receiver) = lane.notify_receiver.as_mut() {
                    loop {
                        match receiver.try_recv() {
                            Ok(events) => {
                                for event in events {
                                    match event {
                                        PaneEvent::Notifications(batch) => {
                                            for event in batch {
                                                let context = DeliveryContext {
                                                    is_active_workspace: index == active_workspace,
                                                    is_focused_surface: surface_focused(
                                                        workspace.focused.as_str(),
                                                        id.as_str(),
                                                        index,
                                                        active_workspace,
                                                    ),
                                                    is_app_focused: self.is_app_focused,
                                                    sound_when_focused: self
                                                        .runtime
                                                        .notifications
                                                        .sound_when_focused,
                                                    suppress_when_app_focused: self
                                                        .runtime
                                                        .notifications
                                                        .suppress_when_app_focused,
                                                    ..DeliveryContext::default()
                                                };
                                                let decision = policy::resolve(
                                                    &context,
                                                    PolicyEffects::all_on(),
                                                );
                                                if decision.effects.mark_unread {
                                                    if self
                                                        .workspace_alerts
                                                        .get_mut(index)
                                                        .is_none()
                                                    {
                                                        self.workspace_alerts
                                                            .resize(workspace_count, 0);
                                                    }
                                                    if let Some(slot) =
                                                        self.workspace_alerts.get_mut(index)
                                                    {
                                                        *slot += 1;
                                                    }
                                                }
                                                // Lane 5: the agent activity stream
                                                // pane records every real event
                                                // that arrived; ring/toast stay
                                                // policy-driven and unchanged.
                                                self.agent_cards.sync_len(workspace_count);
                                                self.agent_cards.push(
                                                    index,
                                                    cmux_shell::agent_cards::AgentCard {
                                                        at: Instant::now(),
                                                        data:
                                                            cmux_shell::agent_cards::CardData::Notification {
                                                                title: event.title.clone(),
                                                                body: event.body.clone(),
                                                            },
                                                    },
                                                );
                                                let title = if event.title.is_empty() {
                                                    "no title".to_owned()
                                                } else {
                                                    event.title.clone()
                                                };
                                                // M3.2b: the desktop effect posts a
                                                // best-effort toast on the worker
                                                // thread; `effects.sound` maps to the
                                                // toast's audio (no sound â‡’ silent).
                                                // The event title is the toast title;
                                                // OSC 9 carries none, so the
                                                // workspace name stands in.
                                                if decision.effects.desktop {
                                                    let toast_title = if event.title.is_empty() {
                                                        format!("Workspace {}", index + 1)
                                                    } else {
                                                        event.title.clone()
                                                    };
                                                    if let Err(error) = self.toast.show(
                                                        &toast_title,
                                                        &event.body,
                                                        !decision.effects.sound,
                                                    ) {
                                                        eprintln!(
                                                        "cmux-shell: toast queue failed: {error}"
                                                    );
                                                    }
                                                }
                                                eprintln!(
                                                "cmux-shell: OSC notification in workspace {} â†’ {} \
                                                 ring={} flash={} desktop={} (title '{}')",
                                                index + 1,
                                                decision.disposition.as_str(),
                                                decision.effects.mark_unread,
                                                decision.effects.pane_flash,
                                                decision.effects.desktop,
                                                title
                                            );
                                            }
                                        }
                                        PaneEvent::Turn(TurnEvent::Pending(turn)) => {
                                            // M3.4: schedule the boundary's
                                            // debounce. A newer identifier
                                            // replaces an older scheduled one
                                            // (the detector may invalidate and
                                            // re-create within one chunk); a
                                            // repaint is requested so an idle
                                            // app still wakes at the deadline.
                                            let now = Instant::now();
                                            let deadline = now + turn.delay;
                                            let supersedes = lane
                                                .pending_turns
                                                .get(turn.agent_id)
                                                .is_none_or(|existing| {
                                                    existing.pending.identifier < turn.identifier
                                                });
                                            if supersedes {
                                                // Lane 5: freeze the pane's
                                                // visible rows for the active
                                                // card at boundary-see time.
                                                let tail = tail_rows(
                                                    &lane
                                                        .terminal
                                                        .visible_text()
                                                        .ok()
                                                        .unwrap_or_default(),
                                                    AGENT_CARD_TAIL_ROWS,
                                                );
                                                lane.pending_turns.insert(
                                                    turn.agent_id,
                                                    PendingTurn {
                                                        pending: turn,
                                                        deadline,
                                                        tail,
                                                    },
                                                );
                                                ctx.request_repaint_after(deadline - now);
                                            }
                                        }
                                        PaneEvent::Turn(TurnEvent::Invalidated { identifier }) => {
                                            // The boundary died before its
                                            // deadline; drop the scheduled
                                            // delivery. Identifiers are
                                            // monotonic per detector, so this
                                            // can only match that boundary's
                                            // own entry.
                                            lane.pending_turns.retain(|_, pending_turn| {
                                                pending_turn.pending.identifier != identifier
                                            });
                                        }
                                    }
                                }
                            }
                            Err(mpsc::TryRecvError::Empty) => break,
                            Err(mpsc::TryRecvError::Disconnected) => {
                                lane.notify_receiver = None;
                                break;
                            }
                        }
                    }
                }
                // M3.4: deliver turn boundaries whose debounce expired,
                // through the same arrival policy as OSC and hook events.
                // The per-agent identifier high-water mark keeps delivery
                // exactly-once; a pane whose shell exits drops its pending
                // state with it (Windows offers no foreground-PID check â€”
                // recorded divergence, ADR-0011 Amendment 6).
                let now = Instant::now();
                let due_agents: Vec<&'static str> = lane
                    .pending_turns
                    .iter()
                    .filter(|(_, pending_turn)| pending_turn.deadline <= now)
                    .map(|(agent_id, _)| *agent_id)
                    .collect();
                for agent_id in due_agents {
                    let Some(pending_turn) = lane.pending_turns.remove(agent_id) else {
                        continue;
                    };
                    if pending_turn.pending.identifier
                        <= lane.delivered_turns.get(agent_id).copied().unwrap_or(0)
                    {
                        continue;
                    }
                    let context = DeliveryContext {
                        is_active_workspace: index == active_workspace,
                        is_focused_surface: surface_focused(
                            workspace.focused.as_str(),
                            id.as_str(),
                            index,
                            active_workspace,
                        ),
                        is_app_focused: self.is_app_focused,
                        sound_when_focused: self.runtime.notifications.sound_when_focused,
                        suppress_when_app_focused: self
                            .runtime
                            .notifications
                            .suppress_when_app_focused,
                        ..DeliveryContext::default()
                    };
                    let decision = policy::resolve(&context, PolicyEffects::all_on());
                    if decision.effects.mark_unread {
                        if self.workspace_alerts.get_mut(index).is_none() {
                            self.workspace_alerts.resize(workspace_count, 0);
                        }
                        if let Some(slot) = self.workspace_alerts.get_mut(index) {
                            *slot += 1;
                        }
                    }
                    // Upstream's prompt-turn handler posts the agent's
                    // display name with the "Task completed" body and the
                    // turn-complete category (category effects parity: the
                    // port realizes ring/toast/log here).
                    if decision.effects.desktop {
                        if let Err(error) = self.toast.show(
                            pending_turn.pending.agent_name,
                            "Task completed",
                            !decision.effects.sound,
                        ) {
                            eprintln!("cmux-shell: toast queue failed: {error}");
                        }
                    }
                    lane.delivered_turns
                        .insert(agent_id, pending_turn.pending.identifier);
                    // Lane 5: the delivered boundary becomes a stream card
                    // (the agent display name + "Task completed" verbatim).
                    self.agent_cards.sync_len(workspace_count);
                    self.agent_cards.push(
                        index,
                        cmux_shell::agent_cards::AgentCard {
                            at: now,
                            data: cmux_shell::agent_cards::CardData::TurnCompleted {
                                agent_name: pending_turn.pending.agent_name.to_owned(),
                                body: "Task completed".to_owned(),
                            },
                        },
                    );
                    eprintln!(
                        "cmux-shell: agent turn-complete {} in workspace {} â†’ {} ring={} flash={} desktop={}{}",
                        pending_turn.pending.agent_id,
                        index + 1,
                        decision.disposition.as_str(),
                        decision.effects.mark_unread,
                        decision.effects.pane_flash,
                        decision.effects.desktop,
                        // Dev tap (wedge slice 2026-10-19): delivery latency,
                        // measured from app start, only when the reader tap
                        // is on.
                        if std::env::var_os("CMUX_DEBUG_READER").is_some() {
                            format!(" t={:.3}s", self.started.elapsed().as_secs_f64())
                        } else {
                            String::new()
                        }
                    );
                }
                let mut put_back: Option<mpsc::Receiver<WatchedStatus>> = None;
                let reason = match lane.status_receiver.take() {
                    Some(receiver) => match receiver.try_recv() {
                        Ok(Ok(status)) => Some(if status.success() {
                            "clean exit".to_owned()
                        } else {
                            format!("exit status {status:?}")
                        }),
                        Ok(Err(error)) => Some(format!("terminated after an error: {error:#}")),
                        Err(mpsc::TryRecvError::Disconnected) => {
                            Some("watcher exited without a status".to_owned())
                        }
                        Err(mpsc::TryRecvError::Empty) => {
                            put_back = Some(receiver);
                            None
                        }
                    },
                    None => None,
                };
                if let Some(receiver) = put_back {
                    lane.status_receiver = Some(receiver);
                }
                if let Some(reason) = reason {
                    eprintln!("cmux-shell: pane {id} closed â€” shell {reason}");
                    exits.push((index, id));
                }
            }
        }
        // Apply descending by workspace index: removing a workspace only
        // shifts indices above it, so a last-pane close earlier in the list
        // can never corrupt the pairs that follow. (Two last panes can exit
        // within one frame â€” e.g. after a taskkill â€” and each must close
        // *its own* workspace.)
        exits.sort_by_key(|&(index, _)| std::cmp::Reverse(index));
        for (index, id) in exits {
            self.close_pane_in_workspace(index, &id);
        }
        // M3.3 slice A: hook events admitted by the control pipe feed the
        // same arrival policy as OSC notifications, after the exits pass so
        // workspace indices are re-read against the current list.
        self.drain_hook_events();
    }

    /// Drains hook events admitted by the pipe server into the shared
    /// arrival policy (M3.3 slice A). The event's routed `CMUX_SURFACE_ID`
    /// routes it to the workspace that owns the pane; a surface the app no
    /// longer has (pane closed since the hook spawned, or a foreign client)
    /// logs its disposition and drops without a ring. Payload bodies stay
    /// out of logs â€” only agent, subcommand, surface ownership, and effect
    /// summary are echoed.
    fn drain_hook_events(&mut self) {
        // Surface id â†’ workspace index, plus each workspace's focused pane,
        // read once per frame.
        let mut by_surface: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        let focused_per_workspace: Vec<&str> = self
            .workspaces
            .items
            .iter()
            .enumerate()
            .map(|(index, workspace)| {
                for id in workspace.tree.ordered_leaf_ids() {
                    by_surface.insert(id, index);
                }
                workspace.focused.as_str()
            })
            .collect();
        let active_workspace = self.workspaces.active;
        loop {
            let request = match self.hook_receiver.try_recv() {
                Ok(request) => request,
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => break,
            };
            let validated = request.validated;
            let surface = validated
                .environment
                .get("CMUX_SURFACE_ID")
                .and_then(serde_json::Value::as_str);
            let workspace_index = surface.and_then(|s| by_surface.get(s).copied());
            let is_focused_surface = match (surface, workspace_index) {
                (Some(surface), Some(index)) => surface_focused(
                    focused_per_workspace[index],
                    surface,
                    index,
                    active_workspace,
                ),
                _ => false,
            };
            let context = DeliveryContext {
                is_active_workspace: workspace_index == Some(active_workspace),
                is_focused_surface,
                is_app_focused: self.is_app_focused,
                sound_when_focused: self.runtime.notifications.sound_when_focused,
                suppress_when_app_focused: self.runtime.notifications.suppress_when_app_focused,
                ..DeliveryContext::default()
            };
            let decision = policy::resolve(&context, PolicyEffects::all_on());
            // M3.3b: the claude table includes journal-route events
            // (`feed`, `pre-tool-use`) upstream never rings. They log and
            // drop â€” no unread, no toast â€” until the journal feed ships.
            let deliverable = subcommand_is_deliverable(&validated.subcommand);
            if deliverable && decision.effects.mark_unread {
                if let Some(index) = workspace_index {
                    if self.workspace_alerts.get_mut(index).is_none() {
                        self.workspace_alerts.resize(self.workspaces.items.len(), 0);
                    }
                    if let Some(slot) = self.workspace_alerts.get_mut(index) {
                        *slot += 1;
                    }
                }
            }
            // The agent's own message stands in as the toast body when the
            // hook carries one (OS-level inputs are bounded upstream of
            // this point; slice long bodies defensively anyway).
            let body = validated
                .payload
                .get("message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .chars()
                .take(160)
                .collect::<String>();
            let title = format!("cmux {}", validated.subcommand);
            if deliverable && decision.effects.desktop {
                if let Err(error) = self.toast.show(&title, &body, !decision.effects.sound) {
                    eprintln!("cmux-shell: toast queue failed: {error}");
                }
            }
            // Lane 5: every validated hook event lands in its workspace's
            // activity stream — the design's step cards (the journal
            // routes included; they still never ring or toast).
            if let Some(index) = workspace_index {
                self.agent_cards.sync_len(self.workspaces.items.len());
                self.agent_cards.push(
                    index,
                    cmux_shell::agent_cards::AgentCard {
                        at: Instant::now(),
                        data: cmux_shell::agent_cards::CardData::Notification { title, body },
                    },
                );
            }
            let where_at = match workspace_index {
                Some(index) => format!("workspace {}", index + 1),
                None => "unknown surface (no ring)".to_owned(),
            };
            eprintln!(
                "cmux-shell: agent hook {} {} in {} â†’ {} ring={} flash={} desktop={}",
                validated.agent,
                validated.subcommand,
                where_at,
                decision.disposition.as_str(),
                deliverable && decision.effects.mark_unread,
                deliverable && decision.effects.pane_flash,
                deliverable && decision.effects.desktop,
            );
        }

        // M4.1: app-plane calls ride the same frame drain — the server
        // thread is parked on each request's reply channel, so answering
        // here completes the round trip inside the reply budget. A
        // `workspace.create`/`select` here mutates the same model the
        // action handlers mutate (no mid-draw reentrancy: this runs
        // before the frame's draw pass).
        self.serve_ipc_calls();
    }

    /// Drains the M4.1 app-plane request queue. Every request runs on the
    /// UI thread; the result (or structured problem) is handed straight
    /// back to the parked server thread. Automation verbs (M5.2) complete
    /// asynchronously: the call is issued here and answered when a later
    /// frame promotes the WebView2 completion (`pending_cdp`).
    fn serve_ipc_calls(&mut self) {
        self.sweep_stale_cdp();
        loop {
            let Ok(message) = self.ipc_calls.try_recv() else {
                // Empty (or the server never existed): nothing to serve.
                break;
            };
            let IpcCallMessage { call, reply } = message;
            eprintln!(
                "cmux-shell: ipc serve {} {}",
                call.id,
                call.request.method_name()
            );
            match cdp_kind_of(&call.request) {
                Some((surface_id, kind)) => self.begin_cdp_call(surface_id, kind, reply),
                None => {
                    let result = self.execute_ipc_call(call.request);
                    let _ = reply.send(result);
                }
            }
        }
    }

    /// Executes one app-plane request against the live model. Failures
    /// are structured problems with upstream's error shape; side effects
    /// reuse the exact code paths the UI actions use.
    fn execute_ipc_call(
        &mut self,
        request: cmux_ipc::AppRequest,
    ) -> Result<serde_json::Value, cmux_ipc::RequestProblem> {
        use cmux_ipc::AppRequest as Req;
        match request {
            Req::Identify => self.identify_result(),
            Req::WorkspaceList => Ok(serde_json::json!({
                "workspaces": self.workspace_rows(),
            })),
            Req::WorkspaceCurrent => {
                let workspace_id = self.workspaces.active_workspace().id.clone();
                Ok(serde_json::json!({ "workspace_id": workspace_id }))
            }
            Req::WorkspaceSelect { workspace_id } => self.select_workspace_by_id(&workspace_id),
            Req::WorkspaceCreate => self
                .spawn_new_workspace()
                .map(|(workspace_id, pane_id)| {
                    serde_json::json!({
                        "workspace_id": workspace_id,
                        "surface_id": pane_id,
                    })
                })
                .map_err(|error| {
                    cmux_ipc::RequestProblem::io(format!("workspace spawn failed: {error:#}"))
                }),
            Req::WorkspaceSshOpen {
                destination,
                ssh_options,
                port,
                identity_file,
                title,
                initial_command,
                focus,
            } => {
                let title_override = Some(title.clone().unwrap_or_else(|| destination.clone()));
                let spec = ssh_spec(
                    &destination,
                    port,
                    identity_file.as_deref(),
                    &ssh_options,
                    initial_command.as_deref(),
                );
                self.spawn_workspace_with_spec(spec, title_override, focus)
                    .map(|(workspace_id, pane_id)| {
                        serde_json::json!({
                            "workspace_id": workspace_id,
                            "surface_id": pane_id,
                        })
                    })
                    .map_err(|error| {
                        cmux_ipc::RequestProblem::io(format!(
                            "ssh workspace spawn failed: {error:#}"
                        ))
                    })
            }
            Req::SurfaceList { workspace_id } => {
                Ok(serde_json::json!({ "surfaces": self.surface_rows(workspace_id.as_deref())? }))
            }
            Req::PaneList { workspace_id } => {
                Ok(serde_json::json!({ "panes": self.pane_rows(workspace_id.as_deref())? }))
            }
            Req::SurfaceSendText { surface_id, text } => {
                let (workspace_index, pane_id) = self.resolve_surface(surface_id.as_deref())?;
                let pane = self
                    .workspaces
                    .items
                    .get_mut(workspace_index)
                    .and_then(|workspace| workspace.tree.leaf_mut(&pane_id))
                    .ok_or_else(|| {
                        cmux_ipc::RequestProblem::not_found(format!("surface '{pane_id}' vanished"))
                    })?;
                if pane.body.is_browser() {
                    // Browser surfaces accept browser.* verbs, not PTY text
                    // (upstream parity: browser panels are not keystroke
                    // targets).
                    return Err(cmux_ipc::RequestProblem::invalid_state(format!(
                        "surface '{pane_id}' is a browser surface"
                    )));
                }
                let terminal = pane.terminal_mut().ok_or_else(|| {
                    cmux_ipc::RequestProblem::not_found(format!(
                        "surface '{pane_id}' has no terminal"
                    ))
                })?;
                terminal.send(text.as_bytes()).map_err(|error| {
                    cmux_ipc::RequestProblem::io(format!(
                        "surface '{pane_id}' write failed: {error:#}"
                    ))
                })?;
                Ok(serde_json::json!({}))
            }
            Req::SurfaceReadText { surface_id } => {
                let (workspace_index, pane_id) = self.resolve_surface(surface_id.as_deref())?;
                let pane = self
                    .workspaces
                    .items
                    .get_mut(workspace_index)
                    .and_then(|workspace| workspace.tree.leaf_mut(&pane_id))
                    .ok_or_else(|| {
                        cmux_ipc::RequestProblem::not_found(format!("surface '{pane_id}' vanished"))
                    })?;
                if pane.body.is_browser() {
                    // Browser surfaces expose browser.url.get instead of grid
                    // text (M5.1 scope: no DOM/screen reads).
                    return Err(cmux_ipc::RequestProblem::invalid_state(format!(
                        "surface '{pane_id}' is a browser surface"
                    )));
                }
                let terminal = pane.terminal_mut().ok_or_else(|| {
                    cmux_ipc::RequestProblem::not_found(format!(
                        "surface '{pane_id}' has no terminal"
                    ))
                })?;
                let text = terminal.visible_text().map_err(|error| {
                    cmux_ipc::RequestProblem::io(format!(
                        "surface '{pane_id}' read failed: {error:#}"
                    ))
                })?;
                // The visible grid only: read_text does not include
                // scrollback or the selection (M4.1 scope; upstream's
                // read-screen reads the live console surface).
                Ok(serde_json::json!({ "text": text, "truncated": false }))
            }
            Req::CommandList => {
                let config = read_commands_config();
                Ok(serde_json::json!({
                    "commands": command_list_json(&config.commands)
                }))
            }
            Req::CommandRun {
                name,
                surface_id,
                ack,
            } => {
                // Resolve the definition against a call-time config read
                // (the trust boundary stays app-side, like upstream's
                // executor); the CLI only names the command and acks.
                let definition = {
                    let config = read_commands_config();
                    resolve_config_command(&config.commands, &name, ack)?.clone()
                };
                let (workspace_index, pane_id) = self.resolve_surface(surface_id.as_deref())?;
                let pane = self
                    .workspaces
                    .items
                    .get_mut(workspace_index)
                    .and_then(|workspace| workspace.tree.leaf_mut(&pane_id))
                    .ok_or_else(|| {
                        cmux_ipc::RequestProblem::not_found(format!("surface '{pane_id}' vanished"))
                    })?;
                if pane.body.is_browser() {
                    // Browser surfaces are not keystroke targets (the
                    // send_text parity shape).
                    return Err(cmux_ipc::RequestProblem::invalid_state(format!(
                        "surface '{pane_id}' is a browser surface"
                    )));
                }
                let terminal = pane.terminal_mut().ok_or_else(|| {
                    cmux_ipc::RequestProblem::not_found(format!(
                        "surface '{pane_id}' has no terminal"
                    ))
                })?;
                let text = cmux_config::sanitize_command_text(
                    definition.command.as_deref().unwrap_or_default(),
                );
                if text.is_empty() {
                    // Upstream's executor silently no-ops a command that
                    // sanitizes to nothing; the port reports it.
                    return Err(cmux_ipc::RequestProblem::invalid_state(format!(
                        "command '{name}' is empty after sanitizing"
                    )));
                }
                // Upstream's executor types `command + "\n"` so the shell
                // executes it (verified `CmuxConfigExecutor` 196); the
                // Windows terminator is CR (see `command_input_bytes`).
                terminal
                    .send(&command_input_bytes(&text))
                    .map_err(|error| {
                        cmux_ipc::RequestProblem::io(format!(
                            "surface '{pane_id}' write failed: {error:#}"
                        ))
                    })?;
                Ok(serde_json::json!({ "name": name, "surface_id": pane_id }))
            }
            Req::BrowserOpenSplit { surface_id, url } => {
                self.browser_open_split(surface_id.as_deref(), url)
            }
            Req::AgentOpenSplit { surface_id } => self.agent_open_split(surface_id.as_deref()),
            Req::BrowserNavigate { surface_id, url } => {
                let (workspace_index, pane_id) = self.resolve_surface(surface_id.as_deref())?;
                let lane = self.browser_lane_at(workspace_index, &pane_id)?;
                if let Some(host) = &lane.host {
                    host.navigate(url.clone()).map_err(|error| {
                        cmux_ipc::RequestProblem::io(format!(
                            "surface '{pane_id}' navigate failed: {error}"
                        ))
                    })?;
                }
                // The page changes: snapshotted refs are stale (upstream
                // ephemerality), a fresh snapshot mints a new table.
                lane.refs.clear();
                // Unwired (host arrives on the next frame): nothing to
                // drive yet; the desired URL below is picked up at wire.
                // The desired first page tracks the live one, so a
                // save/restore cycle reopens where the caller left the
                // pane (M5.1 decision).
                lane.start_url = url;
                Ok(serde_json::json!({ "surface_id": pane_id }))
            }
            Req::BrowserBack { surface_id } => {
                self.browser_history_verb(surface_id.as_deref(), cmux_webview2::HistoryVerb::Back)
            }
            Req::BrowserForward { surface_id } => self
                .browser_history_verb(surface_id.as_deref(), cmux_webview2::HistoryVerb::Forward),
            Req::BrowserReload { surface_id } => {
                self.browser_history_verb(surface_id.as_deref(), cmux_webview2::HistoryVerb::Reload)
            }
            Req::BrowserUrlGet { surface_id } => {
                let (workspace_index, pane_id) = self.resolve_surface(surface_id.as_deref())?;
                let workspace_id = self.workspaces.items[workspace_index].id.clone();
                let lane = self.browser_lane_at(workspace_index, &pane_id)?;
                Ok(serde_json::json!({
                    "surface_id": pane_id,
                    "workspace_id": workspace_id,
                    "type": "browser",
                    "phase": lane.phase.as_str(),
                    "url": if lane.snapshot.source.is_empty() {
                        lane.start_url.clone()
                    } else {
                        lane.snapshot.source.clone()
                    },
                    "title": lane.snapshot.title,
                    "can_go_back": lane.snapshot.can_go_back,
                    "can_go_forward": lane.snapshot.can_go_forward,
                }))
            }
            Req::BrowserGetTitle { surface_id } => {
                let (workspace_index, pane_id) = self.resolve_surface(surface_id.as_deref())?;
                let lane = self.browser_lane_at(workspace_index, &pane_id)?;
                Ok(serde_json::json!({
                    "surface_id": pane_id,
                    "title": lane.snapshot.title,
                }))
            }
            Req::BrowserFocusWebview { surface_id } => {
                let (workspace_index, pane_id) = self.resolve_surface(surface_id.as_deref())?;
                self.browser_lane_at(workspace_index, &pane_id)?;
                // The port hands keyboard focus to a browser pane through
                // the model (same flow a native GotFocus takes): the
                // pane's workspace becomes active (`settle_focus` renames
                // the viewing state), the pane becomes the workspace's
                // focused leaf, and the next tick's
                // `MoveFocus(PROGRAMMATIC)` lands the webview focus.
                if self.workspaces.active != workspace_index {
                    if self.workspaces.select(workspace_index) {
                        self.settle_focus();
                    }
                    self.clear_alert(workspace_index);
                }
                self.set_active_focus(pane_id.clone());
                Ok(serde_json::json!({ "surface_id": pane_id }))
            }
            Req::BrowserIsWebviewFocused { surface_id } => {
                let (workspace_index, pane_id) = self.resolve_surface(surface_id.as_deref())?;
                self.browser_lane_at(workspace_index, &pane_id)?;
                let focused = self.workspaces.active == workspace_index
                    && self.workspaces.items[workspace_index].focused == pane_id;
                Ok(serde_json::json!({
                    "surface_id": pane_id,
                    "focused": focused,
                }))
            }
            // The automation verbs never reach here: the serve pass
            // routes them through the completion plane (`cdp_kind_of`
            // intercepts in `serve_ipc_calls`). The arm keeps the match
            // exhaustive; its reply documents the routing if it ever
            // changes by accident.
            Req::BrowserSnapshot { .. }
            | Req::BrowserEval { .. }
            | Req::BrowserClick { .. }
            | Req::BrowserDblclick { .. }
            | Req::BrowserType { .. }
            | Req::BrowserFill { .. }
            | Req::BrowserFocus { .. }
            | Req::BrowserCheck { .. }
            | Req::BrowserSelect { .. }
            | Req::BrowserHover { .. }
            | Req::BrowserScrollIntoView { .. }
            | Req::BrowserScroll { .. }
            | Req::BrowserGet { .. }
            | Req::BrowserIs { .. }
            | Req::BrowserScreenshot { .. }
            | Req::BrowserKey { .. }
            | Req::BrowserWait { .. } => Err(cmux_ipc::RequestProblem::invalid_state(
                "browser automation verbs answer through the deferred-completion plane".to_owned(),
            )),
        }
    }

    /// The browser lane for a resolved surface, rejecting terminal
    /// surfaces with upstream's `invalidState` (browser verbs on a shell).
    fn browser_lane_at(
        &mut self,
        workspace_index: usize,
        pane_id: &str,
    ) -> Result<&mut PaneBrowser, cmux_ipc::RequestProblem> {
        let pane = self
            .workspaces
            .items
            .get_mut(workspace_index)
            .and_then(|workspace| workspace.tree.leaf_mut(pane_id))
            .ok_or_else(|| {
                cmux_ipc::RequestProblem::not_found(format!("surface '{pane_id}' vanished"))
            })?;
        pane.browser_lane_mut().ok_or_else(|| {
            cmux_ipc::RequestProblem::invalid_state(format!(
                "surface '{pane_id}' is a terminal surface"
            ))
        })
    }

    /// History verbs shared by back/forward/reload. History end points are
    /// no-op successes (recorded M5.1 decision); unwired hosts also accept
    /// silently — the smoke always polls readiness first. Snapshotted refs
    /// die with the shown document (upstream: refs are ephemeral).
    fn browser_history_verb(
        &mut self,
        surface: Option<&str>,
        verb: cmux_webview2::HistoryVerb,
    ) -> Result<serde_json::Value, cmux_ipc::RequestProblem> {
        let (workspace_index, pane_id) = self.resolve_surface(surface)?;
        let lane = self.browser_lane_at(workspace_index, &pane_id)?;
        lane.refs.clear();
        if let Some(host) = &lane.host {
            match verb {
                cmux_webview2::HistoryVerb::Back => host.go_back(),
                cmux_webview2::HistoryVerb::Forward => host.go_forward(),
                cmux_webview2::HistoryVerb::Reload => host.reload(),
            }
        }
        Ok(serde_json::json!({ "surface_id": pane_id }))
    }

    // ------------------------------------------------------------------
    // M5.2: the automation plane (snapshot / eval / click / dblclick / type)
    // ------------------------------------------------------------------

    /// Starts one automation call. Validation failures answer inline; a
    /// successfully issued phase-1 call parks the reply channel in
    /// `pending_cdp` until a completion promotes it (usually the same or
    /// next frame, inside the server thread's reply budget).
    fn begin_cdp_call(
        &mut self,
        surface_id: Option<String>,
        kind: CdpKind,
        reply: mpsc::Sender<Result<serde_json::Value, cmux_ipc::RequestProblem>>,
    ) {
        let preparation = match self.prepare_cdp_call(surface_id.as_deref(), &kind) {
            Ok(preparation) => preparation,
            Err(problem) => {
                let _ = reply.send(Err(problem));
                return;
            }
        };
        let id = self.next_cdp_id;
        self.next_cdp_id += 1;
        let now = Instant::now();
        // M5.4: the wait kind arms its deadline at issue time; every
        // other kind has none (the plain reply budget bounds it).
        let deadline = match &kind {
            CdpKind::Wait { timeout_ms, .. } => {
                Some(now + Duration::from_millis(*timeout_ms as u64))
            }
            _ => None,
        };
        match preparation
            .host
            .call_cdp(id, preparation.method, &preparation.params)
        {
            Ok(()) => {
                self.pending_cdp.push(PendingCdp {
                    id,
                    surface_id: preparation.surface_id,
                    kind,
                    step: preparation.first_step,
                    deadline,
                    first_issued: now,
                    last_activity: now,
                    reply,
                });
            }
            Err(error) => {
                let _ = reply.send(Err(cmux_ipc::RequestProblem::io(format!(
                    "browser call failed: {error}"
                ))));
            }
        }
    }

    /// Validates and shapes an automation verb's first CDP call: a browser
    /// leaf with a wired host, and ref verbs resolved against the current
    /// snapshot table.
    fn prepare_cdp_call(
        &mut self,
        surface: Option<&str>,
        kind: &CdpKind,
    ) -> Result<CdpPrep, cmux_ipc::RequestProblem> {
        let (workspace_index, pane_id) = self.resolve_surface(surface)?;
        let lane = self.browser_lane_at(workspace_index, &pane_id)?;
        let Some(host) = lane.host.clone() else {
            return Err(cmux_ipc::RequestProblem::invalid_state(format!(
                "surface '{pane_id}' is not ready yet (webview not wired)"
            )));
        };
        let preparation = match kind {
            CdpKind::Eval { expression } => CdpPrep {
                surface_id: pane_id,
                host,
                method: "Runtime.evaluate",
                params: cdp_evaluate_params(expression.clone()).to_string(),
                first_step: CdpStep::Final,
            },
            CdpKind::Snapshot => CdpPrep {
                surface_id: pane_id,
                host,
                method: "Runtime.evaluate",
                params: cdp_evaluate_params(SNAPSHOT_WALKER_JS.to_owned()).to_string(),
                first_step: CdpStep::Final,
            },
            CdpKind::ClickMouse {
                target,
                click_count,
            } => {
                let selector = self.selector_for_target(workspace_index, &pane_id, target)?;
                CdpPrep {
                    surface_id: pane_id,
                    host,
                    method: "Runtime.evaluate",
                    params: cdp_evaluate_params(center_evaluate_js(&selector)).to_string(),
                    first_step: CdpStep::AfterCenter {
                        click_count: *click_count,
                    },
                }
            }
            CdpKind::TypeText { target, text } => {
                let selector = self.selector_for_target(workspace_index, &pane_id, target)?;
                CdpPrep {
                    surface_id: pane_id,
                    host,
                    method: "Runtime.evaluate",
                    params: cdp_evaluate_params(focus_evaluate_js(&selector)).to_string(),
                    first_step: CdpStep::AfterFocus { text: text.clone() },
                }
            }
            CdpKind::ElementJs { target, js } => {
                let selector = self.selector_for_target(workspace_index, &pane_id, target)?;
                CdpPrep {
                    surface_id: pane_id,
                    host,
                    method: "Runtime.evaluate",
                    params: cdp_evaluate_params(element_expression_of(js, &selector)).to_string(),
                    first_step: CdpStep::Final,
                }
            }
            CdpKind::HoverMouse { target } => {
                let selector = self.selector_for_target(workspace_index, &pane_id, target)?;
                CdpPrep {
                    surface_id: pane_id,
                    host,
                    method: "Runtime.evaluate",
                    params: cdp_evaluate_params(center_evaluate_js(&selector)).to_string(),
                    first_step: CdpStep::AfterHoverCenter,
                }
            }
            CdpKind::ScrollBy { dx, dy } => CdpPrep {
                surface_id: pane_id,
                host,
                method: "Runtime.evaluate",
                params: cdp_evaluate_params(scroll_evaluate_js(*dx, *dy)).to_string(),
                first_step: CdpStep::Final,
            },
            CdpKind::Screenshot => CdpPrep {
                surface_id: pane_id,
                host,
                method: "Page.captureScreenshot",
                // Viewport-only PNG (the spec's "surface" capture; a
                // full-page variant stays a documented residual).
                params: serde_json::json!({
                    "format": "png",
                    "captureBeyondViewport": false,
                })
                .to_string(),
                first_step: CdpStep::Final,
            },
            CdpKind::Key { half, token } => {
                // The vocabulary check lives here (a prepare error
                // becomes the call's structured invalid_params reply).
                let plan = key_plan_of(token)?;
                match half {
                    // press: keyDown now, the prepared keyUp on arrival.
                    KeyHalf::Press => CdpPrep {
                        surface_id: pane_id,
                        host,
                        method: "Input.dispatchKeyEvent",
                        params: key_dispatch_params(&plan, true).to_string(),
                        first_step: CdpStep::AfterKeyDown {
                            up: key_dispatch_params(&plan, false),
                        },
                    },
                    // keydown/keyup: one event only.
                    half => CdpPrep {
                        surface_id: pane_id,
                        host,
                        method: "Input.dispatchKeyEvent",
                        params: key_dispatch_params(&plan, *half == KeyHalf::Down).to_string(),
                        first_step: CdpStep::Final,
                    },
                }
            }
            CdpKind::Wait {
                poll,
                timeout_ms: _,
            } => CdpPrep {
                surface_id: pane_id,
                host,
                method: "Runtime.evaluate",
                params: cdp_evaluate_params(wait_evaluate_js(poll)).to_string(),
                first_step: CdpStep::Final,
            },
        };
        Ok(preparation)
    }

    /// The CSS selector an element-verb target rides on: a ref resolves
    /// against the current snapshot table (stale refs answer the
    /// re-snapshot hint), a selector is used verbatim (it was injected as
    /// a JSON literal, so page text can never break it out).
    fn selector_for_target(
        &mut self,
        workspace_index: usize,
        pane_id: &str,
        target: &ElementTarget,
    ) -> Result<String, cmux_ipc::RequestProblem> {
        match target {
            ElementTarget::Ref(ref_id) => {
                self.snapshot_ref_selector(workspace_index, pane_id, ref_id)
            }
            ElementTarget::Selector(selector) => Ok(selector.clone()),
        }
    }

    /// The CSS selector a snapshot ref maps to; unknown refs are
    /// `invalid_params` with the re-snapshot hint (upstream stale-ref
    /// behavior).
    fn snapshot_ref_selector(
        &mut self,
        workspace_index: usize,
        pane_id: &str,
        ref_id: &str,
    ) -> Result<String, cmux_ipc::RequestProblem> {
        let pane = self.workspaces.items[workspace_index]
            .tree
            .leaf_mut(pane_id)
            .ok_or_else(|| {
                cmux_ipc::RequestProblem::not_found(format!("surface '{pane_id}' vanished"))
            })?;
        let lane = pane.browser_lane_mut().ok_or_else(|| {
            cmux_ipc::RequestProblem::invalid_state(format!(
                "surface '{pane_id}' is a terminal surface"
            ))
        })?;
        lane.refs
            .iter()
            .find(|entry| entry.id == ref_id)
            .map(|entry| entry.selector.clone())
            .ok_or_else(|| {
                cmux_ipc::RequestProblem::invalid_params(format!(
                    "ref '{ref_id}' is unknown for the current snapshot (call browser.snapshot again)"
                ))
            })
    }

    /// The wired host for follow-up calls (a pane closed between phases
    /// surfaces as `invalid_state`; the pending reply then carries it).
    fn cdp_host_for(
        &mut self,
        surface_id: Option<&str>,
    ) -> Result<cmux_webview2::BrowserHost, cmux_ipc::RequestProblem> {
        let (workspace_index, pane_id) = self.resolve_surface(surface_id)?;
        let lane = self.browser_lane_at(workspace_index, &pane_id)?;
        lane.host.clone().ok_or_else(|| {
            cmux_ipc::RequestProblem::invalid_state(format!(
                "surface '{pane_id}' has no wired webview"
            ))
        })
    }

    /// Issues one follow-up CDP input call for an in-flight automation
    /// call (mouse press/release or text insert), minting its own call id.
    fn issue_cdp_input(
        &mut self,
        surface_id: Option<&str>,
        method: &'static str,
        params: &serde_json::Value,
    ) -> Result<u64, cmux_ipc::RequestProblem> {
        let host = self.cdp_host_for(surface_id)?;
        let id = self.next_cdp_id;
        self.next_cdp_id += 1;
        host.call_cdp(id, method, &params.to_string())
            .map_err(|error| {
                cmux_ipc::RequestProblem::io(format!("browser call failed: {error}"))
            })?;
        Ok(id)
    }

    /// Runs one completion's escalation (the automation state machine).
    /// Called on the frame thread right after the owning host's tick; the
    /// promoted entry either answers the parked reply or re-parks with the
    /// next planned call.
    fn advance_cdp(&mut self, completion: cmux_webview2::CdpCompletion) {
        let Some(index) = self
            .pending_cdp
            .iter()
            .position(|pending| pending.id == completion.id)
        else {
            // The parked server thread already sent its structured
            // timeout, or the pane closed mid-call.
            eprintln!(
                "cmux-shell: cdp completion {} arrived after its reply window (dropped)",
                completion.id
            );
            return;
        };
        let pending = self.pending_cdp.remove(index);
        let raw = match decode_cdp_outcome(&completion.outcome) {
            Ok(raw) => raw,
            Err(problem) => {
                reply_cdp(&pending, Err(problem));
                return;
            }
        };
        match &pending.step {
            CdpStep::Final => {
                // M5.4: a wait's poll completion re-arms the same
                // evaluate (or answers); every other Final shapes its
                // static reply.
                if let CdpKind::Wait { poll, timeout_ms } = &pending.kind {
                    let poll = poll.clone();
                    let timeout_ms = *timeout_ms;
                    match parse_wait_slots(&raw, &poll) {
                        Err(problem) => reply_cdp(&pending, Err(problem)),
                        Ok(true) => {
                            let waited_ms = pending.first_issued.elapsed().as_millis() as u64;
                            reply_cdp(
                                &pending,
                                Ok(serde_json::json!({
                                    "surface_id": pending.surface_id,
                                    "waited_ms": waited_ms,
                                })),
                            );
                        }
                        Ok(false)
                            if pending
                                .deadline
                                .is_some_and(|deadline| Instant::now() >= deadline) =>
                        {
                            reply_cdp(
                                &pending,
                                Err(cmux_ipc::RequestProblem::wait_timeout(timeout_ms)),
                            );
                        }
                        Ok(false) => {
                            // Re-arm: the completion's wake drives the
                            // next poll (one round trip per evaluation,
                            // no busy spin).
                            let params = cdp_evaluate_params(wait_evaluate_js(&poll));
                            match self.issue_cdp_input(
                                Some(&pending.surface_id),
                                "Runtime.evaluate",
                                &params,
                            ) {
                                Ok(poll_id) => {
                                    self.pending_cdp.push(PendingCdp {
                                        id: poll_id,
                                        surface_id: pending.surface_id.clone(),
                                        kind: pending.kind.clone(),
                                        step: CdpStep::Final,
                                        deadline: pending.deadline,
                                        first_issued: pending.first_issued,
                                        last_activity: Instant::now(),
                                        reply: pending.reply,
                                    });
                                }
                                Err(problem) => reply_cdp(&pending, Err(problem)),
                            }
                        }
                    }
                    return;
                }
                let result = self.shape_cdp_final(&pending, &raw);
                reply_cdp(&pending, result);
            }
            CdpStep::AfterCenter { click_count } => {
                let click_count = *click_count;
                match parse_center_cdp(&raw) {
                    Err(problem) => reply_cdp(&pending, Err(problem)),
                    Ok((x, y)) => {
                        let press = serde_json::json!({
                            "type": "mousePressed",
                            "x": x,
                            "y": y,
                            "button": "left",
                            "buttons": 1,
                            "clickCount": click_count,
                        });
                        match self.issue_cdp_input(
                            Some(&pending.surface_id),
                            "Input.dispatchMouseEvent",
                            &press,
                        ) {
                            Ok(press_id) => {
                                self.pending_cdp.push(PendingCdp {
                                    id: press_id,
                                    surface_id: pending.surface_id.clone(),
                                    kind: pending.kind.clone(),
                                    step: CdpStep::AfterPress { x, y, click_count },
                                    deadline: pending.deadline,
                                    first_issued: pending.first_issued,
                                    last_activity: Instant::now(),
                                    reply: pending.reply,
                                });
                            }
                            Err(problem) => reply_cdp(&pending, Err(problem)),
                        }
                    }
                }
            }
            CdpStep::AfterPress { x, y, click_count } => {
                let release = serde_json::json!({
                    "type": "mouseReleased",
                    "x": *x,
                    "y": *y,
                    "button": "left",
                    "buttons": 1,
                    "clickCount": *click_count,
                });
                match self.issue_cdp_input(
                    Some(&pending.surface_id),
                    "Input.dispatchMouseEvent",
                    &release,
                ) {
                    Ok(release_id) => {
                        self.pending_cdp.push(PendingCdp {
                            id: release_id,
                            surface_id: pending.surface_id.clone(),
                            kind: pending.kind.clone(),
                            step: CdpStep::Final,
                            deadline: pending.deadline,
                            first_issued: pending.first_issued,
                            last_activity: Instant::now(),
                            reply: pending.reply,
                        });
                    }
                    Err(problem) => reply_cdp(&pending, Err(problem)),
                }
            }
            CdpStep::AfterFocus { text } => {
                if evaluate_value_of(&raw).as_bool() != Some(true) {
                    reply_cdp(
                        &pending,
                        Err(cmux_ipc::RequestProblem::invalid_state(
                            "the referenced element is gone (snapshot again)".to_owned(),
                        )),
                    );
                    return;
                }
                let insert = serde_json::json!({ "text": text });
                match self.issue_cdp_input(Some(&pending.surface_id), "Input.insertText", &insert) {
                    Ok(insert_id) => {
                        self.pending_cdp.push(PendingCdp {
                            id: insert_id,
                            surface_id: pending.surface_id.clone(),
                            kind: pending.kind.clone(),
                            step: CdpStep::Final,
                            deadline: pending.deadline,
                            first_issued: pending.first_issued,
                            last_activity: Instant::now(),
                            reply: pending.reply,
                        });
                    }
                    Err(problem) => reply_cdp(&pending, Err(problem)),
                }
            }
            CdpStep::AfterHoverCenter => match parse_center_cdp(&raw) {
                Err(problem) => reply_cdp(&pending, Err(problem)),
                Ok((x, y)) => {
                    // Upstream hover = one real mouse move; no press.
                    let mouse_move = serde_json::json!({
                        "type": "mouseMoved",
                        "x": x,
                        "y": y,
                        "button": "none",
                        "buttons": 0,
                    });
                    match self.issue_cdp_input(
                        Some(&pending.surface_id),
                        "Input.dispatchMouseEvent",
                        &mouse_move,
                    ) {
                        Ok(move_id) => {
                            self.pending_cdp.push(PendingCdp {
                                id: move_id,
                                surface_id: pending.surface_id.clone(),
                                kind: pending.kind.clone(),
                                step: CdpStep::Final,
                                deadline: pending.deadline,
                                first_issued: pending.first_issued,
                                last_activity: Instant::now(),
                                reply: pending.reply,
                            });
                        }
                        Err(problem) => reply_cdp(&pending, Err(problem)),
                    }
                }
            },
            CdpStep::AfterKeyDown { up } => {
                // M5.4 press semantics: the keyDown landed, dispatch the
                // prepared keyUp, then shape the static reply.
                match self.issue_cdp_input(Some(&pending.surface_id), "Input.dispatchKeyEvent", up)
                {
                    Ok(up_id) => {
                        self.pending_cdp.push(PendingCdp {
                            id: up_id,
                            surface_id: pending.surface_id.clone(),
                            kind: pending.kind.clone(),
                            step: CdpStep::Final,
                            deadline: pending.deadline,
                            last_activity: Instant::now(),
                            first_issued: pending.first_issued,
                            reply: pending.reply,
                        });
                    }
                    Err(problem) => reply_cdp(&pending, Err(problem)),
                }
            }
        }
    }

    /// Shapes a finished automation call's reply. Snapshot replies also
    /// install the fresh refs table (best-effort: a pane closed between
    /// the call and its completion keeps its stale table, which nothing
    /// can address any more).
    fn shape_cdp_final(
        &mut self,
        pending: &PendingCdp,
        raw: &serde_json::Value,
    ) -> Result<serde_json::Value, cmux_ipc::RequestProblem> {
        match &pending.kind {
            CdpKind::Eval { .. } => Ok(serde_json::json!({
                "surface_id": pending.surface_id,
                "value": evaluate_value_of(raw),
            })),
            CdpKind::Snapshot => {
                let value = evaluate_value_of(raw);
                let refs = value
                    .get("refs")
                    .and_then(serde_json::Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                // Defensive: a walker reply missing either member means
                // the page evaluation happened but the walker broke —
                // never answer a polluted snapshot.
                let text = value
                    .get("text")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| {
                        cmux_ipc::RequestProblem::invalid_state(
                            "the page snapshot reply was malformed".to_owned(),
                        )
                    })?;
                if value
                    .get("refs")
                    .and_then(serde_json::Value::as_array)
                    .is_none()
                {
                    return Err(cmux_ipc::RequestProblem::invalid_state(
                        "the page snapshot reply was malformed".to_owned(),
                    ));
                }
                self.cdp_install_refs(&pending.surface_id, &refs);
                Ok(serde_json::json!({
                    "surface_id": pending.surface_id,
                    "url": value.get("url").and_then(serde_json::Value::as_str),
                    "title": value.get("title").and_then(serde_json::Value::as_str),
                    "text": text,
                    "refs": refs,
                }))
            }
            CdpKind::ClickMouse {
                target,
                click_count,
            } => {
                let mut reply = lean_target_reply(&pending.surface_id, target);
                if let Some(fields) = reply.as_object_mut() {
                    fields.insert("click_count".to_owned(), serde_json::json!(click_count));
                }
                Ok(reply)
            }
            CdpKind::TypeText { target, text } => {
                let mut reply = lean_target_reply(&pending.surface_id, target);
                if let Some(fields) = reply.as_object_mut() {
                    fields.insert("inserted".to_owned(), serde_json::json!(text));
                }
                Ok(reply)
            }
            CdpKind::ElementJs { target, js } => match js {
                ElementJs::Fill { .. }
                | ElementJs::Focus
                | ElementJs::Check { .. }
                | ElementJs::SelectOption { .. }
                | ElementJs::ScrollIntoView => {
                    // Lean action replies echo the consumed target — the
                    // caller already learned the action ran (or gets the
                    // structured violation from the parser).
                    parse_element_action(raw, js)?;
                    Ok(lean_target_reply(&pending.surface_id, target))
                }
                ElementJs::GetDom {
                    field: GetField::Count,
                    ..
                } => Ok(serde_json::json!({
                    "surface_id": pending.surface_id,
                    "count": evaluate_value_of(raw).as_u64().unwrap_or(0),
                })),
                ElementJs::GetDom { .. } => Ok(serde_json::json!({
                    "surface_id": pending.surface_id,
                    "value": evaluate_value_of(raw),
                })),
                ElementJs::IsState { state } => Ok(serde_json::json!({
                    "surface_id": pending.surface_id,
                    "value": parse_is_state(raw, *state),
                })),
            },
            CdpKind::ScrollBy { .. } => {
                let (x, y) = parse_scroll_reply(raw);
                Ok(serde_json::json!({
                    "surface_id": pending.surface_id,
                    "x": x,
                    "y": y,
                }))
            }
            CdpKind::HoverMouse { target } => Ok(lean_target_reply(&pending.surface_id, target)),
            CdpKind::Screenshot => {
                // Upstream writes the capture app-side and reports the
                // path (the CLI reads `path`/`url`); the port stages its
                // viewport PNGs under `%TEMP%\cmux-screenshots`.
                let bytes = screenshot_bytes_of(raw)?;
                let dir = screenshot_dir();
                std::fs::create_dir_all(&dir).map_err(|error| {
                    cmux_ipc::RequestProblem::io(format!("screenshot staging dir failed: {error}"))
                })?;
                let millis = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis();
                let filename = format!(
                    "cmux-{}-{}-{millis}.png",
                    screenshot_surface_fragment(&pending.surface_id),
                    pending.id
                );
                let path = dir.join(filename);
                std::fs::write(&path, &bytes).map_err(|error| {
                    cmux_ipc::RequestProblem::io(format!("screenshot write failed: {error}"))
                })?;
                Ok(serde_json::json!({
                    "surface_id": pending.surface_id,
                    "path": path.display().to_string(),
                }))
            }
            CdpKind::Key { token, .. } => Ok(serde_json::json!({
                "surface_id": pending.surface_id,
                "key": token,
            })),
            CdpKind::Wait { timeout_ms, .. } => {
                // Defensive: the wait arm answers inside `advance_cdp`
                // (its poll loop re-arms or completes); reaching shape
                // would mean the pairing broke, so the deadline's
                // structured outcome is the only honest reply left.
                Err(cmux_ipc::RequestProblem::wait_timeout(*timeout_ms))
            }
        }
    }

    /// Installs a snapshot's fresh refs table for its surface.
    fn cdp_install_refs(&mut self, surface_id: &str, refs: &[serde_json::Value]) {
        if let Ok((workspace_index, pane_id)) = self.resolve_surface(Some(surface_id)) {
            if let Some(pane) = self.workspaces.items[workspace_index]
                .tree
                .leaf_mut(&pane_id)
            {
                if let Some(lane) = pane.browser_lane_mut() {
                    lane.refs = refs
                        .iter()
                        .filter_map(|entry| {
                            Some(BrowserRef {
                                id: entry.get("ref")?.as_str()?.to_owned(),
                                selector: entry.get("selector")?.as_str()?.to_owned(),
                            })
                        })
                        .collect();
                }
            }
        }
    }

    /// Disposes automation pendings whose reply window is long gone
    /// (budget twice, plus slack): their completions either never fire (a
    /// pane closed mid-call) or arrive too late to answer anything.
    /// Freshness reads `last_activity`, so a live wait — which re-arms on
    /// every poll — is never swept.
    fn sweep_stale_cdp(&mut self) {
        let threshold = APP_REPLY_BUDGET * 2 + Duration::from_millis(100);
        let mut expired = 0usize;
        self.pending_cdp.retain(|pending| {
            let fresh = pending.last_activity.elapsed() <= threshold;
            if !fresh {
                expired += 1;
            }
            fresh
        });
        if expired > 0 {
            eprintln!("cmux-shell: cdp pending disposed ({expired} reply window(s) expired)");
        }
    }

    /// `browser.open_split`: splits `surface_id` (default: the focused
    /// surface) right with a browser pane loading `url` (round-8 default:
    /// the token-painted home page, `browser_home_url`). The new pane
    /// takes model focus like a terminal split; the host arrives with the
    /// next frame's sync pass.
    fn browser_open_split(
        &mut self,
        caller: Option<&str>,
        url: Option<String>,
    ) -> Result<serde_json::Value, cmux_ipc::RequestProblem> {
        let (workspace_index, target) = match caller {
            Some(spec) => self.resolve_surface(Some(spec))?,
            None => (
                self.workspaces.active,
                self.workspaces.active_workspace().focused.clone(),
            ),
        };
        let new_id = mint_pane_id();
        let start_url =
            url.unwrap_or_else(|| browser_home_url(self.runtime.theme.viewport_background));
        let pane =
            new_browser_pane(new_id.clone(), start_url, &self.frame_wake).map_err(|error| {
                cmux_ipc::RequestProblem::io(format!("browser pane spawn failed: {error:#}"))
            })?;
        let tree = &mut self.workspaces.items[workspace_index].tree;
        if tree
            .split_leaf(&target, SplitDirection::Right, pane)
            .is_none()
        {
            return Err(cmux_ipc::RequestProblem::io(format!(
                "surface '{target}' cannot split right"
            )));
        }
        self.set_active_focus(new_id.clone());
        let workspace_id = self.workspaces.items[workspace_index].id.clone();
        Ok(serde_json::json!({
            "surface_id": new_id,
            "pane_id": new_id,
            "workspace_id": workspace_id,
            "type": "browser",
        }))
    }

    /// `agent.open_split`: splits `surface_id` (default: the focused
    /// surface) right with an agent activity stream pane (design lane 5).
    /// No upstream wire verb; the body renders the workspace's retained
    /// agent events live (the `agent_cards` store + pending turn facts).
    fn agent_open_split(
        &mut self,
        caller: Option<&str>,
    ) -> Result<serde_json::Value, cmux_ipc::RequestProblem> {
        let (workspace_index, target) = match caller {
            Some(spec) => self.resolve_surface(Some(spec))?,
            None => (
                self.workspaces.active,
                self.workspaces.active_workspace().focused.clone(),
            ),
        };
        let new_id = mint_pane_id();
        let pane = new_agent_pane(new_id.clone(), &self.frame_wake).map_err(|error| {
            cmux_ipc::RequestProblem::io(format!("agent pane spawn failed: {error:#}"))
        })?;
        let tree = &mut self.workspaces.items[workspace_index].tree;
        if tree
            .split_leaf(&target, SplitDirection::Right, pane)
            .is_none()
        {
            return Err(cmux_ipc::RequestProblem::io(format!(
                "surface '{target}' cannot split right"
            )));
        }
        self.set_active_focus(new_id.clone());
        let workspace_id = self.workspaces.items[workspace_index].id.clone();
        Ok(serde_json::json!({
            "surface_id": new_id,
            "pane_id": new_id,
            "workspace_id": workspace_id,
            "placement": "split",
            "type": "agent",
        }))
    }

    /// Resolves an optional surface address against the live model:
    /// exact pane id, `surface:<id>`, `surface:<row index>`, or a bare
    /// row index. `None` → the focused pane of the active workspace
    /// (upstream's default; the harness resolves nothing when unset).
    fn resolve_surface(
        &self,
        spec: Option<&str>,
    ) -> Result<(usize, String), cmux_ipc::RequestProblem> {
        let Some(spec) = spec else {
            let index = self.workspaces.active;
            return Ok((index, self.workspaces.items[index].focused.clone()));
        };
        let text = spec.strip_prefix("surface:").unwrap_or(spec);
        // Exact pane id first (ids are unique across workspaces since
        // M4.1's global mint).
        for (index, workspace) in self.workspaces.items.iter().enumerate() {
            if workspace.tree.leaf(text).is_some() {
                return Ok((index, text.to_owned()));
            }
        }
        // A decimal row position: the same order `surface.list` emits
        // (workspaces in list order, panes in on-screen order).
        if let Ok(row) = text.parse::<usize>() {
            let mut seat: usize = 0;
            for (index, workspace) in self.workspaces.items.iter().enumerate() {
                for pane_id in workspace.tree.ordered_leaf_ids() {
                    if seat == row {
                        return Ok((index, pane_id));
                    }
                    seat += 1;
                }
            }
            return Err(cmux_ipc::RequestProblem::not_found(format!(
                "surface row {row} has no live pane (the listing changed)"
            )));
        }
        Err(cmux_ipc::RequestProblem::not_found(format!(
            "surface '{spec}' does not exist"
        )))
    }

    /// The workspaces a `workspace_id`-parameterized listing covers: the
    /// named workspace, or (absent) the active one — upstream filters
    /// listings by the resolved current workspace when the param is
    /// omitted.
    fn workspaces_pass(
        &self,
        target: Option<&str>,
    ) -> Result<Vec<usize>, cmux_ipc::RequestProblem> {
        match target {
            Some(id) => self
                .workspaces
                .items
                .iter()
                .position(|workspace| workspace.id == id)
                .map(|index| vec![index])
                .ok_or_else(|| {
                    cmux_ipc::RequestProblem::not_found(format!("workspace '{id}' does not exist"))
                }),
            None => Ok(vec![self.workspaces.active]),
        }
    }

    /// Surface rows for the covered workspaces. The port maps panes 1:1 to
    /// surfaces; `type` says `terminal` or `browser` (M5.1: browser panes
    /// report the page title once loaded). `index` is the running row
    /// position.
    fn surface_rows(
        &self,
        target: Option<&str>,
    ) -> Result<Vec<serde_json::Value>, cmux_ipc::RequestProblem> {
        let mut rows = Vec::new();
        for index in self.workspaces_pass(target)? {
            let workspace = &self.workspaces.items[index];
            let is_active_workspace = index == self.workspaces.active;
            for pane_id in workspace.tree.ordered_leaf_ids() {
                let focused = is_active_workspace && pane_id == workspace.focused;
                let (title, surface_type) = workspace
                    .tree
                    .leaf(&pane_id)
                    .map(|pane| {
                        (
                            pane.display_title(),
                            match &pane.body {
                                PaneBody::Agent(_) => "agent",
                                _ if pane.body.is_browser() => "browser",
                                _ => "terminal",
                            },
                        )
                    })
                    .unwrap_or((None, "terminal"));
                rows.push(surface_row(
                    rows.len(),
                    &pane_id,
                    workspace.id.as_str(),
                    surface_type,
                    title,
                    focused,
                ));
            }
        }
        Ok(rows)
    }

    /// Pane rows for the covered workspaces (1:1 with surfaces; the row
    /// shapes mirror upstream's pane list so index resolution stays
    /// uniform).
    fn pane_rows(
        &self,
        target: Option<&str>,
    ) -> Result<Vec<serde_json::Value>, cmux_ipc::RequestProblem> {
        let mut rows = Vec::new();
        for index in self.workspaces_pass(target)? {
            let workspace = &self.workspaces.items[index];
            let is_active_workspace = index == self.workspaces.active;
            for pane_id in workspace.tree.ordered_leaf_ids() {
                let focused = is_active_workspace && pane_id == workspace.focused;
                rows.push(pane_row(
                    rows.len(),
                    &pane_id,
                    workspace.id.as_str(),
                    focused,
                ));
            }
        }
        Ok(rows)
    }

    /// The focused-surface snapshot `system.identify` reports (upstream
    /// `v2Identify`'s port subset: ids are the port's own; `caller` and
    /// the bundle/version fields stay as upstream-null/static).
    fn identify_result(&self) -> Result<serde_json::Value, cmux_ipc::RequestProblem> {
        let workspace = self.workspaces.active_workspace();
        let surface_id = workspace.focused.clone();
        let surface_index = workspace
            .tree
            .ordered_leaf_ids()
            .iter()
            .position(|id| *id == surface_id)
            .unwrap_or_default();
        let (surface_type, is_browser_surface, browser) = workspace
            .tree
            .leaf(&surface_id)
            .map(|pane| match &pane.body {
                PaneBody::Browser(lane) => {
                    let facts = serde_json::json!({
                        "url": lane.snapshot.source,
                        "title": lane.snapshot.title,
                    });
                    ("browser", true, facts)
                }
                PaneBody::Agent(_) => ("agent", false, serde_json::Value::Null),
                PaneBody::Terminal(_) => ("terminal", false, serde_json::Value::Null),
            })
            .unwrap_or(("terminal", false, serde_json::Value::Null));
        Ok(serde_json::json!({
            "socket_path": self.ipc_pipe,
            "app": "cmux",
            "version": env!("CARGO_PKG_VERSION"),
            "build": "unknown",
            "caller": serde_json::Value::Null,
            "focused": {
                "workspace_id": workspace.id,
                "window_id": serde_json::Value::Null,
                "pane_id": surface_id,
                "surface_id": surface_id,
                "surface_type": surface_type,
                "is_browser_surface": is_browser_surface,
                "browser": browser,
            },
            "surface_index": surface_index,
        }))
    }

    /// Switches to the workspace with `workspace_id` — the same observable
    /// behavior as `Action::SelectWorkspace` (switch, refocus settle,
    /// viewing-is-mark-read).
    fn select_workspace_by_id(
        &mut self,
        workspace_id: &str,
    ) -> Result<serde_json::Value, cmux_ipc::RequestProblem> {
        let index = self
            .workspaces
            .items
            .iter()
            .position(|workspace| workspace.id == workspace_id)
            .ok_or_else(|| {
                cmux_ipc::RequestProblem::not_found(format!(
                    "workspace '{workspace_id}' does not exist"
                ))
            })?;
        if self.workspaces.select(index) {
            self.settle_focus();
            self.sidebar_scroll_reveal = Some(self.workspaces.active);
        }
        self.clear_alert(index);
        Ok(serde_json::json!({ "workspace_id": workspace_id }))
    }

    /// `workspace.list` rows for the live model (index, id, title, selected).
    fn workspace_rows(&self) -> Vec<serde_json::Value> {
        (0..self.workspaces.items.len())
            .map(|index| {
                let id = self.workspaces.items[index].id.as_str();
                workspace_row(
                    index,
                    id,
                    &self.workspaces.name_of(index),
                    index == self.workspaces.active,
                )
            })
            .collect()
    }

    /// Marks a workspace's notifications read: viewing a workspace is
    /// its mark-read (upstream's workspace-scoped rule). The sidebar
    /// dot clears with it.
    fn clear_alert(&mut self, index: usize) {
        if let Some(slot) = self.workspace_alerts.get_mut(index) {
            *slot = 0;
        }
    }

    /// Mirrors `Workspaces::close`'s removal for the ring: the alert
    /// slot goes away with the workspace it counted (nothing to do when
    /// the window close path keeps the list).
    fn drop_alert_after_close(&mut self, at: usize) {
        if self.workspaces.items.len() > 1 && !self.workspace_alerts.is_empty() {
            self.workspace_alerts
                .remove(at.min(self.workspace_alerts.len() - 1));
        }
    }

    /// Closes one pane inside one workspace: the workspace's last pane
    /// closes the workspace, and the last remaining workspace closes the
    /// window. Keyboard focus follows within the same workspace.
    fn close_pane_in_workspace(&mut self, ws: usize, pane_id: &str) {
        let Some(workspace) = self.workspaces.items.get_mut(ws) else {
            return;
        };
        if workspace.tree.leaf_count() <= 1 {
            eprintln!("cmux-shell: pane {pane_id} closed â†’ workspace closed");
            self.close_browser_hosts_in(ws);
            if self.workspaces.close(ws) {
                eprintln!("cmux-shell: last workspace closed â†’ window close");
                self.pending_window_close = true;
            }
            self.drop_alert_after_close(ws);
            return;
        }
        let workspace = self.workspaces.items.get_mut(ws).expect("checked above");
        // Teardown before the tree drops the pane: an explicit controller
        // Close releases the WebView2 child (the documented shutdown path;
        // the user data folder is kept on disk).
        if let Some(pane) = workspace.tree.leaf_mut(pane_id) {
            if let Some(lane) = pane.browser_lane_mut() {
                if let Some(host) = lane.host.take() {
                    host.close();
                }
            }
        }
        if !workspace.tree.close_leaf(pane_id) {
            return;
        }
        if workspace.focused == pane_id {
            if let Some(survivor) = workspace.tree.ordered_leaf_ids().first().cloned() {
                workspace.focused = survivor.clone();
                if let Some(pane) = workspace.tree.leaf_mut(&survivor) {
                    if let Some(terminal) = pane.terminal_mut() {
                        terminal.request_focus();
                    }
                }
            }
        }
    }

    /// Explicitly closes every browser host inside a workspace about to be
    /// removed (an implicit ref drop also tears the webview down, but the
    /// documented WebView2 shutdown path is an explicit `Close`).
    fn close_browser_hosts_in(&mut self, at: usize) {
        let Some(workspace) = self.workspaces.items.get_mut(at) else {
            return;
        };
        for id in workspace.tree.ordered_leaf_ids() {
            let Some(pane) = workspace.tree.leaf_mut(&id) else {
                continue;
            };
            if let Some(lane) = pane.browser_lane_mut() {
                if let Some(host) = lane.host.take() {
                    host.close();
                }
            }
        }
    }

    /// Exit-time sweep: every browser surface's controller is closed before
    /// the app tears down (the user data folders stay on disk).
    fn close_all_browser_hosts(&mut self) {
        for at in 0..self.workspaces.items.len() {
            self.close_browser_hosts_in(at);
        }
    }

    /// Applies per-frame WebView2 inputs across all workspaces: hosts are
    /// created here (the first frame with a native window), visible leaves
    /// pin their child HWND to the laid-out rect, hidden workspaces park
    /// their panes on `IsVisible(false)`, focus transitions push a
    /// programmatic webview focus, and each pane refreshes its polled
    /// snapshot for rows/identify.
    fn sync_browser_surfaces(&mut self, visible: &[(String, egui::Rect)]) {
        let Some(hwnd) = self.platform_hwnd else {
            return; // no native window yet: nothing to host into
        };
        let dpi_scale = self.dpi_scale;
        let window_pos = self.window_pos;
        let active = self.workspaces.active;
        let router = self.accel_router.clone();
        // Invisible resize bands: a child window answers HTCLIENT for
        // every point it covers, so a WebView2 child pinned to the exact
        // pane rect would starve the top-level's band route at the
        // window-adjacent edges. Edge-adjacent children shrink by the
        // band width (the egui-painted pane chrome stays full-bleed;
        // the band shows the pane fill tone, matching the clearance
        // native windows reserve under overlapping children).
        let band = title_bar_wndproc::resize_band_pts(hwnd);
        let (client_left, _client_top, client_right, client_bottom) =
            title_bar_wndproc::client_rect_pts(hwnd);
        let inset_bands = |rect: egui::Rect| {
            let mut rect = rect;
            if (rect.left() - client_left).abs() <= 0.5 {
                rect.min.x += band;
            }
            if (rect.right() - client_right).abs() <= 0.5 {
                rect.max.x -= band;
            }
            if (rect.bottom() - client_bottom).abs() <= 0.5 {
                rect.max.y -= band;
            }
            rect
        };
        // Hoisted wake wrapper: each host start arms its completion
        // handlers with a prompt-frame signal (M5.2 automation).
        let wake: cmux_webview2::FrameWakeFn = {
            let frame_wake = Arc::clone(&self.frame_wake);
            Arc::new(move || frame_wake.wake_now())
        };
        // The browser child paints pages with no defined background over
        // the WebView2 default background; the resolved viewport fill (the
        // design's `#0c0e14` token) keeps such pages in the chrome palette.
        // Hoisted so the host loop below never re-reads the theme
        // mid-borrow.
        let browser_default_background = self.runtime.theme.viewport_background;
        // Completions drained across all hosts this frame; the state
        // machine runs after the borrow loop ends.
        let mut completions: Vec<cmux_webview2::CdpCompletion> = Vec::new();
        for index in 0..self.workspaces.items.len() {
            let ids = self.workspaces.items[index].tree.ordered_leaf_ids();
            for id in ids {
                let focused = index == active && self.workspaces.items[index].focused == id;
                let visible_rect = visible
                    .iter()
                    .find(|(visible_id, _)| *visible_id == id)
                    .map(|(_, rect)| *rect);
                let Some(pane) = self.workspaces.items[index].tree.leaf_mut(&id) else {
                    continue;
                };
                let Some(lane) = pane.browser_lane_mut() else {
                    continue;
                };
                let host = match lane.host.clone() {
                    Some(host) => host,
                    None => {
                        let spec = cmux_webview2::BrowserSpec::new(
                            id.clone(),
                            browser_user_data_dir(&user_data_base(), id.as_str()),
                            lane.start_url.clone(),
                            browser_default_background,
                        );
                        eprintln!(
                            "cmux-shell: browser surface {id} starting ({})",
                            lane.start_url
                        );
                        let host = cmux_webview2::BrowserHost::start(
                            spec,
                            hwnd,
                            Some(&router),
                            wake.clone(),
                        );
                        lane.host = Some(host.clone());
                        host
                    }
                };
                let params = cmux_webview2::TickParams {
                    bounds: visible_rect.map(|rect| {
                        let rect = inset_bands(rect);
                        cmux_webview2::Rect::new(
                            f64::from(rect.left()),
                            f64::from(rect.top()),
                            f64::from(rect.right()),
                            f64::from(rect.bottom()),
                        )
                    }),
                    dpi_scale,
                    visible: visible_rect.is_some(),
                    window_pos,
                    focused,
                };
                let (phase, snapshot) = host.tick(&params);
                lane.phase = phase;
                lane.snapshot = snapshot;
                lane.failure = if matches!(phase, cmux_webview2::BrowserPhase::Failed) {
                    host.failure()
                } else {
                    None
                };
                completions.extend(host.take_cdp_completions());
            }
        }
        for completion in completions {
            self.advance_cdp(completion);
        }
    }

    fn drain_actions(&mut self) {
        for action in std::mem::take(&mut self.actions) {
            self.perform(action);
        }
        // A palette jump that crossed workspaces lands its focus once the
        // workspace switch took (lane 3; the jump pane id must exist in
        // the now-active workspace).
        if let Some(pane) = self.pending_jump.take() {
            if self
                .workspaces
                .active_workspace()
                .tree
                .leaf(&pane)
                .is_some()
            {
                self.set_active_focus(pane);
            }
        }
    }

    fn perform(&mut self, action: Action) {
        match action {
            Action::SplitFocused(direction) => {
                let active_index = self.workspaces.active;
                let target = self.workspaces.items[active_index].focused.clone();
                // Globally minted: ids address panes from the IPC surface,
                // so they are unique across workspaces (M4.1 identity).
                let new_id = mint_pane_id();
                // The pane id exists before spawn; identity env is part of
                // the spec (M3.3 slice A).
                let spec = identity_spec(
                    shell_spec(&self.runtime),
                    new_id.as_str(),
                    active_index,
                    self.ipc_pipe.as_str(),
                );
                match new_pane(new_id.clone(), spec, &self.frame_wake) {
                    Ok(pane) => {
                        let tree = &mut self.workspaces.items[active_index].tree;
                        if tree.split_leaf(&target, direction, pane).is_some() {
                            self.set_active_focus(new_id);
                        }
                    }
                    Err(error) => eprintln!("failed to spawn split pane: {error:#}"),
                }
            }
            Action::CloseFocused => {
                let active = self.workspaces.active;
                let id = self.workspaces.active_workspace().focused.clone();
                self.close_pane_in_workspace(active, &id);
            }
            Action::Cycle { forward } => {
                let active = self.workspaces.active;
                let ids = self.workspaces.items[active].tree.ordered_leaf_ids();
                let focused = self.workspaces.items[active].focused.clone();
                if let Some(target) =
                    PaneCycleNavigator.target_pane(&ids, &ids, Some(&focused), forward)
                {
                    self.set_active_focus(target);
                }
            }
            Action::EqualizeAll => {
                self.workspaces.active_workspace_mut().tree.equalize(None);
            }
            Action::CommandPalette => {
                if self.palette_open {
                    self.palette_open = false;
                } else {
                    self.open_palette();
                }
            }
            Action::Resize(direction) => {
                let step = self.runtime.resize_step;
                let target = self.workspaces.active_workspace().focused.clone();
                let tree = &mut self.workspaces.active_workspace_mut().tree;
                tree.resize_pane(&target, direction, step);
            }
            Action::NewWorkspace => {
                if let Err(error) = self.spawn_new_workspace() {
                    eprintln!("failed to spawn a new workspace: {error:#}");
                }
            }
            Action::CloseWorkspace { at } => {
                self.close_browser_hosts_in(at);
                if self.workspaces.close(at) {
                    eprintln!("cmux-shell: last workspace closed â†’ window close");
                    self.pending_window_close = true;
                }
                self.drop_alert_after_close(at);
            }
            Action::CloseActiveWorkspace => {
                let at = self.workspaces.active;
                let pane_count = self.workspaces.items[at].tree.leaf_count();
                eprintln!(
                    "cmux-shell: closing workspace {} ({} pane(s))",
                    at + 1,
                    pane_count
                );
                self.close_browser_hosts_in(at);
                if self.workspaces.close(at) {
                    eprintln!("cmux-shell: last workspace closed â†’ window close");
                    self.pending_window_close = true;
                }
                self.drop_alert_after_close(at);
            }
            Action::SelectWorkspace(index) => {
                if self.workspaces.select(index) {
                    self.settle_focus();
                    self.sidebar_scroll_reveal = Some(self.workspaces.active);
                }
                // Viewing a workspace is its mark-read (M3.1).
                self.clear_alert(index);
            }
            Action::CycleWorkspace { forward } => {
                self.workspaces.cycle(forward);
                self.settle_focus();
                self.clear_alert(self.workspaces.active);
                self.sidebar_scroll_reveal = Some(self.workspaces.active);
            }
            Action::ReorderWorkspace { from, to } => {
                if self.workspaces.items.len() > 1 {
                    self.workspaces.reorder(from, to);
                    self.sidebar_scroll_reveal = Some(self.workspaces.active);
                }
            }
            Action::MoveActiveWorkspace { up } => {
                let count = self.workspaces.items.len();
                let from = self.workspaces.active;
                if count > 1 {
                    let to = if up {
                        from.saturating_sub(1)
                    } else {
                        (from + 1).min(count - 1)
                    };
                    if to != from {
                        self.workspaces.reorder(from, to);
                    }
                }
            }
            Action::ToggleSidebar => {
                self.sidebar_visible = !self.sidebar_visible;
            }
        }
    }

    /// Spawns a fresh workspace (one terminal pane) and makes it active —
    /// the body of upstream's `newTab`. `Action::NewWorkspace` drops the
    /// ids; the M4.1 `workspace.create` app call reports them.
    fn spawn_new_workspace(&mut self) -> anyhow::Result<(String, String)> {
        self.spawn_workspace_with_spec(shell_spec(&self.runtime), None, true)
    }

    /// A new workspace whose single pane runs `spec` — the configured
    /// shell for `workspace.create`, `ssh.exe` for `workspace.ssh.open`
    /// (M6, ADR-0014). `title_override` is the pane's app-known identity
    /// (SSH destinations, `--name`), authoritative over child-set OSC
    /// titles; `focus` mirrors upstream's handoff (`false` pushes the
    /// workspace without switching to it).
    fn spawn_workspace_with_spec(
        &mut self,
        spec: PtySpec,
        title_override: Option<String>,
        focus: bool,
    ) -> anyhow::Result<(String, String)> {
        let workspace_index = self.workspaces.items.len();
        let pane_id = mint_pane_id();
        let spec = identity_spec(
            spec,
            pane_id.as_str(),
            workspace_index,
            self.ipc_pipe.as_str(),
        );
        let mut pane = new_pane_with_focus(pane_id.clone(), spec, focus, &self.frame_wake)?;
        pane.title_override = title_override;
        let workspace_id = mint_workspace_id();
        self.workspaces.push_with_focus(
            Workspace {
                id: workspace_id.clone(),
                tree: PaneTree::new(pane),
                focused: pane_id.clone(),
                custom_title: None,
            },
            focus,
        );
        self.workspace_alerts.push(0);
        if focus {
            self.settle_focus();
            self.sidebar_scroll_reveal = Some(self.workspaces.active);
        }
        Ok((workspace_id, pane_id))
    }

    /// The snapshot this state serializes to on exit: every workspace with
    /// its tree (pane order, split geometry, each pane's tab membership â€”
    /// the rendered strip arrives with M2.4b+) and focused pane, plus the
    /// active index.
    fn build_session_snapshot(&self) -> AppSessionSnapshot {
        let mut workspaces: Vec<WorkspaceSnapshot> = Vec::new();
        for workspace in self.workspaces.items.iter() {
            let outline = workspace.tree.root_outline();
            let focused = workspace.focused.clone();
            let tabs_of = |id: &str| -> Vec<String> {
                match workspace.tree.leaf(id) {
                    Some(pane) => {
                        let mut tabs = pane.panel_ids.clone();
                        if tabs.is_empty() {
                            tabs.push(id.to_owned());
                        }
                        tabs
                    }
                    None => vec![id.to_owned()],
                }
            };
            // Browser panes persist their live page URL so restore reopens
            // them (M5.1); terminal panes save None.
            let url_of = |id: &str| -> Option<String> {
                workspace.tree.leaf(id).and_then(|pane| match &pane.body {
                    PaneBody::Browser(lane) if !lane.start_url.is_empty() => {
                        Some(lane.start_url.clone())
                    }
                    _ => None,
                })
            };
            // Lane 5: the agent activity pane declares its own panel kind
            // so the snapshot stays truthful about what backs each leaf
            // (an agent pane must not restore as a shell).
            let kind_of = |id: &str| -> &'static str {
                match workspace.tree.leaf(id) {
                    Some(pane) => match &pane.body {
                        PaneBody::Agent(_) => "agent",
                        _ => "terminal",
                    },
                    None => "terminal",
                }
            };
            workspaces.push(workspace_from_outline(
                &outline,
                &tabs_of,
                &url_of,
                &kind_of,
                &focused,
                &workspace.id,
                workspace.custom_title.as_deref(),
            ));
        }
        let window = WindowSnapshot {
            window_id: None,
            tab_manager: TabManagerSnapshot {
                selected_workspace_index: Some(self.workspaces.active as u32),
                workspaces,
            },
        };
        AppSessionSnapshot::new_current(vec![window])
    }
}

/// Pure decision of the model-focus reconcile: which pane (if any) this
/// frame settles model focus to. Inputs: the frame's explicit click claim,
/// the frame's standing egui focus, the model's current focus, and the
/// previous frame's egui focus. (1) An explicit click claims focus
/// authoritatively, even when an egui focus TRANSITION is absent: clicking
/// the terminal widget that was already egui-focused produces standing
/// focus without a transition, so rule (2) alone would eat the click —
/// e.g. a browser leaf holding model focus while a terminal keeps egui's
/// standing keyboard focus (the M5.1 accepted corner, closed by these
/// unit-tested corners). (2) Chromium-side checks without a click follow
/// only an egui focus change against the PREVIOUS frame: a BROWSER leaf
/// has no egui widget that could hold standing focus, so this must not
/// steal model focus that `open_split` assigned (M5.1 evidence: identify
/// reported the terminal one frame after the split, with no logged focus
/// change); a click frame with a target subsumes (2) by returning early.
fn focus_reconcile_target(
    clicked_now: Option<&str>,
    focused_now: Option<&str>,
    active_focused: &str,
    last_egui_focus: Option<&str>,
) -> Option<String> {
    if let Some(clicked) = clicked_now.filter(|id| *id != active_focused) {
        return Some(clicked.to_owned());
    }
    focused_now
        .filter(|id| *id != active_focused && Some(*id) != last_egui_focus)
        .map(str::to_owned)
}

/// Renders one pane inside its laid-out rect. `focused_now` records which
/// pane egui currently focuses (the terminal widget focuses itself on
/// click/drag), so the model can follow the keyboard afterwards; a drag or
/// click on an already-focused widget produces no focus TRANSITION, so the
/// terminal branch also reports explicit clicks through `clicked_now` —
/// the model's authoritative click-claim (M5.2). When `surrender` is set
/// (the model focus leaf is a browser pane), any egui-focus this widget
/// holds — including a remounted widget's egui-memory restore — is
/// dropped: the keyboard lives in the webview child, and the transition
/// reconcile must not "follow" that phantom. A real click still reports
/// through `clicked_now`, and `set_active_focus` re-arms the terminal's
/// pending focus, so a click-claim recovers its widget's focus in one
/// frame. Browser panes paint nothing (the child HWND covers the rect)
/// and join the focus model through the click interceptor below.
/// The per-frame paint state that is identical for every pane: the shared
/// terminal style, the design viewport fill behind the cells, the click/
/// focus claims collected across the frame, and the browser-focus rule.
/// Lane 5: one frame's collection of the painting workspace's agent facts
/// — the retained cards plus the live turn boundaries (the design's
/// "active step" state). Computed once per frame at the pane loop.
struct AgentStreamSnapshot {
    /// Completed event cards, oldest first (bounded at ingest).
    cards: Vec<cmux_shell::agent_cards::AgentCard>,
    /// One pending boundary per terminal pane (the active card).
    pending: Vec<AgentPendingFact>,
    /// The frame's clock (the cards' elapsed times anchor here).
    now: Instant,
}

/// The live boundary facts backing one active card.
struct AgentPendingFact {
    /// The detecting agent's display name (the card's uppercase label).
    agent_name: String,
    /// When the boundary arrived (`deadline - delay`: both stored).
    seen: Instant,
    /// The pane's last visible terminal rows (the design's inline
    /// terminal block), bounded.
    tail: Vec<String>,
}

/// How many visible terminal rows the active card's inline block shows.
const AGENT_CARD_TAIL_ROWS: usize = 6;

/// The active card's inline block: the pane's last visible rows, leading
/// blank lines dropped.
fn tail_rows(screen: &str, keep: usize) -> Vec<String> {
    let lines: Vec<&str> = screen.lines().collect();
    let start = lines.len().saturating_sub(keep);
    lines[start..]
        .iter()
        .map(|line| line.trim_end().to_owned())
        .skip_while(|line| line.is_empty())
        .collect()
}

struct PanePaintScope<'a> {
    style: &'a TerminalStyle,
    viewport_fill: egui::Color32,
    focused_now: &'a mut Option<String>,
    clicked_now: &'a mut Option<String>,
    surrender: bool,
    /// Lane 5: this frame's agent-stream facts for the painting
    /// workspace (cards + pending turn boundaries).
    agent_stream: Option<&'a AgentStreamSnapshot>,
}

fn render_pane(
    tree: &mut PaneTree<Pane>,
    ui: &mut egui::Ui,
    pane_id: &str,
    scope: &mut PanePaintScope,
) {
    let Some(pane) = tree.leaf_mut(pane_id) else {
        return;
    };
    let PanePaintScope {
        style,
        viewport_fill,
        focused_now,
        clicked_now,
        surrender,
        agent_stream,
    } = scope;
    match &mut pane.body {
        PaneBody::Terminal(lane) => {
            // The design's terminal viewport is a distinct, darker layer
            // behind the text cells (which draw transparent until a
            // program sets their background). Rounded like the ring so
            // the gutter corners stay clean.
            ui.painter().rect_filled(
                ui.max_rect(),
                CornerRadius::same(PANE_ROUNDING),
                *viewport_fill,
            );
            let response = lane.terminal.ui(ui, style);
            // M6 accessibility: egui_tty provides no widget info itself;
            // label the pane so the platform a11y tree sees it (the buffer
            // content itself is not yet exposed - ADR-0015 residual).
            response.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Label, true, "Terminal".to_owned())
            });
            if response.clicked() {
                **clicked_now = Some(pane_id.to_owned());
            }
            if response.has_focus() {
                if *surrender {
                    response.surrender_focus();
                } else {
                    **focused_now = Some(pane_id.to_owned());
                }
            }
        }
        PaneBody::Agent(agent_lane) => {
            // Lane 5: pure painting, with a click interceptor covering
            // focus the way browser panes do (the stream takes no
            // keyboard in this milestone; the terminal panes beside it
            // keep typing).
            let response = ui.interact(
                ui.max_rect(),
                egui::Id::new(("agent-pane", pane_id)),
                egui::Sense::click(),
            );
            // M6 accessibility: label the region for the platform tree
            // (card texts stay visual pixels — the ADR-0015 residual).
            response.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::Label,
                    true,
                    "Agent activity".to_owned(),
                )
            });
            if let Some(stream) = agent_stream {
                paint_agent_stream(ui, agent_lane, stream);
            }
            if response.clicked() {
                **focused_now = Some(pane_id.to_owned());
                **clicked_now = Some(pane_id.to_owned());
            }
            if response.contains_pointer() {
                let dy = ui.input(|i| i.smooth_scroll_delta()).y;
                if dy != 0.0 {
                    agent_lane.scroll_from_bottom = (agent_lane.scroll_from_bottom + dy).max(0.0);
                }
            }
        }
        PaneBody::Browser(browser_lane) => {
            // The WebView2 child window draws over this rect; egui only
            // needs a click interceptor so a mouse click models focus
            // (native keyboard focus follows the click into the webview).
            let response = ui.interact(
                ui.max_rect(),
                egui::Id::new(("browser-pane", pane_id)),
                egui::Sense::click(),
            );
            // M6 accessibility: the browser region is a focus-carrying
            // interceptor (WebView2 natively covers the surface); a label
            // keeps it identifiable in the platform a11y tree.
            response.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::Button,
                    true,
                    "Embedded browser".to_owned(),
                )
            });
            if response.clicked() {
                **focused_now = Some(pane_id.to_owned());
                // Native focus lands inside the webview; route the model
                // the same way a terminal click does (M5.2 click claims).
                **clicked_now = Some(pane_id.to_owned());
            }
            // Failure overlay (e.g. WebView2 runtime missing): the reason
            // paints in the dead pane until it closes; the host stays
            // failed permanently.
            if matches!(browser_lane.phase, cmux_webview2::BrowserPhase::Failed) {
                let message = browser_lane
                    .failure
                    .as_deref()
                    .unwrap_or("browser unavailable");
                ui.painter().rect_filled(
                    ui.max_rect(),
                    0.0,
                    egui::Color32::from_rgb(0x28, 0x14, 0x14),
                );
                ui.painter().text(
                    ui.max_rect().center(),
                    egui::Align2::CENTER_CENTER,
                    format!("browser pane unavailable\n{message}"),
                    egui::FontId::proportional(14.0),
                    egui::Color32::from_rgb(0xE6, 0x9C, 0x9C),
                );
            }
        }
    }
}

// ===== Lane 5 painters: the agent activity stream (design pane 1:244
// with stream 1:257) =====

/// The tab row height (design 19.67pt, the pane's own tab).
const AGENT_TAB_H: f32 = 20.0;
/// The stream's card gutter (design pad/gap 8pt).
const AGENT_STREAM_INSET: f32 = 8.0;
const AGENT_CARD_GAP: f32 = 8.0;
const AGENT_CARD_RADIUS: f32 = 4.0;
const AGENT_CARD_PAD: f32 = 6.0;
const AGENT_ACTIVE_PAD: f32 = 8.0;
const AGENT_ACTIVE_INSET: f32 = 8.0;
const AGENT_SECTION_GAP: f32 = 6.0;
const AGENT_TITLE_LEADING: f32 = 18.0;
const AGENT_TITLE_GAP: f32 = 2.0;
const AGENT_LABEL_LEADING: f32 = 14.0;
const AGENT_TAIL_LEADING: f32 = 18.0;
const AGENT_ICON_W: f32 = 12.5;
const AGENT_ICON_H: f32 = 14.5;
const AGENT_ICON_GAP: f32 = 8.0;
const AGENT_CHIP_H: f32 = 14.0;
const AGENT_CHIP_PAD: f32 = 4.0;

/// Paints the agent pane's body: the design tab row over the stream
/// card stack. All inks/points are the pinned design specs (docs record
/// the adaptations: port font stack, no letter tracking, the agent
/// glyph hand-drawn, footer parts that the port data lacks recorded as
/// residuals).
fn paint_agent_stream(
    ui: &mut egui::Ui,
    lane: &mut AgentActivityPane,
    stream: &AgentStreamSnapshot,
) {
    let tab_bg = egui::Color32::from_rgb(0x28, 0x2A, 0x31);
    let tab_ink = egui::Color32::from_rgb(0xE2, 0xE2, 0xEB);
    let accent = egui::Color32::from_rgb(0xFF, 0xC4, 0x2F);
    let stream_bg = egui::Color32::from_rgb(0x0C, 0x0E, 0x14);

    let rect = ui.max_rect();
    let painter = ui.painter();

    // Tab row (node 1:245): fill + the hand-drawn agent glyph + title +
    // the warm chip while a boundary is pending.
    let tab_rect =
        egui::Rect::from_min_size(rect.left_top(), egui::vec2(rect.width(), AGENT_TAB_H));
    painter.rect_filled(tab_rect, 0.0, tab_bg);
    let glyph = egui::Stroke::new(1.2, tab_ink);
    let glyph_left = tab_rect.left() + AGENT_STREAM_INSET;
    let glyph_top = tab_rect.center().y - 5.5;
    let glyph_bottom = tab_rect.center().y + 5.5;
    let glyph_right = glyph_left + 5.0;
    painter.line_segment(
        [
            egui::pos2(glyph_left, glyph_top),
            egui::pos2(glyph_left, glyph_bottom),
        ],
        glyph,
    );
    painter.line_segment(
        [
            egui::pos2(glyph_right, glyph_top),
            egui::pos2(glyph_right, glyph_bottom),
        ],
        glyph,
    );
    painter.line_segment(
        [
            egui::pos2(glyph_left, glyph_top),
            egui::pos2(glyph_right, glyph_top),
        ],
        glyph,
    );
    painter.line_segment(
        [
            egui::pos2(glyph_left, glyph_bottom),
            egui::pos2(glyph_right, glyph_bottom),
        ],
        glyph,
    );
    painter.text(
        egui::pos2(glyph_right + 4.0, tab_rect.center().y),
        egui::Align2::LEFT_CENTER,
        "Agent activity",
        egui::FontId::proportional(12.0),
        tab_ink,
    );
    if let Some(first_pending) = stream.pending.first() {
        let chip_text = first_pending.agent_name.to_uppercase();
        let chip_font = egui::FontId::proportional(11.0);
        let chip_w = painter
            .layout_no_wrap(chip_text.clone(), chip_font.clone(), accent)
            .size()
            .x
            + 2.0 * AGENT_CHIP_PAD;
        let chip_rect = egui::Rect::from_min_size(
            egui::pos2(
                tab_rect.right() - 4.0 - chip_w,
                tab_rect.center().y - AGENT_CHIP_H / 2.0,
            ),
            egui::vec2(chip_w, AGENT_CHIP_H),
        );
        painter.rect_filled(
            chip_rect,
            AGENT_CARD_RADIUS,
            egui::Color32::from_rgb(0x60, 0x50, 0x22),
        );
        painter.text(
            chip_rect.center(),
            egui::Align2::CENTER_CENTER,
            chip_text,
            chip_font,
            accent,
        );
    }

    // Stream body (node 1:257): the dark fill behind the card stack.
    let body_rect = egui::Rect::from_min_max(
        egui::pos2(rect.left(), rect.top() + AGENT_TAB_H),
        egui::pos2(rect.right(), rect.bottom()),
    );
    painter.rect_filled(body_rect, 0.0, stream_bg);

    let content_width = body_rect.width() - 2.0 * AGENT_STREAM_INSET;
    let viewport_height = body_rect.height() - 2.0 * AGENT_STREAM_INSET;
    let body_painter = painter.with_clip_rect(body_rect);
    if content_width <= 0.0 || viewport_height <= 0.0 {
        return;
    }
    let x = body_rect.left() + AGENT_STREAM_INSET;

    // Measure pass: text extents decide each card's height.
    let card_text_width = content_width - 2.0 * AGENT_CARD_PAD - AGENT_ICON_W - AGENT_ICON_GAP;
    let measured: Vec<f32> = stream
        .cards
        .iter()
        .map(|card| {
            let detail = body_painter.layout(
                card.detail(),
                egui::FontId::proportional(12.0),
                egui::Color32::from_rgb(0xBD, 0xC8, 0xD1),
                card_text_width.max(1.0),
            );
            2.0 * AGENT_CARD_PAD + AGENT_TITLE_LEADING + AGENT_TITLE_GAP + detail.size().y
        })
        .collect();
    let active_measured: Vec<f32> = stream
        .pending
        .iter()
        .map(|fact| {
            let block_h =
                2.0 * AGENT_ACTIVE_INSET + fact.tail.len().max(1) as f32 * AGENT_TAIL_LEADING;
            2.0 * AGENT_ACTIVE_PAD
                + AGENT_LABEL_LEADING
                + AGENT_SECTION_GAP
                + block_h
                + AGENT_SECTION_GAP
                + AGENT_TITLE_LEADING
        })
        .collect();

    // Bottom-anchored viewport: clamp the scroll before painting.
    let cards_total: f32 =
        measured.iter().sum::<f32>() + measured.len().saturating_sub(1) as f32 * AGENT_CARD_GAP;
    let join_gap: f32 = if !stream.cards.is_empty() && !stream.pending.is_empty() {
        AGENT_CARD_GAP
    } else {
        0.0
    };
    let active_total: f32 = active_measured.iter().sum::<f32>()
        + active_measured.len().saturating_sub(1) as f32 * AGENT_CARD_GAP;
    let content_total = cards_total + join_gap + active_total;
    let max_scroll = (content_total - viewport_height).max(0.0);
    if lane.scroll_from_bottom > max_scroll {
        lane.scroll_from_bottom = max_scroll;
    }

    let bottom_y = body_rect.bottom() - AGENT_STREAM_INSET - lane.scroll_from_bottom;
    let mut y = bottom_y - content_total;
    for (card, height) in stream.cards.iter().zip(&measured) {
        let frame = egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(content_width, *height));
        paint_complete_card(&body_painter, card, frame, stream.now);
        y += height + AGENT_CARD_GAP;
    }
    if join_gap > 0.0 {
        y += join_gap;
    }
    for (fact, height) in stream.pending.iter().zip(&active_measured) {
        let frame = egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(content_width, *height));
        paint_active_card(&body_painter, fact, frame, stream.now);
        y += height + AGENT_CARD_GAP;
    }
}

/// One completed card (stream node 1:258): circle-check glyph, headline,
/// the event's elapsed time, and the detail body wrapped to the column.
fn paint_complete_card(
    painter: &egui::Painter,
    card: &cmux_shell::agent_cards::AgentCard,
    frame: egui::Rect,
    stream_now: Instant,
) {
    painter.rect_filled(
        frame,
        AGENT_CARD_RADIUS,
        egui::Color32::from_rgb(0x19, 0x1B, 0x22),
    );
    let content_left = frame.left() + AGENT_CARD_PAD;
    let content_top = frame.top() + AGENT_CARD_PAD;
    let muted = egui::Color32::from_rgb(0x87, 0x92, 0x9A);
    // The design's circle-check in the 12.5x14.5 slot (hand-drawn).
    let icon_center = egui::pos2(
        content_left + AGENT_ICON_W / 2.0,
        content_top + AGENT_ICON_H / 2.0,
    );
    painter.circle_stroke(icon_center, 5.0, egui::Stroke::new(1.5, muted));
    painter.line_segment(
        [
            egui::pos2(icon_center.x - 2.2, icon_center.y),
            egui::pos2(icon_center.x - 0.4, icon_center.y + 1.9),
        ],
        egui::Stroke::new(1.3, muted),
    );
    painter.line_segment(
        [
            egui::pos2(icon_center.x - 0.4, icon_center.y + 1.9),
            egui::pos2(icon_center.x + 2.4, icon_center.y - 1.7),
        ],
        egui::Stroke::new(1.3, muted),
    );
    let title_y = content_top + AGENT_TITLE_LEADING / 2.0;
    painter.text(
        egui::pos2(content_left + AGENT_ICON_W + AGENT_ICON_GAP, title_y),
        egui::Align2::LEFT_CENTER,
        card.headline(),
        egui::FontId::proportional(12.0),
        egui::Color32::from_rgb(0xE2, 0xE2, 0xEB),
    );
    painter.text(
        egui::pos2(frame.right() - AGENT_CARD_PAD, title_y),
        egui::Align2::RIGHT_CENTER,
        format!(
            "{:.2}s",
            stream_now.saturating_duration_since(card.at).as_secs_f64()
        ),
        egui::FontId::proportional(11.0),
        muted,
    );
    let text_x = content_left + AGENT_ICON_W + AGENT_ICON_GAP;
    let detail_width = frame.width() - 2.0 * AGENT_CARD_PAD - AGENT_ICON_W - AGENT_ICON_GAP;
    let galley = painter.layout(
        card.detail(),
        egui::FontId::proportional(12.0),
        egui::Color32::from_rgb(0xBD, 0xC8, 0xD1),
        detail_width.max(1.0),
    );
    painter.galley(
        egui::pos2(text_x, content_top + AGENT_TITLE_LEADING + AGENT_TITLE_GAP),
        galley,
        egui::Color32::from_rgb(0xBD, 0xC8, 0xD1),
    );
}

/// The active card (node 1:299): 1px accent ring, running dot + the
/// agent label, the pane's last visible terminal rows on the dark block,
/// and the real pending age as the footer.
fn paint_active_card(
    painter: &egui::Painter,
    fact: &AgentPendingFact,
    frame: egui::Rect,
    stream_now: Instant,
) {
    let accent = egui::Color32::from_rgb(0xFF, 0xC4, 0x2F);
    painter.rect_filled(
        frame,
        AGENT_CARD_RADIUS,
        egui::Color32::from_rgb(0x1D, 0x1F, 0x26),
    );
    painter.rect_stroke(
        frame,
        AGENT_CARD_RADIUS,
        egui::Stroke::new(1.0, accent),
        egui::StrokeKind::Inside,
    );
    let header_y = frame.top() + AGENT_ACTIVE_PAD + AGENT_LABEL_LEADING / 2.0;
    let dot = egui::pos2(frame.left() + AGENT_ACTIVE_PAD + 4.0, header_y);
    painter.circle_filled(dot, 4.0, accent);
    painter.text(
        egui::pos2(dot.x + 6.0, header_y),
        egui::Align2::LEFT_CENTER,
        fact.agent_name.to_uppercase(),
        egui::FontId::proportional(11.0),
        accent,
    );
    painter.text(
        egui::pos2(frame.right() - AGENT_ACTIVE_PAD, header_y),
        egui::Align2::RIGHT_CENTER,
        "RUNNING",
        egui::FontId::proportional(12.0),
        accent,
    );
    let block_rect = egui::Rect::from_min_max(
        egui::pos2(
            frame.left() + AGENT_ACTIVE_PAD,
            frame.top() + AGENT_ACTIVE_PAD + AGENT_LABEL_LEADING + AGENT_SECTION_GAP,
        ),
        egui::pos2(
            frame.right() - AGENT_ACTIVE_PAD,
            frame.bottom() - AGENT_ACTIVE_PAD - AGENT_SECTION_GAP - AGENT_TITLE_LEADING,
        ),
    );
    painter.rect_filled(
        block_rect,
        AGENT_CARD_RADIUS,
        egui::Color32::from_rgb(0x0C, 0x0E, 0x14),
    );
    for (row_index, row) in fact.tail.iter().enumerate() {
        painter.galley(
            egui::pos2(
                block_rect.left() + AGENT_ACTIVE_INSET,
                block_rect.top() + AGENT_ACTIVE_INSET + row_index as f32 * AGENT_TAIL_LEADING,
            ),
            painter.layout_no_wrap(
                row.clone(),
                egui::FontId::monospace(12.0),
                egui::Color32::from_rgb(0xE2, 0xE2, 0xEB),
            ),
            egui::Color32::from_rgb(0xE2, 0xE2, 0xEB),
        );
    }
    painter.text(
        egui::pos2(
            frame.right() - AGENT_ACTIVE_PAD,
            frame.bottom() - AGENT_ACTIVE_PAD - AGENT_TITLE_LEADING / 2.0,
        ),
        egui::Align2::RIGHT_CENTER,
        format!(
            "{:.2}s",
            stream_now
                .saturating_duration_since(fact.seen)
                .as_secs_f64()
        ),
        egui::FontId::proportional(12.0),
        egui::Color32::from_rgb(0x87, 0x92, 0x9A),
    );
}

impl App {
    /// Lane 5: collects the painting workspace's agent facts for the pane
    /// loop — the bounded card slot plus each terminal pane's pending turn
    /// boundary (arrival = `deadline - delay`; both stored). Boundaries
    /// carry the screen tails frozen when they were seen, so this pass is
    /// a read-only walk over the model.
    fn agent_stream_snapshot(&self) -> AgentStreamSnapshot {
        let index = self.workspaces.active;
        let cards = self.agent_cards.cards(index).cloned().collect();
        let Some(workspace) = self.workspaces.items.get(index) else {
            return AgentStreamSnapshot {
                cards,
                pending: Vec::new(),
                now: Instant::now(),
            };
        };
        let mut pending = Vec::new();
        for id in workspace.tree.ordered_leaf_ids() {
            let Some(pane) = workspace.tree.leaf(&id) else {
                continue;
            };
            let PaneBody::Terminal(lane) = &pane.body else {
                continue;
            };
            for pending_turn in lane.pending_turns.values() {
                let seen = pending_turn
                    .deadline
                    .checked_sub(pending_turn.pending.delay)
                    .unwrap_or_else(Instant::now);
                // The tail froze into the PendingTurn at boundary-see time;
                // the pass never reads the terminal here.
                let tail = pending_turn.tail.clone();
                pending.push(AgentPendingFact {
                    agent_name: pending_turn.pending.agent_name.to_owned(),
                    seen,
                    tail,
                });
            }
        }
        AgentStreamSnapshot {
            cards,
            pending,
            now: Instant::now(),
        }
    }
}

/// Settings resolved at startup from the config file plus built-in
/// defaults; read-only once handed to the app.
struct RuntimeSettings {
    /// Config `appearance.fontMagnificationPercent`.
    font_percent: i32,
    /// Config `panes.gutter`.
    pane_gutter: f64,
    /// Config `panes.resizeStep`.
    resize_step: u16,
    /// Config `terminal.shell`; `None` keeps the Windows default shell.
    shell: Option<cmux_config::ShellCommand>,
    /// Config `theme` subset, kept as engine-independent data.
    theme: ThemeRuntime,
    /// Config `notifications` subset (M3.2a policy inputs; M3.2b keys).
    notifications: NotificationSettings,
    /// Keybinding table in matching order.
    bindings: Vec<(KeyCombo, Action)>,
}

/// The resolved `notifications` config: upstream's two booleans as raw
/// settings (both default `false`, verified upstream defaults).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct NotificationSettings {
    suppress_when_app_focused: bool,
    sound_when_focused: bool,
}

fn notification_settings_from_config(
    config: &cmux_config::NotificationsConfig,
) -> NotificationSettings {
    NotificationSettings {
        suppress_when_app_focused: config.suppress_when_app_focused.unwrap_or(false),
        sound_when_focused: config.sound_when_focused.unwrap_or(false),
    }
}

/// The resolved `theme` config: terminal scheme + chrome colors as raw
/// `0xRRGGBB` values (converted to egui colors at the frame boundary so
/// this stays UI-library-free). `scheme: None` keeps the scheme derived
/// from egui's visuals; the defaults are the Figma design tokens
/// (`docs/research/figma-design-reference.md` section 2, lane 1 of the
/// design-fidelity ladder).
struct ThemeRuntime {
    /// `Some(true)` = light terminals, `Some(false)` = dark.
    scheme: Option<bool>,
    /// Main area/gutter fill (`#11131a`).
    background: u32,
    /// Sidebar strip fill (`#191b22` — the design lights the strip above
    /// the main background, opposite of the old darker-shade policy;
    /// a configured `theme.background` still derives the darker shade).
    sidebar_background: u32,
    /// Pane interior fill behind terminal cells (`#0c0e14`, the design's
    /// darkest layer); browser panes never paint it (the child covers).
    viewport_background: u32,
    /// Chip/pill/active-row fill (`#1d1f26`); every chrome chip paints
    /// through this field. Design-fidelity round 1 (2026-10-26): the chip
    /// sites used to read the raw token and split the chrome into two
    /// schemes the moment a `theme.background` was configured.
    chip_background: u32,
    /// Terminal cursor color override.
    cursor: Option<u32>,
}

impl Default for ThemeRuntime {
    fn default() -> Self {
        Self {
            scheme: None,
            background: TOKEN_MAIN_BG,
            sidebar_background: TOKEN_SIDEBAR_BG,
            viewport_background: TOKEN_VIEWPORT_BG,
            chip_background: TOKEN_CHIP_BG,
            cursor: None,
        }
    }
}

impl RuntimeSettings {
    fn from_config(config: &Config) -> Self {
        Self {
            font_percent: config.appearance.font_magnification_percent,
            pane_gutter: config.panes.gutter,
            resize_step: config.panes.resize_step,
            shell: config.terminal.shell.clone(),
            theme: theme_from_config(&config.theme),
            notifications: notification_settings_from_config(&config.notifications),
            bindings: build_bindings(&config.shortcuts.bindings),
        }
    }

    /// Terminal font scale (50â€“300% of egui's 12px monospace base).
    fn font_scale(&self) -> f32 {
        f32::from(self.font_percent.clamp(50, 300) as u16) / 100.0
    }
}

/// Resolves `theme` config values with per-field fallback: `scheme` and
/// `background` keep their defaults when absent/unknown (the config's
/// decode-to-default policy), and the sidebar shade derives from the
/// resolved background.
fn theme_from_config(theme: &cmux_config::ThemeConfig) -> ThemeRuntime {
    let mut resolved = ThemeRuntime::default();
    match theme.scheme.as_deref() {
        None => {}
        Some("dark") => resolved.scheme = Some(false),
        Some("light") => resolved.scheme = Some(true),
        Some(other) => {
            eprintln!(
                "cmux-shell: theme.scheme '{other}' is not dark or light; keeping the default"
            );
        }
    }
    if let Some(background) = theme.background.as_deref() {
        match parse_hex_color(background) {
            Some(color) => {
                resolved.background = color;
                resolved.sidebar_background = shade_darker(color);
                // A configured background pins the pane interior too: the
                // config's contract is "chrome blends with the cells".
                // The design viewport token is the unconfigured default.
                resolved.viewport_background = color;
                // And the chip ladder rides along one step lighter than
                // the sidebar, so the whole chrome stays one scheme.
                resolved.chip_background = shade_lighter(resolved.sidebar_background);
            }
            None => eprintln!(
                "cmux-shell: theme.background '{background}' is not #rrggbb; keeping the default"
            ),
        }
    }
    if let Some(cursor) = theme.cursor.as_deref() {
        match parse_hex_color(cursor) {
            Some(color) => resolved.cursor = Some(color),
            None => {
                eprintln!("cmux-shell: theme.cursor '{cursor}' is not #rrggbb; keeping the default")
            }
        }
    }
    resolved
}

/// Parses `#rrggbb` / `rrggbb` into a packed `0xRRGGBB`.
fn parse_hex_color(text: &str) -> Option<u32> {
    let digits = text.strip_prefix('#').unwrap_or(text);
    if digits.len() == 6 && digits.chars().all(|c| c.is_ascii_hexdigit()) {
        u32::from_str_radix(digits, 16).ok()
    } else {
        None
    }
}

/// The sidebar is chrome, not content: the same hue one step darker.
const SIDEBAR_SHADE_FACTOR: f32 = 0.88;

/// Scales every channel so the hue stays while the strip reads as chrome.
fn shade_darker(color: u32) -> u32 {
    let scale = |channel: u32| ((channel as f32) * SIDEBAR_SHADE_FACTOR) as u32;
    (scale((color >> 16) & 0xFF) << 16) | (scale((color >> 8) & 0xFF) << 8) | scale(color & 0xFF)
}

/// The chip layer sits above the sidebar: the same hue one step lighter —
/// the inverse of [`shade_darker`]'s factor, channels rounded and clamped.
/// Checked against the design ladder: `#191b22` rounds to `#1c1f27`,
/// within one channel of the design's flat `#1d1f26` chip token
/// (`figma-design-reference.md` section 2) — recorded approximation, not
/// a design read (design-fidelity round 1, 2026-10-26).
fn shade_lighter(color: u32) -> u32 {
    let up = |channel: u32| (((channel as f32) / SIDEBAR_SHADE_FACTOR).round() as u32).min(0xFF);
    (up((color >> 16) & 0xFF) << 16) | (up((color >> 8) & 0xFF) << 8) | up(color & 0xFF)
}

// Design-exact chrome tokens: verified Figma reads
// (docs/research/figma-design-reference.md section 2 + the 2026-10-23
// node-table re-extract for fills the palette table did not record).
// Packed `0xRRGGBB`; the `TOKEN_HAIRLINE*` tuples carry their own alpha
// because the design states them as rgba().
const TOKEN_MAIN_BG: u32 = 0x11131A; // main background + gutters
const TOKEN_VIEWPORT_BG: u32 = 0x0C0E14; // terminal pane interior
const TOKEN_SIDEBAR_BG: u32 = 0x191B22; // sidebar strip
const TOKEN_ACCENT: u32 = 0x38BDF8; // focus ring, cursor, active accents
const TOKEN_CHIP_BG: u32 = 0x1D1F26; // active sidebar row chip
                                     //
                                     // Theme resolution (design round 1, 2026-10-26): every chrome chip fill
                                     // paints `theme.chip_background` (the token by default; one shade
                                     // lighter than a configured sidebar otherwise). Pane-INTERIOR card
                                     // faces (the agent lane's hard-coded `#191b22`/`#1d1f26`) stay the raw
                                     // design tokens — they are content, not chrome, and the lane-5 spec
                                     // pins them.
const TOKEN_TEXT_PRIMARY: u32 = 0xE2E2EB;
const TOKEN_TEXT_SECONDARY: u32 = 0xBDC8D1;
const TOKEN_TEXT_MUTED: u32 = 0x87929A;
const TOKEN_INFO: u32 = 0x8ED5FF; // active row name, links
const TOKEN_WARNING: u32 = 0xFFC42F; // unread ring dot
const TOKEN_DANGER: u32 = 0xFFB4AB; // failure ink
/// Aside border `rgba(62,72,79,0.4)` and its softer in-strip separators
/// (`rgba(62,72,79,0.3)`); alpha 102/77 = the design's 0.4/0.3.
const TOKEN_HAIRLINE: (u8, u8, u8, u8) = (62, 72, 79, 102);
const TOKEN_HAIRLINE_SOFT: (u8, u8, u8, u8) = (62, 72, 79, 77);
/// Native Win11 caption-button palette (verified OS behavior): the plain
/// glyph ink follows the design's secondary text, hover lightens the
/// caption strip by ~6% of white (≈ #27292E over #191b22), and the close
/// button hovers to the system red with a white glyph.
const TOKEN_TITLE_BTN_HOVER: u32 = 0x27292E;
const TOKEN_TITLE_CLOSE_HOVER: u32 = 0xC42B1C;
const TOKEN_TITLE_CLOSE_INK: u32 = 0xFFFFFF;

// Owner round 4 (2026-10-26): the sub-header violet pair retired with
// the band (`TOKEN_AGENT_INK` 0xCEBDFF over sidebar `#191b22`).
/// Design's warm serif numerals — the services segment's `:port` text
/// (Figma node 1:464, `#ffdf9f`); only the UP rows carry it.
const TOKEN_WARM_INK: u32 = 0xFFDF9F;
// Owner round 4 (2026-10-26): `agent_chip_bg` (rgba(79,49,156,0.2)
// composited over the chrome) retired with the agent chip.

fn token_color(token: u32) -> egui::Color32 {
    egui::Color32::from_rgb(
        ((token >> 16) & 0xFF) as u8,
        ((token >> 8) & 0xFF) as u8,
        (token & 0xFF) as u8,
    )
}

fn hairline_color(hairline: (u8, u8, u8, u8)) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(hairline.0, hairline.1, hairline.2, hairline.3)
}

// Status bar (design ladder lane 2 — file nodes 1:371..1:398): a 24px
// strip at the main area's bottom with the terminal-viewport dark fill
// (#0c0e14; the context export carries `bg-[#0c0e14] h-[24px]` directly
// on node 1:371 — the palette table's earlier "footer fill #191b22" row
// had matched the sub-header, corrected in the reference doc). Left
// cluster 12px, right cluster 11px, both text-only in the port.
const STATUS_BAR_HEIGHT: f32 = 24.0;
/// Sampling cadence for the machine CPU/RAM segments: % and gigabytes
/// sampled per frame would be noise; ~0.75s reads once.
const STATUS_SAMPLE_INTERVAL_MS: u128 = 750;

/// Design lane 2 title bar (node 1:495 "Header"): a 32px row across the
/// whole window top with the sidebar-toned fill and a bottom hairline;
/// breadcrumbs at the left, native-style window controls at the right.
/// With `with_decorations(false)` winit 0.30 keeps the snap/resize/
/// maximize styles and only hides the frame (its `WM_NCCALCSIZE`
/// override), so the port paints its own caption and subclasses
/// `WM_NCHITTEST` for the caption/drag semantics.
const TITLE_BAR_HEIGHT: f32 = 32.0;

/// Sampling cadence for the services lane (row 12): a human-scale ~2s;
/// the Windows process/listener snapshots are cheap but not per-frame
/// material. (The git lane's cadence retired with the sub-header band,
/// owner round 4.)
const SERVICE_SAMPLE_INTERVAL_MS: u128 = 2_000;

/// Workspace-identity segment cap (codepoints, same ellipsis rule): the
/// id + ` · ` + live name (`w1 · powershell` is 15) must paint whole
/// while a pathological name still folds before the leaf tail loses
/// ground (owner round 6, 2026-10-26).
const STATUS_WS_MAX_CHARS: usize = 18;

/// Head-plus-ellipsis fold used by every status-bar segment label: a
/// text longer than `max` codepoints keeps its head and ends at the
/// ellipsis glyph (which counts toward the cap). The pane-marker run
/// this fold shared retired in owner round 8 (2026-10-27); today's
/// only user is the workspace-identity segment.
fn fold_label(text: String, max: usize) -> String {
    if text.chars().count() > max {
        text.chars().take(max - 1).collect::<String>() + "\u{2026}"
    } else {
        text
    }
}

/// Status-bar workspace-identity segment (owner round 6, 2026-10-26):
/// `{id} middot {live name}`, e.g. `w1 · powershell`. The minted `w{N}`
/// id is the stable identity that visibly changes on a workspace
/// switch; the live name alone would fold to the same shell title in
/// every workspace (the same-shell coincidence the owner reported when
/// the marker run still rode the strip), so the id leads. Folded to
/// [`STATUS_WS_MAX_CHARS`] codepoints so one long pane title cannot eat
/// the leaf-tail ground; the ellipsis counts toward the cap.
fn status_workspace_label(id: &str, name: &str) -> String {
    fold_label(format!("{} \u{b7} {}", id, name), STATUS_WS_MAX_CHARS)
}

/// `CPU: 18%` from a sampled machine percentage; `–` before the first
/// sample interval completes (never a fabricated value).
fn status_cpu_label(percent: Option<u32>) -> String {
    match percent {
        Some(value) => format!("CPU: {value}%"),
        None => "CPU: –".to_owned(),
    }
}

/// Machine-memory formatting: GB with one decimal below 10 GB (the
/// design's `MEM: 1.4 GB`), whole GB at/above, whole MB below a gig.
fn format_human_bytes(bytes: u64) -> String {
    const GIB: f64 = 1_073_741_824.0;
    const MIB: f64 = 1_048_576.0;
    let gib = bytes as f64 / GIB;
    if gib >= 10.0 {
        format!("{gib:.0} GB")
    } else if gib >= 1.0 {
        format!("{gib:.1} GB")
    } else {
        format!("{:.0} MB", bytes as f64 / MIB)
    }
}

fn status_mem_label(bytes_used: Option<u64>) -> String {
    match bytes_used {
        Some(bytes) => format!("MEM: {}", format_human_bytes(bytes)),
        None => "MEM: –".to_owned(),
    }
}

/// The agent state segment. Current semantics (inference, the design mock
/// shows only `ACTIVE`): info-blue `ACTIVE` while the active workspace
/// has seen a detected agent session (turn boundary recorded or pending),
/// muted `IDLE` before that.
fn status_claude_label(agent_active: bool) -> String {
    if agent_active {
        "CLAUDE: ACTIVE".to_owned()
    } else {
        "CLAUDE: IDLE".to_owned()
    }
}

/// Status-bar IPC leaf cap (codepoints of the leaf after the `pipe: `
/// prefix, the fold ellipsis counts toward it): the port's default
/// control-pipe name derives from the machine SID
/// (`cmux-s-1-5-21-1806527352-…`) and overflowed its segment into the
/// machine telemetry on the live repro. The design's listener address is
/// short; the leaf's tail is the distinguishing part, so longer leaves
/// fold to a leading ellipsis (design round 1, 2026-10-26; same fold
/// discipline as the pane titles).
const STATUS_IPC_LEAF_MAX: usize = 14;

/// The IPC segment: the design shows a listener address; the port's real
/// control endpoint is a named pipe, so its leaf name backs the label.
/// `capacity` selects the fold (the painted default uses
/// [`STATUS_IPC_LEAF_MAX`]; the owner round 6 fold matrix moves to
/// [`STATUS_IPC_LEAF_SHORT`] before it lets the ws label lose its name).
fn status_ipc_label_capped(pipe: &str, capacity: usize) -> String {
    let leaf = pipe
        .rsplit(['\\', '/'])
        .find(|part| !part.is_empty())
        .unwrap_or("");
    if leaf.is_empty() {
        return "ipc off".to_owned();
    }
    let leaf = if leaf.chars().count() > capacity {
        let tail: String = leaf
            .chars()
            .skip(leaf.chars().count() + 1 - capacity)
            .collect();
        format!("\u{2026}{tail}")
    } else {
        leaf.to_owned()
    };
    format!("pipe: {leaf}")
}

/// The leaf tail's reserve length: the variant the fold matrix drops to
/// so the ws label (id + name) keeps its ground (owner round 6; the
/// marker run it shared the reserve with retired in round 8). Still
/// distinguishing - it cuts into the uuid's variable middle, not its end.
const STATUS_IPC_LEAF_SHORT: usize = 8;

/// The status strip's fold matrix (owner round 6; simplified round 8
/// when the marker run left the strip). `free` is the px left for
/// [ws + leaf] after the fixed right cluster and the telemetry
/// reservation; the leaves add the chosen variant widths on top:
///
///   1. (`true`, `false`)  full `id · name` ws label + full 14-tail leaf
///   2. (`true`, `true`)   full ws label + 8-tail leaf (leaf flexes first)
///   3. (`false`, `false`) id-only label + full leaf
///   4. (`false`, `true`)  id-only label + 8-tail leaf (terminal)
///
/// The minted id - the owner's switch tracker - never folds; the leaf
/// tail and the ws NAME do. When even case 4 does not hold, the caller
/// still paints case 4 (the strip stays overlap-free at its anchor
/// positions; the pathological width was the marker overflow residual).
fn status_ws_fit_case(
    free: f32,
    ws_full_span: f32,
    ws_id_span: f32,
    leaf_full: f32,
    leaf_short: f32,
) -> (bool, bool) {
    let fits = |ws: f32, leaf: f32| free >= ws + leaf;
    if fits(ws_full_span, leaf_full) {
        (true, false)
    } else if fits(ws_full_span, leaf_short) {
        (true, true)
    } else if fits(ws_id_span, leaf_full) {
        (false, false)
    } else {
        (false, true)
    }
}

/// Title-bar breadcrumbs (node 1:495 left cluster): `cmux` / workspace ›
/// pane, with the pane part untitled-safe using the same `–` convention
/// the retired status-bar pane markers used (the design's mock names
/// every pane).
fn title_crumb_texts(workspace: &str, pane: Option<&str>) -> (String, String) {
    (workspace.to_owned(), pane.unwrap_or("\u{2013}").to_owned())
}

/// Lays a chip crumb out within the design's max width (`max-w` with
/// overflow clip): the full text, else a char-tail truncated `…` variant
/// that fits (the CSS clip has no exact egui twin; truncation is kinder
/// than a hard crop).
fn fit_crumb_galley(
    painter: &egui::Painter,
    text: &str,
    font: egui::FontId,
    color: egui::Color32,
    max_width: f32,
) -> std::sync::Arc<egui::Galley> {
    let ellipsis = "\u{2026}";
    let mut candidate = text.to_owned();
    let mut galley = painter.layout_no_wrap(candidate.clone(), font.clone(), color);
    // The candidate length must STRICTLY shrink each pass: dropping a
    // char and re-appending the ellipsis keeps the count constant, so
    // naïvely halving one char would spin forever on wide titles.
    let mut count = candidate.chars().count();
    while galley.size().x > max_width && count > 1 {
        count = (count * 3 / 4).max(1);
        candidate = candidate.chars().take(count).collect::<String>() + ellipsis;
        galley = painter.layout_no_wrap(candidate.clone(), font.clone(), color);
    }
    galley
}

/// Glyph widths used by the hit-test: one 44×32 zone per control
/// (design buttons 1:534/1:537/1:540), so the client strip the egui
/// buttons need is the rightmost three.
const TITLE_CONTROL_ZONE_W_PTS: f32 = 44.0;
const TITLE_CONTROL_ZONES: f32 = 3.0;

/// Machine CPU percentage from two `GetSystemTimes` snapshots (idle and
/// kernel+user; kernel time already includes idle on Windows, so busy =
/// total - idle). `None` between the first two samples and whenever the
/// deltas degenerate to zero.
fn system_cpu_percent(prev: (u64, u64), next: (u64, u64)) -> Option<u32> {
    let idle = next.0.saturating_sub(prev.0);
    let total = next.1.saturating_sub(prev.1);
    if total == 0 {
        return None;
    }
    let busy = total.saturating_sub(idle);
    Some(
        (busy as f32 / total as f32 * 100.0)
            .round()
            .clamp(0.0, 100.0) as u32,
    )
}

/// One host snapshot feeding the status bar.
struct StatusSample {
    idle: u64,
    total: u64,
    mem_used: u64,
}

/// Held CPU/RAM figures for the status bar (a snapshot pair refined into
/// a percentage — the raw times live only for the delta math).
struct StatusState {
    at: Instant,
    idle: u64,
    total: u64,
    cpu_percent: Option<u32>,
    mem_used: u64,
}

/// Kernel32-only host probes (no psapi, no new crates): `GetSystemTimes`
/// for the machine CPU% and `GlobalMemoryStatusEx` for physical RAM in
/// use. FILETIME aliases a `u64` (two packed u32 halves, little-endian).
#[cfg(windows)]
fn host_status_sample() -> Option<StatusSample> {
    #[repr(C)]
    struct MemoryStatusEx {
        length: u32,
        load: u32,
        total_phys: u64,
        avail_phys: u64,
        total_page: u64,
        avail_page: u64,
        total_virt: u64,
        avail_virt: u64,
        avail_extend: u64,
    }
    // SAFETY: kernel32 calls with correctly-sized pinned stack buffers;
    // the FILETIME outputs are plain u64 states and the memory struct is
    // initialized to its byte length first, the documented contract.
    unsafe {
        #[link(name = "kernel32")]
        extern "system" {
            fn GetSystemTimes(idle: *mut u64, kernel: *mut u64, user: *mut u64) -> i32;
            fn GlobalMemoryStatusEx(buffer: *mut MemoryStatusEx) -> i32;
        }
        let (mut idle, mut kernel, mut user) = (0u64, 0u64, 0u64);
        let _present = GetSystemTimes(&mut idle, &mut kernel, &mut user);
        let mut memory = MemoryStatusEx {
            length: core::mem::size_of::<MemoryStatusEx>() as u32,
            load: 0,
            total_phys: 0,
            avail_phys: 0,
            total_page: 0,
            avail_page: 0,
            total_virt: 0,
            avail_virt: 0,
            avail_extend: 0,
        };
        let _present_memory = GlobalMemoryStatusEx(&mut memory);
        Some(StatusSample {
            idle,
            total: kernel.saturating_add(user),
            mem_used: memory.total_phys.saturating_sub(memory.avail_phys),
        })
    }
}

#[cfg(not(windows))]
fn host_status_sample() -> Option<StatusSample> {
    None
}

// ---- custom title bar window integration (design lane 2, node 1:495) ----
//
// Winit evidence (v0.30.13 registry source): with decorations disabled the
// top-level window RETAINS WS_CAPTION|WS_SIZEBOX|WS_MAXIMIZEBOX|
// WS_MINIMIZEBOX (window_state.rs `to_window_styles`: "Required styles to
// properly support common window functionality like aero snap") and the
// visible frame disappears only because winit returns 0 from its
// `WM_NCCALCSIZE` override (client rect = window rect; maximized clamps
// to the monitor work area). Winit registers no `WM_NCHITTEST` handler of
// its own, and with the NCCALCSIZE=0 override the window keeps NO
// nonclient area, so DefWindowProc answers HTCLIENT for every point —
// the classic claim that "DefWindowProc keeps resize borders alive" does
// NOT hold for this recipe (probe-falsified: edge drags over an all-client
// window never resize; the Chromium/WindowChrome recipe computes the
// bands itself). This subclass:
//   1. computes the resize bands (SM_CXSIZEFRAME+SM_CXPADDEDBORDER at the
//      window DPI) from its WM_NCHITTEST route, suppressed while
//      maximized and over the control zones;
//   2. routes strip points to HTCAPTION (native drag, aero snap,
//      double-click maximize, system menu on right-click all follow),
//      except the rightmost three 44×32 control zones which stay HTCLIENT
//      so egui's button widgets keep working; and
//   3. asks the WebView2 surfaces to inset from window-adjacent edges
//      (`resize_band_pts` in `sync_browser_surfaces`) because a child
//      window answers HTCLIENT for every point it covers and would
//      starve the top-level band route.

/// The command-bar button (node 1:512) geometry in egui points from the
/// client width alone — the wndproc and the painter must agree, so the
/// position derives only from facts both can see. The mock floats the
/// 420×24 bar at x=425.68 of a 1280 header (0.3326) and clears the three
/// native control zones on the right; `left` never drops below the
/// worst-case breadcrumb cluster (~335pt), the bar flex-shrinks against
/// the controls (a 150pt minimum keeps glyph+placeholder+pills inside),
/// and below [`CMD_BAR_HIDE_WIDTH`] the bar never shows (the design gap
/// collapses before the shuffle would clip it).
pub fn title_command_bar_rect_pts(width_pts: f32) -> Option<egui::Rect> {
    const CMD_BAR_HIDE_WIDTH: f32 = 800.0;
    const CMD_BAR_LEFT_MIN: f32 = 335.0;
    const CMD_BAR_WIDTH_MAX: f32 = 420.0;
    const CMD_BAR_WIDTH_MIN: f32 = 150.0;
    if width_pts < CMD_BAR_HIDE_WIDTH {
        return None;
    }
    let left = (width_pts * 0.3326).max(CMD_BAR_LEFT_MIN);
    let right_bound = width_pts - TITLE_CONTROL_ZONES * TITLE_CONTROL_ZONE_W_PTS - 16.0;
    let width = CMD_BAR_WIDTH_MAX.min((right_bound - left).max(CMD_BAR_WIDTH_MIN));
    Some(egui::Rect::from_min_max(
        egui::pos2(left, 4.0),
        egui::pos2(left + width, 4.0 + CMD_BAR_HEIGHT_PTS),
    ))
}

const CMD_BAR_HEIGHT_PTS: f32 = 24.0;

/// One WM_NCHITTEST route for a strip point (pure, unit-tested): geometry
/// in physical window pixels; the strip is the top `strip_h` rows, the
/// control zones span `[buttons_left, buttons_right)` and the command
/// bar (`cmd_zone`) inside it — both stay HTCLIENT so egui's button
/// widgets keep working.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum TitleHitDecision {
    /// Painted caption: native drag/snap/menu paths.
    Caption,
    /// Client area: the egui window-control widgets.
    Client,
    /// Below the strip: keep the wndproc chain's own result.
    Pass,
}

/// One WM_NCHITTEST route for a strip point (pure, unit-tested): geometry
/// in physical pixels; the strip occupies the top rows of the client and
/// the native controls / command bar stay HTCLIENT inside it. Arguments
/// exceed clippy's default: the geometry is five live window facts the
/// wndproc already owns (per-fact parameters keep the version unit-tested
/// without a struct-in-a-struct).
#[allow(clippy::too_many_arguments)]
fn title_bar_hit_decision(
    x: i32,
    y: i32,
    strip_h: i32,
    buttons_left: i32,
    buttons_right: i32,
    cmd_zone: Option<(i32, i32)>,
) -> TitleHitDecision {
    if y < 0 || y >= strip_h {
        return TitleHitDecision::Pass;
    }
    let in_cmd = cmd_zone
        .map(|(left, right)| x >= left && x < right)
        .unwrap_or(false);
    if in_cmd || x >= buttons_left && x < buttons_right {
        TitleHitDecision::Client
    } else {
        TitleHitDecision::Caption
    }
}

/// The invisible resize band hit for one client point (pure, unit-tested):
/// `(x, y)` client-relative, `width`/`height` the client-rect size and
/// `band` the band width, all physical pixels; `strip_h` and
/// `buttons_left` carve the painted bar out of the top band.
///
/// With `WM_NCCALCSIZE` overridden to 0 the window has no nonclient area
/// at all, so DefWindowProc answers HTCLIENT for every point and never
/// reports the `HTEDGE_*` codes: the bands must be computed by the
/// subclass (the Chromium/WindowChrome recipe). Returns `None` when the
/// point belongs to the strip/base route: while maximized, over the
/// three control zones (their full 44×32 zone stays a clickable button,
/// like a native caption), and for any interior point.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum TitleBand {
    TopLeft,
    Top,
    TopRight,
    Left,
    Right,
    BottomLeft,
    Bottom,
    BottomRight,
}

// clippy::too_many_arguments — the geometry is five window facts the
// wndproc route already holds; per-parameter keeps the version testable
// point-wise.
#[allow(clippy::too_many_arguments)]
fn title_band_decision(
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    band: i32,
    strip_h: i32,
    buttons_left: i32,
    cmd_zone: Option<(i32, i32)>,
) -> Option<TitleBand> {
    if band <= 0 {
        return None;
    }
    let in_left = x < band;
    let in_right = x >= width - band;
    let in_top = y < band;
    let in_bottom = y >= height - band;
    // A point inside the strip's control-zone or command-bar x-range
    // keeps the strip route (the button widgets), never the band.
    let in_cmd = cmd_zone
        .map(|(left, right)| x >= left && x < right)
        .unwrap_or(false);
    let over_zone = y < strip_h && (x >= buttons_left || in_cmd);
    if (in_right || in_top) && over_zone {
        return None;
    }
    match (in_left, in_right, in_top, in_bottom) {
        (true, _, true, _) => Some(TitleBand::TopLeft),
        (true, _, _, true) => Some(TitleBand::BottomLeft),
        (_, true, true, _) => Some(TitleBand::TopRight),
        (_, true, _, true) => Some(TitleBand::BottomRight),
        (true, _, _, _) => Some(TitleBand::Left),
        (_, true, _, _) => Some(TitleBand::Right),
        (_, _, true, _) => Some(TitleBand::Top),
        (_, _, _, true) => Some(TitleBand::Bottom),
        _ => None,
    }
}

#[cfg(windows)]
mod title_bar_wndproc {
    use std::sync::atomic::{AtomicIsize, Ordering};

    static PREV_WNDPROC: AtomicIsize = AtomicIsize::new(0);

    /// Mirrored from WM_SIZE so the painted bar can switch the maximize
    /// glyph to restore state without waiting on egui's viewport info.
    pub static IS_MAXIMIZED: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);

    /// Installs the subclass (the original wndproc is captured exactly
    /// once; later calls are no-ops — re-reading `GWLP_WNDPROC` would
    /// return this subclass itself and poison the call chain).
    ///
    /// SAFETY: the GUI thread owns the window; the swapped pointer is
    /// written before the subclass becomes reachable, and every hooked
    /// message falls through to the stored original procedure.
    pub fn install(hwnd: isize) {
        const GWLP_WNDPROC: i32 = -4;
        if PREV_WNDPROC.load(Ordering::Acquire) != 0 {
            return;
        }
        let prev = get_window_long_ptr(hwnd, GWLP_WNDPROC);
        if prev == 0 {
            return; // no wndproc installed yet (window still creating)
        }
        if PREV_WNDPROC
            .compare_exchange(0, prev, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            set_window_long_ptr(
                hwnd,
                GWLP_WNDPROC,
                title_bar_window_proc as *const () as usize as isize,
            );
        }
    }

    const WM_NCHITTEST: u32 = 0x0084;
    const WM_SIZE: u32 = 0x0005;
    const WM_DESTROY: u32 = 0x0002;
    const HTCLIENT: i32 = 1;
    const HTCAPTION: i32 = 2;
    /// HTLEFT(10) ..= HTBOTTOMRIGHT(17): DefWindowProc's resize-band
    /// results take precedence over the caption strip (the top few rows
    /// of the bar still resize, like a native caption).
    const HT_EDGE_FIRST: i32 = 10;
    const HT_EDGE_LAST: i32 = 17;
    const SIZE_MAXIMIZED: usize = 2;

    /// `WNDPROC` ABI: HWND (*mut c_void), UINT, WPARAM (usize stalking
    /// 64-bit), LPARAM (isize), returns LRESULT (isize).
    type WndProcFn = unsafe extern "system" fn(*mut core::ffi::c_void, u32, usize, isize) -> isize;

    fn call_prev(prev: WndProcFn, hwnd: isize, msg: u32, wp: usize, lp: isize) -> isize {
        unsafe {
            #[link(name = "user32")]
            extern "system" {
                fn CallWindowProcW(
                    prev: usize,
                    hwnd: usize,
                    msg: u32,
                    wp: usize,
                    lp: isize,
                ) -> isize;
            }
            CallWindowProcW(prev as usize, hwnd as usize, msg, wp, lp)
        }
    }

    fn def_window_proc(hwnd: isize, msg: u32, wp: usize, lp: isize) -> isize {
        unsafe {
            #[link(name = "user32")]
            extern "system" {
                fn DefWindowProcW(hwnd: usize, msg: u32, wp: usize, lp: isize) -> isize;
            }
            DefWindowProcW(hwnd as usize, msg, wp, lp)
        }
    }

    fn get_window_long_ptr(hwnd: isize, index: i32) -> isize {
        unsafe {
            #[link(name = "user32")]
            extern "system" {
                fn GetWindowLongPtrW(hwnd: usize, index: i32) -> isize;
            }
            GetWindowLongPtrW(hwnd as usize, index)
        }
    }

    fn set_window_long_ptr(hwnd: isize, index: i32, value: isize) -> isize {
        unsafe {
            #[link(name = "user32")]
            extern "system" {
                fn SetWindowLongPtrW(hwnd: usize, index: i32, value: isize) -> isize;
            }
            SetWindowLongPtrW(hwnd as usize, index, value)
        }
    }

    #[repr(C)]
    struct RectL {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }

    #[repr(C)]
    struct PointL {
        x: i32,
        y: i32,
    }

    fn get_client_rect(hwnd: isize) -> RectL {
        let mut rect = RectL {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        unsafe {
            #[link(name = "user32")]
            extern "system" {
                fn GetClientRect(hwnd: usize, rect: *mut RectL) -> i32;
            }
            let _present = GetClientRect(hwnd as usize, &mut rect);
        }
        rect
    }

    fn screen_to_client(hwnd: isize, point: &mut PointL) {
        unsafe {
            #[link(name = "user32")]
            extern "system" {
                fn ScreenToClient(hwnd: usize, point: *mut PointL) -> i32;
            }
            let _moved = ScreenToClient(hwnd as usize, point as *mut PointL);
        }
    }

    fn get_dpi_for_window(hwnd: isize) -> u32 {
        unsafe {
            #[link(name = "user32")]
            extern "system" {
                fn GetDpiForWindow(hwnd: usize) -> u32;
            }
            GetDpiForWindow(hwnd as usize)
        }
    }

    /// Physical-pixel width of the invisible resize bands on this window:
    /// the system frame metrics (`SM_CXSIZEFRAME + SM_CXPADDEDBORDER`)
    /// resolved at the window's DPI. `WM_NCCALCSIZE` claims these pixels
    /// for the client, so their hit-testing lives in the subclass instead
    /// of DefWindowProc.
    fn resize_band_px(hwnd: isize) -> i32 {
        const SM_CXSIZEFRAME: i32 = 32;
        const SM_CXPADDEDBORDER: i32 = 92;
        let dpi = get_dpi_for_window(hwnd);
        unsafe {
            #[link(name = "user32")]
            extern "system" {
                fn GetSystemMetricsForDpi(index: i32, dpi: u32) -> i32;
            }
            let band = GetSystemMetricsForDpi(SM_CXSIZEFRAME, dpi)
                + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi);
            if band > 0 {
                return band;
            }
        }
        // Fallback: primary-monitor metrics (per-monitor DPI scales from
        // it near enough for a grabbing affordance).
        unsafe {
            #[link(name = "user32")]
            extern "system" {
                fn GetSystemMetrics(index: i32) -> i32;
            }
            GetSystemMetrics(SM_CXSIZEFRAME) + GetSystemMetrics(SM_CXPADDEDBORDER)
        }
    }

    /// Band width in egui points for the child-window inset: browser
    /// surfaces shrink away from window-adjacent edges so the band
    /// stays reachable (a child window answers HTCLIENT for every
    /// point it covers and would starve the top-level route).
    pub fn resize_band_pts(hwnd: isize) -> f32 {
        resize_band_px(hwnd) as f32 / (get_dpi_for_window(hwnd) as f32 / 96.0)
    }

    /// The client rect in egui points: `(left, top, right, bottom)`.
    pub fn client_rect_pts(hwnd: isize) -> (f32, f32, f32, f32) {
        let rect = get_client_rect(hwnd);
        let scale = get_dpi_for_window(hwnd) as f32 / 96.0;
        (
            rect.left as f32 / scale,
            rect.top as f32 / scale,
            rect.right as f32 / scale,
            rect.bottom as f32 / scale,
        )
    }

    /// Strip + control-zone + command-bar geometry in physical pixels
    /// for this window: `(strip_h, buttons_left, buttons_right, cmd_zone)`
    /// where `cmd_zone` is `None` while the design bar is hidden (narrow
    /// client) and else `((left, right))`.
    #[allow(clippy::type_complexity)]
    fn strip_metrics(hwnd: isize) -> (i32, i32, i32, Option<(i32, i32)>) {
        let rect = get_client_rect(hwnd);
        let scale = get_dpi_for_window(hwnd) as f32 / 96.0;
        let strip_h = (super::TITLE_BAR_HEIGHT * scale).round() as i32;
        let zones_w =
            (super::TITLE_CONTROL_ZONE_W_PTS * super::TITLE_CONTROL_ZONES * scale).round() as i32;
        let cmd_zone = super::title_command_bar_rect_pts(rect.right as f32 / scale).map(|bar| {
            (
                (bar.left() * scale).round() as i32,
                (bar.right() * scale).round() as i32,
            )
        });
        (strip_h, rect.right - zones_w, rect.right, cmd_zone)
    }

    /// Screen-coordinates lParam → `(x, y)`: the packed POINTS halves are
    /// signed 16-bit values.
    fn screen_point(lp: isize) -> (i32, i32) {
        let raw = lp as i32;
        let x = (raw & 0xFFFF) as u16 as i16 as i32;
        let y = ((raw >> 16) & 0xFFFF) as u16 as i16 as i32;
        (x, y)
    }

    /// The subclass procedure: NCHITTEST routing plus a WM_SIZE mirror of
    /// the maximize state, everything else forwarded verbatim.
    ///
    /// SAFETY: a Win32 window procedure; every message is either routed
    /// here itself or forwarded to the recorded original procedure.
    unsafe extern "system" fn title_bar_window_proc(
        hwnd: *mut core::ffi::c_void,
        msg: u32,
        wp: usize,
        lp: isize,
    ) -> isize {
        let hwnd = hwnd as isize;
        if msg == WM_SIZE {
            IS_MAXIMIZED.store(wp == SIZE_MAXIMIZED, Ordering::Relaxed);
        }
        if msg == WM_NCHITTEST {
            let prev = PREV_WNDPROC.load(Ordering::Acquire);
            let base = if prev != 0 {
                call_prev(
                    unsafe { core::mem::transmute::<isize, WndProcFn>(prev) },
                    hwnd,
                    msg,
                    wp,
                    lp,
                )
            } else {
                def_window_proc(hwnd, msg, wp, lp)
            };
            let ht = base as i32;
            if (HT_EDGE_FIRST..=HT_EDGE_LAST).contains(&ht) {
                return base;
            }
            let (sx, sy) = screen_point(lp);
            let mut point = PointL { x: sx, y: sy };
            screen_to_client(hwnd, &mut point);
            let (strip_h, buttons_left, buttons_right, cmd_zone) = strip_metrics(hwnd);
            // Resize bands live here because the NCCALCSIZE=0 override
            // removes the nonclient area DefWindowProc would hit-test
            // (it reports HTCLIENT everywhere; the `HT_EDGE_*` pass
            // above only covers windows that keep a real frame).
            let band_hit = if IS_MAXIMIZED.load(Ordering::Relaxed) {
                None
            } else {
                let rect = get_client_rect(hwnd);
                super::title_band_decision(
                    point.x,
                    point.y,
                    rect.right,
                    rect.bottom,
                    resize_band_px(hwnd),
                    strip_h,
                    buttons_left,
                    cmd_zone,
                )
            };
            if let Some(band) = band_hit {
                // HTLEFT(10), HTRIGHT(11), HTTOPLEFT(12), HTTOP(13),
                // HTTOPRIGHT(14), HTBOTTOMLEFT(15), HTBOTTOM(16),
                // HTBOTTOMRIGHT(17).
                return match band {
                    super::TitleBand::Left => 10,
                    super::TitleBand::Right => 11,
                    super::TitleBand::TopLeft => 12,
                    super::TitleBand::Top => 13,
                    super::TitleBand::TopRight => 14,
                    super::TitleBand::BottomLeft => 15,
                    super::TitleBand::Bottom => 16,
                    super::TitleBand::BottomRight => 17,
                };
            }
            return match super::title_bar_hit_decision(
                point.x,
                point.y,
                strip_h,
                buttons_left,
                buttons_right,
                cmd_zone,
            ) {
                super::TitleHitDecision::Caption => HTCAPTION as isize,
                super::TitleHitDecision::Client => HTCLIENT as isize,
                super::TitleHitDecision::Pass => base,
            };
        }
        if msg == WM_DESTROY {
            // Put the original procedure back so the window can outlive
            // the subclass; the destruction message below forwards once.
            const GWLP_WNDPROC: i32 = -4;
            let prev = PREV_WNDPROC.load(Ordering::Acquire);
            if prev != 0 {
                set_window_long_ptr(hwnd, GWLP_WNDPROC, prev);
            }
        }
        let prev = PREV_WNDPROC.load(Ordering::Acquire);
        if prev != 0 {
            call_prev(
                unsafe { core::mem::transmute::<isize, WndProcFn>(prev) },
                hwnd,
                msg,
                wp,
                lp,
            )
        } else {
            def_window_proc(hwnd, msg, wp, lp)
        }
    }
}

#[cfg(not(windows))]
mod title_bar_wndproc {
    pub static IS_MAXIMIZED: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);

    pub fn install(_hwnd: isize) {}

    /// No resize bands off-Windows: every platform keeps native frames.
    pub fn resize_band_pts(_hwnd: isize) -> f32 {
        0.0
    }

    pub fn client_rect_pts(_hwnd: isize) -> (f32, f32, f32, f32) {
        (0.0, 0.0, 0.0, 0.0)
    }
}

/// Applies the design tokens to egui's visuals before the first frame so
/// egui-fed inks read from the same palette the custom-painted chrome
/// uses: the accent is the terminal cursor and selection stroke, the
/// terminal's notice/exit/error inks are the design text roles, and the
/// unread-ring warning is the design amber. The design file carries no
/// light variant: a configured light scheme skips the override entirely,
/// leaving egui's defaults for the chrome (recorded residual).
fn install_design_visuals(ctx: &egui::Context, theme: &ThemeRuntime) {
    if theme.scheme == Some(true) {
        return;
    }
    let mut visuals = egui::Visuals::dark();
    visuals.selection.stroke = egui::Stroke::new(1.0, token_color(TOKEN_ACCENT));
    visuals.widgets.active.fg_stroke.color = token_color(TOKEN_TEXT_PRIMARY);
    visuals.widgets.noninteractive.fg_stroke.color = token_color(TOKEN_TEXT_SECONDARY);
    visuals.weak_text_color = Some(token_color(TOKEN_TEXT_MUTED));
    visuals.warn_fg_color = token_color(TOKEN_WARNING);
    visuals.error_fg_color = token_color(TOKEN_DANGER);
    visuals.hyperlink_color = token_color(TOKEN_INFO);
    ctx.global_style_mut(|style| style.visuals = visuals);
}

/// One parsed keybinding: an egui key plus the modifiers it requires.
#[derive(Debug, Clone, Copy)]
struct KeyCombo {
    key: Key,
    mods: Modifiers,
}

/// Shortcut action ids and their default strokes, mirroring upstream's
/// `ShortcutAction` naming (verified in
/// `CmuxSettings/Values/ShortcutAction.swift`). `closePane` is a port
/// addition â€” upstream shortcuts close whole tabs, not single panes.
/// The stroke defaults are the owner-accepted provisional sets
/// (2026-10-17 panes, 2026-10-18 workspace tabs), Windows-mapped from
/// upstream's macOS keymap (`cmd` â†’ `ctrl+shift`, family rule for the
/// digit switcher), not upstream's preset. `equalizeSplits` and the
/// `resizePane*` actions exist but ship unbound by default.
const DEFAULT_BINDINGS: &[(&str, Option<&str>)] = &[
    ("splitRight", Some("ctrl+shift+r")),
    ("splitDown", Some("ctrl+shift+d")),
    ("closePane", Some("ctrl+shift+w")),
    ("focusNextPane", Some("ctrl+tab")),
    ("focusPreviousPane", Some("ctrl+shift+tab")),
    ("newTab", Some("ctrl+shift+n")),
    ("closeTab", Some("ctrl+shift+q")),
    ("nextSidebarTab", Some("ctrl+shift+]")),
    ("prevSidebarTab", Some("ctrl+shift+[")),
    ("toggleSidebar", Some("ctrl+shift+b")),
    // Lane 3: the command bar advertises Ctrl+K (node 1:512's pills).
    ("commandPalette", Some("ctrl+k")),
    ("selectWorkspaceByNumber", Some("ctrl+1")),
    ("selectWorkspaceByNumber", Some("ctrl+2")),
    ("selectWorkspaceByNumber", Some("ctrl+3")),
    ("selectWorkspaceByNumber", Some("ctrl+4")),
    ("selectWorkspaceByNumber", Some("ctrl+5")),
    ("selectWorkspaceByNumber", Some("ctrl+6")),
    ("selectWorkspaceByNumber", Some("ctrl+7")),
    ("selectWorkspaceByNumber", Some("ctrl+8")),
    ("selectWorkspaceByNumber", Some("ctrl+9")),
    ("equalizeSplits", None),
    ("moveWorkspaceUp", None),
    ("moveWorkspaceDown", None),
    ("resizePaneLeft", None),
    ("resizePaneRight", None),
    ("resizePaneUp", None),
    ("resizePaneDown", None),
];

fn action_for_id(id: &str) -> Option<Action> {
    match id {
        "splitRight" => Some(Action::SplitFocused(SplitDirection::Right)),
        "splitDown" => Some(Action::SplitFocused(SplitDirection::Down)),
        "closePane" => Some(Action::CloseFocused),
        "focusNextPane" => Some(Action::Cycle { forward: true }),
        "focusPreviousPane" => Some(Action::Cycle { forward: false }),
        "equalizeSplits" => Some(Action::EqualizeAll),
        "resizePaneLeft" => Some(Action::Resize(ResizeDirection::Left)),
        "resizePaneRight" => Some(Action::Resize(ResizeDirection::Right)),
        "resizePaneUp" => Some(Action::Resize(ResizeDirection::Up)),
        "resizePaneDown" => Some(Action::Resize(ResizeDirection::Down)),
        "newTab" => Some(Action::NewWorkspace),
        "closeTab" => Some(Action::CloseActiveWorkspace),
        "nextSidebarTab" => Some(Action::CycleWorkspace { forward: true }),
        "prevSidebarTab" => Some(Action::CycleWorkspace { forward: false }),
        "toggleSidebar" => Some(Action::ToggleSidebar),
        "commandPalette" => Some(Action::CommandPalette),
        // Owner round 9: upstream's `moveWorkspaceUp/Down` reorder
        // actions (ADR-0010 deferred them; the sidebar drag and the
        // inline rename brought the family in). Unbound by default.
        "moveWorkspaceUp" => Some(Action::MoveActiveWorkspace { up: true }),
        "moveWorkspaceDown" => Some(Action::MoveActiveWorkspace { up: false }),
        // `selectWorkspaceByNumber` is bound from the stroke's digit key
        // (the number row), not from the id alone.
        _ => None,
    }
}

/// Builds the runtime binding table: defaults first, then the config's
/// `shortcuts.bindings` overrides (upstream semantics: a stroke replaces,
/// `null` unbinds, a missing action keeps its default). An unrecognized
/// stroke falls back to the action's default â€” mirroring upstream's
/// decode-failure-to-default behavior â€” and chords are not applied yet.
/// The `selectWorkspaceByNumber` family binds from each row's digit key
/// (1â†’0 index); rebinding that id is not applied yet (logged, defaults
/// survive) because a single stroke cannot express the whole family.
fn build_bindings(bindings: &BTreeMap<String, Binding>) -> Vec<(KeyCombo, Action)> {
    let mut table = Vec::new();
    for &(id, default_stroke) in DEFAULT_BINDINGS {
        let (stroke, digits_bound): (Option<&str>, bool) = if id == "selectWorkspaceByNumber" {
            if bindings.contains_key(id) {
                eprintln!("cmux-shell: rebinding '{id}' is not supported yet; keeping ctrl+1..9");
            }
            (default_stroke, true)
        } else {
            let stroke = match bindings.get(id) {
                None => default_stroke,
                Some(Binding::Stroke(stroke)) => Some(stroke.as_str()),
                Some(Binding::Chord(strokes)) => {
                    eprintln!(
                        "cmux-shell: shortcut '{id}' is a chord ({strokes:?}) â€” chords are \
                         not applied yet; keeping the default binding"
                    );
                    default_stroke
                }
                Some(Binding::Unbound) => None,
            };
            (stroke, false)
        };
        if let Some(stroke) = stroke {
            let combo = match parse_stroke(stroke) {
                Some(combo) => combo,
                None => {
                    eprintln!(
                        "cmux-shell: shortcut '{stroke}' for {id} is not recognized; \
                         keeping the default binding"
                    );
                    let Some(combo) = default_stroke.and_then(parse_stroke) else {
                        continue;
                    };
                    combo
                }
            };
            let action = if digits_bound {
                match digit_index(combo.key) {
                    Some(index) => Action::SelectWorkspace(index),
                    None => {
                        eprintln!(
                            "cmux-shell: '{stroke}' is not a digit stroke; the \
                             {id} row is skipped"
                        );
                        continue;
                    }
                }
            } else {
                match action_for_id(id) {
                    Some(action) => action,
                    None => continue,
                }
            };
            table.push((combo, action));
        }
    }
    for name in bindings.keys() {
        if !DEFAULT_BINDINGS
            .iter()
            .any(|(known, _)| *known == name.as_str())
        {
            eprintln!(
                "cmux-shell: shortcut binding '{name}' does not name a known action; ignored"
            );
        }
    }
    table
}

/// `Key::Num1`â€¦`Key::Num9` â†’ 0-based workspace index (`ctrl+1` â†’ 0).
/// `ctrl+0` selects nothing (workspaces number 1â€“9, like upstream).
fn digit_index(key: Key) -> Option<usize> {
    match key {
        Key::Num0 => None,
        Key::Num1 => Some(0),
        Key::Num2 => Some(1),
        Key::Num3 => Some(2),
        Key::Num4 => Some(3),
        Key::Num5 => Some(4),
        Key::Num6 => Some(5),
        Key::Num7 => Some(6),
        Key::Num8 => Some(7),
        Key::Num9 => Some(8),
        _ => None,
    }
}

/// Parses one stroke such as `ctrl+shift+r`: modifier tokens first, the key
/// last. Modifier names follow upstream's binding grammar with Windows
/// naming: `ctrl`, `shift`, `alt`; `win`/`cmd`-style tokens map to
/// `Modifiers::MAC_CMD`, which never fires on Windows (there the command
/// key IS Ctrl â€” prefer ctrl-based strokes).
fn parse_stroke(stroke: &str) -> Option<KeyCombo> {
    let parts: Vec<&str> = stroke
        .split('+')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect();
    let key_name = *parts.last()?;
    let mut mods = Modifiers::NONE;
    for part in &parts[..parts.len() - 1] {
        match *part {
            "ctrl" | "control" => mods.ctrl = true,
            "shift" => mods.shift = true,
            "alt" | "option" | "opt" => mods.alt = true,
            "cmd" | "win" | "super" | "meta" => mods.mac_cmd = true,
            _ => return None,
        }
    }
    Some(KeyCombo {
        key: egui_key_for(key_name)?,
        mods,
    })
}

/// Key-name â†’ egui key: letters, digits, named control keys, and single
/// punctuation characters (`-`, `=`, `,`, `.`, `/`, `\`, `;`, `'`, `` ` ``, `+`).
fn egui_key_for(name: &str) -> Option<Key> {
    let lower = name.to_ascii_lowercase();
    let mut chars = lower.chars();
    let first = chars.next();
    let second = chars.next();
    if let (Some(c), None) = (first, second) {
        if c.is_ascii_digit() {
            return Some(match c {
                '0' => Key::Num0,
                '1' => Key::Num1,
                '2' => Key::Num2,
                '3' => Key::Num3,
                '4' => Key::Num4,
                '5' => Key::Num5,
                '6' => Key::Num6,
                '7' => Key::Num7,
                '8' => Key::Num8,
                _ => Key::Num9,
            });
        }
        if c.is_ascii_alphabetic() {
            return Some(match c {
                'a' => Key::A,
                'b' => Key::B,
                'c' => Key::C,
                'd' => Key::D,
                'e' => Key::E,
                'f' => Key::F,
                'g' => Key::G,
                'h' => Key::H,
                'i' => Key::I,
                'j' => Key::J,
                'k' => Key::K,
                'l' => Key::L,
                'm' => Key::M,
                'n' => Key::N,
                'o' => Key::O,
                'p' => Key::P,
                'q' => Key::Q,
                'r' => Key::R,
                's' => Key::S,
                't' => Key::T,
                'u' => Key::U,
                'v' => Key::V,
                'w' => Key::W,
                'x' => Key::X,
                'y' => Key::Y,
                _ => Key::Z,
            });
        }
    }
    let key = match lower.as_str() {
        "tab" => Key::Tab,
        "enter" | "return" => Key::Enter,
        "space" => Key::Space,
        "esc" | "escape" => Key::Escape,
        "backspace" => Key::Backspace,
        "delete" | "del" => Key::Delete,
        "insert" => Key::Insert,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        "up" => Key::ArrowUp,
        "down" => Key::ArrowDown,
        "left" => Key::ArrowLeft,
        "right" => Key::ArrowRight,
        "minus" | "-" => Key::Minus,
        "plus" => Key::Plus,
        "equals" | "=" => Key::Equals,
        "bracketleft" | "[" => Key::OpenBracket,
        "bracketright" | "]" => Key::CloseBracket,
        "comma" | "," => Key::Comma,
        "period" | "." => Key::Period,
        "slash" | "/" => Key::Slash,
        "backslash" | "\\" => Key::Backslash,
        "semicolon" | ";" => Key::Semicolon,
        "quote" | "'" => Key::Quote,
        "backtick" | "`" => Key::Backtick,
        _ => return None,
    };
    Some(key)
}

// ---------------------------------------------------------------------------
// Design lane 3 helpers: the command bar (node 1:512) and the palette
// that opens from the keymap's `commandPalette` stroke (row 13). The
// lane's sub-header chips (node 1:5) retired in owner round 4. Kept
// free and pure where geometry/text decisions do not touch app state,
// so the wndproc route and the painter agree by construction.
// ---------------------------------------------------------------------------

/// The palette's keyed outcomes for one frame.
#[derive(Debug)]
enum PaletteOutcome {
    Close,
    Activate,
    Move(i32),
}

/// One palette row: the two kinds the port's features back — config
/// custom commands (the M6 `commandConfig` surface) and jump targets
/// (this instance's workspaces and panes).
#[derive(Debug, Clone, PartialEq, Eq)]
enum PaletteEntry {
    Command {
        name: String,
        text: String,
    },
    Jump {
        label: String,
        workspace: usize,
        pane: String,
    },
}

impl PaletteEntry {
    fn label(&self) -> &str {
        match self {
            PaletteEntry::Command { name, .. } => name,
            PaletteEntry::Jump { label, .. } => label,
        }
    }

    fn detail(&self) -> &str {
        match self {
            PaletteEntry::Command { text, .. } => text,
            PaletteEntry::Jump { label, .. } => label,
        }
    }
}

/// The case-insensitive substring filter behind the query line: a row
/// matches when the query hits its label or (for commands) its body.
/// An empty query passes everything.
fn palette_filter(rows: &[PaletteEntry], query: &str) -> Vec<PaletteEntry> {
    let needle = query.trim().to_ascii_lowercase();
    if needle.is_empty() {
        return rows.to_vec();
    }
    rows.iter()
        .filter(|row| {
            row.label().to_ascii_lowercase().contains(&needle)
                || row.detail().to_ascii_lowercase().contains(&needle)
        })
        .cloned()
        .collect()
}

// Owner round 4 (2026-10-26): the keycap helpers (`humanize_key`,
// `keycap_tokens`) retired with the sub-header pills. Restoration:
// figma-design-reference.md sec. 17 (bodies preserved there).

/// The agent-state gate for the status bar (lane 2): the active
/// workspace has seen a detected agent session when any terminal lane
/// holds a pending or delivered turn-boundary slot (M3.4 detector
/// evidence). (The sub-header chip consumer went out with the band —
/// owner round 4.)
fn active_agent_turns(tree: &cmux_shell::panes::PaneTree<Pane>) -> bool {
    tree.ordered_leaf_ids().iter().any(|id| {
        matches!(
            tree.leaf(id.as_str()).map(|pane| &pane.body),
            Some(PaneBody::Terminal(lane))
                if !(lane.pending_turns.is_empty() && lane.delivered_turns.is_empty())
        )
    })
}

/// One terminal-prompt glyph, decoded from the design's lane-3 SVG
/// assets (both are the same rounded terminal window — 1.08-unit side
/// bezel, 2.17-unit top bezel — with a `›` chevron and a baseline
/// underscore; git-chip icon 10.833×8.667 `#8ED5FF`, command-bar icon
/// 11.667×9.333 `#BDC8D1`). `backdrop` fills the inner window at these
/// pixel sizes (the SVG path punches the interior out).
// clippy::approx_constant — the chevron's SVG y coordinate is 6.28 design
// units, not the circle constant; the glyph is angle-free.
#[allow(clippy::approx_constant)]
fn paint_terminal_prompt_glyph(
    painter: &egui::Painter,
    center: egui::Pos2,
    w: f32,
    h: f32,
    ink: egui::Color32,
    backdrop: egui::Color32,
) {
    let sx = w / 10.833;
    let sy = h / 8.667;
    let origin = egui::pos2(center.x - w * 0.5, center.y - h * 0.5);
    let outer = egui::Rect::from_min_size(origin, egui::vec2(w, h));
    painter.rect_filled(outer, egui::CornerRadius::same(1), ink);
    let inner = egui::Rect::from_min_max(
        egui::pos2(origin.x + sx, origin.y + 2.167 * sy),
        egui::pos2(origin.x + 9.75 * sx, origin.y + 7.583 * sy),
    );
    painter.rect_filled(inner, 0.0, backdrop);
    let chevron = [
        origin + egui::vec2(2.21 * sx, 3.47 * sy),
        origin + egui::vec2(3.62 * sx, 4.87 * sy),
        origin + egui::vec2(2.21 * sx, 6.28 * sy),
    ];
    let stem = egui::Stroke::new(1.083 * sx, ink);
    painter.add(egui::Shape::line(chevron.to_vec(), stem));
    let underscore = egui::Rect::from_min_max(
        origin + egui::vec2(5.42 * sx, 5.96 * sy),
        origin + egui::vec2(8.67 * sx, 7.04 * sy),
    );
    painter.rect_filled(underscore, 0.0, ink);
}

// Owner round 4 (2026-10-26): the tray glyphs (`paint_vsplit_glyph`,
// `paint_equalize_glyph`) retired with the split tray surface.
// Restoration bodies live in figma-design-reference.md sec. 17.

impl eframe::App for App {
    /// Saves the session snapshot on shutdown (mirrors upstream's
    /// terminate-time save; the 8 s autosave is a recorded divergence).
    /// Smoke modes that must not touch the store run with `session_paths`
    /// cleared to `None`.
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // WebView2 shutdown first: explicit controller Close on every
        // browser surface, regardless of whether a snapshot saves below.
        self.close_all_browser_hosts();
        let Some(paths) = &self.session_paths else {
            return;
        };
        let snapshot = self.build_session_snapshot();
        let pane_count = snapshot.richness().panels;
        match cmux_workspaces::save(&snapshot, paths) {
            Ok(()) => eprintln!(
                "cmux-shell: session snapshot saved ({} pane(s)) to {}",
                pane_count,
                paths.primary.display()
            ),
            Err(error) => eprintln!("cmux-shell: session snapshot not saved: {error}"),
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        // Reader threads wake this context whenever a chunk or event is
        // delivered into their channels (FrameWake); must be installed
        // before anything else so the wake is armed from the first frame.
        self.frame_wake.install(ui.ctx().clone());
        // The accelerator router wakes the frame loop the same way when a
        // webview forwards an app chord (the egui window never sees
        // keyboard input while the WebView2 child owns native focus).
        if !self.accel_wake_set {
            if let Some(_panel) = frame.winit_window() {
                let ctx = ui.ctx().clone();
                self.accel_router
                    .set_wake(Box::new(move || ctx.request_repaint()));
                self.accel_wake_set = true;
            }
        }
        // Platform facts once per frame: the HWND parent for WebView2
        // hosts, the DPI scale for rasterization, and the outer position
        // for accessibility notifications (all read from the winit window;
        // `scale_factor`/`outer_position` are inherent methods).
        if let Some(window) = frame.winit_window() {
            use raw_window_handle::{HasWindowHandle as _, RawWindowHandle};
            self.platform_hwnd =
                window
                    .window_handle()
                    .ok()
                    .and_then(|handle| match handle.as_raw() {
                        // `Win32WindowHandle::hwnd` is already a
                        // `NonZeroIsize` in raw-window-handle 0.6.
                        RawWindowHandle::Win32(raw) => Some(raw.hwnd.get()),
                        _ => None,
                    });
            self.dpi_scale = window.scale_factor();
            self.window_pos = window.outer_position().ok().map(|point| (point.x, point.y));
            // Design lane 2: the painted 32px caption rides the NCHITTEST
            // subclass (native drag/snap/resize semantics). Install is
            // idempotent, so every frame is cheap and first-frame safe.
            #[cfg(windows)]
            if let Some(hwnd) = self.platform_hwnd {
                title_bar_wndproc::install(hwnd);
            }
        }
        // The viewport's keyboard focus feeds the M3.2 arrival policy
        // (upstream's isAppFocused); read before any delivery decision.
        self.is_app_focused = ui.ctx().input(|input| input.focused);
        // The palette's modal pass runs before the keybindings: while it
        // is up it takes the whole keyboard (typing must not echo into
        // panes, and Ctrl+K belongs to the modal, not the table).
        self.serve_palette(ui.ctx());
        self.process_bindings(ui);
        // Chords the WebView2 accelerator router intercepted while a
        // browser pane owned native focus rejoin the action stream here
        // (mutually exclusive with egui's own routing by construction: the
        // accelerator event only fires while the webview is focused).
        let chords = self.accel_router.take_chords();
        if !chords.is_empty() {
            let names: Vec<String> = chords.iter().map(|key| format!("0x{key:02X}")).collect();
            eprintln!("cmux-shell: webview chords drained ({})", names.join(","));
        }
        for key in chords {
            if let Some(action) = action_for_accel_key(key) {
                self.actions.push(action);
            }
        }
        // Native GotFocus reports (M5.2 focus unification): where the
        // keyboard actually went — clicks landing on the covered Chromium
        // HWND, alt-tab restores onto a last-focused child. A pane outside
        // the ACTIVE workspace must not claim focus (its controller is
        // parked on IsVisible(false)); an already-focused pane is a no-op.
        let focus_gained = self.accel_router.take_focus_gained();
        for surface in focus_gained {
            let (is_leaf, is_focused) = {
                let workspace = self.workspaces.active_workspace();
                (
                    workspace.tree.leaf(&surface).is_some(),
                    workspace.focused == surface,
                )
            };
            if is_leaf && !is_focused {
                self.set_active_focus(surface);
            } else if !is_leaf {
                eprintln!(
                    "cmux-shell: webview focus ignored (not an active-workspace leaf): {surface}"
                );
            }
        }
        self.process_schedule();
        self.poll_children(ui.ctx());
        self.drain_actions();

        let mut style = TerminalStyle::from_visuals(ui.visuals());
        // Terminal font follows the config magnification percent (egui's
        // 12px monospace base Ã— percent, clamped at application time).
        style.font = egui::FontId::monospace(12.0 * self.runtime.font_scale());
        // Configured theme overrides: terminal scheme (dark/light) and,
        // optionally, the cursor color.
        match self.runtime.theme.scheme {
            Some(true) => style.scheme = egui_tty::ColorScheme::Light,
            Some(false) => style.scheme = egui_tty::ColorScheme::Dark,
            None => {}
        }
        if let Some(cursor) = self.runtime.theme.cursor {
            style.cursor = egui::Color32::from_rgb(
                ((cursor >> 16) & 0xFF) as u8,
                ((cursor >> 8) & 0xFF) as u8,
                (cursor & 0xFF) as u8,
            );
        }
        let ctx = ui.ctx().clone();

        // The vertical workspace strip (upstream: a tab IS a workspace)
        // takes a fixed slice of the window; the active workspace's pane
        // tree fills the rest. The strip shrinks on narrow windows.
        let full = ui.max_rect();
        // Design lane 2: the title bar (node 1:495) spans the whole
        // window top; the aside and the panes both start below it.
        let title_rect = egui::Rect::from_min_max(
            egui::pos2(full.left(), full.top()),
            egui::pos2(full.right(), full.top() + TITLE_BAR_HEIGHT),
        );
        let sidebar_width = if self.sidebar_visible {
            SIDEBAR_WIDTH
                .min(full.width() * SIDEBAR_MAX_FRACTION)
                .max(40.0)
        } else {
            0.0
        };
        let sidebar_rect = egui::Rect::from_min_max(
            egui::pos2(full.left(), title_rect.bottom()),
            egui::pos2(full.left() + sidebar_width, full.bottom()),
        );
        // Owner round 4 (2026-10-26): the design lane 3 sub-header band
        // (node 1:5) went out with its surface — main starts at the
        // title row's bottom edge again. Restoration:
        // figma-design-reference.md sec. 11 plus the sec. 17 record.
        let status_bar_rect = egui::Rect::from_min_max(
            egui::pos2(sidebar_rect.right(), full.bottom() - STATUS_BAR_HEIGHT),
            egui::pos2(full.right(), full.bottom()),
        );
        let main_rect = egui::Rect::from_min_max(
            egui::pos2(sidebar_rect.right(), title_rect.bottom()),
            egui::pos2(full.right(), status_bar_rect.top()),
        );

        let mut focused_now: Option<String> = None;
        // Explicit pane clicks this frame (both branches of render_pane):
        // the model's click-claim input, independent of egui transitions.
        let mut clicked_now: Option<String> = None;
        // M5.2 invariant: when a browser leaf owns the model focus, egui
        // holds NO standing keyboard focus (the keyboard lives in the
        // webview child). Without this, egui's memory re-installs a past
        // focused widget on remount (workspace re-entry) and the next
        // frame's transition reconcile steals model focus back from the
        // browser pane even though nothing took the keyboard (observed in
        // the M5.2 smoke: a re-select logged a terminal reclaim while the
        // webview child owned the OS keyboard; a later webview click had
        // to repair the model).
        let surrender_egui = {
            let workspace = self.workspaces.active_workspace();
            workspace
                .tree
                .leaf(&workspace.focused)
                .map(|pane| pane.body.is_browser())
                .unwrap_or(false)
        };
        // Browser panes laid out in the active tree this frame (the sync
        // pass below pins their child HWNDs to these rects).
        let mut browser_bounds: Vec<(String, egui::Rect)> = Vec::new();

        // Background first; panes are laid out inside the non-sidebar
        // remainder. The fill is the configured chrome background (it
        // shows through the gutters and flush pane margins).
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(egui::Color32::from_rgb(
                ((self.runtime.theme.background >> 16) & 0xFF) as u8,
                ((self.runtime.theme.background >> 8) & 0xFF) as u8,
                (self.runtime.theme.background & 0xFF) as u8,
            )))
            .show(ui, |ui| {
                let mut pane_area = ui.new_child(egui::UiBuilder::new().max_rect(main_rect));
                let container = main_rect;
                let container_rect = PixelRect::new(
                    f64::from(container.left()),
                    f64::from(container.top()),
                    f64::from(container.width()),
                    f64::from(container.height()),
                );
                // The focused id is read before the tree borrow below.
                let focused = self.workspaces.items[self.workspaces.active]
                    .focused
                    .clone();
                // Lane 5: this frame's agent-stream facts for the pane
                // painting (bounded cards + pending turn boundaries).
                let agent_stream = self.agent_stream_snapshot();
                let viewport_fill = token_color(self.runtime.theme.viewport_background);
                let tree = &mut self.workspaces.active_workspace_mut().tree;
                let layout = tree.layout(container_rect);

                // Visual separation: each pane renders half of the shared
                // gutter on sides that touch a neighbor (flush sides stay
                // full-bleed). Borders stroke after the terminals: the
                // focused pane wears the 1px accent ring, the rest none
                // (design: gutter-only separation).
                let mut borders: Vec<(egui::Rect, bool)> = Vec::new();
                for (pane_id, pane_rect) in &layout {
                    let inset = gutter_inset(&layout, pane_id, self.runtime.pane_gutter);
                    let bounding = egui::Rect::from_min_max(
                        egui::Pos2::new(
                            (pane_rect.x + inset.left) as f32,
                            (pane_rect.y + inset.top) as f32,
                        ),
                        egui::Pos2::new(
                            (pane_rect.x + pane_rect.width - inset.right) as f32,
                            (pane_rect.y + pane_rect.height - inset.bottom) as f32,
                        ),
                    );
                    borders.push((bounding, *pane_id == focused));
                    // Browser panes get their child HWND pinned to this rect
                    // by the post-layout sync pass (M5.1); egui paints only
                    // the failure overlay inside.
                    if tree
                        .leaf(pane_id)
                        .map(|pane| pane.body.is_browser())
                        .unwrap_or(false)
                    {
                        browser_bounds.push(((*pane_id).clone(), bounding));
                    }
                    let mut pane_ui =
                        pane_area.new_child(egui::UiBuilder::new().max_rect(bounding));
                    render_pane(
                        tree,
                        &mut pane_ui,
                        pane_id,
                        &mut PanePaintScope {
                            style: &style,
                            viewport_fill,
                            focused_now: &mut focused_now,
                            clicked_now: &mut clicked_now,
                            surrender: surrender_egui,
                            agent_stream: Some(&agent_stream),
                        },
                    );
                }

                // Borders after every terminal, so they sit on top;
                // StrokeKind::Inside keeps the stroke within the pane
                // instead of clipping glyphs of the neighbor. The design
                // separates panes with the gutter alone; only the focused
                // pane carries a stroke (the 1px accent ring).
                for (rect, focused) in borders {
                    let stroke = if focused {
                        egui::Stroke::new(1.0, token_color(TOKEN_ACCENT))
                    } else {
                        egui::Stroke::NONE
                    };
                    ui.painter().rect_stroke(
                        rect,
                        CornerRadius::same(PANE_ROUNDING),
                        stroke,
                        StrokeKind::Inside,
                    );
                }
            });

        // Design lane 2: the status bar paints after the panes so it sits
        // above them in z-order at the main area's bottom edge.
        self.draw_status_bar(ui, status_bar_rect);

        // Model-level focus reconciliation (M5.1 rule + M5.2 click claims);
        // the decision is pure and unit-tested (`focus_reconcile_target`).
        let active_focused = self.workspaces.active_workspace().focused.clone();
        if let Some(id) = focus_reconcile_target(
            clicked_now.as_deref(),
            focused_now.as_deref(),
            &active_focused,
            self.last_egui_focus.as_deref(),
        ) {
            self.set_active_focus(id);
        }
        // Remember this frame's egui focus; only a change against it counts
        // as a keyboard transition above.
        self.last_egui_focus = focused_now;

        // The sidebar paints and takes clicks after the terminals, so the
        // chrome never routes a click into a pane.
        if self.sidebar_visible {
            let actions = self.draw_sidebar(ui, sidebar_rect);
            self.actions.extend(actions);
        }

        // The title row paints last so its fill rides above the aside
        // top edge and any pane gutter in the first 32pt; its own
        // control widgets take clicks here. (The sub-header row used to
        // paint beside it; the band went out with its surface — owner
        // round 4.)
        self.draw_title_bar(ui, title_rect);
        // The palette overlay paints above everything (foreground order).
        self.paint_palette_overlay(ui, full);

        // Inactive workspaces keep running: one poll per hidden pane per
        // frame drains PTY output without rendering. (A hidden pane's PTY
        // size is refreshed only when its tab is shown â€” a recorded slice
        // limitation: output written while hidden wraps at the last shown
        // width.)
        for index in 0..self.workspaces.items.len() {
            if index == self.workspaces.active {
                continue;
            }
            let ids = self.workspaces.items[index].tree.ordered_leaf_ids();
            for id in ids {
                if let Some(pane) = self.workspaces.items[index].tree.leaf_mut(&id) {
                    match &mut pane.body {
                        PaneBody::Terminal(lane) => {
                            let _ = lane.terminal.poll();
                        }
                        // The browser sync pass parks hidden panes on
                        // IsVisible(false); nothing to poll here.
                        PaneBody::Browser(_) => {}
                        // No child process to poll either (lane 5).
                        PaneBody::Agent(_) => {}
                    }
                }
            }
        }

        // WebView2 sync pass: deferred hosts are created now that window
        // facts exist, then every browser pane gets bounds/visibility/focus
        // and a fresh polled snapshot (M5.1).
        self.sync_browser_surfaces(&browser_bounds);

        if self.pending_window_close {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

impl App {
    /// Samples the machine CPU% and RAM-in-use for the status bar on the
    /// ~0.75s cadence (per-frame reads are noise). The first completed
    /// interval between two `GetSystemTimes` snapshots is the earliest
    /// honest percentage.
    fn refresh_status_sample(&mut self) {
        let now = Instant::now();
        let due = self
            .status_state
            .as_ref()
            .map(|previous| {
                now.duration_since(previous.at).as_millis() >= STATUS_SAMPLE_INTERVAL_MS
            })
            .unwrap_or(true);
        if !due {
            return;
        }
        match host_status_sample() {
            Some(sample) => {
                let cpu = self.status_state.as_ref().and_then(|previous| {
                    system_cpu_percent((previous.idle, previous.total), (sample.idle, sample.total))
                });
                self.status_state = Some(StatusState {
                    at: now,
                    idle: sample.idle,
                    total: sample.total,
                    cpu_percent: cpu,
                    mem_used: sample.mem_used,
                });
            }
            None => self.status_state = None,
        }
    }

    // Owner round 4 (2026-10-26): the git sample pump retired with the
    // chips; light bodies + plumbing preserved in
    // figma-design-reference.md sec. 17.

    /// Design lane 4 (row 12): sample the services segment on a ~2s
    /// human cadence. The UI sends each workspace's terminal
    /// panes' tree-leader pids; the worker answers with display rows for
    /// every workspace, and the sidebar only reads its active slot.
    fn refresh_service_sample(&mut self) {
        let due = self
            .services_requested_at
            .map(|at| at.elapsed().as_millis() >= SERVICE_SAMPLE_INTERVAL_MS)
            .unwrap_or(true);
        if due {
            let mut roots: Vec<(usize, u32)> = Vec::new();
            for (index, workspace) in self.workspaces.items.iter().enumerate() {
                for id in workspace.tree.ordered_leaf_ids() {
                    if let Some(pane) = workspace.tree.leaf(&id) {
                        match &pane.body {
                            PaneBody::Terminal(_) => {
                                if let Some(pid) = pane.tree_pid {
                                    roots.push((index, pid));
                                }
                            }
                            PaneBody::Browser(_) => {}
                            PaneBody::Agent(_) => {}
                        }
                    }
                }
            }
            if let Some(requests) = &self.service_requests {
                if requests.send(roots).is_ok() {
                    self.services_requested_at = Some(Instant::now());
                }
            }
        }
        while let Ok(rows) = self.service_results.try_recv() {
            self.service_rows = rows.into_iter().collect();
        }
    }

    /// Lane 2 status bar (file nodes 1:371..1:398): a 24px viewport-dark
    /// strip at the main area's bottom edge. Left cluster: the ACTIVE
    /// workspace's identity segment (`w1 · powershell`, owner round 6)
    /// followed by the design's group separator, then the machine
    /// CPU/MEM and agent telemetry at a smaller 10px font (owner
    /// round 3, 2026-10-26: "make the status that tell the cpu and
    /// memory the font is smaller so it never collapse"). Right cluster
    /// (11px): this instance's control-pipe leaf
    /// (the port's honest stand-in for the design's listener address)
    /// and the UTF-8 contract the port enforces through ConPTY. The
    /// design's product chip (`cmux-win11`, nodes 1:397/1:398) is NOT
    /// painted: owner round 6 (2026-10-26) reversed the sec-16
    /// keep-back with "okay remove it" - a deliberate mock divergence
    /// (figma sec 19). The tmux-style pane-marker run (`1:powershell*`,
    /// the design's per-pane markers) is NOT painted either: owner
    /// round 8 (2026-10-27) "in the status bar remove 1:powershell*" -
    /// the markers retired with the strip; panes remain visible in the
    /// workspace/sidebar and the a11y strip label still counts them
    /// (deliberate mock divergence, figma sec 21).
    /// The design's small address glyph is
    /// not drawn (no icon font in the port; recorded residual). Budget:
    /// the right cluster is measured first (it owns the fixed right
    /// edge) and the telemetry run is RESERVED before the ws label
    /// chooses its variant (design round 1, 2026-10-26; long pane
    /// titles previously collided with the right cluster - the living
    /// repro that shaped the reservation).
    fn draw_status_bar(&mut self, ui: &mut egui::Ui, bar: egui::Rect) {
        self.refresh_status_sample();

        let active = self.workspaces.active;
        // The active workspace's id + live name, lifted above the a11y
        // label and reused as the left cluster's leading segment
        // (owner round 6). `ws_label_id` is the fold matrix's minimal
        // variant: the minted id alone is the owner's switch tracker and
        // never leaves the strip when the name does not fit.
        let ws_label = status_workspace_label(
            &self.workspaces.items[active].id,
            &self.workspaces.name_of(active),
        );
        let ws_label_id = self.workspaces.items[active].id.clone();
        let tree = &self.workspaces.items[active].tree;
        let pane_count = tree.ordered_leaf_ids().len();
        // Agent state: the active workspace has seen a detected agent
        // session when any terminal lane holds a pending or delivered
        // turn-boundary slot (M3.4 detector evidence). (Lane 3's own
        // chip consumer went out with the sub-header band — round 4.)
        let agent_active = active_agent_turns(tree);
        let sample_cpu = self
            .status_state
            .as_ref()
            .and_then(|state| state.cpu_percent);
        let sample_mem = self.status_state.as_ref().map(|state| state.mem_used);

        let painter = ui.painter().clone();
        painter.rect_filled(
            bar,
            0.0,
            token_color(self.runtime.theme.viewport_background),
        );
        let cy = bar.center().y;
        let left_font = egui::FontId::proportional(12.0);
        // Owner round 3 (2026-10-26): the machine + agent telemetry
        // (`|` + CPU/MEM + agent state) paints a smaller font so the
        // reserved run practically never collides with the left
        // cluster's labels. Measured with the same font it paints, so
        // the reservation below follows the smaller widths.
        let telemetry_font = egui::FontId::proportional(10.0);
        let right_font = egui::FontId::proportional(11.0);

        // --- right cluster measurements ------------------------------------
        // The right cluster owns the fixed right edge (the leaf anchors to
        // the strip's right pad). The machine + agent telemetry is
        // RESERVED before the ws label chooses its variant (design round
        // 1, 2026-10-26); the leaf has two variants for the owner round 6
        // fold matrix, measured with the font each paints.
        let utf_galley = painter.layout_no_wrap(
            "UTF-8".to_owned(),
            right_font.clone(),
            token_color(TOKEN_TEXT_MUTED),
        );
        let measure = |text: &str, font: &egui::FontId| -> f32 {
            painter
                .layout_no_wrap(text.to_owned(), font.clone(), egui::Color32::WHITE)
                .size()
                .x
        };
        let separator_width = measure("|", &telemetry_font);
        let cpu_text = status_cpu_label(sample_cpu);
        let cpu_width = measure(&cpu_text, &telemetry_font);
        let mem_text = status_mem_label(sample_mem);
        let mem_width = measure(&mem_text, &telemetry_font);
        let agent_text = status_claude_label(agent_active);
        let agent_width = measure(&agent_text, &telemetry_font);
        // The whole `|` + machine + agent run is reserved before the ws
        // label picks a variant; 10px gaps flank the separator, 4px
        // separate the machine run's parts (the lane-2 recipe).
        let telemetry_span =
            separator_width + 10.0 + cpu_width + 4.0 + mem_width + 4.0 + agent_width;

        // The leaf's two variants and the ws segment's two variants (the
        // full `id · name` label, and the id alone - the owner's switch
        // tracker). Spans include the group separator flanked by 10px gaps.
        let leaf_full_label = status_ipc_label_capped(self.ipc_pipe.as_str(), STATUS_IPC_LEAF_MAX);
        let leaf_short_label =
            status_ipc_label_capped(self.ipc_pipe.as_str(), STATUS_IPC_LEAF_SHORT);
        let leaf_w_full = measure(&leaf_full_label, &right_font);
        let leaf_w_short = measure(&leaf_short_label, &right_font);
        let ws_sep_width = measure("|", &left_font);
        let ws_full_span = measure(&ws_label, &left_font) + 10.0 + ws_sep_width + 10.0;
        let ws_id_span = measure(&ws_label_id, &left_font) + 10.0 + ws_sep_width + 10.0;
        // Space for [ws + leaf] with NEITHER reserved-variant assumption:
        // everything after the fixed right cluster minimum, the 10px
        // ws/telemetry gap, and the telemetry run. BOTH 12px anchor gaps
        // (bar pad -> UTF-8 -> leaf) are subtracted so this is the SAME
        // inequality the paint pass applies below - a mismatch here would
        // pick full variants the paint then drops.
        let free_leafless = (bar.right() - 8.0 - 12.0 - utf_galley.size().x - 12.0)
            - 10.0
            - telemetry_span
            - (bar.left() + 8.0);
        let (ws_named, leaf_short) = status_ws_fit_case(
            free_leafless,
            ws_full_span,
            ws_id_span,
            leaf_w_full,
            leaf_w_short,
        );
        let (ws_paint_label, leaf_paint_label, _leaf_paint_w) = match (ws_named, leaf_short) {
            (true, false) => (ws_label.clone(), leaf_full_label.clone(), leaf_w_full),
            (true, true) => (ws_label.clone(), leaf_short_label.clone(), leaf_w_short),
            (false, false) => (ws_label_id.clone(), leaf_full_label.clone(), leaf_w_full),
            (false, true) => (ws_label_id.clone(), leaf_short_label.clone(), leaf_w_short),
        };
        let ws_galley = painter.layout_no_wrap(
            ws_paint_label.clone(),
            left_font.clone(),
            token_color(TOKEN_TEXT_SECONDARY),
        );
        let ws_sep_galley = painter.layout_no_wrap(
            "|".to_owned(),
            left_font.clone(),
            token_color(TOKEN_TEXT_MUTED),
        );
        let ipc_galley = painter.layout_no_wrap(
            leaf_paint_label.clone(),
            right_font.clone(),
            token_color(TOKEN_TEXT_MUTED),
        );
        // One accessible region for the whole strip: its segments are
        // painted text with no widgets, so the a11y label is attached the
        // same way the custom-painted sidebar rows are (M6 precedent). It
        // reads the ws label AS PAINTED (the fold matrix's verdict) and
        // still counts the workspace's real panes (owner round 8: the
        // pane markers left the strip; the panes themselves remain).
        ui.allocate_rect(bar, egui::Sense::hover()).widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::Label,
                true,
                format!(
                    "Status bar, {} workspace, {} pane(s)",
                    ws_paint_label, pane_count
                ),
            )
        });

        // Paint pass: ws label, the design's group separator, then the
        // machine + agent telemetry run; the right cluster anchors to
        // the strip's right pad regardless of the left cluster's width.
        let mut x = bar.left() + 8.0;
        let ws_width = ws_galley.size().x;
        painter.galley(
            egui::pos2(x, cy - ws_galley.size().y * 0.5),
            ws_galley,
            egui::Color32::WHITE,
        );
        x += ws_width + 10.0;
        let ws_sep_width = ws_sep_galley.size().x;
        painter.galley(
            egui::pos2(x, cy - ws_sep_galley.size().y * 0.5),
            ws_sep_galley,
            egui::Color32::WHITE,
        );
        x += ws_sep_width + 10.0;
        // `|` separator, then machine + agent segments (the single 10px
        // gap above now spans ws->telemetry; 4px separates the CPU/MEM/
        // agent run's parts - the design's mock data).
        let galley = painter.text(
            egui::pos2(x, cy),
            egui::Align2::LEFT_CENTER,
            "|",
            telemetry_font.clone(),
            token_color(TOKEN_TEXT_MUTED),
        );
        x += galley.size().x + 10.0;
        let galley = painter.text(
            egui::pos2(x, cy),
            egui::Align2::LEFT_CENTER,
            status_cpu_label(sample_cpu),
            telemetry_font.clone(),
            token_color(TOKEN_TEXT_MUTED),
        );
        x += galley.size().x + 4.0;
        let galley = painter.text(
            egui::pos2(x, cy),
            egui::Align2::LEFT_CENTER,
            status_mem_label(sample_mem),
            telemetry_font.clone(),
            token_color(TOKEN_TEXT_MUTED),
        );
        x += galley.size().x + 4.0;
        painter.text(
            egui::pos2(x, cy),
            egui::Align2::LEFT_CENTER,
            status_claude_label(agent_active),
            telemetry_font.clone(),
            token_color(if agent_active {
                TOKEN_INFO
            } else {
                TOKEN_TEXT_MUTED
            }),
        );

        // --- right cluster (painted) --------------------------------------
        // UTF-8 and the pipe leaf anchor to the strip's right pad (the
        // chip's left-edge anchor dropped with the chip itself).
        let mut x_right = bar.right() - 8.0 - utf_galley.size().x;
        painter.galley(
            egui::pos2(x_right, cy - utf_galley.size().y * 0.5),
            utf_galley,
            egui::Color32::WHITE,
        );
        x_right -= 12.0 + ipc_galley.size().x;
        painter.galley(
            egui::pos2(x_right, cy - ipc_galley.size().y * 0.5),
            ipc_galley,
            egui::Color32::WHITE,
        );
    }

    /// Lane 2 title bar (node 1:495 "Header"): a full-window 32px strip
    /// above both the aside and the panes — sidebar-toned fill with a
    /// bottom hairline; breadcrumbs at the left (`cmux / [chip workspace
    /// › pane]`), three native Win11-style control zones at the right
    /// whose click targets are egui widgets while their drag semantics
    /// ride the WM_NCHITTEST subclass. The design's mid-row command bar
    /// (node 1:512) and the INSPECTOR/avatar pair (node 1:524) are later
    /// lanes; the space between the clusters is a blank drag zone. The
    /// 16px logo glyph (node 1:497) needs no icon font and is skipped
    /// (recorded residual, like the status bar's address glyph).
    fn draw_title_bar(&mut self, ui: &mut egui::Ui, title: egui::Rect) {
        let active = self.workspaces.active;
        let workspace_name = self.workspaces.name_of(active);
        let focused_title = {
            let workspace = &self.workspaces.items[active];
            workspace
                .tree
                .leaf(&workspace.focused)
                .and_then(|pane| pane.display_title())
        };
        let (ws_name, pane_name) = title_crumb_texts(&workspace_name, focused_title.as_deref());

        let painter = ui.painter().clone();
        let strip_fill = egui::Color32::from_rgb(
            ((self.runtime.theme.sidebar_background >> 16) & 0xFF) as u8,
            ((self.runtime.theme.sidebar_background >> 8) & 0xFF) as u8,
            (self.runtime.theme.sidebar_background & 0xFF) as u8,
        );
        painter.rect_filled(title, 0.0, strip_fill);
        // Design: `border-b border-solid border-[rgba(62,72,79,0.4)]`.
        painter.line_segment(
            [
                egui::pos2(title.left(), title.bottom() - 1.0),
                egui::pos2(title.right(), title.bottom() - 1.0),
            ],
            egui::Stroke::new(1.0, hairline_color(TOKEN_HAIRLINE)),
        );

        // One accessible region for the strip (M6 custom-paint label
        // precedent). The window-control widgets allocated below hide it
        // at the right end, like the OS-caption region under its buttons.
        let a11y = ui.allocate_rect(title, egui::Sense::hover());
        a11y.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::Label,
                true,
                format!("Title bar: cmux / {ws_name} › {pane_name}, drag to move window"),
            )
        });

        // --- left cluster: cmux / workspace › pane -----------------------
        let mut x = title.left() + 8.0;
        let name_font = egui::FontId::proportional(13.0);
        let crumb_font = egui::FontId::proportional(12.0);
        let cmux_galley = painter.layout_no_wrap(
            "cmux".to_owned(),
            name_font.clone(),
            token_color(TOKEN_TEXT_PRIMARY),
        );
        painter.galley(
            egui::pos2(
                x,
                title.top() + title.height() * 0.5 - cmux_galley.size().y * 0.5,
            ),
            cmux_galley.clone(),
            egui::Color32::WHITE,
        );
        x += cmux_galley.size().x + 8.0;
        let slash_galley = painter.layout_no_wrap(
            "/".to_owned(),
            crumb_font.clone(),
            token_color(TOKEN_TEXT_MUTED),
        );
        painter.galley(
            egui::pos2(
                x,
                title.top() + title.height() * 0.5 - slash_galley.size().y * 0.5,
            ),
            slash_galley.clone(),
            egui::Color32::WHITE,
        );
        x += slash_galley.size().x + 8.0;

        // Workspace chip (node 1:502): chip fill, 4px corners, px4/py2
        // padding, 4px gaps: workspace name (≤140px) › pane title (≤100px).
        let ws_galley = fit_crumb_galley(
            &painter,
            &ws_name,
            crumb_font.clone(),
            token_color(TOKEN_TEXT_SECONDARY),
            140.0,
        );
        let ws_width = ws_galley.size().x;
        let sep_galley = painter.layout_no_wrap(
            "\u{203A}".to_owned(),
            crumb_font.clone(),
            token_color(TOKEN_TEXT_MUTED),
        );
        let pane_galley = fit_crumb_galley(
            &painter,
            &pane_name,
            crumb_font.clone(),
            token_color(TOKEN_INFO),
            100.0,
        );
        let chip_width =
            4.0 + ws_galley.size().x + 4.0 + sep_galley.size().x + 4.0 + pane_galley.size().x + 4.0;
        let chip_rect = egui::Rect::from_min_size(
            egui::pos2(x, title.top() + 5.0),
            egui::vec2(chip_width, title.height() - 10.0),
        );
        painter.rect_filled(
            chip_rect,
            4.0,
            token_color(self.runtime.theme.chip_background),
        );
        let mut cx = x;
        let cy = title.top() + title.height() * 0.5;
        painter.galley(
            egui::pos2(cx + 4.0, cy - ws_galley.size().y * 0.5),
            ws_galley,
            egui::Color32::WHITE,
        );
        cx += 4.0 + ws_width;
        painter.galley(
            egui::pos2(cx, cy - sep_galley.size().y * 0.5),
            sep_galley.clone(),
            egui::Color32::WHITE,
        );
        cx += sep_galley.size().x + 4.0;
        painter.galley(
            egui::pos2(cx, cy - pane_galley.size().y * 0.5),
            pane_galley,
            egui::Color32::WHITE,
        );

        // --- mid-row command bar (node 1:512; lane 3) ------------------
        // The design floats a 420×24 `#11131a` bar between the crumbs and
        // the native controls; clicking it (like Ctrl+K) opens the
        // palette. Hover has no design state; the chip fill is the
        // nearest palette step (recorded adaptation).
        if let Some(bar) = title_command_bar_rect_pts(title.width())
            .map(|bar| bar.translate(egui::vec2(title.left(), 0.0)))
        {
            let response = ui.allocate_rect(bar, egui::Sense::click());
            response.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::Button,
                    true,
                    "Command bar: dispatch, processes or jump to pane (Ctrl+K)".to_owned(),
                )
            });
            let fill = if response.hovered() {
                token_color(self.runtime.theme.chip_background)
            } else {
                token_color(self.runtime.theme.background)
            };
            painter.rect_filled(bar, 4.0, fill);
            paint_terminal_prompt_glyph(
                &painter,
                egui::pos2(bar.left() + 8.0 + 5.8335, bar.center().y),
                11.667,
                9.333,
                token_color(TOKEN_TEXT_SECONDARY),
                fill,
            );
            let placeholder = painter.layout_no_wrap(
                "Quick dispatch, processes or jump to pane...".to_owned(),
                crumb_font.clone(),
                token_color(TOKEN_TEXT_SECONDARY),
            );
            painter.galley(
                egui::pos2(
                    bar.left() + 8.0 + 11.667 + 4.0,
                    bar.center().y - placeholder.size().y * 0.5,
                ),
                placeholder.clone(),
                egui::Color32::WHITE,
            );
            // The `Ctrl` `K` keycap pills at the right edge: `#1d1f26`,
            // px4, 11pt muted caps (node 1:519/1:521).
            let pill_font = egui::FontId::proportional(11.0);
            let mut pill_widths = Vec::new();
            for token in ["Ctrl", "K"] {
                let galley = painter.layout_no_wrap(
                    token.to_owned(),
                    pill_font.clone(),
                    token_color(TOKEN_TEXT_MUTED),
                );
                pill_widths.push((token, galley.size().x));
            }
            let pills_right = bar.right() - 8.0;
            let mut pill_left =
                pills_right - pill_widths.iter().map(|(_, w)| 8.0 + *w).sum::<f32>() - 2.0;
            for (token, width) in pill_widths {
                let pill = egui::Rect::from_min_max(
                    egui::pos2(pill_left, bar.center().y - 7.0),
                    egui::pos2(pill_left + 8.0 + width, bar.center().y + 7.0),
                );
                painter.rect_filled(pill, 4.0, token_color(self.runtime.theme.chip_background));
                let galley = painter.layout_no_wrap(
                    token.to_owned(),
                    pill_font.clone(),
                    token_color(TOKEN_TEXT_MUTED),
                );
                painter.galley(
                    egui::pos2(pill.left() + 4.0, pill.center().y - galley.size().y * 0.5),
                    galley,
                    egui::Color32::WHITE,
                );
                pill_left = pill.right() + 2.0;
            }
            if response.clicked() {
                self.open_palette();
            }
        }

        // --- right cluster: three 44×32 control zones (1:534/1:537/1:540)
        let is_maximized =
            title_bar_wndproc::IS_MAXIMIZED.load(std::sync::atomic::Ordering::Acquire);
        let zone_w = TITLE_CONTROL_ZONE_W_PTS;
        let mut zone_left = title.right() - TITLE_CONTROL_ZONES * zone_w;
        let ctx_ui = ui.ctx().clone();
        for button in ["Minimize", "Maximize", "Close"].iter() {
            let zone = egui::Rect::from_min_size(
                egui::pos2(zone_left, title.top()),
                egui::vec2(zone_w, title.height()),
            );
            let response = ui.allocate_rect(zone, egui::Sense::click());
            response.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Button, true, (*button).to_owned())
            });
            let hovered = response.hovered();
            let ink = token_color(TOKEN_TEXT_SECONDARY);
            if hovered {
                let fill = if *button == "Close" {
                    token_color(TOKEN_TITLE_CLOSE_HOVER)
                } else {
                    token_color(TOKEN_TITLE_BTN_HOVER)
                };
                // Native Win11 caption buttons hover square (no radius).
                painter.rect_filled(zone, 0.0, fill);
            }
            let center = zone.center();
            let glyph_ink = if hovered && *button == "Close" {
                token_color(TOKEN_TITLE_CLOSE_INK)
            } else {
                ink
            };
            let glyph = egui::Stroke::new(1.0, glyph_ink);
            match *button {
                "Minimize" => {
                    painter.line_segment(
                        [
                            egui::pos2(center.x - 4.5, center.y),
                            egui::pos2(center.x + 4.5, center.y),
                        ],
                        glyph,
                    );
                }
                "Maximize" => {
                    if is_maximized {
                        // Restore glyph: back square peeking top-right,
                        // front square masked against the strip fill.
                        let back = egui::Rect::from_min_size(
                            egui::pos2(center.x - 1.0, center.y - 4.5),
                            egui::vec2(9.0, 9.0),
                        );
                        painter.rect_stroke(back, CornerRadius::same(1), glyph, StrokeKind::Inside);
                        let front = egui::Rect::from_min_size(
                            egui::pos2(center.x - 4.0, center.y - 1.5),
                            egui::vec2(9.0, 9.0),
                        );
                        painter.rect_filled(
                            front,
                            1.0,
                            if hovered {
                                token_color(TOKEN_TITLE_BTN_HOVER)
                            } else {
                                strip_fill
                            },
                        );
                        painter.rect_stroke(
                            front,
                            CornerRadius::same(1),
                            glyph,
                            StrokeKind::Inside,
                        );
                    } else {
                        let square = egui::Rect::from_center_size(center, egui::vec2(10.0, 10.0));
                        painter.rect_stroke(
                            square,
                            CornerRadius::same(1),
                            glyph,
                            StrokeKind::Inside,
                        );
                    }
                }
                "Close" => {
                    painter.line_segment(
                        [
                            egui::pos2(center.x - 4.5, center.y - 4.5),
                            egui::pos2(center.x + 4.5, center.y + 4.5),
                        ],
                        glyph,
                    );
                    painter.line_segment(
                        [
                            egui::pos2(center.x - 4.5, center.y + 4.5),
                            egui::pos2(center.x + 4.5, center.y - 4.5),
                        ],
                        glyph,
                    );
                }
                _ => {}
            }
            if response.clicked() {
                match *button {
                    "Minimize" => ctx_ui.send_viewport_cmd(egui::ViewportCommand::Minimized(true)),
                    "Maximize" => {
                        ctx_ui.send_viewport_cmd(egui::ViewportCommand::Maximized(!is_maximized))
                    }
                    "Close" => self.pending_window_close = true,
                    _ => {}
                }
            }
            zone_left += zone_w;
        }
    }

    // Owner round 4 (2026-10-26): the design lane 3 sub-header band
    // (node 1:5) went out with its surface. Restoration:
    // figma-design-reference.md sec. 11 plus the sec. 17 record.

    /// Design lane 3 (row 13): builds the palette row list once on open
    /// — config custom commands first (the M6 `commandConfig` read), then
    /// jump targets across this instance's workspaces and panes (the
    /// sidebar's click-to-switch surface as keyboard targets).
    fn open_palette(&mut self) {
        self.palette_open = true;
        self.palette_query.clear();
        self.palette_selected = 0;
        let mut rows = Vec::new();
        let config = read_commands_config();
        for definition in &config.commands {
            // Workspace-flavor commands have no supported run path yet
            // (the resolver rejects them); they would be dead rows.
            if definition.is_workspace_flavor() {
                continue;
            }
            rows.push(PaletteEntry::Command {
                name: definition.name.clone(),
                text: definition.command.as_deref().unwrap_or_default().to_owned(),
            });
        }
        for (index, workspace) in self.workspaces.items.iter().enumerate() {
            let workspace_name = self.workspaces.name_of(index);
            for id in workspace.tree.ordered_leaf_ids() {
                let title = workspace
                    .tree
                    .leaf(id.as_str())
                    .and_then(Pane::display_title)
                    .unwrap_or_else(|| "\u{2013}".to_owned());
                rows.push(PaletteEntry::Jump {
                    label: format!("{title} — {workspace_name}"),
                    workspace: index,
                    pane: id,
                });
            }
        }
        self.palette_rows = rows;
    }

    /// Design lane 3 (row 13): the modal palette's keyboard pass. Runs
    /// before `process_bindings` and takes the whole keyboard while up
    /// (the terminal lanes read the shared event stream; a modal that
    /// leaked keys would type into panes). Enter activates the selected
    /// row, arrows move it, Backspace/typing edit the query, Esc closes.
    fn serve_palette(&mut self, ctx: &egui::Context) {
        if !self.palette_open {
            return;
        }
        let mut consumed: Vec<egui::Event> = Vec::new();
        let mut outcome: Option<PaletteOutcome> = None;
        ctx.input(|input| {
            for event in &input.events {
                consumed.push(event.clone());
                match event {
                    egui::Event::Key {
                        key,
                        pressed: true,
                        repeat: false,
                        ..
                    } => match key {
                        Key::Escape => outcome = Some(PaletteOutcome::Close),
                        Key::Enter => outcome = Some(PaletteOutcome::Activate),
                        Key::ArrowDown => outcome = Some(PaletteOutcome::Move(1)),
                        Key::ArrowUp => outcome = Some(PaletteOutcome::Move(-1)),
                        Key::Backspace => {
                            self.palette_query.pop();
                        }
                        Key::Space => self.palette_query.push(' '),
                        _ => {}
                    },
                    egui::Event::Text(text) => self.palette_query.push_str(text),
                    _ => {}
                }
            }
        });
        if !consumed.is_empty() {
            ctx.input_mut(|input| input.events.retain(|event| !consumed.contains(event)));
        }
        match outcome {
            Some(PaletteOutcome::Close) => self.palette_open = false,
            Some(PaletteOutcome::Move(step)) => {
                let count = palette_filter(&self.palette_rows, &self.palette_query).len();
                if count > 0 {
                    // Wrap the selection instead of clamping (arrow
                    // cycling is the palette's muscle memory).
                    let next = self.palette_selected as isize + step as isize;
                    self.palette_selected = next.rem_euclid(count as isize) as usize;
                }
            }
            Some(PaletteOutcome::Activate) => {
                let rows = palette_filter(&self.palette_rows, &self.palette_query);
                let selected = self.palette_selected.min(rows.len().saturating_sub(1));
                if let Some(row) = rows.get(selected).cloned() {
                    self.palette_open = false;
                    self.activate_palette_row(row);
                } else {
                    self.palette_open = false;
                }
            }
            None => {}
        }
    }

    /// Executes one selected palette row: commands type their sanitized
    /// text into the ACTIVE workspace's focused terminal (the exact
    /// semantics of the M6 `command.run` wire, without the IPC plumbing);
    /// jumps hand the model focus to that workspace/pane (the sidebar
    /// click's claim surface).
    fn activate_palette_row(&mut self, row: PaletteEntry) {
        match row {
            PaletteEntry::Command { name, .. } => {
                let config = read_commands_config();
                let Ok(definition) = resolve_config_command(&config.commands, &name, false) else {
                    eprintln!("cmux-shell: palette command '{name}': not in the config file");
                    return;
                };
                let definition = definition.clone();
                drop(config);
                let (workspace_index, pane_id) = {
                    let workspace = self.workspaces.active_workspace();
                    (self.workspaces.active, workspace.focused.clone())
                };
                let pane = self
                    .workspaces
                    .items
                    .get_mut(workspace_index)
                    .and_then(|workspace| workspace.tree.leaf_mut(&pane_id));
                let Some(terminal) = pane.and_then(|pane| pane.terminal_mut()) else {
                    eprintln!("cmux-shell: palette command '{name}': no focused terminal");
                    return;
                };
                let text = cmux_config::sanitize_command_text(
                    definition.command.as_deref().unwrap_or_default(),
                );
                if text.is_empty() {
                    eprintln!("cmux-shell: palette command '{name}' sanitizes to nothing");
                    return;
                }
                if let Err(error) = terminal.send(&command_input_bytes(&text)) {
                    eprintln!("cmux-shell: palette command '{name}' send failed: {error:#}");
                }
            }
            PaletteEntry::Jump {
                workspace, pane, ..
            } => {
                if workspace == self.workspaces.active {
                    self.set_active_focus(pane);
                } else {
                    let at = workspace;
                    self.actions.push(Action::SelectWorkspace(at));
                    self.pending_jump = Some(pane);
                }
            }
        }
    }

    /// Renders the open palette's overlay (row 13). The design file has
    /// no overlay node — the bar's chrome extends: `#11131a` panel, 4px
    /// radius, hairline edge, one 12pt query line and index rows with
    /// the muted detail text (adaptation recorded in the reference doc).
    fn paint_palette_overlay(&mut self, ui: &mut egui::Ui, full: egui::Rect) {
        if !self.palette_open {
            return;
        }
        let rows = palette_filter(&self.palette_rows, &self.palette_query);
        if self.palette_selected >= rows.len().max(1) {
            self.palette_selected = self.palette_selected.min(rows.len().saturating_sub(1));
        }
        const PALETTE_WIDTH: f32 = 420.0;
        const ROW_HEIGHT: f32 = 24.0;
        const VISIBLE: usize = 9;
        let row_count = VISIBLE.min(rows.len());
        let height = 10.0 + 20.0 + 6.0 + row_count.max(1) as f32 * ROW_HEIGHT + 4.0;
        let anchor = egui::pos2(
            (full.center().x - PALETTE_WIDTH * 0.5).max(full.left() + 8.0),
            // Below the whole chrome: just the 32pt title strip since the
            // sub-header band retired (owner round 4).
            full.top() + TITLE_BAR_HEIGHT + 8.0,
        );
        let frame = egui::Rect::from_min_size(anchor, egui::vec2(PALETTE_WIDTH, height));
        let painter = ui.painter().clone();
        painter.rect_filled(frame, 4.0, token_color(self.runtime.theme.background));
        painter.rect_stroke(
            frame,
            egui::CornerRadius::same(4),
            egui::Stroke::new(1.0, hairline_color(TOKEN_HAIRLINE)),
            egui::StrokeKind::Middle,
        );
        ui.allocate_rect(frame, egui::Sense::hover())
            .widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::Label,
                    true,
                    format!(
                    "Command palette: query \u{201c}{}\u{201d}, {} rows, Enter runs, Esc closes",
                    self.palette_query,
                    rows.len()
                ),
                )
            });
        let query = painter.layout_no_wrap(
            format!("> {}", self.palette_query),
            egui::FontId::proportional(12.0),
            token_color(TOKEN_TEXT_PRIMARY),
        );
        painter.galley(
            egui::pos2(frame.left() + 10.0, frame.top() + 9.0),
            query.clone(),
            egui::Color32::WHITE,
        );
        // A blinking caret rides the query line's end (egui time; 500ms
        // half-period like the terminal's block cursor).
        let blink = (ui.input(|input| input.time) * 2.0).fract();
        if blink < 0.5 {
            let caret = egui::pos2(
                frame.left() + 10.0 + query.size().x + 2.0,
                frame.top() + 9.0,
            );
            painter.line_segment(
                [caret, caret + egui::vec2(0.0, 16.0)],
                egui::Stroke::new(1.0, token_color(TOKEN_TEXT_PRIMARY)),
            );
        }
        for (index, row) in rows.iter().enumerate().take(VISIBLE) {
            let row_rect = egui::Rect::from_min_max(
                egui::pos2(
                    frame.left() + 4.0,
                    frame.top() + 30.0 + index as f32 * ROW_HEIGHT,
                ),
                egui::pos2(
                    frame.right() - 4.0,
                    frame.top() + 30.0 + (index + 1) as f32 * ROW_HEIGHT,
                ),
            );
            if index == self.palette_selected {
                painter.rect_filled(
                    row_rect,
                    4.0,
                    token_color(self.runtime.theme.chip_background),
                );
            }
            let label = painter.layout_no_wrap(
                row.label().to_owned(),
                egui::FontId::proportional(12.0),
                token_color(TOKEN_TEXT_PRIMARY),
            );
            let detail = painter.layout_no_wrap(
                match row {
                    PaletteEntry::Command { .. } => row.detail().to_owned(),
                    PaletteEntry::Jump { .. } => "jump".to_owned(),
                },
                egui::FontId::proportional(11.0),
                token_color(TOKEN_TEXT_MUTED),
            );
            let click = ui.allocate_rect(row_rect, egui::Sense::click());
            click.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::Button,
                    true,
                    format!("{} — {}", row.label(), row.detail()),
                )
            });
            let mid = row_rect.center().y;
            painter.galley(
                egui::pos2(row_rect.left() + 8.0, mid - label.size().y * 0.5),
                label,
                egui::Color32::WHITE,
            );
            painter.galley(
                egui::pos2(
                    row_rect.right() - 8.0 - detail.size().x,
                    mid - detail.size().y * 0.5,
                ),
                detail,
                egui::Color32::WHITE,
            );
            if click.clicked() {
                let activated = row.clone();
                self.palette_open = false;
                self.activate_palette_row(activated);
            }
            if click.hovered() {
                self.palette_selected = index;
            }
        }
        if rows.is_empty() {
            let empty = painter.layout_no_wrap(
                "no matches".to_owned(),
                egui::FontId::proportional(12.0),
                token_color(TOKEN_TEXT_MUTED),
            );
            painter.galley(
                egui::pos2(frame.left() + 12.0, frame.top() + 34.0),
                empty,
                egui::Color32::WHITE,
            );
        }
    }

    /// Draws the vertical workspace strip: a + (new workspace) button and
    /// one row per workspace, each clickable to switch and carrying an ×
    /// close button; double-click opens the inline rename editor (owner
    /// round 9), a decisive drag reorders with a 2pt insertion line, and
    /// an overflowed list scrolls (wheel) with edge fades and drag
    /// autoscroll. The row name prefers the user's custom title over the
    /// upstream `processTitle` rule (the port's live shell title),
    /// falling back to a positional name. Returns the queued actions; the
    /// caller applies them after painting (a close or reorder would
    /// otherwise invalidate row indices mid-iteration).
    fn draw_sidebar(&mut self, ui: &mut egui::Ui, rect: egui::Rect) -> Vec<Action> {
        let mut actions = Vec::new();
        // The painter is cloned so allocate_rect's &mut ui borrow can't
        // alias it.
        let painter = ui.painter().clone();
        painter.rect_filled(
            rect,
            0.0,
            egui::Color32::from_rgb(
                ((self.runtime.theme.sidebar_background >> 16) & 0xFF) as u8,
                ((self.runtime.theme.sidebar_background >> 8) & 0xFF) as u8,
                (self.runtime.theme.sidebar_background & 0xFF) as u8,
            ),
        );
        // Design: the aside carries a 1px hairline boundary against the
        // main area (rgba(62,72,79,0.4)).
        painter.line_segment(
            [
                egui::pos2(rect.right() - 1.0, rect.top()),
                egui::pos2(rect.right() - 1.0, rect.bottom()),
            ],
            egui::Stroke::new(1.0, hairline_color(TOKEN_HAIRLINE)),
        );

        // + row (new workspace).
        let plus_rect = egui::Rect::from_min_max(
            egui::pos2(rect.left() + 8.0, rect.top() + 8.0),
            egui::pos2(rect.right() - 8.0, rect.top() + 34.0),
        );
        let plus = ui.allocate_rect(plus_rect, egui::Sense::click());
        // M6 accessibility: custom-painted region, so the a11y label is
        // attached by hand (the paint below carries no widget info).
        plus.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "New workspace".to_owned())
        });
        let plus_fill = if plus.hovered() {
            hairline_color(TOKEN_HAIRLINE_SOFT)
        } else {
            egui::Color32::TRANSPARENT
        };
        painter.rect_filled(plus_rect, 4.0, plus_fill);
        painter.text(
            plus_rect.center(),
            egui::Align2::CENTER_CENTER,
            "+",
            egui::FontId::proportional(16.0),
            token_color(TOKEN_TEXT_PRIMARY),
        );
        if plus.clicked() {
            actions.push(Action::NewWorkspace);
        }
        // Design: a soft hairline separates the header strip from the
        // workspace rows (rgba(62,72,79,0.3)).
        painter.line_segment(
            [
                egui::pos2(rect.left() + 8.0, plus_rect.bottom()),
                egui::pos2(rect.right() - 8.0, plus_rect.bottom()),
            ],
            egui::Stroke::new(1.0, hairline_color(TOKEN_HAIRLINE_SOFT)),
        );

        let top = plus_rect.bottom() + 6.0;
        let bottom_limit = rect.bottom() - 6.0;
        let row_advance = SIDEBAR_ROW_HEIGHT + 2.0;
        let count = self.workspaces.items.len();

        // Lane 4 services data (round 9: the segment scrolls WITH the
        // workspace rows — the strip is one scroll column like upstream's
        // LazyVStack; the data rules are unchanged).
        self.refresh_service_sample();
        let active_index = self.workspaces.active;
        let services: &[cmux_shell::port_state::PortRow] = self
            .service_rows
            .get(&active_index)
            .map(Vec::as_slice)
            .unwrap_or(&[]);

        // Scroll planning (owner round 9). Content px from `top`: the
        // workspace rows span `count * advance - 2` (the last row has no
        // trailing gap; -2 with no rows matches the pre-scroll header
        // anchor exactly), then the services segment (+12 gap per the
        // design's Margin 1:456, 14pt header, rows every 28pt). The
        // view below the plus row is the clip: [top, bottom_limit].
        let rows_span = count as f32 * row_advance - 2.0;
        let services_span = 12.0 + 14.0 + services.len() as f32 * 28.0;
        let content_height = rows_span + services_span;
        let viewport = (bottom_limit - top).max(0.0);
        let content_clip = egui::Rect::from_min_max(
            egui::pos2(rect.left(), top),
            egui::pos2(rect.right(), bottom_limit),
        );

        // Wheel scrolling: the wheel outputs ONLY when the pointer is
        // over the strip's content region (the terminal lane's hit
        // region excludes the aside, so no event stealing). egui's
        // smooth_scroll_delta is positive on a wheel-UP, which must move
        // the view toward the TOP of the content (smaller scroll).
        let (pointer_pos, pointer_pressing, wheel) = ui.input(|input| {
            (
                input.pointer.hover_pos(),
                input.pointer.primary_down(),
                input.smooth_scroll_delta().y,
            )
        });
        let pointer_over_content = pointer_pos.is_some_and(|pos| {
            pos.x >= rect.left() && pos.x <= rect.right() && pos.y >= top && pos.y <= bottom_limit
        });
        if wheel != 0.0 && pointer_over_content {
            self.sidebar_scroll = cmux_shell::sidebar::scroll_clamp(
                self.sidebar_scroll - wheel,
                content_height,
                viewport,
            );
        }

        // Reveal hint (upstream's shouldScrollSelectedWorkspace): a
        // selection change scrolls the active row minimally into view;
        // consumed each frame regardless.
        if let Some(reveal) = self.sidebar_scroll_reveal.take() {
            if reveal < count {
                self.sidebar_scroll = cmux_shell::sidebar::scroll_clamp(
                    cmux_shell::sidebar::scroll_to_reveal(
                        self.sidebar_scroll,
                        reveal,
                        row_advance,
                        SIDEBAR_ROW_HEIGHT,
                        viewport,
                    ),
                    content_height,
                    viewport,
                );
            }
        }

        // Drag planner inputs (screen coordinates with the scroll shift)
        // while a drag holds: the hovered boundary slot and the resulting
        // reorder target (None = no-op drop, upstream `suppressesNoOp`),
        // plus edge autoscroll toward the region's ends.
        const AUTOSCROLL_MARGIN: f32 = 24.0;
        const AUTOSCROLL_SPEED: f32 = 14.0;
        let mut drop_target: Option<usize> = None;
        if let Some(drag) = self.sidebar_drag.as_ref() {
            if let Some(pos) = pointer_pos {
                let drop_slot = cmux_shell::sidebar::insertion_slot(
                    pos.y,
                    top - self.sidebar_scroll,
                    row_advance,
                    count,
                );
                drop_target = cmux_shell::sidebar::reorder_target(drag.from, drop_slot, count);
                let added = cmux_shell::sidebar::drag_autoscroll(
                    pos.y,
                    top,
                    bottom_limit,
                    AUTOSCROLL_MARGIN,
                    AUTOSCROLL_SPEED,
                );
                if added != 0.0 {
                    self.sidebar_scroll = cmux_shell::sidebar::scroll_clamp(
                        self.sidebar_scroll + added,
                        content_height,
                        viewport,
                    );
                }
            }
        }
        let scroll = self.sidebar_scroll;

        // Overflow telemetry: log when the count first overflows (the
        // pre-scroll hidden-row log's successor; scroll resets it when
        // everything fits again).
        if scroll > 0.5 || count as f32 * row_advance - 2.0 > viewport {
            if self
                .sidebar_overflow_reported
                .is_none_or(|seen| seen < count)
            {
                eprintln!(
                    "cmux-shell: {count} workspace(s) overflow the sidebar; scrolling active"
                );
                self.sidebar_overflow_reported = Some(count);
            }
        } else {
            self.sidebar_overflow_reported = None;
        }

        // All scrolled content paints and interacts inside this clip:
        // rows scrolled past an edge paint nothing and — crucially —
        // allocate nothing there (a clipped row must not steal the plus
        // row's or the region's own clicks when it slides under them).
        let scroll_painter = painter.with_clip_rect(content_clip);
        let (first_row, last_row) = cmux_shell::sidebar::visible_row_range(
            scroll,
            count,
            row_advance,
            SIDEBAR_ROW_HEIGHT,
            viewport,
        );

        for index in first_row..last_row {
            let content_top = top + index as f32 * row_advance - scroll;
            let row_rect = egui::Rect::from_min_max(
                egui::pos2(rect.left() + 6.0, content_top),
                egui::pos2(rect.right() - 6.0, content_top + SIDEBAR_ROW_HEIGHT),
            );
            let interact = row_rect.intersect(content_clip);
            if interact.height() <= 1.0 {
                continue;
            }
            let name = self.workspaces.name_of(index);
            let active = index == self.workspaces.active;
            let close_rect = egui::Rect::from_min_size(
                egui::pos2(row_rect.right() - 26.0, row_rect.center().y - 11.0),
                egui::Vec2::splat(22.0),
            );
            let label_rect = egui::Rect::from_min_max(
                row_rect.min,
                egui::pos2(close_rect.left() - 2.0, row_rect.bottom()),
            );
            let close_rect = close_rect.intersect(content_clip);
            let label_rect = label_rect.intersect(content_clip);
            let dragging_this = self
                .sidebar_drag
                .as_ref()
                .is_some_and(|drag| drag.id == self.workspaces.items[index].id);
            let mut rename_here = match self.sidebar_rename.as_ref() {
                Some(rename) => self
                    .workspaces
                    .items
                    .get(index)
                    .is_some_and(|workspace| workspace.id == rename.id),
                None => false,
            };
            if rename_here && !content_clip.contains_rect(row_rect) {
                // The editor's row slid out of the scroll window (wheel
                // while editing): end the session without writing — the
                // port does not scroll-follow its editor.
                self.sidebar_rename = None;
                rename_here = false;
            }

            if rename_here {
                // Inline rename (owner round 9; upstream's
                // SidebarInlineRenameField): the row becomes a text
                // editor. Dragging is off strip-wide while the editor
                // stands (upstream's SidebarRowDragGate; rows and close
                // buttons stay clickable); the chip fill stays behind it.
                scroll_painter.rect_filled(
                    row_rect,
                    4.0,
                    if active {
                        token_color(self.runtime.theme.chip_background)
                    } else {
                        hairline_color(TOKEN_HAIRLINE_SOFT)
                    },
                );
                let Some(session) = self.sidebar_rename.as_mut() else {
                    continue;
                };
                let editor = egui::TextEdit::singleline(&mut session.buffer)
                    .id(egui::Id::new(("sidebar-rename", session.id.clone())))
                    .desired_width(label_rect.width() + close_rect.width() + 2.0)
                    .font(egui::FontId::proportional(13.0))
                    .frame(egui::Frame::NONE)
                    .text_color(if active {
                        token_color(TOKEN_INFO)
                    } else {
                        token_color(TOKEN_TEXT_PRIMARY)
                    });
                let response = ui.put(row_rect.intersect(content_clip), editor);
                if !session.requested_focus {
                    response.request_focus();
                    session.requested_focus = true;
                }
                // Keys: Enter and focus loss commit the rename policy's
                // verdict (upstream SidebarInlineRenameCommit);
                // Escape cancels outright. Divergence: upstream's first
                // Escape parks the caret and only a second one cancels -
                // an AppKit caret affordance the port's editor does
                // without (one press).
                let (enter, escape) = ui.input(|input| {
                    (
                        input.key_pressed(egui::Key::Enter),
                        input.key_pressed(egui::Key::Escape),
                    )
                });
                if escape {
                    self.sidebar_rename = None;
                } else if enter || response.lost_focus() {
                    let committed = cmux_shell::sidebar::rename_commit(
                        &session.buffer,
                        session.baseline.as_deref(),
                        session.custom_open.as_deref(),
                    );
                    if let Some(title) = committed {
                        // A rename never shifts row indices, so this
                        // direct mid-paint model write cannot invalidate
                        // the loop (the Action drain exists for the
                        // mutations that can).
                        self.workspaces.set_custom_title(session.index, Some(title));
                    }
                    self.sidebar_rename = None;
                }
                continue;
            }

            // Drag gate: any open rename session disables drag across
            // the whole strip (the pointer target stays a plain click).
            let row = ui.allocate_rect(
                label_rect,
                if self.sidebar_rename.is_some() {
                    egui::Sense::click()
                } else {
                    egui::Sense::click_and_drag()
                },
            );
            let close = ui.allocate_rect(close_rect, egui::Sense::click());
            // M6 accessibility: the workspace rows and their close buttons
            // are custom-painted regions; these labels are what a screen
            // reader sees (upstream labels tree rows by name too).
            row.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Button, true, name.clone())
            });
            close.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::Button,
                    true,
                    format!("Close workspace {name}"),
                )
            });
            if self.sidebar_drag.is_none() && row.drag_started() {
                self.sidebar_drag = Some(SidebarDrag {
                    from: index,
                    id: self.workspaces.items[index].id.clone(),
                });
            }
            let unread = self.workspace_alerts.get(index).copied().unwrap_or(0);
            let hovered = row.hovered() || close.hovered();
            // Design: the focused workspace row wears the solid chip fill,
            // a hovered row the same hue at its soft overlay (the design
            // states no hover variant; inferred from the file's secondary
            // chip). Corners are the 4px the file rounds row chips to.
            let fill = if active {
                token_color(self.runtime.theme.chip_background)
            } else if hovered {
                hairline_color(TOKEN_HAIRLINE_SOFT)
            } else {
                egui::Color32::TRANSPARENT
            };
            scroll_painter.rect_filled(row_rect, 4.0, fill);
            let text_painter = scroll_painter.clone().with_clip_rect(label_rect);
            let text_ink = if active {
                // Design: the focused row's name is the info-blue.
                token_color(TOKEN_INFO)
            } else {
                token_color(TOKEN_TEXT_SECONDARY)
            };
            // A dragged row keeps its place with dimmed ink (no floating
            // drag image: the insertion line carries the drop point -
            // recorded adaptation of the upstream drag session visual).
            let text_ink = if dragging_this {
                text_ink.gamma_multiply(0.55)
            } else {
                text_ink
            };
            let text_left = if unread > 0 {
                // Unread ring (M3.1): a dot beside the name, in the design
                // warning amber.
                scroll_painter.circle_filled(
                    egui::pos2(label_rect.left() + 4.5, row_rect.center().y),
                    3.5,
                    token_color(TOKEN_WARNING),
                );
                // The name shifts right of the dot.
                label_rect.left() + 13.0
            } else {
                label_rect.left() + 8.0
            };
            text_painter.text(
                egui::pos2(text_left, row_rect.center().y),
                egui::Align2::LEFT_CENTER,
                &name,
                egui::FontId::proportional(13.0),
                text_ink,
            );
            // The row's close affordance: a hand-drawn x (two strokes,
            // the close-icon shape every close button uses). The
            // previous text glyph painted a double-encoded A-tilde +
            // em-dash pair because its source literal carried the
            // mojibake bytes (owner: "weird symbol"; design round 2,
            // 2026-10-26) - strokes cannot drift with fonts or file
            // encodings.
            let arm = 4.5;
            let center = close_rect.center();
            let ink = if hovered {
                token_color(TOKEN_TEXT_PRIMARY)
            } else {
                token_color(TOKEN_TEXT_MUTED)
            };
            let stroke = egui::Stroke::new(1.0, ink);
            scroll_painter.line_segment(
                [
                    egui::pos2(center.x - arm, center.y - arm),
                    egui::pos2(center.x + arm, center.y + arm),
                ],
                stroke,
            );
            scroll_painter.line_segment(
                [
                    egui::pos2(center.x - arm, center.y + arm),
                    egui::pos2(center.x + arm, center.y - arm),
                ],
                stroke,
            );
            // A drag session's release frame must never select: the
            // session exists only after egui decided the press a drag
            // (>6pt move, >0.8s hold, or dragging out of the row), and a
            // decided drag cancels the click itself — this guard covers
            // both decisions agreeing in one frame.
            if row.clicked() && !dragging_this {
                actions.push(Action::SelectWorkspace(index));
            }
            if row.double_clicked() {
                // Upstream's double-click inline rename
                // (`beginInlineRename`, ContentView.rs:16936): the buffer
                // starts at the row's full display name; the commit
                // policy receives the auto-derived name as its baseline
                // and the pre-edit custom title separately.
                let display = self.workspaces.name_of(index);
                self.sidebar_rename = Some(SidebarRename {
                    index,
                    id: self.workspaces.items[index].id.clone(),
                    buffer: display.clone(),
                    baseline: Some(display),
                    custom_open: self.workspaces.items[index].custom_title.clone(),
                    requested_focus: false,
                });
            }
            if close.clicked() {
                actions.push(Action::CloseWorkspace { at: index });
            }
        }

        // Drag end: the session lives frame to frame while the button is
        // down (it opens on the source row's first decisive-drag frame)
        // and closes on the release wherever that lands — egui keeps the
        // button state across the window during a drag. The planner's
        // verdict (`None` = no-op, upstream `suppressesNoOp`) gates the
        // commit, which rides the Action drain because a reorder shifts
        // every later row index.
        if self.sidebar_drag.is_some() && !pointer_pressing {
            if let (Some(drag), Some(to)) = (self.sidebar_drag.take(), drop_target) {
                actions.push(Action::ReorderWorkspace {
                    from: drag.from,
                    to,
                });
            }
        }

        // The 2pt accent insertion line while a drag holds (upstream's
        // SidebarReorderIndicatorView: thickness 2, horizontal inset 8).
        // The slot recomputes from the live pointer: the pre-loop planner
        // runs one frame behind the session on its opening frame, and a
        // drag whose pointer left the window paints nothing.
        if self.sidebar_drag.is_some() {
            if let Some(pos) = pointer_pos {
                let drop_slot =
                    cmux_shell::sidebar::insertion_slot(pos.y, top - scroll, row_advance, count);
                let line_y = cmux_shell::sidebar::indicator_top(
                    drop_slot,
                    top - scroll,
                    row_advance,
                    count,
                    SIDEBAR_ROW_HEIGHT,
                );
                scroll_painter.line_segment(
                    [
                        egui::pos2(rect.left() + 8.0, line_y + 1.0),
                        egui::pos2(rect.right() - 8.0, line_y + 1.0),
                    ],
                    egui::Stroke::new(2.0, token_color(TOKEN_ACCENT)),
                );
            }
        }

        // Design lane 4 / feature-parity row 12 (Figma nodes 1:456..1:484):
        // the RUNTIME SERVICES segment hangs directly under the workspace
        // rows — header +12 (the Margin 1:456 carries the gap), 26px rows
        // on the pane-interior fill with 2px gaps, ascending by port.
        // Segment width follows the sidebar's established ±6 row band (the
        // design gives every aside list the same 239px band). The tracked
        // 0.55px header/badge spacing stays the recorded egui-galley
        // residual; STOP latches label the row for as long as its pane
        // lives (sampler semantics in `port_state`).
        let header_rect = egui::Rect::from_min_size(
            egui::pos2(rect.left(), top + rows_span + 12.0 - scroll),
            egui::vec2(rect.width(), 14.0),
        );
        let segment_summary = if services.is_empty() {
            "Runtime services: none".to_owned()
        } else {
            format!(
                "Runtime services: {}",
                services
                    .iter()
                    .map(|row| {
                        format!(
                            ":{} {} {}",
                            row.port,
                            row.label,
                            if row.listening { "UP" } else { "STOP" }
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        // M6 accessibility: painted-only segment, so the a11y label is
        // attached by hand (same rule the retired sub-header used). The
        // region shrinks to the clipped band so a scrolled-out header
        // holds no part of the strip's response area.
        let header_visible = header_rect.intersect(content_clip);
        if header_visible.height() >= 4.0 {
            ui.allocate_rect(header_visible, egui::Sense::hover())
                .widget_info(|| {
                    egui::WidgetInfo::labeled(
                        egui::WidgetType::Label,
                        true,
                        segment_summary.clone(),
                    )
                });
            scroll_painter.text(
                egui::pos2(rect.left() + 12.0, header_visible.center().y),
                egui::Align2::LEFT_CENTER,
                "RUNTIME SERVICES",
                egui::FontId::new(11.0, cmux_shell::fonts::bold_family_id()),
                token_color(TOKEN_TEXT_MUTED),
            );
        }
        let services_top_content = rows_span + 12.0 + 14.0 + 4.0;
        for (index, row) in services.iter().enumerate() {
            let row_top = top + services_top_content + index as f32 * 28.0 - scroll;
            let row_rect = egui::Rect::from_min_max(
                egui::pos2(rect.left() + 6.0, row_top),
                egui::pos2(rect.right() - 6.0, row_top + 26.0),
            );
            scroll_painter.rect_filled(
                row_rect,
                4.0,
                token_color(self.runtime.theme.viewport_background),
            );
            let port_galley = scroll_painter.layout_no_wrap(
                cmux_shell::port_state::format_port(row.port),
                egui::FontId::proportional(12.0),
                if row.listening {
                    token_color(TOKEN_WARM_INK)
                } else {
                    token_color(TOKEN_TEXT_MUTED)
                },
            );
            let name_galley = scroll_painter.layout_no_wrap(
                row.label.clone(),
                egui::FontId::proportional(12.0),
                token_color(TOKEN_TEXT_MUTED),
            );
            let badge_galley = scroll_painter.layout_no_wrap(
                if row.listening { "UP" } else { "STOP" }.to_owned(),
                egui::FontId::new(11.0, cmux_shell::fonts::bold_family_id()),
                if row.listening {
                    token_color(TOKEN_INFO)
                } else {
                    token_color(TOKEN_TEXT_MUTED)
                },
            );
            let mid = row_rect.center().y;
            scroll_painter.galley(
                egui::pos2(rect.left() + 10.0, mid - port_galley.size().y * 0.5),
                port_galley.clone(),
                egui::Color32::WHITE,
            );
            scroll_painter.galley(
                egui::pos2(
                    rect.left() + 10.0 + port_galley.size().x + 4.0,
                    mid - name_galley.size().y * 0.5,
                ),
                name_galley.clone(),
                egui::Color32::WHITE,
            );
            scroll_painter.galley(
                egui::pos2(
                    row_rect.right() - 4.0 - badge_galley.size().x,
                    mid - badge_galley.size().y * 0.5,
                ),
                badge_galley.clone(),
                egui::Color32::WHITE,
            );
        }

        // Edge fades (upstream's SidebarWorkspaceScrollEdgeFadeMask +
        // SidebarEdgeFadeGradient): the top scrim when the content is
        // scrolled down, the bottom one while more content waits below.
        let sidebar_fill = egui::Color32::from_rgb(
            ((self.runtime.theme.sidebar_background >> 16) & 0xFF) as u8,
            ((self.runtime.theme.sidebar_background >> 8) & 0xFF) as u8,
            (self.runtime.theme.sidebar_background & 0xFF) as u8,
        );
        const FADE_HEIGHT: f32 = 16.0;
        if scroll > 0.5 {
            sidebar_fade_strip(
                &scroll_painter,
                egui::Rect::from_min_max(
                    egui::pos2(rect.left(), top),
                    egui::pos2(rect.right(), top + FADE_HEIGHT.min(viewport)),
                ),
                sidebar_fill,
                true,
            );
        }
        if scroll < (content_height - viewport).max(0.0) - 0.5 {
            let band_top = (bottom_limit - FADE_HEIGHT).max(top);
            sidebar_fade_strip(
                &scroll_painter,
                egui::Rect::from_min_max(
                    egui::pos2(rect.left(), band_top),
                    egui::pos2(rect.right(), bottom_limit),
                ),
                sidebar_fill,
                false,
            );
        }

        // Owner round 3 (2026-10-26): the aside's bottom version strip
        // (design node 1:485 "Background+HorizontalBorder"; chip fill,
        // soft top hairline, and the version chip `#0c0e14` with the
        // info-blue dot and `v` + CARGO_PKG_VERSION label) is no longer
        // drawn — the owner asked to remove the element that tells the
        // version. The design's settings/help icon links on that strip
        // were already a recorded residual (never implemented). With
        // the strip gone, its 38pt budget released to the rows: the
        // workspace and service row bounds now margin 6pt above the
        // aside bottom (matching the rows' own 6pt side inset).
        actions
    }
}

/// One sidebar edge-fade strip (owner round 9): the upstream
/// SidebarScrim mask's 4 stops (0.05/0.25/0.65/1.0) rendered as a solid
/// sidebar-fill alpha ramp over an egui Mesh (recorded adaptation — egui
/// has no mask shader; the fade blends the scrolled rows into the
/// sidebar's own fill instead of a black scrim over them).
fn sidebar_fade_strip(
    painter: &egui::Painter,
    band: egui::Rect,
    fill: egui::Color32,
    solid_at_top: bool,
) {
    const STOPS: [f32; 4] = [0.05, 0.25, 0.65, 1.0];
    let height = band.height();
    if height <= 0.0 {
        return;
    }
    // One vertex pair per stop (plus the solid edge itself), triangles
    // chaining the pairs into a horizontally constant gradient. Alpha
    // ramps linearly from the solid edge (opaque sidebar fill) to the far
    // edge (transparent) in both orientations: for the bottom fade the
    // fractions measure upward from the bottom edge.
    let mut mesh = egui::Mesh::default();
    for fraction in [0.0, STOPS[0], STOPS[1], STOPS[2], STOPS[3]] {
        let y = if solid_at_top {
            band.top() + fraction * height
        } else {
            band.bottom() - fraction * height
        };
        let color = egui::Color32::from_rgba_unmultiplied(
            fill.r(),
            fill.g(),
            fill.b(),
            ((1.0 - fraction) * 255.0).round() as u8,
        );
        let row_left = mesh.vertices.len() as u32;
        mesh.colored_vertex(egui::pos2(band.left(), y), color);
        mesh.colored_vertex(egui::pos2(band.right(), y), color);
        if row_left >= 2 {
            // The first pair has no previous band edge to connect to.
            mesh.add_triangle(row_left - 2, row_left - 1, row_left + 1);
            mesh.add_triangle(row_left - 2, row_left + 1, row_left);
        }
    }
    painter.add(egui::Shape::mesh(mesh));
}

// â€”â€”â€”â€”â€”â€”â€”â€”â€”â€”â€”â€”â€”â€”â€”â€”â€”â€”â€”â€”â€” session persistence â€”â€”â€”â€”â€”â€”â€”â€”â€”â€”â€”â€”â€”â€”â€”â€”â€”â€”â€”â€”â€”

/// Session store location: `CMUX_SESSION_FILE` overrides, else
/// `%APPDATA%\cmux\session.json` (the Application Support equivalent; the
/// user config stays at `~/.config/cmux` per ADR-0008 â€” the two stores
/// follow the same user-config vs. app-state split upstream uses).
fn session_paths() -> Option<SessionPaths> {
    if let Some(value) = std::env::var_os("CMUX_SESSION_FILE") {
        return Some(SessionPaths::overriding(Path::new(&value)));
    }
    match cmux_workspaces::user_session_paths() {
        Ok(paths) => Some(paths),
        Err(reason) => {
            eprintln!("cmux-shell: no session store ({reason}); session restore/save is off");
            None
        }
    }
}

/// Builds the workspace snapshot for a live tree. `tabs_of` supplies each
/// leaf pane's tab membership; panes unknown to it save as their own
/// single tab, and every leaf contributes a `terminal` panel entry.
/// `workspace_id` is the live id (minted `w{n}` or the restored numeric
/// id — opaque to the wire format, which upstream fills with UUIDs).
fn workspace_from_outline(
    outline: &TreeOutline,
    tabs_of: &dyn Fn(&str) -> Vec<String>,
    url_of: &dyn Fn(&str) -> Option<String>,
    kind_of: &dyn Fn(&str) -> &'static str,
    focused: &str,
    workspace_id: &str,
    custom_title: Option<&str>,
) -> WorkspaceSnapshot {
    let mut panels: Vec<PanelSnapshot> = Vec::new();
    let layout = layout_from_outline(outline, tabs_of, url_of, kind_of, &mut panels);
    WorkspaceSnapshot {
        workspace_id: workspace_id.to_owned(),
        process_title: "cmux".to_owned(),
        // Owner round 9: the user's rename persists in upstream's own
        // `customTitle` key (the DTO field predates this; nothing wrote
        // it before the sidebar's inline editor).
        custom_title: custom_title
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .map(str::to_owned),
        custom_color: None,
        current_directory: String::new(),
        focused_panel_id: Some(focused.to_owned()),
        layout,
        panels,
    }
}

/// The session's custom title for one restored workspace: only a
/// non-blank value becomes the model field (mirror of the save-side
/// trim — a blank string either way never renames).
fn restored_custom_title(snapshot: &WorkspaceSnapshot) -> Option<String> {
    snapshot
        .custom_title
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(str::to_owned)
}

/// Serializes the tree shape into the upstream wire enum (leaf: ordered tab
/// list + selection; split: orientation + divider fraction, children in
/// spatial order).
fn layout_from_outline(
    outline: &TreeOutline,
    tabs_of: &dyn Fn(&str) -> Vec<String>,
    url_of: &dyn Fn(&str) -> Option<String>,
    kind_of: &dyn Fn(&str) -> &'static str,
    panels: &mut Vec<PanelSnapshot>,
) -> WorkspaceLayoutSnapshot {
    match outline {
        TreeOutline::Leaf { id } => {
            let mut tabs = tabs_of(id);
            if tabs.is_empty() {
                tabs.push(id.clone());
            }
            let browser_url = url_of(id);
            let is_browser = browser_url.is_some();
            if !panels.iter().any(|panel| panel.id == *id) {
                panels.push(PanelSnapshot {
                    id: id.clone(),
                    // M5.1: a browser leaf records its panel type so the
                    // snapshot's panels stay truthful about what they back;
                    // M6 lane 5 extends that to the agent activity pane.
                    panel_type: if is_browser { "browser" } else { kind_of(id) }.to_owned(),
                    title: None,
                    custom_title: None,
                    directory: None,
                });
            }
            WorkspaceLayoutSnapshot::Pane(PaneLayoutSnapshot {
                panel_ids: tabs,
                selected_panel_id: Some(id.clone()),
                is_full_width_tab_mode: None,
                browser_url,
            })
        }
        TreeOutline::Split {
            orientation,
            divider,
            first,
            second,
        } => {
            let first = layout_from_outline(first, tabs_of, url_of, kind_of, panels);
            let second = layout_from_outline(second, tabs_of, url_of, kind_of, panels);
            let orientation = match orientation {
                Orientation::Horizontal => "horizontal",
                Orientation::Vertical => "vertical",
            };
            WorkspaceLayoutSnapshot::Split(SplitLayoutSnapshot {
                orientation: orientation.to_owned(),
                divider_position: *divider,
                first: Box::new(first),
                second: Box::new(second),
            })
        }
    }
}

/// Rebuilds the pane tree from a repaired snapshot layout. Leaves spawn
/// real shells (the chosen tab is the rendered pane; its full tab list is
/// kept on the pane and rendered from M2.4b). An unknown orientation
/// fails the whole restore â€” the same observable behavior as upstream's
/// decode failure (fresh start).
fn build_tree_from_layout(
    layout: &WorkspaceLayoutSnapshot,
    panels: &[PanelSnapshot],
    spec: &PtySpec,
    workspace_index: usize,
    ipc_pipe: &str,
    split_counter: &mut u64,
    wake: &Arc<FrameWake>,
) -> Result<PaneTree<Pane>, String> {
    match layout {
        WorkspaceLayoutSnapshot::Pane(leaf) => {
            let selected = leaf
                .selected_panel_id
                .clone()
                .filter(|id| leaf.panel_ids.contains(id))
                .or_else(|| leaf.panel_ids.first().cloned())
                .ok_or_else(|| "empty pane leaf".to_owned())?;
            if leaf.panel_ids.len() > 1 {
                eprintln!(
                    "cmux-shell: pane {selected} keeps {} restored tab(s); tab strips render from M2.4b",
                    leaf.panel_ids.len()
                );
            }
            // M5.1: a leaf with a saved browser URL comes back as a browser
            // pane (no shell spawn; the WebView2 host joins on the first
            // frame). No runtime probe here — a missing runtime surfaces as
            // the pane's failure overlay with a log line.
            // Lane 5: an agent activity pane restores from its declared
            // panel type (no process to spawn; the stream stays empty,
            // since cards are not persisted).
            let is_agent = panels
                .iter()
                .any(|panel| panel.id == selected && panel.panel_type == "agent");
            let mut pane = if is_agent {
                new_agent_pane(selected.clone(), wake)
                    .map_err(|error| format!("could not restore pane {selected}: {error:#}"))?
            } else if leaf.browser_url.is_some() {
                new_browser_pane(
                    selected.clone(),
                    leaf.browser_url.clone().unwrap_or_default(),
                    wake,
                )
                .map_err(|error| format!("could not restore pane {selected}: {error:#}"))?
            } else {
                let pane_spec = identity_spec(spec.clone(), &selected, workspace_index, ipc_pipe);
                new_pane_with_focus(selected.clone(), pane_spec, false, wake)
                    .map_err(|error| format!("could not spawn pane {selected}: {error:#}"))?
            };
            pane.panel_ids = leaf.panel_ids.clone();
            Ok(PaneTree::new(pane))
        }
        WorkspaceLayoutSnapshot::Split(split) => {
            let orientation = match split.orientation.as_str() {
                "horizontal" => Orientation::Horizontal,
                "vertical" => Orientation::Vertical,
                other => return Err(format!("unsupported split orientation {other:?}")),
            };
            let first = build_tree_from_layout(
                &split.first,
                panels,
                spec,
                workspace_index,
                ipc_pipe,
                split_counter,
                wake,
            )?;
            let second = build_tree_from_layout(
                &split.second,
                panels,
                spec,
                workspace_index,
                ipc_pipe,
                split_counter,
                wake,
            )?;
            *split_counter += 1;
            Ok(PaneTree::joined(
                first,
                second,
                orientation,
                split.divider_position,
                format!("s-restore-{split_counter}"),
            ))
        }
    }
}

/// The restored tree's id-counter seed: one past the highest numeric
/// `p{N}` id among all restored tabs â€” extra tabs count too, so a future
/// `next_leaf_id()` can never collide with them.
fn restored_seed(layout: &WorkspaceLayoutSnapshot) -> u64 {
    let mut highest: u64 = 0;
    for panel_id in layout.leaf_panel_ids() {
        if let Some(number) = panel_id
            .strip_prefix('p')
            .and_then(|digits| digits.parse::<u64>().ok())
        {
            highest = highest.max(number);
        }
    }
    highest + 1
}

/// Restores every workspace from the session store (unusable or missing
/// snapshots log and start fresh â€” a broken session must not block the
/// window, matching the config policy). A workspace that cannot be built
/// is skipped with a log rather than discarding the whole session. The
/// one-shot smoke never restores.
fn try_restore(spec: &PtySpec, ipc_pipe: &str, wake: &Arc<FrameWake>) -> Option<Workspaces> {
    if std::env::var("CMUX_SHELL_SPAWN_ONESHOT").as_deref() == Ok("1") {
        return None;
    }
    let paths = session_paths()?;
    let snapshot = match cmux_workspaces::load(&paths) {
        LoadOutcome::Loaded(snapshot) => snapshot,
        LoadOutcome::Missing => return None,
        LoadOutcome::Unusable(reason) => {
            eprintln!("cmux-shell: session snapshot unusable ({reason}); starting fresh");
            return None;
        }
    };
    let Some(window) = snapshot.windows.first() else {
        eprintln!("cmux-shell: session snapshot carries no windows; starting fresh");
        return None;
    };
    let tab_manager = &window.tab_manager;
    if tab_manager.workspaces.is_empty() {
        eprintln!("cmux-shell: session snapshot carries no workspaces; starting fresh");
        return None;
    }

    let mut items: Vec<Workspace> = Vec::new();
    let mut pane_total: usize = 0;
    // M4.1 identity: the global id sources lift above every restored id
    // (per-workspace maxima folded here), so later mints never collide.
    let mut pane_seed: u64 = 0;
    let mut workspace_seed: u64 = 0;
    for (index, workspace_snapshot) in tab_manager.workspaces.iter().enumerate() {
        let (repaired, issues) = match workspace_snapshot.repaired() {
            Ok((repaired, issues)) => (repaired, issues),
            Err(reason) => {
                eprintln!(
                    "cmux-shell: workspace {} of the snapshot is not restorable ({reason}); skipping it",
                    index + 1
                );
                continue;
            }
        };
        for issue in &issues {
            eprintln!("cmux-shell: session repair: {issue}");
        }
        let mut split_counter: u64 = 0;
        let mut tree = match build_tree_from_layout(
            &repaired.layout,
            &repaired.panels,
            spec,
            index,
            ipc_pipe,
            &mut split_counter,
            wake,
        ) {
            Ok(tree) => tree,
            Err(reason) => {
                eprintln!(
                    "cmux-shell: workspace {} in the session snapshot is not restorable ({reason}); skipping it",
                    index + 1
                );
                continue;
            }
        };
        tree.seed_next_id(restored_seed(&repaired.layout));
        // The pane counter lifts past this workspace's numeric pane ids
        // (`restored_seed` already returns one past the highest).
        pane_seed = pane_seed.max(restored_seed(&repaired.layout));
        let ids = tree.ordered_leaf_ids();
        let focused = repaired
            .focused_panel_id
            .clone()
            .filter(|id| ids.contains(id))
            .or_else(|| ids.first().cloned());
        let Some(focused) = focused else {
            eprintln!(
                "cmux-shell: workspace {} in the session snapshot carries no panes; skipping it",
                index + 1
            );
            continue;
        };
        pane_total += ids.len();
        if let Some(pane) = tree.leaf_mut(&focused) {
            if let Some(terminal) = pane.terminal_mut() {
                terminal.request_focus();
            }
        }
        // The snapshot's workspace id carries across saves when it is
        // shape-compatible; non-numeric (e.g. upstream UUID) ids don't
        // seed the counter but are still used verbatim. A restored
        // numeric `w{N}` lifts the workspace counter.
        let snapshot_id = workspace_snapshot.workspace_id.trim().to_owned();
        if let Some(rest) = snapshot_id.strip_prefix('w') {
            if let Ok(number) = rest.parse::<u64>() {
                workspace_seed = workspace_seed.max(number + 1);
            }
        }
        let workspace_id = if snapshot_id.is_empty() {
            mint_workspace_id()
        } else {
            snapshot_id
        };
        items.push(Workspace {
            id: workspace_id,
            tree,
            focused,
            custom_title: restored_custom_title(workspace_snapshot),
        });
    }

    if items.is_empty() {
        eprintln!("cmux-shell: no restorable workspaces in the snapshot; starting fresh");
        return None;
    }

    // M4.1: lift the global id sources past every restored id so later
    // minted pane/workspace ids can never collide with restored ones.
    // (`0` seeds mean "nothing numeric was restored" — a no-op floor.)
    seed_pane_ids(pane_seed.max(1));
    seed_workspace_ids(workspace_seed.max(1));

    // The saved active index, clamped to what survived the skips above.
    let active = (tab_manager.selected_workspace_index.unwrap_or(0) as usize).min(items.len() - 1);
    eprintln!(
        "cmux-shell: restored {} workspace(s) with {} pane(s) total from {}",
        items.len(),
        pane_total,
        paths.primary.display()
    );
    Some(Workspaces { items, active })
}

/// Reads the config file: `CMUX_CONFIG_FILE` overrides, else
/// `<home>/.config/cmux/cmux.json` (mirroring upstream's location). A
/// missing file gets a first-run template write; any read or decode error
/// logs and falls back to defaults â€” a broken config must not block the
/// window.
fn load_config() -> (Config, Option<PathBuf>) {
    let path: Option<PathBuf> = std::env::var_os("CMUX_CONFIG_FILE")
        .map(PathBuf::from)
        .or_else(cmux_config::user_config_path);
    let Some(path) = path else {
        eprintln!("cmux-shell: no home directory resolved; using built-in defaults");
        return (Config::default(), None);
    };
    if !path.exists() {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::write(&path, cmux_config::DEFAULT_TEMPLATE) {
            Ok(()) => eprintln!(
                "cmux-shell: wrote default config template to {}",
                path.display()
            ),
            Err(error) => eprintln!("cmux-shell: could not write config template: {error}"),
        }
    }
    match Config::load_file(&path) {
        Ok(config) => (config, Some(path)),
        Err(error) => {
            eprintln!("cmux-shell: {error}; using built-in defaults");
            (Config::default(), Some(path))
        }
    }
}

/// The call-time config read behind the custom-command verbs. The path
/// resolves exactly like [`load_config`], but the read is side-effect
/// free (the startup path owns the first-run template write) and a
/// missing or broken file simply means zero commands - the verbs must
/// not fabricate entries or block the app plane on a bad file.
fn read_commands_config() -> Config {
    let path = std::env::var_os("CMUX_CONFIG_FILE")
        .map(PathBuf::from)
        .or_else(cmux_config::user_config_path);
    let Some(path) = path else {
        return Config::default();
    };
    Config::load_file(&path).unwrap_or_default()
}

/// The `command.list` reply rows, in config order: the stored name and
/// description, the `confirm` flag, the resolved flavor, and for
/// terminal commands the sanitized text (what would actually reach the
/// shell). Workspace flavors carry no `command` text up.
fn command_list_json(commands: &[cmux_config::CommandDefinition]) -> serde_json::Value {
    serde_json::json!(commands
        .iter()
        .map(|entry| {
            let command_text = entry
                .command
                .as_deref()
                .map(cmux_config::sanitize_command_text);
            serde_json::json!({
                "name": entry.name,
                "description": entry.description,
                "confirm": entry.confirm,
                "kind": if entry.is_workspace_flavor() {
                    "workspace"
                } else {
                    "terminal"
                },
                "command": command_text,
            })
        })
        .collect::<Vec<_>>())
}

/// The run-shape checks shared by the handler and its tests: the named
/// entry must exist, must be a terminal command (workspace launches are
/// a recorded slice-1 residual), and a `confirm: true` entry must have
/// been acknowledged - upstream shows a dialog for that; the port's
/// runs refuse without the caller's `ack` instead.
fn resolve_config_command<'a>(
    commands: &'a [cmux_config::CommandDefinition],
    name: &str,
    ack: bool,
) -> Result<&'a cmux_config::CommandDefinition, cmux_ipc::RequestProblem> {
    let definition = commands
        .iter()
        .find(|entry| entry.name == name)
        .ok_or_else(|| {
            cmux_ipc::RequestProblem::not_found(format!(
                "command '{name}' is not defined in the config file"
            ))
        })?;
    if definition.is_workspace_flavor() {
        return Err(cmux_ipc::RequestProblem::invalid_state(format!(
            "command '{name}' launches a workspace, which the port does not support yet (recorded residual)"
        )));
    }
    if definition.confirm && !ack {
        return Err(cmux_ipc::RequestProblem::invalid_state(format!(
            "command '{name}' requires confirmation; call again with --yes to acknowledge it"
        )));
    }
    Ok(definition)
}

fn main() -> anyhow::Result<()> {
    let (config, config_path) = load_config();
    let runtime = RuntimeSettings::from_config(&config);
    let theme_background = format!("#{:06x}", runtime.theme.background);
    eprintln!(
        "cmux-shell: config {} â€” font {}%, gutter {:.0}px, resize step {}px, theme {} {}, \
         {} keybinding(s)",
        config_path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "<none>".to_owned()),
        runtime.font_percent,
        runtime.pane_gutter,
        runtime.resize_step,
        match runtime.theme.scheme {
            Some(true) => "light",
            Some(false) => "dark",
            None => "default",
        },
        theme_background,
        runtime.bindings.len(),
    );

    // Smoke arenas: the one-shot must stay isolated (no restore, no save);
    // the restore smoke restores but never writes the store it reads.
    let oneshot = std::env::var("CMUX_SHELL_SPAWN_ONESHOT").as_deref() == Ok("1");
    let restore_smoke = std::env::var("CMUX_SHELL_RESTORE_SMOKE").as_deref() == Ok("1");

    // M3.3 slice A: the control pipe. `CMUX_SOCKET_PATH` overrides the
    // per-user default (smoke/multi-instance affordance) and â€” because a
    // client the app never spawned may then address it â€” turns off the
    // process-ancestry gate. `CMUX_IPC_DISABLED=1` turns IPC off entirely.
    let override_pipe = std::env::var("CMUX_SOCKET_PATH")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    let ipc_pipe = match override_pipe.clone() {
        Some(pipe) => pipe,
        None => {
            match cmux_ipc::default_pipe_name() {
                Ok(pipe) => pipe,
                Err(error) => {
                    eprintln!("cmux-shell: could not resolve the control pipe name ({error}); IPC disabled");
                    String::new()
                }
            }
        }
    };
    let ipc_enabled = !oneshot
        && !restore_smoke
        && !ipc_pipe.is_empty()
        && std::env::var("CMUX_IPC_DISABLED").as_deref() != Ok("1")
        // M4.2: `CMUX_SOCKET_MODE=off` turns the server off, upstream
        // `SocketControlMode.off` parity (the legacy `CMUX_IPC_DISABLED=1`
        // keeps working).
        && std::env::var("CMUX_SOCKET_MODE").as_deref() != Ok("off");

    let (hook_sender, hook_receiver) = mpsc::channel();
    // M4.1 app-plane channel exists regardless: without a server nothing
    // sends into it, and the receiver drains to a hang-up (callers get
    // the structured timeout).
    let (ipc_call_sender, ipc_call_receiver) = mpsc::channel::<IpcCallMessage>();
    // The frame wake exists before the server spawns: the handler wakes
    // the idle frame loop for app-plane calls.
    let frame_wake = FrameWake::new();
    // Owner round 4 (2026-10-26): the `cmux-git-watcher` worker retired
    // with the chips; its body is preserved in
    // figma-design-reference.md sec. 17.

    // Design lane 4 services lane (row 12): the sampler-worker shape —
    // the UI sends every workspace's tree-leader pids, the
    // worker takes the two Windows snapshots (process table + TCP
    // listener table), latches STOP state between samples, and answers
    // with per-workspace display rows. A failed snapshot keeps the
    // previous rows (None payloads never leave `take_sample`).
    let (service_requests, service_request_rx) = mpsc::channel::<Vec<(usize, u32)>>();
    let (service_results_tx, service_results_rx) =
        mpsc::channel::<Vec<(usize, Vec<cmux_shell::port_state::PortRow>)>>();
    {
        let wake = Arc::clone(&frame_wake);
        std::thread::Builder::new()
            .name("cmux-service-watcher".to_owned())
            .spawn(move || {
                let mut history = cmux_shell::port_state::PortHistory::default();
                let mut sample_failed = false;
                while let Ok(roots) = service_request_rx.recv() {
                    match cmux_shell::port_state::take_sample() {
                        Some(sample) => {
                            if sample_failed {
                                eprintln!("cmux-shell: services sampler recovered");
                                sample_failed = false;
                            }
                            let digest = cmux_shell::port_state::collect_rows(&roots, &sample, &history);
                            history = digest.history;
                            let rows: Vec<_> = digest.rows.into_iter().collect();
                            if service_results_tx.send(rows).is_err() {
                                break;
                            }
                            wake.wake_now();
                        }
                        None => {
                            // Degraded, not dead: report once and retry on
                            // the next cadence tick; the UI keeps showing
                            // its last rows (a state the OS did not report
                            // is never invented).
                            if !sample_failed {
                                eprintln!("cmux-shell: services sampler unavailable (retrying each cadence)");
                                sample_failed = true;
                            }
                        }
                    }
                }
            })
            .expect("services worker thread");
    }
    if ipc_enabled {
        // Upstream binds the daemon socket at startup; admission happens
        // inside the app loop, so the handler answers the enqueue inline
        // (`queued: true`, the upstream in-flight ack) and hands
        // app-plane calls to the frame loop, replying within
        // `APP_REPLY_BUDGET`.
        let sender = hook_sender;
        // M4.2 (ADR-0005 Amendment 3): the access mode. `CMUX_SOCKET_MODE`
        // selects among upstream's `cmuxOnly|automation|password` values;
        // unknown values warn and fall back to the day-one default. The
        // `CMUX_SOCKET_PATH` override's behavior became the named
        // `automation` mode: without an explicit selection, an override
        // pipe is served mode-less of the ancestry gate (upstream
        // `.automation` semantics — same-user admission only), every other
        // run keeps `cmuxOnly` (DACL + ancestry).
        let requested_mode = std::env::var("CMUX_SOCKET_MODE")
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        let auth_mode = match requested_mode.as_deref() {
            Some("cmuxOnly") => cmux_ipc::AuthMode::CmuxOnly,
            Some("automation") => cmux_ipc::AuthMode::Automation,
            Some("password") => cmux_ipc::AuthMode::Password,
            Some(unknown) => {
                eprintln!(
                    "cmux-shell: unknown CMUX_SOCKET_MODE {unknown:?} — falling back to the default"
                );
                if override_pipe.is_some() {
                    cmux_ipc::AuthMode::Automation
                } else {
                    cmux_ipc::AuthMode::CmuxOnly
                }
            }
            None if override_pipe.is_some() => cmux_ipc::AuthMode::Automation,
            None => cmux_ipc::AuthMode::CmuxOnly,
        };
        let app_calls = ipc_call_sender;
        let shared_wake = Arc::clone(&frame_wake);
        let handler = Arc::new(move |call: cmux_ipc::IpcCall| {
            match call {
                cmux_ipc::IpcCall::Enqueue(enqueue) => {
                    let _ = sender.send(enqueue);
                    Some(Ok(serde_json::json!({ "queued": true })))
                }
                cmux_ipc::IpcCall::App(call) => {
                    // M5.4: waits legitimately outlive the plain reply
                    // budget — the app parks until the wait resolves
                    // (deadline plus upstream's CLI headroom formula), so
                    // the actual outcome (result or the structured
                    // timeout problem) is reported by the app handler,
                    // never the generic envelope. Every other call keeps
                    // the plain budget.
                    let budget = match &call.request {
                        cmux_ipc::AppRequest::BrowserWait { timeout_ms, .. } => {
                            Duration::from_millis(*timeout_ms as u64 + WAIT_REPLY_HEADROOM_MS)
                        }
                        _ => APP_REPLY_BUDGET,
                    };
                    let (reply, awaiting) = mpsc::channel();
                    if app_calls.send(IpcCallMessage { call, reply }).is_err() {
                        // No receiver: the app is gone; the server formats
                        // the timeout envelope.
                        return None;
                    }
                    shared_wake.wake_now();
                    awaiting.recv_timeout(budget).ok()
                }
            }
        });
        let ipc_server_config = cmux_ipc::ServerConfig {
            pipe_name: ipc_pipe.clone(),
            mode: auth_mode,
            // The provider reads the live credential per check: env edits
            // rotate immediately, the DPAPI file is consulted when no env
            // var is set. Secrets never reach any log line.
            password: cmux_ipc::default_password_provider(),
        };
        let _ipc_server = cmux_ipc::spawn_config(ipc_server_config, handler);
        let ancestry_gate = match auth_mode {
            cmux_ipc::AuthMode::CmuxOnly => true,
            cmux_ipc::AuthMode::Automation | cmux_ipc::AuthMode::Password => false,
        };
        eprintln!(
            "cmux-shell: control pipe serving at {ipc_pipe} (mode: {auth_mode:?}, ancestry gate: {ancestry_gate})"
        );
    } else {
        drop(hook_sender);
        drop(ipc_call_sender);
        if !ipc_pipe.is_empty() {
            eprintln!("cmux-shell: IPC disabled for this run (smoke or CMUX_IPC_DISABLED)");
        }
    }

    let workspaces = match try_restore(&shell_spec(&runtime), ipc_pipe.as_str(), &frame_wake) {
        Some(workspaces) => workspaces,
        None => {
            // Fixed ids for the very first pane/workspace of a fresh run:
            // the global mints start at `p1`/`w1` (M3.3b's env contract
            // locks `CMUX_SURFACE_ID=p1` on the first pane).
            let pane_id = mint_pane_id();
            let workspace_id = mint_workspace_id();
            let first = new_pane(
                pane_id.clone(),
                identity_spec(shell_spec(&runtime), pane_id.as_str(), 0, ipc_pipe.as_str()),
                &frame_wake,
            )?;
            Workspaces::new_single(PaneTree::new(first), workspace_id, pane_id)
        }
    };

    let auto_exit_ms = std::env::var("CMUX_SHELL_AUTO_EXIT_MS")
        .ok()
        .and_then(|value| value.parse().ok());
    let auto_split_ms = std::env::var("CMUX_SHELL_AUTO_SPLIT_MS")
        .ok()
        .and_then(|value| value.parse().ok());
    let session_paths = if oneshot || restore_smoke {
        None
    } else {
        session_paths()
    };

    eframe::run_native(
        "cmux port â€” M2 slice",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([900.0, 560.0])
                // Design lane 2: the chrome paints its own 32px title row.
                // winit 0.30 keeps the snap/resize/maximize styles under
                // decorations(false) and removes the visible frame through
                // its WM_NCCALCSIZE override; the caption hit-test is the
                // port's WM_NCHITTEST subclass.
                .with_decorations(false),
            ..Default::default()
        },
        Box::new(|creation| {
            // M4.3 (ADR-0012): Windows system fonts ahead of the CJK tofu
            // problem — the log records which of the machine's faces the
            // shell actually loaded.
            let loaded_fonts = cmux_shell::fonts::install(&creation.egui_ctx);
            if loaded_fonts.is_empty() {
                eprintln!(
                    "cmux-shell: fonts: egui defaults only (no Windows font found in {})",
                    cmux_shell::fonts::system_fonts_dir().display()
                );
            } else {
                eprintln!("cmux-shell: fonts: {}", loaded_fonts.join(", "));
            }
            // Design-fidelity lane 1: egui's visuals carry the Figma
            // tokens (accent cursor, terminal inks, links); the raw token
            // colors below paint the custom chrome.
            install_design_visuals(&creation.egui_ctx, &runtime.theme);
            Ok(Box::new(App {
                workspace_alerts: vec![0; workspaces.items.len()],
                agent_cards: cmux_shell::agent_cards::CardStore::default(),
                is_app_focused: true,
                toast: cmux_toast::ToastDelivery::new(),
                hook_receiver,
                ipc_calls: ipc_call_receiver,
                ipc_pipe,
                workspaces,
                actions: Vec::new(),
                runtime,
                session_paths,
                sidebar_visible: true,
                sidebar_overflow_reported: None,
                sidebar_scroll: 0.0,
                sidebar_scroll_reveal: None,
                sidebar_drag: None,
                sidebar_rename: None,
                status_state: None,
                pending_window_close: false,
                auto_exit_ms,
                auto_split_ms,
                started: Instant::now(),
                split_stage: 0,
                frame_wake: Arc::clone(&frame_wake),
                platform_hwnd: None,
                dpi_scale: 1.0,
                window_pos: None,
                accel_router: cmux_webview2::AccelRouter::new(
                    (1u32..=9).map(|digit| b'0' as u32 + digit).collect(),
                ),
                accel_wake_set: false,
                last_egui_focus: None,
                pending_cdp: Vec::new(),
                next_cdp_id: 1,
                service_rows: std::collections::BTreeMap::new(),
                services_requested_at: None,
                service_requests: Some(service_requests),
                service_results: service_results_rx,
                palette_open: false,
                palette_query: String::new(),
                palette_rows: Vec::new(),
                palette_selected: 0,
                pending_jump: None,
            }))
        }),
    )
    .map_err(|error| anyhow::anyhow!("{error}"))
}

/// The terminator typed after an executed config command (M6, the
/// `commandConfig` and palette run paths). Upstream types
/// `command + "\n"` (verified `CmuxConfigExecutor` 196) and macOS shells
/// accept a raw LF as accept-line; the Windows console input model maps
/// **CR** (what the Enter key sends) to Enter, while a raw LF reaches
/// pwsh's PSReadLine as a continuation prompt (`>>`) and the command
/// never runs (observed 2026-10-24: `command.run` left `>> echo MARK`
/// on screen). CR submits in both pwsh and cmd.
fn command_input_bytes(text: &str) -> Vec<u8> {
    format!("{text}\r").into_bytes()
}

#[cfg(test)]
mod command_input_bytes_tests {
    #[test]
    fn executed_commands_end_in_cr_not_lf() {
        let bytes = super::command_input_bytes("echo hi");
        assert_eq!(bytes.last(), Some(&b'\r'));
        assert!(!bytes.ends_with(b"\n"));
    }
}

#[cfg(test)]
mod agent_stream_tests {
    use super::*;

    #[test]
    fn tail_rows_takes_the_last_visible_lines() {
        let screen = "row1\nrow2\nrow3\nrow4\nrow5";
        assert_eq!(
            tail_rows(screen, 3),
            vec!["row3".to_owned(), "row4".to_owned(), "row5".to_owned()]
        );
    }

    #[test]
    fn tail_rows_drops_leading_blank_lines() {
        let screen = "\n\nrow1\nrow2";
        assert_eq!(
            tail_rows(screen, 4),
            vec!["row1".to_owned(), "row2".to_owned()]
        );
    }

    #[test]
    fn tail_rows_handles_a_short_screen() {
        assert_eq!(tail_rows("only", 6), vec!["only".to_owned()]);
        assert_eq!(tail_rows("", 6), Vec::<String>::new());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find_action(table: &[(KeyCombo, Action)], action: &Action) -> Option<KeyCombo> {
        table
            .iter()
            .find(|(_, a)| a == action)
            .map(|(combo, _)| *combo)
    }

    #[test]
    fn parse_stroke_accepts_modifiers_and_key() {
        let combo = parse_stroke("ctrl+shift+r").unwrap();
        assert_eq!(combo.key, Key::R);
        assert!(combo.mods.ctrl && combo.mods.shift && !combo.mods.alt);
    }

    #[test]
    fn parse_stroke_rejects_unknown_tokens() {
        assert!(parse_stroke("hyper+r").is_none());
        assert!(parse_stroke("").is_none());
        // A modifier token never validates as a trailing key.
        assert!(parse_stroke("ctrl+shift+alt").is_none());
    }

    #[test]
    fn parse_stroke_maps_digits_and_named_keys() {
        assert_eq!(parse_stroke("ctrl+6").unwrap().key, Key::Num6);
        assert_eq!(parse_stroke("tab").unwrap().key, Key::Tab);
        assert_eq!(parse_stroke("ctrl+down").unwrap().key, Key::ArrowDown);
    }

    #[test]
    fn default_table_has_the_full_provisional_stroke_set() {
        let table = build_bindings(&BTreeMap::new());
        // 5 pane actions + 5 workspace actions + the 9-row digit family
        // + the lane-3 command palette stroke.
        assert_eq!(table.len(), 20);
        let expected = [
            (Key::R, Action::SplitFocused(SplitDirection::Right)),
            (Key::D, Action::SplitFocused(SplitDirection::Down)),
            (Key::W, Action::CloseFocused),
            (Key::Tab, Action::Cycle { forward: true }),
            (Key::Tab, Action::Cycle { forward: false }),
            (Key::N, Action::NewWorkspace),
            (Key::Q, Action::CloseActiveWorkspace),
            (Key::CloseBracket, Action::CycleWorkspace { forward: true }),
            (Key::OpenBracket, Action::CycleWorkspace { forward: false }),
            (Key::B, Action::ToggleSidebar),
            (Key::K, Action::CommandPalette),
        ];
        for (key, action) in expected {
            let combo =
                find_action(&table, &action).unwrap_or_else(|| panic!("missing {action:?}"));
            assert_eq!(combo.key, key, "{action:?} bound to the wrong key");
        }
        // The command palette advertises Ctrl+K with a bare ctrl.
        let palette = find_action(&table, &Action::CommandPalette).unwrap();
        assert!(palette.mods.ctrl && !palette.mods.shift && !palette.mods.alt);
        // The digit family: ctrl+k selects workspace k (1-based).
        for k in 1..=9u8 {
            let expected = Action::SelectWorkspace(usize::from(k - 1));
            let combo =
                find_action(&table, &expected).unwrap_or_else(|| panic!("missing {expected:?}"));
            assert_eq!(combo.key, digit_key(k), "digit family row {k}");
            assert!(combo.mods.ctrl && !combo.mods.shift);
        }
    }

    #[test]
    fn config_overrides_replace_and_unbind() {
        let mut bindings = BTreeMap::new();
        bindings.insert(
            "splitRight".to_owned(),
            Binding::Stroke("ctrl+shift+s".to_owned()),
        );
        bindings.insert("closePane".to_owned(), Binding::Unbound);
        let table = build_bindings(&bindings);
        // 20 default strokes minus the unbound closePane.
        assert_eq!(table.len(), 19);
        let combo = find_action(&table, &Action::SplitFocused(SplitDirection::Right)).unwrap();
        assert_eq!(combo.key, Key::S);
        assert!(combo.mods.ctrl && combo.mods.shift);
        assert!(find_action(&table, &Action::CloseFocused).is_none());
    }

    #[test]
    fn select_by_number_rebinds_are_rejected_and_defaults_survive() {
        let mut bindings = BTreeMap::new();
        bindings.insert(
            "selectWorkspaceByNumber".to_owned(),
            Binding::Stroke("ctrl+shift+5".to_owned()),
        );
        let table = build_bindings(&bindings);
        for k in 1..=9u8 {
            let expected = Action::SelectWorkspace(usize::from(k - 1));
            let combo =
                find_action(&table, &expected).unwrap_or_else(|| panic!("missing {expected:?}"));
            assert_eq!(
                combo.key,
                digit_key(k),
                "default family row {k} must survive"
            );
        }
    }

    #[test]
    fn action_ids_resolve() {
        assert_eq!(action_for_id("equalizeSplits"), Some(Action::EqualizeAll));
        assert_eq!(
            action_for_id("resizePaneLeft"),
            Some(Action::Resize(ResizeDirection::Left))
        );
        assert_eq!(action_for_id("newTab"), Some(Action::NewWorkspace));
        assert_eq!(
            action_for_id("closeTab"),
            Some(Action::CloseActiveWorkspace)
        );
        assert_eq!(
            action_for_id("nextSidebarTab"),
            Some(Action::CycleWorkspace { forward: true })
        );
        assert_eq!(
            action_for_id("prevSidebarTab"),
            Some(Action::CycleWorkspace { forward: false })
        );
        assert_eq!(action_for_id("toggleSidebar"), Some(Action::ToggleSidebar));
        // The digit family resolves from the stroke's key, not the id.
        assert_eq!(action_for_id("selectWorkspaceByNumber"), None);
        assert_eq!(action_for_id("doesNotExist"), None);
    }

    /// egui key for digit `1..=9` (the digit-family tests).
    fn digit_key(k: u8) -> Key {
        match k {
            1 => Key::Num1,
            2 => Key::Num2,
            3 => Key::Num3,
            4 => Key::Num4,
            5 => Key::Num5,
            6 => Key::Num6,
            7 => Key::Num7,
            8 => Key::Num8,
            _ => Key::Num9,
        }
    }

    #[test]
    fn adjusted_active_after_close_follows_selection() {
        // A tab before the selection shifts the selection back.
        assert_eq!(adjusted_active_after_close(2, 2, 0), 1);
        // Removing the active tab lands on the same slot (now the next tab).
        assert_eq!(adjusted_active_after_close(2, 1, 1), 1);
        // A later tab removed leaves the selection alone.
        assert_eq!(adjusted_active_after_close(2, 0, 1), 0);
        // Clamping when the list shrank to one.
        assert_eq!(adjusted_active_after_close(1, 0, 0), 0);
        // The empty result signals the caller to close the window.
        assert_eq!(adjusted_active_after_close(0, 0, 0), 0);
    }

    #[test]
    fn invalid_stroke_falls_back_to_default() {
        let mut bindings = BTreeMap::new();
        bindings.insert(
            "splitRight".to_owned(),
            Binding::Stroke("hyper+r".to_owned()),
        );
        let table = build_bindings(&bindings);
        let combo = find_action(&table, &Action::SplitFocused(SplitDirection::Right)).unwrap();
        assert_eq!(
            combo.key,
            Key::R,
            "the default stroke must win back the action"
        );
    }

    #[test]
    fn chord_overrides_are_skipped() {
        let mut bindings = BTreeMap::new();
        bindings.insert(
            "splitDown".to_owned(),
            Binding::Chord(["ctrl+b".to_owned(), "d".to_owned()]),
        );
        let table = build_bindings(&bindings);
        let combo = find_action(&table, &Action::SplitFocused(SplitDirection::Down)).unwrap();
        assert_eq!(
            combo.key,
            Key::D,
            "defaults must survive an unapplied chord"
        );
    }

    #[test]
    fn every_default_binding_parses() {
        for &(id, stroke) in DEFAULT_BINDINGS {
            let Some(stroke) = stroke else { continue };
            assert!(parse_stroke(stroke).is_some(), "{stroke} for {id}");
        }
    }

    #[test]
    fn theme_defaults_when_unconfigured() {
        let theme = theme_from_config(&cmux_config::ThemeConfig::default());
        assert_eq!(theme.scheme, None);
        // The design tokens (figma-design-reference.md section 2).
        assert_eq!(theme.background, TOKEN_MAIN_BG);
        assert_eq!(theme.sidebar_background, TOKEN_SIDEBAR_BG);
        assert_eq!(theme.viewport_background, TOKEN_VIEWPORT_BG);
        assert_eq!(theme.chip_background, TOKEN_CHIP_BG);
        assert_eq!(theme.cursor, None);
    }

    #[test]
    fn theme_defaults_match_the_figma_values() {
        // Snapshot of the exact hexes and where each one paints.
        assert_eq!(TOKEN_MAIN_BG, 0x11131A);
        assert_eq!(TOKEN_VIEWPORT_BG, 0x0C0E14);
        assert_eq!(TOKEN_SIDEBAR_BG, 0x191B22);
        assert_eq!(TOKEN_ACCENT, 0x38BDF8);
        assert_eq!(TOKEN_CHIP_BG, 0x1D1F26);
        assert_eq!(TOKEN_TEXT_PRIMARY, 0xE2E2EB);
        assert_eq!(TOKEN_TEXT_SECONDARY, 0xBDC8D1);
        assert_eq!(TOKEN_TEXT_MUTED, 0x87929A);
        assert_eq!(TOKEN_INFO, 0x8ED5FF);
        assert_eq!(TOKEN_WARNING, 0xFFC42F);
        assert_eq!(TOKEN_DANGER, 0xFFB4AB);
        assert_eq!(TOKEN_HAIRLINE, (62, 72, 79, 102));
        assert_eq!(TOKEN_HAIRLINE_SOFT, (62, 72, 79, 77));
    }

    #[test]
    fn token_and_hairline_helpers_convert_packed_values() {
        assert_eq!(
            token_color(0x38BDF8),
            egui::Color32::from_rgb(0x38, 0xBD, 0xF8)
        );
        assert_eq!(
            hairline_color((62, 72, 79, 102)),
            egui::Color32::from_rgba_unmultiplied(62, 72, 79, 102)
        );
    }

    #[test]
    fn browser_home_url_encodes_the_resolved_token() {
        // The default token: the design's viewport fill, percent-encoded.
        assert_eq!(
            browser_home_url(0x0C0E14),
            "data:text/html,<html><body style=\"background:%230c0e14;\
             margin:0;height:100%25;overflow:hidden\"></body></html>"
        );
        // A configured theme.background moves the home with the chrome.
        assert_eq!(
            browser_home_url(0xFFC42F),
            "data:text/html,<html><body style=\"background:%23ffc42f;\
             margin:0;height:100%25;overflow:hidden\"></body></html>"
        );
        // The URL scheme stays a data URL: no network fetch at spawn.
        assert!(browser_home_url(0).starts_with("data:text/html,"));
    }

    #[test]
    fn fold_label_caps_head_and_counts_the_ellipsis_toward_the_cap() {
        // Short labels pass through whole.
        assert_eq!(fold_label("w2".to_owned(), STATUS_WS_MAX_CHARS), "w2");
        // Longer ones keep cap-1 head codepoints; the glyph closes the cap.
        let expected_head: String = "a".repeat(STATUS_WS_MAX_CHARS - 1);
        assert_eq!(
            fold_label("a".repeat(STATUS_WS_MAX_CHARS + 1), STATUS_WS_MAX_CHARS),
            format!("{expected_head}\u{2026}")
        );
    }

    #[test]
    fn status_workspace_label_tracks_the_active_workspace_within_the_budget() {
        // The dominant case paints whole: `w1 · powershell` fits the ws cap.
        assert_eq!(
            status_workspace_label("w1", "powershell"),
            "w1 \u{b7} powershell"
        );
        // A pane without an OSC title still tracks the minted id.
        assert_eq!(status_workspace_label("w2", "shell 2"), "w2 \u{b7} shell 2");
        // The label folds inside its own cap (id + middot survive the
        // fold, so the minted id - the switch tracker - always shows).
        let long = status_workspace_label("w3", "authoritative-workspace-name-without-any-fitting");
        assert_eq!(long.chars().count(), STATUS_WS_MAX_CHARS);
        assert!(long.starts_with("w3 \u{b7} "));
        assert!(long.ends_with('\u{2026}'));
    }

    #[test]
    fn friendly_osc_title_folds_windows_image_paths_to_stems() {
        // The live repro: Windows PowerShell 5.1's console title.
        assert_eq!(
            friendly_osc_title("C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe"),
            "powershell"
        );
        // Drive + forward-slash form and case-insensitive extension.
        assert_eq!(friendly_osc_title("c:/Program Files/node.EXE"), "node");
        // UNC paths fold too.
        assert_eq!(friendly_osc_title("\\\\wsl$\\Ubuntu\\nu.exe"), "nu");
    }

    #[test]
    fn friendly_osc_title_passes_everything_else_through() {
        assert_eq!(friendly_osc_title("zsh"), "zsh");
        // A relative path is not an absolute image path.
        assert_eq!(friendly_osc_title("tools\\build.exe"), "tools\\build.exe");
        // A bare program name is not a path at all.
        assert_eq!(friendly_osc_title("build.exe"), "build.exe");
        // A path that does not end in the image extension stays whole.
        assert_eq!(
            friendly_osc_title("C:\\archive.tar.gz"),
            "C:\\archive.tar.gz"
        );
        // An empty stem is not a fold (keep the full title readable).
        assert_eq!(friendly_osc_title("C:\\dir\\.exe"), "C:\\dir\\.exe");
    }

    #[test]
    fn status_machine_labels_format_design_literal_shapes() {
        assert_eq!(status_cpu_label(Some(18)), "CPU: 18%");
        assert_eq!(status_cpu_label(Some(100)), "CPU: 100%");
        assert_eq!(status_cpu_label(None), "CPU: –");
        // The design's `MEM: 1.4 GB`; whole GB at/above ten, MB below one.
        assert_eq!(status_mem_label(Some(1_503_238_553)), "MEM: 1.4 GB");
        assert_eq!(status_mem_label(Some(10_737_418_240)), "MEM: 10 GB");
        assert_eq!(status_mem_label(Some(314_572_800)), "MEM: 300 MB");
        assert_eq!(status_mem_label(None), "MEM: –");
    }

    #[test]
    fn status_claude_and_ipc_and_version_labels_are_exact() {
        assert_eq!(status_claude_label(true), "CLAUDE: ACTIVE");
        assert_eq!(status_claude_label(false), "CLAUDE: IDLE");
        assert_eq!(
            status_ipc_label_capped("\\\\.\\pipe\\cmux-1404", STATUS_IPC_LEAF_MAX),
            "pipe: cmux-1404"
        );
        assert_eq!(
            status_ipc_label_capped("ipc/fwd", STATUS_IPC_LEAF_MAX),
            "pipe: fwd"
        );
        assert_eq!(status_ipc_label_capped("", STATUS_IPC_LEAF_MAX), "ipc off");
        // The SID-derived default pipe folds to its tail (the
        // distinguishing part), `pipe: ` outside the cap.
        assert_eq!(
            status_ipc_label_capped(
                "\\\\.\\pipe\\cmux-s-1-5-21-1806527352-874381699-1609267108-1001",
                STATUS_IPC_LEAF_MAX
            ),
            "pipe: \u{2026}09267108-1001"
        );
        // Exactly at the cap stays whole.
        let exact = "p".repeat(STATUS_IPC_LEAF_MAX);
        assert_eq!(
            status_ipc_label_capped(&exact, STATUS_IPC_LEAF_MAX),
            format!("pipe: {exact}")
        );
        // One over the cap folds to a leading ellipsis + the LAST
        // `STATUS_IPC_LEAF_MAX - 1` codepoints.
        let overflowed = format!("{exact}0");
        assert_eq!(
            status_ipc_label_capped(&overflowed, STATUS_IPC_LEAF_MAX),
            "pipe: \u{2026}pppppppppppp0"
        );
        // The short reserve (owner round 6 fold matrix) keeps the LAST
        // `STATUS_IPC_LEAF_SHORT - 1` codepoints behind the ellipsis.
        assert_eq!(
            status_ipc_label_capped(&overflowed, STATUS_IPC_LEAF_SHORT),
            "pipe: \u{2026}pppppp0"
        );
    }

    #[test]
    fn status_ws_fit_case_prefers_name_then_leaf_tail_then_name() {
        // Live-like px: a full ws label ~130, id-only ~40, leaf full ~136,
        // leaf short ~96 (round 8: no marker term in the inequality).
        let (full, id, leaf_f, leaf_s) = (130.0, 40.0, 136.0, 96.0);
        // Everything fits -> nothing folds.
        assert_eq!(
            status_ws_fit_case(full + leaf_f + 1.0, full, id, leaf_f, leaf_s),
            (true, false)
        );
        // Small deficit -> the leaf tail flexes first, the name stays.
        assert_eq!(
            status_ws_fit_case(full + leaf_f - 20.0, full, id, leaf_f, leaf_s),
            (true, true)
        );
        // 60px deficit: the tail alone cannot fix it; the NAME drops next
        // while the leaf re-inflates to full.
        assert_eq!(
            status_ws_fit_case(full + leaf_f - 60.0, full, id, leaf_f, leaf_s),
            (false, false)
        );
        // 110px deficit: both variants drop (terminal case).
        assert_eq!(
            status_ws_fit_case(full + leaf_f - 110.0, full, id, leaf_f, leaf_s),
            (false, true)
        );
    }

    #[test]
    fn system_cpu_percent_refines_snapshot_deltas() {
        // Kernel time counts idle, so no idle growth over a busy interval
        // reads every core busy.
        assert_eq!(
            system_cpu_percent((1_000, 10_000), (1_000, 20_000)),
            Some(100)
        );
        assert_eq!(system_cpu_percent((0, 10_000), (4_000, 20_000)), Some(60));
        assert_eq!(
            system_cpu_percent((10_000, 10_000), (10_000, 20_000)),
            Some(100)
        );
        assert_eq!(system_cpu_percent((10_000, 10_000), (10_000, 10_000)), None);
        // A genuinely idle machine clamps to 0 rather than wrapping.
        assert_eq!(
            system_cpu_percent((u64::MAX / 2, u64::MAX / 2), (u64::MAX, u64::MAX)),
            Some(0)
        );
    }

    #[test]
    fn title_crumb_texts_fall_back_to_the_untitled_dash() {
        let (ws, pane) = title_crumb_texts("backend-monorepo", None);
        assert_eq!(ws, "backend-monorepo");
        assert_eq!(pane, "\u{2013}");
        let (ws, pane) = title_crumb_texts("cloud-orchestrator", Some("agent-loop"));
        assert_eq!(ws, "cloud-orchestrator");
        assert_eq!(pane, "agent-loop");
    }

    #[test]
    fn title_bar_hit_decision_routes_caption_client_and_passes_below() {
        // Strip = rows 0..64 (2x scale), control zones = last three
        // 44pt*2-scale zones… asserted through the plain decision fn with
        // physical coordinates matching the WM_NCHITTEST glue.
        let strip_h = 64;
        // physical zone width at 2x: 44*2 = 88; zones start 88*3 back.
        let buttons_left = 900 - 3 * 88;
        let buttons_right = 900;
        assert_eq!(
            title_bar_hit_decision(
                100,
                30,
                strip_h,
                buttons_left,
                buttons_right,
                Some((400, 800))
            ),
            TitleHitDecision::Caption
        );
        assert_eq!(
            title_bar_hit_decision(
                buttons_right - 1,
                10,
                strip_h,
                buttons_left,
                buttons_right,
                Some((400, 800))
            ),
            TitleHitDecision::Client
        );
        assert_eq!(
            title_bar_hit_decision(
                buttons_left,
                63,
                strip_h,
                buttons_left,
                buttons_right,
                Some((400, 800))
            ),
            TitleHitDecision::Client
        );
        // The command-bar zone routes client too (the egui bar button).
        assert_eq!(
            title_bar_hit_decision(
                400,
                20,
                strip_h,
                buttons_left,
                buttons_right,
                Some((400, 800))
            ),
            TitleHitDecision::Client
        );
        assert_eq!(
            title_bar_hit_decision(
                799,
                63,
                strip_h,
                buttons_left,
                buttons_right,
                Some((400, 800))
            ),
            TitleHitDecision::Client
        );
        // Left of the bar reads caption; right of the bar (but left of
        // the control zones) reads caption again.
        assert_eq!(
            title_bar_hit_decision(
                399,
                20,
                strip_h,
                buttons_left,
                buttons_right,
                Some((400, 800))
            ),
            TitleHitDecision::Caption
        );
        // No command-bar zone (narrow client): mid-strip points read
        // caption as before.
        assert_eq!(
            title_bar_hit_decision(100, 30, strip_h, buttons_left, buttons_right, None),
            TitleHitDecision::Caption
        );
        // The next-row boundary and everything below is not the strip.
        assert_eq!(
            title_bar_hit_decision(
                100,
                64,
                strip_h,
                buttons_left,
                buttons_right,
                Some((400, 800))
            ),
            TitleHitDecision::Pass
        );
        assert_eq!(
            title_bar_hit_decision(
                100,
                200,
                strip_h,
                buttons_left,
                buttons_right,
                Some((400, 800))
            ),
            TitleHitDecision::Pass
        );
        // The zones are right-exclusive: exactly at the left edge of the
        // strip's last row still reads caption whenever no command-bar
        // zone underlaps it.
        assert_eq!(
            title_bar_hit_decision(
                buttons_left - 1,
                20,
                strip_h,
                buttons_left,
                buttons_right,
                None
            ),
            TitleHitDecision::Caption
        );
    }

    #[test]
    fn title_command_bar_rect_follows_the_design_float() {
        // The mock floats the 420pt bar at x=425.68 of a 1280 header.
        let bar = title_command_bar_rect_pts(1280.0).expect("wide client shows the bar");
        assert!(
            (bar.left() - 425.728).abs() < 0.01,
            "design float {}",
            bar.left()
        );
        assert_eq!(bar.height(), 24.0);
        // Ours (900pt): left clamps above the design ratio (worst-case
        // breadcrumb room) and the bar clears the control zones.
        let bar = title_command_bar_rect_pts(900.0).expect("900pt shows the bar");
        assert_eq!(bar.left(), 335.0);
        assert!(
            bar.right() <= 900.0 - 3.0 * 44.0,
            "bar clears the native controls: {}",
            bar.right()
        );
        // Narrow client: hidden entirely.
        assert!(title_command_bar_rect_pts(799.0).is_none());
    }

    #[test]
    fn palette_filter_matches_labels_and_bodies_case_insensitively() {
        let rows = vec![
            PaletteEntry::Command {
                name: "Run checks".to_owned(),
                text: "cargo fmt && cargo clippy".to_owned(),
            },
            PaletteEntry::Jump {
                label: "shell — main branch".to_owned(),
                workspace: 0,
                pane: "pane-1".to_owned(),
            },
        ];
        // An empty query passes everything through.
        assert_eq!(palette_filter(&rows, "").len(), 2);
        assert_eq!(
            palette_filter(&rows, "  ").len(),
            rows.len(),
            "whitespace query"
        );
        // Label matching is case-insensitive.
        assert_eq!(palette_filter(&rows, "RUN").len(), 1);
        // A command's body matches too (dispatch by what it types).
        assert_eq!(palette_filter(&rows, "CLIPPY").len(), 1);
        // No matches keep the empty list (the overlay shows its muted line).
        assert!(palette_filter(&rows, "nomatch").is_empty());
    }

    // Owner round 4 (2026-10-26): the keycap pin went with the pills.
    #[test]
    fn title_control_zones_match_the_design_buttons() {
        // The design's three window controls are 44×32 each (nodes
        // 1:534/1:537/1:540 within the 32px header).
        assert_eq!(TITLE_CONTROL_ZONE_W_PTS, 44.0);
        assert_eq!(TITLE_CONTROL_ZONES, 3.0);
        assert_eq!(TITLE_BAR_HEIGHT, 32.0);
    }

    #[test]
    fn title_band_decision_matches_the_chromium_recipe() {
        // 1200×800 client, 10px bands, 40px strip, buttons from 1035,
        // command bar 500..900.
        let (width, height, band, strip_h, buttons_left) = (1200, 800, 10, 40, 1035);
        let cmd = Some((500, 900));
        // Interior: no band.
        assert_eq!(
            title_band_decision(600, 400, width, height, band, strip_h, buttons_left, cmd),
            None
        );
        // Straight edges.
        assert_eq!(
            title_band_decision(4, 400, width, height, band, strip_h, buttons_left, cmd),
            Some(TitleBand::Left)
        );
        assert_eq!(
            title_band_decision(1196, 400, width, height, band, strip_h, buttons_left, cmd),
            Some(TitleBand::Right)
        );
        assert_eq!(
            title_band_decision(600, 795, width, height, band, strip_h, buttons_left, cmd),
            Some(TitleBand::Bottom)
        );
        // Top band over the blank strip (left of the bar and zones).
        assert_eq!(
            title_band_decision(400, 4, width, height, band, strip_h, buttons_left, cmd),
            Some(TitleBand::Top),
            "400 is left of the bar"
        );
        // Corners.
        assert_eq!(
            title_band_decision(4, 4, width, height, band, strip_h, buttons_left, cmd),
            Some(TitleBand::TopLeft)
        );
        assert_eq!(
            title_band_decision(600, 795, width, height, band, strip_h, buttons_left, cmd),
            Some(TitleBand::Bottom)
        );
        assert_eq!(
            title_band_decision(4, 795, width, height, band, strip_h, buttons_left, cmd),
            Some(TitleBand::BottomLeft)
        );
        // Below the strip the right band still applies (pane side).
        assert_eq!(
            title_band_decision(1196, 300, width, height, band, strip_h, buttons_left, cmd),
            Some(TitleBand::Right)
        );
        // Inside the strip's control-zone x-range: buttons win, no band.
        assert_eq!(
            title_band_decision(1196, 4, width, height, band, strip_h, buttons_left, cmd),
            None
        );
        // Top band over the command-bar x-range: the bar's clickable
        // surface wins there too (points reach egui, not the resize edge).
        assert_eq!(
            title_band_decision(499, 4, width, height, band, strip_h, buttons_left, cmd),
            Some(TitleBand::Top),
            "499 is left of the bar's right-exclusive zone"
        );
        assert_eq!(
            title_band_decision(700, 4, width, height, band, strip_h, buttons_left, cmd),
            None,
            "700 is inside the bar"
        );
        // Without a command-bar zone the same point reads the band again.
        assert_eq!(
            title_band_decision(700, 4, width, height, band, strip_h, buttons_left, None),
            Some(TitleBand::Top)
        );
        // No bands while maximized is decided by the caller.
        assert_eq!(
            title_band_decision(4, 400, width, height, 0, strip_h, buttons_left, cmd),
            None
        );
    }

    #[test]
    fn design_visuals_carry_the_tokens_into_egui() {
        let ctx = egui::Context::default();
        install_design_visuals(&ctx, &ThemeRuntime::default());
        let visuals = ctx.global_style().visuals.clone();
        assert_eq!(
            visuals.selection.stroke,
            egui::Stroke::new(1.0, token_color(TOKEN_ACCENT))
        );
        assert_eq!(
            visuals.widgets.active.fg_stroke.color,
            token_color(TOKEN_TEXT_PRIMARY)
        );
        assert_eq!(
            visuals.widgets.noninteractive.fg_stroke.color,
            token_color(TOKEN_TEXT_SECONDARY)
        );
        assert_eq!(visuals.weak_text_color, Some(token_color(TOKEN_TEXT_MUTED)));
        assert_eq!(visuals.warn_fg_color, token_color(TOKEN_WARNING));
        assert_eq!(visuals.error_fg_color, token_color(TOKEN_DANGER));
        assert_eq!(visuals.hyperlink_color, token_color(TOKEN_INFO));
        assert!(visuals.dark_mode);
    }

    #[test]
    fn design_visuals_are_skipped_for_configured_light_schemes() {
        let ctx = egui::Context::default();
        ctx.global_style_mut(|style| style.visuals = egui::Visuals::light());
        let theme = ThemeRuntime {
            scheme: Some(true),
            ..Default::default()
        };
        install_design_visuals(&ctx, &theme);
        // egui's own light visuals stay (no light design variant exists).
        assert!(!ctx.global_style().visuals.dark_mode);
    }

    #[test]
    fn theme_maps_configured_scheme_background_and_cursor() {
        let config = cmux_config::ThemeConfig {
            scheme: Some("light".to_owned()),
            background: Some("#1A1B2E".to_owned()),
            cursor: Some("FFCC00".to_owned()),
        };
        let theme = theme_from_config(&config);
        assert_eq!(theme.scheme, Some(true));
        assert_eq!(theme.background, 0x1A1B2E);
        assert_eq!(theme.cursor, Some(0xFFCC00));
        // The sidebar derives from the resolved background.
        assert_eq!(theme.sidebar_background, 0x161728);
        // And the chip derives from the sidebar (recorded approximation:
        // `#191b22/0.88` rounds to `#1c1f27` against the design's flat
        // `#1d1f26` — here `0x161728` → `0x191a2d`).
        assert_eq!(theme.chip_background, 0x191A2D);
    }

    #[test]
    fn theme_dark_scheme_and_hashless_hex_accepted() {
        let config = cmux_config::ThemeConfig {
            scheme: Some("dark".to_owned()),
            background: Some("10141c".to_owned()),
            ..Default::default()
        };
        let theme = theme_from_config(&config);
        assert_eq!(theme.scheme, Some(false));
        assert_eq!(theme.background, 0x10141C);
    }

    #[test]
    fn theme_unknown_values_fall_back_to_defaults() {
        let config = cmux_config::ThemeConfig {
            scheme: Some("solarized".to_owned()),
            background: Some("#12g45z".to_owned()),
            cursor: Some("xyz".to_owned()),
        };
        let theme = theme_from_config(&config);
        assert_eq!(theme.scheme, None);
        assert_eq!(theme.background, ThemeRuntime::default().background);
        assert_eq!(
            theme.sidebar_background,
            ThemeRuntime::default().sidebar_background
        );
        assert_eq!(
            theme.chip_background,
            ThemeRuntime::default().chip_background
        );
        assert_eq!(theme.cursor, None);
    }

    #[test]
    fn theme_configured_background_pins_the_viewport() {
        // A configured background keeps the old chrome-blends-with-cells
        // contract (background + darker sidebar + same pane interior);
        // only the unconfigured default takes the design viewport token.
        let config = cmux_config::ThemeConfig {
            background: Some("#1e1e1e".to_owned()),
            ..Default::default()
        };
        let theme = theme_from_config(&config);
        assert_eq!(theme.background, 0x1E1E1E);
        assert_eq!(theme.sidebar_background, 0x1A1A1A);
        assert_eq!(theme.viewport_background, 0x1E1E1E);
        // The chip ladder rides along one step lighter than the sidebar
        // (`0x1a1a1a` → `0x1e1e1e`).
        assert_eq!(theme.chip_background, 0x1E1E1E);
    }

    #[test]
    fn parse_hex_color_accepts_hash_and_plain_forms_only() {
        assert_eq!(parse_hex_color("#1a1b2e"), Some(0x1a1b2e));
        assert_eq!(parse_hex_color("1A1B2E"), Some(0x1a1b2e));
        assert_eq!(parse_hex_color("#abc"), None, "3-digit form not supported");
        assert_eq!(parse_hex_color("#1a1b2"), None);
        assert_eq!(parse_hex_color("#zzzzzz"), None);
        assert_eq!(parse_hex_color(""), None);
    }

    #[test]
    fn shade_darker_truncates_channels_downward() {
        assert_eq!(shade_darker(0xFFFFFF), 0xE0E0E0);
        assert_eq!(shade_darker(0), 0);
        // The hue holds while every channel stays at or below the source.
        let shaded = shade_darker(0x507C9B);
        assert!(shaded >> 16 <= 0x50 && ((shaded >> 8) & 0xFF) <= 0x7C && (shaded & 0xFF) <= 0x9B);
    }

    #[test]
    fn shade_lighter_rounds_channels_upward_and_clamps() {
        // The design ladder check (recorded approximation).
        assert_eq!(shade_lighter(0x191B22), 0x1C1F27);
        assert_eq!(shade_lighter(0x161728), 0x191A2D);
        assert_eq!(shade_lighter(0), 0);
        // Clamped at white (255/0.88 would overflow).
        assert_eq!(shade_lighter(0xFFFFFF), 0xFFFFFF);
        // Round-trips the truncated darker shade exactly back up.
        assert_eq!(shade_lighter(shade_darker(0x507C9B)), 0x507C9B);
    }

    #[test]
    fn font_scale_clamps_to_bounds() {
        let mut settings = RuntimeSettings::from_config(&Config::default());
        assert_eq!(settings.font_scale(), 1.0);
        settings.font_percent = 500;
        assert_eq!(settings.font_scale(), 3.0);
        settings.font_percent = 10;
        assert_eq!(settings.font_scale(), 0.5);
    }

    #[test]
    fn config_broken_file_falls_back_via_load_errors() {
        // load_config reads env at call time; exercising the error path of
        // the underlying load instead (load_config itself starts no UI).
        let path = std::env::temp_dir().join(format!(
            "cmux-shell-test-{}-broken.json",
            std::process::id()
        ));
        std::fs::write(&path, "{ not json }").unwrap();
        let error = Config::load_file(&path).unwrap_err();
        assert!(error.to_string().contains("failed to decode config"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn workspace_snapshot_walks_the_outline_with_tab_membership() {
        let outline = TreeOutline::Split {
            orientation: Orientation::Horizontal,
            divider: 0.4,
            first: Box::new(TreeOutline::Leaf {
                id: "p1".to_owned(),
            }),
            second: Box::new(TreeOutline::Leaf {
                id: "p2".to_owned(),
            }),
        };
        let workspace = workspace_from_outline(
            &outline,
            &|id: &str| {
                if id == "p1" {
                    vec!["p1".to_owned(), "p9".to_owned()]
                } else {
                    vec![id.to_owned()]
                }
            },
            &|_| None,
            &|_| "terminal",
            "p2",
            "w1",
            None,
        );
        assert_eq!(workspace.workspace_id, "w1");
        assert_eq!(workspace.focused_panel_id.as_deref(), Some("p2"));
        assert_eq!(workspace.panels.len(), 2);
        // The split survives on the wire; the first leaf carries both tabs.
        let (_, _, first, second) = workspace.layout.as_split().unwrap();
        let first_leaf = first.as_leaf().unwrap();
        assert_eq!(first_leaf.panel_ids, vec!["p1".to_owned(), "p9".to_owned()]);
        assert_eq!(first_leaf.selected_panel_id.as_deref(), Some("p1"));
        let second_leaf = second.as_leaf().unwrap();
        assert_eq!(second_leaf.panel_ids, vec!["p2".to_owned()]);
        assert_eq!(second_leaf.selected_panel_id.as_deref(), Some("p2"));
    }

    #[test]
    fn workspace_snapshot_defaults_unknown_panels_to_their_own_tab() {
        let outline = TreeOutline::Leaf {
            id: "p7".to_owned(),
        };
        let workspace = workspace_from_outline(
            &outline,
            &|_| Vec::new(),
            &|_| None,
            &|_| "terminal",
            "p7",
            "w1",
            None,
        );
        let leaf = workspace.layout.as_leaf().unwrap();
        assert_eq!(leaf.panel_ids, vec!["p7".to_owned()]);
        assert_eq!(leaf.selected_panel_id.as_deref(), Some("p7"));
        assert_eq!(workspace.panels.len(), 1);
        assert_eq!(workspace.panels[0].panel_type, "terminal");
    }

    #[test]
    fn workspace_snapshot_records_an_agent_leaf_panel_type() {
        let outline = TreeOutline::Leaf {
            id: "p8".to_owned(),
        };
        let workspace = workspace_from_outline(
            &outline,
            &|_| Vec::new(),
            &|_| None,
            &|id: &str| if id == "p8" { "agent" } else { "terminal" },
            "p8",
            "w1",
            None,
        );
        // Lane 5: the agent activity pane saves its own panel kind (and
        // no browser URL), so restore rebuilds the pane without a shell.
        assert_eq!(workspace.panels[0].panel_type, "agent");
        let leaf = workspace.layout.as_leaf().unwrap();
        assert!(leaf.browser_url.is_none());
    }

    #[test]
    fn restored_seed_lands_past_every_tab_id() {
        // Two leaves; the second carries extra hidden tabs (p9 and a
        // non-numeric id) and the seed must clear every numeric one.
        let layout = WorkspaceLayoutSnapshot::Split(SplitLayoutSnapshot {
            orientation: "horizontal".to_owned(),
            divider_position: 0.5,
            first: Box::new(WorkspaceLayoutSnapshot::Pane(PaneLayoutSnapshot {
                panel_ids: vec!["p3".to_owned()],
                selected_panel_id: Some("p3".to_owned()),
                is_full_width_tab_mode: None,
                browser_url: None,
            })),
            second: Box::new(WorkspaceLayoutSnapshot::Pane(PaneLayoutSnapshot {
                panel_ids: vec!["p12".to_owned(), "p9".to_owned(), "agent".to_owned()],
                selected_panel_id: Some("p12".to_owned()),
                is_full_width_tab_mode: None,
                browser_url: None,
            })),
        });
        assert_eq!(restored_seed(&layout), 13);
    }

    #[test]
    fn only_the_workspace_focused_pane_counts_as_focused_surface() {
        // Regression lock (M3.3): the old drum compared the event's own pane
        // id against itself, so every pane in the active workspace claimed
        // focus. The workspace's focused pane id decides, and the workspace
        // must be the active one.
        assert!(surface_focused("p1", "p1", 0, 0));
        assert!(
            !surface_focused("p2", "p1", 0, 0),
            "a non-focused pane of the active workspace must not claim focus"
        );
        assert!(
            !surface_focused("p1", "p1", 1, 0),
            "a pane of an inactive workspace must not claim focus"
        );
    }

    #[test]
    fn surface_focused_survives_reordered_ids() {
        assert!(surface_focused("p9", "p9", 2, 2));
        assert!(!surface_focused("p9", "p12", 2, 2));
    }

    #[test]
    fn claude_injection_needs_claude_program_and_a_live_pipe() {
        let claude = PtySpec::new(r"C:\Users\noah\.local\bin\claude.exe")
            .env("CMUX_SURFACE_ID", "p1")
            .env("CMUX_SOCKET_PATH", r"\\.\pipe\cmux-test");
        assert!(claude_injection_eligible(&claude));

        // IPC off (no pipe identity) â†’ no doc: the hook commands would
        // enqueue into nothing.
        let no_pipe = PtySpec::new("claude.exe").env("CMUX_SURFACE_ID", "p1");
        assert!(!claude_injection_eligible(&no_pipe));

        // Upstream's disable toggle (`!= 1` guard semantics).
        let disabled = claude.clone().env("CMUX_CLAUDE_HOOKS_DISABLED", "1");
        assert!(!claude_injection_eligible(&disabled));
        // Any other value leaves the feature on.
        let disabled_other = claude.clone().env("CMUX_CLAUDE_HOOKS_DISABLED", "0");
        assert!(claude_injection_eligible(&disabled_other));

        // Non-claude programs are never touched.
        let shell = PtySpec::new("pwsh.exe").env("CMUX_SOCKET_PATH", "pipe");
        assert!(!claude_injection_eligible(&shell));
        // claude-adjacent names are not claude.
        let helper = PtySpec::new("claude-helper.exe").env("CMUX_SOCKET_PATH", "pipe");
        assert!(!claude_injection_eligible(&helper));
    }

    #[test]
    fn restored_claude_pane_is_left_alone() {
        // A persisted claude spec replays with the old `--settings` value;
        // the transform must not stack a second one.
        let restored = PtySpec::new("claude")
            .env("CMUX_SOCKET_PATH", "pipe")
            .arg("--settings")
            .arg(r"C:\claude-settings\cmux-claude-abc.json");
        assert!(!claude_injection_eligible(&restored));
    }

    #[test]
    fn claude_journal_route_events_never_ring() {
        // Upstream keeps the tool-loop feed route out of the notification
        // policy; the port mirrors that split.
        for deliverable in [
            "session-start",
            "stop",
            "prompt-submit",
            "session-end",
            "notification",
            "push-notification",
            "something-else",
        ] {
            assert!(subcommand_is_deliverable(deliverable), "{deliverable}");
        }
        for journal_only in ["feed", "pre-tool-use"] {
            assert!(
                !subcommand_is_deliverable(journal_only),
                "{journal_only} must stay out of rings and toasts"
            );
        }
    }

    #[test]
    fn identity_forwards_the_claude_hooks_disable_value() {
        // The disable value rides the managed pane env so the spawn gates
        // and hook processes agree on it.
        let forwarded = identity_spec_with(
            PtySpec::new("pwsh.exe"),
            "p1",
            0,
            r"\\.\pipe\cmux-test",
            Some("1"),
            None,
            None,
        );
        assert_eq!(forwarded.env_value("CMUX_CLAUDE_HOOKS_DISABLED"), Some("1"));
        assert_eq!(forwarded.env_value("CMUX_SURFACE_ID"), Some("p1"));

        // An empty value is absent, not forwarded.
        let absent = identity_spec_with(
            PtySpec::new("pwsh.exe"),
            "p1",
            0,
            "pipe",
            Some(""),
            None,
            None,
        );
        assert!(absent.env_value("CMUX_CLAUDE_HOOKS_DISABLED").is_none());
        // And with nothing in the environment nothing is added.
        let none = identity_spec_with(PtySpec::new("pwsh.exe"), "p1", 0, "pipe", None, None, None);
        assert!(none.env_value("CMUX_CLAUDE_HOOKS_DISABLED").is_none());
    }

    #[test]
    fn identity_prepends_the_shim_dir_to_the_pane_path() {
        let shim = ClaudeShim {
            dir: std::path::PathBuf::from(r"C:\loc\cmux\claude-settings\shims"),
            cmux_bin: std::path::PathBuf::from(r"C:\loc\target\debug\cmux.exe"),
        };
        let pane = identity_spec_with(
            PtySpec::new("pwsh.exe"),
            "p1",
            0,
            r"\\.\pipe\cmux-test",
            None,
            Some(r"C:\Windows\system32"),
            Some(&shim),
        );
        assert_eq!(
            pane.env_value("PATH"),
            Some(r"C:\loc\cmux\claude-settings\shims;C:\Windows\system32")
        );

        // No shim (IPC off / disabled / install failed): the pane PATH is
        // still routed explicitly to the app's live PATH â€” portable-pty
        // composes it from the default env block otherwise, which loses
        // app-side PATH mutations.
        let plain = identity_spec_with(
            PtySpec::new("pwsh.exe"),
            "p1",
            0,
            r"\\.\pipe\cmux-test",
            None,
            Some(r"C:\Windows\system32"),
            None,
        );
        assert_eq!(plain.env_value("PATH"), Some(r"C:\Windows\system32"));

        // And with an unreadable PATH nothing is forced.
        let unreadable = identity_spec_with(
            PtySpec::new("pwsh.exe"),
            "p1",
            0,
            r"\\.\pipe\cmux-test",
            None,
            None,
            None,
        );
        assert!(unreadable.env_value("PATH").is_none());
    }

    // ----- M4.1 automation row shapes -----

    #[test]
    fn workspace_rows_carry_the_upstream_keys() {
        let row = workspace_row(2, "w3", "cmux", true);
        assert_eq!(row["index"], 2);
        assert_eq!(row["id"], "w3");
        assert_eq!(row["title"], "cmux");
        assert_eq!(row["selected"], true);
    }

    #[test]
    fn reorder_moves_rows_and_follows_the_active_one() {
        let wake = FrameWake::new();
        let workspace = |id: &str, pane: &str| Workspace {
            id: id.to_owned(),
            tree: PaneTree::new(new_agent_pane(pane.to_owned(), &wake).unwrap()),
            focused: pane.to_owned(),
            custom_title: None,
        };
        let mut workspaces = Workspaces {
            items: vec![
                workspace("w1", "p1"),
                workspace("w2", "p2"),
                workspace("w3", "p3"),
            ],
            active: 2,
        };
        // Drag w3 to the front: the order changes, the active SELECTION
        // follows the moved workspace by id (upstream's link between the
        // tab-manager order and `selectedWorkspaceIndex`).
        workspaces.reorder(2, 0);
        assert_eq!(
            workspaces
                .items
                .iter()
                .map(|ws| ws.id.as_str())
                .collect::<Vec<_>>(),
            vec!["w3", "w1", "w2"]
        );
        assert_eq!(workspaces.active, 0);
        // Moving a non-active row never steals the selection.
        workspaces.reorder(1, 2);
        assert_eq!(
            workspaces
                .items
                .iter()
                .map(|ws| ws.id.as_str())
                .collect::<Vec<_>>(),
            vec!["w3", "w2", "w1"]
        );
        assert_eq!(workspaces.active, 0);
        // Out-of-range slots no-op (the planner filters its own, but the
        // model stays defensive).
        workspaces.reorder(0, 3);
        workspaces.reorder(3, 0);
        assert_eq!(
            workspaces
                .items
                .iter()
                .map(|ws| ws.id.as_str())
                .collect::<Vec<_>>(),
            vec!["w3", "w2", "w1"]
        );
        assert_eq!(workspaces.active, 0);
    }

    #[test]
    fn custom_title_takes_naming_precedence_and_clears_back() {
        let wake = FrameWake::new();
        let mut workspaces = Workspaces {
            items: vec![Workspace {
                id: "w1".to_owned(),
                tree: PaneTree::new(new_agent_pane("p1".to_owned(), &wake).unwrap()),
                focused: "p1".to_owned(),
                custom_title: None,
            }],
            active: 0,
        };
        // The agent pane's deterministic display title names the row.
        assert_eq!(workspaces.name_of(0), "Agent activity");
        // The user's rename wins over the live title.
        workspaces.set_custom_title(0, Some("docs lane".to_owned()));
        assert_eq!(workspaces.name_of(0), "docs lane");
        // Clearing falls back to the live title (the editor's commit
        // policy never clears; the model keeps the reset affordance).
        workspaces.set_custom_title(0, None);
        assert_eq!(workspaces.name_of(0), "Agent activity");
    }

    #[test]
    fn custom_title_round_trips_through_the_snapshot() {
        let outline = TreeOutline::Leaf {
            id: "p7".to_owned(),
        };
        let titled = workspace_from_outline(
            &outline,
            &|_| Vec::new(),
            &|_| None,
            &|_| "terminal",
            "p7",
            "w1",
            Some(" docs lane "),
        );
        assert_eq!(titled.custom_title.as_deref(), Some("docs lane"));
        let wire = serde_json::to_string(&titled).unwrap();
        let decoded: WorkspaceSnapshot = serde_json::from_str(&wire).unwrap();
        assert_eq!(decoded.custom_title.as_deref(), Some("docs lane"));
        // The restore seam narrows it into the model field.
        assert_eq!(
            restored_custom_title(&decoded).as_deref(),
            Some("docs lane")
        );
        // An unrenamed workspace saves no title and restores None.
        let auto = workspace_from_outline(
            &outline,
            &|_| Vec::new(),
            &|_| None,
            &|_| "terminal",
            "p7",
            "w1",
            None,
        );
        let wire = serde_json::to_string(&auto).unwrap();
        let decoded: WorkspaceSnapshot = serde_json::from_str(&wire).unwrap();
        assert_eq!(decoded.custom_title, None);
        assert_eq!(restored_custom_title(&decoded), None);
    }

    #[test]
    fn move_workspace_actions_resolve_unbound() {
        assert_eq!(
            action_for_id("moveWorkspaceUp"),
            Some(Action::MoveActiveWorkspace { up: true })
        );
        assert_eq!(
            action_for_id("moveWorkspaceDown"),
            Some(Action::MoveActiveWorkspace { up: false })
        );
        // Unbound defaults (ADR-0010's deferred family, live since round
        // 9): the ids are listed stroke-less in DEFAULT_BINDINGS —
        // resolvable for rebinding, no default key (the `equalizeSplits`
        // rule).
        assert_eq!(
            DEFAULT_BINDINGS
                .iter()
                .find(|(id, _)| *id == "moveWorkspaceUp")
                .map(|(_, stroke)| *stroke),
            Some(None)
        );
        assert_eq!(
            DEFAULT_BINDINGS
                .iter()
                .find(|(id, _)| *id == "moveWorkspaceDown")
                .map(|(_, stroke)| *stroke),
            Some(None)
        );
    }

    #[test]
    fn surface_rows_map_panes_one_to_one() {
        let row = surface_row(0, "p1", "w1", "terminal", Some("shell".to_owned()), true);
        assert_eq!(row["index"], 0);
        assert_eq!(row["id"], "p1");
        assert_eq!(row["pane_id"], "p1");
        assert_eq!(row["workspace_id"], "w1");
        assert_eq!(row["type"], "terminal");
        assert_eq!(row["focused"], true);
        // An untitled pane serializes a null title (upstream NSNull-ish).
        let untitled = surface_row(1, "p2", "w1", "terminal", None, false);
        assert_eq!(untitled["title"], serde_json::Value::Null);
        assert_eq!(untitled["focused"], false);
        // M5.1: a browser pane's row reports its own surface type.
        let browser = surface_row(2, "p3", "w1", "browser", Some("Page".to_owned()), false);
        assert_eq!(browser["type"], "browser");
        assert_eq!(browser["title"], "Page");
    }

    #[test]
    fn accel_digit_keys_map_to_workspace_actions() {
        // VK_1..VK_9 map to the workspace rows like the egui bindings
        // table (1→row 0); ctrl+0 and non-digits stay with the browser.
        assert_eq!(action_for_accel_key(0x31), Some(Action::SelectWorkspace(0)));
        assert_eq!(action_for_accel_key(0x39), Some(Action::SelectWorkspace(8)));
        assert_eq!(action_for_accel_key(0x30), None);
        assert_eq!(action_for_accel_key(0x41), None); // 'A'
        assert_eq!(action_for_accel_key(0x70), None); // F1
    }

    #[test]
    fn an_explicit_click_claims_model_focus_even_without_a_transition() {
        // The M5.1 accepted corner, closed by the M5.2 click claims: a
        // browser leaf holds model focus while the terminal widget keeps
        // egui's standing focus. Clicking that very widget produces
        // NO egui focus transition (focused == last), yet the explicit
        // click must still route the model to the clicked pane.
        assert_eq!(
            focus_reconcile_target(Some("p1"), Some("p1"), "p3", Some("p1")),
            Some("p1".to_owned()),
        );
    }

    #[test]
    fn reconcile_ignores_a_click_on_the_already_focused_pane() {
        assert_eq!(
            focus_reconcile_target(Some("p1"), Some("p1"), "p1", Some("p1")),
            None,
        );
        // A click frame with no standing egui focus on it is no-op too.
        assert_eq!(
            focus_reconcile_target(None, Some("p1"), "p1", Some("p1")),
            None,
        );
    }

    #[test]
    fn reconcile_still_follows_an_egui_transition_without_a_click() {
        // Rule (2): egui's focused pane changed since the previous frame
        // while no click reported this frame (e.g. a focus a widget
        // gained programmatically) — and the pane differs from the model.
        assert_eq!(
            focus_reconcile_target(None, Some("p1"), "p3", Some("p3")),
            Some("p1".to_owned()),
        );
        // ... but a browser leaf that `open_split` focused must not be
        // stolen back by the standing egui focus of an old terminal:
        // standing focus UNCHANGED against last_egui_focus routes nothing.
        assert_eq!(
            focus_reconcile_target(None, Some("p1"), "p3", Some("p1")),
            None,
        );
    }

    #[test]
    fn browser_user_data_dir_nests_per_surface() {
        let base = std::path::PathBuf::from("C:\\data");
        assert_eq!(
            browser_user_data_dir(&base, "p7"),
            std::path::PathBuf::from("C:\\data\\cmux\\webview2\\p7")
        );
        // The per-surface split: each id gets its own leaf folder.
        assert_ne!(
            browser_user_data_dir(&base, "p1"),
            browser_user_data_dir(&base, "p2")
        );
    }

    #[test]
    fn browser_panes_defer_hosts_and_title() {
        let mut pane = new_browser_pane(
            "pb1".to_owned(),
            "https://example.test/".to_owned(),
            &FrameWake::new(),
        )
        .expect("browser pane");
        // The host waits for the first frame's sync pass; the lane is
        // reachable through the browser helpers only.
        assert!(pane.body.is_browser());
        assert!(pane.terminal_mut().is_none());
        let lane = pane
            .browser_lane_mut()
            .expect("browser lane on a browser pane");
        assert!(lane.host.is_none());
        assert_eq!(lane.start_url, "https://example.test/");
        assert_eq!(lane.phase, cmux_webview2::BrowserPhase::Creating);
        // No page title yet (rows fall back to null until the snapshot
        // carries one).
        assert_eq!(pane.display_title(), None);
    }

    // ----- M6: the SSH verb (ADR-0014) -----

    #[test]
    fn ssh_spec_orders_the_open_ssh_argv() {
        // OpenSSH-standard ordering: port/identity/config options before
        // the destination, the remote command after it.
        let full = ssh_spec(
            "user@build.example",
            Some(2222),
            Some(r"C:\keys\id_ed25519"),
            &["IdentitiesOnly=yes".to_owned(), "BatchMode=yes".to_owned()],
            Some("cd /srv && tail -f log"),
        );
        assert_eq!(full.program(), "ssh");
        assert_eq!(
            full.args().iter().map(String::as_str).collect::<Vec<_>>(),
            [
                "-p",
                "2222",
                "-i",
                r"C:\keys\id_ed25519",
                "-o",
                "IdentitiesOnly=yes",
                "-o",
                "BatchMode=yes",
                "user@build.example",
                "cd /srv && tail -f log",
            ]
        );
        // The minimal invocation is just the destination.
        let bare = ssh_spec("user@build.example", None, None, &[], None);
        assert_eq!(
            bare.args().iter().map(String::as_str).collect::<Vec<_>>(),
            ["user@build.example"]
        );
    }

    #[test]
    fn pane_rows_repeat_the_surface_identity() {
        let row = pane_row(1, "p4", "w2", false);
        assert_eq!(row["index"], 1);
        assert_eq!(row["id"], "p4");
        assert_eq!(row["surface_ids"], serde_json::json!(["p4"]));
        assert_eq!(row["surface_count"], 1);
        assert_eq!(row["selected_surface_id"], "p4");
        assert_eq!(row["focused"], false);
    }

    // ----- M5.2: the browser automation plane -----

    #[test]
    fn automation_kinds_map_from_app_requests() {
        use cmux_ipc::AppRequest as Req;
        // The dedicated parser-covered verbs ride through as kinds with
        // the shared surface id prefix intact.
        assert_eq!(
            cdp_kind_of(&Req::BrowserSnapshot { surface_id: None }),
            Some((None, CdpKind::Snapshot))
        );
        assert_eq!(
            cdp_kind_of(&Req::BrowserEval {
                surface_id: Some("p4".to_owned()),
                expression: "2+3".to_owned(),
            }),
            Some((
                Some("p4".to_owned()),
                CdpKind::Eval {
                    expression: "2+3".to_owned(),
                }
            ))
        );
        assert_eq!(
            cdp_kind_of(&Req::BrowserClick {
                surface_id: None,
                target: ElementTarget::Ref("@e2".to_owned()),
            }),
            Some((
                None,
                CdpKind::ClickMouse {
                    target: ElementTarget::Ref("@e2".to_owned()),
                    click_count: 1,
                }
            ))
        );
        // Dblclick keeps the same handler with the doubled click count.
        assert_eq!(
            cdp_kind_of(&Req::BrowserDblclick {
                surface_id: None,
                target: ElementTarget::Ref("@e7".to_owned()),
            }),
            Some((
                None,
                CdpKind::ClickMouse {
                    target: ElementTarget::Ref("@e7".to_owned()),
                    click_count: 2,
                }
            ))
        );
        assert_eq!(
            cdp_kind_of(&Req::BrowserType {
                surface_id: None,
                target: ElementTarget::Ref("@e3".to_owned()),
                text: "hi".to_owned(),
            }),
            Some((
                None,
                CdpKind::TypeText {
                    target: ElementTarget::Ref("@e3".to_owned()),
                    text: "hi".to_owned(),
                }
            ))
        );
        // The sync verbs stay off the automation plane.
        assert_eq!(cdp_kind_of(&Req::BrowserUrlGet { surface_id: None }), None);
        assert_eq!(cdp_kind_of(&Req::Identify), None);
        assert_eq!(
            cdp_kind_of(&Req::BrowserGetTitle { surface_id: None }),
            None
        );
    }

    #[test]
    fn m53_kinds_map_from_app_requests() {
        use cmux_ipc::AppRequest as Req;
        let dotted = ElementTarget::Selector("#answer".to_owned());
        assert_eq!(
            cdp_kind_of(&Req::BrowserFill {
                surface_id: Some("p4".to_owned()),
                target: dotted.clone(),
                text: "hi".to_owned(),
            }),
            Some((
                Some("p4".to_owned()),
                CdpKind::ElementJs {
                    target: dotted.clone(),
                    js: ElementJs::Fill {
                        text: "hi".to_owned()
                    },
                }
            ))
        );
        assert_eq!(
            cdp_kind_of(&Req::BrowserCheck {
                surface_id: None,
                target: dotted.clone(),
                checked: true,
            }),
            Some((
                None,
                CdpKind::ElementJs {
                    target: dotted.clone(),
                    js: ElementJs::Check { checked: true },
                }
            ))
        );
        assert_eq!(
            cdp_kind_of(&Req::BrowserScroll {
                surface_id: None,
                dx: None,
                dy: Some(500.0),
            }),
            Some((None, CdpKind::ScrollBy { dx: 0.0, dy: 500.0 }))
        );
        assert_eq!(
            cdp_kind_of(&Req::BrowserScroll {
                surface_id: None,
                dx: Some(-2.0),
                dy: None,
            }),
            Some((None, CdpKind::ScrollBy { dx: -2.0, dy: 0.0 }))
        );
        assert_eq!(
            cdp_kind_of(&Req::BrowserGet {
                surface_id: None,
                target: dotted.clone(),
                field: GetField::Count,
                attr: None,
                property: None,
            }),
            Some((
                None,
                CdpKind::ElementJs {
                    target: dotted.clone(),
                    js: ElementJs::GetDom {
                        field: GetField::Count,
                        attr: None,
                        property: None,
                    },
                }
            ))
        );
        assert_eq!(
            cdp_kind_of(&Req::BrowserIs {
                surface_id: None,
                target: dotted.clone(),
                state: IsState::Visible,
            }),
            Some((
                None,
                CdpKind::ElementJs {
                    target: dotted.clone(),
                    js: ElementJs::IsState {
                        state: IsState::Visible
                    },
                }
            ))
        );
        assert_eq!(
            cdp_kind_of(&Req::BrowserScreenshot { surface_id: None }),
            Some((None, CdpKind::Screenshot))
        );
        // The shell-side focus verbs never arrive as CDP calls.
        assert_eq!(
            cdp_kind_of(&Req::BrowserFocusWebview { surface_id: None }),
            None
        );
        assert_eq!(
            cdp_kind_of(&Req::BrowserIsWebviewFocused { surface_id: None }),
            None
        );
    }

    #[test]
    fn m53_js_builders_embed_json_literals() {
        // The hostile pair: quotes/newline in both the selector and the
        // free text must ride as canonical JSON literals (never raw code).
        let selector = "input[data-x='a\"b\nthe-evil']";
        let text = "line\nwith \"quote\"";
        let selector_literal = serde_json::to_string(selector).unwrap();
        let text_literal = serde_json::to_string(text).unwrap();
        for built_on in [
            fill_evaluate_js(selector, text),
            check_evaluate_js(selector, true),
            select_evaluate_js(selector, text),
            scroll_into_view_evaluate_js(selector),
            dom_field_evaluate_js(selector, GetField::Text, None, None),
            dom_field_evaluate_js(selector, GetField::Html, None, None),
            dom_field_evaluate_js(selector, GetField::Value, None, None),
            dom_field_evaluate_js(selector, GetField::Attr, Some("data-q\"x"), None),
            dom_field_evaluate_js(selector, GetField::Box, None, None),
            dom_field_evaluate_js(selector, GetField::Styles, None, Some("color")),
            dom_field_evaluate_js(selector, GetField::Styles, None, None),
            count_evaluate_js(selector),
            is_state_evaluate_js(selector, IsState::Visible),
            is_state_evaluate_js(selector, IsState::Enabled),
            is_state_evaluate_js(selector, IsState::Checked),
        ] {
            assert!(built_on.contains(&selector_literal), "{built_on}");
        }
        assert!(fill_evaluate_js(selector, text).contains(&text_literal));
        // Every builder is a wrapped IIFE the page cannot escape into
        // (`count` is a bare matcher — asserted in the coverage test).
        for wrapped in [
            fill_evaluate_js(selector, text),
            check_evaluate_js(selector, false),
            select_evaluate_js(selector, "de"),
            scroll_into_view_evaluate_js(selector),
            scroll_evaluate_js(0.0, 500.0),
        ] {
            assert!(wrapped.starts_with("(function ()"), "{wrapped}");
        }
    }

    #[test]
    fn m53_getter_builders_cover_every_field() {
        // Count skips the element-null head (a 0 count is valid); the DOM
        // fields keep it.
        for field in [
            GetField::Text,
            GetField::Html,
            GetField::Value,
            GetField::Box,
            GetField::Styles,
        ] {
            let stood = dom_field_evaluate_js("#s", field, None, None);
            assert!(stood.contains("if (!el) { return null; }"), "{field:?}");
        }
        assert!(
            count_evaluate_js("#s").starts_with("document.querySelectorAll("),
            "count is a bare matcher"
        );
        assert_eq!(
            element_expression_of(
                &ElementJs::GetDom {
                    field: GetField::Count,
                    attr: None,
                    property: None,
                },
                "#s"
            ),
            count_evaluate_js("#s")
        );
        // Styles read one property when named, the default set otherwise.
        let one = dom_field_evaluate_js("#s", GetField::Styles, None, Some("color"));
        assert!(one.contains("[\"color\"]"), "{one}");
        let default = dom_field_evaluate_js("#s", GetField::Styles, None, None);
        assert!(default.contains("[\"display\",\"visibility\""), "{default}");
        assert!(
            dom_field_evaluate_js("#s", GetField::Box, None, None)
                .contains("getBoundingClientRect"),
            "box reads the live rect"
        );
    }

    #[test]
    fn m53_action_parser_maps_sentinels_to_problems() {
        let ok = serde_json::json!({ "result": { "value": true } });
        assert!(parse_element_action(&ok, &ElementJs::Focus).is_ok());
        let cases: [(&str, &str); 4] = [
            ("not-checkbox", "not a checkbox"),
            ("not-select", "not a <select>"),
            ("no-option", "no <option>"),
            ("\"whatever\"", "gone"),
        ];
        for (sentinel, expected) in cases {
            let raw = serde_json::json!({ "result": { "value": sentinel } });
            let problem = parse_element_action(&raw, &ElementJs::Check { checked: true })
                .err()
                .unwrap();
            assert_eq!(problem.code, "invalid_state", "{sentinel}");
            assert!(problem.message.contains(expected), "{problem:?}");
        }
        // A null result (element vanished) is the gone-state too.
        let raw = serde_json::json!({ "result": { "value": null } });
        let problem = parse_element_action(
            &raw,
            &ElementJs::Fill {
                text: "t".to_owned(),
            },
        )
        .err()
        .unwrap();
        assert!(problem.message.contains("gone"), "{problem:?}");
    }

    #[test]
    fn m53_state_probes_read_missing_elements_as_false() {
        let raw = serde_json::json!({ "result": { "value": null } });
        for state in [IsState::Visible, IsState::Enabled, IsState::Checked] {
            assert!(!parse_is_state(&raw, state), "{state:?}");
        }
        let visible = serde_json::json!({ "result": { "value": { "visible": true } } });
        assert!(parse_is_state(&visible, IsState::Visible));
        assert!(!parse_is_state(&visible, IsState::Checked));
        let enabled = serde_json::json!({ "result": { "value": { "enabled": false } } });
        assert!(!parse_is_state(&enabled, IsState::Enabled));
        // A probe reply whose value is missing entirely reads as false
        // (never a panic, never an unknown JSON error).
        let hollow = serde_json::json!({});
        assert!(!parse_is_state(&hollow, IsState::Enabled));
    }

    #[test]
    fn m53_scroll_reply_defaults_missing_offsets() {
        let (x, y) = parse_scroll_reply(
            &serde_json::json!({ "result": { "value": { "x": 0.0, "y": 512.5 } } }),
        );
        assert_eq!(x, 0.0);
        assert_eq!(y, 512.5);
        let (x, y) = parse_scroll_reply(&serde_json::json!({}));
        assert_eq!((x, y), (0.0, 0.0));
    }

    #[test]
    fn screenshot_stream_helpers_bound_the_lane() {
        // base64 → bytes; missing data and malformed base64 are structured
        // invalid_state (never a panic on the frame thread).
        let four_bytes = serde_json::json!({ "data": "AQIDBA==" });
        let decoded = screenshot_bytes_of(&four_bytes).unwrap();
        assert_eq!(decoded, vec![1, 2, 3, 4]);
        let missing = serde_json::json!({});
        let problem = screenshot_bytes_of(&missing).err().unwrap();
        assert_eq!(problem.code, "invalid_state");
        assert!(problem.message.contains("no PNG data"));
        let broken = serde_json::json!({ "data": "+/not-base64" });
        let problem = screenshot_bytes_of(&broken).err().unwrap();
        assert!(problem.message.contains("base64"), "{problem:?}");
        // The filename fragment collapses to safe characters only.
        assert_eq!(screenshot_surface_fragment("pane-3"), "pane-3");
        assert_eq!(screenshot_surface_fragment("a/b\\c?d#e"), "abcde");
        assert_eq!(screenshot_surface_fragment("///"), "pane");
        assert_eq!(
            screenshot_surface_fragment(&"x".repeat(80)),
            "x".repeat(40),
            "the name stays bounded"
        );
    }

    #[test]
    fn automation_decode_maps_failures_to_problems() {
        // A COM-level failure is invalid_state (the surface stays usable).
        let problem = decode_cdp_outcome(&Err("COM went away".to_owned()))
            .err()
            .unwrap();
        assert_eq!(problem.code, "invalid_state");
        assert!(problem.message.contains("COM went away"), "{problem:?}");

        // A non-JSON reply is a problem, never a panic.
        let problem = decode_cdp_outcome(&Ok("<html>".to_owned())).err().unwrap();
        assert_eq!(problem.code, "invalid_state");
        assert!(problem.message.contains("valid JSON"), "{problem:?}");

        // The protocol reports page exceptions under exceptionDetails.
        let problem = decode_cdp_outcome(&Ok(
            r#"{"exceptionDetails":{"exception":{"description":"ReferenceError: bogus is not defined"},"text":"Uncaught"}}"#.to_owned(),
        ))
        .err()
        .unwrap();
        assert!(
            problem.message.contains("ReferenceError: bogus"),
            "{problem:?}"
        );

        // A clean evaluate payload decodes as the response object.
        let raw = decode_cdp_outcome(&Ok(r#"{"result":{"type":"number","value":5}}"#.to_owned()))
            .unwrap();
        assert_eq!(evaluate_value_of(&raw), serde_json::json!(5));
    }

    #[test]
    fn automation_center_parse_handles_and_rejects_the_gone_case() {
        let ok =
            serde_json::json!({ "result": { "type": "object", "value": { "x": 1.5, "y": 2.0 } } });
        assert_eq!(parse_center_cdp(&ok).unwrap(), (1.5, 2.0));

        // Element vanished since the snapshot: the walker's null.
        let gone =
            serde_json::json!({ "result": { "type": "object", "subtype": "null", "value": null } });
        assert_eq!(parse_center_cdp(&gone).err().unwrap().code, "invalid_state");

        // A shapeless reply (e.g. undefined) is equally the stale case.
        let shapeless = serde_json::json!({ "result": { "type": "undefined" } });
        assert_eq!(
            parse_center_cdp(&shapeless).err().unwrap().code,
            "invalid_state"
        );
    }

    #[test]
    fn automation_js_builders_json_encode_the_selector() {
        // A selector with a double quote must survive as a JS string
        // literal (JSON-encoding is the injection-safe encoding here).
        let selector = r#"button[title="say \"hi\""]"#;
        let center = center_evaluate_js(selector);
        assert!(center.starts_with("(function () {"), "{center}");
        assert!(center.contains("document.querySelector("), "{center}");
        assert!(
            center.contains(r#""button[title=\"say \\\"hi\\\"\"]""#),
            "the selector must be one JSON-encoded literal: {center}"
        );
        assert!(center.contains("getBoundingClientRect()"), "{center}");
        assert!(center.contains("return null;"), "{center} (gone guard)");

        let focus = focus_evaluate_js(selector);
        assert!(focus.contains("el.focus();"), "{focus}");
        assert!(focus.contains("return null;"), "{focus} (gone guard)");
    }

    #[test]
    fn m54_key_table_stays_desktop_honest() {
        let table = key_table();
        // Every VK stays inside Windows' virtual-key range and every row
        // carries a non-empty printed/figure name.
        for row in table {
            assert!(
                (1..=0xFF).contains(&(row.vk as u32)),
                "vk out of range: {} ({row:?})",
                row.name
            );
            assert!(!row.name.is_empty());
            assert!(!row.label.is_empty());
            // Printing rows print their label; cursor rows produce none.
            if let Some(text) = row.text {
                assert!(!text.is_empty(), "empty text on a printing row: {row:?}");
            } else if row.shift.is_some() {
                panic!("a shifted row without text makes no sense: {row:?}");
            }
        }
        // Spot rows (the desktop facts the vocabulary hangs on).
        let find = |name: &str| {
            table
                .iter()
                .find(|row| row.name == name)
                .unwrap_or_else(|| panic!("{name} is in the table"))
        };
        assert_eq!(find("Enter").vk, 0x0D);
        assert_eq!(find("Enter").text, Some("\r"), "Enter's char event is CR");
        assert_eq!(find("Tab").vk, 0x09);
        assert_eq!(find("Escape").vk, 0x1B);
        assert_eq!(find("Backspace").vk, 0x08);
        assert_eq!(find("ArrowLeft").vk, 0x25);
        assert_eq!(find("ArrowDown").vk, 0x28);
        assert_eq!(find("F1").vk, 0x70);
        assert_eq!(find("F12").vk, 0x7B);
        assert_eq!(find("Meta").vk, 0x5B);
        let semicolon = find("Semicolon");
        assert_eq!(semicolon.vk, 0xBA);
        assert_eq!(semicolon.label, ";");
        assert_eq!(semicolon.shift, Some(":"));
        let five = find("5");
        assert_eq!(five.vk, 0x35);
        assert_eq!(five.shift, Some("%"));
        let k_row = find("k");
        assert_eq!(k_row.vk, 0x4B);
        assert_eq!(k_row.shift, Some("K"));
    }

    #[test]
    fn m54_key_plans_resolve_the_documented_vocabulary() {
        // Named keys, case-insensitive + alias spellings.
        for spelling in ["Enter", "enter", "return"] {
            let plan = key_plan_of(spelling).unwrap();
            assert_eq!(
                plan,
                KeyPlan {
                    key: "Enter".to_owned(),
                    vk: 0x0D,
                    modifiers: 0,
                    text: Some("\r".to_owned()),
                },
                "{spelling}"
            );
        }
        // Plain letters/digits print their own character.
        assert_eq!(key_plan_of("a").unwrap().text.as_deref(), Some("a"));
        assert_eq!(key_plan_of("a").unwrap().vk, 0x41);
        assert_eq!(key_plan_of("7").unwrap().vk, 0x37);
        assert_eq!(key_plan_of("7").unwrap().text.as_deref(), Some("7"));
        // An uppercase single letter IS Shift+letter (Playwright's grammar).
        let plan = key_plan_of("A").unwrap();
        assert_eq!(plan.vk, 0x41);
        assert_eq!(plan.modifiers, KEY_MODIFIER_SHIFT);
        assert_eq!(plan.text.as_deref(), Some("A"));
        // Modifier chains: every accepted spelling, any order.
        let plan = key_plan_of("Control+a").unwrap();
        assert_eq!(plan.modifiers, KEY_MODIFIER_CONTROL);
        // A Ctrl/Alt/Meta chord never carries char text (the char would
        // mis-land); the editing command works off the raw event.
        assert_eq!(plan.text, None);
        assert_eq!(
            key_plan_of("ctrl+k").unwrap().modifiers,
            KEY_MODIFIER_CONTROL
        );
        assert_eq!(
            key_plan_of("cmd+Enter").unwrap().modifiers,
            KEY_MODIFIER_META
        );
        assert_eq!(
            key_plan_of("alt+ArrowDown").unwrap().modifiers,
            KEY_MODIFIER_ALT
        );
        let plan = key_plan_of("Meta+Shift+K").unwrap();
        assert_eq!(plan.modifiers, KEY_MODIFIER_META | KEY_MODIFIER_SHIFT);
        // Any Ctrl/Alt/Meta engagement suppresses the char synthesis (the
        // Chromium-honest mapping — their chars never land in the page);
        // the editing action still runs off the raw event.
        assert_eq!(plan.text, None);
        // A Shift-only chain still carries its shifted char.
        assert_eq!(key_plan_of("Shift+K").unwrap().text.as_deref(), Some("K"));
        // Shift chains pick the shifted partner.
        assert_eq!(key_plan_of("Shift+=").unwrap().text.as_deref(), Some("+"));
        // Lone shifted punctuation spells itself ('+' bypasses split).
        let plan = key_plan_of("+").unwrap();
        assert_eq!(plan.key, "=");
        assert_eq!(plan.modifiers, KEY_MODIFIER_SHIFT);
        assert_eq!(plan.text.as_deref(), Some("+"));
        assert_eq!(key_plan_of("%").unwrap().text.as_deref(), Some("%"));
        // Whitespace and the alias family.
        assert_eq!(key_plan_of(" ").unwrap().key, " ");
        assert_eq!(key_plan_of("esc").unwrap().key, "Escape");
        assert_eq!(key_plan_of("spacebar").unwrap().key, " ");
        assert_eq!(key_plan_of("down").unwrap().key, "ArrowDown");

        // Unknown vocabulary answers the structured listing.
        let problem = key_plan_of("Frobnicate").err().unwrap();
        assert_eq!(problem.code, "invalid_params");
        assert!(problem.message.contains("not a known key"), "{problem:?}");
        // The alias family resolves too; a true non-modifier is structured.
        assert_eq!(
            key_plan_of("Option+Enter").unwrap().modifiers,
            KEY_MODIFIER_ALT
        );
        let problem = key_plan_of("Q+Enter").err().unwrap();
        assert!(problem.message.contains("modifier"), "{problem:?}");
        let problem = key_plan_of("Control+").err().unwrap();
        assert!(problem.message.contains("empty key part"), "{problem:?}");
        let problem = key_plan_of("").err().unwrap();
        assert!(problem.message.contains("empty"), "{problem:?}");
    }

    #[test]
    fn m54_key_events_shape_the_chromium_mapping() {
        // A printing key down = keyDown + text; its half never carries it.
        let plan = key_plan_of("a").unwrap();
        let down = key_dispatch_params(&plan, true);
        assert_eq!(down["type"], "keyDown", "{down}");
        assert_eq!(down["text"], "a");
        assert_eq!(down["windowsVirtualKeyCode"], 0x41);
        assert_eq!(down["modifiers"], 0);
        let up = key_dispatch_params(&plan, false);
        assert_eq!(up["type"], "keyUp", "{up}");
        assert!(up.get("text").is_none(), "the up half has no char: {up}");

        // A cursor/editing key down = rawKeyDown (no char synthesis).
        let plan = key_plan_of("Backspace").unwrap();
        let down = key_dispatch_params(&plan, true);
        assert_eq!(down["type"], "rawKeyDown", "{down}");
        assert_eq!(down["windowsVirtualKeyCode"], 0x08);
        assert!(down.get("text").is_none());

        // Chord modifiers ride the event bitmap.
        let plan = key_plan_of("Control+Shift+k").unwrap();
        let down = key_dispatch_params(&plan, true);
        assert_eq!(down["modifiers"], KEY_MODIFIER_CONTROL | KEY_MODIFIER_SHIFT);
        assert_eq!(down["key"], "k");
    }

    #[test]
    fn m54_wait_builder_rows_the_documented_slots() {
        let full = wait_evaluate_js(&WaitPoll {
            selector: Some("#out".to_owned()),
            text_contains: Some("composed:hi".to_owned()),
            url_contains: Some("form".to_owned()),
            load: Some(LoadState::Load),
            function: Some("window.__x === 1".to_owned()),
        });
        assert!(
            full.contains("!!document.querySelector(\"#out\")"),
            "{full}"
        );
        assert!(
            full.contains("String((document.body || document.documentElement).textContent)"),
            "{full}"
        );
        assert!(full.contains("indexOf(\"composed:hi\") !== -1"), "{full}");
        assert!(
            full.contains("location.href.indexOf(\"form\") !== -1"),
            "{full}"
        );
        assert!(
            full.contains("document.readyState === 'complete'"),
            "{full}"
        );
        assert!(full.contains("!!(window.__x === 1)"), "{full}");
        assert!(full.starts_with("(function () { return ["), "{full}");
        // Exactly one slot per given predicate, in the documented order
        // (the parser indexes by that order).
        let one = wait_evaluate_js(&WaitPoll {
            selector: Some("#a".to_owned()),
            text_contains: None,
            url_contains: None,
            load: None,
            function: None,
        });
        assert_eq!(
            one, "(function () { return [!!document.querySelector(\"#a\")]; })()",
            "{one}"
        );
        let dcl = wait_evaluate_js(&WaitPoll {
            selector: None,
            text_contains: None,
            url_contains: None,
            load: Some(LoadState::DomContentLoaded),
            function: None,
        });
        assert!(dcl.contains("document.readyState !== 'loading'"), "{dcl}");
        // The free-text predicates ride as JSON literals, never raw code.
        let hostile = wait_evaluate_js(&WaitPoll {
            selector: Some("#a\"b".to_owned()),
            text_contains: Some("inj'];evil()".to_owned()),
            url_contains: None,
            load: None,
            function: None,
        });
        assert!(hostile.contains("#a\\\"b\""), "{hostile}");
        assert!(hostile.contains("inj'];evil()"), "{hostile}");
    }

    #[test]
    fn m54_wait_slots_parse_truthiness_and_shapes() {
        let complete = WaitPoll {
            selector: Some("#a".to_owned()),
            text_contains: None,
            url_contains: Some("form".to_owned()),
            load: None,
            function: Some("true".to_owned()),
        };
        let raw = |value: serde_json::Value| serde_json::json!({ "result": { "value": value } });

        // All-true and one-false drive the poll loop's branches.
        let slots = serde_json::json!([true, true, true]);
        assert!(parse_wait_slots(&raw(slots), &complete).unwrap());
        let slots = serde_json::json!([true, false, 1]);
        assert!(!parse_wait_slots(&raw(slots), &complete).unwrap());

        // The function slot is truthy-tested like JS: numbers, strings,
        // null.
        let slots = serde_json::json!([true, true, 1]);
        assert!(parse_wait_slots(&raw(slots), &complete).unwrap());
        let slots = serde_json::json!([true, true, 0]);
        assert!(!parse_wait_slots(&raw(slots), &complete).unwrap());
        let slots = serde_json::json!([true, true, ""]);
        assert!(!parse_wait_slots(&raw(slots), &complete).unwrap());
        let slots = serde_json::json!([true, true, null]);
        assert!(!parse_wait_slots(&raw(slots), &complete).unwrap());
        assert!(!value_is_truthy(&serde_json::json!(0)));
        assert!(!value_is_truthy(&serde_json::json!("")));
        assert!(!value_is_truthy(&serde_json::Value::Null));
        assert!(value_is_truthy(&serde_json::json!("x")));
        assert!(value_is_truthy(&serde_json::json!({})));
        assert!(value_is_truthy(&serde_json::json!([0])));

        // Malformed replies fail fast instead of spinning to the deadline:
        // non-boolean strict slots, missing slots, extra slots, and a
        // non-array evaluate altogether.
        let slots = serde_json::json!([true, "form", true]);
        let problem = parse_wait_slots(&raw(slots), &complete).err().unwrap();
        assert_eq!(problem.code, "invalid_state", "{problem:?}");
        assert!(problem.message.contains("url_contains"), "{problem:?}");
        let slots = serde_json::json!([true]);
        let problem = parse_wait_slots(&raw(slots), &complete).err().unwrap();
        assert!(problem.message.contains("url_contains"), "{problem:?}");
        let slots = serde_json::json!([true, true]);
        let problem = parse_wait_slots(&raw(slots), &complete).err().unwrap();
        assert!(problem.message.contains("function"), "{problem:?}");
        let slots = serde_json::json!([true, true, true, true]);
        let problem = parse_wait_slots(&raw(slots), &complete).err().unwrap();
        assert!(problem.message.contains("slot count"), "{problem:?}");
        let shapeless = serde_json::json!({ "result": { "value": 5 } });
        let problem = parse_wait_slots(&raw(shapeless), &complete).err().unwrap();
        assert!(problem.message.contains("slots array"), "{problem:?}");
    }

    #[test]
    fn m54_kinds_map_from_app_requests() {
        use cmux_ipc::AppRequest as Req;
        assert_eq!(
            cdp_kind_of(&Req::BrowserKey {
                surface_id: Some("p4".to_owned()),
                half: KeyHalf::Press,
                key: "enter".to_owned(),
            }),
            Some((
                Some("p4".to_owned()),
                CdpKind::Key {
                    half: KeyHalf::Press,
                    token: "enter".to_owned(),
                }
            ))
        );
        assert_eq!(
            cdp_kind_of(&Req::BrowserWait {
                surface_id: None,
                selector: Some("#out".to_owned()),
                text_contains: None,
                url_contains: None,
                load_state: Some(LoadState::Load),
                function: None,
                timeout_ms: 1_500,
            }),
            Some((
                None,
                CdpKind::Wait {
                    poll: WaitPoll {
                        selector: Some("#out".to_owned()),
                        text_contains: None,
                        url_contains: None,
                        load: Some(LoadState::Load),
                        function: None,
                    },
                    timeout_ms: 1_500,
                }
            ))
        );
    }

    #[test]
    fn resolve_config_command_enforces_the_run_shape() {
        use cmux_config::CommandDefinition;

        let base = CommandDefinition {
            keywords: Vec::new(),
            restart: None,
            ..CommandDefinition::default()
        };
        let commands = vec![
            CommandDefinition {
                name: "build".to_owned(),
                description: Some("cargo check".to_owned()),
                command: Some("cargo check".to_owned()),
                ..base.clone()
            },
            CommandDefinition {
                name: "guarded".to_owned(),
                confirm: true,
                command: Some("shutdown /r".to_owned()),
                ..base.clone()
            },
            CommandDefinition {
                name: "space".to_owned(),
                workspace: Some(serde_json::json!({})),
                ..base.clone()
            },
        ];

        // A plain command resolves without any acknowledgement.
        assert!(resolve_config_command(&commands, "build", false).is_ok());

        // Unknown names are a structured not-found, not a fallback.
        let problem = resolve_config_command(&commands, "nope", false).unwrap_err();
        assert_eq!(problem.code, "not_found");
        assert!(problem.message.contains("not defined in the config file"));

        // Workspace launches are the recorded residual.
        let problem = resolve_config_command(&commands, "space", true).unwrap_err();
        assert_eq!(problem.code, "invalid_state");
        assert!(problem.message.contains("recorded residual"), "{problem:?}");

        // confirm: true needs the acknowledgement; the message names the fix.
        let problem = resolve_config_command(&commands, "guarded", false).unwrap_err();
        assert_eq!(problem.code, "invalid_state");
        assert!(problem.message.contains("--yes"), "{problem:?}");
        assert!(resolve_config_command(&commands, "guarded", true).is_ok());
    }

    #[test]
    fn command_list_rows_shape_flavors_and_sanitized_text() {
        use cmux_config::CommandDefinition;

        let base = CommandDefinition {
            keywords: Vec::new(),
            restart: None,
            ..CommandDefinition::default()
        };
        let commands = vec![
            CommandDefinition {
                name: "build".to_owned(),
                description: Some("check".to_owned()),
                command: Some("\u{202A}cargo check\u{202C}".to_owned()),
                ..base.clone()
            },
            CommandDefinition {
                name: "space".to_owned(),
                workspace: Some(serde_json::json!({})),
                ..base.clone()
            },
        ];

        let rows = command_list_json(&commands);
        let rows: Vec<serde_json::Value> = serde_json::from_value(rows).unwrap();
        assert_eq!(rows.len(), 2);
        // Terminal row: sanitized command text, confirm false.
        assert_eq!(rows[0]["kind"], "terminal");
        assert_eq!(rows[0]["command"], "cargo check");
        assert_eq!(rows[0]["confirm"], false);
        assert_eq!(rows[0]["description"], "check");
        // Workspace row: no command text leaks up.
        assert_eq!(rows[1]["kind"], "workspace");
        assert!(rows[1]["command"].is_null());
    }

    #[test]
    fn broken_command_entries_fail_the_config_load() {
        // An invalid entry aborts the whole read (the verbs then see
        // zero commands - the startup path falls back to defaults).
        let broken = std::env::temp_dir().join(format!(
            "cmux-m6-commands-broken-{}.json",
            std::process::id()
        ));
        std::fs::write(&broken, b"{ \"commands\": [ { \"name\": \"\" } ] }").unwrap();
        let error = Config::load_file(&broken).unwrap_err();
        assert!(
            error.to_string().contains("Command name must not be blank"),
            "{error}"
        );
        std::fs::remove_file(&broken).ok();
    }
}
