# Lane 3 sub-header ABSENCE probe (battery member).
# Owner round 4 (2026-10-26) removed the design lane 3 sub-header band
# (node 1:5 `Minimal Top Sub-Header`: git chip, staged-diff chip, agent
# chip, split-shortcut keycap pills, split tray). This probe asserts the
# removal holds and that what replaced the band is the pane surface:
# visual:    no band fill/pills/tray buttons/agent violet in y 32..64pt,
#            the pane surface (viewport fill) owns the old band area,
#            the focused pane's accent ring rides the pane top edge now
#            at the title strip's bottom, command bar intact.
# behavior:  palette opens from the command bar click and Ctrl+K (the
#            split tray is gone; `splitRight` keymap splits the pane),
#            arrows move the selection, Esc closes, Enter runs the
#            selected config command into the focused terminal (read
#            back over the automation CLI).
# Real clicks + real keys against the real window; env-isolated app.
import ctypes
import ctypes.wintypes as wt
import json
import os
import re
import subprocess
import sys
import tempfile
import time

user32 = ctypes.windll.user32
gdi32 = ctypes.windll.gdi32

# Per-monitor-aware v2 for this whole probe thread (lane-2 gotcha).
user32.SetThreadDpiAwarenessContext(ctypes.c_void_p(-4))

# Explicit GDI signatures (GetDIBits silently returns 0 lines otherwise).
user32.PrintWindow.restype = ctypes.c_bool
user32.PrintWindow.argtypes = [ctypes.c_void_p, ctypes.c_void_p, ctypes.c_uint]
gdi32.GetDIBits.restype = ctypes.c_int
gdi32.GetDIBits.argtypes = [
    ctypes.c_void_p, ctypes.c_void_p, ctypes.c_uint, ctypes.c_uint,
    ctypes.c_void_p, ctypes.c_void_p, ctypes.c_uint,
]
gdi32.SelectObject.restype = ctypes.c_void_p
gdi32.SelectObject.argtypes = [ctypes.c_void_p, ctypes.c_void_p]
gdi32.CreateCompatibleDC.restype = ctypes.c_void_p
gdi32.CreateCompatibleBitmap.restype = ctypes.c_void_p
gdi32.CreateCompatibleBitmap.argtypes = [ctypes.c_void_p, ctypes.c_int, ctypes.c_int]
gdi32.DeleteObject.argtypes = [ctypes.c_void_p]
gdi32.DeleteDC.argtypes = [ctypes.c_void_p]

PORT = r"D:\workspace\Projects\cmux-clone\port"
BIN = PORT + r"\target\debug\cmux-shell.exe"
CLI = PORT + r"\target\debug\cmux.exe"

passed = 0
failed = 0


def check(name, cond, detail=""):
    global passed, failed
    if cond:
        passed += 1
        print(f"PASS {name}")
    else:
        failed += 1
        print(f"FAIL {name} {detail}")


def close_to(c, ref, tol):
    return all(abs(cs - rs) <= tol for cs, rs in zip(c, ref))


CONFIG = """{
  // JSONC on read: this comment is tolerated like upstream.
  "commands": [
    { "name": "probe-emit", "description": "echo the palette marker",
      "command": "echo probe-palette-hit-1877" }
  ]
}
"""

MARKER = "probe-palette-hit-1877"


def launch(fixture, env):
    log = open(os.path.join(fixture, "stderr.log"), "wb")
    proc = subprocess.Popen([BIN], cwd=fixture, env=env, stdout=log, stderr=log)
    deadline = time.time() + 30
    while time.time() < deadline:
        if subprocess.run([CLI, "ping"], env=env, capture_output=True).returncode == 0:
            return proc
        if proc.poll() is not None:
            log.flush()
            with open(os.path.join(fixture, "stderr.log"), "rb") as f:
                print(f.read()[:2000].decode("utf-8", "replace"))
            raise SystemExit(1)
        time.sleep(0.25)
    raise SystemExit("shell never answered ping")


class _EnumCtx(ctypes.Structure):
    _fields_ = [
        ("pid", ctypes.c_ulong),
        ("HWND", ctypes.c_void_p),
        ("area", ctypes.c_long),
    ]


@ctypes.WINFUNCTYPE(ctypes.c_bool, ctypes.c_void_p, ctypes.c_void_p)
def _enum_proc(hwnd, lparam):
    ctx = _EnumCtx.from_address(lparam)
    pid = wt.DWORD()
    user32.GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
    if pid.value == ctx.pid and user32.IsWindowVisible(hwnd) and \
            user32.GetWindow(hwnd, 4) == 0:
        cls = ctypes.create_unicode_buffer(64)
        user32.GetClassNameW(hwnd, cls, 64)
        rc = wt.RECT()
        user32.GetWindowRect(hwnd, ctypes.byref(rc))
        area = rc.right - rc.left
        if cls.value == "Window Class" and area >= 300:
            if area > ctx.area:
                ctx.area = area
                ctx.HWND = hwnd
    return True


def window_of(pid):
    for _ in range(60):
        ctx = _EnumCtx(pid, ctypes.c_void_p(0), 0)
        user32.EnumWindows(_enum_proc, ctypes.byref(ctx))
        if ctx.HWND:
            return ctx.HWND
        time.sleep(0.5)
    return None


class BMIH(ctypes.Structure):
    _fields_ = [
        ("biSize", ctypes.c_ulong), ("biWidth", ctypes.c_long),
        ("biHeight", ctypes.c_long), ("biPlanes", ctypes.c_ushort),
        ("biBitCount", ctypes.c_ushort), ("biCompression", ctypes.c_ulong),
        ("biSizeImage", ctypes.c_ulong), ("biXPelsPerMeter", ctypes.c_long),
        ("biYPelsPerMeter", ctypes.c_long), ("biClrUsed", ctypes.c_ulong),
        ("biClrImportant", ctypes.c_ulong),
    ]


def capture(hwnd):
    """Window-relative (b, g, r) sampler plus the client-area metrics."""
    rc = wt.RECT()
    user32.GetWindowRect(hwnd, ctypes.byref(rc))
    W, H = rc.right - rc.left, rc.bottom - rc.top
    crc = wt.RECT()
    user32.GetClientRect(hwnd, ctypes.byref(crc))
    cwidth, cheight = crc.right - crc.left, crc.bottom - crc.top
    # client (0,0) expressed in window-relative pixels.
    pt = wt.POINT(0, 0)
    user32.ClientToScreen(hwnd, ctypes.byref(pt))
    ox, oy = pt.x - rc.left, pt.y - rc.top
    dpi = user32.GetDpiForWindow(hwnd)
    hdc = user32.GetWindowDC(hwnd)
    mem = gdi32.CreateCompatibleDC(hdc)
    bmp = gdi32.CreateCompatibleBitmap(hdc, W, H)
    old = gdi32.SelectObject(mem, bmp)
    printed = user32.PrintWindow(hwnd, mem, 2)  # PW_RENDERFULLCONTENT
    gdi32.SelectObject(mem, old)  # deselect: GetDIBits requires it
    bmi = BMIH(ctypes.sizeof(BMIH), W, -H, 1, 32, 0, 0)
    buf = ctypes.create_string_buffer(W * H * 4)
    got = gdi32.GetDIBits(mem, bmp, 0, H, buf, ctypes.byref(bmi), 0)
    gdi32.DeleteObject(bmp)
    gdi32.DeleteDC(mem)
    user32.ReleaseDC(hwnd, hdc)
    raw = buf.raw

    def at(x, y):
        # client-relative; returns window-relative pixels (b, g, r)
        wx, wy = x + ox, y + oy
        if x < 0 or y < 0 or wx < 0 or wy < 0 or wx >= W or wy >= H:
            return None
        i = (wy * W + wx) * 4
        return (raw[i], raw[i + 1], raw[i + 2])

    return dict(W=W, H=H, cwidth=cwidth, cheight=cheight, dpi=dpi,
                ox=ox, oy=oy, at=at, printed=printed, got=got)


class KEYBDINPUT(ctypes.Structure):
    _fields_ = [
        ("wVk", wt.WORD), ("wScan", wt.WORD), ("dwFlags", wt.DWORD),
        ("time", wt.DWORD), ("dwExtraInfo", ctypes.POINTER(wt.ULONG)),
    ]


class MOUSEINPUT(ctypes.Structure):
    _fields_ = [
        ("dx", wt.LONG), ("dy", wt.LONG), ("mouseData", wt.DWORD),
        ("dwFlags", wt.DWORD), ("time", wt.DWORD),
        ("dwExtraInfo", ctypes.POINTER(wt.ULONG)),
    ]


class _INPUTUNION(ctypes.Union):
    _fields_ = [("ki", KEYBDINPUT), ("mi", MOUSEINPUT)]


class INPUT(ctypes.Structure):
    _anonymous_ = ("u",)
    _fields_ = [("type", wt.DWORD), ("u", _INPUTUNION)]


INPUT_KEYBOARD = 1
KEYEVENTF_KEYUP = 0x0002
MOUSEEVENTF_LEFTDOWN = 0x0002
MOUSEEVENTF_LEFTUP = 0x0004
VK_CONTROL = 0x11
VK_SHIFT = 0x10
VK_ESCAPE = 0x1B
VK_RETURN = 0x0D
VK_DOWN = 0x28
VK_UP = 0x26
VK_K = 0x4B
VK_R = 0x52

screen_origin = [0, 0]


def _send(*inputs):
    array = (INPUT * len(inputs))(*inputs)
    sent = user32.SendInput(len(inputs), array, ctypes.sizeof(INPUT))
    if sent != len(inputs):
        raise OSError(f"SendInput accepted {sent}/{len(inputs)} events")


def key_event(vk, up=False):
    return INPUT(
        type=INPUT_KEYBOARD,
        ki=KEYBDINPUT(wVk=vk, wScan=0, dwFlags=KEYEVENTF_KEYUP if up else 0,
                      time=0, dwExtraInfo=None),
    )


def pin_foreground(hwnd):
    """Foreground pin (m34 recipe, verbatim from test_titlebar_2b).

    A background launch may surface UNDER another window (observed
    2026-10-26: the runner's own WindowsTerminal sat over the probe
    instance at the click point — WindowFromPoint hit the terminal, the
    bar click no-oped twice, deterministic), and SetForegroundWindow
    sticks from a non-foreground caller only via the ALT trick; retry
    until it takes."""
    user32.keybd_event(0x12, 0, 0, 0)  # ALT down
    user32.SetForegroundWindow(hwnd)
    time.sleep(0.08)
    user32.keybd_event(0x12, 0, 2, 0)  # ALT up
    for _ in range(20):
        if user32.GetForegroundWindow() == hwnd:
            return True
        time.sleep(0.1)
        user32.keybd_event(0x12, 0, 0, 0)
        user32.SetForegroundWindow(hwnd)
        time.sleep(0.08)
        user32.keybd_event(0x12, 0, 2, 0)
    return user32.GetForegroundWindow() == hwnd


def set_foreground(hwnd):
    """Verified foreground: the m34 pin, then an honest check so a
    stuck pin surfaces as a failed check instead of a silent no-op."""
    stuck = pin_foreground(hwnd)
    check("foreground pin", stuck,
          f"fg={user32.GetForegroundWindow()} hwnd={hwnd}")


def press(vk, ctrl=False, shift=False):
    events = []
    if ctrl:
        events.append(key_event(VK_CONTROL))
    if shift:
        events.append(key_event(VK_SHIFT))
    events.append(key_event(vk))
    events.append(key_event(vk, up=True))
    if shift:
        events.append(key_event(VK_SHIFT, up=True))
    if ctrl:
        events.append(key_event(VK_CONTROL, up=True))
    for event in events:
        _send(event)
        time.sleep(0.03)


def click(x, y):
    """Real desktop click at the given CLIENT-relative pixel."""
    user32.SetCursorPos(int(screen_origin[0] + x), int(screen_origin[1] + y))
    time.sleep(0.08)
    _send(INPUT(type=0, u=_INPUTUNION(mi=MOUSEINPUT(
        dx=0, dy=0, mouseData=0, dwFlags=MOUSEEVENTF_LEFTDOWN, time=0,
        dwExtraInfo=None))))
    time.sleep(0.04)
    _send(INPUT(type=0, u=_INPUTUNION(mi=MOUSEINPUT(
        dx=0, dy=0, mouseData=0, dwFlags=MOUSEEVENTF_LEFTUP, time=0,
        dwExtraInfo=None))))


def count_row(surf, y, x0, x1, pred):
    n = 0
    for x in range(x0, x1):
        c = surf["at"](x, y)
        if c is not None and pred(c):
            n += 1
    return n


def count_box(surf, x0, y0, x1, y1, pred):
    n = 0
    for x in range(x0, x1, max(1, surf["dpi"] // 96)):
        for y in range(y0, y1, max(1, surf["dpi"] // 96)):
            c = surf["at"](x, y)
            if c is not None and pred(c):
                n += 1
    return n


def command_bar_pts(w_pts):
    """Mirrors title_command_bar_rect_pts (wndproc/painter agreement)."""
    if w_pts < 800.0:
        return None
    left = max(w_pts * 0.3326, 335.0)
    width = min(420.0, (w_pts - 148.0) - left)
    if width <= 0.0:
        return None
    return left, width


def band_region(surf):
    """client-px bounds of the REMOVED band (between title row bottom and
    the old 64pt line): the region that must now be pane surface."""
    s = surf["dpi"] / 96.0
    strip_h = round(32 * s)
    band_bottom = round(64 * s)
    sidebar_px = round(256 * s)
    return strip_h, band_bottom, sidebar_px


def palette_open_check(surf, label):
    fl = surf["fl"]
    ftop = surf["ftop"]
    s = surf["dpi"] / 96.0
    # Strict: #11131a (26,19,17) vs pane #0c0e14 (20,14,12) differ by 6;
    # tol 4 keeps them apart while allowing AA noise.
    fill = surf["at"](fl + int(30 * s), ftop + int(12 * s))
    check(f"palette frame fill {label}", fill and close_to(fill, (0x1A, 0x13, 0x11), 4),
          f"{fill} at ({fl}, {ftop})")
    ink = count_box(surf, fl, ftop + int(6 * s), fl + int(140 * s),
                    ftop + int(26 * s),
                    lambda c: c[2] > 150 and c[1] > 150 and c[0] > 150)
    check(f"palette query ink {label}", ink >= 3, f"{ink}px")
    sel = surf["at"](fl + int(180 * s), ftop + int(42 * s))
    check(f"palette selected row fill {label}",
          sel and close_to(sel, (0x26, 0x1F, 0x1D), 5), f"{sel}")
    rows_ink = count_box(surf, fl, ftop + int(30 * s), fl + surf["fw"],
                         ftop + int(54 * s),
                         lambda c: c[2] > 150 and c[1] > 140)
    check(f"palette row label ink {label}", rows_ink >= 4, f"{rows_ink}px")


def palette_closed_check(surf, label):
    fl = surf["fl"]
    ftop = surf["ftop"]
    s = surf["dpi"] / 96.0
    fill = surf["at"](fl + int(30 * s), ftop + int(12 * s))
    check(f"palette overlay gone {label}",
          fill and close_to(fill, (0x14, 0x0E, 0x0C), 4), f"{fill}")


def first_surface_id(env):
    out = subprocess.run([CLI, "surface", "list", "--json"],
                         env=env, capture_output=True, text=True)
    # surface.list emits per surface `{"id": "p1", "pane_id": ...}`; the
    # response envelope itself also carries `"id":"cli"`, so match the
    # pane id (never matched by the envelope).
    ids = re.findall(r'"pane_id"\s*:\s*"([^"]+)"', out.stdout)
    return ids[0] if ids else None


def surface_count(env):
    out = subprocess.run([CLI, "surface", "list", "--json"],
                         env=env, capture_output=True, text=True)
    return len(re.findall(r'"pane_id"\s*:', out.stdout))


def dump_region(surf, x0, y0, x1, y1, label, step=None):
    """Diagnostic: quantized color histogram of a client-relative box."""
    from collections import Counter
    counts = Counter()
    for x in range(x0, x1, step or 3):
        for y in range(y0, y1, step or 3):
            c = surf["at"](x, y)
            if c is not None:
                counts[(c[0] // 6, c[1] // 6, c[2] // 6)] += 1
    top = ", ".join(f"{(b*6, g*6, r*6)}:{n}" for (b, g, r), n in counts.most_common(6))
    print(f"  dump[{label}]: {top}", flush=True)


def detect_gutter(surf):
    """A #11131a vertical divider over the pane row right of the sidebar
    (re-anchored round 4: pane rows start at the title bottom, the old
    sub-header band below it gone)."""
    s = surf["dpi"] / 96.0
    y = round(32 * s) + int(40 * s)
    need = 2
    run = 0
    for x in range(int(400 * s), surf["cwidth"] - int(30 * s)):
        c = surf["at"](x, y)
        if c is not None and close_to(c, (0x1A, 0x13, 0x11), 4):
            run += 1
            if run >= need:
                return True
        else:
            run = 0
    return False


def teardown(hwnd, proc, env):
    user32.PostMessageW(hwnd, 0x0010, 0, 0)  # WM_CLOSE
    time.sleep(1.0)
    if proc.poll() is None:
        subprocess.run(["taskkill", "/PID", str(proc.pid), "/T", "/F"],
                       capture_output=True)
    # drain the pipe session file state so reruns stay hermetic
    subprocess.run([CLI, "ping"], env=env, capture_output=True)


mode = sys.argv[1] if len(sys.argv) > 1 else "visual"
fixture = tempfile.mkdtemp(prefix="cmux-lane3-probe-")
env = os.environ.copy()
env["CMUX_SOCKET_PATH"] = rf"\\.\pipe\cmux-lane3-probe-{os.getpid()}"
env["CMUX_SESSION_FILE"] = os.path.join(fixture, "session.json")
env["CMUX_CONFIG_FILE"] = os.path.join(fixture, "config.json5")
env.pop("CMUX_SHELL_AUTO_EXIT_MS", None)
with open(env["CMUX_CONFIG_FILE"], "w", encoding="ascii") as f:
    f.write(CONFIG)

print("step: launch (no git fixture since round 4)", flush=True)
proc = launch(fixture, env)
print("step: launched, ping ok", flush=True)
try:
    hwnd = None
    for _ in range(80):
        hwnd = window_of(proc.pid)
        if hwnd:
            break
        if proc.poll() is not None:
            break
        time.sleep(0.4)
    check("shell window found", bool(hwnd))
    if not hwnd:
        raise SystemExit(1)
    time.sleep(1.5)
    # the client (0,0) in screen pixels, for real clicks
    pt = wt.POINT(0, 0)
    user32.ClientToScreen(hwnd, ctypes.byref(pt))
    screen_origin[0], screen_origin[1] = pt.x, pt.y

    surf = capture(hwnd)
    s = surf["dpi"] / 96.0
    strip_h, band_bottom, sidebar_px = band_region(surf)
    surf["strip_h"] = strip_h
    surf["sub_bottom"] = strip_h  # legacy alias: the band is gone
    check("printwindow ok", bool(surf["printed"]))
    check("dpi lines ok", surf["got"] == surf["H"])
    check("client large enough", surf["cwidth"] >= 900 and surf["cheight"] >= 500,
          f"{surf['cwidth']}x{surf['cheight']} ox={surf['ox']} oy={surf['oy']}")

    w_pts = surf["cwidth"] / s

    if mode == "visual":
        print("step: visual pass", flush=True)
        y_probe = strip_h + int(12 * s)
        # Absence A1: the old band fill (#191b22 sidebar tone) paints
        # nowhere in the former band rows right of the aside. Exact
        # match: the removed surface painted it flat, and no AA edge of
        # the ring/prompt text can reproduce it.
        fill_hits = count_row(surf, y_probe, sidebar_px, surf["cwidth"],
                              lambda c: c == (0x22, 0x1B, 0x19))
        check("sub-header band fill absent", fill_hits == 0, f"{fill_hits}px")

        # Absence A2: no chip/pill fill (#1d1f26) anywhere in the band
        # (exact match — see A1).
        chip_hits = count_box(surf, sidebar_px, strip_h + 2, surf["cwidth"],
                              band_bottom - 2,
                              lambda c: c == (0x26, 0x1F, 0x1D))
        check("chip/pill fill absent in band", chip_hits == 0, f"{chip_hits}px")

        # Absence A3: agent violet (#cebdff composite) nowhere in the
        # former band rows (also the idle-state assertion).
        violet = count_box(surf, 0, strip_h, surf["cwidth"], band_bottom,
                           lambda c: c[0] > 240 and 170 < c[2] < 235 and 160 < c[1] < 215)
        check("agent chip absent while idle", violet == 0, f"{violet}px")

        # Absence A4: no tray-button fill (#11131a) in the band's right
        # half (the split tray itself is gone; exact match — see A1).
        tray = count_box(surf, int(0.78 * surf["cwidth"]), strip_h + 2,
                         surf["cwidth"], band_bottom - 2,
                         lambda c: c == (0x1A, 0x13, 0x11))
        check("split tray fill absent", tray == 0, f"{tray}px")

        # Replacement R1: the pane surface (viewport fill #0c0e14)
        # dominates the former band area right of the aside.
        pane = count_box(surf, sidebar_px, strip_h + 2, surf["cwidth"] - int(8 * s),
                         band_bottom - 2,
                         lambda c: c is not None and close_to(c, (0x14, 0x0E, 0x0C), 4))
        check("pane surface owns the old band", pane >= 500, f"{pane}px")

        # Replacement R2: the focused pane's accent ring (#38BDF8) rides
        # the pane top edge directly under the title strip.
        ring = count_box(surf, sidebar_px + int(60 * s), strip_h,
                         surf["cwidth"] - int(60 * s), strip_h + int(8 * s),
                         lambda c: c[0] > 200 and 120 < c[1] < 220 and c[2] < 120)
        check("focused pane ring at pane top edge", ring >= 20, f"{ring}px")

        bar = command_bar_pts(w_pts)
        check("command bar rule visible", bar is not None, f"{bar}")
        if bar:
            bx = int(bar[0] * s)
            bw = int(bar[1] * s)
            fill = surf["at"](bx + int(60 * s), int(6 * s))
            check("command bar fill #11131a",
                  fill and close_to(fill, (0x1A, 0x13, 0x11), 6), f"{fill}")
            glyph = count_box(
                surf, bx, int(9 * s), bx + int(34 * s), int(24 * s),
                lambda c: c[2] > 150 and c[1] > 150 and c[0] > 150)
            check("command bar glyph ink", glyph >= 3, f"{glyph}px")
            ink = count_box(
                surf, bx + int(40 * s), int(8 * s), bx + bw - int(40 * s),
                int(24 * s),
                lambda c: c[2] > 150 and c[1] > 150 and c[0] > 150)
            check("placeholder ink", ink >= 8, f"{ink}px")
            pills = count_row(surf, int(16 * s), bx, bx + bw,
                              lambda c: close_to(c, (0x26, 0x1F, 0x1D), 5))
            check("Ctrl/K pill fill in bar", pills >= int(14 * s), f"{pills}px")
    else:
        print("step: behavior pass", flush=True)
        surf["fl"] = surf["cwidth"] // 2 - int(210 * s)
        surf["fw"] = int(420 * s)
        surf["ftop"] = round(40 * s)  # title 32pt + 8pt gap (round 4)
        sid = None
        for _ in range(40):
            sid = first_surface_id(env)
            if sid:
                break
            time.sleep(0.25)
        # echo the resolved id so a failure here is diagnosable
        check("automation surface resolved", bool(sid), f"{sid}")

        # B2: clicking the command bar opens the modal palette.
        bar = command_bar_pts(w_pts)
        if bar:
            bx, bw = int(bar[0] * s), int(bar[1] * s)
            set_foreground(hwnd)
            click(bx + bw // 2, int(16 * s))
            time.sleep(0.6)
            surf3 = capture(hwnd)
            surf3["fl"], surf3["ftop"], surf3["fw"] = surf["fl"], surf["ftop"], surf["fw"]
            palette_open_check(surf3, "after bar click")

            # B3: Esc closes it (set_foreground already owns focus).
            press(VK_ESCAPE)
            time.sleep(0.6)
            surf4 = capture(hwnd)
            surf4["fl"], surf4["ftop"] = surf["fl"], surf["ftop"]
            palette_closed_check(surf4, "after esc")

            # B4: Ctrl+K reopens; arrows move the selection; Enter runs
            # the selected config command into the focused terminal.
            set_foreground(hwnd)
            press(VK_K, ctrl=True)
            time.sleep(0.6)
            surf5 = capture(hwnd)
            surf5["fl"], surf5["ftop"], surf5["fw"] = surf["fl"], surf["ftop"], surf["fw"]
            palette_open_check(surf5, "after ctrl+k")

            press(VK_DOWN)
            time.sleep(0.4)
            surf6 = capture(hwnd)
            surf6["ftop"], surf6["fl"] = surf["ftop"], surf["fl"]
            # The chip highlight moved off row 0 (its previous blank-zone
            # sample now reads the frame fill; row 1 itself may carry the
            # jump row's own text at that x).
            moved = surf6["at"](surf6["fl"] + int(180 * s), surf6["ftop"] + int(42 * s))
            check("arrow moves selection off row 1",
                  moved and close_to(moved, (0x1A, 0x13, 0x11), 6), f"{moved}")
            press(VK_UP)
            time.sleep(0.4)
            press(VK_RETURN)
            time.sleep(0.4)
            if sid:
                seen = False
                for _ in range(40):
                    out = subprocess.run([CLI, "read-screen", "--surface", sid],
                                         env=env, capture_output=True, text=True)
                    if MARKER in (out.stdout + out.stderr):
                        seen = True
                        break
                    time.sleep(0.25)
                check("enter runs the selected command", seen)

                # the modal pass must not have leaked typing into the pane:
                # the query text and arrows live only in the palette.
                out = subprocess.run([CLI, "read-screen", "--surface", sid],
                                     env=env, capture_output=True, text=True)
                screen = (out.stdout + out.stderr)
                check("no stray typing beyond the marker line",
                      MARKER in screen and "DOWN" not in screen.upper(),
                      screen[:200])

        # B1 (round 4 form: the tray button is gone; the keymap's default
        # splitRight stroke is now the only surface): ctrl+shift+r splits
        # the focused pane.
        set_foreground(hwnd)
        press(VK_R, ctrl=True, shift=True)
        time.sleep(1.0)
        n = surface_count(env)
        check("split works via the splitRight keymap", n >= 2, f"{n} surface(s)")
        if n >= 2:
            surf2 = capture(hwnd)
            dump_region(surf2, int(400 * s), round(32 * s) + int(40 * s),
                        surf2["cwidth"] - int(30 * s),
                        round(32 * s) + int(42 * s), "gutter-line-40")
            dump_region(surf2, int(300 * s), round(32 * s) + int(80 * s),
                        surf2["cwidth"] - int(30 * s),
                        round(32 * s) + int(82 * s), "gutter-line-80")
            check("gutter appears after keymap split", detect_gutter(surf2))
        else:
            check("gutter appears after keymap split", False, "no split")
finally:
    if hwnd:
        teardown(hwnd, proc, env)
print(f"subheader lane3 probe: {passed} passed, {failed} failed")
if failed:
    raise SystemExit(1)
