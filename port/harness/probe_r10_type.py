"""One-off: launch the app, foreground it, type the records child command, read the screen."""

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

TEMP = Path(os.environ.get("TEMP", r"C:\Users\noah\AppData\Local\Temp")) / "opencode"


def main() -> int:
    shell_bin = Path(r"D:\workspace\Projects\cmux-clone\port\target\debug\cmux-shell.exe").resolve()
    pipe = rf"\\.\pipe\cmux-r10-type-{os.getpid()}-{random.randint(1000, 9999)}"
    fixture = Path(R10.tempfile.mkdtemp(prefix="cmux-r10-type-"))
    child_log = TEMP / f"probe_records_{os.getpid()}.log"
    child_log.write_text("", encoding="utf-8")
    process, _ = R10.launch_app(pipe, fixture, shell_bin)
    client = harness.cmux(socket_path=pipe, connect_timeout_s=30.0, timeout_s=5.0)
    try:
        hwnd = R10.bring_foreground(process, client)
        surface = R9.ClientSurface(hwnd)
        R9.settle(2.0)
        print("read_text@start:", repr(client.read_text())[:400], flush=True)
        child_path = Path(r"D:\workspace\Projects\cmux-clone\port\harness\mouse_tui_records.py")
        R9.type_text(f"python {child_path} {child_log}")
        R9.send_enter()
        R9.settle(3.0)
        print("read_text@after:", repr(client.read_text())[:900], flush=True)
        print("child_log:", repr(child_log.read_text(encoding="utf-8", errors="replace"))[:400], flush=True)
        return 0
    finally:
        R10.close_app(process, fixture)


if __name__ == "__main__":
    raise SystemExit(main())
