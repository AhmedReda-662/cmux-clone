"""Probe child, records channel: reads mouse as console INPUT_RECORDs.

Enables SGR mouse reporting (?1000h ?1006h), then pumps ReadConsoleInputW
and logs every record (decoded MOUSE_EVENT records especially) plus a READY
line once modes are on. This answers "does the conpty host translate VT
mouse sequences written to the input pipe into MOUSE_EVENT records?".

Usage: python mouse_tui_records.py <log>
"""

from __future__ import annotations

import ctypes
import ctypes.wintypes as wt
import os
import sys
import threading
import time

kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)

ENABLE_PROCESSED_INPUT = 0x0001


class COORD(ctypes.Structure):
    _fields_ = [("X", ctypes.c_short), ("Y", ctypes.c_short)]
ENABLE_WINDOW_INPUT = 0x0008
ENABLE_MOUSE_INPUT = 0x0010
ENABLE_EXTENDED_FLAGS = 0x0080

FOCUS_EVENT, KEY_EVENT, MOUSE_EVENT, WINDOW_BUFFER_SIZE_EVENT = 0x0010, 0x0001, 0x0002, 0x0004


class MOUSE_EVENT_RECORD(ctypes.Structure):
    _fields_ = [
        ("dwMousePosition", COORD),
        ("dwButtonState", ctypes.c_ulong),
        ("dwControlKeyState", ctypes.c_ulong),
        ("dwEventFlags", ctypes.c_ulong),
    ]


class KEY_EVENT_RECORD(ctypes.Structure):
    _fields_ = [
        ("bKeyDown", ctypes.c_int),
        ("wRepeatCount", ctypes.c_short),
        ("wVirtualKeyCode", ctypes.c_short),
        ("wVirtualScanCode", ctypes.c_short),
        ("uChar", wt.CHAR),
        ("dwControlKeyState", ctypes.c_ulong),
    ]


class WINDOW_BUFFER_SIZE_RECORD(ctypes.Structure):
    _fields_ = [("dwSize", COORD)]


class EventUnion(ctypes.Union):
    _fields_ = [
        ("KeyEvent", KEY_EVENT_RECORD),
        ("MouseEvent", MOUSE_EVENT_RECORD),
        ("WindowBufferSizeEvent", WINDOW_BUFFER_SIZE_RECORD),
    ]


class INPUT_RECORD(ctypes.Structure):
    _anonymous_ = ("Event",)
    _fields_ = [("EventType", ctypes.c_short), ("Event", EventUnion)]


log_path = sys.argv[1] if len(sys.argv) > 1 else ""
lock = threading.Lock()


def log(line: str) -> None:
    if not log_path:
        return
    with lock:
        with open(log_path, "a", encoding="utf-8") as fh:
            fh.write(line + "\n")


def describe(record: INPUT_RECORD) -> str:
    kind = record.EventType
    if kind == MOUSE_EVENT:
        m = record.MouseEvent
        return (
            f"MOUSE pos=({m.dwMousePosition.X},{m.dwMousePosition.Y}) "
            f"button=0x{m.dwButtonState:08x} flags=0x{m.dwEventFlags:x}"
        )
    if kind == KEY_EVENT:
        k = record.KeyEvent
        mode = "down" if k.bKeyDown else "up"
        ch = k.uChar.UnicodeChar if isinstance(k.uChar.UnicodeChar, str) else repr(k.uChar.AsciiChar)
        return f"KEY {mode} vk=0x{k.wVirtualKeyCode:02x} char={ch!r}"
    if kind == FOCUS_EVENT:
        return "FOCUS"
    if kind == WINDOW_BUFFER_SIZE_EVENT:
        w = record.WindowBufferSizeEvent.dwSize
        return f"WINSIZE {w.X}x{w.Y}"
    return f"OTHER type=0x{kind:04x}"


def main() -> int:
    log("READY records-child")
    # Raw console input: no line input, so records arrive as they happen.
    stdin = wt.HANDLE(kernel32.GetStdHandle(-10))
    mode = ctypes.c_ulong()
    kernel32.GetConsoleMode(stdin, ctypes.byref(mode))
    log(f"STDIN MODE was 0x{mode.value:08x}")
    want = ENABLE_EXTENDED_FLAGS | ENABLE_WINDOW_INPUT | ENABLE_MOUSE_INPUT
    if kernel32.SetConsoleMode(stdin, want):
        log(f"STDIN MODE set 0x{want:08x} ok")
    else:
        log(f"STDIN MODE set 0x{want:08x} FAILED {ctypes.GetLastError()}")

    # Enable SGR mouse reporting, as the opencode TUI does.
    sys.stdout.write("\x1b[?1000h\x1b[?1006h")
    sys.stdout.flush()
    log("MODES ON 1000h+1006h")

    records = (INPUT_RECORD * 64)()
    read = ctypes.c_ulong()
    stop_at = time.monotonic() + 120
    while time.monotonic() < stop_at:
        if not kernel32.ReadConsoleInputW(stdin, records, 64, ctypes.byref(read), ):
            log(f"READ ERROR {ctypes.GetLastError()}")
            break
        for i in range(read.value):
            log(f"REC {describe(records[i])}")
        if read.value >= 64:
            time.sleep(0.01)
    log("BYE records-child")
    sys.stdout.write("\x1b[?1006l\x1b[?1000l")
    sys.stdout.flush()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
