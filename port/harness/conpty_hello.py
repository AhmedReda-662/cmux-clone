"""Minimal conpty child: proves the stage-1 spawn pipes basic I/O.

Write-free probe: logs to a FIXED side path (no argv dependency) the moment
python starts, prints to stdout/stderr, exits immediately. The exit code
tells whether CreateProcess → interpreter init → main ran at all.
"""

from __future__ import annotations

import os
import sys

if __name__ == "__main__":
    side = os.path.join(
        os.environ["TEMP"], "opencode", "conpty_hello_side.log"
    )
    with open(side, "a", encoding="utf-8") as fh:
        fh.write("HELLO from conpty_hello: python running, argv=%r\n" % (sys.argv,))
    print("HELLO STDOUT", flush=True)
    print("HELLO STDERR", file=sys.stderr, flush=True)
