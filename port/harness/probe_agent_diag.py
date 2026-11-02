# Temporary lane-5 diagnostic (not a battery member; self-contained --
# the lane-5 probe runs its phases at module top-level, so importing it
# is not safe): ground truth for the tab row geometry, the static
# near-#191b22 baseline in the band, and whether the active-card ring
# shows during a live pending window.
import ctypes
import ctypes.wintypes as wt
import os
import re
import subprocess
import tempfile
import time

user32 = ctypes.windll.user32
gdi32 = ctypes.windll.gdi32

user32.SetThreadDpiAwarenessContext(ctypes.c_void_p(-4))
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

PORT_DIR = r"D:\workspace\Projects\cmux-clone\port"
BIN = PORT_DIR + r"\target\debug\cmux-shell.exe"
CLI = PORT_DIR + r"\target\debug\cmux.exe"

STREAM_FILL = (0x14, 0x0E, 0x0C)
CARD_BG = (0x22, 0x1B, 0x19)
TAB_FILL = (0x31, 0x2A, 0x28)
ACCENT = (0x2F, 0xC4, 0xFF)


def close_to(c, ref, tol):
    return all(abs(cs - rs) <= tol for cs, rs in zip(c, ref))


class BMIH(ctypes.Structure):
    _fields_ = [
        ("biSize", ctypes.c_ulong), ("biWidth", ctypes.c_long),
        ("biHeight", ctypes.c_long), ("biPlanes", ctypes.c_ushort),
        ("biBitCount", ctypes.c_ushort), ("biCompression", ctypes.c_ulong),
        ("biSizeImage", ctypes.c_ulong), ("biXPelsPerMeter", ctypes.c_long),
        ("biYPelsPerMeter", ctypes.c_long), ("biClrUsed", ctypes.c_ulong),
        ("biClrImportant", ctypes.c_ulong),
    ]


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


def capture(hwnd):
    rc = wt.RECT()
    user32.GetWindowRect(hwnd, ctypes.byref(rc))
    W, H = rc.right - rc.left, rc.bottom - rc.top
    crc = wt.RECT()
    user32.GetClientRect(hwnd, ctypes.byref(crc))
    cwidth, cheight = crc.right - crc.left, crc.bottom - crc.top
    pt = wt.POINT(0, 0)
    user32.ClientToScreen(hwnd, ctypes.byref(pt))
    ox, oy = pt.x - rc.left, pt.y - rc.top
    dpi = user32.GetDpiForWindow(hwnd)
    hdc = user32.GetWindowDC(hwnd)
    mem = gdi32.CreateCompatibleDC(hdc)
    bmp = gdi32.CreateCompatibleBitmap(hdc, W, H)
    old = gdi32.SelectObject(mem, bmp)
    printed = user32.PrintWindow(hwnd, mem, 2)
    gdi32.SelectObject(mem, old)
    bmi = BMIH(ctypes.sizeof(BMIH), W, -H, 1, 32, 0, 0)
    buf = ctypes.create_string_buffer(W * H * 4)
    got = gdi32.GetDIBits(mem, bmp, 0, H, buf, ctypes.byref(bmi), 0)
    gdi32.DeleteObject(bmp)
    gdi32.DeleteDC(mem)
    user32.ReleaseDC(hwnd, hdc)
    raw = buf.raw

    def at(x, y):
        wx, wy = x + ox, y + oy
        if x < 0 or y < 0 or wx < 0 or wy < 0 or wx >= W or wy >= H:
            return None
        i = (wy * W + wx) * 4
        return (raw[i], raw[i + 1], raw[i + 2])

    return dict(W=W, H=H, cwidth=cwidth, cheight=cheight, dpi=dpi,
                ox=ox, oy=oy, at=at, printed=printed, got=got)


def longest_row_run(surf, y, x0, x1, pred):
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
    user32.PostMessageW(hwnd, 0x0010, 0, 0)
    time.sleep(2.0)
    if proc.poll() is None:
        subprocess.run(["taskkill", "/PID", str(proc.pid), "/T", "/F"],
                       capture_output=True)
    cli("ping", env=env)


def band(surf):
    s = surf["dpi"] / 96.0
    return (int(surf["cwidth"] * 0.64), int(90 * s),
            surf["cwidth"] - 4, surf["cheight"] - int(24 * s) - 2)


fixture = tempfile.mkdtemp(prefix="cmux-lane5-diag-")
env = os.environ.copy()
env["CMUX_SOCKET_PATH"] = rf"\\.\pipe\cmux-lane5-diag-{os.getpid()}"
env["CMUX_SESSION_FILE"] = os.path.join(fixture, "session.json")
env["CMUX_CONFIG_FILE"] = os.path.join(fixture, "config.json5")
env.pop("CMUX_SURFACE_ID", None)
with open(os.path.join(fixture, "config.json5"), "w", encoding="utf-8") as f:
    f.write("{\n}\n")

proc = launch(fixture, env)
hwnd = window_of(proc.pid)
reply = cli("agent", "open", "--json", env=env)
print("open:", reply.returncode, reply.stdout[:160])
time.sleep(1.0)

surf = capture(hwnd)
s = surf["dpi"] / 96.0
cw, ch = surf["cwidth"], surf["cheight"]
print(f"client={cw}x{ch} dpi={surf['dpi']} s={s:.3f}")

# 1) raw pixel probes down the middle of the right pane, 30..60pt.
xm = int(cw * 0.75)
for ypt in range(30, 62, 2):
    y = int(ypt * s)
    print(f"pt x=0.75cw y={ypt}pt -> {surf['at'](xm, y)}")

# 2) where do near-card-bg pixels actually live in the band?
x0, y0, x1, y1 = band(surf)
found = []
for x in range(x0, x1, 6):
    for y in range(y0, y1, 6):
        c = surf["at"](x, y)
        if c is not None and close_to(c, CARD_BG, 3):
            found.append((x, y))
print(f"near-card-bg strided hits: {len(found)}")
if found:
    xs = [p[0] for p in found]
    ys = [p[1] for p in found]
    print(f"bbox x {min(xs)}..{max(xs)} y {min(ys)}..{max(ys)} (client px)")
    rows = {}
    for _, fy in found:
        rows[fy // 12] = rows.get(fy // 12, 0) + 1
    print("row histogram (12px rows):", dict(sorted(rows.items())))
    for px, py in found[:8]:
        print(f"  sample ({px},{py}) {surf['at'](px, py)}")

# 3) tab runs per row in the scan window.
for ypt in range(32, 58, 2):
    y = int(ypt * s)
    r = longest_row_run(surf, y, int(cw * 0.52), cw - 2,
                        lambda c: close_to(c, TAB_FILL, 3))
    if r:
        print(f"tab run y={ypt}pt -> {r}")

# 4) live pending-window watch.
OLLAMA = ">>> Send a message (/? for help)"
seed = cli("surface", "list", "--json", env=env).stdout
seed_id = re.findall(r'"pane_id"\s*:\s*"([^"]+)"', seed)[0]
cli("send", "--surface", seed_id,
    f'python -c "import time; print(\'{OLLAMA}\'); time.sleep(1)"', env=env)
cli("send", "--surface", seed_id, "\r", env=env)
time.sleep(2.0)
cli("send", "--surface", seed_id,
    f'python -c "import time; print(\'{OLLAMA}\', end=\'\', flush=True); time.sleep(8)"',
    env=env)
cli("send", "--surface", seed_id, "\r", env=env)
t0 = time.time()
while time.time() - t0 < 11.0:
    surf2 = capture(hwnd)
    a = 0
    x0, _, x1, y1 = band(surf2)
    top = y1 - int(200 * s)
    hist = {}
    for x in range(x0, x1, 8):
        for y in range(top, y1, 8):
            c = surf2["at"](x, y)
            if c:
                if close_to(c, ACCENT, 6):
                    a += 1
                hist[c] = hist.get(c, 0) + 1
    top3 = sorted(hist.items(), key=lambda kv: -kv[1])[:4]
    pretty = [(f"{c[2]:02x}{c[1]:02x}{c[0]:02x}", n) for c, n in top3]
    print(f"t={time.time()-t0:5.2f} accent={a:4d} top={pretty}")
    time.sleep(0.12)

teardown(hwnd, proc, env)
print("diag done")
