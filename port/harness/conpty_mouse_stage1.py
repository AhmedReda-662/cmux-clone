"""Stage 1: how does a conpty host deliver SGR mouse bytes written to its input?

Standalone (no cmux app): creates a ConPTY around one of the mouse_tui_*
probe children, writes the exact escape sequences the widget would write
(`ESC[<0;2;3M` press / `ESC[<0;2;3m` release), and reports what the child
received — on stdin bytes and/or on console INPUT_RECORDs.

By default the pseudoconsole comes from `kernel32.dll` (the stock host).
Pass `--dll <path>` (e.g. port\\third_party\\conpty\\conpty.dll) to load a
sideloaded conpty.dll instead and see how that host differs. Everything the
child saw is also in its own side log (argv[1]) for the record.

Probe children:
  mouse_tui_records.py  - child reads console INPUT_RECORDs (ReadConsoleInput)
  mouse_tui_vtinput.py  - child enables VT-input stdin and reads raw bytes
"""

from __future__ import annotations

import argparse
import ctypes
import ctypes.wintypes as wt
import os
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)

INVALID_HANDLE_VALUE = ctypes.c_void_p(-1).value or 0xFFFFFFFFFFFFFFFF


class COORD(ctypes.Structure):
    _fields_ = [("X", ctypes.c_short), ("Y", ctypes.c_short)]


class SECURITY_ATTRIBUTES(ctypes.Structure):
    _fields_ = [
        ("nLength", ctypes.c_ulong),
        ("lpSecurityDescriptor", ctypes.c_void_p),
        ("bInheritHandle", ctypes.c_int),
    ]


class PROCESS_INFORMATION(ctypes.Structure):
    _fields_ = [
        ("hProcess", ctypes.c_void_p),
        ("hThread", ctypes.c_void_p),
        ("dwProcessId", ctypes.c_ulong),
        ("dwThreadId", ctypes.c_ulong),
    ]


class STARTUPINFOW(ctypes.Structure):
    _fields_ = [
        ("cb", ctypes.c_ulong),
        ("lpReserved", ctypes.c_wchar_p),
        ("lpDesktop", ctypes.c_wchar_p),
        ("lpTitle", ctypes.c_wchar_p),
        ("dwX", ctypes.c_ulong),
        ("dwY", ctypes.c_ulong),
        ("dwXSize", ctypes.c_ulong),
        ("dwYSize", ctypes.c_ulong),
        ("dwXCountChars", ctypes.c_ulong),
        ("dwYCountChars", ctypes.c_ulong),
        ("dwFillAttribute", ctypes.c_ulong),
        ("dwFlags", ctypes.c_ulong),
        ("wShowWindow", ctypes.c_short),
        ("cbReserved2", ctypes.c_short),
        ("lpReserved2", ctypes.c_void_p),
        ("hStdInput", ctypes.c_void_p),
        ("hStdOutput", ctypes.c_void_p),
        ("hStdError", ctypes.c_void_p),
    ]


class STARTUPINFOEXW(ctypes.Structure):
    _anonymous_ = ("StartupInfo",)
    _fields_ = [
        ("StartupInfo", STARTUPINFOW),
        ("lpAttributeList", ctypes.c_void_p),
    ]


PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE = 0x00020016
EXTENDED_STARTUPINFO_PRESENT = 0x00080000
CREATE_UNICODE_ENVIRONMENT = 0x00000400

kernel32.CreatePseudoConsole.restype = ctypes.c_long  # HRESULT
kernel32.CreatePseudoConsole.argtypes = [
    COORD, ctypes.c_void_p, ctypes.c_void_p, ctypes.c_ulong, ctypes.POINTER(ctypes.c_void_p),
]
kernel32.ResizePseudoConsole.restype = ctypes.c_long
kernel32.ResizePseudoConsole.argtypes = [ctypes.c_void_p, COORD]
kernel32.ClosePseudoConsole.argtypes = [ctypes.c_void_p]
kernel32.InitializeProcThreadAttributeList.restype = ctypes.c_int
kernel32.InitializeProcThreadAttributeList.argtypes = [
    ctypes.c_void_p, ctypes.c_ulong, ctypes.c_ulong, ctypes.POINTER(ctypes.c_size_t),
]
kernel32.UpdateProcThreadAttribute.restype = ctypes.c_int
kernel32.UpdateProcThreadAttribute.argtypes = [
    ctypes.c_void_p, ctypes.c_ulong, ctypes.c_void_p, ctypes.c_void_p,
    ctypes.c_size_t, ctypes.c_void_p, ctypes.c_void_p,
]
kernel32.CreatePipe.restype = ctypes.c_int
kernel32.CreatePipe.argtypes = [
    ctypes.POINTER(ctypes.c_void_p), ctypes.POINTER(ctypes.c_void_p),
    ctypes.POINTER(SECURITY_ATTRIBUTES), ctypes.c_ulong,
]
kernel32.CreateProcessW.restype = ctypes.c_int
kernel32.CreateProcessW.argtypes = [
    ctypes.c_wchar_p, ctypes.c_wchar_p, ctypes.c_void_p, ctypes.c_void_p,
    ctypes.c_int, ctypes.c_ulong, ctypes.c_void_p, ctypes.c_wchar_p,
    ctypes.c_void_p, ctypes.c_void_p,
]

PSEUDOCONSOLE_INHERIT_CURSOR = 0x1
PSEUDOCONSOLE_RESIZE_QUIRK = 0x2
PSEUDOCONSOLE_WIN32_INPUT_MODE = 0x4


def make_pipe_pair():
    read_side, write_side = ctypes.c_void_p(), ctypes.c_void_p()
    sa = SECURITY_ATTRIBUTES(ctypes.sizeof(SECURITY_ATTRIBUTES), None, 1)
    ok = kernel32.CreatePipe(
        ctypes.byref(read_side), ctypes.byref(write_side), ctypes.byref(sa), 0
    )
    assert ok, f"CreatePipe failed: {ctypes.GetLastError()}"
    return read_side, write_side


def read_log(path: Path) -> list[str]:
    try:
        return [
            line
            for line in path.read_text(encoding="utf-8", errors="replace").splitlines()
            if line.strip()
        ]
    except FileNotFoundError:
        return []


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--dll", default=None, help="conpty.dll to sideload instead of kernel32")
    parser.add_argument("--child", default=None, help="probe child .py (required unless --cmd)")
    parser.add_argument("--wait", type=float, default=8.0, help="seconds waiting for READY")
    parser.add_argument(
        "--cmd", default=None, help="raw command line (overrides --child; argv.got probe skipped)"
    )
    parser.add_argument(
        "--no-inherit", action="store_true", help="drop PSEUDOCONSOLE_INHERIT_CURSOR"
    )
    args = parser.parse_args()

    root = Path(__file__).resolve().parents[1]
    spec = Path(args.child).resolve() if args.child else None
    assert args.cmd or (spec and spec.exists()), "no child file given"

    stamp = str(int(time.time() * 1000))
    tag = "conptydll" if args.dll else "kernel32"
    child_log = Path(os.environ["TEMP"]) / "opencode" / f"stage1_{tag}_{stamp}.log"
    out_log = Path(os.environ["TEMP"]) / "opencode" / f"stage1_{tag}_{stamp}.console"
    child_log.write_text("", encoding="utf-8")
    out_log.write_text("", encoding="utf-8")
    print(f"host={args.dll or 'kernel32.dll'}")
    print(f"child={(spec.name if spec else args.cmd)!r} -> {child_log}")

    in_r, in_w = make_pipe_pair()
    out_r, out_w = make_pipe_pair()

    host = ctypes.WinDLL(args.dll) if args.dll else kernel32
    hpc = ctypes.c_void_p()
    flags = PSEUDOCONSOLE_RESIZE_QUIRK | PSEUDOCONSOLE_WIN32_INPUT_MODE
    if not args.no_inherit:
        flags |= PSEUDOCONSOLE_INHERIT_CURSOR
    print(f"flags={flags:#x}")
    result = host.CreatePseudoConsole(
        COORD(80, 25), in_r.value, out_w.value, flags, ctypes.byref(hpc)
    )
    assert result == 0, f"CreatePseudoConsole HRESULT {result:#x}"

    # Spawn the child with the pty attribute list.
    py = sys.executable
    cmdline = args.cmd or f'"{py}" -u "{spec}" "{child_log}"'
    print(f"cmdline: {cmdline}")
    size = ctypes.c_size_t(0)
    kernel32.InitializeProcThreadAttributeList(None, 1, 0, ctypes.byref(size))
    attr = ctypes.create_string_buffer(size.value)
    assert kernel32.InitializeProcThreadAttributeList(attr, 1, 0, ctypes.byref(size))
    hpc_const = ctypes.c_void_p(hpc.value)
    assert kernel32.UpdateProcThreadAttribute(
        attr, 0, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, hpc_const,
        ctypes.sizeof(ctypes.c_void_p), None, None,
    ), f"UpdateProcThreadAttribute failed: {ctypes.GetLastError()}"

    STARTF_USESTDHANDLES = 0x100

    si = STARTUPINFOEXW()
    si.StartupInfo.cb = ctypes.sizeof(STARTUPINFOEXW)
    si.lpAttributeList = ctypes.cast(attr, ctypes.c_void_p).value
    si.StartupInfo.dwFlags = STARTF_USESTDHANDLES
    invalid = ctypes.c_void_p(0xFFFFFFFFFFFFFFFF)
    si.hStdInput = invalid
    si.hStdOutput = invalid
    si.hStdError = invalid
    pi = PROCESS_INFORMATION()
    wcmd = ctypes.create_unicode_buffer(cmdline)
    if args.cmd:
        wexe = None
    else:
        wexe = ctypes.create_unicode_buffer(str(py))
    ok = kernel32.CreateProcessW(
        wexe, wcmd, None, None, 0,
        EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
        None, None, ctypes.byref(si), ctypes.byref(pi),
    )
    err = ctypes.get_last_error()
    assert ok, f"CreateProcessW failed: le={err}"
    print(f"child pid={pi.dwProcessId}")

    # Drain the child's console output in the background (the pty output pipe
    # would block the child's writes once full).
    out_fh = open(out_log, "wb")
    drain_dbg = open(Path(str(out_log) + ".drain"), "wb")
    import threading

    def drain():
        count = 0
        while True:
            buf = ctypes.create_string_buffer(4096)
            got = ctypes.c_ulong()
            ok_read = kernel32.ReadFile(out_r, buf, 4096, ctypes.byref(got), None)
            if not ok_read:
                drain_dbg.write(f"DRAIN le={ctypes.get_last_error()} stopped after {count}\n".encode())
                break
            if not got.value:
                drain_dbg.write(f"DRAIN zero read after {count}\n".encode())
                break
            count += 1
            drain_dbg.write(f"DRAIN {count} got={got.value} {buf.raw[: got.value][:120]!r}\n".encode())
            drain_dbg.flush()
            out_fh.write(buf.raw[: got.value])
            out_fh.flush()

    threading.Thread(target=drain, daemon=True).start()

    def child_exit_code() -> int | None:
        code = ctypes.c_ulong()
        if kernel32.GetExitCodeProcess(pi.hProcess, ctypes.byref(code)):
            return code.value
        return None

    # Liveness trail: 4s, 12s and after the pty closes.
    for tag, delay in (("after 4s", 4.0), ("after 12s", 8.0)):
        time.sleep(delay)
        code = child_exit_code()
        label = "STILL_ACTIVE" if code == 259 else f"exit {code:#x}"
        print(f"{tag}: child {label}")

    # Wait for READY from the child before treating its probes as meaningful.
    deadline = time.monotonic() + args.wait
    while time.monotonic() < deadline:
        if any(l.startswith("READY") for l in read_log(child_log)):
            break
        time.sleep(0.2)
    print(f"READY reached: {any('READY' in l for l in read_log(child_log))}")
    time.sleep(1.0)

    # The widget's reports: left-button press then release at column 2, row 3.
    sgr = b"\x1b[<0;2;3M" b"\x1b[<0;2;3m"
    written = ctypes.c_ulong()
    assert kernel32.WriteFile(in_w, sgr, len(sgr), ctypes.byref(written), None)
    print(f"written: {sgr!r} ({written.value} bytes; expect 20)")
    time.sleep(3.0)

    print("--- child log ---")
    for line in read_log(child_log):
        print("  ", line[:200])
    kernel32.ClosePseudoConsole(hpc)
    time.sleep(1.0)
    code = child_exit_code()
    print(f"after close: child {'STILL_ACTIVE' if code == 259 else f'exit {code:#x}'}")
    try:
        proc = ctypes.c_ulong(pi.dwProcessId)
        handle = kernel32.OpenProcess(0x0001, 0, proc)
        if handle:
            kernel32.TerminateProcess(handle, 1)
            kernel32.CloseHandle(handle)
    finally:
        print(f"child_log={child_log}")
        print(f"console_out={out_log}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
