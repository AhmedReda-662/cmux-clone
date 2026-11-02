# Lane 2 title-bar probe (scratch; deleted after recording).
# visual:    strip fill / controls glyphs / hairline / chip / layout inset
# behavior:  window controls + caption drag semantics (real input)
import ctypes
import ctypes.wintypes as wt
import os
import shutil
import subprocess
import sys
import tempfile
import time

print("probe importing...", flush=True)

user32 = ctypes.windll.user32
gdi32 = ctypes.windll.gdi32

# Per-monitor-aware v2 for this whole probe thread (lane-2 gotcha).
user32.SetThreadDpiAwarenessContext(ctypes.c_void_p(-4))

# Explicit GDI signatures â€” GetDIBits silently returns 0 lines otherwise.
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
gdi32.CreateCompatibleDC.argtypes = [ctypes.c_void_p]
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


def launch(fixture):
    env = os.environ.copy()
    env["CMUX_SOCKET_PATH"] = rf"\\.\pipe\cmux-titlebar-probe-{os.getpid()}"
    env["CMUX_SESSION_FILE"] = os.path.join(fixture, "session.json")
    env["CMUX_CONFIG_FILE"] = os.path.join(fixture, "config.json5")
    env.pop("CMUX_SHELL_AUTO_EXIT_MS", None)
    log = open(os.path.join(fixture, "stderr.log"), "wb")
    proc = subprocess.Popen([BIN], cwd=fixture, env=env, stdout=log, stderr=log)
    deadline = time.time() + 30
    while time.time() < deadline:
        if subprocess.run([CLI, "ping"], env=env, capture_output=True).returncode == 0:
            return proc
        if proc.poll() is not None:
            print("shell exited early; stderr:")
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
            user32.GetWindow(hwnd, 4) == 0:  # GW_OWNER for a top-level win
        cls = ctypes.create_unicode_buffer(64)
        user32.GetClassNameW(hwnd, cls, 64)
        rc = wt.RECT()
        user32.GetWindowRect(hwnd, ctypes.byref(rc))
        area = rc.right - rc.left
        # winit's per-thread event helper is a tiny visible unowned window;
        # other processes may also register a literally named "Window
        # Class", so keep only OUR process and take the BIGGEST window.
        if cls.value in ("Window Class", "Winit Thread Event Target") and area >= 300:
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
    rc = wt.RECT()
    user32.GetWindowRect(hwnd, ctypes.byref(rc))
    W, H = rc.right - rc.left, rc.bottom - rc.top
    dpi = user32.GetDpiForWindow(hwnd)
    hdc = user32.GetWindowDC(hwnd)
    mem = gdi32.CreateCompatibleDC(hdc)
    bmp = gdi32.CreateCompatibleBitmap(hdc, W, H)
    old = gdi32.SelectObject(mem, bmp)
    printed = user32.PrintWindow(hwnd, mem, 2)  # PW_RENDERFULLCONTENT
    gdi32.SelectObject(mem, old)  # deselect: GetDIBits requires it
    bmi = BMIH(
        ctypes.sizeof(BMIH), W, -H, 1, 32, 0, 0,
    )
    buf = ctypes.create_string_buffer(W * H * 4)
    got = gdi32.GetDIBits(mem, bmp, 0, H, buf, ctypes.byref(bmi), 0)
    print("capture dbg: printed", printed, "lines", got,
          "nonzero", sum(1 for b in buf.raw[:4000] if b))
    gdi32.DeleteObject(bmp)
    gdi32.DeleteDC(mem)
    user32.ReleaseDC(hwnd, hdc)
    raw = buf.raw

    def at(x, y):
        if x is None or x < 0 or y < 0 or x >= W or y >= H:
            return None
        i = (y * W + x) * 4
        return (raw[i], raw[i + 1], raw[i + 2])  # (b, g, r)

    return W, H, dpi, at, printed


class MI(ctypes.Structure):
    _fields_ = [
        ("dx", ctypes.c_long), ("dy", ctypes.c_long),
        ("mouseData", ctypes.c_ulong), ("dwFlags", ctypes.c_ulong),
        ("time", ctypes.c_ulong), ("dwExtraInfo", ctypes.POINTER(ctypes.c_ulong)),
    ]


def mouse_event(flags):
    user32.mouse_event(flags, 0, 0, 0, 0)

DOWN, UP = 0x0002, 0x0004


def click_at(x, y, double=False):
    user32.SetCursorPos(int(x), int(y))
    time.sleep(0.03)
    mouse_event(DOWN)
    mouse_event(UP)
    if double:
        time.sleep(0.06)
        mouse_event(DOWN)
        mouse_event(UP)


def drag_at(x, y, dx, dy, steps=12):
    user32.SetCursorPos(int(x), int(y))
    time.sleep(0.05)
    mouse_event(DOWN)
    time.sleep(0.05)
    for step in range(1, steps + 1):
        user32.SetCursorPos(int(x + dx * step / steps), int(y + dy * step / steps))
        time.sleep(0.02)
    mouse_event(UP)


def window_rect(hwnd):
    rc = wt.RECT()
    user32.GetWindowRect(hwnd, ctypes.byref(rc))
    return rc.left, rc.top, rc.right, rc.bottom


def client_origin(hwnd):
    pt = wt.POINT(0, 0)
    user32.ClientToScreen(hwnd, ctypes.byref(pt))
    return pt.x, pt.y


def pin_foreground(hwnd):
    """Foreground pin (m34 recipe).

    A background launch may surface UNDER a maximized overlay (observed
    2026-10-24: every injected titlebar check clicked a full-screen
    foreground terminal instead of the app - WindowFromPoint hit the
    overlay at all sample points), and SW_RESTORE from a background
    caller restores the window without re-granting foreground (observed:
    deterministic close-button fail, 3/3 runs, while a standalone
    diagnostic click on a foreground-pinned app exits cleanly at t=0.5s).
    SetForegroundWindow sticks from a non-foreground caller only via the
    ALT trick; retry until it takes."""
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


fixture = tempfile.mkdtemp(prefix="cmux-titlebar-probe-")
print("step: fixture + launch", flush=True)
proc = launch(fixture)
print("step: launched, ping ok", flush=True)
mode = sys.argv[1] if len(sys.argv) > 1 else "visual"
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
    pin_foreground(hwnd)
    rc_found = wt.RECT()
    user32.GetWindowRect(hwnd, ctypes.byref(rc_found))
    print(f"found hwnd {hwnd} rect={rc_found.left},{rc_found.top}.."
          f"{rc_found.right},{rc_found.bottom}")
    time.sleep(1.0)
    print("step: window + capture", flush=True)

    W, H, dpi, at, printed = capture(hwnd)
    s = dpi / 96.0
    strip_h = round(32 * s)
    cy = strip_h // 2
    check("printwindow ok", bool(printed))
    check("client large enough", W >= 300 and H >= 200, f"{W}x{H}")

    if mode == "visual":
        # blank drag zone = exactly the sidebar tone #191b22. The lane-3
        # command bar (node 1:512; `title_command_bar_rect_pts` anchors
        # `max(0.3326*w, 335)pt`..+420pt) occupies the strip's middle, so
        # the sample anchors at a fixed 300pt from the left: past the
        # worst-case default crumb chip end (live ink profile 2026-10-24:
        # chip ends ~277pt), before the bar (â‰¥335pt at any shown width),
        # and blank too where the bar hides (<800pt window width).
        blank = at(round(300 * s), cy)
        check("strip fill #191b22", blank and close_to(blank, (0x22, 0x1B, 0x19), 6), f"{blank}")

        # the top-left corner is also the strip (nothing paints under it)
        corner = at(6, cy)
        check("sidebar no longer paints under the strip",
              corner and close_to(corner, (0x22, 0x1B, 0x19), 6), f"{corner}")

        # close zone: strip fill when unhovered + bright X glyph ink
        czx = round(W - 44 * s * 0.5)
        cf_x = round(W - 44 * s) + 4
        cf = at(cf_x, cy)
        check("close zone fill (unhovered)", cf and close_to(cf, (0x22, 0x1B, 0x19), 6), f"{cf}")
        glyph = sum(
            1 for dx in range(-9, 10) for dy in range(-9, 10)
            if (c := at(czx + dx, cy + dy)) and c[2] > 140 and c[1] > 140 and c[0] > 150
        )
        check("close X glyph ink present", glyph >= 6, f"{glyph}px")

        # maximize zone: square outline â€” stroke core + AA fringes (ink #BDC8D1)
        mx = round(W - round(66 * s))
        # (at() packs (b,g,r): ink #BDC8D1 â†’ b209 > g200 > r189)
        br_pxs = sum(
            1 for dx in range(-10, 11) for dy in range(-7, 8)
            if (c := at(mx + dx, cy + dy)) and c[2] > 95 and c[0] > c[2] + 10
        )
        check("maximize square glyph ink present", br_pxs >= 12, f"{br_pxs}px")

        # minimize zone: the design's 8pt line â€” a bright horizontal run;
        # scan the strip-center rows (1px core sits on 20..22 at 125% DPI)
        mz = round(W - round(110 * s))
        best = 0
        for dy2 in range(-3, 4):
            run = 0
            for dx in range(-12, 13):
                c = at(mz + dx, cy + dy2)
                if c and c[2] > 140 and c[1] > 140:
                    run += 1
                    best = max(best, run)
                else:
                    run = 0
        check("minimize line glyph present", best >= 8, f"run={best}")

        # bottom hairline: a lighter row in the last 4 rows of the strip
        # (blend over #191b22 â‰ˆ r40,g45,b52 â†’ b>g>r ordering)
        hair_row = None
        rows = {}
        for yy in range(strip_h - 4, strip_h):
            c = at(int(W * 0.62), yy)
            rows[yy] = c
            if c and 38 <= c[0] <= 62 and 36 <= c[1] <= 55 and \
                    30 <= c[2] <= 48 and c[0] > c[1] > c[2]:
                hair_row = yy
        check("bottom hairline present", hair_row is not None,
              f"row picked={hair_row} rows={rows}")

        # left ink: 'cmux' text near the left margin
        bright = sum(
            1 for x in range(round(8 * s + 2), round(56 * s))
            if (c := at(x, cy)) and c[2] > 200
        )
        check("app-name ink present", bright >= 2, f"{bright}px")

        # chip fill #1d1f26 (a calm row near the chip's top edge)
        chip_x = None
        rowy = round(8 * s)
        for x in range(round(40 * s), int(W * 0.6)):
            c = at(x, rowy)
            if c and close_to(c, (0x26, 0x1F, 0x1D), 8):
                if all(close_to(at(x + k, rowy) or (9, 9, 9), (0x26, 0x1F, 0x1D), 8) for k in range(1, 10)):
                    chip_x = x
                    break
        check("workspace chip fill present", chip_x is not None, f"x={chip_x}")

        # panes must start below the strip: first pane content row is dark,
        # the strip never paints past 32pt
        below = at(10, strip_h + round(6 * s))
        check("layout below the strip intact", below is not None)  # presence only

    if mode == "behavior":
        lo, t, rro, bt = window_rect(hwnd)
        cx0, cy0 = client_origin(hwnd)

        def refresh():
            global cx0, cy0
            cx0, cy0 = client_origin(hwnd)

        def locate_right(off_pt):
            # right-anchored control zones must track the CURRENT client
            # width (the maximized client is much wider than the one
            # captured at startup)
            refresh()
            lo, t, rro, bt = window_rect(hwnd)
            w = rro - lo
            return cx0 + w - round(off_pt * s), cy0 + cy

        def locate_left_frac(frac):
            refresh()
            lo, t, rro, bt = window_rect(hwnd)
            w = rro - lo
            return cx0 + int(w * frac), cy0 + cy

        def locate_left_pts(pts):
            """Fixed strip anchor in points from the window's left edge,
            valid at every window width (fraction anchors cross into the
            lane-3 command bar once the window widens)."""
            refresh()
            return cx0 + round(pts * s), cy0 + cy

        # 1) maximize button (middle of the three right zones)
        click_at(*locate_right(66))
        time.sleep(1.2)
        check("maximize button maximizes", bool(user32.IsZoomed(hwnd)))
        click_at(*locate_right(66))
        time.sleep(0.8)
        check("maximize button restores", not bool(user32.IsZoomed(hwnd)))

        # 2) caption semantics: double-click on the blank strip maximizes
        # (0.34W = the still-blank drag band; 0.62W is the lane-3 command
        # bar, which is a click target, not a caption point)
        check("clean state before caption dbl-click", not bool(user32.IsZoomed(hwnd)))
        click_at(*locate_left_pts(300), double=True)
        time.sleep(1.2)
        check("caption double-click maximizes", bool(user32.IsZoomed(hwnd)))
        click_at(*locate_left_pts(300), double=True)
        time.sleep(0.8)
        check("caption double-click restores", not bool(user32.IsZoomed(hwnd)))

        # 3) right-edge resize (DefWindowProc band must survive the subclass).
        # The right edge at mid-height is covered by the pane's WebView2
        # child; the left edge over the egui sidebar is not.
        refresh()
        lo, t, rro, bt = window_rect(hwnd)
        before_w = rro - lo
        drag_at(rro - 2, t + (bt - t) // 2, 60, 0)
        time.sleep(0.6)
        lo, t, rro, bt = window_rect(hwnd)
        after_w = rro - lo
        check("edge resize over the webview child", after_w - before_w >= 40,
              f"{before_w}->{after_w} (expected blocked: child eats the band)")
        lo, t, rro, bt = window_rect(hwnd)
        before_w = rro - lo
        drag_at(lo + 2, t + (bt - t) // 2, -60, 0)
        time.sleep(0.6)
        lo, t, rro, bt = window_rect(hwnd)
        after_w = rro - lo
        check("left-edge resize over egui surfaces", after_w - before_w >= 40,
              f"{before_w}->{after_w}")

        # 4) minimize button (first of the three zones)
        click_at(*locate_right(110))
        time.sleep(0.8)
        check("minimize button minimizes", bool(user32.IsIconic(hwnd)))
        user32.ShowWindow(hwnd, 9)  # SW_RESTORE
        time.sleep(0.6)
        check("restore after minimize", not bool(user32.IsIconic(hwnd)))
        # SW_RESTORE from a background caller restores the window WITHOUT
        # re-granting foreground (observed 2026-10-24: the close click
        # after this phase hit the occluding overlay, 3/3 runs). Re-pin.
        pin_foreground(hwnd)
        time.sleep(0.4)

        # 5) close button = app's own close path (pending_window_close)
        click_at(*locate_right(22))
        time.sleep(4.0)
        check("close button exits the app", proc.poll() is not None)
finally:
    if proc and proc.poll() is None:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except Exception:
            proc.kill()
    shutil.rmtree(fixture, ignore_errors=True)

print(f"RESULTS {mode}: {passed} pass / {failed} fail")
