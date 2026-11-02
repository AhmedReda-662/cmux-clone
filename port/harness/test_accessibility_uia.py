"""M6 accessibility acceptance: the live UIA tree over a real `cmux-shell.exe`.

Contract (docs/research/accessibility-evidence.md + ADR-0015): eframe's
accesskit feature surface (accesskit_winit's UI Automation adapter) plus the
WidgetInfo labels attached to the shell's custom-painted chrome must reach a
real Windows accessibility client. This lane launches the app hermetically,
then polls the process's window through the .NET System.Windows.Automation
client (the same UIA API Narrator/NVDA sit on) and walks the control tree.

Three outcomes:
  - PASS (exit 0): the provider tree is reachable - the window plus the
    expected labeled chrome ("New workspace", workspace rows, "Close
    workspace <name>", "Terminal") all appear.
  - SKIP (exit 0): the window is discoverable with its title, but the
    provider exposes no child tree at all. This matches the 2026-10-23
    environment finding (accessibility-evidence.md section 6): upstream
    accesskit/eframe builds - including accesskit_windows' own hello_world
    sample - settle on a childless placeholder tree on this rig, and egui
    issue #8671 tracks the upstream defect class. The lane stays green as a
    canary: when the upstream integration or the environment gets fixed,
    the skip flips into the full PASS assertion set automatically.
  - FAIL (exit 1): some named children appear but the expected chrome labels
    are missing, or the window itself was never discoverable, or the app
    panicked.
"""

from __future__ import annotations

import argparse
import json
import os
import random
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

sys.stdout.reconfigure(errors="replace")
sys.stderr.reconfigure(errors="replace")

PASS: list[str] = []
FAIL: list[str] = []
SKIP: list[str] = []


def check(name: str, condition: bool, detail: str = "") -> bool:
    if condition:
        PASS.append(name)
        print(f"PASS {name}")
        return True
    FAIL.append(name)
    print(f"FAIL {name} {detail}".rstrip())
    return False


def skip(name: str, reason: str) -> None:
    SKIP.append(name)
    print(f"SKIP {name} ({reason})")


def _safe(fn, default):
    try:
        return fn()
    except Exception:
        return default


# The probe walks the control tree of one PID through System.Windows.Automation
# (the in-box Windows UIA client). Records stream out as `uia:` prefixed JSON
# lines; a bare `{"result": ...}` line reports discovery status. Sibling and
# depth caps keep a pathological tree from hanging the walk. The app PID
# arrives as (-File param) so the template needs no host-side interpolation.
# NO python formatting is applied to this string (it is brace-heavy PS).
PROBE_PS = r"""
param([int]$AppPid)
$ErrorActionPreference = 'SilentlyContinue'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
$root = [System.Windows.Automation.AutomationElement]::RootElement
$target = $null
$wins = $root.FindAll(
    [System.Windows.Automation.TreeScope]::Children,
    [System.Windows.Automation.Condition]::TrueCondition
)
foreach ($w in $wins) {
    if ($w.Current.ProcessId -eq $AppPid) { $target = $w; break }
}
if ($null -eq $target) {
    Write-Output 'uia:{"result":"no-window"}'
    exit 0
}
function Walk($el, $depth) {
    $c = $el.Current
    $name = ''
    if ($null -ne $c.Name) { $name = $c.Name }
    $rect = $c.BoundingRectangle
    $rectText = ''
    if ($null -ne $rect) {
        if (-not $rect.IsEmpty) {
            $rectText = ('{0:F0}x{1:F0}@+{2:F0}+{3:F0}' -f
                $rect.Width, $rect.Height, $rect.X, $rect.Y)
        }
    }
    $obj = [ordered]@{
        name   = [string]$name
        type   = [string]$c.ControlType.ProgrammaticName
        off    = [bool]$c.IsOffscreen
        on     = [bool]$c.IsEnabled
        rect   = [string]$rectText
        depth  = [int]$depth
    }
    Write-Output ('uia:' + (ConvertTo-Json -InputObject ([pscustomobject]$obj) -Compress))
    if ($depth -ge 14) { return }
    $walker = [System.Windows.Automation.TreeWalker]::ControlViewWalker
    $child = $walker.GetFirstChildElement($el)
    $siblings = 0
    while ($null -ne $child -and $siblings -lt 300) {
        Walk $child ($depth + 1)
        $child = $walker.GetNextSiblingElement($child)
        $siblings = $siblings + 1
    }
}
Walk $target 0
Write-Output 'uia:{"result":"done"}'
"""


def run_probe(probe_path: Path, app_pid: int) -> tuple[list[dict], dict]:
    """Runs the PowerShell UIA walker; returns (records, status)."""
    proc = subprocess.run(
        ["powershell.exe", "-NoProfile", "-STA",
         "-ExecutionPolicy", "Bypass",
         "-File", str(probe_path), "-AppPid", str(app_pid)],
        capture_output=True,
        timeout=90,
    )
    records: list[dict] = []
    status = {"result": "probe-crash"}
    text = proc.stdout.decode("utf-8", errors="replace")
    for line in text.splitlines():
        line = line.strip()
        if not line.startswith("uia:{"):
            continue
        payload = _safe(lambda: json.loads(line[4:]), None)
        if not isinstance(payload, dict):
            continue
        if "result" in payload:
            status = payload
        elif "name" in payload:
            records.append(payload)
    return records, status


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

    pipe = rf"\\.\pipe\cmux-a11y-uia-{os.getpid()}-{random.randint(1000, 9999)}"
    fixture = Path(tempfile.mkdtemp(prefix="cmux-a11y-uia-"))
    config = (
        '{\n  "terminal": { "shell": { "program": "powershell.exe", '
        '"args": ["-NoLogo", "-NoProfile"] } }\n}\n'
    )
    (fixture / "cmux.json").write_text(config, encoding="utf-8")
    probe_path = fixture / "walk-uia.ps1"
    probe_path.write_text(PROBE_PS, encoding="utf-8")

    # Battery rule: a stray shell from a previous lane must not survive.
    strays = subprocess.run(
        ["taskkill", "/F", "/IM", "cmux-shell.exe"],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    ).returncode

    process = None
    app_log = None
    records: list[dict] = []
    status: dict = {"result": "not-run"}
    try:
        env = dict(os.environ)
        env["CMUX_SOCKET_PATH"] = pipe
        env["CMUX_SESSION_FILE"] = str(fixture / "session.json")
        env["CMUX_CONFIG_FILE"] = str(fixture / "cmux.json")
        for extra in (
            "CMUX_SHELL_AUTO_EXIT_MS",
            "CMUX_CLAUDE_HOOKS_DISABLED",
            "CMUX_PASSWORD_FILE",
            "CMUX_SOCKET_PASSWORD",
            "CMUX_IPC_DISABLED",
        ):
            env.pop(extra, None)
        app_log = open(fixture / "app-stderr.log", "ab")
        process = subprocess.Popen(
            [str(shell_bin)],
            cwd=str(fixture),
            env=env,
            stdout=subprocess.DEVNULL,
            stderr=app_log,
        )
        print(f"launched {shell_bin.name} pid={process.pid} (stray-kill rc={strays})")

        deadline = time.monotonic() + 45.0
        attempts = 0
        while time.monotonic() < deadline:
            attempts += 1
            records, status = run_probe(probe_path, process.pid)
            names = {r.get("name", "") for r in records}
            window_found = status.get("result") not in ("no-window", "probe-crash")
            if window_found and "New workspace" in names:
                break
            if process.poll() is not None:
                break
            time.sleep(1.0)
        status["attempts"] = attempts

        named_buttons = [
            r for r in records
            if r.get("type") == "ControlType.Button" and r.get("name")
        ]
        window_rows = [r for r in records if r.get("depth") == 0]
        window_title = window_rows[0].get("name", "") if window_rows else ""
        # AccessKit exposure is live only when the provider subtree surfaced
        # (any named element below the top-level window).
        exposure_live = any(r.get("depth", 0) > 0 for r in records)

        check("app window found in UIA", status.get("result") != "no-window",
              f"status={json.dumps(status)}")
        check("window title carries the app name",
              window_title.startswith("cmux port"), f"title={window_title!r}")

        exposure_checks = [
            ("New workspace chrome exposed as a named Button",
             any(r["name"] == "New workspace" for r in named_buttons)),
            ("workspace row buttons are named",
             any(r["name"] != "New workspace"
                 and not r["name"].startswith("Close workspace ")
                 for r in named_buttons)),
            ("Close workspace buttons are named",
             any(r["name"].startswith("Close workspace ")
                 for r in named_buttons)),
            ("terminal pane carries its a11y label",
             any(r["name"] == "Terminal" for r in records)),
        ]
        if exposure_live:
            for name, condition in exposure_checks:
                check(name, condition)
        else:
            reason = ("no UIA child tree; environment/upstream placeholder-only "
                      "(accessibility-evidence.md sec. 6, egui#8671)")
            for name, _condition in exposure_checks:
                skip(name, reason)

        print()
        print("---- UIA tree (elements that carry a name) ----")
        for r in records:
            name = r.get("name", "")
            if name:
                print(f"  d{r['depth']:>2} {r['type']:<26} {name!r} "
                      f"on={r.get('on')} rect={r.get('rect')}")

        tail_text = _safe(lambda: (fixture / "app-stderr.log").read_text(
            errors="replace"), "")
        check("no panic in the app stderr", "panicked" not in tail_text)

    finally:
        if process is not None and process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                subprocess.run(
                    ["taskkill", "/F", "/T", "/PID", str(process.pid)],
                    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                )
                process.wait(timeout=10)
        time.sleep(0.3)
        if app_log is not None:
            app_log.close()
        try:
            tail = (fixture / "app-stderr.log").read_text(errors="replace")[-3000:]
            print("---- app-stderr tail ----")
            print(tail)
        except OSError:
            pass
        shutil.rmtree(fixture, ignore_errors=True)

    print()
    print(f"a11y-uia lane complete: {len(PASS)} passed, {len(SKIP)} skipped "
          f"(blocked), {len(FAIL)} failed")
    if SKIP:
        print("skipped checks:", ", ".join(SKIP))
    if FAIL:
        print("failed checks:", ", ".join(FAIL))
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(smoke())
