# Temporary lane-2b diagnostic (not a battery member): replicate the
# behavior probe's close-button click on a fresh app and watch what the
# app actually does (click lands? process exits? what does stderr say?).
import ctypes
import ctypes.wintypes as wt
import os
import subprocess
import tempfile
import time

user32 = ctypes.windll.user32
user32.SetThreadDpiAwarenessContext(ctypes.c_void_p(-4))

PORT_DIR = r"D:\workspace\Projects\cmux-clone\port"
BIN = PORT_DIR + r"\target\debug\cmux-shell.exe"


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


fixture = tempfile.mkdtemp(prefix="cmux-lane2b-diag2-")
env = os.environ.copy()
env["CMUX_SOCKET_PATH"] = rf"\\.\pipe\cmux-lane2b-diag2-{os.getpid()}"
env["CMUX_SESSION_FILE"] = os.path.join(fixture, "session.json")
env["CMUX_CONFIG_FILE"] = os.path.join(fixture, "config.json5")
with open(os.path.join(fixture, "config.json5"), "w", encoding="utf-8") as f:
    f.write("{}\n")
log_path = os.path.join(fixture, "stderr.log")
log = open(log_path, "wb")
proc = subprocess.Popen([BIN], cwd=fixture, env=env, stdout=log, stderr=log)
hwnd = None
for _ in range(60):
    ctx = _EnumCtx(proc.pid, ctypes.c_void_p(0), 0)
    user32.EnumWindows(_enum_proc, ctypes.byref(ctx))
    if ctx.HWND:
        hwnd = ctx.HWND
        break
    time.sleep(0.5)
if not hwnd:
    print("no window")
    proc.kill()
    raise SystemExit(1)

user32.keybd_event(0x12, 0, 0, 0)
user32.SetForegroundWindow(hwnd)
time.sleep(0.08)
user32.keybd_event(0x12, 0, 2, 0)
for _ in range(20):
    if user32.GetForegroundWindow() == hwnd:
        break
    time.sleep(0.1)
    user32.SetForegroundWindow(hwnd)
print(f"foreground: {user32.GetForegroundWindow() == hwnd}")

rc = wt.RECT()
user32.GetWindowRect(hwnd, ctypes.byref(rc))
pt = wt.POINT(0, 0)
user32.ClientToScreen(hwnd, ctypes.byref(pt))
W = rc.right - rc.left
dpi = user32.GetDpiForWindow(hwnd)
s = dpi / 96.0
# The probe's locate_right(22) + cy (strip mid, 16pt).
x = pt.x + W - round(22 * s)
y = pt.y + round(16 * s)
print(f"window {rc.left},{rc.top} {W}x{rc.bottom - rc.top} client-origin "
      f"{pt.x},{pt.y} click target ({x},{y})")
time.sleep(1.0)
user32.SetCursorPos(x, y)
time.sleep(0.2)
hit = user32.WindowFromPoint(wt.POINT(x, y))
buf = ctypes.create_unicode_buffer(64)
user32.GetClassNameW(hit, buf, 64)

MOUSEEVENTF_MOVE = 1
MOUSEEVENTF_LEFTDOWN = 2
MOUSEEVENTF_LEFTUP = 4
user32.mouse_event(MOUSEEVENTF_LEFTDOWN, 0, 0, 0, 0)
time.sleep(0.05)
user32.mouse_event(MOUSEEVENTF_LEFTUP, 0, 0, 0, 0)
print(f"clicked; hit-before class='{buf.value}'; proc={proc.poll()}")
for i in range(16):
    time.sleep(0.5)
    print(f"t={0.5 * i + 0.5:.1f} proc={proc.poll()}")
    if proc.poll() is not None:
        break
time.sleep(0.5)
try:
    with open(log_path, "rb") as f:
        tail = f.read()[-1200:].decode("utf-8", "replace")
    print("stderr tail:")
    print(tail)
except OSError as e:
    print("log read failed:", e)
if proc.poll() is None:
    user32.PostMessageW(hwnd, 0x0010, 0, 0)
    time.sleep(1.5)
    if proc.poll() is None:
        subprocess.run(["taskkill", "/PID", str(proc.pid), "/T", "/F"],
                       capture_output=True)
print("diag done")
