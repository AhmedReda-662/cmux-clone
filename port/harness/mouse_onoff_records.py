"""On/off probe, records flavor: SetConsoleMode(MOUSE_INPUT) + DECSET on/off.

For the 1.24 conpty host: a child that reads MOUSE_EVENTs (console-API TUI)
turns mouse on with SetConsoleMode and DECSET 1000/1006, then exits cleanly.
Watching the output pipe tells whether OpenConsole's host-facing
`?1003;1006h` enable is also undone at clean exit (?1003l/?1006l), or lingers.
"""

from __future__ import annotations

import ctypes
import ctypes.wintypes as wt
import sys
import time

kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)

ENABLE_EXTENDEd_FLAGS = 0x0080
ENABLE_WINDOW_INPUT = 0x0008
ENABLE_MOUSE_INPUT = 0x0010


def log(path: str, line: str) -> None:
    if not path:
        return
    with open(path, "a", encoding="utf-8") as fh:
        fh.write(line + "\n")


if __name__ == "__main__":
    path = sys.argv[1] if len(sys.argv) > 1 else ""
    log(path, "READY onoff-records")
    stdin = kernel32.GetStdHandle(-10)
    mode = ctypes.c_ulong()
    want = ENABLE_MOUSE_INPUT | ENABLE_WINDOW_INPUT | ENABLE_EXTENDEd_FLAGS
    if kernel32.SetConsoleMode(stdin, want):
        log(path, f"RECORDS MODE 0x{want:08x} ok")
    sys.stdout.write("\x1b[?1000h\x1b[?1006h")
    sys.stdout.flush()
    log(path, "ON")
    time.sleep(0.5)
    sys.stdout.write("\x1b[?1006l\x1b[?1000l")
    sys.stdout.flush()
    log(path, "OFF")
    time.sleep(0.5)
    log(path, "BYE clean")
