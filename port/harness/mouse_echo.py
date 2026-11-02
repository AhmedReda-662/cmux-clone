"""Diagnostic child for mouse-report investigation (not a battery test).

Turns mouse reporting ON (SGR format), then echoes every byte it receives on
stdin: once into a lossless side log file (argv[1]) so a driver can assert on
the exact escape sequences, and once onto its own screen so a human can see
clicks light the TUI up.

The side log carries one JSON-free hex line per read plus markers, e.g.:
    READY
    RX 1b5b3c303b353b314d
    MODE OFF
When the log only grows READY/MODE lines and no RX lines, clicks never reached
the child -- which is the reported complaint, reproduced.
"""

from __future__ import annotations

import os
import sys
import threading
import time

ESC = "\x1b"
log_path = sys.argv[1] if len(sys.argv) > 1 else ""
log_lock = threading.Lock()


def log(line: str) -> None:
    if not log_path:
        return
    with log_lock:
        with open(log_path, "a", encoding="utf-8") as fh:
            fh.write(line + "\n")


def redraw(title: str, events: list[str]) -> None:
    cols, rows = (80, 24)
    try:
        size = os.get_terminal_size()
        cols, rows = size.columns, size.lines
    except (OSError, ValueError):
        pass
    out = [f"{ESC}[2J{ESC}[H", title]
    out.append(f"grid={cols}x{rows} clicks_expected=nothing_yet")
    for i, text in enumerate(events[-8:]):
        out.append(f"RX{i}: {text}")
    tail = max(events[-8:], key=len) if events else ""
    out.append(f"STORED-LAST: {tail}")
    sys.stdout.write("\r\n".join(out) + f"{ESC}[{rows - 1};1Hmouse_echo waiting..." )
    sys.stdout.flush()


def main() -> int:
    log(f"READY pid={os.getpid()}")
    events: list[str] = []
    redraw("mouse_echo: SGR mouse ON (?1000 ?1006)", events)

    # Enable SGR mouse reporting with events on press/release. A plain
    # click that nothing forwards looks like silence on this side.
    sys.stdout.write(f"{ESC}[?1000h{ESC}[?1006h")
    sys.stdout.flush()
    redraw("mouse_echo: SGR mouse ON (?1000 ?1006)", events)

    stop_at = time.monotonic() + 600
    while time.monotonic() < stop_at:
        try:
            chunk = os.read(0, 4096)
        except OSError:
            break
        if not chunk:
            continue
        hexline = chunk.hex()
        events.append(hexline)
        log(f"RX {hexline}")
        text = "".join(
            chr(b) if 32 <= b < 127 else f"\\x{b:02x}" for b in chunk
        )
        redraw(f"mouse_echo: SGR mouse ON ({text[:60]!r})", events)
        if b"\x03" in chunk:  # ctrl+c
            break

    log("BYE")
    sys.stdout.write(f"{ESC}[?1006l{ESC}[?1000l")
    sys.stdout.flush()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
