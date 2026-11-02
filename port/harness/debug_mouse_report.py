"""Debug driver: does a click inside a pane reach a mouse-reporting child?

Pairs with mouse_echo.py. Launches the app (the r9 battery's launch idiom),
types the echo child into the pane, then:
  1. Control: a slow drag-select inside the pane must repaint the terminal
     band (selection ink) - proves egui's pointer events reach the widget.
  2. Subject: two plain fast clicks at pane cells. The echo side log gains
     one `RX <hex>` line per click ONLY IF the widget forwarded mouse
     reports to the child. No RX lines = clicks die in the widget.

Temporary diagnostic for the mouse-report investigation -- the round battery
replaces it once the fix lands.
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
import test_sidebar_depth_r9 as R9  # noqa: E402  (helper library)
from cmux import cmuxError  # noqa: E402


def log_lines(path: Path) -> list[str]:
    try:
        return [
            line
            for line in path.read_text(encoding="utf-8", errors="replace").splitlines()
            if line
        ]
    except FileNotFoundError:
        return []


def main() -> int:
    root = Path(__file__).resolve().parents[1]
    shell_bin = root / "target" / "debug" / "cmux-shell.exe"
    if not shell_bin.exists():
        shell_bin = root / "target" / "release" / "cmux-shell.exe"
    assert shell_bin.exists(), f"cmux-shell.exe not found ({shell_bin})"
    shell_bin = shell_bin.resolve()

    pipe = rf"\\.\pipe\cmux-mousedbg-{os.getpid()}-{random.randint(1000, 9999)}"
    fixture = Path(tempfile.mkdtemp(prefix="cmux-mousedbg-"))
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
    print(f"launched pid={process.pid} pipe={pipe}")

    echo_log = Path(os.environ["TEMP"]) / "opencode" / f"mouse_echo_{process.pid}.log"
    echo_log.write_text("", encoding="utf-8")
    child_py = Path(__file__).resolve().parent / "mouse_echo.py"

    result = 0
    try:
        client = harness.cmux(socket_path=pipe, connect_timeout_s=30.0, timeout_s=5.0)
        deadline = time.monotonic() + 60.0
        while time.monotonic() < deadline:
            try:
                answers = bool(client.ping())
            except Exception as exc:  # noqa: BLE001  (diagnostic print)
                answers = False
                if time.monotonic() > deadline - 55.0:
                    print(f"ping err: {exc!r}")
            if answers:
                break
            time.sleep(1.0)
        assert answers, "no IPC ping"

        hwnd = R9.poll_until(lambda: R9.main_window_for(process.pid), 20.0)
        assert hwnd, "no main window"
        R9.user32.SetForegroundWindow(hwnd)
        assert R9.poll_until(
            lambda: R9.foreground_window(hwnd)
            and R9.user32.GetForegroundWindow() == hwnd,
            10.0,
        ), "window not foreground"
        R9.pin_us_layout(hwnd)

        surface = R9.ClientSurface(hwnd)
        R9.settle(2.0)

        # Spawn the echo child inside the pane by typing at the fresh prompt.
        R9.type_text(f"python {child_py} {echo_log}")
        R9.send_enter()

        # The child logs READY once its modes are on and stdin is armed.
        ready = R9.poll_until(
            lambda: any(l.startswith("READY") for l in log_lines(echo_log)), 20.0
        )
        print(f"echo READY: {ready!r}")
        R9.settle(2.0)

        # Control: drag-select a range in the pane; selection ink must
        # change the band's bytes (the widget does receive pointer events).
        band = (400.0, 210.0, 560.0, 265.0)
        before = R9.band_bytes(surface, *band)
        px1, py1 = surface.screen_of(430.0, 220.0)
        px2, _py2 = surface.screen_of(550.0, 220.0)
        R9.drag(px1, py1, px2, py1)
        R9.settle(1.0)
        after = R9.band_bytes(surface, *band)
        diff = R9.band_diff(before, after)
        print(f"CONTROL selection drag repaints band: diff={diff} / {len(before)}")

        # Subject: two plain fast clicks at distinct cells.
        for tag, pt in (
            ("A", (420.0, 240.0)),
            ("B", (500.0, 262.0)),
        ):
            cx, cy = surface.screen_of(*pt)
            R9.click(cx, cy)
            R9.settle(1.0)

        rx_lines = [ln for ln in log_lines(echo_log) if ln.startswith("RX")]
        print("--- echo log ---")
        for line in log_lines(echo_log):
            print(" ", line[:180])
        print(f"RX count: {len(rx_lines)}  (click_count target: 2+)")
        result = 0 if len(rx_lines) >= 2 else 1

    except Exception as exc:  # noqa: BLE001  (diagnostic: report, don't crash)
        print(f"EXCEPTION: {exc!r}")
        result = 2

    finally:
        closer = Path(os.environ["TEMP"]) / "opencode" / "close_cmux.py"
        if closer.exists():
            subprocess.run([sys.executable, str(closer), str(process.pid)], timeout=60)
            R9.poll_until(lambda: None, 1.0)
        tail = fixture / "app-stderr.log"
        if tail.exists() and tail.stat().st_size:
            print("--- app stderr tail ---")
            print("\n".join(tail.read_text(encoding='utf-8', errors='replace').splitlines()[-12:]))
        print(f"fixture={fixture} echo_log={echo_log}")

    return result


if __name__ == "__main__":
    raise SystemExit(main())
