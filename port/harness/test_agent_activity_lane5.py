# Lane 5 agent activity stream probe (battery member).
# visual:   agent pane open (right split; tab fill #282a31, stream fill
#           #0c0e14, no cards), after hook seeds (card fill #191b22 +
#           headline ink #e2e2eb grow), during a turn boundary (accent
#           #ffc42f active-card ring), after delivery (ring gone, card
#           fill grew), after the seeded pane exits (agent pane keeps the
#           workspace alive; the plain-mode grammar is asserted here,
#           where a second uncloseable pane no longer disturbs counting),
#           after a relaunch (agent panel type restored with the original
#           id; stream still empty - cards are not persisted).
# behavior: cards ride REAL ingest paths only. `cmux agent open --json`
#           opens the pane (port extension: upstream has no agent verb).
#           Hook events arrive through the real admission queue
#           (`cmux hooks enqueue claude notification|feed`, payload on
#           stdin, CMUX_SURFACE_ID pinning the workspace; feed is a
#           journal route - card only, never ring/toast). The turn
#           boundary uses the m34 hand-over through the pane's own shell:
#           a `python -c` that prints the exact Ollama line (arm: line +
#           CRLF, settle, pending: line without CRLF while the process
#           sleeps - nothing else prints, so the bare bottom line holds).
# Passive captures only (PrintWindow): no clicks/keys against the window;
# CLI/input go through the pipe and the seeded pane's own shell.
# Pixel predicates sample on strides (full-band scans cost ~0.7M px/call
# and would miss the 500ms pending window); ring predicates use the
# tighter strides. Env-isolated app, no listening port.
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

PORT_DIR = r"D:\workspace\Projects\cmux-clone\port"
BIN = PORT_DIR + r"\target\debug\cmux-shell.exe"
CLI = PORT_DIR + r"\target\debug\cmux.exe"

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


def count_strided(surf, x0, y0, x1, y1, pred, stride):
    """Strided pixel census (counts are in SAMPLED pixels, not raw)."""
    n = 0
    for x in range(x0, x1, stride):
        for y in range(y0, y1, stride):
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


def hook_enqueue(env, subcommand, message):
    # Real admission queue: payload on stdin, surface pinned via the
    # allow-listed CMUX_SURFACE_ID environment key (the CLI fails open at
    # exit 0 with `{}` on anything else, so the probe asserts on pixels).
    call_env = dict(env)
    call_env["CMUX_SURFACE_ID"] = SEED_SURFACE_ID
    return subprocess.run(
        [CLI, "hooks", "enqueue", "claude", subcommand],
        env=call_env, input='{"message": "%s"}' % message,
        capture_output=True, text=True)


def surface_rows_json(env):
    return cli("surface", "list", "--json", env=env).stdout


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
    time.sleep(2.0)
    if proc.poll() is None:
        subprocess.run(["taskkill", "/PID", str(proc.pid), "/T", "/F"],
                       capture_output=True)
    # drain leftover server state so reruns stay hermetic
    cli("ping", env=env)


def wait_for(label, pred, seconds, phase=""):
    """Poll a pixel/CLI predicate until true or the deadline lapses."""
    deadline = time.time() + seconds
    while time.time() < deadline:
        if pred():
            check(f"{label} {phase}", True)
            return True
        time.sleep(0.1)
    check(f"{label} {phase}", False, f"not observed within {seconds}s")
    return False


# ---- lane 5 inks (design 1:244/1:257, BGR; card tol 3 keeps it distinct
# from the nearest palette chip #1d1f26) -------------------------------------
STREAM_FILL = (0x14, 0x0E, 0x0C)   # #0c0e14 stream background
CARD_BG = (0x22, 0x1B, 0x19)       # #191b22 complete card fill
TAB_FILL = (0x31, 0x2A, 0x28)      # #282a31 pane tab row fill
HEADLINE_INK = (0xEB, 0xE2, 0xE2)  # #e2e2eb card title text
TIME_INK = (0x9A, 0x92, 0x87)      # #87929a card elapsed time
ACCENT = (0x2F, 0xC4, 0xFF)        # #ffc42f active-card ring/dot/label
TOL = 4


def agent_band(surf):
    """Deep band of the agent pane's interior, below strip and tab row.

    Measured on the live app (instrumented paint print, 2026-10-24): the
    right split is the balanced 2-leaf split with a 12pt gutter, so the
    agent leaf's left edge solves to (cwidth_pt + SIDEBAR + 12) / 2 pt.
    Design round 1 (2026-10-26) returned the aside to its design width
    256 (node 1:399), placing the leaf at [[584,32]..[900,536]] pt at the
    default window since round 4 (2026-10-26) removed the 32pt sub-header
    band. The leaf top sits at 32pt (title strip only), and the leaf's
    own 20pt tab row runs 32..52pt with the 8pt stream inset after it.
    Point-based rows keep this independent of the exact pixel size."""
    s = surf["dpi"] / 96.0
    x0 = int(((surf["cwidth"] / s + 256.0 + 12.0) / 2.0) * s) + int(2 * s)
    x1 = surf["cwidth"] - 4   # keep the leaf's right border column out
    y0 = int(58 * s)   # leaf top 32pt + tab 20pt + stream inset 8pt (+margin)
    # The leaf's bottom sits 24pt above the client bottom at the default
    # layout (measured: leaf rect bottom 536pt vs client 560pt); keep the
    # app's bottom chrome strip (which carries near-card-bg inks) out.
    y1 = surf["cheight"] - int(24 * s) - 2
    return x0, y0, x1, y1


def card_px(surf):
    x0, y0, x1, y1 = agent_band(surf)
    return count_strided(surf, x0, y0, x1, y1,
                         lambda c: close_to(c, CARD_BG, 3), 6)


def headline_px(surf):
    x0, y0, x1, y1 = agent_band(surf)
    return count_strided(surf, x0, y0, x1, y1,
                         lambda c: close_to(c, HEADLINE_INK, 6), 3)


def accent_px(surf):
    # The active card is the bottom-most; the ring is a 1px stroke, so a
    # tight stride over its region is required.
    x0, _, x1, y1 = agent_band(surf)
    top = y1 - int(200 * surf["dpi"] / 96.0)
    return count_strided(surf, x0, top, x1, y1,
                         lambda c: close_to(c, ACCENT, 6), 2)


def time_px(surf):
    x0, y0, x1, y1 = agent_band(surf)
    return count_strided(surf, x0, y0, x1, y1,
                         lambda c: close_to(c, TIME_INK, 8), 3)


def stream_run(surf):
    x0, y0, x1, _ = agent_band(surf)
    best = 0
    for y in range(y0, surf["cheight"] - int(2 * surf["dpi"] / 96.0), 6):
        best = max(best, longest_row_run(surf, y, x0, x1,
                                         lambda c: close_to(c, STREAM_FILL, TOL)))
    return best


def tab_run(surf):
    # The leaf's own tab row sits 32..52pt (round 4 moved it up with the
    # sub-header band; above the stream fill).
    s = surf["dpi"] / 96.0
    leaf_x0 = int(((surf["cwidth"] / s + 256.0 + 12.0) / 2.0) * s) + int(2 * s)
    best = 0
    for y in range(int(34 * s), int(52 * s), 2):
        best = max(best, longest_row_run(
            surf, y, leaf_x0, surf["cwidth"] - 2,
            lambda c: close_to(c, TAB_FILL, 3)))
    return best


# ---- fixture ------------------------------------------------------------------
CONFIG = """{
  // JSONC on read: lane 5 uses the default shell (pwsh); the turn phase
  // drives the m34 hand-over lines through this pane's own shell.
}
"""

mode = sys.argv[1] if len(sys.argv) > 1 else "visual"
fixture = tempfile.mkdtemp(prefix="cmux-lane5-probe-")
env = os.environ.copy()
env["CMUX_SOCKET_PATH"] = rf"\\.\pipe\cmux-lane5-probe-{os.getpid()}"
env["CMUX_SESSION_FILE"] = os.path.join(fixture, "session.json")
env["CMUX_CONFIG_FILE"] = os.path.join(fixture, "config.json5")
env.pop("CMUX_SURFACE_ID", None)
with open(os.path.join(fixture, "config.json5"), "w", encoding="utf-8") as f:
    f.write(CONFIG)

SEED_SURFACE_ID = ""
AGENT_SURFACE_ID = ""

# ---- phase A: the agent pane opens on the right with an empty stream -------
proc = launch(fixture, env)
hwnd = window_of(proc.pid)
check("window appears", hwnd is not None)

reply = cli("agent", "open", "--json", env=env)
check("agent open exits 0", reply.returncode == 0, reply.stderr[:200])
check("agent open json ok", '"ok"' in reply.stdout and re.search(
    r'"type"\s*:\s*"agent"', reply.stdout) is not None, reply.stdout[:200])
m = re.search(r'"pane_id"\s*:\s*"([^"]+)"', reply.stdout)
check("agent open carries a pane id", m is not None, reply.stdout[:200])
m2 = re.search(r'"placement"\s*:\s*"split"', reply.stdout)
check("agent open opens a split", m2 is not None, reply.stdout[:200])
AGENT_SURFACE_ID = m.group(1) if m else ""

identify = cli("identify", "--json", env=env)
check("identify reports the agent surface", re.search(
    r'"surface_type"\s*:\s*"agent"', identify.stdout) is not None,
    identify.stdout[:300])

rows = surface_rows_json(env)
check("surface list types the pane agent", re.search(
    r'"type"\s*:\s*"agent"', rows) is not None, rows[:300])
check("surface list titles the pane", re.search(
    r'"title"\s*:\s*"Agent activity"', rows) is not None, rows[:300])
ids = re.findall(r'"pane_id"\s*:\s*"([^"]+)"', rows)
check("launch shows two surfaces", len(ids) == 2, f"ids={ids}")
if ids:
    SEED_SURFACE_ID = ids[0]  # first leaf = the seeded shell pane

surf = capture(hwnd)
check("capture works", surf["printed"] and surf["got"] == surf["H"], str(surf["got"]))
check("tab fill painted", tab_run(surf) >= 120, f"run={tab_run(surf)}")
check("stream fill painted", stream_run(surf) >= 200, f"run={stream_run(surf)}")
check("launch stream has no cards", card_px(surf) == 0, f"card={card_px(surf)}")

# ---- phase B: hook-pipe events land as cards (real admission queue) --------
n2 = 0
if SEED_SURFACE_ID:
    sent1 = hook_enqueue(env, "notification", "Build finished in 3.1s")
    check("notification enqueue admitted", '"ok"' in sent1.stdout,
          sent1.stdout[:160])
    wait_for("cards grow after notification",
             lambda: card_px(capture(hwnd)) > 0, 8, "hook")
    n1 = card_px(capture(hwnd))
    check("seed 1 card count recorded", n1 > 0, f"card={n1}")

    sent2 = hook_enqueue(env, "feed", "Parsed 14 files across src/server/**")
    check("feed enqueue admitted", '"ok"' in sent2.stdout, sent2.stdout[:160])
    wait_for("cards grow after feed", lambda: card_px(capture(hwnd)) > n1, 8,
             "journal")
    n2 = card_px(capture(hwnd))
    check("seed 2 card count grew", n2 > n1, f"{n1} -> {n2}")
    wait_for("card headline ink", lambda: headline_px(capture(hwnd)) > 2, 4)
    wait_for("card time ink", lambda: time_px(capture(hwnd)) > 2, 4)
else:
    check("hook seeds ran", False, "no surface id resolved")

# ---- phase C: turn boundary -> active card -> completed card ---------------
# The m34 hand-over run as ONE python invocation, with the detector's full
# sequence: t0 idle prompt + CRLF arms; t1 a submission echo + response
# (`>>> Return FINAL_OLLAMA_OK` / `FINAL_OLLAMA_OK`) moves internal state
# only; t2 a prompt WITHOUT a trailing CRLF opens the 500ms pending while
# the process idles; delivery rides the frame-wake deadline sweep. One
# invocation also keeps pwsh's typed-line echoes and prompts out of the
# byte stream (a typed line containing the prompt text mid-line is stray
# state noise the echo-off m34 runner never had). flush=True defeats stdio
# buffering; the tail sleep holds the bare prompt line past delivery.
OLLAMA = ">>> Send a message (/? for help)"
cli("send", "--surface", SEED_SURFACE_ID,
    f'python -c "import time; print(\'{OLLAMA}\'); time.sleep(1); '
    f"print('>>> Return FINAL_OLLAMA_OK'); print('FINAL_OLLAMA_OK'); "
    f"time.sleep(1); print('{OLLAMA}', end='', flush=True); time.sleep(11)\"",
    env=env)
cli("send", "--surface", SEED_SURFACE_ID, "\r", env=env)

wait_for("accent ring", lambda: accent_px(capture(hwnd)) >= 2, 12, "pending")
wait_for("accent gone", lambda: accent_px(capture(hwnd)) == 0, 10, "delivered")
n3 = card_px(capture(hwnd))
check("delivery adds the completed card", n3 > n2 + 2, f"{n2} -> {n3}")
check("stream still shows cards", stream_run(capture(hwnd)) >= 200)

# ---- phase D: close semantics + plain grammar ------------------------------
# The hand-over python sits up to ~13s after its CR (1s + 1s + 11s inside
# the invocation); wait it out so the typed exit lands on pwsh, not on
# python's stdin.
time.sleep(10.0)
cli("send", "--surface", SEED_SURFACE_ID, "exit", env=env)
cli("send", "--surface", SEED_SURFACE_ID, "\r", env=env)
check("pane list shrinks to the agent pane", wait_for(
    "surfaces shrink", lambda: len(re.findall(
        r'"pane_id"\s*:', surface_rows_json(env))) == 1, 20, "exit"))
time.sleep(1.0)
check("agent pane keeps the workspace alive",
      user32.IsWindow(hwnd) and window_of(proc.pid) is not None)
refuse = cli("send", "--surface", AGENT_SURFACE_ID, "x", env=env)
check("agent pane refuses text input", refuse.returncode != 0,
      f"rc={refuse.returncode} out={refuse.stdout[:100]}")
# Plain mode now (a second agent pane here no longer disturbs counting;
# panes have no close verb yet - recorded lane-5 residual).
plain = cli("agent", "open", env=env)
check("agent open plain prints OK split", plain.returncode == 0 and re.search(
    r"^OK surface=\S+ pane=\S+ placement=split", plain.stdout) is not None,
    plain.stdout[:120] + plain.stderr[:120])

# ---- phase E: relaunch restores the agent panes, stream empty --------------
teardown(hwnd, proc, env)

proc2 = launch(fixture, env)
hwnd2 = window_of(proc2.pid)
check("window reappears", hwnd2 is not None)
rows2 = surface_rows_json(env)
check("restored agent pane", re.search(r'"type"\s*:\s*"agent"', rows2)
      is not None, rows2[:400])
check("restored id survives", AGENT_SURFACE_ID in rows2, rows2[:400])
surf2 = capture(hwnd2)
check("restored stream is empty", card_px(surf2) == 0, f"card={card_px(surf2)}")
check("restored pane tab painted", tab_run(surf2) >= 120, f"run={tab_run(surf2)}")
check("previous window destroyed", not user32.IsWindow(hwnd))
teardown(hwnd2, proc2, env)

print(f"\n{passed} passed, {failed} failed ({mode})")
sys.exit(1 if failed else 0)
