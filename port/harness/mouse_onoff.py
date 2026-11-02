"""On/off probe: enable SGR mouse, disable, exit -- capture the host's tail.

For the 1.24 conpty host: does a clean child exit also clear the
host-facing ?1003;1006 pair (so a later shell in the same pane is free), or
does the hosting terminal stay in mouse mode forever?
"""

from __future__ import annotations

import os
import sys
import time

log_path = sys.argv[1] if len(sys.argv) > 1 else ""


def log(line: str) -> None:
    if not log_path:
        return
    with open(log_path, "a", encoding="utf-8") as fh:
        fh.write(line + "\n")


if __name__ == "__main__":
    log("READY onoff")
    sys.stdout.write("\x1b[?1000h\x1b[?1006h")   # enable
    sys.stdout.flush()
    log("ON")
    time.sleep(0.5)
    sys.stdout.write("\x1bprint screen frontier")  # untouched by conpty render
    sys.stdout.write("\x1b[?1006l\x1b[?1000l")   # disable
    sys.stdout.flush()
    log("OFF")
    time.sleep(0.5)
    log("BYE clean exit follows")
