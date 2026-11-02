"""M4.3 live smoke: real terminal-UX input over the egui_tty surface.

Driver lane parity rows 5/6/7 (M4.3): what the ghostty engine underneath
cannot prove by itself — that a person's real window events reach the
emulator. Everything here is injected the way the C library would do it:
`SendInput` keyboard/mouse events, `SetWindowPos` resize, the Windows
clipboard, `PostMessageW(WM_CLOSE)`. No pytest, no extra deps (ctypes
only), the same assert-script style as `test_automation_m4.py`.

Checks (each must pass):
  - fonts log line: the machine's mono primary + a CJK fallback loaded
  - window found by pid and foregrounded
  - a click inside the pane area keeps the model's focused surface
  - typed text lands (full stack: SendInput -> winit -> egui -> ghostty
    encoding -> ConPTY -> PowerShell echo)
  - Up/Up recalls previous history through the same stack
  - Ctrl+C interrupts the recalled line (empty selection -> ETX) and
    exactly one copy of the first marker remains visible
  - drag-select + Ctrl+C puts the selected text on the real clipboard
  - Ctrl+V pastes the clipboard into the prompt line
  - an IME-style unicode burst (no key events) renders the CJK text
  - scroll wheel moves back into scrollback and forward again
  - rapid SetWindowPos hammering does not wedge or crash the app
  - WM_CLOSE exits cleanly (session file written)

Hermetic: unique pipe, temp session file, a config that pins
`powershell.exe -NoLogo -NoProfile` so prompts behave identically on any
machine. Exit 0 only when every check printed PASS.
"""

from __future__ import annotations

import argparse
import ctypes
import ctypes.wintypes as wt
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

PASS: list[str] = []
FAIL: list[str] = []

# The TUI console may be cp1252: never let a diagnostic tail crash the run.
if sys.stdout and hasattr(sys.stdout, "reconfigure"):
    sys.stdout.reconfigure(encoding="utf-8", errors="backslashreplace")
    sys.stderr.reconfigure(encoding="utf-8", errors="backslashreplace")


def check(name: str, condition: bool, detail: str = "") -> bool:
    if condition:
        PASS.append(name)
        print(f"PASS {name}")
        return True
    FAIL.append(name)
    print(f"FAIL {name} {detail}".rstrip())
    return False


def poll_until(fn, deadline_s: float, interval_s: float = 0.2):
    """Calls `fn` until it returns truthy (or the deadline expires)."""
    end = time.monotonic() + deadline_s
    last = None
    while time.monotonic() < end:
        last = fn()
        if last:
            return last
        time.sleep(interval_s)
    return last


# --------------------------- Win32 input layer --------------------------

user32 = ctypes.WinDLL("user32", use_last_error=True)
kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
shcore = ctypes.WinDLL("shcore")

# Explicit prototypes: default ctypes restypes (32-bit int) truncate
# 64-bit handles and crash the read.
user32.GetClipboardData.restype = ctypes.c_void_p
user32.GetClipboardData.argtypes = [ctypes.c_uint]
user32.SetClipboardData.restype = ctypes.c_void_p
user32.SetClipboardData.argtypes = [ctypes.c_uint, ctypes.c_void_p]
user32.EmptyClipboard.argtypes = []
user32.SetClipboardData.argtypes = [ctypes.c_uint, ctypes.c_void_p]
kernel32.GlobalAlloc.restype = ctypes.c_void_p
kernel32.GlobalAlloc.argtypes = [wt.WORD, ctypes.c_size_t]
kernel32.GlobalLock.restype = ctypes.c_void_p
kernel32.GlobalLock.argtypes = [ctypes.c_void_p]
kernel32.GlobalUnlock.argtypes = [ctypes.c_void_p]
kernel32.GlobalFree.argtypes = [ctypes.c_void_p]
user32.SetForegroundWindow.argtypes = [wt.HWND]
user32.SetForegroundWindow.restype = wt.BOOL
user32.GetForegroundWindow.restype = wt.HWND
user32.PostMessageW.argtypes = [wt.HWND, wt.UINT, wt.WPARAM, wt.LPARAM]
user32.MoveWindow.argtypes = [wt.HWND, ctypes.c_int, ctypes.c_int, ctypes.c_int, ctypes.c_int, wt.BOOL]
user32.GetClientRect.argtypes = [wt.HWND, ctypes.POINTER(wt.RECT)]
user32.ClientToScreen.argtypes = [wt.HWND, ctypes.POINTER(wt.POINT)]
user32.GetWindowRect.argtypes = [wt.HWND, ctypes.POINTER(wt.RECT)]
user32.GetWindowThreadProcessId.argtypes = [wt.HWND, ctypes.POINTER(wt.DWORD)]


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


INPUT_MOUSE, INPUT_KEYBOARD = 0, 1
KEYEVENTF_KEYUP = 0x0002
KEYEVENTF_UNICODE = 0x0004
MOUSEEVENTF_LEFTDOWN = 0x0002
MOUSEEVENTF_LEFTUP = 0x0004
MOUSEEVENTF_MOVE = 0x0001
MOUSEEVENTF_WHEEL = 0x0800
MOUSEEVENTF_ABSOLUTE = 0x8000
WHEEL_DELTA = 120
WM_CLOSE = 0x0010
CF_UNICODETEXT = 13
GMEM_MOVEABLE = 0x0002
VK_RETURN, VK_UP, VK_CONTROL, VK_C, VK_L, VK_V, VK_MENU = 0x0D, 0x26, 0x11, 0x43, 0x4C, 0x56, 0x12
EINVAL_NONE = 0


def _send(*inputs):
    array = (INPUT * len(inputs))(*inputs)
    sent = user32.SendInput(len(inputs), array, ctypes.sizeof(INPUT))
    if sent != len(inputs):
        raise OSError(f"SendInput accepted {sent}/{len(inputs)} events")


def key(vk, up: bool = False) -> INPUT:
    return INPUT(
        type=INPUT_KEYBOARD,
        ki=KEYBDINPUT(wVk=vk, wScan=0, dwFlags=KEYEVENTF_KEYUP if up else 0, time=0, dwExtraInfo=None),
    )


def key_unicode(char: str) -> list[INPUT]:
    events = []
    for flag in (0, KEYEVENTF_KEYUP):
        events.append(
            INPUT(
                type=INPUT_KEYBOARD,
                ki=KEYBDINPUT(wVk=0, wScan=ord(char), dwFlags=KEYEVENTF_UNICODE | flag, time=0, dwExtraInfo=None),
            )
        )
    return events


def type_text(text: str) -> None:
    """Types literal characters (no layout dependence) with tiny gaps."""
    for char in text:
        _send(*key_unicode(char))
        time.sleep(0.01)


def press(vk: int, ctrl: bool = False, shift: bool = False) -> None:
    mods = [VK_CONTROL] if ctrl else []
    if shift:
        mods.append(0x10)
    _send(*[key(m) for m in mods])
    _send(key(vk), key(vk, up=True))
    _send(*[key(m, up=True) for m in mods])


def send_enter() -> None:
    _send(key(VK_RETURN), key(VK_RETURN, up=True))


def set_cursor(x: int, y: int) -> None:
    if not user32.SetCursorPos(int(x), int(y)):
        raise OSError(f"SetCursorPos failed ({ctypes.get_last_error()})")


def click(x: int, y: int) -> None:
    """One press and release, in separate egui frames.

    egui collates per frame; a debug build's frame period is ~30 ms. If
    the down and up land in one frame the widget's press branch fires and
    the release is swallowed by the elif chain, so the gesture machine
    never sees the release and repeated presses can't advance the click
    count (M4.3 evidence). 80 ms separates the two frames while keeping
    press-to-press under the 400 ms repeat-click interval.
    """
    set_cursor(x, y)
    time.sleep(0.05)
    _send(
        INPUT(type=INPUT_MOUSE, mi=MOUSEINPUT(dx=0, dy=0, mouseData=0, dwFlags=MOUSEEVENTF_LEFTDOWN, time=0, dwExtraInfo=None))
    )
    time.sleep(0.08)
    _send(
        INPUT(type=INPUT_MOUSE, mi=MOUSEINPUT(dx=0, dy=0, mouseData=0, dwFlags=MOUSEEVENTF_LEFTUP, time=0, dwExtraInfo=None))
    )
    time.sleep(0.08)


def drag(x1: int, y1: int, x2: int, y2: int) -> None:
    """Press, move, release — all through SendInput.

    `SetCursorPos` alone didn't deliver pointer-move input while the
    button was down (no selection ever formed in any terminal), so every
    position change is a real injected mouse event with absolute screen
    coordinates.
    """
    screen_w, screen_h = user32.GetSystemMetrics(0), user32.GetSystemMetrics(1)

    def move_abs(x: int, y: int) -> INPUT:
        return INPUT(
            type=INPUT_MOUSE,
            mi=MOUSEINPUT(
                dx=round(x * 65535 / max(screen_w - 1, 1)),
                dy=round(y * 65535 / max(screen_h - 1, 1)),
                mouseData=0,
                dwFlags=MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE,
                time=0,
                dwExtraInfo=None,
            ),
        )

    def button(flags: int) -> INPUT:
        return INPUT(
            type=INPUT_MOUSE,
            mi=MOUSEINPUT(dx=0, dy=0, mouseData=0, dwFlags=flags, time=0, dwExtraInfo=None),
        )

    events = [move_abs(x1, y1), button(MOUSEEVENTF_LEFTDOWN)]
    steps = 6
    for step in range(1, steps + 1):
        events.append(
            move_abs(round(x1 + (x2 - x1) * step / steps), round(y1 + (y2 - y1) * step / steps))
        )
    events.append(button(MOUSEEVENTF_LEFTUP))
    # Dwell around EVERY event: a debug build's frame period can exceed
    # 200 ms, and the widget grows the selection only on frames between
    # down and up — if the moves collapse into those two frames the
    # gesture is "a click that never moved" and Copy becomes an
    # interrupt (M4.3 evidence).
    _send(events[0])
    _send(events[1])
    time.sleep(0.5)
    for event in events[2:-1]:
        _send(event)
        time.sleep(0.35)
    time.sleep(0.5)
    _send(events[-1])


def wheel(x: int, y: int, ticks: int) -> None:
    """`ticks > 0` scrolls back into history; negative goes forward."""
    set_cursor(x, y)
    time.sleep(0.05)
    per = ticks * WHEEL_DELTA
    for _ in range(3):
        _send(
            INPUT(
                type=INPUT_MOUSE,
                mi=MOUSEINPUT(dx=0, dy=0, mouseData=per, dwFlags=MOUSEEVENTF_WHEEL, time=0, dwExtraInfo=None),
            )
        )
        time.sleep(0.03)


def foreground_window(hwnd: int) -> bool:
    """The Alt trick: a synthesized Alt press grants SetForegroundWindow."""
    _send(key(VK_MENU))
    ok = bool(user32.SetForegroundWindow(hwnd))
    _send(key(VK_MENU, up=True))
    return ok


WM_INPUTLANGCHANGEREQUEST = 0x0050


def pin_us_layout(hwnd: int) -> None:
    """Pin the window to the US keyboard layout.

    This machine's preload list carries Arabic (Egypt) as a second layout
    and the app's window thread sometimes adopts it, where VK_C resolves
    to a different character and the Ctrl+C copy/interrupt chord never
    matches (M4.3 layout-diag evidence). LoadKeyboardLayoutW +
    WM_INPUTLANGCHANGEREQUEST reliably re-activates the US layout.
    """
    handle = user32.LoadKeyboardLayoutW("00000409", 1)  # KLF_ACTIVATE
    user32.PostMessageW(hwnd, WM_INPUTLANGCHANGEREQUEST, 0, handle)
    time.sleep(0.4)


def clipboard_get() -> str:
    if not user32.OpenClipboard(None):
        return ""
    try:
        handle = user32.GetClipboardData(CF_UNICODETEXT)
        if not handle:
            return ""
        data_ptr = kernel32.GlobalLock(handle)
        if not data_ptr:
            return ""
        try:
            # NUL-terminated wide string owned by the clipboard block.
            return ctypes.cast(data_ptr, ctypes.c_wchar_p).value or ""
        finally:
            kernel32.GlobalUnlock(handle)
    finally:
        user32.CloseClipboard()


def clipboard_set(text: str) -> None:
    if not user32.OpenClipboard(None):
        raise OSError("OpenClipboard failed")
    try:
        user32.EmptyClipboard()
        size = ctypes.sizeof(wt.WCHAR) * (len(text) + 1)
        handle = kernel32.GlobalAlloc(0x0002, size)
        data_ptr = kernel32.GlobalLock(handle)
        ctypes.memmove(data_ptr, ctypes.create_unicode_buffer(text), size)
        kernel32.GlobalUnlock(handle)
        if not user32.SetClipboardData(CF_UNICODETEXT, handle):
            kernel32.GlobalFree(handle)
            raise OSError("SetClipboardData failed")
    finally:
        user32.CloseClipboard()


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


class ClientSurface:
    """Positions the app's client area and computes injection targets."""

    def __init__(self, hwnd: int):
        self.hwnd = hwnd
        user32.MoveWindow(hwnd, 60, 40, 900, 560, True)
        time.sleep(0.4)
        self.refresh()

    def refresh(self) -> None:
        rect = wt.RECT()
        user32.GetClientRect(self.hwnd, ctypes.byref(rect))
        point = wt.POINT(0, 0)
        # ClientToScreen translates without needing Shell APIs.
        ctypes.windll.user32.ClientToScreen(self.hwnd, ctypes.byref(point))
        self.x, self.y = point.x, point.y
        self.w, self.h = rect.right, rect.bottom
        # The app's SIDEBAR_WIDTH (256 since design round 1, 2026-10-26;
        # it painted 216 before) is egui logical points; the pointer
        # pushes physical screen pixels. Scale by the window's DPI or
        # every drag presses the sidebar instead of the pane
        # (pane rect [[256,0]-[900,560]] in logical at the default
        # window).
        dpi = user32.GetDpiForWindow(self.hwnd) or 96
        self.scale = dpi / 96.0
        self.side = round(self.SIDE * self.scale)

    SIDE = 256  # SIDEBAR_WIDTH, in egui logical points

    def terminal_center(self) -> tuple[int, int]:
        return (self.x + self.side + int((self.w - self.side) * 0.5), self.y + int(self.h * 0.55))

    def drag_span(self) -> tuple[int, int, int, int]:
        # NOTE: a whole-pane span selects mostly empty grid cells; the
        # battery instead measures the content rows from read_text and
        # single-row drags (see the selection step in main()).
        x1, y1 = self.x + self.SIDE + 20, self.y + 10
        x2, y2 = self.x + self.w - 40, self.y + int(self.h * 0.5)
        return x1, y1, x2, y2


def _safe(fn):
    try:
        return fn()
    except cmuxError:
        return None


def last_nonempty(line_text: str) -> str:
    lines = [entry.strip() for entry in (line_text or "").splitlines() if entry.strip()]
    return lines[-1] if lines else ""


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

    pipe = rf"\\.\pipe\cmux-m43-smoke-{os.getpid()}-{random.randint(1000, 9999)}"
    fixture = Path(tempfile.mkdtemp(prefix="cmux-m43-smoke-"))
    config = (
        '{\n  "terminal": { "shell": { "program": "powershell.exe", '
        '"args": ["-NoLogo", "-NoProfile"] } }\n}\n'
    )
    (fixture / "cmux.json").write_text(config, encoding="utf-8")
    env = dict(os.environ)
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

    app_log = open(fixture / "app-stderr.log", "ab")
    process = subprocess.Popen(
        [str(shell_bin)],
        cwd=str(fixture),
        env=env,
        stdout=subprocess.DEVNULL,
        stderr=app_log,
    )
    print(f"launched {shell_bin.name} pid={process.pid} pipe={pipe}")

    client = None
    try:
        client = harness.cmux(socket_path=pipe, connect_timeout_s=30.0, timeout_s=5.0)
        check("ping answers pong", poll_until(lambda: _safe(client.ping), 30.0) is True)

        # Fonts: the machine's primary mono face plus a CJK fallback must
        # be in the startup log (egui defaults alone would tofu the IME).
        app_log.flush()
        fonts_line = poll_until(
            lambda: next(
                (
                    entry
                    for entry in (fixture / "app-stderr.log").read_text(errors="replace").splitlines()
                    if entry.startswith("cmux-shell: fonts:")
                ),
                None,
            ),
            20.0,
        )
        check(
            "fonts log line carries the machine faces",
            fonts_line is not None and "YaHei" in fonts_line and ("Consolas" in fonts_line or "Cascadia" in fonts_line),
            f"line={fonts_line!r}",
        )

        hwnd = poll_until(lambda: main_window_for(process.pid), 20.0)
        surfaced = ClientSurface(hwnd) if hwnd else None
        check("main window found by pid", surfaced is not None)
        if surfaced is None:
            raise SystemExit(1)
        check(
            "window foregrounded",
            poll_until(lambda: foreground_window(hwnd) and user32.GetForegroundWindow() == hwnd, 10.0)
            is True,
        )
        # Deterministic chords: the window must not sit on the Arabic
        # preload layout when the keyboard steps run.
        pin_us_layout(hwnd)

        ident = client.identify()
        surface_id = (ident.get("focused") or {}).get("surface_id", "")
        check("identify carries the focused surface", bool(surface_id), f"identify={ident}")

        cx, cy = surfaced.terminal_center()
        click(cx, cy)
        time.sleep(0.5)
        ident = client.identify()
        check("pane click keeps the focused surface", (ident.get("focused") or {}).get("surface_id") == surface_id)

        # Typed text through the whole stack, then Enter via a real VK.
        marker1 = f"echo cmux-UX-MARK1-{random.randint(10000, 99999)}"
        type_text(marker1)
        send_enter()
        seen = poll_until(lambda: _safe(lambda: marker1 in (client.read_text(surface_id) or "")), 20.0)
        check("typed marker becomes visible", bool(seen), "marker not visible after typing")

        marker2 = f"echo cmux-UX-MARK2-{random.randint(10000, 99999)}"
        type_text(marker2)
        send_enter()
        seen = poll_until(
            lambda: _safe(lambda: marker2 in (client.read_text(surface_id) or "")), 20.0
        )
        check("second marker becomes visible", bool(seen), "marker2 not visible")

        # History recall: two Up presses walk from MARK2 back to MARK1.
        press(VK_UP)
        time.sleep(0.15)
        press(VK_UP)
        time.sleep(0.4)
        recalled = poll_until(
            lambda: _safe(lambda: last_nonempty(client.read_text(surface_id)).startswith("PS") and marker1 in last_nonempty(client.read_text(surface_id))),
            10.0,
        )
        check("arrow-up recalls the earlier command", bool(recalled), f"tail={last_nonempty(client.read_text(surface_id) or '')!r}")

        # Ctrl+C with an empty selection interrupts the recalled line:
        # PSReadLine echoes `^C` onto the recalled line and re-renders the
        # prompt below it (Windows Terminal parity; requires no forced
        # TERM in the pane env — M4.3 evidence).
        press(VK_C, ctrl=True)
        interrupted = poll_until(
            lambda: _safe(
                lambda: any(
                    line.rstrip().endswith("^C") and marker1 in line
                    for line in (client.read_text(surface_id) or "").splitlines()
                )
            ),
            5.0,
        )
        tail_after = _safe(lambda: last_nonempty(client.read_text(surface_id) or ""))
        check(
            "ctrl+c interrupts the recalled line",
            bool(interrupted) and tail_after.startswith("PS") and marker1 not in tail_after,
            f"tail={tail_after!r}",
        )

        # Selection + copy: echo two markers, drag a box over the top
        # rows (where the content is), Ctrl+C (copy wins over ETX when
        # text is selected).
        alpha = f"echo SELECT-ALPHA-{random.randint(10000, 99999)}"
        beta = f"echo SELECT-BETA-{random.randint(10000, 99999)}"
        type_text(alpha)
        send_enter()
        type_text(beta)
        send_enter()
        poll_until(lambda: _safe(lambda: beta in (client.read_text(surface_id) or "")), 20.0)
        # Select grid row 0 (the first prompt line, which carries the
        # MARK1 echo): a short single-row drag at a y that clamps to the
        # top row regardless of the tab strip's offset, then Ctrl+C (copy
        # wins over ETX when text is selected). This is the drag shape
        # verified to hold a selection under injection.
        #
        # The frameless title strip (design lane 2b) fills the first
        # 32pt of the client — the lane 3 sub-header band went out in
        # owner round 4, so the chrome is the title strip alone. A drag
        # in the strip is HTCAPTION and MOVES the window instead of
        # selecting text. Anchor on the first pane row below the chrome.
        copied = False
        strip_px = round(32 * surfaced.scale)
        for _attempt in range(2):
            x_from = surfaced.x + surfaced.side + 30
            y_row = surfaced.y + strip_px + 12
            drag(x_from, y_row, x_from + 380, y_row)
            time.sleep(0.3)
            press(VK_C, ctrl=True)
            # The clipboard set is lazy (arboard inside egui): poll for it.
            copied = poll_until(
                lambda: "cmux-UX-MARK1-" in clipboard_get(),
                5.0,
            )
            if copied:
                break
        check(
            "selected text reaches the clipboard",
            bool(copied),
            f"clipboard={clipboard_get()[:200]!r}",
        )

        # Paste: write the clipboard, click (clears the selection), Ctrl+V.
        pasted = f"PASTED-MARK-{random.randint(10000, 99999)}"
        clipboard_set(pasted)
        click(cx, cy)
        time.sleep(0.3)
        press(VK_V, ctrl=True)
        seen = poll_until(lambda: _safe(lambda: pasted in (client.read_text(surface_id) or "")), 15.0)
        check("ctrl+v pastes from the clipboard", bool(seen), f"clipboard was {pasted!r}")

        # The IME path: characters with no key events behind them (exactly
        # what a committed composition delivers) must render, not tofu.
        # Wide CJK cells come back with spacer cells in read_text, so the
        # comparison strips spaces on both sides.
        press(VK_C, ctrl=True)
        time.sleep(0.3)
        type_text("\u4F60\u597D")
        time.sleep(0.6)
        seen = poll_until(
            lambda: _safe(
                lambda: "\u4F60\u597D"
                in (client.read_text(surface_id) or "").replace(" ", "")
            ),
            10.0,
        )
        check("unicode-only burst renders as text", bool(seen), f"text=...{(client.read_text(surface_id) or '')[-80:]!r}...")
        press(VK_C, ctrl=True)
        time.sleep(0.3)

        # Scrollback: push ~120 output lines, wheel back and forward.
        type_text("1..120 | ForEach-Object { \"scroll-$_\" }; echo done-scroll")
        send_enter()
        settled = poll_until(lambda: _safe(lambda: "done-scroll" in (client.read_text(surface_id) or "")), 25.0)
        check("long output settles in the viewport", bool(settled), "done-scroll never appeared")
        wheel(cx, cy, 3)
        stage = poll_until(
            lambda: _safe(lambda: "done-scroll" not in (client.read_text(surface_id) or "")), 10.0
        )
        check("wheel up leaves the bottom of the scrollback", bool(stage), "viewport did not move")
        wheel(cx, cy, -3)
        returned = poll_until(
            lambda: _safe(lambda: "done-scroll" in (client.read_text(surface_id) or "")), 10.0
        )
        check("wheel down returns to the live tail", bool(returned), "viewport did not return")

        # Rapid resize: alternating SetWindowPos with no settling; the app
        # must stay coherent and keep serving the pipe.
        sizes = [(760, 500), (1020, 680), (880, 560), (1100, 720)]
        for round_no in range(24):
            width, height = sizes[round_no % len(sizes)]
            user32.MoveWindow(hwnd, 60, 40, width, height, True)
            time.sleep(0.025)
        user32.MoveWindow(hwnd, 60, 40, 900, 560, True)
        surfaced.refresh()
        alive = poll_until(lambda: _safe(client.ping), 10.0) is True
        text_alive = _safe(lambda: "done-scroll" in (client.read_text(surface_id) or "")) is not None
        check("resize hammer leaves the app serving", alive and text_alive, f"alive={alive} text={text_alive}")

        # Grateful exit: WM_CLOSE runs the save-on-exit path.
        user32.PostMessageW(hwnd, WM_CLOSE, 0, 0)
        exited = True
        try:
            code = process.wait(timeout=20)
        except subprocess.TimeoutExpired:
            exited = False
            code = None
        check("wm_close exits cleanly", exited and code == 0, f"exited={exited} code={code}")
        check("session file written on exit", (fixture / "session.json").exists())

    finally:
        if process.poll() is None:
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
        app_log.close()
        if FAIL:
            try:
                tail = (fixture / "app-stderr.log").read_text(errors="replace")[-2000:]
                print("---- app stderr tail ----")
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
