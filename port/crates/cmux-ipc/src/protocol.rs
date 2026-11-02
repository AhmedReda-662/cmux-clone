//! v2-style JSON-lines protocol types shared by the pipe server and the
//! CLI client (ADR-0005: "Protocol: unchanged v2 JSON-lines"). The served
//! surface per slice:
//!
//! - `ping` (V1 name) / `system.ping` (V2 name) — availability probe
//!   (reply `{"ok":true,"result":{"pong":true}}`);
//! - `agent.hook.enqueue` — admit one hook event (M3.3; upstream
//!   `AgentHookDeliveryPolicy` budgets: payload ≤ 64 KiB; the context is a
//!   filtered environment map);
//! - `system.capabilities` — static discovery reply (M4.1);
//! - the M4.1 app-plane automation subset (`system.identify`,
//!   `workspace.*`, `surface.list/send_text/read_text`, `pane.list`),
//!   validated here and answered by the app on the UI thread (`AppRequest`).
//!
//! Wire shape mirrors upstream as closely as the port can express it:
//! request `{"id","method","params"}`, response
//! `{"id","ok",("result"|("error":{"code","message","action","data"}}`).
//! Every response id echoes the request id so a hook process can correlate
//! and the CLI can fail-open without parsing bodies it did not ask for.

use serde_json::Value;

/// Upstream `AgentHookDeliveryPolicy` payload cap (64 KiB).
pub const MAX_PAYLOAD_BYTES: usize = 64 * 1024;
/// Bounded request line: upstream reads ≤ 1 MiB of hook stdin; the wire
/// request (payload + envelope) gets the same ceiling.
pub const MAX_REQUEST_BYTES: usize = 1024 * 1024;

/// The pre-M5 method table (the M4.1 static plane plus its app-plane
/// subset, frozen at the M4 close). M5's browser families live in
/// [`APP_METHODS`]; `capabilities_result` serves the union of both tables
/// (sorted) so discovery stays truthful as the app plane grows. Anything
/// outside both is rejected with a structured `unknown_method` error so
/// clients can fail open visibly.
pub const KNOWN_METHODS: [&str; 14] = [
    "ping",
    "system.ping",
    "system.capabilities",
    "agent.hook.enqueue",
    "auth.login",
    "system.identify",
    "workspace.list",
    "workspace.current",
    "workspace.select",
    "workspace.create",
    "surface.list",
    "pane.list",
    "surface.send_text",
    "surface.read_text",
];

/// Agents whose `agent.hook.enqueue` events are admitted in this slice.
/// `codex` landed with slice A, `claude` with M3.3b; the set grows with
/// each installer slice and the server rejects others outright (an
/// explicit error beats a silently-lost event).
pub const KNOWN_AGENTS: [&str; 2] = ["codex", "claude"];

/// Environment keys copied from the hook process into the request, the
/// port subset of upstream's `queuedAgentHookDataEnvironmentKeys`
/// (`CMUXCLI+AgentHookAdmission.swift`). Values are trimmed to single
/// lines and capped, and unknown keys are dropped — an allow-list, not
/// free-form forwarding (manifest §11: never act on unvalidated IPC).
pub const ROUTED_ENV_KEYS: [&str; 7] = [
    "PWD",
    "CMUX_SURFACE_ID",
    "CMUX_WORKSPACE_ID",
    "CMUX_STABLE_SURFACE_ID",
    "CMUX_AGENT_HOOK_ROUTE_SNAPSHOT",
    // The per-agent PID key is routed dynamically
    // (`CMUX_<AGENT>_PID`), see `routed_env_for`.
    "CMUX_AGENT_HOOK_INVOKER_PID",
    // The port fills `agent_pid` itself (below).
    "agent_pid",
];

/// Per-agent PID env key (upstream `agentHookPIDEnvironmentVariable`).
pub fn agent_pid_env_key(agent: &str) -> String {
    format!("CMUX_{}_PID", agent.to_uppercase().replace('-', "_"))
}

/// Builds the routed environment for a request: the allow-listed keys only,
/// values single-line and ≤ 256 chars (upstream truncates bounded fields the
/// same way). `agent_pid` comes either from `CMUX_<AGENT>_PID` or — when the
/// hook has no export — the invoking process's parent PID supplied by the
/// client (upstream: `agent_pid=${CMUX_<AGENT>_PID:-${PPID:-}}`).
pub fn routed_env_for(
    agent: &str,
    process_env: impl Fn(&str) -> Option<String>,
    fallback_pid: Option<u32>,
) -> serde_json::Map<String, Value> {
    let mut out = serde_json::Map::new();
    let mut pid: Option<String> = None;
    let pid_key = agent_pid_env_key(agent);
    for key in ROUTED_ENV_KEYS {
        if key == "agent_pid" {
            continue;
        }
        if let Some(value) = sanitized_env_value(process_env(key)) {
            if key == pid_key {
                pid = Some(value);
            } else {
                out.insert(key.to_owned(), Value::String(value));
            }
        }
    }
    // Fallback PID applies only when the agent's own key is unset.
    if pid.is_none() {
        if let Some(value) = process_env(&pid_key).and_then(|v| sanitized_env_value(Some(v))) {
            pid = Some(value);
        }
    }
    if pid.is_none() {
        if let Some(raw) = fallback_pid {
            pid = Some(raw.to_string());
        }
    }
    if let Some(pid) = pid {
        out.insert("agent_pid".to_owned(), Value::String(pid));
    }
    out
}

fn sanitized_env_value(raw: Option<String>) -> Option<String> {
    let raw = raw?;
    let single_line: String = raw
        .chars()
        .map(|c| {
            if c == '\n' || c == '\r' || c == '\0' {
                ' '
            } else {
                c
            }
        })
        .collect();
    let single_line = single_line.trim();
    if single_line.is_empty() {
        return None;
    }
    // take(256) yields whole chars, so the result stays valid UTF-8.
    Some(single_line.chars().take(256).collect())
}

/// Validates one `agent.hook.enqueue` params object. Returns the normalized
/// pieces the app needs, or an error message naming the violated budget.
pub fn validate_enqueue_params(params: &Value) -> Result<ValidatedEnqueue, String> {
    let obj = params
        .as_object()
        .ok_or_else(|| "params must be an object".to_owned())?;
    let agent = obj
        .get("agent")
        .and_then(Value::as_str)
        .ok_or_else(|| "params.agent must be a string".to_owned())?;
    if !KNOWN_AGENTS.contains(&agent) {
        return Err(format!(
            "agent '{agent}' is not admit-able yet (known: {})",
            KNOWN_AGENTS.join(", ")
        ));
    }
    let subcommand = obj
        .get("subcommand")
        .and_then(Value::as_str)
        .ok_or_else(|| "params.subcommand must be a string".to_owned())?
        // Upstream normalizes hook subcommands by lowercasing
        // (`supportsQueuedDelivery`); mirror instead of rejecting case
        // variance from the agent runtime.
        .to_ascii_lowercase();
    if subcommand.is_empty()
        || subcommand.len() > 64
        || !subcommand
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return Err("params.subcommand must be [a-z0-9-]{1,64}".to_owned());
    }
    let payload = obj.get("payload").cloned().unwrap_or(Value::Null);
    let payload_bytes = match &payload {
        Value::Null => 0,
        Value::String(s) => s.len(),
        other => serde_json::to_vec(other).unwrap_or_default().len(),
    };
    if payload_bytes > MAX_PAYLOAD_BYTES {
        return Err(format!(
            "payload is {payload_bytes} bytes; the admission budget is {MAX_PAYLOAD_BYTES}"
        ));
    }
    // Environment objects pass through as already-filtered maps from the
    // CLI; a malicious direct client's map is re-filtered on the server.
    let pid_key = agent_pid_env_key(agent);
    let environment = match obj.get("environment") {
        Some(Value::Object(map)) => {
            let mut cleaned = serde_json::Map::new();
            for (key, value) in map {
                if key.as_str() == "agent_pid" || key.as_str() == pid_key {
                    continue;
                }
                if ROUTED_ENV_KEYS.contains(&key.as_str()) {
                    if let Value::String(s) = value {
                        if let Some(sanitized) = sanitized_env_value(Some(s.clone())) {
                            cleaned.insert(key.clone(), Value::String(sanitized));
                        }
                    }
                }
            }
            // Keep only upstream-routable routing/ID values; drop PWD
            // duplicates etc. implicitly by rebuilding through route keys.
            let mut routed = serde_json::Map::new();
            for key in ROUTED_ENV_KEYS {
                if let Some(value) = cleaned.get(key) {
                    routed.insert(key.to_owned(), value.clone());
                }
            }
            routed
        }
        Some(Value::Null) | None => serde_json::Map::new(),
        Some(_) => return Err("params.environment must be an object".to_owned()),
    };
    Ok(ValidatedEnqueue {
        agent: agent.to_owned(),
        subcommand: subcommand.to_owned(),
        payload,
        environment,
    })
}

#[derive(Debug, Clone, PartialEq)]
pub struct ValidatedEnqueue {
    pub agent: String,
    pub subcommand: String,
    pub payload: Value,
    pub environment: serde_json::Map<String, Value>,
}

// ---------------------------------------------------------------------------
// M4.1: the app-plane automation surface (ADR-0005 Amendment 2)
// ---------------------------------------------------------------------------

/// A structured app-plane rejection; upstream error shape
/// `{error:{code,message,action}}` mirrored as fields. `invalid_params`
/// keeps the code name the enqueue path already uses, so client-side
/// handling stays uniform.
#[derive(Debug, Clone, PartialEq)]
pub struct RequestProblem {
    pub code: &'static str,
    pub message: String,
    pub action: &'static str,
}

impl RequestProblem {
    /// A parameter violated its shape/budget; the client fixes the request.
    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self {
            code: "invalid_params",
            message: message.into(),
            action: "fix the parameters",
        }
    }

    /// The named id does not exist right now (closed pane, unknown
    /// workspace…). Recoverable: list and retry with a live id.
    pub fn not_found(message: impl Into<String>) -> Self {
        Self {
            code: "not_found",
            message: message.into(),
            action: "list workspaces or surfaces, then retry with a listed id",
        }
    }

    /// The named object exists but cannot serve the verb (e.g. a
    /// browser verb on a terminal surface); the caller picks a
    /// compatible target instead (upstream's `invalidState`).
    pub fn invalid_state(message: impl Into<String>) -> Self {
        Self {
            code: "invalid_state",
            message: message.into(),
            action: "retry with a compatible target",
        }
    }

    /// A terminal/model action failed locally (write/read error);
    /// recoverable by retrying.
    pub fn io(message: impl Into<String>) -> Self {
        Self {
            code: "io_error",
            message: message.into(),
            action: "retry",
        }
    }

    /// The app did not answer within the reply budget (busy/quit window).
    pub fn app_timeout() -> Self {
        Self {
            code: "timeout",
            message: "the app did not answer within the reply budget".to_owned(),
            action: "retry",
        }
    }

    /// A `browser.wait` deadline elapsed without its predicate(s) becoming
    /// true (upstream: the app handler reports the elapsed wait; the port's
    /// per-call reply budget covers the deadline plus headroom so this
    /// never collapses into the generic envelope).
    pub fn wait_timeout(timeout_ms: u32) -> Self {
        Self {
            code: "timeout",
            message: format!(
                "the wait deadline ({timeout_ms} ms) elapsed before the predicate(s) became true"
            ),
            action: "retry with a longer timeout or a weaker predicate",
        }
    }
}

/// Serializes a problem as a full error envelope (server-side callers).
pub fn problem_error(id: &Value, problem: &RequestProblem) -> String {
    error(id, problem.code, &problem.message, problem.action)
}

/// The app-plane methods (M4.1 subset of upstream's v2 control surface,
/// grown by the M5 browser families and the M6 custom-command verbs).
/// Every entry is parsed by
/// `parse_app_request` and executed on the app's UI thread; the server
/// thread only forwards.
pub const APP_METHODS: [&str; 50] = [
    "system.identify",
    "workspace.list",
    "workspace.current",
    "workspace.select",
    "workspace.create",
    "surface.list",
    "pane.list",
    "surface.send_text",
    "surface.read_text",
    // Lane 5: the agent activity stream pane (the CLI's `cmux agent
    // open`; upstream has no such method - recorded port extension).
    "agent.open_split",
    "browser.open_split",
    "browser.navigate",
    "browser.back",
    "browser.forward",
    "browser.reload",
    "browser.url.get",
    "browser.snapshot",
    "browser.eval",
    "browser.click",
    "browser.dblclick",
    "browser.type",
    // M5.3: the observation + form/scroll family (uniform one-evaluate
    // verbs plus the two shell-side focus checks and the screenshot lane).
    "browser.get.title",
    "browser.get.text",
    "browser.get.html",
    "browser.get.value",
    "browser.get.attr",
    "browser.get.count",
    "browser.get.box",
    "browser.get.styles",
    "browser.is.visible",
    "browser.is.enabled",
    "browser.is.checked",
    "browser.fill",
    "browser.focus",
    "browser.check",
    "browser.uncheck",
    "browser.select",
    "browser.hover",
    "browser.scroll",
    "browser.scroll_into_view",
    "browser.screenshot",
    "browser.focus_webview",
    "browser.is_webview_focused",
    // M5.4: the key family (focused-element key synthesis, no selector on
    // the wire) and the wait family (predicate polling to a deadline).
    "browser.press",
    "browser.keydown",
    "browser.keyup",
    "browser.wait",
    // M6 slice 1: the custom-command verbs over the config file's
    // `commands` entries (upstream surfaces them through the palette;
    // there is no wire verb upstream to mirror - port-side names).
    "command.list",
    "command.run",
    // M6 (ADR-0014): the SSH verb. The port's subset opens a workspace
    // whose pane child is `ssh.exe` (upstream's unmanaged fallback
    // shape); the managed cmux-tui carrier stays deferred.
    "workspace.ssh.open",
];

/// Parameter id length ceiling (ids are short; anything longer is bogus).
pub const MAX_PARAM_BYTES: usize = 256;

/// `browser.wait` deadline default (upstream's CLI falls back to 5000 ms).
pub const DEFAULT_WAIT_MS: u32 = 5_000;

/// `browser.wait` deadline cap; bounds how long one call may hold its pipe
/// thread (`deadline + headroom` reply budget, ADR-0006 Amendment 5).
pub const MAX_WAIT_MS: u32 = 120_000;

/// One app-plane call, already validated. Wire names mirror upstream's v2
/// methods (`tests_v2/cmux.py` consumes the same shapes: rows carry
/// `index`/`id`/`title`/`selected`, creates reply `workspace_id`,
/// `identify` carries a `focused` object).
#[derive(Debug, Clone, PartialEq)]
pub enum AppRequest {
    /// `system.identify` — identity + the focused surface snapshot.
    Identify,
    /// `workspace.list` — one row per workspace; `selected` marks the active.
    WorkspaceList,
    /// `workspace.current` — the active workspace's id only.
    WorkspaceCurrent,
    /// `workspace.select` — make a workspace active.
    WorkspaceSelect {
        workspace_id: String,
    },
    /// `workspace.create` - a new workspace with one terminal pane; it
    /// becomes the active workspace (upstream `newTab` semantics).
    WorkspaceCreate,
    /// `workspace.ssh.open` (M6, ADR-0014) - a new workspace whose pane
    /// child is the Windows OpenSSH client. Upstream's managed carrier
    /// (relay bootstrap, `auth_required` login loop) stays deferred; the
    /// port answers with the workspace/pane ids and spawns immediately.
    WorkspaceSshOpen {
        destination: String,
        ssh_options: Vec<String>,
        port: Option<u16>,
        identity_file: Option<String>,
        title: Option<String>,
        initial_command: Option<String>,
        focus: bool,
    },
    /// `surface.list` — terminal surface rows; `workspace_id` filters to
    /// one workspace, `None` → the active workspace (upstream behavior:
    /// the harness resolves the current workspace itself before listing).
    SurfaceList {
        workspace_id: Option<String>,
    },
    /// `pane.list` — pane rows; the port maps panes 1:1 to terminal
    /// surfaces, so rows repeat the surface id (documented divergence from
    /// upstream's pane/surface separation for browser panels).
    PaneList {
        workspace_id: Option<String>,
    },
    /// `surface.send_text` — type text into a surface as keystrokes
    /// (upstream: `surface.send_text`); `None` → the focused surface.
    SurfaceSendText {
        surface_id: Option<String>,
        text: String,
    },
    /// `surface.read_text` — the visible screen text (`read-screen` CLI
    /// equivalent); no scrollback, no selection (M4.1 scope).
    SurfaceReadText {
        surface_id: Option<String>,
    },
    /// `browser.open_split` — split right from the caller surface (the
    /// focused one when unset) with a browser pane loading `url`
    /// (M5.1 subset of upstream's `browser.open_split`).
    BrowserOpenSplit {
        surface_id: Option<String>,
        url: Option<String>,
    },
    /// `agent.open_split` — split right from the caller surface (the
    /// focused one when unset) with an agent activity stream pane.
    /// Design lane 5's pane surface; upstream has no wire verb for it
    /// (recorded divergence, port extension).
    AgentOpenSplit {
        surface_id: Option<String>,
    },
    /// `browser.navigate` — navigate a browser surface to `url`
    /// (upstream `browser.navigate`).
    BrowserNavigate {
        surface_id: Option<String>,
        url: String,
    },
    /// `browser.back` / `browser.forward` / `browser.reload` — history
    /// verbs on one browser surface (upstream same names).
    BrowserBack {
        surface_id: Option<String>,
    },
    BrowserForward {
        surface_id: Option<String>,
    },
    BrowserReload {
        surface_id: Option<String>,
    },
    /// `browser.url.get` — the surface's current URL and document title
    /// (M5.1 port shape; upstream's URL/title surface reads live in the
    /// browser object — the port reports a snapshot).
    BrowserUrlGet {
        surface_id: Option<String>,
    },
    /// `browser.snapshot` — the visible page text with ephemeral refs for
    /// the interactive elements (M5.2 agent-browser-port subset: refs are
    /// surface-local and replaced by the next snapshot / any navigation).
    BrowserSnapshot {
        surface_id: Option<String>,
    },
    /// `browser.eval` — evaluate a JavaScript expression on the focused
    /// page (`Runtime.evaluate` over `CallDevToolsProtocolMethod`,
    /// returnByValue).
    BrowserEval {
        surface_id: Option<String>,
        expression: String,
    },
    /// `browser.click` / `browser.dblclick` — dispatch a real mouse
    /// press+release pair at the targeted element's fresh center
    /// (`Input.dispatchMouseEvent` over CDP).
    BrowserClick {
        surface_id: Option<String>,
        target: ElementTarget,
    },
    BrowserDblclick {
        surface_id: Option<String>,
        target: ElementTarget,
    },
    /// `browser.type` — focus the referenced element (`el.focus()` via
    /// evaluate) and insert the text as one IME-style insert
    /// (`Input.insertText`; documented M5.2 divergence from upstream's
    /// per-key stream).
    BrowserType {
        surface_id: Option<String>,
        target: ElementTarget,
        text: String,
    },
    /// `browser.fill` — replace the element's value with `text` and
    /// dispatch `input`+`change` (upstream `fill` semantics: whole-value
    /// replace, not keystrokes).
    BrowserFill {
        surface_id: Option<String>,
        target: ElementTarget,
        text: String,
    },
    /// `browser.focus` — focus the referenced element (`el.focus()`).
    BrowserFocus {
        surface_id: Option<String>,
        target: ElementTarget,
    },
    /// `browser.check` / `browser.uncheck` — set the checkbox's checked
    /// state and dispatch `change` (M5.3 divergence-guarded: radio inputs
    /// also qualify; non-checkbox targets answer `invalid_state`).
    BrowserCheck {
        surface_id: Option<String>,
        target: ElementTarget,
        checked: bool,
    },
    /// `browser.select` — choose a `<select>` option by value or visible
    /// label and dispatch `change` (upstream `select` with one value).
    BrowserSelect {
        surface_id: Option<String>,
        target: ElementTarget,
        value: String,
    },
    /// `browser.hover` — move the real mouse over the element's fresh
    /// center (`Input.dispatchMouseEvent` `mouseMoved`).
    BrowserHover {
        surface_id: Option<String>,
        target: ElementTarget,
    },
    /// `browser.scroll_into_view` — bring the element into the viewport
    /// (`scrollIntoView`, block: center).
    BrowserScrollIntoView {
        surface_id: Option<String>,
        target: ElementTarget,
    },
    /// `browser.scroll` — window scroll by deltas; the reply carries the
    /// resulting offsets (`window.scrollBy`, then `scrollX/scrollY`).
    BrowserScroll {
        surface_id: Option<String>,
        dx: Option<f64>,
        dy: Option<f64>,
    },
    /// `browser.get.text|html|value|attr|count|box|styles` — read one DOM
    /// property off the targeted element (or the match count); the reply
    /// carries `value` (a JSON-mapped read) except `count` (an integer).
    BrowserGet {
        surface_id: Option<String>,
        target: ElementTarget,
        field: GetField,
        /// `browser.get.attr` only: the attribute name.
        attr: Option<String>,
        /// `browser.get.styles` only: one computed style property; absent
        /// reads the documented default set.
        property: Option<String>,
    },
    /// `browser.is.visible|enabled|checked` — one state probe off the
    /// targeted element; the reply carries `value: bool` (a missing
    /// element reads as `false`, upstream contrast-free semantics).
    BrowserIs {
        surface_id: Option<String>,
        target: ElementTarget,
        state: IsState,
    },
    /// `browser.get.title` — the surface's live document title as polled
    /// by the shell tick (the sync lane, like `browser.url.get`; the
    /// title came from the same snapshot instrumentation as M5.1).
    BrowserGetTitle {
        surface_id: Option<String>,
    },
    /// `browser.screenshot` — one `Page.captureScreenshot` (viewport PNG);
    /// the app decodes the base64 and writes it under `%TEMP%`, the reply
    /// carries the written `path` (upstream returns path/url/png_base64).
    BrowserScreenshot {
        surface_id: Option<String>,
    },
    /// `browser.focus_webview` — hand keyboard focus to the targeted
    /// browser surface (model focus + the tick's programmatic
    /// MoveFocus; shell-side, no CDP).
    BrowserFocusWebview {
        surface_id: Option<String>,
    },
    /// `browser.is_webview_focused` — whether the targeted browser surface
    /// is the active workspace's focused leaf right now (the port's
    /// documented blur-never-deselects model; shell-side, no CDP).
    BrowserIsWebviewFocused {
        surface_id: Option<String>,
    },
    /// `browser.press` / `browser.keydown` / `browser.keyup` — synthesize
    /// one key on the focused element via `Input.dispatchKeyEvent`: press
    /// sends the down+up pair, the other two a single half. No selector on
    /// the wire (upstream shape): keys go to whatever has focus.
    BrowserKey {
        surface_id: Option<String>,
        half: KeyHalf,
        /// The caller's key token (e.g. `Enter`, `Control+a`); validated
        /// against the shell's key table (invalid names are denied there).
        key: String,
    },
    /// `browser.wait` — poll the page until every given predicate becomes
    /// true or the deadline elapses (AND of the given predicates — with
    /// one predicate it is the plain upstream wait). The port polls via
    /// repeated `Runtime.evaluate` on the deferred plane; the per-call
    /// reply budget covers the deadline plus headroom so a deadline hit
    /// is reported by the app handler, not the generic envelope
    /// (ADR-0006 Amendment 5).
    BrowserWait {
        surface_id: Option<String>,
        selector: Option<String>,
        text_contains: Option<String>,
        url_contains: Option<String>,
        load_state: Option<LoadState>,
        function: Option<String>,
        timeout_ms: u32,
    },
    /// `command.list` — the config file's `commands` entries with their
    /// runable flavor resolved (M6 slice 1; upstream surfaces the same
    /// set through the command palette - no wire verb upstream exists,
    /// so the name is port-side).
    CommandList,
    /// `command.run` — look up one named command in the config file and
    /// type its `command` text plus a trailing newline into the given
    /// (or focused) terminal surface, mirroring upstream's executor
    /// (`command + "\n"` through sendInput). `ack` explicitly
    /// acknowledges a `confirm: true` entry: upstream shows a dialog,
    /// the port's dialogs are deferred, so scripts ack on the wire
    /// (recorded divergence). Workspace-flavor entries answer a
    /// structured problem (the workspace-launch slice is a residual).
    CommandRun {
        name: String,
        surface_id: Option<String>,
        ack: bool,
    },
}

/// Which half of a key event a `browser.press|keydown|keyup` wants
/// (`press` = the down+up pair).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyHalf {
    Press,
    /// The down half only (`browser.keydown`).
    Down,
    /// The up half only (`browser.keyup`).
    Up,
}

/// A `browser.wait` `load_state` predicate; unknown states are rejected at
/// parse (`networkidle` needs network instrumentation the port has not
/// instrumented — recorded as a residual).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadState {
    /// `document.readyState === 'complete'`.
    Load,
    /// `document.readyState !== 'loading'`.
    DomContentLoaded,
}

impl AppRequest {
    /// The method's wire name, for request tracing (app-side serve logs).
    pub fn method_name(&self) -> &'static str {
        match self {
            AppRequest::Identify => "system.identify",
            AppRequest::WorkspaceList => "workspace.list",
            AppRequest::WorkspaceCurrent => "workspace.current",
            AppRequest::WorkspaceSelect { .. } => "workspace.select",
            AppRequest::WorkspaceCreate => "workspace.create",
            AppRequest::SurfaceList { .. } => "surface.list",
            AppRequest::PaneList { .. } => "pane.list",
            AppRequest::SurfaceSendText { .. } => "surface.send_text",
            AppRequest::SurfaceReadText { .. } => "surface.read_text",
            AppRequest::BrowserOpenSplit { .. } => "browser.open_split",
            AppRequest::AgentOpenSplit { .. } => "agent.open_split",
            AppRequest::BrowserNavigate { .. } => "browser.navigate",
            AppRequest::BrowserBack { .. } => "browser.back",
            AppRequest::BrowserForward { .. } => "browser.forward",
            AppRequest::BrowserReload { .. } => "browser.reload",
            AppRequest::BrowserUrlGet { .. } => "browser.url.get",
            AppRequest::BrowserSnapshot { .. } => "browser.snapshot",
            AppRequest::BrowserEval { .. } => "browser.eval",
            AppRequest::BrowserClick { .. } => "browser.click",
            AppRequest::BrowserDblclick { .. } => "browser.dblclick",
            AppRequest::BrowserType { .. } => "browser.type",
            AppRequest::BrowserFill { .. } => "browser.fill",
            AppRequest::BrowserFocus { .. } => "browser.focus",
            AppRequest::BrowserCheck { checked, .. } => {
                if *checked {
                    "browser.check"
                } else {
                    "browser.uncheck"
                }
            }
            AppRequest::BrowserSelect { .. } => "browser.select",
            AppRequest::BrowserHover { .. } => "browser.hover",
            AppRequest::BrowserScrollIntoView { .. } => "browser.scroll_into_view",
            AppRequest::BrowserScroll { .. } => "browser.scroll",
            AppRequest::BrowserGet { field, .. } => get_wire_name(*field),
            AppRequest::BrowserIs { state, .. } => is_wire_name(*state),
            AppRequest::BrowserGetTitle { .. } => "browser.get.title",
            AppRequest::BrowserScreenshot { .. } => "browser.screenshot",
            AppRequest::BrowserFocusWebview { .. } => "browser.focus_webview",
            AppRequest::BrowserIsWebviewFocused { .. } => "browser.is_webview_focused",
            AppRequest::BrowserKey { half, .. } => match half {
                KeyHalf::Press => "browser.press",
                KeyHalf::Down => "browser.keydown",
                KeyHalf::Up => "browser.keyup",
            },
            AppRequest::BrowserWait { .. } => "browser.wait",
            AppRequest::CommandList => "command.list",
            AppRequest::CommandRun { .. } => "command.run",
            AppRequest::WorkspaceSshOpen { .. } => "workspace.ssh.open",
        }
    }
}

/// Which `browser.get.*` read a request wants (all DOM reads except
/// `count`, which counts selector matches).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GetField {
    /// `browser.get.text` — the element's `textContent` (verbatim).
    Text,
    /// `browser.get.html` — the element's `outerHTML`.
    Html,
    /// `browser.get.value` — the element's `value` property (null when the
    /// element carries none).
    Value,
    /// `browser.get.attr` — one named attribute (null when absent).
    Attr,
    /// `browser.get.count` — how many elements the selector matches
    /// (0 is a valid reply; the element-null contract does not apply).
    Count,
    /// `browser.get.box` — the element's `getBoundingClientRect` as a
    /// plain `{x,y,width,height}` object (null when the element is gone).
    Box,
    /// `browser.get.styles` — a map of computed style properties (one
    /// requested, or the documented default set when none is).
    Styles,
}

/// Which `browser.is.*` probe a request wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsState {
    /// `browser.is.visible` — a nonzero box whose computed visibility is
    /// not `none`/`hidden` (Playwright-approximate; documented residual).
    Visible,
    /// `browser.is.enabled` — not `disabled` and not `aria-disabled`.
    Enabled,
    /// `browser.is.checked` — the element's `checked` property.
    Checked,
}

/// The `browser.get.*` wire name for one getter (upstream noun order:
/// `browser.get.<field>`; `get.url` reads via `browser.url.get`).
pub fn get_wire_name(field: GetField) -> &'static str {
    match field {
        GetField::Text => "browser.get.text",
        GetField::Html => "browser.get.html",
        GetField::Value => "browser.get.value",
        GetField::Attr => "browser.get.attr",
        GetField::Count => "browser.get.count",
        GetField::Box => "browser.get.box",
        GetField::Styles => "browser.get.styles",
    }
}

/// The `browser.is.*` wire name for one probe.
pub fn is_wire_name(state: IsState) -> &'static str {
    match state {
        IsState::Visible => "browser.is.visible",
        IsState::Enabled => "browser.is.enabled",
        IsState::Checked => "browser.is.checked",
    }
}

/// How an element-verb addresses its element (M5.3): a snapshot ref
/// (`@eN`, normalized — the M5.2 harness shape) or a raw CSS selector
/// (upstream CLI grammar). Exactly one flavor per request; every element
/// verb accepts both. Refs resolve against the live table and answer
/// `invalid_params` when stale, selectors resolve fresh against the DOM.
#[derive(Debug, Clone, PartialEq)]
pub enum ElementTarget {
    Ref(String),
    Selector(String),
}

impl ElementTarget {
    /// The reply-side echo of the consumed target (`{"ref": …}` or
    /// `{"selector": …}`), so clients can correlate the driven element
    /// with the shape they sent.
    pub fn target_json(&self) -> Value {
        match self {
            ElementTarget::Ref(id) => serde_json::json!({ "ref": id }),
            ElementTarget::Selector(selector) => serde_json::json!({ "selector": selector }),
        }
    }
}

/// Normalizes an automation element reference. Refs mint by `browser.
/// snapshot` as `@e<digits>`; callers may spell them with or without the
/// leading `@` or a leading `e` (case-insensitive), keeping the canonical
/// form `@e<digits>` for the app's per-surface ref tables.
pub fn normalize_ref(raw: &str) -> Option<String> {
    let text = raw.trim().trim_start_matches('@');
    let digits = text
        .strip_prefix(['e', 'E'])
        .filter(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))?;
    Some(format!("@e{digits}"))
}

/// Routes a request method to the plane that answers it. Static routes
/// (`Ping`, `Capabilities`, `Auth`) are answered by the server thread alone;
/// `Enqueue` is the M3.3 admission queue; `App` calls go through the
/// handler into the app's request plane. `auth.login` is served inline in
/// every mode (upstream's system coordinator); in password mode it never
/// gets here — the password gate intercepts it before dispatch.
pub fn method_route(method: &str) -> MethodRoute {
    match method {
        "ping" | "system.ping" => MethodRoute::Ping,
        "system.capabilities" => MethodRoute::Capabilities,
        "agent.hook.enqueue" => MethodRoute::Enqueue,
        "auth.login" => MethodRoute::Auth,
        m if APP_METHODS.contains(&m) => MethodRoute::App,
        _ => MethodRoute::Unknown,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MethodRoute {
    Ping,
    Capabilities,
    Enqueue,
    Auth,
    App,
    Unknown,
}

/// Parses one app-plane request from its validated wire form. `params`
/// must be an object (envelope parsing guarantees an object default); each
/// method's violations become `invalid_params` problems. Ids are opaque
/// strings (≤ `MAX_PARAM_BYTES`): the app resolves them against its model.
pub fn parse_app_request(method: &str, params: &Value) -> Result<AppRequest, RequestProblem> {
    let obj = params
        .as_object()
        .ok_or_else(|| RequestProblem::invalid_params("params must be an object"))?;
    // Optional string id parameter; `null`/absent both mean "not given"
    // (upstream omits vs. `NSNull`s freely).
    let opt_id = |key: &str| -> Result<Option<String>, RequestProblem> {
        match obj.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) => {
                let s = s.trim();
                if s.is_empty() {
                    return Err(RequestProblem::invalid_params(format!(
                        "params.{key} must be a non-empty string"
                    )));
                }
                if s.len() > MAX_PARAM_BYTES {
                    return Err(RequestProblem::invalid_params(format!(
                        "params.{key} exceeds the {MAX_PARAM_BYTES}-byte id budget"
                    )));
                }
                Ok(Some(s.to_owned()))
            }
            Some(_) => Err(RequestProblem::invalid_params(format!(
                "params.{key} must be a string"
            ))),
        }
    };
    let req_workspace_id = |key: &str| -> Result<String, RequestProblem> {
        opt_id(key)?
            .ok_or_else(|| RequestProblem::invalid_params(format!("params.{key} is required")))
    };
    // Optional URL string; `null`/absent mean "not given" (upstream
    // browser verbs leave the URL unset to reuse a default). Same length
    // budget as the text payload; URLs are data, never commands.
    let opt_url = |key: &str| -> Result<Option<String>, RequestProblem> {
        match obj.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) => {
                if s.is_empty() {
                    return Err(RequestProblem::invalid_params(format!(
                        "params.{key} must not be empty"
                    )));
                }
                if s.len() > MAX_PAYLOAD_BYTES {
                    return Err(RequestProblem::invalid_params(format!(
                        "params.{key} exceeds the {MAX_PAYLOAD_BYTES}-byte payload budget"
                    )));
                }
                Ok(Some(s.clone()))
            }
            Some(_) => Err(RequestProblem::invalid_params(format!(
                "params.{key} must be a string"
            ))),
        }
    };
    let required_url = |key: &str| -> Result<String, RequestProblem> {
        opt_url(key)?
            .ok_or_else(|| RequestProblem::invalid_params(format!("params.{key} is required")))
    };
    match method {
        "system.identify" => Ok(AppRequest::Identify),
        "workspace.list" => Ok(AppRequest::WorkspaceList),
        "workspace.current" => Ok(AppRequest::WorkspaceCurrent),
        "workspace.select" => Ok(AppRequest::WorkspaceSelect {
            workspace_id: req_workspace_id("workspace_id")?,
        }),
        "workspace.create" => Ok(AppRequest::WorkspaceCreate),
        "workspace.ssh.open" => {
            // M6 (ADR-0014): the port's SSH verb. Upstream's CLI parses
            // its own flags first, so these mirror the v2 handoff
            // params; the wire re-validates so direct clients get the
            // same refusals the CLI produces.
            let destination = required_text(obj, "destination")?;
            let ssh_options = match obj.get("ssh_options") {
                None | Some(Value::Null) => Vec::new(),
                Some(Value::Array(values)) => {
                    let mut options = Vec::with_capacity(values.len());
                    for value in values {
                        match value {
                            Value::String(text) => {
                                let text = text.trim();
                                if text.is_empty() {
                                    return Err(RequestProblem::invalid_params(
                                        "params.ssh_options must not contain empty values",
                                    ));
                                }
                                options.push(text.to_owned());
                            }
                            _ => {
                                // Upstream hands off config-style
                                // OpenSSH options; a non-string entry
                                // is always a caller bug.
                                return Err(RequestProblem::invalid_params(
                                    "params.ssh_options must be an array of strings",
                                ));
                            }
                        }
                    }
                    options
                }
                Some(_) => {
                    return Err(RequestProblem::invalid_params(
                        "params.ssh_options must be an array of strings",
                    ))
                }
            };
            let port = match obj.get("port") {
                None | Some(Value::Null) => None,
                Some(Value::Number(number)) => match number.as_u64() {
                    Some(raw @ 1..=65535) => Some(raw as u16),
                    _ => {
                        return Err(RequestProblem::invalid_params(
                            "params.port must be an integer in 1-65535",
                        ))
                    }
                },
                Some(_) => {
                    return Err(RequestProblem::invalid_params(
                        "params.port must be an integer in 1-65535",
                    ))
                }
            };
            let identity_file = opt_id("identity_file")?;
            let title = opt_id("title")?;
            let initial_command = match obj.get("initial_command") {
                None | Some(Value::Null) => None,
                Some(Value::String(text)) => {
                    let text = text.trim();
                    if text.is_empty() {
                        None
                    } else if text.len() > MAX_PAYLOAD_BYTES {
                        return Err(RequestProblem::invalid_params(format!(
                            "params.initial_command exceeds the {MAX_PAYLOAD_BYTES}-byte payload budget"
                        )));
                    } else {
                        Some(text.to_owned())
                    }
                }
                Some(_) => {
                    return Err(RequestProblem::invalid_params(
                        "params.initial_command must be a string",
                    ))
                }
            };
            let focus = match obj.get("focus") {
                None | Some(Value::Null) => true,
                Some(Value::Bool(value)) => *value,
                Some(_) => {
                    return Err(RequestProblem::invalid_params(
                        "params.focus must be a boolean",
                    ))
                }
            };
            Ok(AppRequest::WorkspaceSshOpen {
                destination,
                ssh_options,
                port,
                identity_file,
                title,
                initial_command,
                focus,
            })
        }
        "surface.list" => Ok(AppRequest::SurfaceList {
            workspace_id: opt_id("workspace_id")?,
        }),
        "pane.list" => Ok(AppRequest::PaneList {
            workspace_id: opt_id("workspace_id")?,
        }),
        "surface.send_text" => {
            let surface_id = opt_id("surface_id")?;
            let text = match obj.get("text") {
                Some(Value::String(text)) => text.clone(),
                Some(_) => {
                    return Err(RequestProblem::invalid_params(
                        "params.text must be a string",
                    ))
                }
                None => return Err(RequestProblem::invalid_params("params.text is required")),
            };
            // Upstream's CLI refuses empty keystroke sends; mirror it so
            // automation clients see the same contract.
            if text.is_empty() {
                return Err(RequestProblem::invalid_params(
                    "params.text must not be empty",
                ));
            }
            if text.len() > MAX_PAYLOAD_BYTES {
                return Err(RequestProblem::invalid_params(format!(
                    "params.text exceeds the {MAX_PAYLOAD_BYTES}-byte payload budget"
                )));
            }
            Ok(AppRequest::SurfaceSendText { surface_id, text })
        }
        "surface.read_text" => Ok(AppRequest::SurfaceReadText {
            surface_id: opt_id("surface_id")?,
        }),
        "browser.open_split" => Ok(AppRequest::BrowserOpenSplit {
            surface_id: opt_id("surface_id")?,
            url: opt_url("url")?,
        }),
        "agent.open_split" => Ok(AppRequest::AgentOpenSplit {
            surface_id: opt_id("surface_id")?,
        }),
        "browser.navigate" => {
            let url = required_url("url")?;
            Ok(AppRequest::BrowserNavigate {
                surface_id: opt_id("surface_id")?,
                url,
            })
        }
        "browser.back" => Ok(AppRequest::BrowserBack {
            surface_id: opt_id("surface_id")?,
        }),
        "browser.forward" => Ok(AppRequest::BrowserForward {
            surface_id: opt_id("surface_id")?,
        }),
        "browser.reload" => Ok(AppRequest::BrowserReload {
            surface_id: opt_id("surface_id")?,
        }),
        "browser.url.get" => Ok(AppRequest::BrowserUrlGet {
            surface_id: opt_id("surface_id")?,
        }),
        "browser.snapshot" => Ok(AppRequest::BrowserSnapshot {
            surface_id: opt_id("surface_id")?,
        }),
        "browser.eval" => {
            // The port's canonical field is `expression` (M5.2); upstream's
            // CLI names the same field `script` — accepted as the fallback
            // so upstream-grammar clients work unmodified (documented
            // divergence reconciliation, ADR-0006 Amendment 4).
            let take_free_text = |value: Option<&Value>| -> Result<String, RequestProblem> {
                let text = match value {
                    Some(Value::String(text)) => text.clone(),
                    Some(_) => {
                        return Err(RequestProblem::invalid_params(
                            "params.expression must be a string",
                        ))
                    }
                    None => {
                        return Err(RequestProblem::invalid_params(
                            "params.expression is required",
                        ))
                    }
                };
                if text.is_empty() {
                    return Err(RequestProblem::invalid_params(
                        "params.expression must not be empty",
                    ));
                }
                if text.len() > MAX_PAYLOAD_BYTES {
                    return Err(RequestProblem::invalid_params(format!(
                        "params.expression exceeds the {MAX_PAYLOAD_BYTES}-byte payload budget"
                    )));
                }
                Ok(text)
            };
            let given = |value: Option<&Value>| !matches!(value, None | Some(Value::Null));
            if given(obj.get("expression")) && given(obj.get("script")) {
                return Err(RequestProblem::invalid_params(
                    "params.expression and params.script are mutually exclusive",
                ));
            }
            let expression = if given(obj.get("expression")) {
                take_free_text(obj.get("expression"))?
            } else if given(obj.get("script")) {
                take_free_text(obj.get("script"))?
            } else {
                return Err(RequestProblem::invalid_params(
                    "params.expression is required",
                ));
            };
            Ok(AppRequest::BrowserEval {
                surface_id: opt_id("surface_id")?,
                expression,
            })
        }
        "browser.click" => Ok(AppRequest::BrowserClick {
            surface_id: opt_id("surface_id")?,
            target: parse_element_target(obj)?,
        }),
        "browser.dblclick" => Ok(AppRequest::BrowserDblclick {
            surface_id: opt_id("surface_id")?,
            target: parse_element_target(obj)?,
        }),
        "browser.type" => {
            let text = required_text(obj, "text")?;
            Ok(AppRequest::BrowserType {
                surface_id: opt_id("surface_id")?,
                target: parse_element_target(obj)?,
                text,
            })
        }
        // M5.3: the observation + form/scroll family. Element verbs share
        // the ref-XOR-selector target grammar; the actions read one
        // evaluate and the shell decides the reply shape.
        "browser.fill" => {
            let text = required_text(obj, "text")?;
            Ok(AppRequest::BrowserFill {
                surface_id: opt_id("surface_id")?,
                target: parse_element_target(obj)?,
                text,
            })
        }
        "browser.focus" => Ok(AppRequest::BrowserFocus {
            surface_id: opt_id("surface_id")?,
            target: parse_element_target(obj)?,
        }),
        "browser.check" | "browser.uncheck" => Ok(AppRequest::BrowserCheck {
            surface_id: opt_id("surface_id")?,
            target: parse_element_target(obj)?,
            checked: method == "browser.check",
        }),
        "browser.select" => {
            let value = required_text(obj, "value")?;
            Ok(AppRequest::BrowserSelect {
                surface_id: opt_id("surface_id")?,
                target: parse_element_target(obj)?,
                value,
            })
        }
        "browser.hover" => Ok(AppRequest::BrowserHover {
            surface_id: opt_id("surface_id")?,
            target: parse_element_target(obj)?,
        }),
        "browser.scroll_into_view" => Ok(AppRequest::BrowserScrollIntoView {
            surface_id: opt_id("surface_id")?,
            target: parse_element_target(obj)?,
        }),
        "browser.scroll" => {
            let dx = opt_f64(obj, "dx")?;
            let dy = opt_f64(obj, "dy")?;
            if dx.is_none() && dy.is_none() {
                return Err(RequestProblem::invalid_params(
                    "params.dx or params.dy is required",
                ));
            }
            Ok(AppRequest::BrowserScroll {
                surface_id: opt_id("surface_id")?,
                dx,
                dy,
            })
        }
        "browser.get.text" | "browser.get.html" | "browser.get.value" | "browser.get.count" => {
            let field = match method {
                "browser.get.text" => GetField::Text,
                "browser.get.html" => GetField::Html,
                "browser.get.value" => GetField::Value,
                _ => GetField::Count,
            };
            Ok(AppRequest::BrowserGet {
                surface_id: opt_id("surface_id")?,
                target: parse_element_target(obj)?,
                field,
                attr: None,
                property: None,
            })
        }
        "browser.get.attr" => Ok(AppRequest::BrowserGet {
            surface_id: opt_id("surface_id")?,
            target: parse_element_target(obj)?,
            field: GetField::Attr,
            attr: Some(param_name(obj, "attr")?),
            property: None,
        }),
        "browser.get.box" => Ok(AppRequest::BrowserGet {
            surface_id: opt_id("surface_id")?,
            target: parse_element_target(obj)?,
            field: GetField::Box,
            attr: None,
            property: None,
        }),
        "browser.get.styles" => Ok(AppRequest::BrowserGet {
            surface_id: opt_id("surface_id")?,
            target: parse_element_target(obj)?,
            field: GetField::Styles,
            attr: None,
            property: opt_name(obj, "property")?,
        }),
        "browser.is.visible" | "browser.is.enabled" | "browser.is.checked" => {
            Ok(AppRequest::BrowserIs {
                surface_id: opt_id("surface_id")?,
                target: parse_element_target(obj)?,
                state: match method {
                    "browser.is.visible" => IsState::Visible,
                    "browser.is.enabled" => IsState::Enabled,
                    _ => IsState::Checked,
                },
            })
        }
        "browser.get.title" => Ok(AppRequest::BrowserGetTitle {
            surface_id: opt_id("surface_id")?,
        }),
        "browser.screenshot" => Ok(AppRequest::BrowserScreenshot {
            surface_id: opt_id("surface_id")?,
        }),
        "browser.focus_webview" => Ok(AppRequest::BrowserFocusWebview {
            surface_id: opt_id("surface_id")?,
        }),
        "browser.is_webview_focused" => Ok(AppRequest::BrowserIsWebviewFocused {
            surface_id: opt_id("surface_id")?,
        }),
        // M5.4: the key family — one short key token, no selector (keys go
        // to the focused element; the shell validates the vocabulary).
        "browser.press" | "browser.keydown" | "browser.keyup" => Ok(AppRequest::BrowserKey {
            surface_id: opt_id("surface_id")?,
            half: match method {
                "browser.press" => KeyHalf::Press,
                "browser.keydown" => KeyHalf::Down,
                _ => KeyHalf::Up,
            },
            key: param_name(obj, "key")?,
        }),
        "browser.wait" => {
            // An optional free-text predicate (same budget as the eval
            // expression; it rides into page JS as a JSON literal).
            let opt_pred = |key: &str| -> Result<Option<String>, RequestProblem> {
                match obj.get(key) {
                    None | Some(Value::Null) => Ok(None),
                    _ => required_text(obj, key).map(Some),
                }
            };
            let selector = opt_pred("selector")?;
            let text_contains = opt_pred("text_contains")?;
            let url_contains = opt_pred("url_contains")?;
            let function = opt_pred("function")?;
            let load_state = match obj.get("load_state") {
                None | Some(Value::Null) => None,
                Some(Value::String(state)) => match state.as_str() {
                    "load" => Some(LoadState::Load),
                    "domcontentloaded" => Some(LoadState::DomContentLoaded),
                    other => {
                        return Err(RequestProblem::invalid_params(format!(
                            "params.load_state '{other}' is not one of 'load', 'domcontentloaded'"
                        )))
                    }
                },
                Some(_) => {
                    return Err(RequestProblem::invalid_params(
                        "params.load_state must be a string",
                    ))
                }
            };
            if selector.is_none()
                && text_contains.is_none()
                && url_contains.is_none()
                && load_state.is_none()
                && function.is_none()
            {
                return Err(RequestProblem::invalid_params(
                    "one of params.selector, params.text_contains, params.url_contains, params.load_state or params.function is required",
                ));
            }
            // Upstream's CLI default (5000 ms); capped to bound how long
            // one call may hold its pipe thread.
            let timeout_ms = match obj.get("timeout_ms") {
                None | Some(Value::Null) => DEFAULT_WAIT_MS,
                Some(Value::Number(ms)) => {
                    let ms = ms.as_f64().ok_or_else(|| {
                        RequestProblem::invalid_params("params.timeout_ms must be finite")
                    })?;
                    if ms.fract() != 0.0 {
                        return Err(RequestProblem::invalid_params(
                            "params.timeout_ms must be an integer number of milliseconds",
                        ));
                    }
                    if ms < 0.0 {
                        return Err(RequestProblem::invalid_params(
                            "params.timeout_ms must not be negative",
                        ));
                    }
                    if ms > MAX_WAIT_MS as f64 {
                        return Err(RequestProblem::invalid_params(format!(
                            "params.timeout_ms exceeds the {MAX_WAIT_MS} ms cap"
                        )));
                    }
                    ms as u32
                }
                Some(_) => {
                    return Err(RequestProblem::invalid_params(
                        "params.timeout_ms must be a number",
                    ))
                }
            };
            Ok(AppRequest::BrowserWait {
                surface_id: opt_id("surface_id")?,
                selector,
                text_contains,
                url_contains,
                load_state,
                function,
                timeout_ms,
            })
        }
        // M6 slice 1: the custom-command verbs. `ack` defaults to false
        // and only the boolean form is accepted (a stringy "true" would
        // blur what the caller actually acknowledged).
        "command.list" => Ok(AppRequest::CommandList),
        "command.run" => {
            let name = required_text(obj, "name")?;
            let surface_id = opt_id("surface_id")?;
            let ack = match obj.get("ack") {
                None => false,
                Some(Value::Bool(value)) => *value,
                Some(_) => {
                    return Err(RequestProblem::invalid_params(
                        "params.ack must be a boolean",
                    ))
                }
            };
            Ok(AppRequest::CommandRun {
                name,
                surface_id,
                ack,
            })
        }
        _ => Err(RequestProblem::invalid_params(format!(
            "method '{method}' is not an app-plane method"
        ))),
    }
}

/// A required string parameter with the payload budget (shared by the
/// automation verbs' free-text fields).
fn required_text(
    obj: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<String, RequestProblem> {
    let text = match obj.get(key) {
        Some(Value::String(text)) => text.clone(),
        Some(_) => {
            return Err(RequestProblem::invalid_params(format!(
                "params.{key} must be a string"
            )))
        }
        None => {
            return Err(RequestProblem::invalid_params(format!(
                "params.{key} is required"
            )))
        }
    };
    if text.is_empty() {
        return Err(RequestProblem::invalid_params(format!(
            "params.{key} must not be empty"
        )));
    }
    if text.len() > MAX_PAYLOAD_BYTES {
        return Err(RequestProblem::invalid_params(format!(
            "params.{key} exceeds the {MAX_PAYLOAD_BYTES}-byte payload budget"
        )));
    }
    Ok(text)
}

/// A required element reference parameter, normalized to `@e<digits>`.
fn parse_ref(key: &str, obj: &serde_json::Map<String, Value>) -> Result<String, RequestProblem> {
    let raw = match obj.get(key) {
        Some(Value::String(raw)) => raw.clone(),
        Some(_) => {
            return Err(RequestProblem::invalid_params(format!(
                "params.{key} must be a string"
            )))
        }
        None => {
            return Err(RequestProblem::invalid_params(format!(
                "params.{key} is required"
            )))
        }
    };
    if raw.len() > MAX_PARAM_BYTES {
        return Err(RequestProblem::invalid_params(format!(
            "params.{key} exceeds the {MAX_PARAM_BYTES}-byte param budget"
        )));
    }
    normalize_ref(&raw).ok_or_else(|| {
        RequestProblem::invalid_params(format!(
            "params.{key} is not an element ref ('{{@}}e<digits>')"
        ))
    })
}

/// An optional finite number parameter (M5.3 `browser.scroll` deltas);
/// `null`/absent mean "not given" and a non-number is `invalid_params`.
fn opt_f64(obj: &serde_json::Map<String, Value>, key: &str) -> Result<Option<f64>, RequestProblem> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(number)) => number
            .as_f64()
            .map(Some)
            .ok_or_else(|| RequestProblem::invalid_params(format!("params.{key} must be finite"))),
        Some(_) => Err(RequestProblem::invalid_params(format!(
            "params.{key} must be a number"
        ))),
    }
}

/// A required short identifier-like name (M5.3 `browser.get.attr`'s
/// attribute name): non-empty, trimmed ≤ `MAX_PARAM_BYTES`.
fn param_name(obj: &serde_json::Map<String, Value>, key: &str) -> Result<String, RequestProblem> {
    let name = required_text(obj, key)?;
    if name.len() > MAX_PARAM_BYTES {
        return Err(RequestProblem::invalid_params(format!(
            "params.{key} exceeds the {MAX_PARAM_BYTES}-byte param budget"
        )));
    }
    Ok(name)
}

/// An optional short name (M5.3 `browser.get.styles`' single property);
/// absent means "the documented default property set".
fn opt_name(
    obj: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<String>, RequestProblem> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        _ => param_name(obj, key).map(Some),
    }
}

/// The element-verb target: `ref` XOR `selector`. Both at once is
/// `invalid_params` (ambiguous), neither is `invalid_params`; a ref
/// normalizes like every other ref verb, a selector is free text with the
/// payload budget (it rides into page JS as a JSON literal, never as code).
fn parse_element_target(
    obj: &serde_json::Map<String, Value>,
) -> Result<ElementTarget, RequestProblem> {
    let has_ref = !matches!(obj.get("ref"), None | Some(Value::Null));
    let has_selector = !matches!(obj.get("selector"), None | Some(Value::Null));
    match (has_ref, has_selector) {
        (false, false) => Err(RequestProblem::invalid_params(
            "params.ref or params.selector is required",
        )),
        (true, true) => Err(RequestProblem::invalid_params(
            "params.ref and params.selector are mutually exclusive",
        )),
        (true, false) => Ok(ElementTarget::Ref(parse_ref("ref", obj)?)),
        (false, true) => Ok(ElementTarget::Selector(required_text(obj, "selector")?)),
    }
}

/// Builds the `system.capabilities` result (upstream `v2Capabilities`:
/// protocol name, wire version, served method list — sorted here). The
/// list is the union of [`KNOWN_METHODS`] (statics + the M4.1 freeze) and
/// [`APP_METHODS`] (the app plane as grown), so discovery tracks reality.
pub fn capabilities_result() -> Value {
    let mut methods: Vec<&str> = KNOWN_METHODS.to_vec();
    methods.extend(APP_METHODS.iter().copied());
    methods.sort_unstable();
    methods.dedup();
    serde_json::json!({ "protocol": "cmux-socket", "version": 2, "methods": methods })
}

/// Parses one request line into id/method/params; `Ok(None)` = a JSON value
/// that is not an envelope object (log-and-close, no reply: a non-object
/// has no id to correlate).
pub fn parse_request_line(line: &str) -> Result<Option<(Value, String, Value)>, String> {
    let value: Value = serde_json::from_str(line).map_err(|e| format!("invalid JSON line: {e}"))?;
    let Some(obj) = value.as_object() else {
        return Ok(None);
    };
    let id = obj.get("id").cloned().unwrap_or(Value::Null);
    let method = obj
        .get("method")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing method".to_owned())?
        .to_owned();
    let params = obj
        .get("params")
        .cloned()
        .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
    Ok(Some((id, method, params)))
}

/// Builds a success response.
pub fn success(id: &Value, result: Value) -> String {
    serde_json::json!({ "id": id, "ok": true, "result": result }).to_string()
}

/// Builds a structured error response (upstream error shape:
/// `{ok:false,error:{code,message,action,data}}`).
pub fn error(id: &Value, code: &str, message: &str, action: &str) -> String {
    serde_json::json!({
        "id": id,
        "ok": false,
        "error": { "code": code, "message": message, "action": action, "data": {} }
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_a_clean_enqueue() {
        let params = serde_json::json!({
            "agent": "codex",
            "subcommand": "stop",
            "payload": { "cwd": "/tmp" },
            "environment": { "CMUX_SURFACE_ID": "p1", "PWD": "x", "EVIL": "no" }
        });
        let validated = validate_enqueue_params(&params).expect("valid");
        assert_eq!(validated.agent, "codex");
        assert_eq!(validated.subcommand, "stop");
        assert_eq!(validated.environment.get("CMUX_SURFACE_ID").unwrap(), "p1");
        // PWD is allow-listed upstream; anything else is dropped.
        assert!(validated.environment.contains_key("PWD"));
        assert!(!validated.environment.contains_key("EVIL"));
    }

    #[test]
    fn rejects_unknown_agents_and_bad_subcommands() {
        // Unknown agents stay rejected (the catalog grows per slice).
        let params = serde_json::json!({ "agent": "grok", "subcommand": "stop" });
        assert!(validate_enqueue_params(&params)
            .err()
            .unwrap()
            .contains("not admit-able"));
        // claude joined the admit set in M3.3b.
        let params = serde_json::json!({ "agent": "claude", "subcommand": "stop" });
        let validated = validate_enqueue_params(&params).expect("claude admitted");
        assert_eq!(validated.agent, "claude");
        let params = serde_json::json!({ "agent": "codex", "subcommand": "stop; rm" });
        assert!(validate_enqueue_params(&params).is_err());
        let params = serde_json::json!({ "agent": "codex", "subcommand": "" });
        assert!(validate_enqueue_params(&params).is_err());
    }

    #[test]
    fn enforces_the_payload_budget() {
        let big = "x".repeat(MAX_PAYLOAD_BYTES + 1);
        let params = serde_json::json!({ "agent": "codex", "subcommand": "stop", "payload": big });
        let error = validate_enqueue_params(&params).err().unwrap();
        assert!(error.contains("budget"), "{error}");
    }

    #[test]
    fn routed_env_takes_the_agent_pid_first_then_the_fallback() {
        let with_key = routed_env_for(
            "codex",
            |k| (k == "CMUX_CODEX_PID").then(|| "4242".to_owned()),
            Some(777),
        );
        assert_eq!(with_key.get("agent_pid").unwrap(), "4242");
        let fallback = routed_env_for("codex", |_| None, Some(777));
        assert_eq!(fallback.get("agent_pid").unwrap(), "777");
        let none = routed_env_for("codex", |_| None, None);
        assert_eq!(none.get("agent_pid"), None);
    }

    #[test]
    fn env_values_are_single_line_and_capped() {
        let env = routed_env_for(
            "codex",
            |k| (k == "CMUX_SURFACE_ID").then(|| format!("a\nb{}", "y".repeat(300))),
            None,
        );
        let value = env.get("CMUX_SURFACE_ID").unwrap().as_str().unwrap();
        assert!(!value.contains('\n'));
        // "a b" + 300 y's, capped at 256 chars.
        assert_eq!(value.chars().count(), 256);
    }

    #[test]
    fn parse_and_response_helpers_round_trip() {
        let (id, method, params) = parse_request_line(r#"{"id":"9","method":"ping","params":{}}"#)
            .unwrap()
            .unwrap();
        assert_eq!(id, "9");
        assert_eq!(method, "ping");
        assert_eq!(params, serde_json::json!({}));
        assert!(parse_request_line("[]").unwrap().is_none());
        let reply = success(&id, serde_json::json!({ "queued": true }));
        assert!(reply.contains(r#""ok":true"#), "{reply}");
        let err = error(&id, "queue_full", "full", "retry");
        assert!(err.contains(r#""ok":false"#), "{err}");
    }

    // ----- M4.1: routing, app-plane parsing, capabilities -----

    #[test]
    fn routes_every_known_method_to_the_right_plane() {
        assert_eq!(method_route("ping"), MethodRoute::Ping);
        assert_eq!(method_route("system.ping"), MethodRoute::Ping);
        assert_eq!(
            method_route("system.capabilities"),
            MethodRoute::Capabilities
        );
        assert_eq!(method_route("agent.hook.enqueue"), MethodRoute::Enqueue);
        // M4.2: `auth.login` serves inline in every mode (upstream system
        // coordinator); the password gate intercepts it before this route.
        assert_eq!(method_route("auth.login"), MethodRoute::Auth);
        for method in APP_METHODS {
            assert_eq!(method_route(method), MethodRoute::App, "{method}");
        }
        assert_eq!(method_route("zzz"), MethodRoute::Unknown);
        // Capability discovery advertises exactly the served set, sorted.
        let caps = capabilities_result();
        assert_eq!(caps["protocol"], "cmux-socket");
        assert_eq!(caps["version"], 2);
        let methods: Vec<&str> = caps["methods"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        let sorted = {
            let mut m = methods.clone();
            m.sort_unstable();
            m
        };
        assert_eq!(methods, sorted, "capability method list is sorted");
        assert!(methods.contains(&"system.ping"));
        assert!(methods.contains(&"surface.read_text"));
        // The login probe is advertiser-visible (upstream capabilities).
        assert!(methods.contains(&"auth.login"));
    }

    #[test]
    fn parses_the_app_plane_happy_paths() {
        let ok = serde_json::json!({});
        assert_eq!(
            parse_app_request("system.identify", &ok).unwrap(),
            AppRequest::Identify
        );
        assert_eq!(
            parse_app_request("workspace.list", &ok).unwrap(),
            AppRequest::WorkspaceList
        );
        let ws = serde_json::json!({ "workspace_id": "w2" });
        assert_eq!(
            parse_app_request("workspace.select", &ws).unwrap(),
            AppRequest::WorkspaceSelect {
                workspace_id: "w2".to_owned()
            }
        );
        assert_eq!(
            parse_app_request(
                "workspace.create",
                &serde_json::json!({ "window_id": null })
            )
            .unwrap(),
            AppRequest::WorkspaceCreate
        );
        let filtered = serde_json::json!({ "workspace_id": "w1" });
        assert_eq!(
            parse_app_request("surface.list", &filtered).unwrap(),
            AppRequest::SurfaceList {
                workspace_id: Some("w1".to_owned())
            }
        );
        assert_eq!(
            parse_app_request("pane.list", &ok).unwrap(),
            AppRequest::PaneList { workspace_id: None }
        );
        let send = serde_json::json!({ "text": "echo hi\n" });
        assert_eq!(
            parse_app_request("surface.send_text", &send).unwrap(),
            AppRequest::SurfaceSendText {
                surface_id: None,
                text: "echo hi\n".to_owned()
            }
        );
        let send_to = serde_json::json!({ "surface_id": "p2", "text": "x" });
        assert_eq!(
            parse_app_request("surface.send_text", &send_to).unwrap(),
            AppRequest::SurfaceSendText {
                surface_id: Some("p2".to_owned()),
                text: "x".to_owned()
            }
        );
        let read = serde_json::json!({ "surface_id": "p2" });
        assert_eq!(
            parse_app_request("surface.read_text", &read).unwrap(),
            AppRequest::SurfaceReadText {
                surface_id: Some("p2".to_owned())
            }
        );
        // `null` ids read as absent (upstream omits vs NSNulls freely).
        let nulled = serde_json::json!({ "surface_id": null });
        assert_eq!(
            parse_app_request("surface.read_text", &nulled).unwrap(),
            AppRequest::SurfaceReadText { surface_id: None }
        );
    }

    #[test]
    fn command_verbs_parse_their_params() {
        let empty = serde_json::json!({});
        assert_eq!(
            parse_app_request("command.list", &empty).unwrap(),
            AppRequest::CommandList
        );

        let bare = serde_json::json!({ "name": "build" });
        assert_eq!(
            parse_app_request("command.run", &bare).unwrap(),
            AppRequest::CommandRun {
                name: "build".to_owned(),
                surface_id: None,
                ack: false
            }
        );
        let full = serde_json::json!({
            "name": "build",
            "surface_id": "p2",
            "ack": true
        });
        assert_eq!(
            parse_app_request("command.run", &full).unwrap(),
            AppRequest::CommandRun {
                name: "build".to_owned(),
                surface_id: Some("p2".to_owned()),
                ack: true
            }
        );

        // `ack` only accepts booleans.
        let stringy = serde_json::json!({ "name": "build", "ack": "yes" });
        assert_eq!(
            parse_app_request("command.run", &stringy)
                .err()
                .unwrap()
                .code,
            "invalid_params"
        );

        // A missing or empty name fails like the other text params.
        assert_eq!(
            parse_app_request("command.run", &empty).err().unwrap().code,
            "invalid_params"
        );
        let blank = serde_json::json!({ "name": "" });
        assert_eq!(
            parse_app_request("command.run", &blank).err().unwrap().code,
            "invalid_params"
        );
    }

    #[test]
    fn ssh_open_parses_the_v2_handoff_params() {
        // M6 (ADR-0014): defaults mirror upstream's `runSSHTui` handoff —
        // `focus` defaults true, `ssh_options` to an empty list.
        let bare = serde_json::json!({ "destination": "user@host" });
        assert_eq!(
            parse_app_request("workspace.ssh.open", &bare).unwrap(),
            AppRequest::WorkspaceSshOpen {
                destination: "user@host".to_owned(),
                ssh_options: Vec::new(),
                port: None,
                identity_file: None,
                title: None,
                initial_command: None,
                focus: true
            }
        );
        let full = serde_json::json!({
            "destination": "user@host",
            "ssh_options": ["IdentitiesOnly=yes"],
            "port": 2222,
            "identity_file": "C:/keys/id_ed25519",
            "title": "build box",
            "initial_command": "cd /srv && tail -f log",
            "focus": false
        });
        assert_eq!(
            parse_app_request("workspace.ssh.open", &full).unwrap(),
            AppRequest::WorkspaceSshOpen {
                destination: "user@host".to_owned(),
                ssh_options: vec!["IdentitiesOnly=yes".to_owned()],
                port: Some(2222),
                identity_file: Some("C:/keys/id_ed25519".to_owned()),
                title: Some("build box".to_owned()),
                initial_command: Some("cd /srv && tail -f log".to_owned()),
                focus: false
            }
        );

        // Explicit refusals: the wire re-validates so direct clients get
        // the same refusals the CLI produces.
        let empty = serde_json::json!({});
        let problem = parse_app_request("workspace.ssh.open", &empty)
            .err()
            .unwrap();
        assert_eq!(problem.code, "invalid_params");
        assert!(problem.message.contains("destination"));

        let stringy_focus = serde_json::json!({ "destination": "h", "focus": "yes" });
        assert_eq!(
            parse_app_request("workspace.ssh.open", &stringy_focus)
                .err()
                .unwrap()
                .code,
            "invalid_params"
        );
        for raw_port in [0i64, 65536, -1] {
            let bad_port = serde_json::json!({ "destination": "h", "port": raw_port });
            assert_eq!(
                parse_app_request("workspace.ssh.open", &bad_port)
                    .err()
                    .unwrap()
                    .code,
                "invalid_params",
                "port {raw_port} must refuse"
            );
        }
        let lazy_port = serde_json::json!({ "destination": "h", "port": "2222" });
        assert_eq!(
            parse_app_request("workspace.ssh.open", &lazy_port)
                .err()
                .unwrap()
                .code,
            "invalid_params"
        );
        let numeric_options = serde_json::json!({
            "destination": "h",
            "ssh_options": [42]
        });
        // OpenSSH config-style options are strings; a non-string entry is
        // a caller bug.
        assert_eq!(
            parse_app_request("workspace.ssh.open", &numeric_options)
                .err()
                .unwrap()
                .code,
            "invalid_params"
        );
        let nulled = serde_json::json!({
            "destination": "h",
            "ssh_options": null,
            "port": null
        });
        assert_eq!(
            parse_app_request("workspace.ssh.open", &nulled).unwrap(),
            AppRequest::WorkspaceSshOpen {
                destination: "h".to_owned(),
                ssh_options: Vec::new(),
                port: None,
                identity_file: None,
                title: None,
                initial_command: None,
                focus: true
            }
        );
    }

    #[test]
    fn app_plane_rejections_carry_codes_and_actions() {
        let empty = serde_json::json!({});
        let problem = parse_app_request("workspace.select", &empty).err().unwrap();
        assert_eq!(problem.code, "invalid_params");
        assert!(problem.message.contains("workspace_id"));

        let problem = parse_app_request("surface.send_text", &empty)
            .err()
            .unwrap();
        assert_eq!(problem.code, "invalid_params");
        assert!(problem.message.contains("text"));

        let empty_text = serde_json::json!({ "text": "" });
        assert_eq!(
            parse_app_request("surface.send_text", &empty_text)
                .err()
                .unwrap()
                .code,
            "invalid_params"
        );

        let big = "x".repeat(MAX_PAYLOAD_BYTES + 1);
        let big_text = serde_json::json!({ "text": big });
        assert!(parse_app_request("surface.send_text", &big_text).is_err());

        let bad_id = serde_json::json!({ "workspace_id": 7 });
        assert_eq!(
            parse_app_request("workspace.select", &bad_id)
                .err()
                .unwrap()
                .code,
            "invalid_params"
        );

        let not_object = serde_json::json!("nope");
        assert_eq!(
            parse_app_request("workspace.list", &not_object)
                .err()
                .unwrap()
                .code,
            "invalid_params"
        );

        // Non-app methods never parse here (the router rejects first).
        assert!(parse_app_request("zzz", &empty).is_err());
    }

    // ----- M5.1: browser verb parsing -----

    // ----- Lane 5: agent pane verb parsing -----

    #[test]
    fn parses_agent_open_split_with_optional_surface() {
        let ok = serde_json::json!({});
        assert_eq!(
            parse_app_request("agent.open_split", &ok).unwrap(),
            AppRequest::AgentOpenSplit { surface_id: None }
        );
        let with_surface = serde_json::json!({ "surface_id": "42" });
        assert_eq!(
            parse_app_request("agent.open_split", &with_surface).unwrap(),
            AppRequest::AgentOpenSplit {
                surface_id: Some("42".to_owned())
            }
        );
    }

    #[test]
    fn parses_browser_verbs_with_optional_ids_and_urls() {
        let ok = serde_json::json!({});
        assert_eq!(
            parse_app_request("browser.open_split", &ok).unwrap(),
            AppRequest::BrowserOpenSplit {
                surface_id: None,
                url: None
            }
        );
        let with_url = serde_json::json!({ "url": "file:///C:/page.html" });
        assert_eq!(
            parse_app_request("browser.open_split", &with_url).unwrap(),
            AppRequest::BrowserOpenSplit {
                surface_id: None,
                url: Some("file:///C:/page.html".to_owned())
            }
        );
        assert_eq!(
            parse_app_request("browser.back", &ok).unwrap(),
            AppRequest::BrowserBack { surface_id: None }
        );
        assert_eq!(
            parse_app_request("browser.forward", &ok).unwrap(),
            AppRequest::BrowserForward { surface_id: None }
        );
        assert_eq!(
            parse_app_request("browser.reload", &ok).unwrap(),
            AppRequest::BrowserReload { surface_id: None }
        );
        assert_eq!(
            parse_app_request("browser.url.get", &ok).unwrap(),
            AppRequest::BrowserUrlGet { surface_id: None }
        );
        let navigate = serde_json::json!({ "surface_id": "p7", "url": "https://example.local" });
        assert_eq!(
            parse_app_request("browser.navigate", &navigate).unwrap(),
            AppRequest::BrowserNavigate {
                surface_id: Some("p7".to_owned()),
                url: "https://example.local".to_owned()
            }
        );
    }

    #[test]
    fn browser_url_violations_carry_invalid_params() {
        let empty = serde_json::json!({});
        let problem = parse_app_request("browser.navigate", &empty).err().unwrap();
        assert_eq!(problem.code, "invalid_params");
        assert!(problem.message.contains("url"));

        let blank = serde_json::json!({ "url": "" });
        assert_eq!(
            parse_app_request("browser.open_split", &blank)
                .err()
                .unwrap()
                .code,
            "invalid_params"
        );

        let big_url = "x".repeat(MAX_PAYLOAD_BYTES + 1);
        let big = serde_json::json!({ "url": big_url });
        assert!(parse_app_request("browser.open_split", &big).is_err());

        let non_string = serde_json::json!({ "url": 42 });
        assert_eq!(
            parse_app_request("browser.open_split", &non_string)
                .err()
                .unwrap()
                .code,
            "invalid_params"
        );
    }

    #[test]
    fn param_ids_are_bounded() {
        let long = "w".repeat(MAX_PARAM_BYTES + 1);
        let ws = serde_json::json!({ "workspace_id": long });
        assert_eq!(
            parse_app_request("workspace.select", &ws)
                .err()
                .unwrap()
                .code,
            "invalid_params"
        );
        // Exactly at the ceiling passes.
        let at = "w".repeat(MAX_PARAM_BYTES);
        let ws = serde_json::json!({ "workspace_id": at });
        assert!(parse_app_request("workspace.select", &ws).is_ok());
    }

    // ----- M5.2: browser automation (agent-browser-port subset) -----

    #[test]
    fn parses_the_automation_verbs_happy_paths() {
        let ok = serde_json::json!({});
        assert_eq!(
            parse_app_request("browser.snapshot", &ok).unwrap(),
            AppRequest::BrowserSnapshot { surface_id: None }
        );
        let eval = serde_json::json!({ "surface_id": "p4", "expression": "2+3" });
        assert_eq!(
            parse_app_request("browser.eval", &eval).unwrap(),
            AppRequest::BrowserEval {
                surface_id: Some("p4".to_owned()),
                expression: "2+3".to_owned()
            }
        );
        let click = serde_json::json!({ "ref": "@e12" });
        assert_eq!(
            parse_app_request("browser.click", &click).unwrap(),
            AppRequest::BrowserClick {
                surface_id: None,
                target: ElementTarget::Ref("@e12".to_owned())
            }
        );
        let dbl = serde_json::json!({ "surface_id": "p4", "ref": "e3" });
        assert_eq!(
            parse_app_request("browser.dblclick", &dbl).unwrap(),
            AppRequest::BrowserDblclick {
                surface_id: Some("p4".to_owned()),
                target: ElementTarget::Ref("@e3".to_owned())
            }
        );
        let typing = serde_json::json!({ "ref": "E2", "text": "hi cmux" });
        assert_eq!(
            parse_app_request("browser.type", &typing).unwrap(),
            AppRequest::BrowserType {
                surface_id: None,
                target: ElementTarget::Ref("@e2".to_owned()),
                text: "hi cmux".to_owned()
            }
        );
    }

    #[test]
    fn eval_accepts_upstreams_script_fallback() {
        // Upstream's CLI names the eval field `script`; the port treats it
        // as the fallback for `expression` (never both at once).
        let script = serde_json::json!({ "surface_id": "p4", "script": "2+3" });
        assert_eq!(
            parse_app_request("browser.eval", &script).unwrap(),
            AppRequest::BrowserEval {
                surface_id: Some("p4".to_owned()),
                expression: "2+3".to_owned()
            }
        );
        let both = serde_json::json!({ "expression": "a", "script": "b" });
        let problem = parse_app_request("browser.eval", &both).err().unwrap();
        assert!(
            problem.message.contains("mutually exclusive"),
            "{problem:?}"
        );
        let present_but_typed = serde_json::json!({ "script": 7 });
        let problem = parse_app_request("browser.eval", &present_but_typed)
            .err()
            .unwrap();
        assert!(problem.message.contains("expression"), "{problem:?}");
    }

    #[test]
    fn ref_spellings_normalize_to_canonical_form() {
        assert_eq!(normalize_ref("@e12").as_deref(), Some("@e12"));
        assert_eq!(normalize_ref("e12").as_deref(), Some("@e12"));
        assert_eq!(normalize_ref(" E12 ").as_deref(), Some("@e12"));
        assert_eq!(normalize_ref("12"), None);
        assert_eq!(normalize_ref(""), None);
        assert_eq!(normalize_ref("@"), None);
        assert_eq!(normalize_ref("@e"), None);
        assert_eq!(normalize_ref("ex"), None);
    }

    #[test]
    fn automation_verb_violations_carry_invalid_params() {
        let empty = serde_json::json!({});
        for method in ["browser.eval", "browser.click", "browser.type"] {
            let problem = parse_app_request(method, &empty).err().unwrap();
            assert_eq!(problem.code, "invalid_params", "{method}");
        }
        // `browser.snapshot` needs nothing (surface defaults to focused).
        assert!(parse_app_request("browser.snapshot", &empty).is_ok());

        let bad_ref = serde_json::json!({ "ref": "1" });
        let problem = parse_app_request("browser.click", &bad_ref).err().unwrap();
        assert!(problem.message.contains("ref"), "{problem:?}");

        let empty_expression = serde_json::json!({ "expression": "" });
        let problem = parse_app_request("browser.eval", &empty_expression)
            .err()
            .unwrap();
        assert!(problem.message.contains("expression"), "{problem:?}");

        let empty_text = serde_json::json!({ "ref": "@e1", "text": "" });
        let problem = parse_app_request("browser.type", &empty_text)
            .err()
            .unwrap();
        assert!(problem.message.contains("text"), "{problem:?}");

        // The payload budget caps the free-text fields too.
        let big = "x".repeat(MAX_PAYLOAD_BYTES + 1);
        for (method, params) in [
            ("browser.eval", serde_json::json!({ "expression": big })),
            (
                "browser.type",
                serde_json::json!({ "ref": "@e1", "text": big }),
            ),
        ] {
            assert!(parse_app_request(method, &params).is_err(), "{method}");
        }
    }

    #[test]
    fn automation_verb_methods_route_and_advertise() {
        assert!(APP_METHODS.contains(&"browser.snapshot"));
        assert!(APP_METHODS.contains(&"browser.eval"));
        assert!(APP_METHODS.contains(&"browser.click"));
        assert!(APP_METHODS.contains(&"browser.dblclick"));
        assert!(APP_METHODS.contains(&"browser.type"));
        for method in APP_METHODS {
            assert_eq!(method_route(method), MethodRoute::App, "{method}");
        }
    }

    // ----- M5.3: the observation + form/scroll family -----

    #[test]
    fn parses_element_targets_with_both_flavors() {
        let selector_req = serde_json::json!({ "selector": "#answer" });
        assert_eq!(
            parse_app_request("browser.hover", &selector_req).unwrap(),
            AppRequest::BrowserHover {
                surface_id: None,
                target: ElementTarget::Selector("#answer".to_owned())
            }
        );
        // A ref normalizes like every other ref verb; `null` flavors read
        // as absent (upstream omits vs NSNulls freely).
        let null_selector =
            serde_json::json!({ "ref": "e7", "selector": null, "text": "filled up" });
        assert_eq!(
            parse_app_request("browser.fill", &null_selector).unwrap(),
            AppRequest::BrowserFill {
                surface_id: None,
                target: ElementTarget::Ref("@e7".to_owned()),
                text: "filled up".to_owned(),
            }
        );
    }

    #[test]
    fn element_target_violations_are_invalid_params() {
        let both = serde_json::json!({ "ref": "@e1", "selector": "#a" });
        let problem = parse_app_request("browser.click", &both).err().unwrap();
        assert_eq!(problem.code, "invalid_params");
        assert!(
            problem.message.contains("mutually exclusive"),
            "{problem:?}"
        );

        let neither = serde_json::json!({});
        let problem = parse_app_request("browser.hover", &neither).err().unwrap();
        assert!(
            problem.message.contains("params.ref or params.selector"),
            "{problem:?}"
        );

        let empty_selector = serde_json::json!({ "selector": "" });
        let problem = parse_app_request("browser.get.text", &empty_selector)
            .err()
            .unwrap();
        assert!(problem.message.contains("selector"), "{problem:?}");

        // A selector is free text under the payload budget (it rides into
        // page JS as a JSON literal, never as code).
        let big = "x".repeat(MAX_PAYLOAD_BYTES + 1);
        let problem = parse_app_request("browser.focus", &serde_json::json!({ "selector": big }))
            .err()
            .unwrap();
        assert!(problem.message.contains("selector"), "{problem:?}");
    }

    #[test]
    fn parses_the_m53_verb_families_happy_paths() {
        let target = serde_json::json!({ "ref": "@e1" });
        assert_eq!(
            parse_app_request("browser.check", &target).unwrap(),
            AppRequest::BrowserCheck {
                surface_id: None,
                target: ElementTarget::Ref("@e1".to_owned()),
                checked: true
            }
        );
        assert_eq!(
            parse_app_request("browser.uncheck", &target).unwrap(),
            AppRequest::BrowserCheck {
                surface_id: None,
                target: ElementTarget::Ref("@e1".to_owned()),
                checked: false
            }
        );
        assert_eq!(
            parse_app_request(
                "browser.select",
                &serde_json::json!({ "selector": "#lang", "value": "de" })
            )
            .unwrap(),
            AppRequest::BrowserSelect {
                surface_id: None,
                target: ElementTarget::Selector("#lang".to_owned()),
                value: "de".to_owned()
            }
        );
        assert_eq!(
            parse_app_request("browser.scroll", &serde_json::json!({ "dy": 500 })).unwrap(),
            AppRequest::BrowserScroll {
                surface_id: None,
                dx: None,
                dy: Some(500.0)
            }
        );
        assert_eq!(
            parse_app_request(
                "browser.scroll",
                &serde_json::json!({ "dx": -1.5, "dy": 0 })
            )
            .unwrap(),
            AppRequest::BrowserScroll {
                surface_id: None,
                dx: Some(-1.5),
                dy: Some(0.0)
            }
        );
        assert_eq!(
            parse_app_request(
                "browser.get.attr",
                &serde_json::json!({ "selector": "#a", "attr": "placeholder" })
            )
            .unwrap(),
            AppRequest::BrowserGet {
                surface_id: None,
                target: ElementTarget::Selector("#a".to_owned()),
                field: GetField::Attr,
                attr: Some("placeholder".to_owned()),
                property: None
            }
        );
        assert_eq!(
            parse_app_request(
                "browser.get.styles",
                &serde_json::json!({ "ref": "@e3", "property": "color" })
            )
            .unwrap(),
            AppRequest::BrowserGet {
                surface_id: None,
                target: ElementTarget::Ref("@e3".to_owned()),
                field: GetField::Styles,
                attr: None,
                property: Some("color".to_owned())
            }
        );
        let bare_styles = serde_json::json!({ "selector": "#a" });
        assert_eq!(
            parse_app_request("browser.get.styles", &bare_styles).unwrap(),
            AppRequest::BrowserGet {
                surface_id: None,
                target: ElementTarget::Selector("#a".to_owned()),
                field: GetField::Styles,
                attr: None,
                property: None
            }
        );
        assert_eq!(
            parse_app_request("browser.is.checked", &target).unwrap(),
            AppRequest::BrowserIs {
                surface_id: None,
                target: ElementTarget::Ref("@e1".to_owned()),
                state: IsState::Checked
            }
        );
        assert_eq!(
            parse_app_request(
                "browser.get.title",
                &serde_json::json!({ "surface_id": "p9" })
            )
            .unwrap(),
            AppRequest::BrowserGetTitle {
                surface_id: Some("p9".to_owned())
            }
        );
        assert_eq!(
            parse_app_request("browser.screenshot", &serde_json::json!({})).unwrap(),
            AppRequest::BrowserScreenshot { surface_id: None }
        );
        assert_eq!(
            parse_app_request(
                "browser.focus_webview",
                &serde_json::json!({"surface_id": "p4"})
            )
            .unwrap(),
            AppRequest::BrowserFocusWebview {
                surface_id: Some("p4".to_owned())
            }
        );
        assert_eq!(
            parse_app_request(
                "browser.is_webview_focused",
                &serde_json::json!({"surface_id": "p4"})
            )
            .unwrap(),
            AppRequest::BrowserIsWebviewFocused {
                surface_id: Some("p4".to_owned())
            }
        );
    }

    #[test]
    fn m53_verb_violations_carry_invalid_params() {
        // scroll without deltas is meaningless.
        let empty_scroll = serde_json::json!({});
        let problem = parse_app_request("browser.scroll", &empty_scroll)
            .err()
            .unwrap();
        assert!(problem.message.contains("dx"), "{problem:?}");

        // Non-number deltas surface with the field in the message.
        let bad_delta = serde_json::json!({ "dy": "down" });
        let problem = parse_app_request("browser.scroll", &bad_delta)
            .err()
            .unwrap();
        assert!(problem.message.contains("dy"), "{problem:?}");

        // get.attr requires its attribute name; styles' property must be
        // a short string when given.
        let no_attr = serde_json::json!({ "selector": "#a" });
        let problem = parse_app_request("browser.get.attr", &no_attr)
            .err()
            .unwrap();
        assert!(problem.message.contains("attr"), "{problem:?}");
        let bad_property = serde_json::json!({ "selector": "#a", "property": true });
        let problem = parse_app_request("browser.get.styles", &bad_property)
            .err()
            .unwrap();
        assert!(problem.message.contains("property"), "{problem:?}");

        // select/fill require their free-text fields.
        let no_value = serde_json::json!({ "ref": "@e1" });
        let problem = parse_app_request("browser.select", &no_value)
            .err()
            .unwrap();
        assert!(problem.message.contains("value"), "{problem:?}");
        let no_text = serde_json::json!({ "ref": "@e1" });
        let problem = parse_app_request("browser.fill", &no_text).err().unwrap();
        assert!(problem.message.contains("text"), "{problem:?}");

        // Surface-only verbs parse bare (surface defaults to focused).
        for method in [
            "browser.get.title",
            "browser.screenshot",
            "browser.focus_webview",
            "browser.is_webview_focused",
        ] {
            assert!(
                parse_app_request(method, &serde_json::json!({})).is_ok(),
                "{method}"
            );
        }
    }

    #[test]
    fn element_target_json_echoes_the_consumed_shape() {
        assert_eq!(
            ElementTarget::Ref("@e2".to_owned()).target_json(),
            serde_json::json!({ "ref": "@e2" })
        );
        assert_eq!(
            ElementTarget::Selector("#a".to_owned()).target_json(),
            serde_json::json!({ "selector": "#a" })
        );
        // The getter/is wire names stay exact for tracing replies.
        assert_eq!(get_wire_name(GetField::Box), "browser.get.box");
        assert_eq!(is_wire_name(IsState::Visible), "browser.is.visible");
    }

    #[test]
    fn problem_envelopes_carry_the_full_shape() {
        let id = serde_json::json!(42);
        let line = problem_error(&id, &RequestProblem::invalid_params("bad"));
        assert!(line.contains(r#""id":42"#), "{line}");
        assert!(line.contains(r#""code":"invalid_params""#), "{line}");
        assert!(line.contains(r#""action":"fix the parameters""#), "{line}");
    }

    #[test]
    fn m54_key_family_parses_to_typed_requests() {
        // press / keydown / keyup parse to one BrowserKey carrying the half.
        assert_eq!(
            parse_app_request(
                "browser.press",
                &serde_json::json!({ "surface_id": "p4", "key": "Enter" })
            )
            .unwrap(),
            AppRequest::BrowserKey {
                surface_id: Some("p4".to_owned()),
                half: KeyHalf::Press,
                key: "Enter".to_owned(),
            }
        );
        for (method, half) in [
            ("browser.press", KeyHalf::Press),
            ("browser.keydown", KeyHalf::Down),
            ("browser.keyup", KeyHalf::Up),
        ] {
            let request = parse_app_request(method, &serde_json::json!({ "key": "a" })).unwrap();
            assert_eq!(
                request,
                AppRequest::BrowserKey {
                    surface_id: None,
                    half,
                    key: "a".to_owned(),
                },
                "{method}"
            );
            assert_eq!(request.method_name(), method, "{method}");
        }
        // The key token is required.
        let problem = parse_app_request("browser.press", &serde_json::json!({}))
            .err()
            .unwrap();
        assert!(problem.message.contains("params.key"), "{problem:?}");
        // …and bounded like other short params.
        let oversized = "k".repeat(MAX_PARAM_BYTES + 1);
        let problem =
            parse_app_request("browser.keydown", &serde_json::json!({ "key": oversized }))
                .err()
                .unwrap();
        assert!(problem.message.contains("param budget"), "{problem:?}");
    }

    #[test]
    fn m54_wait_parses_predicates_and_deadline() {
        // One predicate + the documented default deadline.
        assert_eq!(
            parse_app_request(
                "browser.wait",
                &serde_json::json!({ "surface_id": "p4", "selector": "#out" })
            )
            .unwrap(),
            AppRequest::BrowserWait {
                surface_id: Some("p4".to_owned()),
                selector: Some("#out".to_owned()),
                text_contains: None,
                url_contains: None,
                load_state: None,
                function: None,
                timeout_ms: DEFAULT_WAIT_MS,
            }
        );
        // The full AND shape with every wire field name upstream's CLI emits.
        assert_eq!(
            parse_app_request(
                "browser.wait",
                &serde_json::json!({
                    "text_contains": "composed",
                    "url_contains": "form",
                    "load_state": "domcontentloaded",
                    "function": "window.__x === 1",
                    "timeout_ms": 600,
                })
            )
            .unwrap(),
            AppRequest::BrowserWait {
                surface_id: None,
                selector: None,
                text_contains: Some("composed".to_owned()),
                url_contains: Some("form".to_owned()),
                load_state: Some(LoadState::DomContentLoaded),
                function: Some("window.__x === 1".to_owned()),
                timeout_ms: 600,
            }
        );
        // The load-state values the port serves.
        assert_eq!(
            parse_app_request("browser.wait", &serde_json::json!({ "load_state": "load" }))
                .unwrap(),
            AppRequest::BrowserWait {
                surface_id: None,
                selector: None,
                text_contains: None,
                url_contains: None,
                load_state: Some(LoadState::Load),
                function: None,
                timeout_ms: DEFAULT_WAIT_MS,
            }
        );
        // No predicate at all → invalid_params naming the alternatives.
        let problem = parse_app_request("browser.wait", &serde_json::json!({}))
            .err()
            .unwrap();
        assert!(problem.message.contains("params.selector"), "{problem:?}");
        // An unknown load state is rejected at parse.
        let bad_state = serde_json::json!({ "load_state": "networkidle" });
        let problem = parse_app_request("browser.wait", &bad_state).err().unwrap();
        assert!(problem.message.contains("load_state"), "{problem:?}");
        // The deadline must be a non-negative integer under the cap.
        let fractional = serde_json::json!({ "text_contains": "x", "timeout_ms": 12.5 });
        let problem = parse_app_request("browser.wait", &fractional)
            .err()
            .unwrap();
        assert!(problem.message.contains("integer"), "{problem:?}");
        let negative = serde_json::json!({ "text_contains": "x", "timeout_ms": -1 });
        let problem = parse_app_request("browser.wait", &negative).err().unwrap();
        assert!(problem.message.contains("negative"), "{problem:?}");
        let capped = serde_json::json!({ "text_contains": "x", "timeout_ms": MAX_WAIT_MS + 1 });
        let problem = parse_app_request("browser.wait", &capped).err().unwrap();
        assert!(problem.message.contains("cap"), "{problem:?}");
        // The deadline problem shape the app handler answers with.
        let timeout = RequestProblem::wait_timeout(600);
        assert_eq!(timeout.code, "timeout");
        assert_eq!(
            timeout.action,
            "retry with a longer timeout or a weaker predicate"
        );
    }
}
