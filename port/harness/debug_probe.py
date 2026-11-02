"""One-off probe: launch cmux-shell with the debug-driver env and watch it."""

from __future__ import annotations

import ctypes
import ctypes.wintypes as wt
import os
import random
import subprocess
import sys
import tempfile
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import cmux as harness  # noqa: E402
from cmux import cmuxError  # noqa: E402

user32 = ctypes.WinDLL("user32")


def windows_of(pid: int) -> list[int]:
    found = []

    @ctypes.WINFUNCTYPE(wt.BOOL, wt.HWND, wt.LPARAM)
    def cb(hwnd, _lp):
        p = wt.DWORD()
        user32.GetWindowThreadProcessId(hwnd, ctypes.byref(p))
        if p.value == pid:
            found.append((hwnd, bool(user32.IsWindowVisible(hwnd))))
        return True

    user32.EnumWindows(cb, 0)
    return found


def main() -> int:
    pipe = rf"\\.\pipe\cmux-probe-{os.getpid()}-{random.randint(1000, 9999)}"
    fixture = Path(tempfile.mkdtemp(prefix="cmux-probe-"))
    (fixture / "cmux.json").write_text(
        '{\n  "terminal": { "shell": { "program": "powershell.exe", '
        '"args": ["-NoLogo", "-NoProfile"] } }\n}\n',
        encoding="utf-8",
    )
    env = dict(os.environ)
    env["CMUX_SOCKET_PATH"] = pipe
    env["CMUX_SESSION_FILE"] = str(fixture / "session.json")
    env["CMUX_CONFIG_FILE"] = str(fixture / "cmux.json")
    for extra in ("CMUX_SHELL_AUTO_EXIT_MS", "CMUX_CLAUDE_HOOKS_DISABLED",
                  "CMUX_PASSWORD_FILE", "CMUX_SOCKET_PASSWORD", "CMUX_IPC_DISABLED"):
        env.pop(extra, None)

    process = subprocess.Popen(
        [sys.argv[1]], cwd=str(fixture), env=env,
        stdout=subprocess.DEVNULL,
        stderr=open(fixture / "err.log", "ab"),
    )
    print(f"pid={process.pid} pipe={pipe}")
    client = None
    for sweep in range(20):
        alive = process.poll() is None
        wins = windows_of(process.pid)
        ping = None
        err = None
        try:
            if client is None:
                client = harness.cmux(socket_path=pipe, connect_timeout_s=5.0, timeout_s=3.0)
            ping = bool(client.ping())
        except Exception as exc:  # noqa: BLE001
            err = repr(exc)
        print(f"sweep={sweep} alive={alive} wins={wins} ping={ping} err={err}")
        time.sleep(1.5)
    if client is not None:
        try:
            print("caps:", client.capabilities())
        except Exception as exc:  # noqa: BLE001
            print("caps err:", repr(exc))
    subprocess.run([
        sys.executable,
        str(Path(os.environ["TEMP"]) / "opencode" / "close_cmux.py"),
        str(process.pid),
    ], timeout=60)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
