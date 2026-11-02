"""Round 9 live battery: sidebar depth (scroll/overflow, drag-reorder, inline rename).

Owner round 9 (2026-10-27). What cargo tests cannot prove - that a real
person's gestures drive the sidebar: `SendInput` wheel over the strip,
press-run-release drags onto insertion slots, double-click into the inline
rename editor, and a full WM_CLOSE / relaunch round trip through the
session store. Same no-deps assert-script style as `test_terminal_ux_m43.py`.

Checks:
  - 12 workspaces seeded over a 400pt-tall aside (only ~10 rows fit:
    a real overflow, the state the strip could not had before round 9)
  - wheel down over the sidebar scrolls (rows leave their top slots, the
    bytes of the strip band change); wheel up restores the baseline bytes
  - wheel over the TERMINAL never scrolls the sidebar band (no event steal)
  - drag row 1 onto row 3's slot at scroll 0 lands the exact upstream
    planner outcome (workspace 1 -> index 3) in `workspace.list` order
  - a drag while scrolled still commits and never moves the active
    selection off its workspace (active follows by id)
  - double-click opens the rename editor; Ctrl+A + typing + Enter commits,
    and `workspace.list` reports the custom title
  - double-click, Escape: title unchanged (cancel never writes)
  - WM_CLOSE saves; the session file carries `customTitle` for the renamed
    workspace; a relaunch restores the same ORDER and the custom title

Hermetic: unique pipe, temp session file, pinned
`powershell.exe -NoLogo -NoProfile`. Exit 0 only when every check PASSed.

Divergences recorded at the commit site (not asserted here): no floating
drag image (dimmed row + 2pt indicator), one-press Escape cancel, solid-fill
edge fades instead of the upstream black scrim.
"""

from __future__ import annotations

import argparse
import ctypes
import ctypes.wintypes as wt
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

PASS: list[str] = []
FAIL: list[str] = []

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


# --------------------------- Win32 input layer --------------------------

user32 = ctypes.WinDLL("user32", use_last_error=True)
shcore = ctypes.WinDLL("shcore")

try:
    shcore.SetProcessDpiAwareness(2)
except OSError:
    pass

user32.SetForegroundWindow.argtypes = [wt.HWND]
user32.SetForegroundWindow.restype = wt.BOOL
user32.GetForegroundWindow.restype = wt.HWND
user32.PostMessageW.argtypes = [wt.HWND, wt.UINT, wt.WPARAM, wt.LPARAM]
user32.MoveWindow.argtypes = [wt.HWND, ctypes.c_int, ctypes.c_int, ctypes.c_int, ctypes.c_int, wt.BOOL]
user32.GetClientRect.argtypes = [wt.HWND, ctypes.POINTER(wt.RECT)]
user32.ClientToScreen.argtypes = [wt.HWND, ctypes.POINTER(wt.POINT)]
user32.GetWindowRect.argtypes = [wt.HWND, ctypes.POINTER(wt.RECT)]
user32.GetWindowThreadProcessId.argtypes = [wt.HWND, ctypes.POINTER(wt.DWORD)]
user32.GetDpiForWindow.argtypes = [wt.HWND]
user32.GetDpiForWindow.restype = ctypes.c_uint


class KEYBDINPUT(ctypes.Structure):
    _fields_ = [
        ("wVk", wt.WORD),
        ("wScan", wt.WORD),
        ("dwFlags", wt.DWORD),
        ("time", wt.DWORD),
        ("dwExtraInfo", ctypes.POINTER(wt.ULONG)),
    ]


class MOUSEINPUT(ctypes.Structure):
    # SendInput's MOUSEINPUT order: dx, dy, mouseData, dwFlags, time,
    # dwExtraInfo (the m43 battery's verified layout).
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
WM_INPUTLANGCHANGEREQUEST = 0x0050
VK_RETURN, VK_A, VK_CONTROL, VK_ESCAPE, VK_MENU = 0x0D, 0x41, 0x11, 0x1B, 0x12


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


def key_unicode(char: str) -> tuple[INPUT, INPUT]:
    scan = ord(char)
    return (
        INPUT(
            type=INPUT_KEYBOARD,
            ki=KEYBDINPUT(wVk=0, wScan=scan, dwFlags=KEYEVENTF_UNICODE, time=0, dwExtraInfo=None),
        ),
        INPUT(
            type=INPUT_KEYBOARD,
            ki=KEYBDINPUT(wVk=0, wScan=scan, dwFlags=KEYEVENTF_UNICODE | KEYEVENTF_KEYUP, time=0, dwExtraInfo=None),
        ),
    )


def type_text(text: str) -> None:
    for char in text:
        _send(*key_unicode(char))
        time.sleep(0.012)


def press(vk: int, ctrl: bool = False) -> None:
    mods = [VK_CONTROL] if ctrl else []
    _send(*[key(m) for m in mods])
    _send(key(vk), key(vk, up=True))
    _send(*[key(m, up=True) for m in mods])


def send_enter() -> None:
    press(VK_RETURN)


def set_cursor(x: int, y: int) -> None:
    if not user32.SetCursorPos(int(x), int(y)):
        raise OSError(f"SetCursorPos failed ({ctypes.get_last_error()})")


def click(x: int, y: int) -> None:
    set_cursor(x, y)
    time.sleep(0.05)
    _send(INPUT(
        type=INPUT_MOUSE,
        mi=MOUSEINPUT(dx=0, dy=0, mouseData=0, dwFlags=MOUSEEVENTF_LEFTDOWN, time=0, dwExtraInfo=None),
    ))
    time.sleep(0.08)
    _send(INPUT(
        type=INPUT_MOUSE,
        mi=MOUSEINPUT(dx=0, dy=0, mouseData=0, dwFlags=MOUSEEVENTF_LEFTUP, time=0, dwExtraInfo=None),
    ))
    time.sleep(0.08)


def double_click(x: int, y: int) -> None:
    click(x, y)
    click(x, y)


def sidebar_pointer(x: int, y: int) -> INPUT:
    screen_w, screen_h = user32.GetSystemMetrics(0), user32.GetSystemMetrics(1)
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


def drag(x1: int, y1: int, x2: int, y2: int) -> None:
    """Press, move (only after the click-distance is cleared), release.

    egui resolves click-vs-drag after >6pt of movement, a >0.8s hold, or
    leaving the row; the press frame stays a pending click, so the events
    settle a beat before moving (mirrors the m43 drag dwell evidence).
    """
    set_cursor(x1, y1)
    time.sleep(0.05)
    _send(sidebar_pointer(x1, y1), button(MOUSEEVENTF_LEFTDOWN))
    time.sleep(0.5)
    steps = 8
    for step in range(1, steps + 1):
        _send(
            sidebar_pointer(
                round(x1 + (x2 - x1) * step / steps),
                round(y1 + (y2 - y1) * step / steps),
            )
        )
        time.sleep(0.2)
    time.sleep(0.4)
    _send(button(MOUSEEVENTF_LEFTUP))
    time.sleep(0.5)


def wheel(x: int, y: int, ticks: int) -> None:
    """One SendInput per tick (each 120 delta) with a beat between, so
    egui's scroll smoothing forwards the events in order instead of
    coalescing them into one burst."""
    set_cursor(x, y)
    time.sleep(0.05)
    for _ in range(abs(ticks)):
        _send(INPUT(
            type=INPUT_MOUSE,
            mi=MOUSEINPUT(
                dx=0,
                dy=0,
                mouseData=(1 if ticks > 0 else -1) * WHEEL_DELTA,
                dwFlags=MOUSEEVENTF_WHEEL,
                time=0,
                dwExtraInfo=None,
            ),
        ))
        time.sleep(0.06)
    time.sleep(0.1)


def settle(seconds: float = 2.0) -> None:
    """Lets egui's smooth-scroll decay and any repaint cadence drain."""
    time.sleep(seconds)


def foreground_window(hwnd: int) -> bool:
    _send(key(VK_MENU))
    ok = bool(user32.SetForegroundWindow(hwnd))
    _send(key(VK_MENU, up=True))
    return ok


def pin_us_layout(hwnd: int) -> None:
    handle = user32.LoadKeyboardLayoutW("00000409", 1)
    user32.PostMessageW(hwnd, WM_INPUTLANGCHANGEREQUEST, 0, handle)
    time.sleep(0.4)


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
        return True

    user32.EnumWindows(on_window, 0)
    return found[0] if found else None


# --------------------------- capture (PrintWindow) ----------------------

GDI32 = ctypes.WinDLL("gdi32")


class BITMAPINFOHEADER(ctypes.Structure):
    _fields_ = [
        ("biSize", wt.DWORD), ("biWidth", wt.LONG), ("biHeight", wt.LONG),
        ("biPlanes", wt.WORD), ("biBitCount", wt.WORD), ("biCompression", wt.DWORD),
        ("biSizeImage", wt.DWORD), ("biXPelsPerMeter", wt.LONG), ("biYPelsPerMeter", wt.LONG),
        ("biClrUsed", wt.DWORD), ("biClrImportant", wt.DWORD),
    ]


class RGBQUAD(ctypes.Structure):
    _fields_ = [("rgbBlue", ctypes.c_ubyte), ("rgbGreen", ctypes.c_ubyte), ("rgbRed", ctypes.c_ubyte), ("rgbReserved", ctypes.c_ubyte)]


class BITMAPINFO(ctypes.Structure):
    _fields_ = [("bmiHeader", BITMAPINFOHEADER), ("bmiColors", RGBQUAD * 1)]


def capture(hwnd: int):
    """Client-area capture; returns (w, h, BGRX buffer accessor, dpi)."""
    PW_RENDERFULLCONTENT = 0x00000002
    rect = wt.RECT()
    user32.GetClientRect(hwnd, ctypes.byref(rect))
    w, h = rect.right, rect.bottom
    hdc = user32.GetDC(hwnd)
    mem = GDI32.CreateCompatibleDC(hdc)
    bmi = BITMAPINFO()
    bmi.bmiHeader.biSize = ctypes.sizeof(BITMAPINFOHEADER)
    bmi.bmiHeader.biWidth = w
    bmi.bmiHeader.biHeight = -h
    bmi.bmiHeader.biPlanes = 1
    bmi.bmiHeader.biBitCount = 32
    bmi.bmiHeader.biCompression = 0
    bmi.bmiHeader.biSizeImage = w * h * 4
    bmp = GDI32.CreateCompatibleBitmap(hdc, w, h)
    GDI32.SelectObject(mem, bmp)
    # Client-only + DWM-composed content: a plain PrintWindow paints the
    # whole window (caption included) into the client-sized bitmap and
    # every row lands caption-th pixels too low.
    PW_CLIENTONLY = 0x1
    user32.PrintWindow(hwnd, mem, PW_CLIENTONLY | PW_RENDERFULLCONTENT)
    GDI32.GetDIBits(mem, bmp, 0, h, None, ctypes.byref(bmi), 0)
    buf = (ctypes.c_ubyte * (w * h * 4))()
    GDI32.GetDIBits(mem, bmp, 0, h, buf, ctypes.byref(bmi), 0)
    GDI32.DeleteObject(bmp)
    GDI32.DeleteDC(mem)
    user32.ReleaseDC(hwnd, hdc)
    dpi = user32.GetDpiForWindow(hwnd) or 96

    def pixel(x: int, y: int):
        i = (y * w + x) * 4
        return (buf[i + 2], buf[i + 1], buf[i])  # RGB

    return w, h, pixel, dpi


def band_bytes(surface, x0_pt: float, y0_pt: float, x1_pt: float, y1_pt: float) -> list:
    _, _, pixel, _ = capture(surface.hwnd)
    s = surface.scale
    out = []
    for y in range(int(y0_pt * s), min(int(y1_pt * s), surface.h)):
        for x in range(int(x0_pt * s), min(int(x1_pt * s), surface.w)):
            out.append(pixel(x, y))
    return out


def band_diff(a: list, b: list) -> int:
    return sum(1 for u, v in zip(a, b) if u != v)


CHIP = (0x1D, 0x1F, 0x26)   # the ACTIVE row's chip fill


class ClientSurface:
    """Positions the app's client area and computes sidebar geometry."""

    # Logical-point geometry mirrored from draw_sidebar/ClientSurface:
    # the aside starts at the title strip's 32pt bottom; the plus row
    # spans top+8..top+34 and content starts +6 below it; rows advance
    # 32pt (30pt row + 2 gap). Row i's band: 72+i*32 .. +30; its safe
    # press point is the label area left half (the close X sits right).
    def __init__(self, hwnd: int, width: int = 640, height: int = 400):
        self.hwnd = hwnd
        user32.MoveWindow(hwnd, 60, 40, width, height, True)
        time.sleep(0.4)
        self.refresh()

    def refresh(self) -> None:
        rect = wt.RECT()
        user32.GetClientRect(self.hwnd, ctypes.byref(rect))
        point = wt.POINT(0, 0)
        ctypes.windll.user32.ClientToScreen(self.hwnd, ctypes.byref(point))
        self.x, self.y = point.x, point.y
        self.w, self.h = rect.right, rect.bottom
        dpi = user32.GetDpiForWindow(self.hwnd) or 96
        self.scale = dpi / 96.0
        self.side = round(256 * self.scale)

    def row_press_pt(self, row: int) -> tuple[float, float]:
        return (30.0, 72.0 + row * 32.0 + 15.0)

    def screen_of(self, x_pt: float, y_pt: float) -> tuple[int, int]:
        return (self.x + round(x_pt * self.scale), self.y + round(y_pt * self.scale))

    def main_pane_pt(self) -> tuple[float, float]:
        return (400.0, 240.0)


def order_of(client) -> tuple[str, ...]:
    return tuple(row["id"] for row in client.list_workspaces())


def titles_of(client) -> tuple[str, ...]:
    return tuple((row.get("title") or "") for row in client.list_workspaces())


def smoke() -> int:
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

    pipe = rf"\\.\pipe\cmux-r9-sidebar-{os.getpid()}-{random.randint(1000, 9999)}"
    fixture = Path(tempfile.mkdtemp(prefix="cmux-r9-sidebar-"))
    config = (
        '{\n  "terminal": { "shell": { "program": "powershell.exe", '
        '"args": ["-NoLogo", "-NoProfile"] } }\n}\n'
    )
    (fixture / "cmux.json").write_text(config, encoding="utf-8")
    env = dict(os.environ)
    env["CMUX_SOCKET_PATH"] = pipe
    env["CMUX_SESSION_FILE"] = str(fixture / "session.json")
    env["CMUX_CONFIG_FILE"] = str(fixture / "cmux.json")
    for extra in ("CMUX_SHELL_AUTO_EXIT_MS", "CMUX_CLAUDE_HOOKS_DISABLED",
                  "CMUX_PASSWORD_FILE", "CMUX_SOCKET_PASSWORD", "CMUX_IPC_DISABLED"):
        env.pop(extra, None)

    app_log = open(fixture / "app-stderr.log", "ab")

    def launch(name: str) -> subprocess.Popen:
        process = subprocess.Popen(
            [str(shell_bin)],
            cwd=str(fixture),
            env=env,
            stdout=subprocess.DEVNULL,
            stderr=app_log,
        )
        print(f"launched {name} pid={process.pid} pipe={pipe}")
        return process

    process = launch("leg-1")
    client = None
    try:
        client = harness.cmux(socket_path=pipe, connect_timeout_s=30.0, timeout_s=5.0)
        check("ping answers pong (leg 1)", poll_until(lambda: _safe(client.ping), 30.0) is True)

        hwnd = poll_until(lambda: main_window_for(process.pid), 20.0)
        surface = ClientSurface(hwnd) if hwnd else None
        check("main window found by pid", surface is not None)
        if surface is None:
            raise SystemExit(1)
        check(
            "window foregrounded",
            poll_until(lambda: foreground_window(hwnd) and user32.GetForegroundWindow() == hwnd, 10.0)
            is True,
        )
        pin_us_layout(hwnd)

        # Seed the overflow: 12 workspaces over a 400pt-tall aside keeps
        # rows 10..11 (and the services segment) below the fold.
        seeded = poll_until(lambda: _safe(lambda: len(client.list_workspaces())), 15.0)
        check("one workspace at launch", seeded == 1, f"count={seeded}")
        for _ in range(11):
            _safe(lambda: client.new_workspace())
            time.sleep(0.15)
        count = poll_until(lambda: _safe(lambda: len(client.list_workspaces()) == 12), 30.0)
        check("12 workspaces seeded", bool(count))
        order0 = order_of(client)
        check("seed order is mint order", order0[0] == "w1" and order0[-1] == "w12", f"order0={order0}")

        # ------- Phase B: wheel over the strip -------
        # Pinned-state architecture: the seeding's reveal-on-select left
        # the strip wherever the last IPC landed, so the phase pins its
        # own baseline — IPC select of row 0 reveals it at the top edge
        # and the app's reveal math pins the scroll to exactly 0
        # (row 0's top is content y 0).
        band_pts = (0.0, 40.0, 64.0, 230.0)
        _safe(lambda: client.select_workspace(0))
        check(
            "phase B pinned the active row to row 0",
            poll_until(lambda: _safe(client.current_workspace) == order0[0], 10.0) is True,
            f"active={_safe(client.current_workspace)} expected={order0[0]}",
        )
        settle(2.0)

        # The active row's chip fill #1d1f26 paints on row 0's lower
        # half — below the 16pt top fade, left of the label ink (x 10pt)
        # — so three client pixels there must carry the chip color.
        _, _, pixel, _ = capture(hwnd)
        chip_hits = sum(
            1
            for dy_pt in (92.0, 95.0, 97.0)
            if all(
                abs(pixel(round(10.0 * surface.scale), round(dy_pt * surface.scale))[i] - c) <= 3
                for i, c in enumerate(CHIP)
            )
        )
        check("row chip fill paints at row 0", chip_hits >= 2, f"chip_hits={chip_hits}/3")

        # Wheel round trip from the pinned baseline. Every comparison
        # sample is taken with the cursor PARKED off the strip: a
        # hovered inactive row paints a soft hairline fill (the row
        # fill branch in draw_sidebar), which would poison the byte
        # comparison. wheel() moves the cursor into the strip; park it
        # again before each capture.
        tpx, tpy = surface.screen_of(*surface.main_pane_pt())
        set_cursor(tpx, tpy)
        settle(0.5)
        base_band = band_bytes(surface, *band_pts)
        check("baseline band captured non-empty", len(base_band) > 1000, f"len={len(base_band)}")
        wx, wy = surface.screen_of(30.0, 120.0)
        wheel(wx, wy, -2)
        settle(1.5)
        set_cursor(tpx, tpy)
        settle(0.5)
        scrolled_band = band_bytes(surface, *band_pts)
        moved = band_diff(base_band, scrolled_band)
        check("wheel down scrolls the strip", moved > 300, f"diff={moved}")
        wheel(wx, wy, 6)
        settle(2.0)
        set_cursor(tpx, tpy)
        settle(0.6)
        # The wheel-up's smoothed delta drains over a couple of seconds
        # (the app's own debug log shows ~40 frames), so poll: resample
        # until the strip matches the baseline or the retries exhaust.
        back = band_diff(base_band, band_bytes(surface, *band_pts))
        for _ in range(4):
            if back < 150:
                break
            settle(1.2)
            back = min(back, band_diff(base_band, band_bytes(surface, *band_pts)))
        check("wheel up returns to the baseline", back < 150, f"diff={back}")

        # No-steal at the pinned baseline: the pointer is already parked
        # on the terminal lane, whose hit region excludes the aside.
        wheel(tpx, tpy, 2)
        settle()
        set_cursor(tpx, tpy)
        settle(0.5)
        after_terminal_band = band_bytes(surface, *band_pts)
        steal = band_diff(base_band, after_terminal_band)
        check("terminal wheel never scrolls the sidebar", steal < 120, f"diff={steal}")

        # ------- Phase C: exact drag at scroll 0 -------
        # Re-pin (idempotent): selecting row 0 keeps the reveal at 0.
        _safe(lambda: client.select_workspace(0))
        check(
            "phase C precondition: active pinned to row 0",
            poll_until(lambda: _safe(client.current_workspace) == order0[0], 10.0) is True,
            f"active={_safe(client.current_workspace)} expected={order0[0]}",
        )
        settle(1.0)
        # Row 1's label center -> row 3's lower half: at scroll 0 the
        # drop pointer's content y is (188-72) = 116, past row 3's band
        # midpoint (96+15 = 111), so the planner maps it to slot 4 and
        # reorder_target(1, 4) = 3 — w2 lands after w4 (upstream
        # SidebarDropPlanner arithmetic).
        x1, y1 = surface.screen_of(*surface.row_press_pt(1))
        x2, y2 = surface.screen_of(30.0, 72.0 + 3 * 32.0 + 20.0)
        drag(x1, y1, x2, y2)
        moved_order = poll_until(lambda: _safe(lambda: order_of(client) != order0), 10.0)
        order1 = _safe(lambda: order_of(client))
        check(
            "drag row 1 -> row 3 commits the planner slot",
            moved_order is True and order1 == order0[:1] + order0[2:4] + (order0[1],) + order0[4:],
            f"order1={order1}",
        )
        check("drag keeps ids stable (a move, not a delete)", sorted(order1) == sorted(order0))

        # ------- Phase D: drag while scrolled (active follows by id) -------
        # Pin the scroll with the app's own reveal: IPC-select the LAST
        # row (index 11) reveals it at the window's bottom edge
        # (scroll_to_reveal -> bottom - viewport = 140 at this window).
        order_before = _safe(lambda: order_of(client))
        _safe(lambda: client.select_workspace(11))
        expected_id = order_before[11]
        check(
            "phase D pinned the active row to index 11",
            poll_until(lambda: _safe(client.current_workspace) == expected_id, 10.0) is True,
            f"active={_safe(client.current_workspace)} expected={expected_id}",
        )
        current = _safe(client.current_workspace)
        settle(2.0)
        # Press the second visible row at scroll 140 (content row 5
        # spans screen 92..122) and drop above the content clip in the
        # plus row's screen band: the planner derives the slot purely
        # from the pointer's content y, so the commit still lands.
        # The reorder must NOT move the selection (it follows by id).
        dx1, dy1 = surface.screen_of(30.0, 120.0)
        dx2, dy2 = surface.screen_of(30.0, 45.0)
        drag(dx1, dy1, dx2, dy2)
        changed = poll_until(lambda: _safe(lambda: order_of(client) != order_before), 10.0)
        after_id = _safe(client.current_workspace)
        check(
            "scrolled drag reorders and the selection follows by id",
            changed is True and after_id == current,
            f"before={current} after={after_id} expected={expected_id}",
        )

        # ------- Phase E: inline rename -------
        # Pin the scroll via IPC for deterministic row targeting: select
        # row 0, then rename whatever sits at index 3 in the CURRENT
        # order.
        _safe(lambda: client.select_workspace(0))
        order_e0 = _safe(lambda: order_of(client))
        check(
            "phase E precondition: active pinned to row 0",
            poll_until(lambda: _safe(client.current_workspace) == order_e0[0], 10.0) is True,
            f"active={_safe(client.current_workspace)} expected={order_e0[0]}",
        )
        settle(2.0)
        print(f"  [diag] order entering phase E (sensor-free): {order_e0}")
        order2 = order_e0
        dragged_id = order2[3]  # the row to double-click at scroll 0
        here = order2.index(dragged_id)
        rx, ry = surface.screen_of(*surface.row_press_pt(here))
        double_click(rx, ry)
        time.sleep(0.5)
        poll_until(lambda: _safe(lambda: list(titles_of(client))), 10.0)
        press(VK_A, ctrl=True)  # select all; the typed text replaces it
        time.sleep(0.2)
        rename = "renamed-lane9"
        type_text(rename)
        time.sleep(0.3)
        send_enter()
        time.sleep(0.5)
        titles_after = poll_until(
            lambda: _safe(lambda: rename in list(titles_of(client))), 10.0
        )
        check(
            "double-click rename commits the custom title",
            bool(titles_after),
            f"titles={titles_after}",
        )
        titled_rows = [r for r in client.list_workspaces() if r.get("title") == rename]
        check(
            "the rename lands on the double-clicked workspace",
            len(titled_rows) == 1 and titled_rows[0]["id"] == dragged_id,
            f"rows={[(r['id'], r.get('title')) for r in client.list_workspaces() if 'lane9' in (r.get('title') or '')]}",
        )

        # Escape cancels: open a second editor on ROW 0 (w1), discard it
        # with one Escape press; nothing must be written anywhere (the
        # phase-C commit's rename is expected to still sit on its row).
        row0_id = next(iter(_safe(lambda: order_of(client)) or ["missing"]))
        row0_before = next(r for r in client.list_workspaces() if r["id"] == row0_id).get("title")
        rx, ry = surface.screen_of(*surface.row_press_pt(0))
        double_click(rx, ry)
        time.sleep(0.4)
        press(VK_ESCAPE)
        time.sleep(0.4)
        after_esc = list(client.list_workspaces())
        row0_after = next(r for r in after_esc if r["id"] == row0_id).get("title")
        check(
            "escape cancels without writing",
            row0_after == row0_before
            and sum(1 for r in after_esc if (r.get("title") or "") == rename) == 1,
            f"row0 {row0_before!r}->{row0_after!r} rename_count={sum(1 for r in after_esc if (r.get('title') or '') == rename)}",
        )

        # ------- Phase F: persistence round trip -------
        # The live state is frozen here: after WM_CLOSE this pipe is gone.
        final_order = _safe(lambda: order_of(client))
        final_active = _safe(client.current_workspace)
        wm_close_exit(process, fixture)
        session_text = (fixture / "session.json").read_text(encoding="utf-8")
        snapshot = json.loads(session_text)
        workspaces = snapshot["windows"][0]["tabManager"]["workspaces"]
        check("customTitle key persisted", any(ws.get("customTitle") == rename for ws in workspaces))
        ids_saved = tuple(ws["workspaceId"] for ws in workspaces)
        check("saved order matches the live order", ids_saved == final_order, f"saved={ids_saved} live={final_order}")

        # Relaunch on the SAME session file: restore must reproduce the
        # order and the rename (restored_custom_title seam).
        process = launch("leg-2")
        client = harness.cmux(socket_path=pipe, connect_timeout_s=30.0, timeout_s=5.0)
        check("ping answers pong (leg 2)", poll_until(lambda: _safe(client.ping), 30.0) is True)
        restored = poll_until(lambda: _safe(lambda: len(client.list_workspaces()) == 12), 40.0)
        check("restore rebuilds all 12 workspaces", bool(restored))
        restored_order = poll_until(lambda: _safe(lambda: order_of(client) == final_order), 20.0)
        check("restored order matches", bool(restored_order), f"restored={_safe(lambda: order_of(client))}")
        restored_titles = poll_until(
            lambda: _safe(lambda: rename in list(titles_of(client))), 15.0
        )
        check("restored rename resolves", bool(restored_titles), f"titles={restored_titles}")
        row = [r for r in client.list_workspaces() if (r.get("title") or "") == rename]
        check(
            "restored rename on the right workspace id",
            len(row) == 1 and row[0]["id"] == dragged_id,
        )
        active_restored = _safe(client.current_workspace)
        check(
            "active selection restored from the saved index",
            active_restored == final_active,
            f"active={active_restored} expected={final_active}",
        )
        wm_close_exit(process, fixture)

    finally:
        if process.poll() is None:
            hwnd = process_window(process.pid)
            if hwnd:
                user32.PostMessageW(hwnd, WM_CLOSE, 0, 0)
            time.sleep(1.0)
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
        if client is not None:
            try:
                client.close()
            except Exception:
                pass
        if FAIL:
            print(f"fixture kept (failures present): {fixture}")
        else:
            shutil.rmtree(fixture, ignore_errors=True)

    print(f"\nvisual={len(PASS)} pass / {len(FAIL)} fail")
    for name in FAIL:
        print(f"  FAILED: {name}")
    return 0 if not FAIL else 1


def wm_close_exit(process: subprocess.Popen, fixture: Path) -> None:
    """Posts WM_CLOSE to the app's own window and awaits the exit path."""
    hwnd = poll_until(lambda: main_window_for(process.pid), 10.0)
    check("wm_close window found", hwnd is not None)
    if hwnd:
        user32.PostMessageW(hwnd, WM_CLOSE, 0, 0)
    exited = True
    try:
        code = process.wait(timeout=25)
    except subprocess.TimeoutExpired:
        exited = False
        code = None
    check("wm_close exits cleanly", exited and code == 0, f"exited={exited} code={code}")
    check("session file written on exit", (fixture / "session.json").exists())


def process_window(pid: int) -> int | None:
    return main_window_for(pid)


if __name__ == "__main__":
    sys.exit(smoke())
