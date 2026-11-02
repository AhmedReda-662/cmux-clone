"""M4.4 live smoke: the concurrent connection pool over a real `cmux-shell.exe`.

Upstream contract (cli-contract.md, "Control-socket admission and deadlines"):
the app never lets one control command block the others — every accepted
connection gets a real reply. The port's shape (ADR-0005 Amendment 4): the
pipe name holds `PIPE_INSTANCES` (8) byte-mode slots, each served on its own
thread; one parked peer occupies one slot and cannot hold the pipe.

Hermetic, reusing the M4.1 launch conventions: a unique `CMUX_SOCKET_PATH`
override (keeps the ancestry gate off), an isolated session file, a missing
(default-producing) config file, and an owned process tree at teardown.

Checks (each must pass):
  - readiness ping, plus the M4.1 identify shape as a cheap regression
  - 4 concurrent clients × (ping + send) rounds: every exchange answers in
    its own envelope (any cross-talk raises `Mismatched response id`)
  - a silent peer — connected, never writes — does not starve a CLI ping
    (with the old single-instance server the open stalls until it hangs up)
  - a second app instance on the SAME pipe name claims → `IPC disabled`,
    while the first server keeps serving
  - no panic anywhere in the app's stderr at teardown

Exit 0 only when every check printed PASS.
"""

from __future__ import annotations

import argparse
import ctypes
import os
import random
import shutil
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import cmux as harness  # noqa: E402
from cmux import cmuxError  # noqa: E402

PASS: list[str] = []
FAIL: list[str] = []


def check(name: str, condition: bool, detail: str = "") -> bool:
    if condition:
        PASS.append(name)
        print(f"PASS {name}")
        return True
    FAIL.append(name)
    print(f"FAIL {name} {detail}".rstrip())
    return False


def poll_until(fn, deadline_s: float, interval_s: float = 0.2):
    """Calls `fn` until it returns truthy (or the deadline expires)."""
    end = time.monotonic() + deadline_s
    last = None
    while time.monotonic() < end:
        last = fn()
        if last:
            return last
        time.sleep(interval_s)
    return last


def _safe(fn):
    try:
        return fn()
    except cmuxError:
        return None


SILENT_PEER = """\
import sys, time, ctypes
GENERIC_READ_WRITE = 0xC0000000
OPEN_EXISTING = 3
handle = ctypes.windll.kernel32.CreateFileW(
    sys.argv[1], GENERIC_READ_WRITE, 0, None, OPEN_EXISTING, 0, None)
if handle in (-1, ctypes.c_void_p(-1).value):
    sys.stderr.write("open-failed err=%d" % ctypes.GetLastError())
else:
    sys.stderr.write("held")
sys.stderr.flush()
time.sleep(3.0)
if handle not in (-1, ctypes.c_void_p(-1).value):
    ctypes.windll.kernel32.CloseHandle(handle)
"""


def smoke() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--bin", default=None, help="cmux-shell.exe path override")
    args = parser.parse_args()

    root = Path(__file__).resolve().parents[1]
    default = root / "target" / "debug" / "cmux-shell.exe"
    if not default.exists():
        default = root / "target" / "release" / "cmux-shell.exe"
    shell_bin = Path(args.bin) if args.bin else default
    assert shell_bin.exists(), f"cmux-shell.exe not found at {shell_bin} (build first)"
    shell_bin = shell_bin.resolve()
    cli_bin = root / "target" / "debug" / "cmux.exe"
    if not cli_bin.exists():
        cli_bin = root / "target" / "release" / "cmux.exe"
    assert cli_bin.exists(), f"cmux.exe not found at {cli_bin} (build first)"
    cli_bin = cli_bin.resolve()

    pipe = rf"\\.\pipe\cmux-m44-smoke-{os.getpid()}-{random.randint(1000, 9999)}"
    fixture = Path(tempfile.mkdtemp(prefix="cmux-m44-smoke-"))
    env = dict(os.environ)
    env["CMUX_SOCKET_PATH"] = pipe
    env["CMUX_SESSION_FILE"] = str(fixture / "session.json")
    env["CMUX_CONFIG_FILE"] = str(fixture / "config.json5")  # absent -> defaults
    env.pop("CMUX_SHELL_AUTO_EXIT_MS", None)
    env.pop("CMUX_CLAUDE_HOOKS_DISABLED", None)
    peer_script = fixture / "silent_peer.py"
    peer_script.write_text(SILENT_PEER, encoding="utf-8")

    app_log = open(fixture / "app-stderr.log", "ab")
    process = subprocess.Popen(
        [str(shell_bin)],
        cwd=str(fixture),
        env=env,
        stdout=subprocess.DEVNULL,
        stderr=app_log,
    )
    print(f"launched {shell_bin.name} pid={process.pid} pipe={pipe}")

    client = None
    try:
        client = harness.cmux(socket_path=pipe, connect_timeout_s=30.0, timeout_s=5.0)
        # Readiness = the app plane, not the pipe: ping/capabilities are
        # answered inline by the server, while identify hops to the frame
        # loop (the M4.1 400 ms budget, structured timeout before the first
        # frame). Poll identify once, then everything below is app-plane.
        ident = poll_until(lambda: _safe(client.identify), 30.0)
        check("app plane ready (identify answers)", isinstance(ident, dict))
        if not isinstance(ident, dict):
            raise SystemExit(1)
        focused = ident.get("focused") or {}
        check(
            "identify shape survives the pool (M4.1 regression)",
            ident.get("app") == "cmux"
            and isinstance(focused.get("surface_id"), str)
            and bool(focused.get("surface_id"))
            and focused.get("surface_type") == "terminal",
            f"identify={ident}",
        )

        # --- concurrent burst: 4 clients x 8 rounds, all through the pool ---
        failures: list[str] = []

        def worker(index: int) -> None:
            try:
                own = harness.cmux(
                    socket_path=pipe, connect_timeout_s=10.0, timeout_s=5.0
                )
            except cmuxError as error:
                failures.append(f"client {index} connect: {error}")
                return
            try:
                for round_index in range(8):
                    try:
                        if own.ping() is not True:
                            failures.append(f"client {index} ping r{round_index}: no pong")
                    except cmuxError as error:
                        failures.append(f"client {index} ping r{round_index}: {error}")
                    try:
                        own.send(f"echo pool-w{index}r{round_index}")
                    except cmuxError as error:
                        failures.append(f"client {index} send r{round_index}: {error}")
            finally:
                pass

        threads = [
            threading.Thread(target=worker, args=(index,), name=f"pool-worker-{index}")
            for index in range(4)
        ]
        stamp = time.monotonic()
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join(timeout=90)
        burst_s = time.monotonic() - stamp
        check(
            "32 pooled exchanges all answered in their own envelopes",
            not failures and all(not thread.is_alive() for thread in threads),
            "; ".join(failures[:4]),
        )
        print(f"     (4 clients x 8 rounds in {burst_s:.1f}s)")

        # --- silent peer must not starve the pipe ---
        silent = subprocess.Popen(
            [sys.executable, "-X", "utf8", str(peer_script), pipe],
            cwd=str(fixture),
            env=env,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            text=True,
            errors="replace",
        )
        held_marker = None
        try:
            held_marker = silent.stderr.readline().strip()  # blocks until the peer marks
        except (OSError, ValueError) as error:
            check("silent peer opened a slot", False, f"marker read: {error}")
        if held_marker == "held":
            check("silent peer opened a slot", True)
            started = time.monotonic()
            cli = subprocess.run(
                [str(cli_bin), "--socket", pipe, "ping"],
                capture_output=True,
                text=True,
                errors="replace",
                timeout=30,
            )
            cli_s = time.monotonic() - started
            check(
                "CLI ping answers fast while a silent peer holds a slot",
                cli.returncode == 0 and cli_s < 2.0 and "pong" in cli.stdout,
                f"exit={cli.returncode} elapsed={cli_s:.2f}s out={cli.stdout.strip()[:80]!r}",
            )
            print(f"     (ping while held: {cli_s:.2f}s)")
        else:
            check("silent peer opened a slot", False, f"marker={held_marker!r}")
            subprocess.run(["taskkill", "/F", "/T", "/PID", str(silent.pid)],
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        if silent.poll() is None:
            try:
                silent.wait(timeout=6)
            except subprocess.TimeoutExpired:
                silent.kill()
                silent.wait(timeout=6)
        if silent.stderr:
            silent.stderr.close()

        # --- same-name second instance: the claim path ---
        second_log_path = fixture / "second-stderr.log"
        seen_claim = {"lines": None}

        def claim_line_seen():
            try:
                text = second_log_path.read_text(errors="replace")
            except OSError:
                return None
            if "IPC disabled" in text:
                return [line for line in text.splitlines() if "IPC disabled" in line]
            return None

        second_log = open(second_log_path, "ab")
        second = subprocess.Popen(
            [str(shell_bin)],
            cwd=str(fixture),
            env=env,
            stdout=subprocess.DEVNULL,
            stderr=second_log,
        )
        try:
            seen_claim["lines"] = poll_until(claim_line_seen, 25.0)
            check(
                "second instance on the owned name logs `IPC disabled`",
                bool(seen_claim["lines"]),
                f"claimed={seen_claim['lines']}",
            )
            still = _safe(client.ping) is True
            check("first server keeps serving during the claim attempt", still)
        finally:
            second.terminate()
            try:
                second.wait(timeout=10)
            except subprocess.TimeoutExpired:
                subprocess.run(["taskkill", "/F", "/T", "/PID", str(second.pid)],
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                second.wait(timeout=10)
            second_log.close()

        # --- no panics ---
        try:
            tail_text = (fixture / "app-stderr.log").read_text(errors="replace")
        except OSError:
            tail_text = ""
        check("no panic in the app stderr", "panicked" not in tail_text)

    finally:
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                subprocess.run(
                    ["taskkill", "/F", "/T", "/PID", str(process.pid)],
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                )
                process.wait(timeout=10)
        time.sleep(0.3)
        app_log.close()
        if FAIL:
            try:
                tail = (fixture / "app-stderr.log").read_text(errors="replace")[-2000:]
                print("---- app stderr tail ----")
                print(tail)
            except OSError:
                pass
        shutil.rmtree(fixture, ignore_errors=True)

    print()
    print(f"smoke complete: {len(PASS)} passed, {len(FAIL)} failed")
    if FAIL:
        print("failed checks:", ", ".join(FAIL))
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(smoke())
