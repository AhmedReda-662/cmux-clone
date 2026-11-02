"""M4.1 live smoke: the harness drives a real `cmux-shell.exe`.

Hermetic: a unique `CMUX_SOCKET_PATH` override (the documented override
keeps the per-user ancestry gate off), an isolated session file, and a
missing (default-producing) config file. The app stays up until teardown,
which kills the root process — its per-pane job objects collapse every
shell descendant.

Checks (each must pass):
  - ping/pong over `system.ping`, capabilities shape, identify rows
  - workspace list/create: ids, selected flag flips to the new workspace
  - surface/pane rows are 1:1 for the created workspace
  - typed text round trip: `echo` marker becomes visible via read_text
  - workspace.select switches the selected row back
  - structured errors: `unknown_method`, `invalid_params`, `not_found`

Exit 0 only when every check printed PASS.
"""

from __future__ import annotations

import argparse
import os
import random
import shutil
import subprocess
import sys
import tempfile
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

    pipe = rf"\\.\pipe\cmux-m41-smoke-{os.getpid()}-{random.randint(1000, 9999)}"
    fixture = Path(tempfile.mkdtemp(prefix="cmux-m41-smoke-"))
    env = dict(os.environ)
    env["CMUX_SOCKET_PATH"] = pipe
    env["CMUX_SESSION_FILE"] = str(fixture / "session.json")
    env["CMUX_CONFIG_FILE"] = str(fixture / "config.json5")  # absent -> defaults
    env.pop("CMUX_SHELL_AUTO_EXIT_MS", None)
    env.pop("CMUX_CLAUDE_HOOKS_DISABLED", None)

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
        check("ping answers pong", poll_until(lambda: _safe(client.ping), 30.0) is True)

        caps = client.capabilities()
        check(
            "capabilities shape",
            caps.get("protocol") == "cmux-socket"
            and caps.get("version") == 2
            and isinstance(caps.get("methods"), list)
            and "system.ping" in caps["methods"]
            and "workspace.create" in caps["methods"],
            f"caps={caps}",
        )

        # The first app-plane call can land inside the documented
        # startup-burst window (the pump drops one reply while the shell
        # starts its PTY lanes; the app answers normally from the next
        # attempt on), so identify polls instead of firing one shot -
        # the same robustness this file already applies to ping.
        ident = poll_until(lambda: _safe(client.identify), 30.0) or {}
        focused = ident.get("focused") or {}
        check(
            "identify carries the focused surface",
            ident.get("app") == "cmux"
            and isinstance(focused.get("surface_id"), str)
            and focused["surface_id"]
            and focused.get("surface_type") == "terminal",
            f"identify={ident}",
        )

        workspaces = client.list_workspaces()
        check(
            "initial workspace row shape",
            len(workspaces) >= 1
            and all(
                isinstance(row.get("index"), int)
                and row.get("id")
                and isinstance(row.get("selected"), bool)
                for row in workspaces
            ),
            f"rows={workspaces}",
        )
        check("exactly one selected workspace", sum(1 for r in workspaces if r["selected"]) == 1)
        prior_count = len(workspaces)
        first_workspace = workspaces[0]["id"]
        first_surface = (focused or {}).get("surface_id", "")

        created = client.new_workspace()
        check(
            "workspace.create reports ids",
            created.get("workspace_id")
            and created.get("surface_id")
            and created["workspace_id"] != first_workspace,
            f"created={created}",
        )
        workspace_id = created["workspace_id"]
        surface_id = created["surface_id"]

        workspaces = client.list_workspaces()
        check("list grows after create", len(workspaces) == prior_count + 1)
        created_row = next((r for r in workspaces if r["id"] == workspace_id), None)
        check(
            "the created workspace becomes selected",
            created_row is not None and created_row["selected"] is True,
            f"rows={workspaces}",
        )

        surfaces = client.list_surfaces(workspace_id)
        check(
            "surface rows for the created workspace",
            len(surfaces) == 1
            and surfaces[0]["id"] == surface_id
            and surfaces[0].get("pane_id") == surface_id
            and surfaces[0].get("type") == "terminal"
            and surfaces[0]["focused"] is True,
            f"surfaces={surfaces}",
        )
        panes = client.list_panes(workspace_id)
        check(
            "pane rows mirror surfaces 1:1",
            len(panes) == 1
            and panes[0]["id"] == surface_id
            and panes[0]["surface_ids"] == [surface_id]
            and panes[0]["selected_surface_id"] == surface_id,
            f"panes={panes}",
        )

        marker = f"cmux-m41-MARKER-{random.randint(10000, 99999)}"
        client.send(f"echo {marker}\r")
        seen = poll_until(
            lambda: _safe(lambda: marker in (client.read_text(surface_id) or "")), 25.0
        )
        check("typed marker becomes visible", bool(seen), "marker not visible in read_text")

        client.select_workspace(first_workspace)
        workspaces = client.list_workspaces()
        selected = [r["id"] for r in workspaces if r["selected"]]
        check("select switches back", selected == [first_workspace], f"rows={workspaces}")

        ident = client.identify()
        refocused = (ident.get("focused") or {}).get("surface_id")
        check("identify follows the refocus", refocused == first_surface, f"identify={ident}")

        try:
            client._call("zzz.not.a.method")
            check("unknown method rejects", False, "no error raised")
        except cmuxError as error:
            check("unknown method rejects", error.code == "unknown_method", f"code={error.code}")

        try:
            client._call("surface.send_text", {"surface_id": surface_id})
            check("missing text rejects", False, "no error raised")
        except cmuxError as error:
            check("missing text rejects", error.code == "invalid_params", f"code={error.code}")

        try:
            client.read_text("p999999")
            check("unknown surface rejects", False, "no error raised")
        except cmuxError as error:
            check("unknown surface rejects", error.code == "not_found", f"code={error.code}")

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


def _safe(fn):
    try:
        return fn()
    except cmuxError:
        return None


if __name__ == "__main__":
    raise SystemExit(smoke())
