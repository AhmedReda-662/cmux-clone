"""Round 10 live battery: terminal mouse reporting for TUI programs.

Owner round 10 (2026-10-27/28). Proves the click story end to end with a
real person's gestures (SendInput) against the bundled ConPTY host:

  Leg A - TUI mouse reporting (records channel, the channel crossterm/tcell/
          bubbletea-class Windows TUIs read):
    - launch a real ReadConsoleInput child in a pane (it enables SGR mouse
      reporting ?1000h ?1006h and ENABLE_MOUSE_INPUT console mode)
    - two SendInput clicks at distinct cells -> MOUSE_EVENT records with
      distinct 0-based positions
    - a press-drag-release -> at least one button-held record plus release,
      positions diverging from the press cell
    - a wheel tick -> a MOUSE_WHEELED record

  Leg B - native selection survives when nothing reports mouse:
    - fresh app/shell pane (no mouse modes ever emitted)
    - drag-select paints selection ink (band bytes change), and
      none of a widget's mouse reports leak (shell pane stays selection-only)

Style: same no-deps assert script as the r9 battery, identical launch and
gesture helpers imported from test_sidebar_depth_r9.
"""

from __future__ import annotations

import os
import random
import subprocess
import sys
import tempfile
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import cmux as harness  # noqa: E402
import test_sidebar_depth_r9 as R9  # noqa: E402
from cmux import cmuxError  # noqa: E402

TEMP = Path(os.environ.get("TEMP", r"C:\Users\noah\AppData\Local\Temp")) / "opencode"
TEMP.mkdir(parents=True, exist_ok=True)


def read_lines(path: Path) -> list[str]:
    try:
        return [
            line
            for line in path.read_text(encoding="utf-8", errors="replace").splitlines()
            if line.strip()
        ]
    except FileNotFoundError:
        return []


def mouse_records(path: Path) -> list[tuple[int, int, int, int]]:
    """Parsed REC MOUSE lines: (x, y, button_state, event_flags)."""
    out = []
    for line in read_lines(path):
        if not line.startswith("REC MOUSE"):
            continue
        try:
            pos = line.split("pos=(", 1)[1].split(")", 1)[0]
            x, y = (int(v) for v in pos.split(","))
            button = int(line.split("button=0x", 1)[1].split(" ", 1)[0], 16)
            flags = int(line.split("flags=0x", 1)[1], 16)
        except (IndexError, ValueError):
            continue
        out.append((x, y, button, flags))
    return out


def poll_records(path: Path, deadline_s: float, predicate):
    end = time.monotonic() + deadline_s
    while time.monotonic() < end:
        records = mouse_records(path)
        if predicate(records):
            return records
        time.sleep(0.25)
    return mouse_records(path)


def launch_app(pipe: str, fixture: Path, shell_bin: Path) -> tuple[subprocess.Popen, Path]:
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
        [str(shell_bin)], cwd=str(fixture), env=env,
        stdout=subprocess.DEVNULL, stderr=app_log,
    )
    return process, fixture / "app-stderr.log"


def ping_and_hwnd(process: subprocess.Popen, client) -> int:
    deadline = time.monotonic() + 30.0
    pinged = False
    last_ping_error: Exception | None = None
    while time.monotonic() < deadline and not pinged:
        try:
            pinged = bool(client.ping())
        except Exception as exc:  # noqa: BLE001
            last_ping_error = exc
            pinged = False
        time.sleep(0.2)
    assert pinged, f"no IPC ping (last error: {last_ping_error!r})"
    hwnd = R9.poll_until(lambda: R9.main_window_for(process.pid), 20.0)
    assert hwnd, "no main window"
    return hwnd


def focus_surface(surface: R9.ClientSurface) -> None:
    # R9's idiom: after ClientSurface's MoveWindow the activation must be
    # re-taken, then the US layout pinned.
    assert R9.poll_until(
        lambda: R9.foreground_window(surface.hwnd)
        and R9.user32.GetForegroundWindow() == surface.hwnd,
        10.0,
    ), "window not foreground"
    R9.pin_us_layout(surface.hwnd)
    R9.settle(0.6)
    surface.refresh()


def close_app(process: subprocess.Popen, fixture: Path) -> None:
    closer = TEMP / "close_cmux.py"
    if closer.exists():
        subprocess.run(
            [sys.executable, str(closer), str(process.pid)], timeout=60,
            capture_output=True,
        )
        R9.poll_until(lambda: None, 2.0)


def smoke() -> int:
    root = Path(__file__).resolve().parents[1]
    shell_bin = root / "target" / "debug" / "cmux-shell.exe"
    if not shell_bin.exists():
        shell_bin = root / "target" / "release" / "cmux-shell.exe"
    assert shell_bin.exists(), f"cmux-shell.exe not found ({shell_bin})"
    shell_bin = shell_bin.resolve()
    assert (shell_bin.parent / "conpty.dll").is_file(), (
        "bundled conpty pair not staged next to the app (build.rs)"
    )

    child_path = Path(__file__).resolve().parent / "mouse_tui_records.py"
    pipe = rf"\\.\pipe\cmux-r10-mouse-{os.getpid()}-{random.randint(1000, 9999)}"
    fixture = Path(tempfile.mkdtemp(prefix="cmux-r10-mouse-"))
    child_log = TEMP / f"mouse_records_{os.getpid()}.log"
    child_log.write_text("", encoding="utf-8")

    process = None
    failures: list[str] = []
    try:
        process, _app_log = launch_app(pipe, fixture, shell_bin)
        client = harness.cmux(socket_path=pipe, connect_timeout_s=30.0, timeout_s=5.0)
        hwnd = ping_and_hwnd(process, client)
        if os.environ.get("R10_NO_RESIZE") == "1":
            # Diagnostic lane: verify on the default-size window (the size the
            # earlier in-app mouse receipts were taken at).
            surface = R9.ClientSurface.__new__(R9.ClientSurface)
            surface.hwnd = hwnd
            surface.refresh()
            R9.poll_until(
                lambda: R9.foreground_window(hwnd)
                and R9.user32.GetForegroundWindow() == hwnd, 10.0,
            )
            R9.pin_us_layout(hwnd)
        else:
            surface = R9.ClientSurface(hwnd)
            focus_surface(surface)
        R9.settle(1.5)

        # --- Leg A: records-channel TUI child in the pane ----------------
        R9.type_text(f"python {child_path} {child_log}")
        R9.send_enter()
        ready = R9.poll_until(
            lambda: any(l.startswith("READY") for l in read_lines(child_log)), 25.0
        )
        assert ready, "records child never reported READY"
        R9.settle(2.5)

        # Two clicks at distinct cells -> distinct MOUSE_EVENT positions.
        # Pane-safe points inside the 640x400 window (512 pt wide at 1.25x).
        if os.environ.get("R10_NO_RESIZE") == "1":
            # Default-size window (1125x700 px, 900x560 pt): the geometry the
            # earlier in-app receipts were taken against.
            c1, c2 = (420.0, 250.0), (530.0, 285.0)
        else:
            c1, c2 = (300.0, 250.0), (430.0, 270.0)
        R9.click(*surface.screen_of(*c1))
        R9.settle(1.2)
        R9.click(*surface.screen_of(*c2))
        R9.settle(1.2)
        recs = poll_records(child_log, 5.0, lambda r: len(r) >= 4)
        presses = [r for r in recs if r[2] == 0x1]
        releases = [r for r in recs if r[2] == 0x0]
        check_requires(failures, "clicks deliver press records", len(presses) >= 2)
        check_requires(failures, "clicks deliver release records", len(releases) >= 2)
        if len(recs) >= 4:
            xs, ys = {r[0] for r in recs}, {r[1] for r in recs}
            check_requires(failures, "distinct cells produce distinct positions",
                           len(xs) >= 2 or len(ys) >= 2,
                           f"positions seen: {sorted({(r[0], r[1]) for r in recs})}")
        else:
            check_requires(failures, "at least 4 mouse records for two clicks", False,
                           f"have {len(recs)}")

        # Drag: press, move, release -> motion records under a held button.
        before = len(recs)
        R9.click(*surface.screen_of(300.0, 250.0))  # anchor press before the drag
        R9.settle(0.5)
        px1, py1 = surface.screen_of(310.0, 250.0)
        px2, py2 = surface.screen_of(440.0, 275.0)
        R9.drag(px1, py1, px2, py2)
        R9.settle(1.5)
        recs = poll_records(child_log, 5.0, lambda r: len(r) >= before + 6)
        drag_window = recs[before:]
        held = [r for r in drag_window if r[2] == 0x1]
        releases = [r for r in drag_window if r[2] == 0x0]
        check_requires(failures, "drag press lands as a record", len(held) >= 1)
        check_requires(failures, "drag release lands as a record", len(releases) >= 1)
        if held:
            last = drag_window[-1]
            check_requires(failures, "drag moves across cells", last[0] != held[0][0],
                           f"press pos {held[0][:2]} -> last drag pos {last[:2]}")

        # Wheel: egui scrolls the widget by whole lines while tracking -> one
        # press+release of button four/five per line -> MOUSE_WHEELED records.
        at_leg_a_end = len(recs)
        R9.wheel(*surface.screen_of(340.0, 260.0), 3)
        R9.settle(1.0)
        R9.wheel(*surface.screen_of(340.0, 260.0), -3)
        R9.settle(1.2)
        recs = poll_records(child_log, 5.0, lambda r: len(r) >= at_leg_a_end + 2)
        wheelish = [r for r in recs[at_leg_a_end:] if r[3] == 0x4]
        check_requires(failures, "wheel ticks deliver MOUSE_WHEELED records",
                       len(wheelish) >= 1,
                       f"new records after wheel: {recs[at_leg_a_end:]}")

        # --- Leg B: fresh shell pane keeps native selection --------------
        close_app(process, fixture)
        R9.poll_until(lambda: None, 1.0)
        pipe = rf"\\.\pipe\cmux-r10-mouse-{os.getpid()}-{random.randint(1000, 99999)}"
        fixture = Path(tempfile.mkdtemp(prefix="cmux-r10-shell-"))
        process, _app_log = launch_app(pipe, fixture, shell_bin)
        client = harness.cmux(socket_path=pipe, connect_timeout_s=30.0, timeout_s=5.0)
        hwnd = ping_and_hwnd(process, client)
        surface = R9.ClientSurface(hwnd)
        focus_surface(surface)
        R9.settle(2.5)

        band = (300.0, 230.0, 460.0, 280.0)
        before_ink = R9.band_bytes(surface, *band)
        px1, py1 = surface.screen_of(310.0, 240.0)
        px2, py2 = surface.screen_of(450.0, 240.0)
        R9.drag(px1, py1, px2, py2)
        R9.settle(1.0)
        after_ink = R9.band_bytes(surface, *band)
        diff = R9.band_diff(before_ink, after_ink)
        check_requires(failures, "shell drag still paints selection ink",
                       diff >= 200, f"band diff={diff}/{len(before_ink)}")
        leaked = mouse_records(child_log)
        check_requires(failures, "no mouse records were reported for the shell",
                       len(leaked) == 0, f"records: {leaked}")

        if failures:
            for f in failures:
                print(f"FAIL: {f}")
            return 1
        print("all checks passed")
        return 0

    finally:
        if process is not None:
            close_app(process, fixture)
        print(f"fixture={fixture} child_log={child_log}")


def after_before(before_count: int, records: list) -> range:
    return range(before_count, len(records))


def check_requires(failures: list[str], name: str, condition: bool, detail: str = "") -> None:
    if condition:
        print(f"PASS: {name}")
    else:
        failures.append(f"{name} {f'({detail})' if detail else ''}")


if __name__ == "__main__":
    raise SystemExit(smoke())
