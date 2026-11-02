"""M6 live smoke: session save/restore end to end.

M2.4 landed the snapshot write/read machinery with unit tests, and the M5.1
lane (test_browser_m51.py) already proves the browser-leaf restart leg. What
stayed unverified end to end is the M6 gate this lane closes: a real
close-save-relaunch cycle with live terminal panes, the persisted tree
shape, the active workspace, the focused pane, and the failure paths a
broken store must survive (fresh start, per-workspace skip, -previous
fallback, restore-smoke never writing).

Scenarios:

  A. round trip (two launches, one store)
     - first run: three panes (env-driven split-right then split-down),
       every pane a live shell (marker echo per surface), plus a second
       workspace with its own live marker; WM_CLOSE saves and exits
     - snapshot checks: version 1, one window, two workspaces, active
       index 1, split shape (horizontal top with a nested vertical on the
       split side), terminal panels, divider fractions in range
     - second run restores with the same workspace ids and pane ids, the
       active index lands on the second workspace, the saved focused pane
       owns the focus flag, and every restored pane spawns a live shell
     - second close: the re-saved tree structure is stable, divider
       geometry stays close, and the run1 snapshot rotated into -previous

  B. store resilience (each case has its own fixture)
     - truncated store: unusable log line, the app starts fresh with one
       workspace and one pane, and still exits cleanly
     - one unrestorable workspace (unknown split orientation) beside a
       valid one: only the broken one is skipped; the valid one restores
       with its own pane ids and the active index clamps to the survivor
     - CMUX_SHELL_RESTORE_SMOKE=1: the snapshot restores (log line) and
       the store bytes never change (restore mode never saves)
     - broken primary beside a valid -previous backup: the backup wins
       (its workspace and pane ids come back)

No pytest, no extra deps (ctypes only), the same assert-script style as the
other lanes. Hermetic: unique pipes, temp fixtures, a config that pins
`powershell.exe -NoLogo -NoProfile`. Exit 0 only when every check printed
PASS.
"""

from __future__ import annotations

import argparse
import ctypes
import ctypes.wintypes as wt
import hashlib
import json
import os
import random
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


def taskkill_pid(pid: int) -> None:
    subprocess.run(
        ["taskkill", "/F", "/PID", str(pid)], capture_output=True, check=False
    )


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


# --------------------------- launch + store ----------------------------


def launch(
    shell_bin: Path,
    fixture: Path,
    pipe: str,
    extra_env: dict | None = None,
):
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
    if extra_env:
        env.update(extra_env)
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


def wait_for_log(
    needle: str, log_path: Path, name: str, deadline_s: float = 20.0
) -> bool:
    return check(
        name,
        bool(poll_until(lambda: log_has(log_path, needle), deadline_s)),
        f"needle={needle!r} log={log_path}",
    )


def eat_snapshot(path: Path) -> dict:
    """Parses the session store; {} on any error (checks then fail loudly)."""
    try:
        raw = path.read_text(encoding="utf-8")
    except OSError as error:
        print(f"store read failed for {path.name}: {error}")
        return {}
    try:
        return json.loads(raw)
    except ValueError as error:
        print(f"store parse failed for {path.name}: {error}")
        return {}


def collect_leaf_ids(node, out: list[str]) -> None:
    """Leaf panel ids in spatial traversal order."""
    if not isinstance(node, dict):
        return
    if node.get("type") == "pane" and isinstance(node.get("pane"), dict):
        out.extend(node["pane"].get("panelIds") or [])
        return
    split = node.get("split")
    if isinstance(split, dict):
        collect_leaf_ids(split.get("first"), out)
        collect_leaf_ids(split.get("second"), out)


def split_nodes(node) -> list[dict]:
    result: list[dict] = []
    if not isinstance(node, dict):
        return result
    if node.get("type") == "split" and isinstance(node.get("split"), dict):
        split = node["split"]
        result.append(split)
        result.extend(split_nodes(split.get("first")))
        result.extend(split_nodes(split.get("second")))
    return result


def shape_of(node):
    """Structure signature: kinds, orientations, ordered leaf ids.

    Divider fractions are geometry and may drift a hair while the frame
    loop re-balances pixels; they are compared separately with a
    tolerance.
    """
    if not isinstance(node, dict):
        return None
    if node.get("type") == "pane" and isinstance(node.get("pane"), dict):
        pane = node["pane"]
        return ("pane", tuple(pane.get("panelIds") or []), pane.get("selectedPanelId"))
    split = node.get("split")
    if not isinstance(split, dict):
        return None
    return (
        "split",
        split.get("orientation"),
        shape_of(split.get("first")),
        shape_of(split.get("second")),
    )


def workspace_map(snapshot: dict) -> dict[str, dict]:
    out: dict[str, dict] = {}
    for window in snapshot.get("windows") or []:
        manager = (window or {}).get("tabManager") or {}
        for workspace in manager.get("workspaces") or []:
            workspace = workspace or {}
            out[str(workspace.get("workspaceId", ""))] = workspace
    return out


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


# --------------------------- crafted snapshots -------------------------

# Hand-built snapshots mirror the wire schema the app writes (upstream
# `type`-tagged layout nodes; Restore keeps id strings verbatim).


def crafted_workspace(
    workspace_id: str, layout: dict, panel_ids: list[str], focused: str | None = None
) -> dict:
    if focused is None:
        focused = panel_ids[0] if panel_ids else None
    return {
        "workspaceId": workspace_id,
        "processTitle": "cmux",
        "customTitle": None,
        "customColor": None,
        "currentDirectory": "",
        "focusedPanelId": focused,
        "layout": layout,
        "panels": [
            {
                "id": pid,
                "type": "terminal",
                "title": None,
                "customTitle": None,
                "directory": None,
            }
            for pid in panel_ids
        ],
    }


def crafted_leaf(panel_ids: list[str]) -> dict:
    selected = panel_ids[0] if panel_ids else None
    return {
        "type": "pane",
        "pane": {"panelIds": list(panel_ids), "selectedPanelId": selected},
    }


def crafted_split(orientation: str, first: dict, second: dict, divider: float = 0.5) -> dict:
    return {
        "type": "split",
        "split": {
            "orientation": orientation,
            "dividerPosition": divider,
            "first": first,
            "second": second,
        },
    }


def crafted_snapshot(workspaces: list[dict], selected: int = 0) -> str:
    return json.dumps(
        {
            "version": 1,
            "createdAt": time.time(),
            "windows": [
                {
                    "windowId": None,
                    "tabManager": {
                        "selectedWorkspaceIndex": selected,
                        "workspaces": workspaces,
                    },
                }
            ],
        }
    )


def marker_round_trip(client, surface_id: str, tag: str) -> bool:
    """Sends an echo marker into one surface and watches it return.

    Restore verification only counts when the restored pane is a shell we
    can type into again - the same bar every other lane uses.
    """
    client.send(f"echo cmux-restore-{tag}\n", surface=surface_id)
    return bool(
        poll_until(
            lambda: tag in (_safe(lambda: client.read_text(surface=surface_id)) or ""),
            12.0,
        )
    )


# --------------------------- scenario A --------------------------------


def smoke_a(shell_bin: Path) -> None:
    kill_stray_shells()
    fixture = Path(tempfile.mkdtemp(prefix="cmux-restore-m6-a-"))
    make_config(fixture)
    pipe_one = rf"\\.\pipe\cmux-restore-m6-r1-{os.getpid()}-{random.randint(100, 999)}"
    pipe_two = rf"\\.\pipe\cmux-restore-m6-r2-{os.getpid()}-{random.randint(100, 999)}"
    store = fixture / "session.json"
    client = None
    leaves: dict[str, list[str]] = {}
    saved_focused: dict[str, object] = {}
    dividers_one: list = []
    tree_saved = None
    saved_created_at = None

    # -------- first run: three panes + a second workspace --------
    process, _log_one = launch(
        shell_bin, fixture, pipe_one, extra_env={"CMUX_SHELL_AUTO_SPLIT_MS": "1500"}
    )
    print(f"launched run1 pid={process.pid} pipe={pipe_one}")
    try:
        client = harness.cmux(
            socket_path=pipe_one, connect_timeout_s=30.0, timeout_s=5.0
        )
        ident = poll_until(lambda: _safe(client.identify), 30.0)
        if not check("r1: app plane ready", isinstance(ident, dict)):
            raise SystemExit(1)

        panes = poll_until(
            lambda: rows
            if (rows := retry_ipc(lambda: client.list_panes()) or [])
            and len(rows) >= 3
            else None,
            20.0,
        ) or []
        pane_ids = [str(row.get("id")) for row in panes]
        check(
            "r1: auto splits build three panes",
            len(pane_ids) == 3 and len(set(pane_ids)) == 3,
            f"panes={pane_ids}",
        )

        results = [
            (pane_id, marker_round_trip(client, pane_id, f"r1-p{index}"))
            for index, pane_id in enumerate(pane_ids)
        ]
        check(
            "r1: every fresh pane is a live shell",
            all(result for _, result in results),
            f"results={results}",
        )

        created = retry_ipc(lambda: client.new_workspace()) or {}
        workspace_two = str(created.get("workspace_id") or "")
        surface_two = str(created.get("surface_id") or "")
        check(
            "r1: second workspace created live",
            bool(workspace_two and surface_two),
            f"created={created}",
        )
        check(
            "r1: second workspace is a live shell",
            marker_round_trip(client, surface_two, "r1-ws2"),
        )
        current = retry_ipc(lambda: client.current_workspace()) or ""
        check(
            "r1: create switches to the new workspace",
            current == workspace_two,
            f"current={current} created={workspace_two}",
        )

        exited = close_gracefully(process)
        code = process.poll()
        check(
            "r1: wm_close saves and exits cleanly",
            exited and code == 0,
            f"exited={exited} code={code}",
        )

        snapshot_one = eat_snapshot(store)
        if not check(
            "r1: snapshot written", bool(snapshot_one), f"store={store}"
        ):
            raise SystemExit(1)
        check(
            "r1: snapshot version 1",
            snapshot_one.get("version") == 1,
            f"version={snapshot_one.get('version')}",
        )
        windows = snapshot_one.get("windows") or []
        manager = ((windows or [{}])[0] or {}).get("tabManager") or {}
        workspaces = manager.get("workspaces") or []
        check(
            "r1: one window with two workspaces",
            len(windows) == 1 and len(workspaces) == 2,
            f"windows={len(windows)} workspaces={len(workspaces)}",
        )
        check(
            "r1: active index saved as 1 (workspace two)",
            manager.get("selectedWorkspaceIndex") == 1,
            f"selected={manager.get('selectedWorkspaceIndex')}",
        )
        leaves = {
            wid: _leaves_of(ws) for wid, ws in workspace_map(snapshot_one).items()
        }
        check(
            "r1: saved workspace ids are w1 + w2",
            list(leaves) == ["w1", "w2"],
            f"ids={list(leaves)}",
        )
        check(
            "r1: w1 saved three leaf panes",
            len(leaves.get("w1", [])) == 3,
            f"leaves={leaves.get('w1')}",
        )
        check(
            "r1: w2 saved one leaf pane",
            len(leaves.get("w2", [])) == 1,
            f"leaves={leaves.get('w2')}",
        )

        tree_one = (workspace_map(snapshot_one).get("w1") or {}).get("layout") or {}
        top = tree_one.get("split") if tree_one.get("type") == "split" else None
        if not check(
            "r1: w1 tree is a split", isinstance(top, dict), f"layout={tree_one}"
        ):
            raise SystemExit(1)
        check(
            "r1: top split is horizontal (split-right)",
            top.get("orientation") == "horizontal",
            f"orientation={top.get('orientation')}",
        )
        nested = top.get("second") or {}
        check(
            "r1: the second child holds a nested split (split-down)",
            nested.get("type") == "split"
            and (nested.get("split") or {}).get("orientation") == "vertical",
            f"second={nested}",
        )
        dividers_one = [
            node.get("dividerPosition") for node in split_nodes(tree_one)
        ]
        check(
            "r1: divider fractions in range",
            all(
                isinstance(div, (int, float)) and div == div and 0.04 <= div <= 0.96
                for div in dividers_one
            ),
            f"dividers={dividers_one}",
        )
        panels_ok = all(
            panel.get("type") == "terminal"
            for workspace in workspaces
            for panel in workspace.get("panels", [])
        )
        check("r1: all saved panels are terminal", panels_ok)
        tree_saved = shape_of(tree_one)
        saved_focused = {
            wid: ws.get("focusedPanelId")
            for wid, ws in workspace_map(snapshot_one).items()
        }
        saved_created_at = snapshot_one.get("createdAt")
    finally:
        if process.poll() is None:
            taskkill_pid(process.pid)
        if client is not None:
            try:
                client.close()
            except OSError:
                pass

    # -------- second run: same store, restored --------
    kill_stray_shells()
    process, _log_two = launch(shell_bin, fixture, pipe_two, extra_env=None)
    print(f"launched run2 pid={process.pid} pipe={pipe_two}")
    try:
        client = harness.cmux(
            socket_path=pipe_two, connect_timeout_s=30.0, timeout_s=5.0
        )
        ident = poll_until(lambda: _safe(client.identify), 30.0)
        if not check("r2: app plane ready", isinstance(ident, dict)):
            raise SystemExit(1)

        rows = retry_ipc(lambda: client.list_workspaces()) or []
        ids = [str(row.get("id")) for row in rows]
        check(
            "r2: restored workspace ids preserved",
            ids == ["w1", "w2"],
            f"ids={ids}",
        )
        current = retry_ipc(lambda: client.current_workspace()) or ""
        check(
            "r2: active index restored (w2 current)",
            current == "w2",
            f"current={current}",
        )
        ws2_leaves = [str(x) for x in leaves.get("w2", [])]
        got = [
            str(row.get("id"))
            for row in retry_ipc(lambda: client.list_panes("w2")) or []
        ]
        check(
            "r2: w2 pane ids preserved",
            got == ws2_leaves,
            f"got={got} saved={ws2_leaves}",
        )

        retry_ipc(lambda: client.select_workspace("w1"))
        pane_rows = poll_until(
            lambda: rows
            if (rows := retry_ipc(lambda: client.list_panes("w1")) or [])
            and len(rows) == 3
            else None,
            15.0,
        ) or []
        got = [str(row.get("id")) for row in pane_rows]
        check(
            "r2: w1 three panes restored with their ids",
            got == [str(x) for x in leaves.get("w1", [])],
            f"got={got} saved={leaves.get('w1')}",
        )
        focused_id = str(saved_focused.get("w1") or "")
        focused_rows = [row for row in pane_rows if row.get("focused")]
        check(
            "r2: restored focus lands on the saved focused pane",
            len(focused_rows) == 1 and str(focused_rows[0].get("id")) == focused_id,
            f"rows={[(row.get('id'), row.get('focused')) for row in pane_rows]} "
            f"saved={focused_id}",
        )
        results = [
            (pane_id, marker_round_trip(client, pane_id, f"r2-p{index}"))
            for index, pane_id in enumerate(got)
        ]
        check(
            "r2: every restored pane spawns a live shell",
            all(result for _, result in results),
            f"results={results}",
        )
        retry_ipc(lambda: client.select_workspace("w2"))
        check(
            "r2: restored second workspace is a live shell",
            bool(ws2_leaves) and marker_round_trip(client, ws2_leaves[0], "r2-ws2"),
        )

        exited = close_gracefully(process)
        code = process.poll()
        check(
            "r2: wm_close exits cleanly",
            exited and code == 0,
            f"exited={exited} code={code}",
        )
        snapshot_two = eat_snapshot(store)
        if not check("r2: re-save happened", bool(snapshot_two)):
            raise SystemExit(1)
        tree_two = (workspace_map(snapshot_two).get("w1") or {}).get("layout") or {}
        check(
            "r2: re-saved tree structure stable",
            shape_of(tree_two) == tree_saved,
            f"saved={tree_saved}\n  resaved={shape_of(tree_two)}",
        )
        dividers_two = [
            node.get("dividerPosition") for node in split_nodes(tree_two)
        ]
        check(
            "r2: divider geometry close to the saved one",
            len(dividers_two) == len(dividers_one)
            and all(
                isinstance(a, (int, float))
                and isinstance(b, (int, float))
                and abs(a - b) <= 0.05
                for a, b in zip(dividers_two, dividers_one)
            ),
            f"saved={dividers_one} resaved={dividers_two}",
        )
        workspaces_two = (
            ((snapshot_two.get("windows") or [{}])[0].get("tabManager") or {}).get(
                "workspaces"
            )
            or []
        )
        check(
            "r2: workspace count stayed two",
            len(workspaces_two) == 2,
            f"count={len(workspaces_two)}",
        )
        round_focus = all(
            ws.get("focusedPanelId") in [str(x) for x in _leaves_of(ws)]
            for ws in workspaces_two
        )
        check(
            "r2: focus ids still name leaves after round trip",
            round_focus,
        )
        previous = fixture / "session.json-previous"
        previous_snapshot = eat_snapshot(previous)
        check(
            "r2: backup rotation published the run1 snapshot",
            previous.is_file()
            and previous_snapshot.get("version") == 1
            and previous_snapshot.get("createdAt") == saved_created_at,
            f"previous={previous} exists={previous.is_file()} "
            f"prevCreatedAt={previous_snapshot.get('createdAt')} "
            f"r1CreatedAt={saved_created_at}",
        )
    finally:
        if process.poll() is None:
            taskkill_pid(process.pid)
        if client is not None:
            try:
                client.close()
            except OSError:
                pass


def _leaves_of(workspace: dict) -> list[str]:
    out: list[str] = []
    collect_leaf_ids((workspace or {}).get("layout"), out)
    return out


# --------------------------- scenario B --------------------------------


def smoke_b(shell_bin: Path) -> None:
    root = Path(tempfile.mkdtemp(prefix="cmux-restore-m6-b-"))

    # ---- b1: truncated store starts fresh ----
    kill_stray_shells()
    fixture = root / "b1-unusable"
    fixture.mkdir()
    make_config(fixture)
    (fixture / "session.json").write_bytes(b'{"version": 1, "windows": [  {"t')
    process, log_path = launch(
        shell_bin,
        fixture,
        rf"\\.\pipe\cmux-restore-b1-{os.getpid()}-{random.randint(100, 999)}",
        extra_env=None,
    )
    print(f"b1 launched pid={process.pid}")
    try:
        wait_for_log(
            "session snapshot unusable",
            log_path,
            "b1: unusable store is logged",
        )
        # Fresh-start observation rides the "starting fresh" clause of the
        # same log line plus a clean exit; the fresh-start default layout
        # itself is already covered by the M4 lanes.
        wait_for_log(
            "; starting fresh",
            log_path,
            "b1: fresh-start clause logged",
        )
    finally:
        exited = close_gracefully(process) or poll_until(
            lambda: process.poll() == 0, 5.0
        )
        if process.poll() is None:
            taskkill_pid(process.pid)
        check(
            "b1: app survives and exits cleanly",
            exited or process.poll() == 0,
            f"code={process.poll()}",
        )

    # ---- b2: one unrestorable workspace, one good one ----
    kill_stray_shells()
    fixture = root / "b2-partial"
    fixture.mkdir()
    make_config(fixture)
    bad_workspace = crafted_workspace(
        "wb-diag",
        crafted_split("diagonal", crafted_leaf(["p91"]), crafted_leaf(["p92"])),
        ["p91", "p92"],
        focused="p91",
    )
    good_workspace = crafted_workspace(
        "wb-good",
        crafted_split("vertical", crafted_leaf(["p93"]), crafted_leaf(["p94"])),
        ["p93", "p94"],
        focused="p93",
    )
    (fixture / "session.json").write_text(
        crafted_snapshot([bad_workspace, good_workspace], selected=0),
        encoding="utf-8",
    )
    pipe = rf"\\.\pipe\cmux-restore-b2-{os.getpid()}-{random.randint(100, 999)}"
    launch_call = pipe
    process, log_path = launch(shell_bin, fixture, pipe, extra_env=None)
    print(f"b2 launched pid={process.pid} pipe={pipe}")
    try:
        wait_for_log(
            "workspace 1 in the session snapshot is not restorable",
            log_path,
            "b2: the broken workspace is skipped",
        )
        wait_for_log(
            "restored 1 workspace(s) with 2 pane(s)",
            log_path,
            "b2: the valid workspace still restores",
        )
        client = harness.cmux(
            socket_path=launch_call, connect_timeout_s=30.0, timeout_s=5.0
        )
        rows = poll_until(
            lambda: rows
            if (rows := retry_ipc(lambda: client.list_workspaces()) or []) and rows
            else None,
            20.0,
        ) or []
        ids = [str(row.get("id")) for row in rows]
        check(
            "b2: only the good workspace survives",
            ids == ["wb-good"],
            f"ids={ids}",
        )
        current = retry_ipc(lambda: client.current_workspace()) or ""
        check(
            "b2: active index clamps to the survivor",
            current == "wb-good",
            f"current={current}",
        )
        got = [
            str(row.get("id"))
            for row in retry_ipc(lambda: client.list_panes("wb-good")) or []
        ]
        check(
            "b2: surviving workspace keeps its saved pane ids",
            got == ["p93", "p94"],
            f"got={got}",
        )
        try:
            client.close()
        except OSError:
            pass
    finally:
        exited = close_gracefully(process)
        if process.poll() is None:
            taskkill_pid(process.pid)
        check(
            "b2: exits cleanly",
            exited or process.poll() == 0,
            f"code={process.poll()}",
        )


    # ---- b3: restore smoke restores and never writes ----
    kill_stray_shells()
    fixture = root / "b3-smoke"
    fixture.mkdir()
    make_config(fixture)
    smoke_workspace = crafted_workspace(
        "wb-smoke",
        crafted_split("vertical", crafted_leaf(["p61"]), crafted_leaf(["p62"])),
        ["p61", "p62"],
        focused="p61",
    )
    store = fixture / "session.json"
    store.write_text(crafted_snapshot([smoke_workspace], selected=0), encoding="utf-8")
    before = sha256(store)
    process, log_path = launch(
        shell_bin,
        fixture,
        rf"\\.\pipe\cmux-restore-b3-{os.getpid()}-{random.randint(100, 999)}",
        extra_env={"CMUX_SHELL_RESTORE_SMOKE": "1"},
    )
    print(f"b3 launched pid={process.pid}")
    try:
        wait_for_log(
            "restored 1 workspace(s) with 2 pane(s)",
            log_path,
            "b3: restore smoke restores the crafted tree",
        )
    finally:
        exited = close_gracefully(process)
        if process.poll() is None:
            taskkill_pid(process.pid)
        check(
            "b3: restore smoke exits cleanly",
            exited or process.poll() == 0,
            f"code={process.poll()}",
        )
        check(
            "b3: the store bytes never change",
            store.is_file() and sha256(store) == before,
            f"before={before} "
            f"after={sha256(store) if store.is_file() else 'gone'}",
        )

    # ---- b4: broken primary, valid -previous backup ----
    kill_stray_shells()
    fixture = root / "b4-backup"
    fixture.mkdir()
    make_config(fixture)
    (fixture / "session.json").write_bytes(b"not-json-at-all\x00\x01")
    prev_workspace = crafted_workspace(
        "wb-prev",
        crafted_split("horizontal", crafted_leaf(["p81"]), crafted_leaf(["p82"])),
        ["p81", "p82"],
        focused="p82",
    )
    (fixture / "session.json-previous").write_text(
        crafted_snapshot([prev_workspace], selected=0), encoding="utf-8"
    )
    pipe = rf"\\.\pipe\cmux-restore-b4-{os.getpid()}-{random.randint(100, 999)}"
    process, log_path = launch(shell_bin, fixture, pipe, extra_env=None)
    print(f"b4 launched pid={process.pid} pipe={pipe}")
    try:
        restored = wait_for_log(
            "restored 1 workspace(s) with 2 pane(s)",
            log_path,
            "b4: backup wins over the broken primary",
        )
        if restored:
            client = harness.cmux(
                socket_path=pipe, connect_timeout_s=30.0, timeout_s=5.0
            )
            rows = poll_until(
                lambda: rows
                if (rows := retry_ipc(lambda: client.list_workspaces()) or []) and rows
                else None,
                20.0,
            ) or []
            ids = [str(row.get("id")) for row in rows]
            check(
                "b4: restored workspace id comes from the backup",
                ids == ["wb-prev"],
                f"ids={ids}",
            )
            current = retry_ipc(lambda: client.current_workspace()) or ""
            check(
                "b4: active index reflects the backup",
                current == "wb-prev",
                f"current={current}",
            )
            got = [
                str(row.get("id"))
                for row in retry_ipc(lambda: client.list_panes("wb-prev")) or []
            ]
            check(
                "b4: backup pane ids preserved",
                got == ["p81", "p82"],
                f"got={got}",
            )
            try:
                client.close()
            except OSError:
                pass
    finally:
        exited = close_gracefully(process)
        if process.poll() is None:
            taskkill_pid(process.pid)
        check(
            "b4: exits cleanly",
            exited or process.poll() == 0,
            f"code={process.poll()}",
        )


# --------------------------- main --------------------------------------


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

    print(f"== M6 session-restore lane: {shell_bin} ==")
    smoke_a(shell_bin)
    smoke_b(shell_bin)
    print(f"\nSUMMARY pass={len(PASS)} fail={len(FAIL)}")
    if FAIL:
        print("FAILED:")
        for name in FAIL:
            print(f"  {name}")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
