"""M4.2 live smoke: password-gate serving over the automation surface.

Hermetic like the M4.1 smoke: unique `CMUX_SOCKET_PATH` overrides, isolated
session files, and a missing (default-producing) config file. Two
`cmux-shell.exe` instances are launched:

  A. `CMUX_SOCKET_MODE=password` + `CMUX_SOCKET_PASSWORD` (the env source;
     the DPAPI file stays untouched)
  B. `CMUX_SOCKET_MODE=automation` (upstream `.automation`: same-user DACL,
     no ancestry gate, no password)

No auto-exit env is set: the apps stay up until teardown, which kills the
root processes — their per-pane job objects collapse every shell
descendant.

Checks (each must pass):

  Password mode A:
    - unauthenticated ping -> structured `auth_required`
    - wrong password       -> `auth_failed` ("Invalid password")
    - missing params.password -> `invalid_params`
    - correct password client: login-then-call on one connection works
    - capabilities carries `auth.login` (served authenticated)
  Automation mode B:
    - client with resolved password still serves (the non-password
      `auth.login` ack: authenticated+required:false)
    - client without a password serves (no ancestry gate live here)
    - CLI with `-password` ignored-ish: tolerated and answered
  CLI (the upstream harness contract):
    - `--socket <pipe> --password wrong ping`   -> exit 1, Invalid password
    - `--socket <pipe> --password right ping`   -> exit 0, pong on stdout
    - password-less `ping` (no sources)         -> exit 0 (B)

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
    end = time.monotonic() + deadline_s
    last = None
    while time.monotonic() < end:
        last = fn()
        if last:
            return last
        time.sleep(interval_s)
    return last


def launch(shell_bin: Path, fixture: Path, env: dict) -> subprocess.Popen:
    app_log = open(fixture / "app-stderr.log", "ab")
    process = subprocess.Popen(
        [str(shell_bin)],
        cwd=str(fixture),
        env=env,
        stdout=subprocess.DEVNULL,
        stderr=app_log,
    )
    return process


def teardown(process: subprocess.Popen, fixture: Path) -> None:
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
    shutil.rmtree(fixture, ignore_errors=True)


def run_cli(cli_bin: Path, args: list[str], extra_env: dict | None = None):
    env = dict(os.environ)
    if extra_env is not None:
        env.update(extra_env)
    completed = subprocess.run(
        [str(cli_bin), *args],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=env,
        timeout=30,
    )
    return (
        completed.returncode,
        completed.stdout.decode("utf-8", errors="replace"),
        completed.stderr.decode("utf-8", errors="replace"),
    )


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
    cli_bin = shell_bin.with_name("cmux.exe")
    assert cli_bin.exists(), f"cmux.exe not found at {cli_bin} (build the CLI first)"

    pipe_password = rf"\\.\pipe\cmux-m42-pw-{os.getpid()}-{random.randint(1000, 9999)}"
    pipe_automation = rf"\\.\pipe\cmux-m42-auto-{os.getpid()}-{random.randint(1000, 9999)}"

    fixture_password = Path(tempfile.mkdtemp(prefix="cmux-m42-pw-"))
    fixture_automation = Path(tempfile.mkdtemp(prefix="cmux-m42-auto-"))

    env_password = dict(os.environ)
    env_password["CMUX_SOCKET_PATH"] = pipe_password
    env_password["CMUX_SOCKET_MODE"] = "password"
    env_password["CMUX_SOCKET_PASSWORD"] = "secret"
    env_password["CMUX_SESSION_FILE"] = str(fixture_password / "session.json")
    env_password["CMUX_CONFIG_FILE"] = str(fixture_password / "config.json5")
    env_password.pop("CMUX_PASSWORD_FILE", None)
    env_password.pop("CMUX_IPC_DISABLED", None)
    env_password.pop("CMUX_SHELL_AUTO_EXIT_MS", None)
    env_password.pop("CMUX_CLAUDE_HOOKS_DISABLED", None)

    env_automation = dict(env_password)
    env_automation["CMUX_SOCKET_PATH"] = pipe_automation
    env_automation["CMUX_SOCKET_MODE"] = "automation"
    env_automation.pop("CMUX_SOCKET_PASSWORD", None)
    env_automation["CMUX_SESSION_FILE"] = str(fixture_automation / "session.json")
    env_automation["CMUX_CONFIG_FILE"] = str(fixture_automation / "config.json5")

    pw_process = launch(shell_bin, fixture_password, env_password)
    auto_process = launch(shell_bin, fixture_automation, env_automation)
    print(f"launched password-mode pid={pw_process.pid} pipe={pipe_password}")
    print(f"launched automation-mode pid={auto_process.pid} pipe={pipe_automation}")

    pw_authed = None
    pw_plain = None
    auto_client = None
    auto_plain = None
    try:
        # --- password mode A ---
        pw_plain = harness.cmux(
            socket_path=pipe_password, connect_timeout_s=30.0, timeout_s=5.0
        )

        def _unauthenticated_refused():
            try:
                pw_plain._call("system.ping")
            except cmuxError as error:
                if error.code == "auth_required":
                    return True
                return None
            except OSError:
                return None

        check("unauthenticated ping is refused", poll_until(_unauthenticated_refused, 30.0) is True)

        def _wrong_password_fails():
            try:
                pw_plain._call("auth.login", {"password": "nope"})
            except cmuxError as error:
                if error.code == "auth_failed":
                    return error
            except OSError:
                pass
            return None

        refused = poll_until(_wrong_password_fails, 30.0)
        check(
            "wrong password answers auth_failed",
            bool(refused) and "Invalid password" in str(refused),
            f"error={refused}",
        )

        def _missing_param_rejected():
            try:
                pw_plain._call("auth.login", {})
            except cmuxError as error:
                if error.code == "invalid_params":
                    return error
            except OSError:
                pass
            return None

        missing = poll_until(_missing_param_rejected, 30.0)
        check(
            "missing params.password rejects as invalid_params",
            bool(missing) and "params.password" in str(missing),
            f"error={missing}",
        )

        pw_authed = harness.cmux(
            socket_path=pipe_password, connect_timeout_s=30.0, timeout_s=5.0, password="secret"
        )
        check("authenticated ping serves", poll_until(lambda: _safe(pw_authed.ping), 30.0) is True)

        caps = _safe(lambda: pw_authed.capabilities()) or {}
        check(
            "capabilities advertise auth.login",
            "auth.login" in (caps.get("methods") or []),
            f"caps={caps}",
        )

        def _correct_cli_ping():
            code, out, err = run_cli(
                cli_bin,
                ["--socket", pipe_password, "--password", "secret", "ping"],
            )
            return code == 0 and '"pong":true' in out

        check("CLI pings with the right password", poll_until(_correct_cli_ping, 30.0) is True)

        code, _out, err = run_cli(
            cli_bin,
            ["--socket", pipe_password, "--password", "wrong-password", "ping"],
        )
        check(
            "CLI ping with a wrong password fails with exit 1",
            code == 1 and "Invalid password" in err,
            f"code={code} err={err.strip()!r}",
        )

        code, _out, err = run_cli(
            cli_bin,
            ["--socket", pipe_password, "--password", "", "ping"],
            extra_env={"CMUX_SOCKET_PASSWORD": "", "CMUX_PASSWORD_FILE": ""},
        )
        check(
            "CLI ping without any password in password mode fails (auth_required)",
            code == 1,
            f"code={code} err={err.strip()!r}",
        )

        # --- automation mode B (upstream `.automation`: same-user, no gate,
        # no password; login probes answer the non-password ack) ---
        auto_plain = harness.cmux(
            socket_path=pipe_automation, connect_timeout_s=30.0, timeout_s=5.0
        )
        check(
            "automation mode serves without login",
            poll_until(lambda: _safe(auto_plain.ping), 30.0) is True,
        )

        def _ack_login_probe():
            try:
                return auto_plain._call("auth.login", {"password": "unused"}) or {}
            except (cmuxError, OSError):
                return None

        ack = poll_until(_ack_login_probe, 30.0)
        check(
            "auth.login in a non-password mode acks required:false",
            bool(ack) and ack.get("authenticated") is True and ack.get("required") is False,
            f"ack={ack}",
        )

        auto_authed = harness.cmux(
            socket_path=pipe_automation, connect_timeout_s=30.0, timeout_s=5.0, password="ignored"
        )
        check(
            "client with a resolved password still serves (CLI tolerance shape)",
            poll_until(lambda: _safe(auto_authed.ping), 30.0) is True,
        )

        code, out, _err = run_cli(
            cli_bin,
            ["--socket", pipe_automation, "--password", "ignored-in-automation", "ping"],
        )
        check(
            "CLI --password ping succeeds in automation mode (upstream harness pin)",
            code == 0 and '"pong":true' in out,
            f"code={code} out={out.strip()!r}",
        )

        code, out, _err = run_cli(
            cli_bin,
            ["--socket", pipe_automation, "ping"],
            extra_env={"CMUX_SOCKET_PASSWORD": "", "CMUX_PASSWORD_FILE": ""},
        )
        check(
            "CLI ping without any password in automation mode succeeds",
            code == 0 and '"pong":true' in out,
            f"code={code} out={out.strip()!r}",
        )

    finally:
        teardown(pw_process, fixture_password)
        teardown(auto_process, fixture_automation)
        print()
        if FAIL:
            pass

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
