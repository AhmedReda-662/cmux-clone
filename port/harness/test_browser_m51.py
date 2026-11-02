"""M5.1 live smoke: browser panes over a real `cmux-shell.exe`.

M5.1 contract (ADR-0006 amendment): a browser pane is a split-tree leaf
hosting a WebView2 controller in a child HWND of the app window; the pane's
laid-out rect pins the child every frame (points x DPI scale), the surface
exposes `browser.*` verbs over the control pipe, and its model focus/title
reach `system.identify` and `surface.list` like a terminal surface.

Hermetic, using the M4.3 launch conventions: a unique `CMUX_SOCKET_PATH`
override (keeps the ancestry gate off), an isolated session file
(REUSED for the restart check — restores must come back as browser panes),
a config file pinning PowerShell -NoProfile, and an owned process tree at
teardown. All pages are `file://` URLs written to the fixture — no network.

Checks (each must pass):
  - the app plane answers and identify reports a terminal surface first
  - `browser.open_split` creates a browser leaf that focuses and loads the
    file page (url.get reaches `ready` with the page title)
  - identify/surface rows report the browser pane (`surface_type`,
    `focused.browser.{url,title}`, row `type`)
  - navigate -> back -> history start (no-op success) -> forward -> reload,
    each observed through url.get (`can_go_*` flips)
  - browser verb on a terminal surface -> `invalid_state`; bad/absent URL ->
    `invalid_params`; unknown ids -> `not_found`; text verbs on the browser
    -> `invalid_state`
  - the WebView2 child HWND is pinned inside the browser pane region
    (EnumChildWindows rect match — classes only logged, matched by rect)
  - ctrl+2 while the webview has native focus still switches workspaces
    (the accelerator router intercepts the app chord the browser would
    otherwise swallow), and the hidden pane survives the switch (still
    `ready`, same URL) once its workspace is shown again
  - M5.2 modifier gate: a PLAIN digit and an ALT+digit through the
    webview-owned keyboard never switch workspaces (only a Ctrl-held
    registered key re-enters the app's action stream), and ctrl+2 still
    routes after the gate landed
  - M5.2 CDP automation subset over the second instance: eval round
    trips stay inside the reply budget over the ONE persistent
    connection; `browser.snapshot` mints `@eN` refs with selectors and
    carries the page text; unknown refs and oversized payloads are
    `invalid_params` with actionable messages; synthetic
    click/dblclick/type drive the fixture page's real handlers (the
    composed output proves the whole chain through `browser.eval`);
    navigation kills the ref table; automation verbs on a terminal are
    `invalid_state`
  - M5.4 key synthesis and waits: `browser.press`/`keydown`/`keyup` drive
    the focused element through the real key path (Backspace edits, the
    Control+a chord selects all with the modifier bit visible in the
    page's key log, the keydown/keyup halves land in order); the wait
    family resolves selector/text/url/load-state/AND/function predicates
    by re-polling, and an unsatisfied wait answers the structured
    `timeout` past its deadline
  - `msedgewebview2` processes exist while a surface lives, attributed to
    the app exactly by the per-surface udf path in their command lines,
    and the app-owned set is empty after WM_CLOSE; the udf folder itself
    stays on disk
  - restart with the same session file: the browser leaf restores with its
    saved URL (browser_url round-trip) and a terminal leaf alongside
  - no panic anywhere in the app's stderr at teardown

Exit 0 only when every check printed PASS.
"""

from __future__ import annotations

import argparse
import ctypes
import json
import os
import random
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import cmux as harness  # noqa: E402
from cmux import cmuxError  # noqa: E402

# App stderr can carry lossily-decoded OS text (U+FFFD); a cp1252 console
# codec must not kill the run's final dump (observed after the M5.2
# accelerator changes). Check lines stay ASCII either way.
sys.stdout.reconfigure(errors="replace")
sys.stderr.reconfigure(errors="replace")

PASS: list[str] = []
FAIL: list[str] = []


def check(name: str, condition: bool, detail: str = "") -> bool:
    if condition:
        PASS.append(name)
        print(f"PASS {name}")
        return True
    FAIL.append(name)
    print(f"FAIL {name} {detail}".rstrip())
    return False


def poll_until(fn, deadline_s: float, interval_s: float = 0.25):
    """Calls `fn` until it returns truthy (or the deadline expires)."""
    end = time.monotonic() + deadline_s
    last = None
    while time.monotonic() < end:
        last = fn()
        if last:
            return last
        time.sleep(interval_s)
    return last


def _safe(fn):
    try:
        return fn()
    except cmuxError:
        return None


def probe(fn):
    """Returns the result, or the cmuxError itself (for error-gate checks)."""
    try:
        return fn()
    except cmuxError as error:
        return error


# ---------------------------------------------------------------------------
# Win32 plumbing (mirrors test_terminal_ux_m43.py)
# ---------------------------------------------------------------------------

wt = ctypes.wintypes
user32 = ctypes.windll.user32
kernel32 = ctypes.windll.kernel32
shcore = ctypes.windll.shcore

user32.SetForegroundWindow.argtypes = [wt.HWND]
user32.SetForegroundWindow.restype = wt.BOOL
user32.GetForegroundWindow.restype = wt.HWND
user32.PostMessageW.argtypes = [wt.HWND, wt.UINT, wt.WPARAM, wt.LPARAM]
user32.MoveWindow.argtypes = [wt.HWND, ctypes.c_int, ctypes.c_int, ctypes.c_int, ctypes.c_int, wt.BOOL]
user32.GetClientRect.argtypes = [wt.HWND, ctypes.POINTER(wt.RECT)]
user32.GetWindowRect.argtypes = [wt.HWND, ctypes.POINTER(wt.RECT)]
user32.ClientToScreen.argtypes = [wt.HWND, ctypes.POINTER(wt.POINT)]
user32.GetWindowThreadProcessId.argtypes = [wt.HWND, ctypes.POINTER(wt.DWORD)]
user32.GetClassNameW.argtypes = [wt.HWND, wt.LPWSTR, ctypes.c_int]
user32.GetClassNameW.restype = ctypes.c_int

INPUT_MOUSE, INPUT_KEYBOARD = 0, 1
KEYEVENTF_KEYUP = 0x0002
MOUSEEVENTF_LEFTDOWN = 0x0002
MOUSEEVENTF_LEFTUP = 0x0004
MOUSEEVENTF_MOVE = 0x0001
MOUSEEVENTF_ABSOLUTE = 0x8000
WM_CLOSE = 0x0010
WM_INPUTLANGCHANGEREQUEST = 0x0050
VK_CONTROL, VK_MENU, VK_2 = 0x11, 0x12, 0x32


class KEYBDINPUT(ctypes.Structure):
    _fields_ = [
        ("wVk", wt.WORD),
        ("wScan", wt.WORD),
        ("dwFlags", wt.DWORD),
        ("time", wt.DWORD),
        ("dwExtraInfo", ctypes.POINTER(wt.ULONG)),
    ]


class MOUSEINPUT(ctypes.Structure):
    _fields_ = [
        ("dx", wt.LONG),
        ("dy", wt.LONG),
        ("mouseData", wt.DWORD),
        ("dwFlags", wt.DWORD),
        ("time", wt.DWORD),
        ("dwExtraInfo", ctypes.POINTER(wt.ULONG)),
    ]


class _INPUTUNION(ctypes.Union):
    _fields_ = [("ki", KEYBDINPUT), ("mi", MOUSEINPUT)]


class INPUT(ctypes.Structure):
    _anonymous_ = ("u",)
    _fields_ = [("type", wt.DWORD), ("u", _INPUTUNION)]


def _send(*inputs) -> None:
    array = (INPUT * len(inputs))(*inputs)
    sent = user32.SendInput(len(inputs), array, ctypes.sizeof(INPUT))
    if sent != len(inputs):
        raise OSError(f"SendInput accepted {sent}/{len(inputs)} events")


def key_event(vk: int, up: bool = False) -> INPUT:
    return INPUT(
        type=INPUT_KEYBOARD,
        ki=KEYBDINPUT(
            wVk=vk,
            wScan=0,
            dwFlags=KEYEVENTF_KEYUP if up else 0,
            time=0,
            dwExtraInfo=None,
        ),
    )


def press(vk: int, ctrl: bool = False, alt: bool = False) -> None:
    events: list[INPUT] = []
    if ctrl:
        events.append(key_event(VK_CONTROL))
    if alt:
        events.append(key_event(VK_MENU))
    events.append(key_event(vk))
    events.append(key_event(vk, up=True))
    if alt:
        events.append(key_event(VK_MENU, up=True))
    if ctrl:
        events.append(key_event(VK_CONTROL, up=True))
    for event in events:
        _send(event)
        time.sleep(0.03)


def set_cursor(x: int, y: int) -> None:
    ctypes.windll.user32.SetCursorPos(int(x), int(y))
    time.sleep(0.05)


def click(x: int, y: int) -> None:
    set_cursor(x, y)
    time.sleep(0.1)
    _send(
        INPUT(type=INPUT_MOUSE, mi=MOUSEINPUT(0, 0, 0, MOUSEEVENTF_LEFTDOWN, 0, None)),
        INPUT(type=INPUT_MOUSE, mi=MOUSEINPUT(0, 0, 0, MOUSEEVENTF_LEFTUP, 0, None)),
    )


def main_window_for(pid: int) -> int | None:
    found = []

    @ctypes.WINFUNCTYPE(wt.BOOL, wt.HWND, wt.LPARAM)
    def on_window(hwnd, _lparam):
        owner_pid = wt.DWORD()
        user32.GetWindowThreadProcessId(hwnd, ctypes.byref(owner_pid))
        if owner_pid.value == pid and user32.IsWindowVisible(hwnd):
            rect = wt.RECT()
            user32.GetWindowRect(hwnd, ctypes.byref(rect))
            if rect.right - rect.left > 200 and rect.bottom - rect.top > 150:
                found.append(hwnd)
        return True  # keep enumerating

    user32.EnumWindows(on_window, 0)
    return found[0] if found else None


def child_windows(hwnd: int) -> list[int]:
    found = []

    @ctypes.WINFUNCTYPE(wt.BOOL, wt.HWND, wt.LPARAM)
    def on_child(child, _lparam):
        found.append(child)
        return True

    user32.EnumChildWindows(hwnd, on_child, 0)
    return found


def window_class(hwnd: int) -> str:
    buffer = ctypes.create_unicode_buffer(64)
    user32.GetClassNameW(hwnd, buffer, 64)
    return buffer.value


class GUITHREADINFO(ctypes.Structure):
    """Minimal winuser GUITHREADINFO (x64 layout) — hwndFocus is enough."""

    _fields_ = [
        ("cbSize", wt.DWORD),
        ("flags", wt.DWORD),
        ("hwndActive", wt.HWND),
        ("hwndFocus", wt.HWND),
        ("rcCaret", wt.RECT),
        ("rcClip", wt.RECT),
        ("hwndMenuOwner", wt.HWND),
        ("hwndMenuActive", wt.HWND),
    ]

    def __init__(self):
        super().__init__()
        self.cbSize = ctypes.sizeof(self)


user32.GetGUIThreadInfo.argtypes = [wt.DWORD, ctypes.POINTER(GUITHREADINFO)]
user32.GetGUIThreadInfo.restype = wt.BOOL


def window_rect(hwnd: int) -> tuple[int, int, int, int]:
    rect = wt.RECT()
    user32.GetWindowRect(hwnd, ctypes.byref(rect))
    return rect.left, rect.top, rect.right, rect.bottom


def pin_us_layout(hwnd: int) -> None:
    """Pin the window's thread to the US keyboard layout (M4.3 evidence)."""
    handle = user32.LoadKeyboardLayoutW("00000409", 1)  # KLF_ACTIVATE
    user32.PostMessageW(hwnd, WM_INPUTLANGCHANGEREQUEST, 0, handle)
    time.sleep(0.3)


def webview_process_count() -> int:
    out = subprocess.run(
        ["powershell", "-NoProfile", "-Command",
         "@(Get-Process -Name msedgewebview2 -ErrorAction SilentlyContinue).Count"],
        capture_output=True,
        text=True,
        timeout=30,
    )
    try:
        return int((out.stdout or "0").strip() or "0")
    except ValueError:
        return 0


def foreground_window(hwnd: int) -> bool:
    """The Alt trick (M4.3 evidence): a synthesized Alt press grants
    SetForegroundWindow past the foreground lock; the plain call fails
    nondeterministically."""
    _send(key_event(VK_MENU))
    ok = bool(user32.SetForegroundWindow(hwnd))
    _send(key_event(VK_MENU, up=True))
    return ok


def retry_ipc(fn, attempts: int = 4, delay_s: float = 0.4):
    """One server reply-budget timeout is transient (a busy frame may
    exceed the budget once); a check fails only when every attempt does."""
    last = None
    for _ in range(attempts):
        try:
            return fn()
        except cmuxError as error:
            last = error
            time.sleep(delay_s)
    raise last


def safe_current(client):
    """workspace.current with retry; None when the app stops answering."""
    try:
        return retry_ipc(lambda: client.current_workspace())
    except cmuxError:
        return None


def create_workspace_once(client, attempts: int = 3):
    """workspace.create with late-frame verification.

    A reply-budget timeout during the app's startup burst is the
    documented M4.1 transient (the frame thread can be busy for more
    than 400 ms while the first panes spawn) — but the frame may still
    have done the work after the server gave up, so a blind retry
    could create two workspaces. Treat "the row count grew" as success
    (returning the newest row) and only retry when it did not.
    """
    last: object = None
    for _ in range(attempts):
        before = retry_ipc(lambda: client.list_workspaces()) or []
        try:
            return client.new_workspace()
        except cmuxError as error:
            last = error
            rows = retry_ipc(lambda: client.list_workspaces()) or []
            if len(rows) > len(before):
                return rows[-1]
            time.sleep(0.5)
    assert isinstance(last, BaseException)
    raise last


PAGE_ONE = """<!doctype html><html><head><title>Page One</title></head>
<body><h1>M51-PAGE-ONE</h1><p>first</p></body></html>
"""

PAGE_TWO = """<!doctype html><html><head><title>Page Two</title></head>
<body><h1>M51-PAGE-TWO</h1><p>second</p></body></html>
"""

# The automation fixture (M5.2 ×5.3×5.4): one input, one echo button, a
# double-click counter, a checkbox, a select, a hover target, a hidden
# element, and a tall scrollable body. Refs mint in DOM order (@e1 answer,
# @e2 compose, @e3 dbl, @e4 agree, @e5 lang, @e6 hoverTarget); `out`,
# `count`, `hoverState` are plain nodes the snapshot only reads. The key
# logging (M5.4) records every synthetic keydown/keyup into `#keyLog`
# (a non-interactive div, so the refs and the snapshot stay unchanged).
PAGE_FORM = """<!doctype html><html><head><title>Form Page</title></head>
<body><h1>M52-CDP-FORM</h1>
<input id="answer" placeholder="type here">
<button id="compose">Compose</button>
<div id="out"></div>
<button id="dbl">Double</button>
<div id="count">0</div>
<input type="checkbox" id="agree">
<div id="agreeState">unchecked</div>
<select id="lang">
<option value="en">English</option>
<option value="de">German</option>
<option value="fr">French</option>
</select>
<button id="hoverTarget">Hover me</button>
<div id="hoverState"></div>
<div id="keyLog"></div>
<div id="ghost" style="display:none">invisible</div>
<div style="height:3000px" id="tall"></div>
<div id="bottom" style="position:absolute;top:2800px">bottom</div>
<script>
document.getElementById("compose").addEventListener("click", function () {
  document.getElementById("out").textContent = "composed:" + document.getElementById("answer").value;
});
document.getElementById("dbl").addEventListener("dblclick", function () {
  var el = document.getElementById("count");
  var n = parseInt(el.textContent || "0", 10) + 1;
  el.textContent = String(n);
});
document.getElementById("agree").addEventListener("change", function () {
  document.getElementById("agreeState").textContent = this.checked ? "checked" : "unchecked";
});
document.getElementById("lang").addEventListener("change", function () {
  document.getElementById("agreeState").setAttribute("data-lang", this.value);
});
document.getElementById("hoverTarget").addEventListener("mouseenter", function () {
  document.getElementById("hoverState").textContent = "hovered";
});
window.__keyLog = "";
["keydown", "keyup"].forEach(function (kind) {
  document.addEventListener(kind, function (ev) {
    window.__keyLog += (kind === "keydown" ? "D:" : "U:") + ev.key + (ev.ctrlKey ? "[C]" : "") + ";";
    document.getElementById("keyLog").textContent = window.__keyLog;
  });
});
</script>
</body></html>
"""


def _rows_over_all_workspaces(client) -> list[dict]:
    """Surface rows across every workspace (restore may activate either)."""
    rows: list[dict] = []
    try:
        for workspace_row in client.list_workspaces():
            rows.extend(client.list_surfaces(workspace_row.get("id")))
    except cmuxError:
        pass
    return rows


def page_url(pages_dir: Path, name: str) -> str:
    # Chromium wants forward slashes on file URLs.
    path = str(pages_dir / name).replace("\\", "/")
    return f"file:///{path}"


# ---------------------------------------------------------------------------
# Smoke
# ---------------------------------------------------------------------------


def launch(shell_bin: Path, fixture: Path, pipe: str, env: dict, log_path: Path):
    env = dict(env)
    env["CMUX_SOCKET_PATH"] = pipe
    env["CMUX_SESSION_FILE"] = str(fixture / "session.json")
    env["CMUX_CONFIG_FILE"] = str(fixture / "cmux.json")
    for extra in (
        "CMUX_SHELL_AUTO_EXIT_MS",
        "CMUX_CLAUDE_HOOKS_DISABLED",
        "CMUX_PASSWORD_FILE",
        "CMUX_SOCKET_PASSWORD",
        "CMUX_IPC_DISABLED",
    ):
        env.pop(extra, None)
    app_log = open(log_path, "ab")
    process = subprocess.Popen(
        [str(shell_bin)],
        cwd=str(fixture),
        env=env,
        stdout=subprocess.DEVNULL,
        stderr=app_log,
    )
    return process, app_log, env


def smoke() -> int:
    try:
        shcore.SetProcessDpiAwareness(2)
    except OSError:
        pass  # already aware or unsupported; coordinates stay approximate

    parser = argparse.ArgumentParser()
    parser.add_argument("--bin", default=None, help="cmux-shell.exe path override")
    args = parser.parse_args()

    root = Path(__file__).resolve().parents[1]
    default = root / "target" / "debug" / "cmux-shell.exe"
    if not default.exists():
        default = root / "target" / "release" / "cmux-shell.exe"
    shell_bin = Path(args.bin) if args.bin else default
    assert shell_bin.exists(), f"cmux-shell.exe not found at {shell_bin} (build first)"
    shell_bin = shell_bin.resolve()

    pipe = rf"\\.\pipe\cmux-m51-smoke-{os.getpid()}-{random.randint(1000, 9999)}"
    fixture = Path(tempfile.mkdtemp(prefix="cmux-m51-smoke-"))
    config = (
        '{\n  "terminal": { "shell": { "program": "powershell.exe", '
        '"args": ["-NoLogo", "-NoProfile"] } }\n}\n'
    )
    (fixture / "cmux.json").write_text(config, encoding="utf-8")
    pages = fixture / "pages"
    pages.mkdir()
    (pages / "one.html").write_text(PAGE_ONE, encoding="utf-8")
    (pages / "two.html").write_text(PAGE_TWO, encoding="utf-8")
    (pages / "form.html").write_text(PAGE_FORM, encoding="utf-8")
    url_one, url_two = page_url(pages, "one.html"), page_url(pages, "two.html")
    url_form = page_url(pages, "form.html")

    webview_baseline = webview_process_count()
    process = None
    app_log = None
    surface_id = None
    udf = None
    try:
        process, app_log, env = launch(shell_bin, fixture, pipe, dict(os.environ), fixture / "app-stderr.log")
        print(f"launched {shell_bin.name} pid={process.pid} pipe={pipe}")

        client = harness.cmux(socket_path=pipe, connect_timeout_s=30.0, timeout_s=5.0)
        ident = poll_until(lambda: _safe(client.identify), 30.0)
        check("app plane ready (identify answers)", isinstance(ident, dict))
        if not isinstance(ident, dict):
            raise SystemExit(1)
        focused = ident.get("focused") or {}
        check(
            "identify opens on a terminal surface",
            ident.get("app") == "cmux"
            and focused.get("surface_type") == "terminal"
            and focused.get("is_browser_surface") is False,
            f"identify={ident}",
        )

        # A second workspace so the router chord later has a row to land on;
        # creation switches the active workspace, so move back to the first.
        create_workspace_once(client)
        rows = client.list_workspaces()
        check("second workspace exists", len(rows) == 2, f"rows={rows}")
        workspace_one = rows[0]["id"]
        retry_ipc(lambda: client.select_workspace(workspace_one))
        terminal_id = next(
            (row["id"] for row in retry_ipc(
                lambda: client.list_surfaces(workspace_one)) if row.get("type") == "terminal"),
            None,
        )
        check("workspace one reports its terminal", bool(terminal_id), f"terminal_id={terminal_id!r}")

        # --- browser.open_split + readiness through url.get ---
        opened = client._call("browser.open_split", {"url": url_one})
        surface_id = (opened or {}).get("surface_id")
        check(
            "browser.open_split returns a browser surface",
            isinstance(opened, dict)
            and isinstance(surface_id, str)
            and bool(surface_id)
            and opened.get("type") == "browser",
            f"opened={opened}",
        )
        check(
            "browser.open_split focuses the new leaf (model focus)",
            (_safe(client.identify) or {}).get("focused", {}).get("surface_id") == surface_id,
        )

        def browser_state(expected_url: str):
            state = _safe(lambda: client._call("browser.url.get", {"surface_id": surface_id}))
            if not isinstance(state, dict):
                return None
            url = str(state.get("url") or "")
            # Chromium canonicalizes `file:///C:/...`; compare on the path
            # tail after the scheme (fixture paths are plain ASCII).
            tail = expected_url[len("file:///"):]
            if not url.endswith(tail):
                return None
            if state.get("phase") != "ready":
                return None
            return state

        state_one = poll_until(lambda: browser_state(url_one), 40.0)
        check(
            "browser pane reaches ready on the first page",
            isinstance(state_one, dict) and state_one.get("title") == "Page One",
            f"state={state_one}",
        )

        identify = _safe(client.identify) or {}
        focused = (identify or {}).get("focused") or {}
        browser_facts = focused.get("browser") or {}
        check(
            "identify carries the browser facts of the focused pane",
            focused.get("surface_type") == "browser"
            and focused.get("is_browser_surface") is True
            and str(browser_facts.get("url") or "").endswith("one.html")
            and browser_facts.get("title") == "Page One",
            f"focused={focused}",
        )

        surface_rows = client._call("surface.list", {"workspace_id": workspace_one}) or {}
        rows = list((surface_rows or {}).get("surfaces") or [])
        kinds = {str(row.get("id")): str(row.get("type")) for row in rows}
        titles = {str(row.get("id")): row.get("title") for row in rows}
        check(
            "surface rows report both surface kinds",
            set(kinds.values()) == {"terminal", "browser"} and titles.get(surface_id) == "Page One",
            f"rows={rows}",
        )

        # --- navigation fan ---
        client._call("browser.navigate", {"surface_id": surface_id, "url": url_two})
        state_two = poll_until(lambda: browser_state(url_two), 40.0)
        check(
            "navigate lands on page two",
            isinstance(state_two, dict) and state_two.get("title") == "Page Two",
            f"state={state_two}",
        )
        client._call("browser.back", {"surface_id": surface_id})
        back_state = poll_until(
            lambda: (lambda s: s if (s and s.get("can_go_forward") is True
                     and s.get("title") == "Page One") else None)(browser_state(url_one)),
            40.0,
        )
        check(
            "back returns to page one (forward becomes possible)",
            isinstance(back_state, dict) and back_state.get("can_go_forward") is True,
            f"state={back_state}",
        )
        # Back at history start is a no-op SUCCESS (recorded M5.1 decision):
        # the same page stays and the reply stays an ok envelope.
        noop_ok = _safe(lambda: client._call("browser.back", {"surface_id": surface_id})) is not None
        first_page = browser_state(url_one)
        check(
            "back at history start is a no-op success",
            noop_ok and isinstance(first_page, dict) and first_page.get("title") == "Page One",
            f"noop_ok={noop_ok} state={first_page}",
        )
        client._call("browser.forward", {"surface_id": surface_id})
        forward_state = poll_until(
            lambda: (lambda s: s if (s and s.get("can_go_back") is True
                     and s.get("title") == "Page Two") else None)(browser_state(url_two)),
            40.0,
        )
        check(
            "forward returns to page two",
            isinstance(forward_state, dict) and forward_state.get("can_go_back") is True,
            f"state={forward_state}",
        )
        reloaded = _safe(lambda: client._call("browser.reload", {"surface_id": surface_id})) is not None
        reloaded_state = browser_state(url_two)
        check(
            "reload keeps the current page",
            reloaded and isinstance(reloaded_state, dict) and reloaded_state.get("title") == "Page Two",
            f"reloaded={reloaded} state={reloaded_state}",
        )

        # --- error gates ---
        wrong_kind = probe(lambda: client._call("browser.navigate", {"surface_id": terminal_id, "url": url_one}))
        check(
            "browser verb on a terminal surface -> invalid_state",
            isinstance(wrong_kind, cmuxError) and wrong_kind.code == "invalid_state",
            f"error={wrong_kind!r}",
        )
        url_probe = probe(lambda: client._call("browser.url.get", {"surface_id": terminal_id}))
        check(
            "url.get on a terminal surface -> invalid_state",
            isinstance(url_probe, cmuxError) and url_probe.code == "invalid_state",
            f"error={url_probe!r}",
        )
        unknown = probe(lambda: client._call(
            "browser.back", {"surface_id": "cmux-m51-missing"}
        ))
        check(
            "browser verb on an unknown surface -> not_found",
            isinstance(unknown, cmuxError) and unknown.code == "not_found",
            f"error={unknown!r}",
        )
        bad_params = probe(lambda: client._call(
            "browser.navigate", {"surface_id": surface_id, "url": ""}
        ))
        check(
            "empty navigate URL -> invalid_params",
            isinstance(bad_params, cmuxError) and bad_params.code == "invalid_params",
            f"error={bad_params!r}",
        )
        missing_url = probe(lambda: client._call("browser.navigate", {"surface_id": surface_id}))
        check(
            "absent navigate URL -> invalid_params",
            isinstance(missing_url, cmuxError) and missing_url.code == "invalid_params",
            f"error={missing_url!r}",
        )
        text_on_browser = probe(lambda: client.send("echo hi", surface_id))
        check(
            "text verb on a browser surface -> invalid_state",
            isinstance(text_on_browser, cmuxError) and text_on_browser.code == "invalid_state",
            f"error={text_on_browser!r}",
        )
        read_on_browser = probe(lambda: client.read_text(surface_id))
        check(
            "read verb on a browser surface -> invalid_state",
            isinstance(read_on_browser, cmuxError) and read_on_browser.code == "invalid_state",
            f"error={read_on_browser!r}",
        )

        def webview_process_pids(marker: str | None = None) -> set[int]:
            """Pids of msedgewebview2 processes; with `marker`, only those
            whose command line carries it. The app's per-surface user-data
            folder path is exact ownership attribution ambient instances
            cannot meet (machine-wide +/-1 churn broke the count check
            once)."""
            out = subprocess.run(
                ["powershell", "-NoProfile", "-Command",
                 "Get-CimInstance Win32_Process -Filter \"Name='msedgewebview2.exe'\" | "
                 "ForEach-Object { \"$($_.ProcessId)`t$($_.CommandLine)\" }"],
                capture_output=True,
                text=True,
                timeout=30,
            )
            found: set[int] = set()
            for line in (out.stdout or "").splitlines():
                line = line.strip()
                if not line:
                    continue
                pid_text, sep, command = line.partition("\t")
                if not sep or not pid_text.isdigit():
                    continue
                if marker is not None and marker.lower() not in command.lower():
                    continue
                found.add(int(pid_text))
            return found

        # --- webview processes exist per surface (own environment) ---
        peak = webview_process_count()
        check(
            "msedgewebview2 processes run for the browser surface",
            peak > webview_baseline,
            f"peak={peak} baseline={webview_baseline}",
        )
        udf = Path(
            os.environ.get("LOCALAPPDATA", str(tempfile.gettempdir()))
        ) / "cmux" / "webview2" / surface_id
        owned_peak = poll_until(
            lambda: webview_process_pids(str(udf)) or None,
            10.0,
        )
        check(
            "app-owned webview processes carry the per-surface udf command line",
            isinstance(owned_peak, set) and bool(owned_peak),
            f"owned={sorted(owned_peak or [])} marker={udf}",
        )

        # --- child HWND pinned into the pane region ---
        hwnd = poll_until(lambda: main_window_for(process.pid), 20.0)
        if not check("main window found by pid", bool(hwnd)):
            raise SystemExit(1)
        user32.MoveWindow(hwnd, 60, 40, 900, 560, True)
        time.sleep(0.6)
        client_rect = wt.RECT()
        user32.GetClientRect(hwnd, ctypes.byref(client_rect))
        origin = wt.POINT(0, 0)
        user32.ClientToScreen(hwnd, ctypes.byref(origin))
        dpi = user32.GetDpiForWindow(hwnd) or 96
        scale = dpi / 96.0
        client_x, client_y = origin.x, origin.y
        client_w, client_h = client_rect.right, client_rect.bottom
        sidebar_px = round(256 * scale)  # SIDEBAR_WIDTH (design round 1: 256)
        area_w = client_w - sidebar_px
        pane_left_min = client_x + sidebar_px + max(int(area_w * 0.5) - 110, 0)
        pane_left_max = client_x + sidebar_px + int(area_w * 0.5) + 110
        pane_right = client_x + client_w
        pane_top_min = client_y - 16
        # The frameless title strip (design lane 2b) fills the first 32pt
        # of the client: the pane (and the webview child pinned to it)
        # starts that much below the client top. (Owner round 4 removed
        # the lane-3 sub-header; the first pane row is directly below.)
        pane_top_nominal = client_y + round(32 * scale)
        # The pane ends above the status bar (design lane 2): the bar is
        # 24 app points tall, the observed geometry is physical pixels.
        pane_bottom = client_y + client_h - round(24 * scale)

        rect_children = poll_until(
            lambda: [
                child
                for child in child_windows(hwnd)
                if (
                    pane_left_min <= window_rect(child)[0] <= pane_left_max
                    and abs(window_rect(child)[2] - pane_right) <= 16
                    and window_rect(child)[1] >= pane_top_min
                    and window_rect(child)[1] <= pane_top_nominal + 16
                    and abs(window_rect(child)[3] - pane_bottom) <= 16
                    and window_rect(child)[2] - window_rect(child)[0] > 100
                    and window_rect(child)[3] - window_rect(child)[1] > 100
                )
            ],
            15.0,
        )
        classes_seen = [window_class(child) for child in child_windows(hwnd)]
        check(
            "webview child hwnd is pinned to the browser pane rect",
            bool(rect_children),
            f"children={[window_rect(child) + (window_class(child),) for child in child_windows(hwnd)][:6]} "
            f"expected≈left[{pane_left_min}..{pane_left_max}] right={pane_right} "
            f"bottom={pane_bottom} classes={classes_seen[:6]}",
        )

        # The per-surface user data folder exists while (and after) the
        # surface lives (kept on disk; ADR-0006 residual); `udf` itself was
        # defined for process attribution above.
        check("per-surface user data folder exists", udf.is_dir(), f"udf={udf}")

        # --- ctrl+2 through the router vs. the egui bindings ---
        # Control experiment first: with the TERMINAL (egui) side owning
        # native input, ctrl+2 must switch via the ordinary bindings
        # table. This splits signal from the webview-focused case below.
        # NOTE: the app pushes programmatic webview focus when a browser
        # pane gains model focus, so at this point the webview may already
        # own native focus and the router (not egui) may serve this chord;
        # the two routs are distinguished in the transcript by the app's
        # own diagnostic lines.
        foreground_window(hwnd)
        try:
            press(VK_2, ctrl=True)
            egui_switch = poll_until(
                lambda: safe_current(client) != workspace_one,
                10.0,
            )
            check(
                "ctrl+2 switches workspaces (first press, webview-focused model pane)",
                bool(egui_switch),
                f"process_alive={process.poll() is None} idle_ms_probe=pass",
            )
        except cmuxError as error:
            check(
                "ctrl+2 switches workspaces (first press, webview-focused model pane)",
                False,
                f"process_alive={process.poll() is None} ipc_error={error}",
            )

        # Back to workspace one for the webview-facing half of the test.
        # A reply-budget timeout once left the reply lost while the switch
        # still applied server-side, so verify the STATE, not the reply.
        try:
            retry_ipc(lambda: client.select_workspace(workspace_one))
        except cmuxError as error:
            current = safe_current(client)
            check(
                "workspace one re-selected after the switch",
                current == workspace_one,
                f"ipc_error={error} current={current!r}",
            )
        else:
            current = poll_until(
                lambda: (lambda c: c if c == workspace_one else None)(
                    safe_current(client)
                ),
                10.0,
            )
            check(
                "workspace one re-selected after the switch",
                isinstance(current, str) and current == workspace_one,
                f"current={current!r}",
            )
            poll_until(
                lambda: (browser_state(url_two) or {}).get("phase") == "ready",
                20.0,
            )
        # --- ctrl+2 while the webview owns native focus ---
        pin_us_layout(hwnd)
        click_x = client_x + sidebar_px + int(area_w * 0.75)
        click_y = client_y + int(client_h * 0.55)
        click(click_x, click_y)
        time.sleep(0.4)
        # Prove the click really moved the operating system's keyboard
        # focus into the webview window tree: examine each owned thread's
        # GUITHREADINFO (the webview children run on their own threads, so
        # thread id 0 alone does not see them). With that fact, only the
        # accelerator-router route — not egui's own binding table — can
        # deliver the chord below; if focus did not move, the chord check
        # is reported with that caveat rather than silently counted.
        def focus_owner_hwnd():
            candidates = []
            threads = {ctypes.windll.kernel32.GetCurrentThreadId()}
            for child in child_windows(hwnd):
                threads.add(user32.GetWindowThreadProcessId(child, None))
            for tid in threads:
                info = GUITHREADINFO()
                if user32.GetGUIThreadInfo(tid, ctypes.byref(info)):
                    if info.hwndFocus:
                        candidates.append(info.hwndFocus)
            return candidates

        child_set = set(child_windows(hwnd))
        # A click can settle without native focus moving into the
        # webview child on a busy desktop (three distinct failure
        # profiles were recorded from this slot: the gate switching on a
        # plain digit, an empty GUITHREADINFO candidate list, and green
        # runs). Re-click until the probe sees the focus owner in the
        # child, so the checks below assert the settled state.
        for _focus_attempt in range(5):
            focus_candidates = focus_owner_hwnd()
            if any(f in child_set for f in focus_candidates):
                break
            click(click_x, click_y)
            time.sleep(0.4)
        focus_candidates = focus_owner_hwnd()
        focus_in_webview = any(hwnd_focus in child_set for hwnd_focus in focus_candidates)
        check(
            "click moves native focus into the webview child",
            focus_in_webview,
            f"focus_candidates={focus_candidates} classes={[window_class(child) for child in child_windows(hwnd)][:8]}",
        )
        # The webview owns native focus now; egui no longer sees the keys.
        # F1 maps to no character, so per the WebView2 accelerator contract
        # it is ALWAYS an accelerator: if the registration works, the app
        # logs the event even though F1 has no binding (probe).
        press(0x70)  # VK_F1
        time.sleep(0.3)
        try:
            press(VK_2, ctrl=True)
            switched = poll_until(
                lambda: safe_current(client) != workspace_one,
                10.0,
            )
            check(
                "ctrl+2 through the webview routes the workspace chord",
                bool(switched) and focus_in_webview,
                f"focus_in_webview={focus_in_webview} "
                f"process_alive={process.poll() is None}",
            )
        except cmuxError as error:
            check(
                "ctrl+2 through the webview routes the workspace chord",
                False,
                f"focus_in_webview={focus_in_webview} "
                f"process_alive={process.poll() is None} ipc_error={error}",
            )

        # The hidden browser surface survives the switch: show the tab again.
        try:
            retry_ipc(lambda: client.select_workspace(workspace_one))
            shown = poll_until(
                lambda: (lambda s: s if (s and s.get("phase") == "ready"
                         and s.get("title") == "Page Two") else None)(browser_state(url_two)),
                20.0,
            )
            check(
                "hidden pane restores visibility with its state",
                isinstance(shown, dict),
                f"state={shown} process_alive={process.poll() is None}",
            )
        except cmuxError as error:
            check(
                "hidden pane restores visibility with its state",
                False,
                f"process_alive={process.poll() is None} ipc_error={error}",
            )

        # --- M5.2 modifier gate: only a Ctrl-confirmed digit re-routes ---
        # Native focus is back inside the webview child after the re-select
        # (settling a browser pane pushing programmatic webview focus);
        # click anyway to guarantee it. Then: a plain digit is a page
        # keystroke, alt+digit is the browser's accelerator (access keys),
        # and only ctrl+digit re-enters the app's action stream. A switch
        # would surface as `workspace.current` moving off workspace one.
        # M5.2 renovation evidence: desktop apps (a live Firefox window on
        # the smoke host) steal foreground minutes into a run and the
        # chord then goes to them — the press stream therefore re-asserts
        # the app's foreground with the established Alt trick and RECORDS
        # the foreground owner beside every expectation below.
        foreground_window(hwnd)
        fg_is_app = user32.GetForegroundWindow() == hwnd
        click(click_x, click_y)
        time.sleep(0.5)
        # Same settle rule for the gate half: guarantee the plain digit
        # and the alt/ctrl variants are sent with native focus inside
        # the webview child, where the gate is meaningful.
        for _focus_attempt in range(5):
            if any(f in child_set for f in focus_owner_hwnd()):
                break
            click(click_x, click_y)
            time.sleep(0.4)
        press(VK_2)
        switched = poll_until(
            lambda: safe_current(client) != workspace_one,
            6.0,
        )
        check(
            "plain digit through the webview does not switch (M5.2 gate)",
            not switched,
            f"current={safe_current(client)!r} foreground_is_app={fg_is_app}",
        )
        press(VK_2, alt=True)
        switched = poll_until(
            lambda: safe_current(client) != workspace_one,
            6.0,
        )
        check(
            "alt+digit through the webview does not switch (M5.2 gate)",
            not switched,
            f"current={safe_current(client)!r} foreground_is_app={fg_is_app}",
        )
        try:
            # M5.2×M5.3 mitigation: re-assert the app's foreground right
            # before the chord (desktop apps steal it minutes into a run;
            # SendInput goes wholesale to them otherwise — recorded M5.2
            # environment evidence, the failure detail keeps the owner).
            foreground_window(hwnd)
            press(VK_2, ctrl=True)
            switched_again = poll_until(
                lambda: safe_current(client) != workspace_one,
                10.0,
            )
            check(
                "ctrl+2 still routes after the gate (M5.2 regression)",
                bool(switched_again),
                f"process_alive={process.poll() is None} "
                f"foreground_is_app={user32.GetForegroundWindow() == hwnd}",
            )
        except cmuxError as error:
            check(
                "ctrl+2 still routes after the gate (M5.2 regression)",
                False,
                f"process_alive={process.poll() is None} "
                f"foreground_is_app={user32.GetForegroundWindow() == hwnd} "
                f"ipc_error={error}",
            )
        # Leave workspace one active; the teardown/snapshot flow below reads
        # it (and the restore in the restart leg settles from the file).
        retry_ipc(lambda: client.select_workspace(workspace_one))
        poll_until(
            lambda: (browser_state(url_two) or {}).get("phase") == "ready",
            20.0,
        )

        # --- grateful teardown: WM_CLOSE, then the browser tree exits ---
        procs_before_close = webview_process_count()
        owned_before_closed = webview_process_pids(str(udf))
        user32.PostMessageW(hwnd, WM_CLOSE, 0, 0)
        exited = poll_until(lambda: process.poll() == 0, 20.0)
        check("wm_close exits cleanly", exited is True, f"code={process.returncode}")
        gone = poll_until(
            lambda: not (webview_process_pids(str(udf)) & owned_before_closed) or None,
            20.0,
        )
        owned_after = webview_process_pids(str(udf))
        check(
            "browser processes exit with the app",
            bool(gone) and not owned_after,
            f"app-owned before={sorted(owned_before_closed)} after={sorted(owned_after)} "
            f"(machine-wide before={procs_before_close} baseline={webview_baseline})",
        )
        check(
            "user data folder kept after close (documented residual)",
            udf.is_dir(),
            f"udf={udf}",
        )

        # Save-path evidence: the persisted snapshot must carry the browser
        # leaf's live URL (what restore reopens). Print the raw values so a
        # wrong save is attributable from the transcript alone.
        try:
            import json as _json
            saved = _json.loads((fixture / "session.json").read_text(encoding="utf-8"))
        except (OSError, ValueError) as error:
            saved = {"_error": str(error)}
        saved_browser_urls = []

        def _collect_urls(node):
            if isinstance(node, dict):
                if "browserUrl" in node:
                    saved_browser_urls.append(node.get("browserUrl"))
                for value in node.values():
                    _collect_urls(value)
            elif isinstance(node, list):
                for item in node:
                    _collect_urls(item)

        _collect_urls(saved)
        check(
            "session snapshot carries the browser leaf's live URL",
            any(isinstance(url, str) and url.endswith("two.html") for url in saved_browser_urls),
            f"browserUrls={saved_browser_urls}",
        )

        # --- restart: the saved browser leaf restores with its URL ---
        process2, app_log2, _ = launch(shell_bin, fixture, pipe, dict(os.environ), fixture / "app-stderr2.log")
        try:
            client2 = harness.cmux(socket_path=pipe, connect_timeout_s=30.0, timeout_s=5.0)
            ready2 = poll_until(lambda: _safe(client2.identify), 30.0)
            if not check("restart: app plane ready", isinstance(ready2, dict)):
                raise SystemExit(1)
            restored_rows = poll_until(
                lambda: (lambda rows: rows if any(
                    row.get("type") == "browser" for row in rows) else None)(
                    _rows_over_all_workspaces(client2)
                ),
                20.0,
            ) or []
            restored_browser = next(
                (row for row in restored_rows if row.get("type") == "browser"), None
            )
            restored_terminal = sum(
                1 for row in restored_rows if row.get("type") == "terminal"
            )
            check(
                "restart: the browser leaf restores as a browser surface",
                restored_browser is not None and restored_terminal >= 1,
                f"rows={restored_rows}",
            )
            if restored_browser:
                restored_id = str(restored_browser["id"])

                def restored_ready():
                    state = _safe(lambda: client2._call(
                        "browser.url.get", {"surface_id": restored_id}
                    ))
                    if not isinstance(state, dict):
                        return None
                    # The phase flips to ready the instant the controller
                    # wires (the async Navigate may not have committed
                    # yet), so require the saved page's title too.
                    if state.get("phase") != "ready":
                        return None
                    if not str(state.get("url") or "").endswith("two.html"):
                        return None
                    if state.get("title") != "Page Two":
                        return None
                    return state

                url_state = poll_until(restored_ready, 40.0)
                check(
                    "restart: the restored browser reloads the saved page",
                    isinstance(url_state, dict)
                    and str(url_state.get("url") or "").endswith("two.html"),
                    f"state={url_state}",
                )
                # The replay must land on exactly what the session saved:
                # read the saved focus from session.json and compare with
                # the restored identify. Which pane that is follows from
                # the click/chord flow above, not hardcoded — since M5.2
                # the covering webview child's clicks DO update model focus
                # (interceptor claims plus GotFocus routing), so the saved
                # id is whatever the flow focused last.
                session_doc = json.loads(
                    (fixture / "session.json").read_text(encoding="utf-8")
                )
                focus_by_workspace = {
                    ws.get("workspaceId"): ws.get("focusedPanelId")
                    for ws in session_doc["windows"][0]["tabManager"]["workspaces"]
                }
                identify2 = _safe(client2.identify) or {}
                focused2 = (identify2.get("focused") or {})
                saved_focus = focus_by_workspace.get(focused2.get("workspace_id"))
                check(
                    "restart: identify replays the session's saved focus",
                    bool(focused2)
                    and focused2.get("pane_id") == saved_focus,
                    f"identify={identify2} saved_focus={saved_focus!r}",
                )

                # --- M5.2 CDP automation subset (feature row 18) ---
                # Runs on the SECOND instance and only adds surfaces: the
                # session file the first exit wrote stays untouched for
                # everything above, and this instance's final save is not
                # read by any check. The fixture page carries one input,
                # two buttons (a click echo and a double-click counter).
                opened_form = client2._call(
                    "browser.open_split",
                    {"surface_id": restored_id, "url": url_form},
                )
                form_surface = (opened_form or {}).get("surface_id")
                check(
                    "automation: the form surface opens beside the restored leaf",
                    isinstance(form_surface, str)
                    and bool(form_surface)
                    and (opened_form or {}).get("type") == "browser",
                    f"opened_form={opened_form}",
                )

                def form_state(expected_url: str):
                    state = _safe(lambda: client2._call(
                        "browser.url.get", {"surface_id": form_surface}
                    ))
                    if not isinstance(state, dict):
                        return None
                    tail = expected_url[len("file:///"):]
                    if not str(state.get("url") or "").endswith(tail):
                        return None
                    if state.get("phase") != "ready":
                        return None
                    return state

                ready_form = poll_until(lambda: form_state(url_form), 40.0)
                check(
                    "automation: the form page reaches ready",
                    isinstance(ready_form, dict)
                    and ready_form.get("title") == "Form Page",
                    f"state={ready_form} process_alive={process2.poll() is None}",
                )

                # The measurement probe (ADR-0006 Amendment 3): five
                # back-to-back eval round trips over ONE persistent
                # connection, measured client-side. The reply budget is
                # 400 ms app-side; the asserted margin leaves slack.
                timings: list[float] = []
                for index in range(5):
                    started_ts = time.monotonic()
                    client2.browser_eval(str(index + 1), surface=form_surface)
                    timings.append((time.monotonic() - started_ts) * 1000.0)
                timings.sort()
                # The ADR-0006 measurement trail: the actual round-trip
                # numbers belong in the transcript even on a pass.
                print(
                    "INFO automation eval round trips (ms): "
                    f"min={timings[0]:.1f} median={timings[2]:.1f} max={timings[-1]:.1f}"
                )
                check(
                    "automation: eval round trips stay inside the reply budget",
                    timings[-1] < 350.0,
                    f"min={timings[0]:.1f}ms median={timings[2]:.1f}ms max={timings[-1]:.1f}ms",
                )

                snap = client2.browser_snapshot(surface=form_surface)
                refs = {str(row.get("ref")): row for row in (snap.get("refs") or [])}
                check(
                    "snapshot carries the live page identity",
                    str(snap.get("url") or "").endswith("form.html")
                    and snap.get("title") == "Form Page"
                    and isinstance(snap.get("text"), str)
                    and "M52-CDP-FORM" in str(snap.get("text")),
                    f"snap={ {k: snap.get(k) for k in ('url', 'title')} }",
                )
                check(
                    "snapshot mints refs for the controls in DOM order",
                    set(refs) == {"@e1", "@e2", "@e3", "@e4", "@e5", "@e6"}
                    and refs["@e1"].get("selector") == "#answer"
                    and refs["@e1"].get("tag") == "input"
                    and refs["@e2"].get("selector") == "#compose"
                    and refs["@e2"].get("tag") == "button"
                    and refs["@e3"].get("selector") == "#dbl"
                    and refs["@e4"].get("selector") == "#agree"
                    and refs["@e4"].get("tag") == "input"
                    and refs["@e5"].get("selector") == "#lang"
                    and refs["@e5"].get("tag") == "select"
                    and refs["@e6"].get("selector") == "#hoverTarget",
                    f"refs={refs}",
                )

                unknown_ref = probe(
                    lambda: client2.browser_click("e9", surface=form_surface)
                )
                check(
                    "an unknown ref is invalid_params with the re-snapshot hint",
                    isinstance(unknown_ref, cmuxError)
                    and unknown_ref.code == "invalid_params"
                    and "snapshot" in str(unknown_ref),
                    f"error={unknown_ref}",
                )

                oversized = probe(
                    lambda: client2._call(
                        "browser.eval",
                        {
                            "surface_id": form_surface,
                            # Over MAX_PAYLOAD_BYTES (64 KiB) but under the
                            # server's 1 MiB frame limit, so the rejection
                            # happens at the request plane, not transport.
                            "expression": "x" * 70_000,
                        },
                    )
                )
                check(
                    "an oversized expression is rejected at the plane",
                    isinstance(oversized, cmuxError) and oversized.code == "invalid_params",
                    f"error={oversized}",
                )

                clicked = client2.browser_click("@e2", surface=form_surface)
                check(
                    "click echoes the ref it dispatched",
                    (clicked or {}).get("ref") == "@e2"
                    and (clicked or {}).get("click_count") == 1,
                    f"clicked={clicked}",
                )
                after_click = client2.browser_eval(
                    'document.getElementById("out").textContent', surface=form_surface
                )
                check(
                    "a synthetic click fires the page handler",
                    (after_click or {}).get("value") == "composed:",
                    f"after_click={after_click}",
                )

                typed = client2.browser_type("@e1", "hi cmux", surface=form_surface)
                check(
                    "type echoes the text it inserted",
                    (typed or {}).get("ref") == "@e1"
                    and (typed or {}).get("inserted") == "hi cmux",
                    f"typed={typed}",
                )
                typed_value = client2.browser_eval(
                    'document.getElementById("answer").value', surface=form_surface
                )
                check(
                    "inserted text lands in the field",
                    (typed_value or {}).get("value") == "hi cmux",
                    f"typed_value={typed_value}",
                )
                client2.browser_click("@e2", surface=form_surface)
                composed = client2.browser_eval(
                    'document.getElementById("out").textContent', surface=form_surface
                )
                check(
                    "click + type + eval compose the whole chain",
                    (composed or {}).get("value") == "composed:hi cmux",
                    f"composed={composed}",
                )

                dblclicked = client2.browser_dblclick("@e3", surface=form_surface)
                check(
                    "dblclick reports its doubled click count",
                    (dblclicked or {}).get("ref") == "@e3"
                    and (dblclicked or {}).get("click_count") == 2,
                    f"dblclicked={dblclicked}",
                )
                count_value = client2.browser_eval(
                    'document.getElementById("count").textContent', surface=form_surface
                )
                check(
                    "dblclick fires the page handler once per pair",
                    (count_value or {}).get("value") == "1",
                    f"count_value={count_value}",
                )

                # --- M5.3: the observation + form/scroll/screenshot family.
                # Page state rides in from the M5.2 section above (answer =
                # "hi cmux", out echo, count = 1). Both driver flavors show
                # up below: snapshot refs and raw selectors (upstream CLI
                # grammar); reply echoes use whichever the caller sent.
                title_read = client2.browser_get_title(surface=form_surface)
                check(
                    "get.title answers the polled lane title",
                    title_read.get("surface_id") == form_surface
                    and title_read.get("title") == "Form Page",
                    f"title_read={title_read}",
                )

                out_text = client2.browser_get("text", selector="#out", surface=form_surface)
                check(
                    "get.text reads the echo div by selector",
                    out_text.get("value") == "composed:hi cmux",
                    f"out_text={out_text}",
                )

                count_html = client2.browser_get("html", selector="#count", surface=form_surface)
                check(
                    "get.html reads the element verbatim",
                    count_html.get("value") == '<div id="count">1</div>',
                    f"count_html={count_html}",
                )

                answer_value = client2.browser_get("value", selector="#answer", surface=form_surface)
                check(
                    "get.value reads the typed field",
                    answer_value.get("value") == "hi cmux",
                    f"answer_value={answer_value}",
                )

                placeholder = client2.browser_get(
                    "attr", selector="#answer", attr="placeholder", surface=form_surface
                )
                missing_attr = client2.browser_get(
                    "attr", selector="#answer", attr="data-absent", surface=form_surface
                )
                check(
                    "get.attr answers the name or null when absent",
                    placeholder.get("value") == "type here"
                    and missing_attr.get("value") is None,
                    f"placeholder={placeholder} missing_attr={missing_attr}",
                )

                answer_refs = client2.browser_get("count", ref="@e1", surface=form_surface)
                no_match = client2.browser_get("count", selector="#no-such-thing", surface=form_surface)
                check(
                    "get.count counts ref-driven and 0 for absent selectors",
                    answer_refs.get("count") == 1 and no_match.get("count") == 0,
                    f"answer_refs={answer_refs} no_match={no_match}",
                )

                answer_box = client2.browser_get("box", ref="@e1", surface=form_surface)
                box = answer_box.get("value") or {}
                check(
                    "get.box answers the live rectangle",
                    isinstance(box.get("x"), (int, float))
                    and isinstance(box.get("y"), (int, float))
                    and box.get("width", 0) > 0
                    and box.get("height", 0) > 0,
                    f"answer_box={answer_box}",
                )

                styled = client2.browser_get("styles", selector="#out", surface=form_surface)
                colored = client2.browser_get("styles", selector="#out", property="color", surface=form_surface)
                styles_value = styled.get("value") or {}
                check(
                    "get.styles reads the default set and single properties",
                    "display" in styles_value
                    and styles_value.get("display") == "block"
                    and str((colored.get("value") or {}).get("color", "")).startswith(("rgb", "rgba")),
                    f"styled={styled} colored={colored}",
                )

                out_visible = client2.browser_is("visible", selector="#out", surface=form_surface)
                ghost_visible = client2.browser_is("visible", selector="#ghost", surface=form_surface)
                absent_visible = client2.browser_is("visible", selector="#no-such-thing", surface=form_surface)
                check(
                    "is.visible answers rendered, hidden, and missing elements",
                    out_visible.get("value") is True
                    and ghost_visible.get("value") is False
                    and absent_visible.get("value") is False,
                    f"out_visible={out_visible} ghost_visible={ghost_visible} absent_visible={absent_visible}",
                )
                enabled_probe = client2.browser_is("enabled", ref="@e1", surface=form_surface)
                check(
                    "is.enabled answers the ref-driven probe",
                    enabled_probe.get("value") is True,
                    f"enabled_probe={enabled_probe}",
                )

                unchecked = client2.browser_is("checked", ref="@e4", surface=form_surface)
                check(
                    "is.checked starts false on the fresh checkbox ref",
                    unchecked.get("value") is False,
                    f"unchecked={unchecked}",
                )
                checked_box = client2.browser_check(ref="@e4", surface=form_surface)
                check(
                    "check echoes the ref it drove",
                    checked_box.get("ref") == "@e4"
                    and client2.browser_is("checked", ref="@e4", surface=form_surface).get("value") is True,
                    f"checked_box={checked_box}",
                )
                agree_state = client2.browser_eval(
                    'document.getElementById("agreeState").textContent', surface=form_surface
                )
                check(
                    "check fired the page's change handler",
                    (agree_state or {}).get("value") == "checked",
                    f"agree_state={agree_state}",
                )
                client2.browser_uncheck(ref="@e4", surface=form_surface)
                agree_state = client2.browser_eval(
                    'document.getElementById("agreeState").textContent', surface=form_surface
                )
                check(
                    "uncheck returns the checkbox to unchecked",
                    client2.browser_is("checked", ref="@e4", surface=form_surface).get("value") is False
                    and (agree_state or {}).get("value") == "unchecked",
                    f"agree_state={agree_state}",
                )

                not_a_checkbox = probe(
                    lambda: client2.browser_check(selector="#answer", surface=form_surface)
                )
                check(
                    "check on a text input is invalid_state",
                    isinstance(not_a_checkbox, cmuxError)
                    and not_a_checkbox.code == "invalid_state"
                    and "checkbox" in str(not_a_checkbox),
                    f"error={not_a_checkbox}",
                )

                chosen = client2.browser_select("de", ref="@e5", surface=form_surface)
                chosen_label = client2.browser_select("German", ref="@e5", surface=form_surface)
                lang_value = client2.browser_get("value", selector="#lang", surface=form_surface)
                check(
                    "select chooses by value and by label",
                    (chosen or {}).get("ref") == "@e5"
                    and (chosen_label or {}).get("ref") == "@e5"
                    and (lang_value or {}).get("value") == "de",
                    f"chosen={chosen} chosen_label={chosen_label} lang_value={lang_value}",
                )
                no_option = probe(
                    lambda: client2.browser_select("ru", ref="@e5", surface=form_surface)
                )
                check(
                    "select on an unmatched value is invalid_state",
                    isinstance(no_option, cmuxError)
                    and no_option.code == "invalid_state"
                    and "option" in str(no_option),
                    f"error={no_option}",
                )

                filled = client2.browser_fill("filled up", selector="#answer", surface=form_surface)
                filled_value = client2.browser_get("value", selector="#answer", surface=form_surface)
                check(
                    "fill replaces the value with the whole string",
                    (filled or {}).get("selector") == "#answer"
                    and (filled_value or {}).get("value") == "filled up",
                    f"filled={filled} filled_value={filled_value}",
                )

                both_flavors = probe(
                    lambda: client2._call(
                        "browser.click",
                        {"surface_id": form_surface, "ref": "@e2", "selector": "#compose"},
                    )
                )
                check(
                    "ref and selector together are rejected as ambiguous",
                    isinstance(both_flavors, cmuxError)
                    and both_flavors.code == "invalid_params"
                    and "mutually exclusive" in str(both_flavors),
                    f"error={both_flavors}",
                )
                selector_click = client2.browser_click(selector="#dbl", surface=form_surface)
                check(
                    "click accepts the selector flavor (upstream grammar)",
                    (selector_click or {}).get("selector") == "#dbl"
                    and (selector_click or {}).get("click_count") == 1,
                    f"selector_click={selector_click}",
                )

                hovered = client2.browser_hover(selector="#hoverTarget", surface=form_surface)
                hover_state = client2.browser_get("text", selector="#hoverState", surface=form_surface)
                check(
                    "hover moves the real mouse (mouseenter fired)",
                    (hovered or {}).get("selector") == "#hoverTarget"
                    and (hover_state or {}).get("value") == "hovered",
                    f"hovered={hovered} hover_state={hover_state}",
                )

                scrolled = client2.browser_scroll(dy=500, surface=form_surface)
                scrolled_back = client2.browser_scroll(dy=-500, surface=form_surface)
                # The verb's contract is the vertical delta and the reply's
                # offsets. The page's own horizontal offset stays whatever
                # it was (an 8px Chromium viewport-rounding sliver: constant
                # across both calls, untouched by a dy-only scrollBy) —
                # asserting stability, not zero.
                check(
                    "scroll reports the resulting offsets and reverses",
                    (scrolled or {}).get("x") == (scrolled_back or {}).get("x")
                    and (scrolled or {}).get("y") == 500
                    and (scrolled_back or {}).get("y") == 0,
                    f"scrolled={scrolled} scrolled_back={scrolled_back}",
                )

                into_view = client2.browser_scroll_into_view(
                    selector="#bottom", surface=form_surface
                )
                scroll_y = client2.browser_eval("window.scrollY", surface=form_surface)
                check(
                    "scroll_into_view centers the bottom element",
                    (into_view or {}).get("selector") == "#bottom"
                    and (scroll_y or {}).get("value", 0) > 1000,
                    f"into_view={into_view} scroll_y={scroll_y}",
                )

                focused_state = client2.browser_is_webview_focused(surface=form_surface)
                check(
                    "is_webview_focused answers a bool",
                    isinstance((focused_state or {}).get("focused"), bool),
                    f"focused_state={focused_state}",
                )
                try:
                    client2.browser_focus_webview(surface=form_surface)
                    got_focus = poll_until(
                        lambda: _safe(
                            lambda: client2.browser_is_webview_focused(surface=form_surface)
                        )
                        or {},
                        10.0,
                    )
                except cmuxError as error:
                    got_focus = {"focused": False, "error": str(error)}
                check(
                    "focus_webview hands keyboard focus to the webview",
                    (got_focus or {}).get("focused") is True,
                    f"got_focus={got_focus}",
                )

                # --- M5.4: key synthesis on the focused element + the
                # wait family. State rides in: the answer field focuses
                # per call, so the keyboard flow clears it first.
                client2.browser_eval(
                    'document.getElementById("answer").value = ""', surface=form_surface
                )
                client2.browser_type(selector="#answer", text="abc", surface=form_surface)
                value_after_backspace = None
                client2.browser_press("Backspace", surface=form_surface)
                value_after_backspace = client2.browser_get(
                    "value", selector="#answer", surface=form_surface
                )
                check(
                    "press Backspace edits the focused field (raw key down)",
                    (value_after_backspace or {}).get("value") == "ab",
                    f"value_after_backspace={value_after_backspace}",
                )

                client2.browser_press("Control+a", surface=form_surface)
                span = client2.browser_eval(
                    'document.getElementById("answer").selectionEnd - document.getElementById("answer").selectionStart',
                    surface=form_surface,
                )
                check(
                    "press Control+a selects all (the modifier bit reached the page)",
                    (span or {}).get("value") == 2,
                    f"span={span}",
                )
                client2.browser_press("Backspace", surface=form_surface)
                cleared = client2.browser_get("value", selector="#answer", surface=form_surface)
                check(
                    "press Backspace empties the Control+a selection",
                    (cleared or {}).get("value") == "",
                    f"cleared={cleared}",
                )

                client2.browser_press("Enter", surface=form_surface)
                key_log = client2.browser_get("text", selector="#keyLog", surface=form_surface)
                logged = key_log.get("value") or ""
                print(f"INFO automation keyLog={logged!r}")
                check(
                    "press burns down+up pairs with the ctrl flag visible",
                    "D:a[C]" in logged
                    and "D:Backspace;U:Backspace" in logged
                    and "D:Enter" in logged,
                    f"logged={logged!r}",
                )
                client2.browser_keydown("Shift", surface=form_surface)
                client2.browser_keyup("Shift", surface=form_surface)
                shifted_log = client2.browser_get(
                    "text", selector="#keyLog", surface=form_surface
                )
                shifted = shifted_log.get("value") or ""
                check(
                    "keydown/keyup halves land in order across calls",
                    "D:Shift;U:Shift" in shifted and shifted.count("D:") >= 3,
                    f"shifted={shifted!r}",
                )

                wait_ok = client2.browser_wait(selector="#out", surface=form_surface)
                check(
                    "wait for a selector resolves on the first poll",
                    (wait_ok or {}).get("surface_id") == form_surface
                    and 0 <= (wait_ok or {}).get("waited_ms", 10**9) < 2000,
                    f"wait_ok={wait_ok}",
                )
                wait_url = client2.browser_wait(url_contains="form.html", surface=form_surface)
                check(
                    "wait url_contains resolves the live URL",
                    isinstance((wait_url or {}).get("waited_ms"), int),
                    f"wait_url={wait_url}",
                )
                wait_pair = client2.browser_wait(
                    load_state="domcontentloaded",
                    url_contains="form.html",
                    surface=form_surface,
                )
                wait_and = client2.browser_wait(
                    selector="#out", text_contains="M52-CDP-FORM", surface=form_surface
                )
                check(
                    "wait satisfies the AND of given predicates",
                    isinstance((wait_pair or {}).get("waited_ms"), int)
                    and isinstance((wait_and or {}).get("waited_ms"), int),
                    f"wait_pair={wait_pair} wait_and={wait_and}",
                )
                counted = client2.browser_wait(
                    function="(window.__w = (window.__w || 0) + 1, window.__w >= 3)",
                    timeout_ms=5000,
                    surface=form_surface,
                )
                check(
                    "wait re-polls until the function readies (and reports the elapsed)",
                    (counted or {}).get("waited_ms", 0) > 0,
                    f"counted={counted}",
                )
                timed_out = probe(
                    lambda: client2.browser_wait(
                        text_contains="never-there", timeout_ms=500, surface=form_surface
                    )
                )
                check(
                    "wait past the deadline answers the structured timeout",
                    isinstance(timed_out, cmuxError)
                    and timed_out.code == "timeout"
                    and "500 ms" in str(timed_out),
                    f"error={timed_out}",
                )

                unknown_key = probe(
                    lambda: client2.browser_press("Frobnicate", surface=form_surface)
                )
                check(
                    "press on unknown vocabulary is structured invalid_params",
                    isinstance(unknown_key, cmuxError)
                    and unknown_key.code == "invalid_params"
                    and "not a known key" in str(unknown_key),
                    f"error={unknown_key}",
                )

                stage = Path(os.environ.get("TEMP", tempfile.gettempdir())) / "cmux-screenshots"
                shot = client2.browser_screenshot(surface=form_surface)
                shot_path = (shot or {}).get("path")
                shot_bytes = b""
                if isinstance(shot_path, str) and shot_path:
                    try:
                        shot_bytes = Path(shot_path).read_bytes()[:4]
                    except OSError:
                        shot_bytes = b""
                print(
                    f"INFO automation screenshot path={shot_path!r} "
                    f"magic={shot_bytes!r} staged_dir={stage}"
                )
                check(
                    "screenshot stages a real PNG under the temp lane",
                    isinstance(shot_path, str)
                    and shot_path.startswith(str(stage))
                    and shot_bytes == b"\x89PNG",
                    f"shot={shot} head_bytes={shot_bytes}",
                )
                rows2 = _rows_over_all_workspaces(client2)
                terminal2 = next(
                    (row["id"] for row in rows2 if row.get("type") == "terminal"), None
                )
                terminal_eval = probe(
                    lambda: client2.browser_eval("1", surface=terminal2)
                )
                check(
                    "an automation verb on a terminal surface is invalid_state",
                    isinstance(terminal_eval, cmuxError)
                    and terminal_eval.code == "invalid_state",
                    f"terminal2={terminal2!r} error={terminal_eval}",
                )
                terminal_press = probe(
                    lambda: client2.browser_press("Enter", surface=terminal2)
                )
                check(
                    "press on a terminal surface is invalid_state",
                    isinstance(terminal_press, cmuxError)
                    and terminal_press.code == "invalid_state",
                    f"terminal2={terminal2!r} error={terminal_press}",
                )
                terminal_title = probe(
                    lambda: client2.browser_get_title(surface=terminal2)
                )
                terminal_focus = probe(
                    lambda: client2.browser_is_webview_focused(surface=terminal2)
                )
                check(
                    "the sync focus/title verbs reject terminal surfaces",
                    isinstance(terminal_title, cmuxError)
                    and terminal_title.code == "invalid_state"
                    and isinstance(terminal_focus, cmuxError)
                    and terminal_focus.code == "invalid_state",
                    f"terminal_title={terminal_title} terminal_focus={terminal_focus}",
                )

                # Refs die with the shown document (upstream ephemerality):
                # navigate the form away, then the old ref must not re-resolve.
                client2._call(
                    "browser.navigate", {"surface_id": form_surface, "url": url_one}
                )
                moved = poll_until(lambda: form_state(url_one), 40.0)
                check(
                    "navigation to the plain page succeeds",
                    isinstance(moved, dict) and moved.get("title") == "Page One",
                    f"moved={moved}",
                )
                stale_click = probe(
                    lambda: client2.browser_click("@e2", surface=form_surface)
                )
                check(
                    "navigation kills the snapshot's refs",
                    isinstance(stale_click, cmuxError)
                    and stale_click.code == "invalid_params",
                    f"error={stale_click}",
                )
            # Close the second instance the same way.
            hwnd2 = poll_until(lambda: main_window_for(process2.pid), 20.0)
            if hwnd2:
                user32.PostMessageW(hwnd2, WM_CLOSE, 0, 0)
                poll_until(lambda: process2.poll() == 0, 20.0)
        finally:
            if process2.poll() is None:
                process2.terminate()
                try:
                    process2.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    subprocess.run(
                        ["taskkill", "/F", "/T", "/PID", str(process2.pid)],
                        stdout=subprocess.DEVNULL,
                        stderr=subprocess.DEVNULL,
                    )
                    process2.wait(timeout=10)
            app_log2.close()

        # --- no panics across both runs ---
        try:
            tail_text = (fixture / "app-stderr.log").read_text(errors="replace")
            tail_text += (fixture / "app-stderr2.log").read_text(errors="replace")
        except OSError:
            tail_text = ""
        check("no panic in the app stderr", "panicked" not in tail_text)

    finally:
        if process is not None and process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                subprocess.run(
                    ["taskkill", "/F", "/T", "/PID", str(process.pid)],
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                )
                process.wait(timeout=10)
        time.sleep(0.3)
        if app_log is not None:
            app_log.close()
        # Always keep the evidence in the transcript: both stderr logs'
        # tails, whatever the outcome (a mid-run crash used to discard
        # them with the fixture).
        for name in ("app-stderr.log", "app-stderr2.log"):
            try:
                tail = (fixture / name).read_text(errors="replace")[-3000:]
                print(f"---- {name} tail ----")
                print(tail)
            except OSError:
                pass
        shutil.rmtree(fixture, ignore_errors=True)

    print()
    print(f"smoke complete: {len(PASS)} passed, {len(FAIL)} failed")
    if FAIL:
        print("failed checks:", ", ".join(FAIL))
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(smoke())
