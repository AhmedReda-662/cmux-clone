"""M6 live smoke: the SSH verb (workspace.ssh.open, ADR-0014).

Upstream's full SSH feature is the managed carrier stack (relay bootstrap,
the CMUX_TUI engine, ssh-config connection sharing). The owner approved the
Windows subset: `cmux ssh <destination>` opens a workspace whose pane child
is the Windows OpenSSH client (`ssh.exe`), mounted through the same pane
machinery as every shell pane - child-exit lifecycle, shell-tracked pane
identity, and a title the app knows (destination, or --name). This lane
verifies that subset end to end, plus the parser's refusal grammar.

Scenarios:

  A. parser refusals (no app needed: the CLI parses before it connects)
     - missing destination, value-less/range-out --port, a leading short
       flag where the destination must be, unknown --flag, and the
       managed-carrier-only --window/--transport/--session lines

  B. live opens against loopback tcp (hermetic - no remote host involved)
     - value-refused: workspace.ssh.open to a closed local port; the reply
       carries workspace/surface ids, the ssh child dies on "connect
       refused", the pane closes, the workspace closes, and the app lives
     - silent hold: an open to a local listener that accepts but never
       sends a banner; the ssh child waits (TCP connect done, protocol
       blocked), the pane stays, the row title is the destination, and the
       workspace is active (focus default)
     - named hold: --name build box --no-focus: the same hold with the
       override title, and the previously activated workspace stays active
     - teardown: WM_CLOSE exits 0, and every ssh child spawned by the app
       (parent == app pid) is reaped with it

The port-set claims under ADR-0014: destination + --port/--identity/
--ssh-option/--name/--command/--focus/--no-focus/-- and -A/-a pass-through;
--window/--transport/--session refuse as not supported; the managed
engine, relay auth_required flow, and mosh stay documented residuals.

No pytest, no extra deps (ctypes only), the same assert-script style as
the other lanes. Hermetic: unique pipes, temp fixtures, a config that pins
`powershell.exe -NoLogo -NoProfile`. Exit 0 only when every check printed
PASS.
"""

from __future__ import annotations

import argparse
import ctypes
import ctypes.wintypes as wt
import json
import os
import random
import socket
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

if sys.stdout and hasattr(sys.stdout, "reconfigure"):
    sys.stdout.reconfigure(encoding="utf-8", errors="backslashreplace")
    sys.stderr.reconfigure(encoding="utf-8", errors="backslashreplace")


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
    except (cmuxError, OSError):
        return None


def retry_ipc(fn, attempts: int = 4, delay_s: float = 0.4):
    """IPC replies can land during a rotation instant; retry the same call."""
    last = None
    for _attempt in range(attempts):
        last = _safe(fn)
        if last is not None:
            return last
        time.sleep(delay_s)
    return last


# --------------------------- Win32 helpers -----------------------------

user32 = ctypes.WinDLL("user32", use_last_error=True)
WM_CLOSE = 0x0010


def main_window_for(pid: int) -> int | None:
    """The app's visible top-level window (the m43 lane's finder)."""
    found = []

    @ctypes.WINFUNCTYPE(wt.BOOL, wt.HWND, wt.LPARAM)
    def on_window(hwnd, _lparam):
        owner_pid = wt.DWORD()
        user32.GetWindowThreadProcessId(hwnd, ctypes.byref(owner_pid))
        if owner_pid.value == pid and user32.IsWindowVisible(hwnd):
            rect = wt.RECT()
            user32.GetWindowRect(hwnd, ctypes.byref(rect))
            if rect.right - rect.left > 200 and rect.bottom - rect.top > 150:
                found.append(hwnd)
        return True  # keep enumerating

    user32.EnumWindows(on_window, 0)
    return found[0] if found else None


def kill_stray_shells() -> None:
    subprocess.run(
        ["taskkill", "/F", "/IM", "cmux-shell.exe"], capture_output=True, check=False
    )
    time.sleep(0.6)


def close_gracefully(process, deadline_s: float = 20.0) -> bool:
    """WM_CLOSE on the app's window, then poll for exit 0."""
    if process.poll() is not None:
        return process.returncode == 0
    hwnd = poll_until(lambda: main_window_for(process.pid), 10.0)
    if not hwnd:
        return False
    user32.PostMessageW(hwnd, WM_CLOSE, 0, 0)
    return bool(poll_until(lambda: process.poll() == 0, deadline_s))


def ssh_children_of(parent_pid: int) -> list[int]:
    """ssh.exe pids whose parent is the app (the pane children only)."""
    script = (
        "Get-CimInstance -ClassName Win32_Process "
        f"-Filter \"Name='ssh.exe' and ParentProcessId={parent_pid}\" "
        "| ForEach-Object { $_.ProcessId }"
    )
    try:
        out = subprocess.run(
            ["powershell.exe", "-NoProfile", "-Command", script],
            capture_output=True,
            timeout=30,
            check=False,
        )
    except OSError:
        return []
    pids: list[int] = []
    for token in out.stdout.decode("utf-8", "backslashreplace").split():
        if token.strip().isdigit():
            pids.append(int(token))
    return sorted(set(pids))


# --------------------------- launch + store ----------------------------


def launch(shell_bin: Path, fixture: Path, pipe: str):
    env = dict(os.environ)
    env["CMUX_SOCKET_PATH"] = pipe
    env["CMUX_SESSION_FILE"] = str(fixture / "session.json")
    env["CMUX_CONFIG_FILE"] = str(fixture / "cmux.json")
    for gone in (
        "CMUX_SHELL_AUTO_EXIT_MS",
        "CMUX_SHELL_AUTO_SPLIT_MS",
        "CMUX_CLAUDE_HOOKS_DISABLED",
        "CMUX_PASSWORD_FILE",
        "CMUX_SOCKET_PASSWORD",
        "CMUX_IPC_DISABLED",
        "CMUX_SOCKET_MODE",
    ):
        env.pop(gone, None)
    log_path = fixture / "app-stderr.log"
    app_log = open(log_path, "ab")
    process = subprocess.Popen(
        [str(shell_bin)],
        cwd=str(fixture),
        env=env,
        stdout=subprocess.DEVNULL,
        stderr=app_log,
    )
    return process, log_path


def make_config(fixture: Path) -> None:
    config = (
        '{\n  "terminal": { "shell": { "program": "powershell.exe", '
        '"args": ["-NoLogo", "-NoProfile"] } }\n}\n'
    )
    (fixture / "cmux.json").write_text(config, encoding="utf-8")


def log_has(log_path: Path, needle: str) -> bool:
    try:
        return needle in log_path.read_text(
            encoding="utf-8", errors="backslashreplace"
        )
    except OSError:
        return False


def silent_listener() -> tuple[socket.socket, int]:
    """A loopback listener: ssh can TCP-connect but never gets a banner."""
    listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    listener.bind(("127.0.0.1", 0))
    listener.listen(1)
    return listener, listener.getsockname()[1]


def grab_free_port() -> int:
    """A port that currently has nothing listening (connect-refused lane)."""
    probe = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    probe.bind(("127.0.0.1", 0))
    port = probe.getsockname()[1]
    probe.close()
    time.sleep(0.3)
    return port


def surface_title(client, surface_id: str, workspace: str | None = None) -> str | None:
    rows = _safe(lambda: client.list_surfaces(workspace=workspace)) or []
    for row in rows:
        if str(row.get("id")) == surface_id:
            title = row.get("title")
            return None if title is None else str(title)
    return "<absent-from-rows>"


# --------------------------- scenario A --------------------------------


def smoke_parser(cli_bin: Path, fixture: Path, pipe: str) -> None:
    """The refusal grammar runs before any connection, app-less."""
    env = dict(os.environ)
    env["CMUX_SOCKET_PATH"] = pipe
    cases = [
        (
            ["ssh"],
            "cmux ssh: requires a destination (example: cmux ssh user@host)",
        ),
        (["ssh", "--port"], "cmux ssh: --port requires a value"),
        (
            ["ssh", "--port", "99999", "host"],
            "cmux ssh: --port must be 1-65535",
        ),
        (
            ["ssh", "-p", "2222", "host"],
            "cmux ssh: destination must be <user@host>. Use --port/--identity/"
            "--ssh-option for SSH flags and `--` for remote command args.",
        ),
        (["ssh", "--bogus", "host"], "cmux ssh: unknown flag '--bogus'"),
        (["ssh", "host", "remote", "--port"], "cmux ssh: --port requires a value"),
        (["ssh", "host", "-tX"], "cmux ssh: mixed short-option cluster '-tX' is not supported after the destination; use --ssh-option for SSH flags or -- for remote command arguments"),
        (
            ["ssh", "--window", "3", "host"],
            "cmux ssh: --window is not supported by the port",
        ),
        (
            ["ssh", "--transport", "ssh", "host"],
            "cmux ssh: --transport is not supported by the port",
        ),
        (
            ["ssh", "--session", "2", "host"],
            "cmux ssh: --session is not supported by the port",
        ),
        (
            ["ssh", "--command", "a", "host", "echo", "hi"],
            "cmux ssh: --command cannot be combined with trailing remote command arguments",
        ),
    ]
    for index, (argv, expected) in enumerate(cases, start=1):
        try:
            done = subprocess.run(
                [str(cli_bin), *argv],
                env=env,
                capture_output=True,
                timeout=30,
                check=False,
            )
            stderr = done.stderr.decode("utf-8", "backslashreplace").strip()
            check(
                f"parse: refusal #{index} shape",
                done.returncode == 1 and expected == stderr,
                f"rc={done.returncode} stderr={stderr!r}",
            )
        except OSError as error:
            check(f"parse: refusal #{index} shape", False, f"error={error}")


# --------------------------- scenario B --------------------------------


def smoke_verb(
    shell_bin: Path,
    cli_bin: Path,
    fixture: Path,
    pipe: str,
) -> None:
    kill_stray_shells()
    fixture.mkdir(parents=True, exist_ok=True)
    make_config(fixture)
    process, log_path = launch(shell_bin, fixture, pipe)
    print(f"launched pid={process.pid} pipe={pipe}")
    try:
        client = harness.cmux(socket_path=pipe, connect_timeout_s=30.0, timeout_s=5.0)
        ident = poll_until(lambda: _safe(client.identify), 30.0)
        if not check("live: app plane ready", isinstance(ident, dict)):
            raise SystemExit(1)

        baseline_rows = retry_ipc(lambda: client.list_workspaces()) or []
        baseline = len(baseline_rows)
        check("live: fresh workspace baseline", baseline >= 1, f"rows={baseline}")

        # -- value-refused open: connect refused kills the ssh child fast.
        refused_port = grab_free_port()
        refused = retry_ipc(
            lambda: client._call(
                "workspace.ssh.open",
                {"destination": "127.0.0.1", "port": refused_port},
            )
        ) or {}
        refused_workspace = str(refused.get("workspace_id") or "")
        refused_surface = str(refused.get("surface_id") or "")
        check(
            "live: refused open replies with ids",
            bool(refused_workspace and refused_surface),
            f"reply={refused}",
        )
        pane_closed = poll_until(
            lambda: log_has(log_path, f"pane {refused_surface} closed"), 30.0
        )
        check("live: refused child closes the pane", bool(pane_closed))
        reset = poll_until(
            lambda: (len(_safe(lambda: client.list_workspaces()) or []) == baseline),
            10.0,
        )
        check("live: refused workspace closes too", bool(reset))
        check("live: app survives a dead ssh pane", process.poll() is None)

        # -- silent-hold open through the CLI verb (default focus).
        hold_listener, hold_port = silent_listener()
        try:
            open_one = retry_ipc(
                lambda: _run_cli(
                    cli_bin, pipe, ["ssh", "--port", str(hold_port), "127.0.0.1", "--json"]
                )
            )
            first = _cli_json_reply(open_one)
            hold_workspace = str((first or {}).get("workspace_id") or "")
            hold_surface = str((first or {}).get("surface_id") or "")
            check(
                "live: cmux ssh opens a workspace",
                bool(hold_workspace and hold_surface),
                f"out={open_one!r}",
            )
            current = retry_ipc(lambda: client.current_workspace()) or ""
            check(
                "live: ssh open activates its workspace",
                current == hold_workspace,
                f"current={current} opened={hold_workspace}",
            )
            title = poll_until(
                lambda: surface_title(client, hold_surface) == "127.0.0.1", 8.0
            )
            check(
                "live: pane title is the destination",
                bool(title),
                f"title={surface_title(client, hold_surface)!r}",
            )
            held = poll_until(
                lambda: len(_safe(lambda: ssh_children_of(process.pid)) or [])
                >= 1,
                10.0,
            )
            check("live: ssh child holds the pane open", bool(held))

            # -- named hold: --name wins the title, --no-focus keeps the
            #    previously activated workspace active.
            named_listener, named_port = silent_listener()
            try:
                open_two = retry_ipc(
                    lambda: _run_cli(
                        cli_bin,
                        pipe,
                        [
                            "ssh",
                            "--name",
                            "build box",
                            "--no-focus",
                            "--port",
                            str(named_port),
                            "127.0.0.1",
                            "--json",
                        ],
                    )
                )
                second = _cli_json_reply(open_two)
                named_workspace = str((second or {}).get("workspace_id") or "")
                named_surface = str((second or {}).get("surface_id") or "")
                check(
                    "live: --name --no-focus open replies with ids",
                    bool(named_workspace and named_surface),
                    f"out={open_two!r}",
                )
                current = retry_ipc(lambda: client.current_workspace()) or ""
                check(
                    "live: --no-focus keeps the previous workspace active",
                    current == hold_workspace and named_workspace != hold_workspace,
                    f"current={current}",
                )
                title = poll_until(
                    lambda: surface_title(client, named_surface, named_workspace)
                    == "build box",
                    8.0,
                )
                check(
                    "live: --name titles the pane",
                    bool(title),
                    f"title={surface_title(client, named_surface, named_workspace)!r}",
                )
                rows = retry_ipc(lambda: client.list_workspaces()) or []
                check(
                    "live: both silent panes keep their workspaces",
                    len(rows) == baseline + 2,
                    f"rows={len(rows)} baseline={baseline}",
                )
                children = ssh_children_of(process.pid)
                check(
                    "live: exactly two ssh children under the app",
                    len(children) == 2,
                    f"children={children}",
                )
            finally:
                named_listener.close()
        finally:
            hold_listener.close()

        # -- teardown: the app exits cleanly and reaps its ssh children.
        exited = close_gracefully(process)
        code = process.poll()
        check(
            "teardown: wm_close exits cleanly",
            exited and code == 0,
            f"exited={exited} code={code}",
        )
        reaped = poll_until(lambda: not ssh_children_of(process.pid), 10.0)
        check(
            "teardown: the app reaps its ssh children",
            bool(reaped),
            f"left={ssh_children_of(process.pid)}",
        )
    finally:
        if process.poll() is None:
            subprocess.run(
                ["taskkill", "/F", "/PID", str(process.pid)],
                capture_output=True,
                check=False,
            )


def _run_cli(cli_bin: Path, pipe: str, argv: list[str]) -> str | None:
    env = dict(os.environ)
    env["CMUX_SOCKET_PATH"] = pipe
    try:
        done = subprocess.run(
            [str(cli_bin), *argv],
            env=env,
            capture_output=True,
            timeout=30,
            check=False,
        )
    except OSError:
        return None
    if done.returncode != 0:
        return None
    return done.stdout.decode("utf-8", "backslashreplace").strip()


def _cli_json_reply(stdout: str | None) -> dict | None:
    """--json mode prints the full reply envelope; extract its result."""
    if not stdout:
        return None
    try:
        reply = json.loads(stdout.splitlines()[-1])
    except ValueError:
        return None
    result = reply.get("result") if isinstance(reply, dict) else None
    return result if isinstance(result, dict) else None


# --------------------------- main ---------------------------------------


def main() -> int:
    try:
        ctypes.WinDLL("shcore").SetProcessDpiAwareness(2)
    except OSError:
        pass  # already aware or unsupported

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

    cli_default = root / "target" / "debug" / "cmux.exe"
    if not cli_default.exists():
        cli_default = root / "target" / "release" / "cmux.exe"
    cli_bin = cli_default
    assert cli_bin.exists(), f"cmux.exe not found at {cli_bin} (build first)"

    fixture = Path(tempfile.mkdtemp(prefix="cmux-ssh-m6-"))
    pipe = rf"\\.\pipe\cmux-ssh-m6-{os.getpid()}-{random.randint(100, 999)}"

    print(f"== M6 SSH lane: {shell_bin} ==")
    smoke_parser(cli_bin, fixture, pipe)
    smoke_verb(shell_bin, cli_bin, fixture, pipe)
    print(f"\nSUMMARY pass={len(PASS)} fail={len(FAIL)}")
    if FAIL:
        print("FAILED:")
        for name in FAIL:
            print(f"  {name}")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
