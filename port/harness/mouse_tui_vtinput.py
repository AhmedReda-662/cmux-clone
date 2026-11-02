"""Probe child, VT-input bytes channel: reads mouse off stdin as raw bytes.

Enables SGR mouse reporting (?1000h ?1006h), puts its console stdin into
ENABLE_VIRTUAL_TERMINAL_INPUT raw mode, then reads stdin bytes and logs
them — this answers "does the conpty host pass mouse sequences through on
the stdin byte stream for a VT-input child?".

Usage: python mouse_tui_vtinput.py <log>
"""

from __future__ import annotations

import ctypes
import ctypes.wintypes as wt
import msvcrt
import os
import sys
import threading
import time

kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)

ENABLE_VIRTUAL_TERMINAL_INPUT = 0x0200
ENABLE_WINDOW_INPUT = 0x0008
ENABLE_EXTENDED_FLAGS = 0x0080

log_path = sys.argv[1] if len(sys.argv) > 1 else ""
lock = threading.Lock()


def log(line: str) -> None:
    if not log_path:
        return
    with lock:
        with open(log_path, "a", encoding="utf-8") as fh:
            fh.write(line + "\n")


def main() -> int:
    log("READY vtinput-child")
    stdin = wt.HANDLE(kernel32.GetStdHandle(-10))
    mode = ctypes.c_ulong()
    kernel32.GetConsoleMode(stdin, ctypes.byref(mode))
    log(f"STDIN MODE was 0x{mode.value:08x}")
    want = ENABLE_VIRTUAL_TERMINAL_INPUT | ENABLE_WINDOW_INPUT | ENABLE_EXTENDED_FLAGS
    if not kernel32.SetConsoleMode(stdin, want):
        log(f"VT-input set FAILED {ctypes.GetLastError()}; trying plain raw 0")
        kernel32.SetConsoleMode(stdin, 0)
        log("STDIN MODE set 0x00000000 (raw)")

    # Enable SGR mouse reporting, as the opencode TUI does.
    sys.stdout.write("\x1b[?1000h\x1b[?1006h")
    sys.stdout.flush()
    log("MODES ON 1000h+1006h")

    stop_at = time.monotonic() + 120
    stdin_fd = 0
    while time.monotonic() < stop_at:
        if msvcrt.kbhit():
            try:
                chunk = os.read(stdin_fd, 4096)
            except OSError as exc:
                log(f"READ ERROR {exc}")
                break
            if chunk:
                log(f"RX {chunk.hex()}")
                if b"\x03" in chunk:
                    break
        time.sleep(0.01)
    log("BYE vtinput-child")
    sys.stdout.write("\x1b[?1006l\x1b[?1000l")
    sys.stdout.flush()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
