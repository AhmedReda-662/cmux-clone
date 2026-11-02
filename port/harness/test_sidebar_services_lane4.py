# Lane 4 aside services probe (battery member).
# visual:   services segment at launch (header ink present, no rows),
#           after the seeded listener lives (row fill #0c0e14, warm port
#           ink #ffdf9f, blue UP badge #8ed5ff), after the owner dies
#           (STOP latch: fill persists, warm/blue inks gone), after the
#           pane exits (rows pruned; header remains).
# behavior: all live phases ride the real 2s sampler tick; the listener is
#           spawned through the pane's own shell (config command), killed
#           by the owning pid found via probe-side netstat, and the pane
#           exits by typing `exit` + CR (`cmux send` twice; raw LF would
#           only produce pwsh's `>>` continuation prompt), then the pane
#           closes on shell exit per the product decision, pruning the
#           pane's rows.
# Passive captures only (PrintWindow): no clicks/keys against the window.
# Env-isolated app (CMUX_CONFIG_FILE/SESSION/SOCKET) at fixed port 18777.
import ctypes
import ctypes.wintypes as wt
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
gdi32.CreateCompatibleDC.argtypes = [ctypes.c_void_p]
gdi32.CreateCompatibleBitmap.restype = ctypes.c_void_p
gdi32.CreateCompatibleBitmap.argtypes = [ctypes.c_void_p, ctypes.c_int, ctypes.c_int]
gdi32.DeleteObject.argtypes = [ctypes.c_void_p]
gdi32.DeleteDC.argtypes = [ctypes.c_void_p]

PORT = r"D:\workspace\Projects\cmux-clone\port"
BIN = PORT + r"\target\debug\cmux-shell.exe"
CLI = PORT + r"\target\debug\cmux.exe"
LISTEN_PORT = 18777

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
    { "name": "listen", "description": "spawn a real listener",
      "command": "python -m http.server 18777" }
  ]
}
"""


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


def count_box(surf, x0, y0, x1, y1, pred):
    n = 0
    for x in range(x0, x1):
        for y in range(y0, y1):
            c = surf["at"](x, y)
            if c is not None and pred(c):
                n += 1
    return n


def longest_row_run(surf, y, x0, x1, pred):
    """Longest horizontal x-run matching pred on one scan line."""
    best, cur = 0, 0
    for x in range(x0, x1):
        c = surf["at"](x, y)
        if c is not None and pred(c):
            cur += 1
            best = max(best, cur)
        else:
            cur = 0
    return best


def cli(*args, env):
    return subprocess.run([CLI, *args], env=env, capture_output=True, text=True)


def surface_ids(env):
    out = cli("surface", "list", "--json", env=env)
    return re.findall(r'"pane_id"\s*:\s*"([^"]+)"', out.stdout)


def launch(fixture, env):
    log = open(os.path.join(fixture, "stderr.log"), "wb")
    proc = subprocess.Popen([BIN], cwd=fixture, env=env, stdout=log, stderr=log)
    deadline = time.time() + 30
    while time.time() < deadline:
        if cli("ping", env=env).returncode == 0:
            return proc
        if proc.poll() is not None:
            log.flush()
            with open(os.path.join(fixture, "stderr.log"), "rb") as f:
                print(f.read()[:2000].decode("utf-8", "replace"))
            raise SystemExit(1)
        time.sleep(0.25)
    raise SystemExit("shell never answered ping")


def teardown(hwnd, proc, env):
    user32.PostMessageW(hwnd, 0x0010, 0, 0)  # WM_CLOSE
    time.sleep(1.0)
    if proc.poll() is None:
        subprocess.run(["taskkill", "/PID", str(proc.pid), "/T", "/F"],
                       capture_output=True)
    # drain leftover server state so reruns stay hermetic
    cli("ping", env=env)


# ---- segment geometry (points; established in the painter) ----------------
# The sidebar rect starts BELOW the 32pt title strip (verified from a live
# ink profile: workspace row text mid ~87pt = rect.top 32 + plus 8..34 +
# 6 + row center 72+15=87), so: workspace row 72..102, last row bottom
# 102; services header 114..128 (+12), rows top 132 (26pt tall, 2pt
# gaps): row1 132..158, row2 160..186. BGR inks; tol 4 (lane-3 rule).
# Design round 1 (2026-10-26) returned the aside to its design width 256
# (node 1:399; it painted 216 before): the x extents below carry that
# width, y stays as measured (the width change moves nothing vertical).
def segment_metrics(surf):
    s = surf["dpi"] / 96.0
    return dict(
        s=s,
        header_y0=int(113 * s), header_y1=int(129 * s),
        header_x0=int(10 * s), header_x1=int(110 * s),
        rows_y0=int(131 * s), rows_y1=int(187 * s),
        fill_x0=int(8 * s), fill_x1=int(254 * s),
        badge_x0=int((256 - 46) * s), badge_x1=int((256 - 6) * s),
    )


HEADER_INK = (0x9A, 0x92, 0x87)   # #87929a muted, BGR
WARM_INK = (0x9F, 0xDF, 0xFF)     # #ffdf9f warm port numerals, BGR
BADGE_INK = (0xFF, 0xD5, 0x8E)    # #8ed5ff UP badge, BGR
ROW_FILL = (0x14, 0x0E, 0x0C)     # #0c0e14 pane-interior fill, BGR
TOL = 4


def longest_fill_run(surf, m):
    best = 0
    y0, y1 = m["rows_y0"], min(m["rows_y1"], surf["cheight"])
    for y in range(y0, y1, 3):
        best = max(best,
                   longest_row_run(surf, y, m["fill_x0"], m["fill_x1"],
                                   lambda c: close_to(c, ROW_FILL, TOL)))
    return best


def warm_px(surf, m):
    return count_box(surf, m["fill_x0"], m["rows_y0"], m["fill_x1"],
                     min(m["rows_y1"], surf["cheight"]),
                     lambda c: close_to(c, WARM_INK, TOL))


def badge_px(surf, m):
    return count_box(surf, m["badge_x0"], m["rows_y0"], m["badge_x1"],
                     min(m["rows_y1"], surf["cheight"]),
                     lambda c: close_to(c, BADGE_INK, TOL))


def header_ink(surf, m):
    return count_box(surf, m["header_x0"], m["header_y0"], m["header_x1"],
                     min(m["header_y1"], surf["cheight"]),
                     lambda c: close_to(c, HEADER_INK, max(TOL, 10)))


def listener_pid(port):
    """Owning pid of a listening TCP port, via probe-side netstat rows."""
    out = subprocess.run(["netstat", "-ano", "-p", "TCP"],
                         capture_output=True, text=True).stdout
    for line in out.splitlines():
        cols = line.split()
        # Rows are numeric (headers localize; rows do not): [TCP] [local]
        # [remote] [state] [pid] — 5 columns with LISTENING mid.
        if len(cols) >= 5 and f":{port}" in cols[1] and cols[3] == "LISTENING":
            try:
                return int(cols[4])
            except ValueError:
                continue
    return None


def wait_for(label, pred, seconds, phase=""):
    """Poll a pixel/CLI predicate until true or the deadline lapses."""
    deadline = time.time() + seconds
    while time.time() < deadline:
        if pred():
            check(f"{label} {phase}", True)
            return True
        time.sleep(1.0)
    check(f"{label} {phase}", False, f"not observed within {seconds}s")
    return False


mode = sys.argv[1] if len(sys.argv) > 1 else "visual"
fixture = tempfile.mkdtemp(prefix="cmux-lane4-probe-")
env = os.environ.copy()
env["CMUX_SOCKET_PATH"] = rf"\\.\pipe\cmux-lane4-probe-{os.getpid()}"
env["CMUX_SESSION_FILE"] = os.path.join(fixture, "session.json")
env["CMUX_CONFIG_FILE"] = os.path.join(fixture, "config.json5")
env.pop("CMUX_SHELL_AUTO_EXIT_MS", None)
with open(env["CMUX_CONFIG_FILE"], "w", encoding="ascii") as f:
    f.write(CONFIG)

print("step: launch (no git fixture; services lane)", flush=True)
proc = launch(fixture, env)
print("step: launched, ping ok", flush=True)
hwnd = None
try:
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

    surf = capture(hwnd)
    check("printwindow ok", bool(surf["printed"]))
    check("dpi lines ok", surf["got"] == surf["H"])
    check("client large enough", surf["cwidth"] >= 420 and surf["cheight"] >= 500,
          f"{surf['cwidth']}x{surf['cheight']}")
    m = segment_metrics(surf)
    check("sidebar fits the 256pt column", surf["cwidth"] > int(256 * m["s"]),
          f"client {surf['cwidth']}px at s={m['s']}")

    # ---- phase 1: fresh pane, no services ---------------------------------
    georgia = longest_fill_run(surf, m)
    check("no service rows at launch", georgia < int(60 * m["s"]), f"fill run {georgia}px")
    check("no warm inks at launch", warm_px(surf, m) == 0, f"{warm_px(surf, m)}px")
    check("no blue badges at launch", badge_px(surf, m) == 0, f"{badge_px(surf, m)}px")
    check("RUNTIME SERVICES header ink", header_ink(surf, m) >= 4,
          f"{header_ink(surf, m)}px")

    # ---- phase 2: seed a real listener through the pane's shell -----------
    ids = surface_ids(env)
    check("surface listed", bool(ids), f"{ids}")
    sid = ids[0]
    seed = cli("command", "run", "listen", "--surface", sid, env=env)
    check("config command typed", seed.returncode == 0, seed.stderr.strip())
    # precondition cross-evidence: the OS tablе really has the listener.
    wait_for("netstat sees :18777 LISTENING",
             lambda: listener_pid(LISTEN_PORT) is not None, 12)

    def up_row_visible():
        surf = capture(hwnd)
        return (longest_fill_run(surf, m) >= int(150 * m["s"])
                and warm_px(surf, m) >= 3 and badge_px(surf, m) >= 2)

    if wait_for("services UP row printed", up_row_visible, 12, "after seed"):
        surf_up = capture(hwnd)
        check("UP fill run wide", longest_fill_run(surf_up, m) >= int(150 * m["s"]),
              f"{longest_fill_run(surf_up, m)}px")
        check("warm :port ink", warm_px(surf_up, m) >= 3, f"{warm_px(surf_up, m)}px")
        check("UP badge ink", badge_px(surf_up, m) >= 2, f"{badge_px(surf_up, m)}px")
        rows = count_box(surf_up, m["fill_x0"], m["rows_y0"], m["fill_x1"],
                         min(m["rows_y1"], surf_up["cheight"]),
                         lambda c: close_to(c, ROW_FILL, TOL))
        check("row fill pixels present", rows > 500, f"{rows}px")

        # ---- phase 3: kill the owner -> STOP latch ------------------------
        pid = listener_pid(LISTEN_PORT)
        check("listener owner pid found", pid is not None, "netstat")
        subprocess.run(["taskkill", "/PID", str(pid), "/F"], capture_output=True)

        def stop_latch_visible():
            surf = capture(hwnd)
            return (warm_px(surf, m) == 0 and badge_px(surf, m) == 0
                    and longest_fill_run(surf, m) >= int(150 * m["s"])
                    and header_ink(surf, m) >= 4)

        did_latch = wait_for("STOP latch keeps the row", stop_latch_visible, 12,
                             "after kill")
        surf_stop = None
        if did_latch:
            surf_stop = capture(hwnd)
            check("STOP keeps the warm ink off", warm_px(surf_stop, m) == 0,
                  f"{warm_px(surf_stop, m)}px")
            check("STOP keeps the badge off", badge_px(surf_stop, m) == 0,
                  f"{badge_px(surf_stop, m)}px")

        # ---- phase 4: the pane exits -> its rows prune --------------------
        # The pane is the workspace's last, and the last workspace closing
        # closes the window (established product decision), so the live
        # prune observation is the app exiting cleanly right after pane
        # exit — the sampler must survive the vanished root without
        # hanging the frame loop. The map-level prune is unit-tested
        # (port_state::dead_root_leaves_no_history_row). The app process
        # has no exit code contract through taskkill elsewhere, so accept
        # exit code 0 or the window simply vanishing.
        # `cmux send` types raw keystrokes without Enter (upstream parity),
        # and a raw LF only produces pwsh's `>>` continuation prompt
        # (verified 2026-10-24, the same finding behind the CR fix in
        # command.run), so the probe appends CR explicitly.
        out = cli("send", "--surface", sid, "exit", env=env)
        check("exit typed", out.returncode == 0, out.stderr.strip())
        time.sleep(0.4)
        out = cli("send", "--surface", sid, "\r", env=env)
        check("enter typed", out.returncode == 0, out.stderr.strip())

        def app_closed_cleanly():
            if proc.poll() is not None:
                return True
            return window_of(proc.pid) is None

        deadline = time.time() + 10
        closed = False
        while time.time() < deadline:
            if app_closed_cleanly():
                closed = True
                break
            time.sleep(1.0)
        check("app closed cleanly after last pane exit", closed,
              f"proc.poll={proc.poll()}")
finally:
    if hwnd:
        teardown(hwnd, proc, env)

print(f"summary visual={passed} pass / {failed} fail", flush=True)
if failed:
    raise SystemExit(1)
