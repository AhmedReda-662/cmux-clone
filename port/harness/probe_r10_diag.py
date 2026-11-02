"""One-off full diagnostic for the r10 battery: windows, processes, geometry, records."""

from __future__ import annotations

import ctypes
import ctypes.wintypes as wt
import os
import random
import subprocess
import struct
import sys
import time
import zlib
from pathlib import Path

sys.path.insert(0, r"D:\workspace\Projects\cmux-clone\port\harness")
import test_terminal_mouse_r10 as R10  # noqa: E402
import test_sidebar_depth_r9 as R9  # noqa: E402

TEMP = Path(os.environ.get("TEMP", r"C:\Users\noah\AppData\Local\Temp")) / "opencode"

user32 = ctypes.WinDLL("user32", use_last_error=True)


def save_png(surface, name: str) -> None:
    """Save the app client capture as a PNG for visual inspection."""
    w, h, pixel, _ = R9.capture(surface.hwnd)
    rows = bytearray()
    for y in range(h):
        rows.append(0)
        for x in range(w):
            b, g, r = pixel(x, y)
            rows += bytes((r, g, b))
    raw = bytes(rows)
    def chunk(tag: bytes, data: bytes) -> bytes:
        return (
            struct.pack(">I", len(data)) + tag + data
            + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
        )
    png = (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(raw, 6))
        + chunk(b"IEND", b"")
    )
    (TEMP / name).write_bytes(png)
    print(f"    saved {name} ({w}x{h})", flush=True)


def enumerate_windows(label: str, target_pid: int) -> None:
    found = []

    @ctypes.WINFUNCTYPE(wt.BOOL, wt.HWND, wt.LPARAM)
    def cb(hwnd, _lp):
        pid = wt.DWORD()
        user32.GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
        r = wt.RECT()
        user32.GetWindowRect(hwnd, ctypes.byref(r))
        found.append(
            (
                hwnd,
                pid.value,
                (r.left, r.top, r.right - r.left, r.bottom - r.top),
                bool(user32.IsWindowVisible(hwnd)),
            )
        )
        return True

    user32.EnumWindows(cb, 0)
    plains = [
        w
        for w in found
        if w[3] and w[2][2] >= 200 and w[2][3] >= 150
    ]
    print(f"{label}: visible-windows count={len(plains)}", flush=True)
    for hwnd, pid, rect, _vis in plains[:6]:
        print(f"    hwnd=0x{hwnd:X} pid={pid} rect={rect}", flush=True)
    ours = [w for w in found if w[1] == target_pid]
    print(f"{label}: windows owned by app pid {target_pid}: {len(ours)}", flush=True)
    for hwnd, pid, rect, vis in ours[:6]:
        print(f"    hwnd=0x{hwnd:X} rect={rect} visible={vis}", flush=True)


def process_tree(ppid: int) -> None:
    cmd = (
        "Get-CimInstance Win32_Process | "
        f"Where-Object {{ $_.ProcessId -eq {ppid} -or $_.ParentProcessId -eq {ppid} }} | "
        "Select-Object ProcessId, ParentProcessId, Name | "
        "Format-Table -AutoSize | Out-String -Width 80"
    )
    out = subprocess.run(
        ["powershell", "-NoProfile", "-Command", cmd], capture_output=True, text=True
    )
    print("process tree:", out.stdout.strip(), "err:", out.stderr.strip(), flush=True)


def main() -> int:
    shell_bin = Path(r"D:\workspace\Projects\cmux-clone\port\target\debug\cmux-shell.exe").resolve()
    pipe = rf"\\.\pipe\cmux-r10-diag-{os.getpid()}-{random.randint(1000, 9999)}"
    fixture = Path(R10.tempfile.mkdtemp(prefix="cmux-r10-diag-"))
    child_log = TEMP / f"diag_records_{os.getpid()}.log"
    child_log.write_text("", encoding="utf-8")
    process, _ = R10.launch_app(pipe, fixture, shell_bin)
    print(f"launched app pid={process.pid}", flush=True)
    import cmux as harness
    client = harness.cmux(socket_path=pipe, connect_timeout_s=30.0, timeout_s=5.0)

    try:
        process_tree(process.pid)
        hwnd = R9.poll_until(lambda: R9.main_window_for(process.pid), 15.0)
        print(f"main hwnd={hwnd}", flush=True)
        enumerate_windows("T1 found", process.pid)
        R9.pin_us_layout(hwnd)
        ok = R9.poll_until(
            lambda: R9.foreground_window(hwnd) and R9.user32.GetForegroundWindow() == hwnd, 10.0
        )
        print(f"foreground accepted={ok}", flush=True)
        R9.settle(1.0)
        rects = wt.RECT()
        user32.GetWindowRect(hwnd, ctypes.byref(rects))
        print(f"after foreground: hwnd rect=({rects.right-rects.left}x{rects.bottom-rects.top})", flush=True)
        enumerate_windows("T2 foreground", process.pid)

        surface = R9.ClientSurface(hwnd)
        R9.settle(1.0)
        surface_state = (
            f"surface: x={surface.x} y={surface.y} w={surface.w} h={surface.h} "
            f"scale={surface.scale}"
        )
        print(surface_state, flush=True)
        save_png(surface, "r10_diag_0_start.png")

        child_path = Path(r"D:\workspace\Projects\cmux-clone\port\harness\mouse_tui_records.py")
        R9.type_text(f"python {child_path} {child_log}")
        R9.send_enter()
        ready = R9.poll_until(
            lambda: any(l.startswith("READY") for l in R10.read_lines(child_log)), 20.0
        )
        print(f"child ready={ready}", flush=True)
        R9.settle(2.0)
        save_png(surface, "r10_diag_1_child.png")

        R9.click(*surface.screen_of(420.0, 250.0))
        R9.settle(1.2)
        R9.click(*surface.screen_of(530.0, 285.0))
        R9.settle(1.2)
        save_png(surface, "r10_diag_2_clicked.png")
        print("child log after clicks:", child_log.read_text(encoding="utf-8", errors="replace"), flush=True)
        print("read_text:", repr(client.read_text())[:300], flush=True)
        return 0
    finally:
        R10.close_app(process, fixture)


if __name__ == "__main__":
    raise SystemExit(main())
