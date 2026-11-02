"""One-off: run test_terminal_mouse_r10.launch_app + ping, printing exceptions."""

from __future__ import annotations

import os
import random
import sys
import time
from pathlib import Path

sys.path.insert(0, r"D:\workspace\Projects\cmux-clone\port\harness")
import test_terminal_mouse_r10 as R10  # noqa: E402
import test_sidebar_depth_r9 as R9  # noqa: E402
import cmux as harness  # noqa: E402


def main() -> int:
    shell_bin = Path(r"D:\workspace\Projects\cmux-clone\port\target\debug\cmux-shell.exe").resolve()
    pipe = rf"\\.\pipe\cmux-r10-ping-{os.getpid()}-{random.randint(1000, 9999)}"
    fixture = Path(R10.tempfile.mkdtemp(prefix="cmux-r10-ping-"))
    process, app_log_path = R10.launch_app(pipe, fixture, shell_bin)
    print(f"pid={process.pid} pipe={pipe} fixture={fixture}", flush=True)
    client = None
    try:
        client = harness.cmux(socket_path=pipe, connect_timeout_s=30.0, timeout_s=5.0)
        print("client connected", flush=True)
        deadline = time.monotonic() + 20.0
        attempt = 0
        while time.monotonic() < deadline:
            attempt += 1
            try:
                value = client.ping()
                print(f"attempt {attempt}: ping -> {value!r}", flush=True)
                if value:
                    break
            except Exception as exc:  # noqa: BLE001
                print(f"attempt {attempt}: ping raised {exc!r}", flush=True)
            time.sleep(1.0)
        alive = process.poll() is None
        print(f"alive={alive} poll={process.poll()!r}", flush=True)
        if alive:
            print(app_log_path.read_text(encoding="utf-8", errors="replace"), flush=True)
        return 0
    finally:
        if client is not None:
            pass
        if process is not None:
            R10.close_app(process, fixture)


if __name__ == "__main__":
    raise SystemExit(main())
