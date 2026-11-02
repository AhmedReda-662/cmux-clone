//! `cmux` — the port's CLI, M3.3: the direct hook transport (slice A) and
//! the claude settings surface (M3.3b), plus the M4.1 automation verbs
//! over the same pipe (`workspace`/`surface` subcommands, `send`,
//! `read-screen`, `identify`).
//!
//! Commands (upstream `CLI/cmux.swift` surface, port subset):
//! - `cmux ping` — probe the running app's control pipe; exit 0 only when
//!   it answers, exit 1 otherwise.
//! - `cmux hooks enqueue <agent> <sub>` — ship one hook payload over
//!   stdin. **Always** exits 0; on any failure it prints `{}` so a broken
//!   integration never stalls an agent run (upstream admission behavior).
//! - `cmux hooks feed --source <agent>` — upstream's direct feed verb,
//!   funneled through the same admission queue.
//! - `cmux hooks codex install [--yes] | uninstall [--yes]` — install or
//!   remove codex hooks anchors in the agent's config (marker-block only).
//! - `cmux hooks claude inject-settings` — print the generated claude
//!   settings document (no socket contact; the consumer stores it and
//!   launches claude with `--settings <file>`).
//! - `cmux hooks claude inbox-drain|inbox-wait|auto-name|cron-create-guard`
//!   — fail-open `{}` stubs until their app-side backends land (M4+;
//!   upstream marks them async/non-decisional, so this cannot stall or
//!   rewake the agent).
//!
//! Divergences (ADR-0011 amendments, documented): the admission gating
//! upstream performs in the hook's shell guard (surface-id presence,
//! per-agent disable var) runs here inside the client; there is no spool
//! fast path in this slice.
//!
//! M6 warm-up (`cmux browser …`): the upstream agent-browser CLI grammar
//! over the port's landed wire methods — navigation, snapshot/eval, the
//! element family (click/hover/type/fill/select/check/scroll, refs or
//! selectors), the key family (`press|key|keydown|keyup --key`), the
//! wait family, `get`/`is`, screenshot, and the webview focus checks.
//! Upstream-only verbs without a port wire answer a clear unsupported
//! message naming the recorded residual (ADR-0006 Amendment 6).

use std::io::{Read, Write as IoWrite};
use std::time::Duration;

use cmux_hooks::{CodexInstallPlan, CodexUninstallPlan};
use cmux_ipc::protocol::{self, MAX_REQUEST_BYTES};

fn main() {
    std::process::exit(run());
}

/// M4.2 global flags (upstream CLI contract): `--socket <pipe>` and
/// `--password <pw>` may appear before or after the verb. The pipe value
/// wins over `CMUX_SOCKET_PATH`; the password (explicit →
/// `CMUX_SOCKET_PASSWORD` env → DPAPI file) drives the login probe the CLI
/// sends along every connection.
struct Globals {
    socket: Option<String>,
    password: Option<String>,
}

static GLOBALS: std::sync::OnceLock<Globals> = std::sync::OnceLock::new();

/// Extracts `--socket`/`--password` from anywhere in the argument list
/// (`--flag value` and `--flag=value` forms); everything else stays in
/// order for the verb parsers. Values are trimmed; a blank value counts as
/// absent (a password never matches itself in trim-away form).
fn parse_globals(raw: &[String]) -> Vec<String> {
    let mut socket: Option<String> = None;
    let mut password: Option<String> = None;
    let mut rest = Vec::new();
    let mut pending: Option<&str> = None;
    for argument in raw {
        if let Some(name) = pending {
            match name {
                "--socket" => socket = Some(normalized_global(argument)),
                _ => password = Some(normalized_global(argument)),
            }
            pending = None;
            continue;
        }
        if let Some(inline) = argument.strip_prefix("--socket=") {
            socket = Some(normalized_global(inline));
        } else if let Some(inline) = argument.strip_prefix("--password=") {
            password = Some(normalized_global(inline));
        } else if argument == "--socket" {
            pending = Some("--socket");
        } else if argument == "--password" {
            pending = Some("--password");
        } else {
            rest.push(argument.clone());
        }
    }
    // A trailing flag with no value stays absent (never a crash).
    let _ = GLOBALS.set(Globals { socket, password });
    rest
}

fn normalized_global(value: &str) -> String {
    value.trim().to_owned()
}

fn global_socket() -> Option<&'static str> {
    GLOBALS.get().and_then(|g| g.socket.as_deref())
}

fn global_password() -> Option<&'static str> {
    GLOBALS.get().and_then(|g| g.password.as_deref())
}

fn run() -> i32 {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<String> = parse_globals(&raw);
    match args.as_slice() {
        [single] if single == "ping" => cmd_ping(),
        [hooks, enqueue, agent, sub] if hooks == "hooks" && enqueue == "enqueue" => {
            cmd_enqueue(agent, sub)
        }
        // Upstream `hooks feed --source <agent>`: the direct feed verb —
        // the port funnels it through the same admission queue (ADR-0011
        // amendment 4) and fails open like every hook input.
        [hooks, feed, source, agent]
            if hooks == "hooks" && feed == "feed" && source == "--source" =>
        {
            cmd_enqueue(agent, "feed")
        }
        [hooks, agent, action, rest @ ..] if hooks == "hooks" && agent == "codex" => {
            match action.as_str() {
                "install" => cmd_codex_install(rest),
                "uninstall" => cmd_codex_uninstall(rest),
                _ => usage(),
            }
        }
        [hooks, agent, action, rest @ ..] if hooks == "hooks" && agent == "claude" => {
            match action.as_str() {
                // No-socket verb: print the generated settings document
                // (upstream `emitClaudeWrapperInjectSettings`).
                "inject-settings" => cmd_claude_inject_settings(),
                // The PATH-shim target (M3.3c): exec the real claude,
                // injecting the settings document when this runs inside a
                // live cmux pane; plainly (pass-through) otherwise.
                "launch" => cmd_claude_launch(rest),
                // Agent-messages / team verbs whose app-side backends do
                // not exist in the port yet (M4+). They are async or
                // non-decisional upstream, so printing the fail-open `{}`
                // and exiting 0 reproduces their absent-feature behavior
                // without ever stalling or rewaking the agent.
                "inbox-drain" | "inbox-wait" | "auto-name" | "cron-create-guard" => {
                    fail_open_exit()
                }
                _ => usage(),
            }
        }
        // M4.1 automation surface (upstream v2 methods over the pipe):
        // identity/discovery, workspace + surface listing/addressing,
        // typed text, and the visible-screen reader.
        [verb, rest @ ..] if verb == "workspace" => cmd_workspace(rest),
        [verb, rest @ ..] if verb == "surface" => cmd_surface(rest),
        [verb, rest @ ..] if verb == "send" => cmd_send(rest),
        [verb, rest @ ..] if verb == "read-screen" => cmd_read_screen(rest),
        // M6 slice 1: the config-file custom commands (upstream surfaces
        // them through the palette; the port drives them over the wire).
        [verb, rest @ ..] if verb == "command" => cmd_command(rest),
        // M6 (ADR-0014): the SSH verb — a new workspace whose pane child
        // is the Windows OpenSSH client (upstream `workspace.ssh.open`'s
        // plain-session subset; the managed carrier stays deferred).
        [verb, rest @ ..] if verb == "ssh" => cmd_ssh(rest),
        // M6 warm-up: the upstream browser automation grammar over the
        // port's landed wire methods (see the module header + ADR-0006
        // Amendment 6).
        [verb, rest @ ..] if verb == "browser" => cmd_browser(rest),
        // Design lane 5: the agent activity stream pane (`agent.open_split`
        // wires it). Upstream has no `agent` CLI verb — the pane is the
        // design file's agent surface, recorded as a port extension.
        [verb, rest @ ..] if verb == "agent" => cmd_agent(rest),
        [verb, rest @ ..] if verb == "identify" => {
            let json_mode = rest == ["--json"];
            if !(rest.is_empty() || json_mode) {
                return usage();
            }
            cmd_call("system.identify", &serde_json::json!({}), json_mode)
        }
        _ => usage(),
    }
}

fn usage() -> i32 {
    eprintln!(
        "usage:\n  cmux [--socket <pipe>] [--password <pw>] ping\n  cmux [--socket <pipe>] [--password <pw>] identify [--json]\n  … the same globals work with every other verb\n  cmux ping\n  cmux identify [--json]\n  cmux workspace list|new|current [--json]\n  cmux workspace select <workspace_id> [--json]\n  cmux surface list [--workspace <id>] [--json]\n  cmux send [--surface <id|index|ref>] <text...>\n  cmux read-screen [--surface <id|index|ref>] [--json]\n  cmux command list [--json]\n  cmux command run <name> [--surface <id|index|ref>] [--yes] [--json]\n  cmux ssh [--port <1-65535>] [--identity <path>] [--name <title>] [--ssh-option <opt>] [--command <cmd>] [--no-focus] [--json] <destination> [remote args...] (after `--` all args go to the remote command)\n  cmux browser [--surface <spec>] open|open-split|new [url] [--json]\n  cmux browser [--surface <spec>] navigate|back|forward|reload|url|snapshot\n  cmux browser [--surface <spec>] eval <js> | wait [--selector|--text|--url-contains|--load-state|--function|--timeout-ms]\n  cmux browser [--surface <spec>] click|dblclick|hover|focus|check|uncheck|scroll-into-view <selector|@ref>\n  cmux browser [--surface <spec>] type|fill <selector|@ref> <text> | select <sel> <value> | scroll [--dx N] [--dy N]\n  cmux browser [--surface <spec>] press|keydown|keyup <key> | screenshot [--out <path>]\n  cmux browser [--surface <spec>] get url|title|text|html|value|attr|count|box|styles | is visible|enabled|checked\n  cmux browser [--surface <spec>] focus-webview | is-webview-focused
  cmux agent [--surface <spec>] open [--json]   (design lane 5 agent activity pane; the stream is read-only)\n  cmux hooks enqueue <agent> <subcommand>   (payload on stdin)\n  cmux hooks feed --source <agent>             (payload on stdin)\n  cmux hooks codex install [--yes]\n  cmux hooks codex uninstall [--yes]\n  cmux hooks claude inject-settings            (prints the settings document)\n  cmux hooks claude launch [claude args...]    (PATH-shim target)",
    );
    2
}

/// Upstream discovery order: `--socket` → `CMUX_SOCKET_PATH` → the
/// per-user pipe name (the port's single candidate).
fn resolve_pipe_name() -> Option<String> {
    if let Some(flagged) = global_socket() {
        let trimmed = flagged.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_owned());
        }
    }
    if let Ok(value) = std::env::var("CMUX_SOCKET_PATH") {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_owned());
        }
    }
    cmux_ipc::default_pipe_name().ok()
}

/// Upstream `SocketPasswordResolver.resolve`: `--password` favorite,
/// `CMUX_SOCKET_PASSWORD` env, then the DPAPI password file.
fn resolve_password() -> Option<String> {
    let explicit = global_password();
    cmux_ipc::password::configured_password(explicit)
}

fn cmd_ping() -> i32 {
    let Some(pipe) = resolve_pipe_name() else {
        eprintln!("cmux ping: could not resolve the control pipe");
        return 1;
    };
    let request = r#"{"id":"ping","method":"ping","params":{}}"#;
    match make_request(&pipe, request) {
        Ok(reply) => {
            println!("{reply}");
            if reply_ok(&reply) {
                0
            } else {
                1
            }
        }
        Err(error) => {
            eprintln!("cmux ping: {error}");
            1
        }
    }
}

/// Sends one envelope and returns the raw reply line (or a call failure
/// description). The shared plumbing of every M4 automation verb.
/// `call_raw` with a per-call response budget (the wait verb).
fn call_raw_budgeted(
    method: &str,
    params: &serde_json::Value,
    response_budget: Duration,
) -> Result<String, String> {
    let Some(pipe) = resolve_pipe_name() else {
        return Err(
            "could not resolve the control pipe (CMUX_SOCKET_PATH or the per-user default)"
                .to_owned(),
        );
    };
    let envelope = serde_json::json!({ "id": "cli", "method": method, "params": params });
    let request = serde_json::to_string(&envelope).unwrap_or_default();
    make_request_budgeted(&pipe, &request, response_budget).map_err(|error| error.to_string())
}

/// Prints one success value per upstream CLI conventions: `--json` keeps
/// the complete response line (scripts parse it), the plain mode prints
/// the result body only. Non-ok replies fail to stderr and exit 1.
fn cmd_call(method: &str, params: &serde_json::Value, json_mode: bool) -> i32 {
    cmd_call_budgeted(
        method,
        params,
        json_mode,
        method,
        cmux_ipc::DEFAULT_RESPONSE_BUDGET,
    )
}

/// `cmd_call` with a per-call display name (browser verbs report
/// `cmux browser click: ...` instead of the wire spelling) and response
/// budget override (the wait verb).
fn cmd_call_budgeted(
    method: &str,
    params: &serde_json::Value,
    json_mode: bool,
    display: &str,
    response_budget: Duration,
) -> i32 {
    let reply = match call_raw_budgeted(method, params, response_budget) {
        Ok(reply) => reply,
        Err(problem) => {
            if json_mode {
                println!("null");
            }
            eprintln!("cmux {display}: {problem}");
            return 1;
        }
    };
    let value: serde_json::Value = match serde_json::from_str(&reply) {
        Ok(value) => value,
        Err(_) => {
            eprintln!("cmux {display}: unreadable reply: {reply}");
            return 1;
        }
    };
    if value.get("ok").and_then(serde_json::Value::as_bool) == Some(true) {
        if json_mode {
            println!("{reply}");
        } else {
            println!(
                "{}",
                value
                    .get("result")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null)
            );
        }
        0
    } else {
        if json_mode {
            println!("{reply}");
        }
        let error = &value["error"];
        eprintln!(
            "cmux {display}: {}: {} (action: {})",
            error
                .get("code")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("error"),
            error
                .get("message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(""),
            error
                .get("action")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(""),
        );
        1
    }
}

/// Extracts one string field out of a successful reply's result.
fn result_string(reply: &str, key: &str) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(reply).ok()?;
    if value.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        return None;
    }
    value
        .get("result")?
        .get(key)?
        .as_str()
        .map(ToOwned::to_owned)
}

/// `cmux workspace <sub>`: list/new/current/select over `workspace.*`.
fn cmd_workspace(args: &[String]) -> i32 {
    let mut json_mode = false;
    let mut flags: Vec<String> = Vec::new();
    for argument in args {
        match argument.as_str() {
            "--json" => json_mode = true,
            other => flags.push(other.to_owned()),
        }
    }
    let [sub, rest @ ..] = flags.as_slice() else {
        return usage();
    };
    match sub.as_str() {
        "list" if rest.is_empty() => cmd_call("workspace.list", &serde_json::json!({}), json_mode),
        "new" | "create" if rest.is_empty() => {
            cmd_call("workspace.create", &serde_json::json!({}), json_mode)
        }
        "current" if rest.is_empty() => {
            cmd_call("workspace.current", &serde_json::json!({}), json_mode)
        }
        "select" => {
            let Some(workspace_id) = rest.first() else {
                return usage();
            };
            cmd_call(
                "workspace.select",
                &serde_json::json!({ "workspace_id": workspace_id }),
                json_mode,
            )
        }
        _ => usage(),
    }
}

/// `cmux surface list [--workspace <id>] [--json]`.
fn cmd_surface(args: &[String]) -> i32 {
    let mut json_mode = false;
    let mut workspace: Option<String> = None;
    let mut positional: Vec<String> = Vec::new();
    let mut drain_workspace = false;
    for argument in args {
        if drain_workspace {
            workspace = Some(argument.clone());
            drain_workspace = false;
            continue;
        }
        match argument.as_str() {
            "--json" => json_mode = true,
            "--workspace" => drain_workspace = true,
            other => positional.push(other.to_owned()),
        }
    }
    if positional.as_slice() != ["list"] {
        return usage();
    }
    let params = match workspace {
        Some(id) => serde_json::json!({ "workspace_id": id }),
        None => serde_json::json!({}),
    };
    cmd_call("surface.list", &params, json_mode)
}

/// `cmux send [--surface <id|index|ref>] <text...>` — types the text into
/// the (focused) surface as keystrokes. Upstream's `--paste` path is not
/// in this slice.
fn cmd_send(args: &[String]) -> i32 {
    let (surface, text) = match send_flags(args) {
        Ok(parts) => parts,
        Err(()) => return usage(),
    };
    let text = text.join(" ");
    if text.is_empty() {
        eprintln!("cmux send: no text given");
        return 2;
    }
    let params = match surface {
        Some(id) => serde_json::json!({ "surface_id": id, "text": text }),
        None => serde_json::json!({ "text": text }),
    };
    cmd_call("surface.send_text", &params, false)
}

/// `cmux read-screen [--surface <id|index|ref>] [--json]` — the visible
/// screen text. Plain mode prints the raw text (the field scripts
/// consume); `--json` keeps the complete response (the upstream
/// `--json` contract).
fn cmd_read_screen(args: &[String]) -> i32 {
    let mut surface: Option<String> = None;
    let mut json_mode = false;
    let mut drain_surface = false;
    for argument in args {
        if drain_surface {
            surface = Some(argument.clone());
            drain_surface = false;
            continue;
        }
        match argument.as_str() {
            "--json" => json_mode = true,
            "--surface" => drain_surface = true,
            _ => return usage(),
        }
    }
    let params = match surface {
        Some(id) => serde_json::json!({ "surface_id": id }),
        None => serde_json::json!({}),
    };
    let reply = match call_raw_budgeted(
        "surface.read_text",
        &params,
        cmux_ipc::DEFAULT_RESPONSE_BUDGET,
    ) {
        Ok(reply) => reply,
        Err(problem) => {
            eprintln!("cmux read-screen: {problem}");
            return 1;
        }
    };
    if json_mode {
        println!("{reply}");
        if reply_ok(&reply) {
            0
        } else {
            1
        }
    } else {
        match result_string(&reply, "text") {
            Some(text) => {
                println!("{text}");
                0
            }
            None => {
                // A structured error (or nothing) still fails loudly.
                let value: serde_json::Value =
                    serde_json::from_str(&reply).unwrap_or(serde_json::Value::Null);
                eprintln!(
                    "cmux read-screen: {}: {}",
                    value["error"]["code"].as_str().unwrap_or("error"),
                    value["error"]["message"].as_str().unwrap_or("no result"),
                );
                1
            }
        }
    }
}

/// `cmux command list [--json] | cmux command run <name> [--surface <spec>] [--yes] [--json]`
/// — the M6 slice-1 custom-command wrappers. `list` mirrors the config
/// file order with each entry's runable flavor; `run` types the named
/// command plus a newline (upstream's palette behavior), refusing
/// `confirm: true` entries unless the caller passes `--yes`.
fn cmd_command(args: &[String]) -> i32 {
    let (verb, rest) = match args.split_first() {
        Some((verb, rest)) => (verb.as_str(), rest),
        None => return usage(),
    };
    match verb {
        "list" => cmd_command_list(rest),
        "run" => cmd_command_run(rest),
        other => {
            eprintln!("cmux command: unknown subcommand '{other}'");
            usage()
        }
    }
}

/// The command-verb flag scan: `--surface <spec>` (when allowed),
/// `--yes`, `--json`; any other `--...` token is a strict use error
/// (the browser grammar's rejection style). Non-dash tokens are the
/// subcommand's positionals.
#[derive(Default)]
struct CommandFlags {
    surface: Option<String>,
    yes: bool,
    json: bool,
}

fn split_command_flags(
    args: &[String],
    allow_surface: bool,
    allow_yes: bool,
) -> Result<(CommandFlags, Vec<String>), ()> {
    let mut flags = CommandFlags::default();
    let mut positionals = Vec::new();
    let mut drain_surface = false;
    for token in args {
        if drain_surface {
            flags.surface = Some(token.clone());
            drain_surface = false;
            continue;
        }
        match token.as_str() {
            "--surface" if allow_surface => drain_surface = true,
            "--yes" if allow_yes => flags.yes = true,
            "--json" => flags.json = true,
            other if !other.starts_with("--") => positionals.push(other.to_owned()),
            other => {
                eprintln!("cmux command: unrecognized option {other}");
                return Err(());
            }
        }
    }
    if drain_surface {
        eprintln!("cmux command: --surface needs a value");
        return Err(());
    }
    Ok((flags, positionals))
}

/// Empty for `list`; `run` uses it for the command name and `--surface`/
/// `--yes`/`--json`. Both wrap the app's config-directed verbs.
fn cmd_command_list(args: &[String]) -> i32 {
    let (flags, positionals) = match split_command_flags(args, false, false) {
        Ok(parts) => parts,
        // The scanner printed the specific one-line grammar error; the
        // browser verbs' option rejection exits 1, never a usage dump.
        Err(()) => return 1,
    };
    if !positionals.is_empty() {
        return usage();
    }
    let reply = match call_raw_budgeted(
        "command.list",
        &serde_json::json!({}),
        cmux_ipc::DEFAULT_RESPONSE_BUDGET,
    ) {
        Ok(reply) => reply,
        Err(problem) => {
            eprintln!("cmux command list: {problem}");
            return 1;
        }
    };
    if flags.json {
        println!("{reply}");
        return if reply_ok(&reply) { 0 } else { 1 };
    }
    let value: serde_json::Value = serde_json::from_str(&reply).unwrap_or(serde_json::Value::Null);
    if !reply_ok(&reply) {
        eprintln!(
            "cmux command list: {}: {}",
            value["error"]["code"].as_str().unwrap_or("error"),
            value["error"]["message"].as_str().unwrap_or("no result"),
        );
        return 1;
    }
    let Some(rows) = value["result"]
        .get("commands")
        .and_then(|rows| rows.as_array())
    else {
        eprintln!("cmux command list: the app reply carries no command rows");
        return 1;
    };
    for entry in rows {
        let name = entry["name"].as_str().unwrap_or("");
        let description = entry["description"].as_str().unwrap_or("");
        let confirm = entry["confirm"].as_bool().unwrap_or(false);
        let kind = entry["kind"].as_str().unwrap_or("terminal");
        let command = entry["command"].as_str().unwrap_or("");
        let mut line = String::from(name);
        if !description.is_empty() {
            line.push_str(" - ");
            line.push_str(description);
        }
        if confirm {
            line.push_str(" [confirm]");
        }
        if kind == "workspace" {
            line.push_str(" (workspace launch - not supported by the port)");
        } else if !command.is_empty() {
            line.push_str("  # ");
            line.push_str(command);
        }
        println!("{line}");
    }
    0
}

fn cmd_command_run(args: &[String]) -> i32 {
    let (flags, positionals) = match split_command_flags(args, true, true) {
        Ok(parts) => parts,
        // See cmd_command_list: scanner errors are one-line + exit 1.
        Err(()) => return 1,
    };
    let Some(name) = positionals.first() else {
        eprintln!("cmux command: usage: cmux command run <name> [--surface <spec>] [--yes]");
        return usage();
    };
    if positionals.len() > 1 {
        eprintln!("cmux command: only one command name may be run");
        return usage();
    }
    let mut params = serde_json::json!({ "name": name });
    if let Some(surface) = &flags.surface {
        params["surface_id"] = serde_json::json!(surface);
    }
    if flags.yes {
        params["ack"] = serde_json::json!(true);
    }
    let reply = match call_raw_budgeted("command.run", &params, cmux_ipc::DEFAULT_RESPONSE_BUDGET) {
        Ok(reply) => reply,
        Err(problem) => {
            eprintln!("cmux command: {problem}");
            return 1;
        }
    };
    if flags.json {
        println!("{reply}");
        return if reply_ok(&reply) { 0 } else { 1 };
    }
    let value: serde_json::Value = serde_json::from_str(&reply).unwrap_or(serde_json::Value::Null);
    if !reply_ok(&reply) {
        eprintln!(
            "cmux command: {}: {}",
            value["error"]["code"].as_str().unwrap_or("error"),
            value["error"]["message"].as_str().unwrap_or("no result"),
        );
        return 1;
    }
    println!(
        "OK name={} surface={}",
        name,
        value["result"]["surface_id"].as_str().unwrap_or("")
    );
    0
}

/// `cmux ssh ...` (M6, ADR-0014): opens a workspace whose pane child is
/// the Windows OpenSSH client. Mirrors upstream `parseSSHCommandOptions`'s
/// flag grammar for the flags the port's subset supports (verbatim
/// refusal texts), forwards `workspace.ssh.open` over the wire, and prints
/// like every other M4 verb (`--json` keeps the reply, plain mode prints
/// the payload). Upstream's managed carrier — relay bootstrap, the
/// `auth_required` interactive-login retry, `--window`/`--transport`
/// routing — is not part of the subset; those flags refuse with the
/// port's not-supported line.
fn cmd_ssh(args: &[String]) -> i32 {
    let json_mode = args.iter().any(|arg| arg == "--json");
    let filtered: Vec<String> = args
        .iter()
        .filter(|arg| arg.as_str() != "--json")
        .cloned()
        .collect();
    let invocation = match cmd_ssh_parse(&filtered) {
        Ok(invocation) => invocation,
        Err(exit_code) => return exit_code,
    };
    let mut params = serde_json::json!({
        "destination": invocation.destination,
        "ssh_options": invocation.options,
        "focus": invocation.focus,
    });
    if let Some(port) = invocation.port {
        params["port"] = serde_json::json!(port);
    }
    if let Some(identity_file) = invocation.identity_file {
        params["identity_file"] = serde_json::json!(identity_file);
    }
    if let Some(title) = invocation.name {
        params["title"] = serde_json::json!(title);
    }
    if let Some(initial_command) = invocation.initial_command {
        params["initial_command"] = serde_json::json!(initial_command);
    }
    cmd_call_budgeted(
        "workspace.ssh.open",
        &params,
        json_mode,
        "ssh",
        cmux_ipc::DEFAULT_RESPONSE_BUDGET,
    )
}

/// The parsed ssh invocation; field shapes mirror
/// `CLI/cmux.swift` `SSHCommandOptions` for the supported subset.
struct SshInvocation {
    destination: String,
    options: Vec<String>,
    port: Option<u16>,
    identity_file: Option<String>,
    name: Option<String>,
    initial_command: Option<String>,
    focus: bool,
}

/// One-line refusal + exit 1, the port's CLI grammar for parse
/// failures (upstream throws `CLIError(message:)`).
fn ssh_refuse(message: &str) -> i32 {
    eprintln!("cmux ssh: {message}");
    1
}

/// Upstream `CmuxTuiRemoteRouting.focusFlagValue`: case-insensitive,
/// true/1/yes and false/0/no only.
fn ssh_focus_value(token: &str) -> Option<bool> {
    match token.to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" => Some(true),
        "false" | "0" | "no" => Some(false),
        _ => None,
    }
}

/// Upstream `isMixedTTYOptionCluster`: a leading `-abc` cluster is mixed
/// when it carries a TTY flag next to non-TTY flags (`T`/`t` beside any
/// other letter — a single-letter cluster can never be both).
fn ssh_mixed_tty_cluster(argument: &str) -> bool {
    let Some(rest) = argument.strip_prefix('-') else {
        return false;
    };
    if rest.starts_with('-') {
        return false;
    }
    let has_tty = rest.chars().any(|c| c == 't' || c == 'T');
    let has_other = rest.chars().any(|c| c != 't' && c != 'T');
    has_tty && has_other
}

/// Upstream `SSHRemoteCommandTTYRequest`: the effective OpenSSH
/// `RequestTTY` state while applying ordered `-t`/`-T` flags.
#[derive(Clone, Copy, PartialEq)]
enum SshTtyRequest {
    Automatic,
    Disabled,
    Enabled,
    Forced,
}

impl SshTtyRequest {
    /// Upstream `init(optionValue:)` parsing.
    fn from_option_value(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "no" | "false" => Self::Disabled,
            "yes" | "true" => Self::Enabled,
            "force" => Self::Forced,
            _ => Self::Automatic,
        }
    }

    fn option_value(self) -> &'static str {
        match self {
            Self::Automatic => "auto",
            Self::Disabled => "no",
            Self::Enabled => "yes",
            Self::Forced => "force",
        }
    }
}

/// Splits an `Key=Value` ssh option; `None` for anything else.
fn ssh_option_key_value(option: &str) -> Option<(&str, &str)> {
    let (key, value) = option.split_once('=')?;
    let key = key.trim();
    if key.is_empty() {
        return None;
    }
    Some((key, value.trim()))
}

/// `cmd_ssh`'s parser: the supported subset of upstream's flag scan with
/// its verbatim refusal texts.
fn cmd_ssh_parse(args: &[String]) -> Result<SshInvocation, i32> {
    let mut destination: Option<String> = None;
    let mut options: Vec<String> = Vec::new();
    let mut port: Option<u16> = None;
    let mut identity_file: Option<String> = None;
    let mut name: Option<String> = None;
    let mut command: Option<String> = None;
    let mut focus = true;
    let mut undelimited: Vec<String> = Vec::new();
    let mut delimited: Option<Vec<String>> = None;
    let mut passthrough = false;

    let mut index = 0;
    while index < args.len() {
        let argument = args[index].as_str();
        if passthrough {
            delimited
                .as_mut()
                .expect("delimiter flag opens the slot")
                .push(argument.to_owned());
            index += 1;
            continue;
        }
        match argument {
            "--" => {
                passthrough = true;
                delimited = Some(Vec::new());
                index += 1;
            }
            "--port" => {
                let Some(value) = args.get(index + 1) else {
                    return Err(ssh_refuse("--port requires a value"));
                };
                let Ok(parsed) = value.parse::<u32>() else {
                    return Err(ssh_refuse("--port must be 1-65535"));
                };
                if !(1..=65535).contains(&parsed) {
                    return Err(ssh_refuse("--port must be 1-65535"));
                }
                port = Some(parsed as u16);
                index += 2;
            }
            "--identity" => {
                let Some(value) = args.get(index + 1) else {
                    return Err(ssh_refuse("--identity requires a path"));
                };
                identity_file = Some(value.clone());
                index += 2;
            }
            "--name" => {
                let Some(value) = args.get(index + 1) else {
                    return Err(ssh_refuse("--name requires a workspace title"));
                };
                name = Some(value.clone());
                index += 2;
            }
            "--command" => {
                let Some(value) = args.get(index + 1) else {
                    return Err(ssh_refuse("--command requires non-empty command text"));
                };
                if value.trim().is_empty() {
                    return Err(ssh_refuse("--command requires non-empty command text"));
                }
                command = Some(value.clone());
                index += 2;
            }
            "--ssh-option" => {
                let Some(value) = args.get(index + 1) else {
                    return Err(ssh_refuse("--ssh-option requires a value"));
                };
                let trimmed = value.trim();
                if !trimmed.is_empty() {
                    options.push(trimmed.to_owned());
                }
                index += 2;
            }
            "--no-focus" => {
                focus = false;
                index += 1;
            }
            "--focus" => match args.get(index + 1).and_then(|v| ssh_focus_value(v)) {
                Some(value) => {
                    focus = value;
                    index += 2;
                }
                None => {
                    focus = true;
                    index += 1;
                }
            },
            other if other.starts_with("--focus=") => {
                let Some(value) = ssh_focus_value(&other["--focus=".len()..]) else {
                    return Err(ssh_refuse("--focus takes true or false"));
                };
                focus = value;
                index += 1;
            }
            "-A" | "--forward-agent" | "-a" | "--no-forward-agent" => {
                // Upstream consumes these switches for its sharing config
                // and carries the request into the session; the subset
                // leaves forwarding to ssh's own config (Windows OpenSSH
                // honors it) — the flags parse for grammar fidelity.
                index += 1;
            }
            "--window" | "--transport" | "--session" => {
                return Err(ssh_refuse(&format!(
                    "{argument} is not supported by the port"
                )));
            }
            other => {
                // Upstream's default case: an unknown `--` flag refuses
                // everywhere in the scan; a single-dash token sets the
                // destination, or rides as a remote argument after it.
                if other.starts_with("--") {
                    return Err(ssh_refuse(&format!("unknown flag '{other}'")));
                }
                if destination.is_none() {
                    if other.starts_with('-') {
                        return Err(ssh_refuse(
                            "destination must be <user@host>. Use --port/--identity/\
                             --ssh-option for SSH flags and `--` for remote command args.",
                        ));
                    }
                    destination = Some(other.to_owned());
                } else {
                    undelimited.push(other.to_owned());
                }
                index += 1;
            }
        }
    }

    let Some(destination) = destination else {
        return Err(ssh_refuse(
            "requires a destination (example: cmux ssh user@host)",
        ));
    };

    // Upstream guards the leading `-`-run of the undelimited remote
    // arguments against mixed short-option clusters (verbatim text).
    for cluster in undelimited
        .iter()
        .take_while(|token| token.starts_with('-'))
    {
        if ssh_mixed_tty_cluster(cluster) {
            return Err(ssh_refuse(&format!(
                "mixed short-option cluster '{cluster}' is not supported after the \
                 destination; use --ssh-option for SSH flags or -- for remote command \
                 arguments"
            )));
        }
    }

    // Upstream `SSHRemoteCommand`: a leading pure `-t`/`-T` run is
    // OpenSSH TTY configuration, not remote text; everything else (plus
    // the always-literal `--` side) is the remote command. A `-tX`-style
    // mixed cluster was refused above; `-x`-style non-TTY tokens stop the
    // TTY run and ride in the command.
    let tty_run_count = undelimited
        .iter()
        .take_while(|token| {
            token.len() > 1
                && token.starts_with('-')
                && token[1..].chars().all(|c| c == 't' || c == 'T')
        })
        .count();
    let mut command_arguments = undelimited[tty_run_count..].to_vec();
    if let Some(values) = &delimited {
        command_arguments.extend(values.iter().cloned());
    }
    if !command_arguments.is_empty() && command.is_some() {
        return Err(ssh_refuse(
            "--command cannot be combined with trailing remote command arguments",
        ));
    }
    let initial_command = if !command_arguments.is_empty() {
        Some(command_arguments.join(" "))
    } else {
        command
    };

    // The wire carries only option strings, so a recognized TTY run is
    // durably encoded as its effective `RequestTTY` value — upstream's
    // `sshOptionsPersistingTTYRequest` state machine, including the
    // rule that an existing RequestTTY option is dropped and replaced
    // (the first `-o RequestTTY` wins per ssh config semantics).
    if tty_run_count > 0 {
        let mut base: Option<SshTtyRequest> = None;
        let mut rebuilt = Vec::with_capacity(options.len() + 1);
        for option in &options {
            let tty_value = ssh_option_key_value(option)
                .filter(|(key, _)| key.eq_ignore_ascii_case("RequestTTY"))
                .map(|(_, value)| value.to_owned());
            match tty_value {
                Some(value) if base.is_none() => {
                    base = Some(SshTtyRequest::from_option_value(&value));
                }
                Some(_) => {}
                None => rebuilt.push(option.clone()),
            }
        }
        let mut request = base.unwrap_or(SshTtyRequest::Automatic);
        for token in &undelimited[..tty_run_count] {
            for flag in token[1..].chars() {
                if flag == 'T' {
                    request = SshTtyRequest::Disabled;
                } else if flag == 't' {
                    request = if request == SshTtyRequest::Enabled {
                        SshTtyRequest::Forced
                    } else {
                        SshTtyRequest::Enabled
                    };
                }
            }
        }
        rebuilt.push(format!("RequestTTY={}", request.option_value()));
        options = rebuilt;
    }

    Ok(SshInvocation {
        destination,
        options,
        port,
        identity_file,
        name,
        initial_command,
        focus,
    })
}

/// Splits `--surface <spec>` from the remaining text tokens.
fn send_flags(args: &[String]) -> Result<(Option<String>, Vec<String>), ()> {
    let mut surface: Option<String> = None;
    let mut text: Vec<String> = Vec::new();
    let mut drain_surface = false;
    for argument in args {
        if drain_surface {
            surface = Some(argument.clone());
            drain_surface = false;
            continue;
        }
        match argument.as_str() {
            "--surface" => drain_surface = true,
            other => text.push(other.to_owned()),
        }
    }
    if drain_surface {
        return Err(());
    }
    Ok((surface, text))
}

/// `true` when the reply JSON carries `ok: true` (the only success reply
/// the server sends). An `ok: false` answer (for example the ancestry
/// gate) is a failed probe, not a transport hang-up.
fn reply_ok(reply: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(reply)
        .ok()
        .and_then(|value| value.get("ok").and_then(serde_json::Value::as_bool))
        .unwrap_or(false)
}

fn make_request(pipe: &str, request: &str) -> Result<String, cmux_ipc::IpcError> {
    make_request_budgeted(pipe, request, cmux_ipc::DEFAULT_RESPONSE_BUDGET)
}

/// Same plumbing with a caller-chosen response budget. Only the wait verb
/// needs it: the app plane enforces its own reply budget first (400 ms for
/// the fast verbs, the wait deadline plus headroom for `browser.wait`), so
/// the CLI just adds transport slack.
fn make_request_budgeted(
    pipe: &str,
    request: &str,
    response_budget: Duration,
) -> Result<String, cmux_ipc::IpcError> {
    match resolve_password() {
        // Upstream `authenticateClientIfNeeded`: one connection, the V1
        // `auth <pw>` probe, then the request; an `ERROR: Unknown command
        // 'auth'` reply is tolerated (the server is not in password mode).
        Some(password) => cmux_ipc::request_with_auth(
            pipe,
            Some(&format!("auth {password}")),
            request,
            cmux_ipc::DEFAULT_CONNECT_BUDGET,
            response_budget,
        ),
        None => cmux_ipc::request(
            pipe,
            request,
            cmux_ipc::DEFAULT_CONNECT_BUDGET,
            response_budget,
        ),
    }
}

/// The fail-open answer: hooks must never stall an agent.
fn fail_open_exit() -> i32 {
    println!("{{}}");
    0
}

fn cmd_enqueue(agent: &str, sub: &str) -> i32 {
    // Spawn gate 1: without a surface id the event is meaningless — the
    // shell guard upstream performs this before the CLI even runs.
    if env_trimmed("CMUX_SURFACE_ID").is_none() {
        return fail_open_exit();
    }
    // Spawn gate 2: the per-agent disable variable.
    if let Some(def) = cmux_hooks::hook_def(agent) {
        if env_trimmed(def.disable_env_var) == Some("1".to_owned()) {
            return fail_open_exit();
        }
    } else {
        // Unknown agent: never block the agent's run.
        return fail_open_exit();
    }

    let Some(pipe) = resolve_pipe_name() else {
        return fail_open_exit();
    };

    // Payload: stdin, ≤ 1 MiB envelope ceiling, 1 s EOF budget; any transport
    // or parse trouble fail-opens.
    let payload = match read_stdin_budgeted(Duration::from_secs(1)) {
        Some(payload) => payload,
        None => return fail_open_exit(),
    };
    let payload: serde_json::Value = match serde_json::from_str(payload.as_str())
        .or_else(|_| serde_json::from_str(payload.trim()))
    {
        Ok(payload) => payload,
        Err(_) => return fail_open_exit(),
    };

    // Environment allow-list + routed agent pid (with parent fallback).
    let parent_pid = cmux_ipc::ancestry::current_process_parent_pid();
    let environment = protocol::routed_env_for(agent, |key| std::env::var(key).ok(), parent_pid);

    let params = serde_json::json!({
        "agent": agent,
        "subcommand": sub,
        "payload": payload,
        "environment": environment,
    });
    let line = serde_json::json!({
        "id": "hook",
        "method": "agent.hook.enqueue",
        "params": params,
    });
    let request = serde_json::to_string(&line).unwrap_or_default();
    // Note: `cmux_ipc::request` appends the line terminator; MAX caps the
    // envelope itself.
    if request.len() > MAX_REQUEST_BYTES {
        return fail_open_exit();
    }

    match make_request(&pipe, &request) {
        Ok(reply) => {
            // The agent runner reads stdout as the hook result; only an
            // admitted reply (`ok: true`) passes through. Anything else —
            // transport failure, ancestry rejection, validation error —
            // fails open with `{}` at exit 0 (upstream admission rule).
            if reply_ok(&reply) {
                println!("{reply}");
            } else {
                println!("{{}}");
            }
            0
        }
        Err(_) => fail_open_exit(),
    }
}

/// `cmux hooks claude inject-settings` (upstream
/// `emitClaudeWrapperInjectSettings`): prints the complete cmux-owned
/// claude settings document (compact, sorted, slashes unescaped) without
/// contacting the app. The wrapper (or the shell's spawn transform) stores
/// it and launches the agent with `--settings <file>`.
fn cmd_claude_inject_settings() -> i32 {
    let bin = claude_hook_bin();
    println!("{}", cmux_hooks::claude::settings_doc_string(&bin));
    0
}

/// The absolute CLI path the settings document's hook commands point at:
/// `CMUX_CLAUDE_HOOK_CMUX_BIN` wins (upstream's env key), else this
/// executable.
fn claude_hook_bin() -> std::path::PathBuf {
    if let Ok(value) = std::env::var("CMUX_CLAUDE_HOOK_CMUX_BIN") {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return std::path::PathBuf::from(trimmed);
        }
    }
    std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("cmux.exe"))
}

/// `cmux hooks claude launch [claude args...]` (M3.3c) — the contract the
/// PATH shim delegates to (upstream's `cmux-claude-wrapper` executable
/// equivalent). Inside a live cmux pane it stores the settings document
/// and execs the real claude with `--settings <doc>` plus the forwarded
/// args, waiting and forwarding the exit code. Everywhere else it execs
/// the real claude plainly: no surface id, the disable variable set, a
/// dead cmux (ping fails), a doc-unwritable state, or an unresolvable
/// settings location all degrade to pass-through; claude unresolvable on
/// PATH exits 127 loudly (never re-invoking the shim).
fn cmd_claude_launch(args: &[String]) -> i32 {
    let Some(real) = real_claude_program() else {
        eprintln!("cmux hooks claude launch: no real claude found on PATH (the shim is skipped)");
        return 127;
    };

    let in_pane = env_trimmed("CMUX_SURFACE_ID").is_some() && disabled_var_is_not_set();
    if !in_pane {
        return launch_plain(&real, args);
    }
    let Some(pipe) = env_trimmed("CMUX_SOCKET_PATH") else {
        return launch_plain(&real, args);
    };
    // Upstream's wrapper pings the socket (~0.75 s) and passes through
    // when cmux is not alive; a named-pipe miss answers fast.
    let ping_request = r#"{"id":"ping","method":"ping","params":{}}"#;
    let live = make_request(&pipe, ping_request)
        .map(|reply| reply_ok(&reply))
        .unwrap_or(false);
    if !live {
        return launch_plain(&real, args);
    }

    let Some(base) = cmux_hooks::claude::settings_dir() else {
        return launch_plain(&real, args);
    };
    match cmux_hooks::claude::write_settings_file(&claude_hook_bin(), &base) {
        Ok(doc) => run_claude(
            &real,
            std::iter::once("--settings".to_owned())
                .chain(std::iter::once(doc.to_string_lossy().into_owned()))
                .chain(args.iter().cloned())
                .collect::<Vec<String>>(),
        ),
        Err(_) => launch_plain(&real, args),
    }
}

/// Resolves the real claude along this process' PATH, skipping the shim
/// directory (which is first on a pane PATH — skipping it is the re-entry
/// proof).
fn real_claude_program() -> Option<std::path::PathBuf> {
    let skip = cmux_hooks::claude::settings_dir()
        .as_deref()
        .map(|base| vec![cmux_hooks::claude::shim_dir(base)])
        .unwrap_or_default();
    let path_value = std::env::var("PATH").ok()?;
    cmux_hooks::claude::resolve_real_claude(&path_value, &skip)
}

fn disabled_var_is_not_set() -> bool {
    env_trimmed("CMUX_CLAUDE_HOOKS_DISABLED").is_none()
}

/// Plain pass-through: no documented settings, just the real program and
/// the user's args.
fn launch_plain(real: &std::path::Path, args: &[String]) -> i32 {
    run_claude(real, args.to_vec())
}

/// Spawns claude inheriting stdio (the pane's ConPTY carries the output),
/// waits, and forwards the exit code.
fn run_claude(real: &std::path::Path, args: Vec<String>) -> i32 {
    match std::process::Command::new(real).args(args).status() {
        Ok(status) => status.code().unwrap_or(1),
        Err(error) => {
            eprintln!(
                "cmux hooks claude launch: spawning {} failed: {error}",
                real.display()
            );
            127
        }
    }
}

fn env_trimmed(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

/// Reads stdin until EOF with a bounded wait (upstream budgets the hook
/// payload read too); `None` = give up and fail open on `{}`.
fn read_stdin_budgeted(budget: Duration) -> Option<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("cmux-stdin".to_owned())
        .spawn(move || {
            let mut limited = Vec::new();
            let mut stdin = std::io::stdin().lock();
            loop {
                let mut chunk = [0u8; 8192];
                match stdin.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => {
                        limited.extend_from_slice(&chunk[..n]);
                        if limited.len() > MAX_REQUEST_BYTES {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            let _ = tx.send(String::from_utf8_lossy(&limited).into_owned());
        })
        .ok()?;
    rx.recv_timeout(budget).ok()
}

fn bool_flag(rest: &[String], flag: &str) -> bool {
    rest.iter().any(|a| a == flag)
}

fn cmd_codex_install(rest: &[String]) -> i32 {
    let Some(hooks) = cmux_hooks::CodexHooks::resolve(None) else {
        eprintln!("Required agent configuration is missing. Run `cmux hooks codex install` after installing your agent CLI.");
        return 1;
    };
    let plan = match hooks.plan_install() {
        Ok(plan) => plan,
        Err(error) => {
            eprintln!("{error}");
            return 1;
        }
    };
    let CodexInstallPlan::Write {
        hooks_json_path,
        config_toml_path,
        hooks_content,
        config_content,
        config_toml_existed,
    } = plan
    else {
        println!(
            "Codex hooks already up to date at {}",
            hooks.hooks_json_path().display()
        );
        return 0;
    };
    print_codex_install_preview(
        &hooks_json_path,
        &config_toml_path,
        &hooks_content,
        &config_content,
        config_toml_existed,
    );
    if !bool_flag(rest, "--yes") && !confirm("Install Codex hooks?") {
        println!("(no changes made)");
        return 0;
    }
    write_text(&hooks_json_path, &hooks_content);
    if config_toml_existed {
        write_text(&config_toml_path, &config_content);
    }
    println!(
        "Installed cmux hooks for Codex at {}",
        hooks_json_path.display()
    );
    0
}

fn write_text(path: &std::path::Path, content: &str) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(error) = std::fs::write(path, content) {
        eprintln!("cmux: writing {} failed: {error}", path.display());
    }
}

fn print_codex_install_preview(
    hooks_path: &std::path::Path,
    config_path: &std::path::Path,
    hooks_content: &str,
    config_content: &str,
    config_toml_existed: bool,
) {
    println!("cmux will write:");
    println!("  {}:", hooks_path.display());
    println!("{}", indent(hooks_content));
    if config_toml_existed {
        println!("  {}:", config_path.display());
        println!("{}", indent(config_content));
    } else {
        println!(
            "  {}: not present — hooks.json installs without config.toml (codex may prompt to trust the hooks)",
            config_path.display()
        );
    }
}

fn indent(text: &str) -> String {
    text.lines()
        .map(|l| format!("    {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn confirm(question: &str) -> bool {
    print!("{question} [y/N] ");
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer).is_err() {
        return false;
    }
    matches!(answer.trim(), "y" | "Y")
}

fn cmd_codex_uninstall(rest: &[String]) -> i32 {
    let Some(hooks) = cmux_hooks::CodexHooks::resolve(None) else {
        eprintln!("Required agent configuration is missing.");
        return 1;
    };
    let plan = match hooks.plan_uninstall() {
        Ok(plan) => plan,
        Err(error) => {
            eprintln!("{error}");
            return 1;
        }
    };
    let CodexUninstallPlan::Remove {
        hooks_json_path,
        hooks_content,
        removed,
        config_toml,
    } = plan
    else {
        println!(
            "Nothing to uninstall (no hooks.json at {})",
            hooks.hooks_json_path().display()
        );
        return 0;
    };
    if removed == 0 && config_toml.is_none() {
        println!(
            "Nothing to uninstall (no cmux hook entries at {})",
            hooks_json_path.display()
        );
        return 0;
    }
    println!("cmux will remove {removed} cmux hook(s) from:");
    println!("  {}", hooks_json_path.display());
    if let Some((path, content)) = &config_toml {
        println!("  and edit {}", path.display());
        println!("{}", indent(content));
    }
    if !bool_flag(rest, "--yes") && !confirm("Uninstall Codex hooks?") {
        println!("(no changes made)");
        return 0;
    }
    write_text(&hooks_json_path, &hooks_content);
    if let Some((path, content)) = &config_toml {
        write_text(path, content);
    }
    println!(
        "Removed {removed} cmux hook(s) from {}",
        hooks_json_path.display()
    );
    0
}

// =====================================================================
// M6 warm-up: `cmux browser` (upstream `runBrowserCommand` grammar).
// The grammar mirrors cmux-main/CLI/cmux.swift over the port's landed
// wire methods; every unsupported upstream verb answers a clear message
// naming the recorded residual. Design + divergences: ADR-0006
// Amendment 6. Wire shapes: cmux-ipc protocol.rs.
// =====================================================================

/// Upstream verbs whose backends the port has not built; every entry
/// maps to a recorded parity residual (ADR-0006 amendments /
/// feature-parity rows 18-19). The CLI rejects each one before handle
/// extraction so scripts get a plain rejection instead of upstream's
/// silent misroute of the token into the surface position.
const UNSUPPORTED_BROWSER_VERBS: &[&str] = &[
    "identify",
    "import",
    "profile",
    "profiles",
    "react-grab",
    "reactgrab",
    "devtools",
    "dev-tools",
    "focus-mode",
    "design-mode",
    "zoom",
    "history",
    "repl",
    "find",
    "frame",
    "dialog",
    "download",
    "cookies",
    "storage",
    "tab",
    "console",
    "errors",
    "highlight",
    "state",
    "addinitscript",
    "addscript",
    "addstyle",
    "viewport",
    "geolocation",
    "geo",
    "offline",
    "trace",
    "network",
    "screencast",
    "input",
    "set-input-files",
];

/// Upstream verbs that may run without a leading surface handle. The
/// port supports exactly these three; every other scoped verb requires
/// a handle (upstream grammar) even though the wire itself would
/// default to the focused pane when the param is omitted.
const BROWSER_VERBS_WITHOUT_SURFACE: &[&str] = &["open", "open-split", "new"];

/// `browser.wait` CLI grace: the app answers a deadline hit at
/// `deadline + 8s` (the upstream-derived headroom the pump enforces), so
/// the client sits one second above it - a deadline must surface as the
/// app's structured timeout reply, never as a CLI socket timeout
/// (ADR-0006 Amendment 6).
const CLI_WAIT_GRACE_MS: u64 = 9_000;

/// Upstream's wait budget floor: `max(15s, ms/1000 + 8s)`.
const CLI_WAIT_FLOOR_MS: u64 = 15_000;

/// The CLI-side response budget for one `browser.wait` call.
fn wait_response_budget(wait_ms: u64) -> Duration {
    Duration::from_millis((wait_ms + CLI_WAIT_GRACE_MS).max(CLI_WAIT_FLOOR_MS))
}

/// How a successful plain-mode reply prints for one verb (`--json`
/// always prints the complete reply line, as upstream does).
#[derive(Debug, PartialEq)]
enum BrowserPrint {
    /// A fixed `OK` line (action verbs, upstream fallbacks).
    Ok,
    /// One string field printed raw; a missing field prints empty
    /// (upstream's `?? ""`).
    Str(&'static str),
    /// One read field: strings raw, scalars plain, objects as compact
    /// JSON, `null` spelled out (upstream's BrowserValueTextFormatter
    /// roles for reads). Note `get.count` reads `count`.
    Json(&'static str),
    /// `browser.open_split`'s
    /// `OK surface=<sid> pane=<pid> placement=split` - the port always
    /// creates a split (no reuse path on the wire).
    OpenSplit,
    /// `browser.screenshot`: with `--out`, copy the app-written file and
    /// print `OK <dest>`; without it, print the written path.
    Screenshot(Option<std::path::PathBuf>),
}

/// Sends one browser verb and prints its reply: plain mode per the
/// verb's [`BrowserPrint`], `--json` prints the full reply line. Exit 0
/// on ok, 1 on transport failure, structured errors, or copy failure.
fn browser_call(
    display: &str,
    method: &'static str,
    params: &serde_json::Value,
    print: BrowserPrint,
    json_mode: bool,
    response_budget: Duration,
) -> i32 {
    let reply = match call_raw_budgeted(method, params, response_budget) {
        Ok(reply) => reply,
        Err(problem) => {
            if json_mode {
                println!("null");
            }
            eprintln!("cmux {display}: {problem}");
            return 1;
        }
    };
    let value: serde_json::Value = match serde_json::from_str(&reply) {
        Ok(value) => value,
        Err(_) => {
            eprintln!("cmux {display}: unreadable reply: {reply}");
            return 1;
        }
    };
    if value.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        if json_mode {
            println!("{reply}");
        }
        let error = &value["error"];
        eprintln!(
            "cmux {display}: {}: {} (action: {})",
            error
                .get("code")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("error"),
            error
                .get("message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(""),
            error
                .get("action")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(""),
        );
        return 1;
    }
    if json_mode {
        println!("{reply}");
        return 0;
    }
    let result = value
        .get("result")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    match print {
        BrowserPrint::Ok => println!("OK"),
        BrowserPrint::Str(key) => println!(
            "{}",
            result
                .get(key)
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
        ),
        BrowserPrint::Json(key) => println!(
            "{}",
            jsonish(result.get(key).unwrap_or(&serde_json::Value::Null))
        ),
        BrowserPrint::OpenSplit => {
            let surface = result
                .get("surface_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown");
            let pane = result
                .get("pane_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown");
            println!("OK surface={surface} pane={pane} placement=split");
        }
        BrowserPrint::Screenshot(out) => match out {
            Some(destination) => {
                let Some(source) = result.get("path").and_then(serde_json::Value::as_str) else {
                    eprintln!("cmux {display}: the reply carries no screenshot path");
                    return 1;
                };
                if let Some(parent) = destination.parent() {
                    if let Err(error) = std::fs::create_dir_all(parent) {
                        eprintln!(
                            "cmux {display}: creating {} failed: {error}",
                            parent.display()
                        );
                        return 1;
                    }
                }
                match std::fs::copy(source, &destination) {
                    Ok(_) => println!("OK {}", destination.display()),
                    Err(error) => {
                        eprintln!(
                            "cmux {display}: copying {source} to {} failed: {error}",
                            destination.display()
                        );
                        return 1;
                    }
                }
            }
            None => println!(
                "{}",
                result
                    .get("path")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
            ),
        },
    }
    0
}

/// One reply field, printed the way scripts read it: strings raw (no
/// quotes), numbers/bools plain, `null` spelled out, everything else as
/// compact JSON.
fn jsonish(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Null => "null".to_owned(),
        serde_json::Value::Bool(flag) => flag.to_string(),
        serde_json::Value::Number(number) => number.to_string(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// Splits one verb's arguments the way upstream's strict per-verb
/// validator does: `--name value` / `--name=value` only for declared
/// value options, everything else positional. Unknown options are
/// refused - a stray flag is never folded into a URL or selector
/// (upstream CLI error behavior). The port grammar has no bare flags:
/// upstream's one boolean (`--snapshot-after`) is not on the wire, so
/// a `--name` that is not a declared value option is always an error.
fn parse_browser_options(args: &[String], value_options: &[&str]) -> Result<BrowserParser, String> {
    let mut parser = BrowserParser::default();
    let mut iter = args.iter();
    while let Some(argument) = iter.next() {
        let token = argument.as_str();
        let Some(rest) = token.strip_prefix("--") else {
            parser.positionals.push(token.to_owned());
            continue;
        };
        if rest.is_empty() {
            parser.positionals.push(token.to_owned());
            continue;
        }
        let (name, inline) = match rest.split_once('=') {
            Some((name, value)) => (name, Some(value.to_owned())),
            None => (rest, None),
        };
        if value_options.contains(&name) {
            let value = match inline {
                Some(value) => value,
                None => match iter.next() {
                    Some(value) => value.clone(),
                    None => return Err(format!("browser: option --{name} requires a value")),
                },
            };
            parser.values.push((name.to_owned(), value));
        } else {
            return Err(format!(
                "browser: unrecognized option --{name} (options the port has not built are refused, never folded into the command)"
            ));
        }
    }
    Ok(parser)
}

#[derive(Debug, Default, PartialEq)]
struct BrowserParser {
    values: Vec<(String, String)>,
    positionals: Vec<String>,
}

impl BrowserParser {
    fn value(&self, name: &str) -> Option<&str> {
        self.values
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// Builds the wire's strict element-target object from one caller token.
/// Upstream sends the token as `selector`; the port's target grammar is
/// ref-XOR-selector, so a canonical ref (`@e<digits>`) routes to the
/// `ref` key. A bare `e<digits>` stays CSS (that is also a valid type
/// selector; the CLI routes only `@`-spelled tokens).
fn element_target(token: &str) -> Result<serde_json::Value, String> {
    if token.starts_with('@') {
        return match cmux_ipc::protocol::normalize_ref(token) {
            Some(canonical) => Ok(serde_json::json!({ "ref": canonical })),
            None => Err(format!(
                "browser: '{token}' looks like a ref but is not (@e<digits>)"
            )),
        };
    }
    Ok(serde_json::json!({ "selector": token }))
}

/// Merges an element-target object's one key into the params object.
fn with_target(mut params: serde_json::Value, target: serde_json::Value) -> serde_json::Value {
    if let (Some(map), Some(object)) = (params.as_object_mut(), target.as_object()) {
        for (key, value) in object {
            map.insert(key.clone(), value.clone());
        }
    }
    params
}

fn click_family_method(subcommand: &str) -> Option<&'static str> {
    match subcommand {
        "click" => Some("browser.click"),
        "dblclick" => Some("browser.dblclick"),
        "hover" => Some("browser.hover"),
        "focus" => Some("browser.focus"),
        "check" => Some("browser.check"),
        "uncheck" => Some("browser.uncheck"),
        "scrollintoview" | "scrollinto" | "scroll-into-view" => Some("browser.scroll_into_view"),
        _ => None,
    }
}

fn key_half_method(subcommand: &str) -> Option<&'static str> {
    match subcommand {
        "press" | "key" => Some("browser.press"),
        "keydown" => Some("browser.keydown"),
        "keyup" => Some("browser.keyup"),
        _ => None,
    }
}

fn get_field_method(field: &str) -> Option<&'static str> {
    match field {
        "text" => Some("browser.get.text"),
        "html" => Some("browser.get.html"),
        "value" => Some("browser.get.value"),
        "attr" => Some("browser.get.attr"),
        "count" => Some("browser.get.count"),
        "box" => Some("browser.get.box"),
        "styles" => Some("browser.get.styles"),
        _ => None,
    }
}

fn is_state_method(state: &str) -> Option<&'static str> {
    match state {
        "visible" => Some("browser.is.visible"),
        "enabled" => Some("browser.is.enabled"),
        "checked" => Some("browser.is.checked"),
        _ => None,
    }
}

/// How the first positional token behaves before the verb slot: it is a
/// surface handle unless it starts with a flag dash, names a
/// handle-free verb, or names an unsupported verb. Unsupported tokens
/// fall through to the verb slot and answer a plain rejection below;
/// argument slots such as selectors or URLs are never verb-scanned
/// (`get count input` and `open history` must keep working).
fn first_token_is_surface(token: &str) -> bool {
    !token.starts_with('-')
        && !BROWSER_VERBS_WITHOUT_SURFACE.contains(&token.to_ascii_lowercase().as_str())
        && !UNSUPPORTED_BROWSER_VERBS.contains(&token.to_ascii_lowercase().as_str())
}

/// Scoped verbs demand a surface handle: `browser <surface> <verb> ...`
/// or `--surface <spec>` (upstream grammar; the wire itself would let
/// the param default to the focused pane - the CLI stays explicit so
/// scripts never silently drive focus).
fn require_surface_verb(subcommand: &str, surface: &Option<String>) -> Result<String, String> {
    surface.clone().ok_or_else(|| {
        format!(
            "browser {subcommand} requires a surface handle (use: browser <surface> {subcommand} ... or --surface)"
        )
    })
}

/// `cmux browser <args...>`: the entry point behind every grammar error.
/// Design lane 5's agent activity pane: `cmux agent [--surface <spec>]
/// open [--json]` splits an agent activity stream pane next to the
/// focused (or named) surface. Upstream has no `cmux agent` verb — the
/// pane is the design file's agent surface, recorded as a port
/// extension (docs/research/figma-design-reference.md section 13).
fn cmd_agent(args: &[String]) -> i32 {
    match run_agent(args) {
        Ok(code) => code,
        Err(message) => {
            eprintln!("cmux: {message}");
            1
        }
    }
}

fn run_agent(args: &[String]) -> Result<i32, String> {
    // Display + surface flags follow the browser verb's conventions
    // (`--json` anywhere, `--surface <spec>` or `--surface=<spec>`).
    let mut json_mode = false;
    let mut surface: Option<String> = None;
    let mut tokens: Vec<String> = Vec::new();
    let mut drain_surface = false;
    for argument in args {
        let token = argument.as_str();
        if token == "--json" {
            json_mode = true;
            continue;
        }
        if drain_surface {
            surface = Some(token.trim().to_owned());
            drain_surface = false;
            continue;
        }
        if let Some(inline) = token.strip_prefix("--surface=") {
            surface = Some(inline.trim().to_owned());
        } else if token == "--surface" {
            drain_surface = true;
        } else {
            tokens.push(token.to_owned());
        }
    }
    if drain_surface {
        return Err("--surface requires a surface handle".to_owned());
    }
    let Some(subcommand) = tokens.first() else {
        return Err(
            "agent requires a subcommand (usage: cmux agent [--surface <spec>] open [--json])"
                .to_owned(),
        );
    };
    let subcommand = subcommand.to_ascii_lowercase();
    if subcommand != "open" {
        return Err(format!(
            "agent {subcommand}: not supported by the port (the stream pane is read-only; see docs/research/figma-design-reference.md section 13)"
        ));
    }
    if tokens.len() > 1 {
        return Err("agent open takes no positional arguments".to_owned());
    }
    let mut params = serde_json::json!({});
    if let Some(handle) = &surface {
        params["surface_id"] = serde_json::Value::String(handle.clone());
    }
    Ok(browser_call(
        "agent open",
        "agent.open_split",
        &params,
        BrowserPrint::OpenSplit,
        json_mode,
        cmux_ipc::DEFAULT_RESPONSE_BUDGET,
    ))
}

fn cmd_browser(args: &[String]) -> i32 {
    match run_browser(args) {
        Ok(code) => code,
        Err(message) => {
            eprintln!("cmux: {message}");
            1
        }
    }
}

fn run_browser(args: &[String]) -> Result<i32, String> {
    // Display flags first: `--json` anywhere (upstream strips trailing
    // display flags; anywhere is a safe superset), `--id-format` refused
    // - port ids are plain strings, no ref/uuid formatting exists.
    let mut json_mode = false;
    let mut rest: Vec<&String> = Vec::new();
    for argument in args {
        let token = argument.as_str();
        if token == "--json" {
            json_mode = true;
            continue;
        }
        if token == "--id-format" || token.starts_with("--id-format=") {
            return Err(
                "--id-format is not supported by the port (ids are plain strings)".to_owned(),
            );
        }
        rest.push(argument);
    }

    // `--surface <spec>` / `--surface=<spec>`.
    let mut surface: Option<String> = None;
    let mut tokens: Vec<String> = Vec::new();
    let mut drain_surface = false;
    for argument in &rest {
        let token = argument.as_str();
        if drain_surface {
            surface = Some(token.trim().to_owned());
            drain_surface = false;
            continue;
        }
        if let Some(inline) = token.strip_prefix("--surface=") {
            surface = Some(inline.trim().to_owned());
        } else if token == "--surface" {
            drain_surface = true;
        } else {
            tokens.push(token.to_owned());
        }
    }
    if drain_surface {
        return Err("--surface requires a surface handle".to_owned());
    }

    // A leading positional handle answers `browser <surface> <verb> ...`
    // unless the verb is one of the handle-free ones or the token is an
    // unsupported verb (the latter falls through to the verb slot so it
    // answers a plain rejection below - argument slots are never verb
    // scans: selectors and URLs may legitimately spell like one).
    let mut verb_iter = tokens.into_iter().peekable();
    if surface.is_none()
        && verb_iter
            .peek()
            .is_some_and(|token| first_token_is_surface(token))
    {
        surface = Some(verb_iter.next().unwrap_or_default().trim().to_owned());
    }
    let Some(subcommand_raw) = verb_iter.next() else {
        return Err(format!(
            "browser requires a subcommand (usage: {})",
            BROWSER_VERBS_WITHOUT_SURFACE.join(", ")
        ));
    };
    let subcommand = subcommand_raw.to_ascii_lowercase();
    if UNSUPPORTED_BROWSER_VERBS.contains(&subcommand.as_str()) {
        return Err(format!(
            "browser {subcommand}: not supported by the port (recorded residual; see docs/architecture/feature-parity.md)"
        ));
    }
    let rest: Vec<String> = verb_iter.collect();
    let display = format!("browser {subcommand}");

    match subcommand.as_str() {
        // ---- the handle-free open family -------------------------------
        "open" | "open-split" | "new" => {
            // None of upstream's routing flags (--workspace/--window/
            // --focus/--profile) exist on the port wire; refuse them
            // visibly instead of folding tokens into the URL (upstream
            // refuses strays the same way).
            if let Some(stray) = rest.iter().find(|token| token.starts_with("--")) {
                return Err(format!(
                    "browser {subcommand} does not support {stray} (recorded residual)"
                ));
            }
            let url = rest.join(" ").trim().to_owned();
            // Upstream ergonomics: with a source surface, `open` means
            // navigate that surface.
            if let Some(handle) = &surface {
                if url.is_empty() {
                    return Err("browser <surface> open requires a URL".to_owned());
                }
                let params = serde_json::json!({ "surface_id": handle, "url": url });
                return Ok(browser_call(
                    &display,
                    "browser.navigate",
                    &params,
                    BrowserPrint::Ok,
                    json_mode,
                    cmux_ipc::DEFAULT_RESPONSE_BUDGET,
                ));
            }
            let mut params = serde_json::json!({});
            if !url.is_empty() {
                params["url"] = serde_json::Value::String(url);
            }
            Ok(browser_call(
                &display,
                "browser.open_split",
                &params,
                BrowserPrint::OpenSplit,
                json_mode,
                cmux_ipc::DEFAULT_RESPONSE_BUDGET,
            ))
        }
        // ---- navigation + page reads -----------------------------------
        "navigate" | "goto" => {
            let handle = require_surface_verb(&subcommand, &surface)?;
            let parser = parse_browser_options(&rest, &[])?;
            let url = parser.positionals.join(" ").trim().to_owned();
            if url.is_empty() {
                return Err(format!("browser {subcommand} requires a URL"));
            }
            let params = serde_json::json!({ "surface_id": handle, "url": url });
            Ok(browser_call(
                &display,
                "browser.navigate",
                &params,
                BrowserPrint::Ok,
                json_mode,
                cmux_ipc::DEFAULT_RESPONSE_BUDGET,
            ))
        }
        "back" | "forward" | "reload" => {
            let handle = require_surface_verb(&subcommand, &surface)?;
            // Strict flags, ignored strays (upstream shape).
            parse_browser_options(&rest, &[])?;
            let params = serde_json::json!({ "surface_id": handle });
            let method = match subcommand.as_str() {
                "back" => "browser.back",
                "forward" => "browser.forward",
                _ => "browser.reload",
            };
            Ok(browser_call(
                &display,
                method,
                &params,
                BrowserPrint::Ok,
                json_mode,
                cmux_ipc::DEFAULT_RESPONSE_BUDGET,
            ))
        }
        "url" | "get-url" => {
            let handle = require_surface_verb(&subcommand, &surface)?;
            let params = serde_json::json!({ "surface_id": handle });
            Ok(browser_call(
                &display,
                "browser.url.get",
                &params,
                BrowserPrint::Str("url"),
                json_mode,
                cmux_ipc::DEFAULT_RESPONSE_BUDGET,
            ))
        }
        "snapshot" => {
            let handle = require_surface_verb(&subcommand, &surface)?;
            // Upstream's --selector/--max-depth/--interactive/--cursor/
            // --compact are not on the port wire; the strict parser
            // refuses them instead of dropping them.
            parse_browser_options(&rest, &[])?;
            let params = serde_json::json!({ "surface_id": handle });
            Ok(browser_call(
                &display,
                "browser.snapshot",
                &params,
                BrowserPrint::Str("text"),
                json_mode,
                cmux_ipc::DEFAULT_RESPONSE_BUDGET,
            ))
        }
        "eval" => {
            let handle = require_surface_verb(&subcommand, &surface)?;
            let parser = parse_browser_options(&rest, &["script"])?;
            let script = match parser.value("script") {
                Some(script) => script.to_owned(),
                None => parser.positionals.join(" "),
            };
            let script = script.trim().to_owned();
            if script.is_empty() {
                return Err("browser eval requires a script".to_owned());
            }
            // `script` is the upstream field name; the wire accepts it
            // alongside the port's canonical `expression` (ADR-0006
            // Amendment 4 reconciliation).
            let params = serde_json::json!({ "surface_id": handle, "script": script });
            Ok(browser_call(
                &display,
                "browser.eval",
                &params,
                BrowserPrint::Json("value"),
                json_mode,
                cmux_ipc::DEFAULT_RESPONSE_BUDGET,
            ))
        }
        // ---- the wait family --------------------------------------------
        "wait" => {
            let handle = require_surface_verb(&subcommand, &surface)?;
            let parser = parse_browser_options(
                &rest,
                &[
                    "selector",
                    "text",
                    "url-contains",
                    "url",
                    "load-state",
                    "function",
                    "timeout-ms",
                    "timeout",
                ],
            )?;
            let mut params = serde_json::json!({ "surface_id": handle });
            let mut extras = parser.positionals.as_slice();
            let selector = match parser.value("selector") {
                Some(value) => Some(value.to_owned()),
                None => match extras.split_first() {
                    Some((first, remaining)) => {
                        extras = remaining;
                        Some(first.clone())
                    }
                    None => None,
                },
            };
            if let Some(token) = extras.first() {
                return Err(format!("browser {subcommand}: unexpected argument {token}"));
            }
            if let Some(selector) = selector {
                params["selector"] = serde_json::Value::String(selector);
            }
            if let Some(text) = parser.value("text") {
                params["text_contains"] = serde_json::Value::String(text.to_owned());
            }
            if let Some(url_contains) = parser.value("url-contains").or_else(|| parser.value("url"))
            {
                params["url_contains"] = serde_json::Value::String(url_contains.to_owned());
            }
            if let Some(load_state) = parser.value("load-state") {
                params["load_state"] = serde_json::Value::String(load_state.to_owned());
            }
            if let Some(function) = parser.value("function") {
                params["function"] = serde_json::Value::String(function.to_owned());
            }
            let max_wait = cmux_ipc::protocol::MAX_WAIT_MS as u64;
            let wait_ms: u64 = if let Some(raw) = parser.value("timeout-ms") {
                let ms: u32 = raw.parse().map_err(|_| "--timeout-ms must be an integer")?;
                if ms as u64 > max_wait {
                    return Err(format!("--timeout-ms exceeds the {max_wait} ms cap"));
                }
                ms as u64
            } else if let Some(raw) = parser.value("timeout") {
                let seconds: f64 = raw.parse().map_err(|_| "--timeout must be a number")?;
                if !seconds.is_finite() || seconds < 0.0 {
                    return Err("--timeout must be a number".to_owned());
                }
                let ms = ((seconds * 1000.0) as u64).max(1);
                if ms > max_wait {
                    return Err(format!("--timeout exceeds the {max_wait} ms cap"));
                }
                ms
            } else {
                // The wire enforces upstream's 5000 ms default.
                cmux_ipc::protocol::DEFAULT_WAIT_MS as u64
            };
            params["timeout_ms"] = serde_json::json!(wait_ms);
            Ok(browser_call(
                &display,
                "browser.wait",
                &params,
                BrowserPrint::Ok,
                json_mode,
                wait_response_budget(wait_ms),
            ))
        }
        // ---- the key family ---------------------------------------------
        "press" | "key" | "keydown" | "keyup" => {
            let handle = require_surface_verb(&subcommand, &surface)?;
            let parser = parse_browser_options(&rest, &["key"])?;
            let mut extras = parser.positionals.as_slice();
            let key = match parser.value("key") {
                Some(value) => Some(value.to_owned()),
                None => match extras.split_first() {
                    Some((first, remaining)) => {
                        extras = remaining;
                        Some(first.clone())
                    }
                    None => None,
                },
            };
            let Some(key) = key else {
                return Err(format!("browser {subcommand} requires a key"));
            };
            if let Some(token) = extras.first() {
                return Err(format!("browser {subcommand}: unexpected argument {token}"));
            }
            // The match arm guarantees a family hit.
            let method = key_half_method(&subcommand).unwrap_or("browser.press");
            let params = serde_json::json!({ "surface_id": handle, "key": key });
            Ok(browser_call(
                &display,
                method,
                &params,
                BrowserPrint::Ok,
                json_mode,
                cmux_ipc::DEFAULT_RESPONSE_BUDGET,
            ))
        }
        // ---- element actions ---------------------------------------------
        family @ ("click" | "dblclick" | "hover" | "focus" | "check" | "uncheck"
        | "scrollintoview" | "scrollinto" | "scroll-into-view") => {
            let handle = require_surface_verb(&subcommand, &surface)?;
            let parser = parse_browser_options(&rest, &["selector"])?;
            let mut extras = parser.positionals.as_slice();
            let selector = match parser.value("selector") {
                Some(value) => Some(value.to_owned()),
                None => match extras.split_first() {
                    Some((first, remaining)) => {
                        extras = remaining;
                        Some(first.clone())
                    }
                    None => None,
                },
            };
            let Some(selector) = selector else {
                return Err(format!("browser {subcommand} requires a selector"));
            };
            if let Some(token) = extras.first() {
                return Err(format!("browser {subcommand}: unexpected argument {token}"));
            }
            let target = element_target(&selector)?;
            // The match arm guarantees a family hit.
            let method = click_family_method(family).unwrap_or("browser.click");
            let params = with_target(serde_json::json!({ "surface_id": handle }), target);
            Ok(browser_call(
                &display,
                method,
                &params,
                BrowserPrint::Ok,
                json_mode,
                cmux_ipc::DEFAULT_RESPONSE_BUDGET,
            ))
        }
        "type" | "fill" => {
            let handle = require_surface_verb(&subcommand, &surface)?;
            let parser = parse_browser_options(&rest, &["selector", "text"])?;
            let selector = parser
                .value("selector")
                .map(ToOwned::to_owned)
                .or_else(|| parser.positionals.first().cloned());
            let Some(selector) = selector else {
                return Err(format!("browser {subcommand} requires a selector"));
            };
            // Positional text starts after an implicit-selector positional
            // (upstream: explicit --selector keeps every positional).
            let text_start = if parser.value("selector").is_none() {
                1
            } else {
                0
            };
            let text = match parser.value("text") {
                Some(text) => text.to_owned(),
                None => parser
                    .positionals
                    .get(text_start..)
                    .unwrap_or(&[])
                    .join(" "),
            };
            if subcommand == "type" {
                let explicit =
                    parser.value("text").is_some() || parser.positionals.len() > text_start;
                if !explicit || text.is_empty() {
                    return Err("browser type requires text".to_owned());
                }
            }
            // `fill` with no text flows to the wire: the port rejects an
            // empty fill text via `required_text` (upstream allows a
            // whole-value clear) - a documented divergence, ADR-0006
            // Amendment 6.
            let method = if subcommand == "type" {
                "browser.type"
            } else {
                "browser.fill"
            };
            let target = element_target(&selector)?;
            let params = with_target(
                serde_json::json!({ "surface_id": handle, "text": text }),
                target,
            );
            Ok(browser_call(
                &display,
                method,
                &params,
                BrowserPrint::Ok,
                json_mode,
                cmux_ipc::DEFAULT_RESPONSE_BUDGET,
            ))
        }
        "select" => {
            let handle = require_surface_verb(&subcommand, &surface)?;
            let parser = parse_browser_options(&rest, &["selector", "value"])?;
            let mut extras = parser.positionals.as_slice();
            let selector = match parser.value("selector") {
                Some(value) => Some(value.to_owned()),
                None => match extras.split_first() {
                    Some((first, remaining)) => {
                        extras = remaining;
                        Some(first.clone())
                    }
                    None => None,
                },
            };
            let Some(selector) = selector else {
                return Err(format!("browser {subcommand} requires a selector"));
            };
            let value = match parser.value("value") {
                Some(value) => Some(value.to_owned()),
                None => match extras.split_first() {
                    Some((first, remaining)) => {
                        extras = remaining;
                        Some(first.clone())
                    }
                    None => None,
                },
            };
            let Some(value) = value else {
                return Err(format!("browser {subcommand} requires a value"));
            };
            if let Some(token) = extras.first() {
                return Err(format!("browser {subcommand}: unexpected argument {token}"));
            }
            let target = element_target(&selector)?;
            let params = with_target(
                serde_json::json!({ "surface_id": handle, "value": value }),
                target,
            );
            Ok(browser_call(
                &display,
                "browser.select",
                &params,
                BrowserPrint::Ok,
                json_mode,
                cmux_ipc::DEFAULT_RESPONSE_BUDGET,
            ))
        }
        "scroll" => {
            let handle = require_surface_verb(&subcommand, &surface)?;
            // Upstream also scrolls within a --selector; the port wire
            // takes no target on scroll, so the strict parser refuses
            // --selector instead of ignoring it.
            let parser = parse_browser_options(&rest, &["dx", "dy"])?;
            let mut params = serde_json::json!({ "surface_id": handle });
            if let Some(raw) = parser.value("dx") {
                let dx: i64 = raw.parse().map_err(|_| "--dx must be an integer")?;
                params["dx"] = serde_json::json!(dx);
            }
            if let Some(raw) = parser.value("dy") {
                let dy: i64 = raw.parse().map_err(|_| "--dy must be an integer")?;
                params["dy"] = serde_json::json!(dy);
            } else if let Some(first) = parser.positionals.first() {
                // Upstream's positional dy fallback tolerates non-integers
                // (silently ignored); mirror it.
                if let Ok(dy) = first.parse::<i64>() {
                    params["dy"] = serde_json::json!(dy);
                }
            }
            Ok(browser_call(
                &display,
                "browser.scroll",
                &params,
                BrowserPrint::Ok,
                json_mode,
                cmux_ipc::DEFAULT_RESPONSE_BUDGET,
            ))
        }
        "screenshot" => {
            let handle = require_surface_verb(&subcommand, &surface)?;
            let parser = parse_browser_options(&rest, &["out"])?;
            let out = parser.value("out").map(std::path::PathBuf::from);
            let params = serde_json::json!({ "surface_id": handle });
            Ok(browser_call(
                &display,
                "browser.screenshot",
                &params,
                BrowserPrint::Screenshot(out),
                json_mode,
                cmux_ipc::DEFAULT_RESPONSE_BUDGET,
            ))
        }
        "focus-webview" | "focus_webview" => {
            let handle = require_surface_verb(&subcommand, &surface)?;
            let params = serde_json::json!({ "surface_id": handle });
            Ok(browser_call(
                &display,
                "browser.focus_webview",
                &params,
                BrowserPrint::Ok,
                json_mode,
                cmux_ipc::DEFAULT_RESPONSE_BUDGET,
            ))
        }
        "is-webview-focused" | "is_webview_focused" => {
            let handle = require_surface_verb(&subcommand, &surface)?;
            let params = serde_json::json!({ "surface_id": handle });
            Ok(browser_call(
                &display,
                "browser.is_webview_focused",
                &params,
                BrowserPrint::Json("focused"),
                json_mode,
                cmux_ipc::DEFAULT_RESPONSE_BUDGET,
            ))
        }
        // ---- the get/is families ------------------------------------------
        "get" => {
            let handle = require_surface_verb(&subcommand, &surface)?;
            let Some(field) = rest.first().map(|token| token.to_ascii_lowercase()) else {
                return Err(format!(
                    "browser {subcommand} requires a subcommand (port: url title text html value attr count box styles)"
                ));
            };
            if field == "url" {
                let params = serde_json::json!({ "surface_id": handle });
                return Ok(browser_call(
                    &display,
                    "browser.url.get",
                    &params,
                    BrowserPrint::Str("url"),
                    json_mode,
                    cmux_ipc::DEFAULT_RESPONSE_BUDGET,
                ));
            }
            if field == "title" {
                let params = serde_json::json!({ "surface_id": handle });
                return Ok(browser_call(
                    &display,
                    "browser.get.title",
                    &params,
                    BrowserPrint::Str("title"),
                    json_mode,
                    cmux_ipc::DEFAULT_RESPONSE_BUDGET,
                ));
            }
            let Some(method) = get_field_method(&field) else {
                return Err(format!(
                    "unsupported browser get subcommand: {field} (port: url title text html value attr count box styles)"
                ));
            };
            let parser = parse_browser_options(&rest[1..], &["selector", "attr", "property"])?;
            let selector = parser
                .value("selector")
                .map(ToOwned::to_owned)
                .or_else(|| parser.positionals.first().cloned());
            let Some(selector) = selector else {
                return Err(format!("browser get {field} requires a selector"));
            };
            let mut params = with_target(
                serde_json::json!({ "surface_id": handle }),
                element_target(&selector)?,
            );
            if field == "attr" {
                // Upstream's positional fallback reads the second
                // positional (after the selector); --attr wins.
                let name = parser
                    .value("attr")
                    .map(ToOwned::to_owned)
                    .or_else(|| parser.positionals.get(1).cloned());
                let Some(name) = name else {
                    return Err(format!("browser get {field} requires --attr <name>"));
                };
                params["attr"] = serde_json::Value::String(name);
            }
            if field == "styles" {
                if let Some(property) = parser.value("property") {
                    params["property"] = serde_json::Value::String(property.to_owned());
                }
            }
            let print = if field == "count" {
                BrowserPrint::Json("count")
            } else {
                BrowserPrint::Json("value")
            };
            Ok(browser_call(
                &display,
                method,
                &params,
                print,
                json_mode,
                cmux_ipc::DEFAULT_RESPONSE_BUDGET,
            ))
        }
        "is" => {
            let handle = require_surface_verb(&subcommand, &surface)?;
            let Some(state) = rest.first().map(|token| token.to_ascii_lowercase()) else {
                return Err(format!(
                    "browser {subcommand} requires a subcommand (port: visible enabled checked)"
                ));
            };
            let Some(method) = is_state_method(&state) else {
                return Err(format!(
                    "unsupported browser is subcommand: {state} (port: visible enabled checked)"
                ));
            };
            let parser = parse_browser_options(&rest[1..], &["selector"])?;
            let mut extras = parser.positionals.as_slice();
            let selector = match parser.value("selector") {
                Some(value) => Some(value.to_owned()),
                None => match extras.split_first() {
                    Some((first, remaining)) => {
                        extras = remaining;
                        Some(first.clone())
                    }
                    None => None,
                },
            };
            let Some(selector) = selector else {
                return Err(format!("browser is {state} requires a selector"));
            };
            if let Some(token) = extras.first() {
                return Err(format!("browser {subcommand}: unexpected argument {token}"));
            }
            let params = with_target(
                serde_json::json!({ "surface_id": handle }),
                element_target(&selector)?,
            );
            Ok(browser_call(
                &display,
                method,
                &params,
                BrowserPrint::Json("value"),
                json_mode,
                cmux_ipc::DEFAULT_RESPONSE_BUDGET,
            ))
        }
        other => Err(format!(
            "browser: unknown subcommand {other} (try: cmux browser with no args for usage)"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args_list(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn options_parse_values_and_positionals() {
        let parser = parse_browser_options(
            &args_list(&["--selector", ".btn", "x", "--text=hi"]),
            &["selector", "text"],
        )
        .unwrap();
        assert_eq!(parser.value("selector"), Some(".btn"));
        assert_eq!(parser.value("text"), Some("hi"));
        assert_eq!(parser.positionals, vec!["x".to_owned()]);
    }

    #[test]
    fn options_refuse_unknown_missing_and_inline_bogus() {
        // Unknown option: refused, never folded into a URL or selector.
        let error = parse_browser_options(&args_list(&["--photo", "x"]), &[]).unwrap_err();
        assert!(error.contains("unrecognized option --photo"), "{error}");
        // Inline value form is refused the same way.
        let error = parse_browser_options(&args_list(&["--snapshot-after"]), &[]).unwrap_err();
        assert!(error.contains("--snapshot-after"), "{error}");
        // A declared value option with no value is an error.
        let error = parse_browser_options(&args_list(&["--selector"]), &["selector"]).unwrap_err();
        assert!(error.contains("--selector requires a value"), "{error}");
    }

    #[test]
    fn element_target_routes_canonical_refs_only() {
        assert_eq!(
            element_target("@e12").unwrap(),
            serde_json::json!({ "ref": "@e12" })
        );
        // `e12` is a valid CSS type selector: stays CSS (conservative).
        assert_eq!(
            element_target("e12").unwrap(),
            serde_json::json!({ "selector": "e12" })
        );
        assert_eq!(
            element_target("div#main").unwrap(),
            serde_json::json!({ "selector": "div#main" })
        );
        assert!(element_target("@e").is_err());
        assert!(element_target("@zzz").is_err());
    }

    #[test]
    fn method_maps_cover_the_family_aliases() {
        assert_eq!(
            click_family_method("scroll-into-view"),
            Some("browser.scroll_into_view")
        );
        assert_eq!(
            click_family_method("scrollinto"),
            Some("browser.scroll_into_view")
        );
        assert_eq!(
            click_family_method("scrollintoview"),
            Some("browser.scroll_into_view")
        );
        assert_eq!(click_family_method("click"), Some("browser.click"));
        assert_eq!(click_family_method("navigate"), None);
        assert_eq!(key_half_method("key"), Some("browser.press"));
        assert_eq!(key_half_method("keyup"), Some("browser.keyup"));
        assert_eq!(key_half_method("keydown"), Some("browser.keydown"));
        assert_eq!(key_half_method("wait"), None);
        assert_eq!(get_field_method("count"), Some("browser.get.count"));
        assert_eq!(
            get_field_method("url"),
            None,
            "get url routes via browser.url.get"
        );
        assert_eq!(is_state_method("checked"), Some("browser.is.checked"));
        assert_eq!(is_state_method("focused"), None);
    }

    #[test]
    fn wait_budget_matches_the_app_contract() {
        // Floor: max(15s, ms + 9s); 5000 + 9000 = 14000 floored to 15000.
        assert_eq!(wait_response_budget(5_000), Duration::from_millis(15_000));
        // 20s wait + 9s grace beats the floor.
        assert_eq!(wait_response_budget(20_000), Duration::from_millis(29_000));
        // The cap keeps the budget bounded: 120000 + 9000.
        assert_eq!(
            wait_response_budget(120_000),
            Duration::from_millis(129_000)
        );
    }

    #[test]
    fn jsonish_prints_scalars_raw_and_objects_compact() {
        assert_eq!(jsonish(&serde_json::json!("hi")), "hi");
        assert_eq!(jsonish(&serde_json::json!(null)), "null");
        assert_eq!(jsonish(&serde_json::json!(true)), "true");
        assert_eq!(jsonish(&serde_json::json!(3)), "3");
        assert_eq!(jsonish(&serde_json::json!({"x": 1})), r#"{"x":1}"#);
        assert_eq!(jsonish(&serde_json::json!([1, 2])), "[1,2]");
    }

    #[test]
    fn with_target_merges_exactly_one_target_key() {
        assert_eq!(
            with_target(
                serde_json::json!({ "surface_id": "p1" }),
                element_target("@e2").unwrap(),
            ),
            serde_json::json!({ "surface_id": "p1", "ref": "@e2" })
        );
        assert_eq!(
            with_target(
                serde_json::json!({ "surface_id": "p1" }),
                element_target("#btn").unwrap(),
            ),
            serde_json::json!({ "surface_id": "p1", "selector": "#btn" })
        );
    }

    #[test]
    fn first_token_is_surface_never_eats_verb_words() {
        // Regression (caught live): `browser get count input` must not
        // reject the CSS selector `input` - also an upstream verb word.
        // Verb-slot unsupported tokens still fall through and reject.
        assert!(first_token_is_surface("p1"));
        assert!(first_token_is_surface("2"));
        assert!(!first_token_is_surface("--x"));
        assert!(!first_token_is_surface("open"));
        assert!(!first_token_is_surface("find"));
        assert!(!first_token_is_surface("input"));
    }

    #[test]
    fn command_flag_scan_is_strict_per_verb() {
        // run: the name is positional; surface/yes/json parse.
        let tokens: Vec<String> = ["--surface", "p2", "build", "--yes", "--json"]
            .iter()
            .map(|&token| token.to_owned())
            .collect();
        let (flags, positionals) = split_command_flags(&tokens, true, true).unwrap();
        assert_eq!(flags.surface.as_deref(), Some("p2"));
        assert!(flags.yes);
        assert!(flags.json);
        assert_eq!(positionals, ["build"]);

        // An unknown dash token rejects like the browser grammar.
        let tokens: Vec<String> = ["build", "--profile", "x"]
            .iter()
            .map(|&token| token.to_owned())
            .collect();
        assert!(split_command_flags(&tokens, true, true).is_err());

        // list accepts neither --surface nor --yes; a bare positional
        // passes the scanner and the caller refuses it.
        let tokens: Vec<String> = ["--json", "--surface", "p2"]
            .iter()
            .map(|&token| token.to_owned())
            .collect();
        assert!(split_command_flags(&tokens, false, false).is_err());
        let tokens: Vec<String> = ["--json", "-x"]
            .iter()
            .map(|&token| token.to_owned())
            .collect();
        let (flags, positionals) = split_command_flags(&tokens, false, false).unwrap();
        assert!(flags.json);
        assert_eq!(positionals, ["-x"]);

        // A value-less --surface is an error, not a silent drop.
        let tokens: Vec<String> = ["build", "--surface"]
            .iter()
            .map(|&token| token.to_owned())
            .collect();
        assert!(split_command_flags(&tokens, true, true).is_err());
    }

    #[test]
    fn verb_tables_match_the_recorded_scope() {
        // Residuals stay rejected; supported verbs stay supported.
        assert!(UNSUPPORTED_BROWSER_VERBS.contains(&"find"));
        assert!(UNSUPPORTED_BROWSER_VERBS.contains(&"set-input-files"));
        assert!(UNSUPPORTED_BROWSER_VERBS.contains(&"import"));
        assert!(!UNSUPPORTED_BROWSER_VERBS.contains(&"navigate"));
        assert!(!UNSUPPORTED_BROWSER_VERBS.contains(&"snapshot"));
        assert_eq!(
            BROWSER_VERBS_WITHOUT_SURFACE,
            &["open", "open-split", "new"][..]
        );
    }

    // ----- M6: the ssh verb's parser (ADR-0014) -----

    fn ssh_tokens(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| (*arg).to_owned()).collect()
    }

    #[test]
    fn ssh_parse_carries_the_supported_flags() {
        let invocation = cmd_ssh_parse(&ssh_tokens(&[
            "--port",
            "2222",
            "--identity",
            "C:/keys/id",
            "--name",
            "build box",
            "--ssh-option",
            "IdentitiesOnly=yes",
            "--no-focus",
            "--command",
            "tail -f log",
            "user@host",
        ]))
        .unwrap();
        assert_eq!(invocation.destination, "user@host");
        assert_eq!(invocation.port, Some(2222));
        assert_eq!(invocation.identity_file.as_deref(), Some("C:/keys/id"));
        assert_eq!(invocation.name.as_deref(), Some("build box"));
        assert_eq!(invocation.options, ["IdentitiesOnly=yes"]);
        assert_eq!(invocation.initial_command.as_deref(), Some("tail -f log"));
        assert!(!invocation.focus);
    }

    #[test]
    fn ssh_parse_remote_arguments_join_upstream_style() {
        // `--`-delimited args are the literal remote command (upstream
        // `SSHRemoteCommand`), joined with spaces for the wire.
        let invocation = cmd_ssh_parse(&ssh_tokens(&["user@host", "--", "tail", "f"])).unwrap();
        assert_eq!(invocation.initial_command.as_deref(), Some("tail f"));
        // A bare `--command` fills the slot when no trailing arguments
        // carry command text…
        let command_only = cmd_ssh_parse(&ssh_tokens(&["--command", "sync", "user@host"])).unwrap();
        assert_eq!(command_only.initial_command.as_deref(), Some("sync"));
        // …and combining both refuses (verbatim upstream text shape).
        assert!(
            cmd_ssh_parse(&ssh_tokens(&["--command", "a", "user@host", "echo", "hi"])).is_err()
        );
    }

    #[test]
    fn ssh_parse_flags_consume_anywhere_and_single_dash_tokens_ride() {
        // Upstream's scan consumes its known flags anywhere in the
        // argument list, including after the destination (OpenSSH would
        // call those remote args — the CLI's flag grammar sits one level
        // above raw ssh argv). Single-dash tokens after the destination
        // ride as remote arguments.
        let invocation =
            cmd_ssh_parse(&ssh_tokens(&["user@host", "--port", "2222", "-x", "-X"])).unwrap();
        assert_eq!(invocation.port, Some(2222));
        assert_eq!(invocation.initial_command.as_deref(), Some("-x -X"));
        // An unknown `--` flag refuses even after the destination.
        assert!(cmd_ssh_parse(&ssh_tokens(&["user@host", "--bogus", "x"])).is_err());
    }

    #[test]
    fn ssh_parse_encodes_the_tty_run_as_request_tty() {
        // `-tt` after the destination is OpenSSH TTY configuration; the
        // wire carries only option strings, so the run durably encodes as
        // one RequestTTY option (upstream's durable encoding), out of the
        // remote command text.
        let invocation = cmd_ssh_parse(&ssh_tokens(&[
            "--command",
            "docker exec",
            "user@host",
            "-tt",
        ]))
        .unwrap();
        assert_eq!(invocation.options, ["RequestTTY=force"]);
        assert_eq!(invocation.initial_command.as_deref(), Some("docker exec"));
        // A `-tT` run lands on `no` (the T disables after the t), and a
        // bare `-t` promotes an explicit RequestTTY=no to yes.
        let disabled = cmd_ssh_parse(&ssh_tokens(&["user@host", "-tT"])).unwrap();
        assert_eq!(disabled.options, ["RequestTTY=no"]);
        assert_eq!(disabled.initial_command, None);
        let promoted = cmd_ssh_parse(&ssh_tokens(&[
            "--ssh-option",
            "RequestTTY=no",
            "user@host",
            "-t",
        ]))
        .unwrap();
        assert_eq!(promoted.options, ["RequestTTY=yes"]);
    }

    #[test]
    fn ssh_parse_refuses_the_upstream_shapes() {
        fn refused_code(args: &[&str]) -> i32 {
            cmd_ssh_parse(&ssh_tokens(args)).err().unwrap()
        }
        // No destination at all.
        assert_eq!(refused_code(&[]), 1);
        assert_eq!(refused_code(&["--port", "2222"]), 1);
        // A leading short option where the destination must be.
        assert_eq!(refused_code(&["-p", "2222", "host"]), 1);
        // Unknown flag before the destination.
        assert_eq!(refused_code(&["--bogus", "host"]), 1);
        // Value-less trailing flag.
        assert_eq!(refused_code(&["host", "remote", "--port"]), 1);
        // Out-of-port-range values.
        assert_eq!(refused_code(&["--port", "99999", "host"]), 1);
        assert_eq!(refused_code(&["--port", "0", "host"]), 1);
        // A mixed TTY cluster right after the destination.
        assert_eq!(refused_code(&["host", "-tX"]), 1);
        // The managed-carrier-only flags refuse with the port's line.
        assert_eq!(refused_code(&["--window", "3", "host"]), 1);
        assert_eq!(refused_code(&["--transport", "ssh", "host"]), 1);
        assert_eq!(refused_code(&["--session", "2", "host"]), 1);
    }
}
