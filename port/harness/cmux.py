"""cmux port harness — the M4.x automation client.

A protocol-compatible mirror of the upstream automation harness
(`cmux-main/tests_v2/cmux.py`) for the port's Windows named-pipe transport
(ADR-0005). stdlib-only (no socket transport on this platform): the same
JSON-lines envelope, the same M4-slice method names, the same
integer-index convenience resolution, and the same error surfacing
(`cmuxError` with `code: message`).

Wire shape (verified against the port's server and the upstream parser):

  -> {"id": "...", "method": "...", "params": {...}}
  <- {"id": "...", "ok": true,  "result": {...}}
  <- {"id": "...", "ok": false, "error": {"code","message","action","data"}}

Connection shape (M5.2, ADR-0005 Amendment 5 — upstream's persistent
client): one named-pipe connection per client instance. The first call
opens it (and runs the M4.2 `auth.login` handshake once when a password
is configured); later calls reuse it. A transport-level failure (or an
unreadable reply line) drops the connection so the next call reconnects
and re-handshakes; a cleanly parsed non-ok reply keeps the pipe. The
harness creates its pipe name from `CMUX_SOCKET_PATH` (the documented
override; the port's ancestry gate is `cmuxOnly` only on the per-user
default name) — the same environment variable the app and `cmux`
CLI honor.

Usage:

    import cmux
    c = cmux.cmux()
    c.ping()                      # -> True
    ws = c.new_workspace()        # -> {"workspace_id": "w2", "surface_id": ...}
    c.send("echo hello\\n")       # types into the focused surface
    c.read_text()                 # -> visible screen text

Password-mode servers (ADR-0005 Amendment 3): pass `password=` to log in
before every call; a refused login raises `cmuxError` with the server's
code (`auth_failed`, `auth_required`, `auth_unconfigured`, `invalid_params`).
"""

from __future__ import annotations

import ctypes
import ctypes.wintypes as wt
import json
import os
import time
from typing import Any, Dict, List, Optional, Union

__all__ = ["cmux", "cmuxError", "DEFAULT_CONNECT_TIMEOUT_S", "DEFAULT_TIMEOUT_S"]

# Generous vs. the app's own reply budget (400 ms) and the CLI's 500 ms
# response read: a stuck server surfaces here as a Python-level timeout.
DEFAULT_CONNECT_TIMEOUT_S = 5.0
DEFAULT_TIMEOUT_S = 5.0

_GENERIC_READ_WRITE = 0xC0000000
_OPEN_EXISTING = 3
_FILE_ATTRIBUTE_NORMAL = 0x80
_INVALID_HANDLE = ctypes.c_void_p(-1).value
_ERROR_PIPE_BUSY = 231
_ERROR_BROKEN_PIPE = 109
_ERROR_ACCESS_DENIED = 5

# 64-bit-safe kernel32 bindings: without `restype = c_void_p` the returned
# HANDLE truncates to 32 bits and ReadFile rejects it on 64-bit Python.
_kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
_kernel32.CreateFileW.restype = ctypes.c_void_p
_kernel32.CreateFileW.argtypes = [
    wt.LPCWSTR,
    wt.DWORD,
    wt.DWORD,
    ctypes.c_void_p,
    wt.DWORD,
    wt.DWORD,
    ctypes.c_void_p,
]
_kernel32.WriteFile.restype = wt.BOOL
_kernel32.WriteFile.argtypes = [ctypes.c_void_p, ctypes.c_char_p, wt.DWORD, ctypes.POINTER(wt.DWORD), ctypes.c_void_p]
_kernel32.ReadFile.restype = wt.BOOL
_kernel32.ReadFile.argtypes = [ctypes.c_void_p, ctypes.c_void_p, wt.DWORD, ctypes.POINTER(wt.DWORD), ctypes.c_void_p]
_kernel32.PeekNamedPipe.restype = wt.BOOL
_kernel32.PeekNamedPipe.argtypes = [ctypes.c_void_p, ctypes.c_void_p, wt.DWORD, ctypes.c_void_p, ctypes.POINTER(wt.DWORD), ctypes.c_void_p]
_kernel32.CloseHandle.restype = wt.BOOL
_kernel32.CloseHandle.argtypes = [ctypes.c_void_p]


class cmuxError(Exception):
    """Raised for a non-ok reply (or a transport failure).

    `code`/`message` mirror the upstream error fields; `str(err)` formats
    `code: message` exactly like upstream, plus ` (action)` when the
    server carries a recovery hint.
    """

    def __init__(self, message: str, code: str = "error", data: Any = None) -> None:
        super().__init__(message)
        self.code = code
        self.data = data


class _Pipe:
    """One byte-mode named-pipe connection (`CreateFileW` + reads/writes)."""

    def __init__(self, name: str, connect_timeout_s: float) -> None:
        deadline = time.monotonic() + connect_timeout_s
        handle = None
        while True:
            handle = _kernel32.CreateFileW(
                name,
                _GENERIC_READ_WRITE,
                0,
                None,
                _OPEN_EXISTING,
                _FILE_ATTRIBUTE_NORMAL,
                None,
            )
            if handle != _INVALID_HANDLE and handle is not None:
                break
            error = _kernel32.GetLastError()
            if error == _ERROR_ACCESS_DENIED:
                # The DACL rejected this caller (a foreign user's pipe or a
                # same-user non-descendant against the gated default name):
                # a failure, not an availability wait.
                raise cmuxError(f"the pipe rejected this process (error {error})", code="unauthorized_peer")
            if time.monotonic() >= deadline:
                raise cmuxError(f"the pipe did not answer within {connect_timeout_s}s", code="unavailable")
            # Everything else (not-yet-created pipe, all-instances-busy)
            # is part of the retry window, like the port's own client.
            time.sleep(0.02)
        self.handle = handle
        self._leftover = b""

    def write(self, data: bytes) -> None:
        written = wt.DWORD(0)
        if not _kernel32.WriteFile(self.handle, data, len(data), ctypes.byref(written), None):
            raise cmuxError(f"write failed (error {_kernel32.GetLastError()})", code="io_error")
        if written.value != len(data):
            raise cmuxError("short write on the pipe", code="io_error")

    def read_line(self, timeout_s: float) -> str:
        """Reads one `\\n`-terminated JSON line behind a hard deadline."""
        deadline = time.monotonic() + timeout_s
        chunks = self._leftover
        self._leftover = b""
        while b"\n" not in chunks:
            if time.monotonic() >= deadline:
                raise cmuxError("timed out waiting for the reply", code="timeout")
            available = wt.DWORD(0)
            if not _kernel32.PeekNamedPipe(self.handle, None, 0, None, ctypes.byref(available), None):
                error = _kernel32.GetLastError()
                if chunks:
                    raise cmuxError(
                        "connection closed before a full reply arrived", code="io_error"
                    )
                if error == _ERROR_BROKEN_PIPE:
                    raise cmuxError("the pipe closed before answering", code="io_error")
                raise cmuxError(f"pipe state check failed (error {error})", code="io_error")
            if available.value > 0:
                bufsize = min(available.value, 65536)
                buf = ctypes.create_string_buffer(bufsize)
                read = wt.DWORD(0)
                if not _kernel32.ReadFile(self.handle, buf, bufsize, ctypes.byref(read), None):
                    error = _kernel32.GetLastError()
                    if error == _ERROR_BROKEN_PIPE and read.value == 0:
                        continue
                    raise cmuxError(f"read failed (error {error})", code="io_error")
                if read.value:
                    chunks += buf.raw[: read.value]
            else:
                time.sleep(0.01)
        line, _, rest = chunks.partition(b"\n")
        self._leftover = rest
        return line.decode("utf-8", errors="replace")

    def close(self) -> None:
        if getattr(self, "handle", None) not in (None, _INVALID_HANDLE):
            _kernel32.CloseHandle(self.handle)
            self.handle = None


def _default_pipe_name() -> Optional[str]:
    value = os.environ.get("CMUX_SOCKET_PATH", "").strip()
    return value or None


class cmux:
    """The harness client: upstream API subset over the port's pipe.

    Convenience methods resolve integer/list members to ids exactly like
    upstream (`list_workspaces()[k]["index"]` never appears in tests —
    ids resolve through row lookups), so tests written against upstream's
    harness read the same here.
    """

    def __init__(
        self,
        socket_path: Optional[str] = None,
        connect_timeout_s: float = DEFAULT_CONNECT_TIMEOUT_S,
        timeout_s: float = DEFAULT_TIMEOUT_S,
        password: Optional[str] = None,
    ) -> None:
        self.path = socket_path or _default_pipe_name()
        if not self.path:
            raise cmuxError(
                "CMUX_SOCKET_PATH is not set and no default was given", code="config"
            )
        self._connect_timeout_s = connect_timeout_s
        self._timeout_s = timeout_s
        self._call_count = 0
        self.password = password
        # M5.2 (ADR-0005 Amendment 5): upstream's client keeps ONE
        # persistent socket per instance (`tests_v2/cmux.py` `connect()`
        # reuses `_socket`, `close()` releases it; the password handshake
        # runs once per client lifetime). The port mirrors that shape: a
        # connection survives across healthy calls and is dropped only on
        # a transport-level failure — the next call reconnects and
        # re-handshakes. A cleanly parsed non-ok reply keeps the pipe.
        self._pipe: Optional[_Pipe] = None

    # ------------------------------------------------------------------
    # Core call channel
    # ------------------------------------------------------------------

    def connect(self) -> _Pipe:
        """Returns the persistent connection, opening it lazily.

        The login handshake (when `password=` was configured) runs once
        per connection, after the first open — the port's server keeps
        per-connection auth state, so a reset pipe re-handshakes on its
        next `connect()`.
        """
        if self._pipe is None:
            pipe = _Pipe(self.path, self._connect_timeout_s)
            if self.password:
                # The login handshake on the fresh connection (upstream
                # auth-then-command shape; the port's server keeps one
                # auth state per connection, so a reset pipe re-handshakes
                # at its next `connect()`).
                # `_call` increments the counter before connecting, so the
                # current call number spaces the auth ids out exactly like
                # the per-connection model did.
                auth_id = f"auth-{self._call_count}"
                auth_envelope = {
                    "id": auth_id,
                    "method": "auth.login",
                    "params": {"password": self.password},
                }
                auth_line = json.dumps(auth_envelope, separators=(",", ":")) + "\n"
                pipe.write(auth_line.encode("utf-8"))
                auth_reply_line = pipe.read_line(self._timeout_s)
                auth_reply = json.loads(auth_reply_line)
                if auth_reply.get("id") != auth_id:
                    pipe.close()
                    raise cmuxError(
                        f"Mismatched login id: expected {auth_id}, got {auth_reply.get('id')}"
                    )
                if auth_reply.get("ok") is not True:
                    pipe.close()
                    err = auth_reply.get("error") or {}
                    code = err.get("code") or "error"
                    raise cmuxError(
                        f"{code}: {err.get('message') or 'login refused'}",
                        code=code,
                        data=err.get("data"),
                    )
            self._pipe = pipe
        return self._pipe

    def close(self) -> None:
        """Releases the persistent connection (upstream shape)."""
        if self._pipe is not None:
            self._pipe.close()
            self._pipe = None

    def _call(
        self, method: str, params: Optional[Dict[str, Any]] = None, timeout_s: float = None
    ) -> Any:
        self._call_count += 1
        req_id = f"call-{self._call_count}"
        envelope = {"id": req_id, "method": method, "params": params or {}}
        line = json.dumps(envelope, separators=(",", ":")) + "\n"
        pipe = self.connect()
        try:
            pipe.write(line.encode("utf-8"))
            reply_line = pipe.read_line(
                self._timeout_s if timeout_s is None else timeout_s
            )
            resp = json.loads(reply_line)
        except (cmuxError, json.JSONDecodeError) as error:
            # Transport-level failure or a corrupt line: the connection is
            # unusable; reset it so the next call reconnects and
            # re-handshakes. The error itself propagates exactly as before.
            self.close()
            if isinstance(error, json.JSONDecodeError):
                raise cmuxError(
                    f"unreadable reply line: {error}", code="io_error"
                ) from error
            raise
        # Any cleanly parsed reply leaves the pipe healthy (a server-side
        # error is a normal conversation, not a transport fault).
        if resp.get("id") != req_id:
            self.close()
            raise cmuxError(
                f"Mismatched response id: expected {req_id}, got {resp.get('id')}"
            )
        if resp.get("ok") is True:
            return resp.get("result")
        err = resp.get("error") or {}
        code = err.get("code") or "error"
        message = err.get("message") or "Unknown error"
        action = err.get("action")
        data = err.get("data")
        suffix = "" if not action else f" (action: {action})"
        if data:
            raise cmuxError(f"{code}: {message}{suffix} ({data})", code=code, data=data)
        raise cmuxError(f"{code}: {message}{suffix}", code=code, data=data)

    # ------------------------------------------------------------------
    # System
    # ------------------------------------------------------------------

    def ping(self) -> bool:
        res = self._call("system.ping")
        return bool((res or {}).get("pong"))

    def capabilities(self) -> dict:
        return dict(self._call("system.capabilities") or {})

    def identify(self, caller: Optional[dict] = None) -> dict:
        params: Dict[str, Any] = {}
        if caller is not None:
            params["caller"] = caller
        return dict(self._call("system.identify", params) or {})

    # ------------------------------------------------------------------
    # Workspaces
    # ------------------------------------------------------------------

    def list_workspaces(self) -> List[dict]:
        res = self._call("workspace.list") or {}
        return list(res.get("workspaces") or [])

    def current_workspace(self) -> str:
        res = self._call("workspace.current") or {}
        workspace_id = res.get("workspace_id")
        if not workspace_id:
            raise cmuxError("No workspace selected", code="not_found")
        return str(workspace_id)

    def new_workspace(self) -> dict:
        return dict(self._call("workspace.create") or {})

    def select_workspace(self, workspace) -> None:
        workspace_id = self._resolve_workspace_id(workspace)
        self._call("workspace.select", {"workspace_id": workspace_id})

    def _resolve_workspace_id(self, workspace: Union[str, int, None]) -> str:
        if workspace is None:
            return self.current_workspace()
        if isinstance(workspace, int) or str(workspace).isdigit():
            rows = self.list_workspaces()
            index = int(workspace)
            if index < 0 or index >= len(rows):
                raise cmuxError(f"Workspace index {index} out of range", code="not_found")
            return str(rows[index].get("id"))
        return str(workspace)

    # ------------------------------------------------------------------
    # Surfaces / panes
    # ------------------------------------------------------------------

    def list_surfaces(self, workspace=None) -> List[dict]:
        workspace_id = self._resolve_workspace_id(workspace)
        res = self._call("surface.list", {"workspace_id": workspace_id}) or {}
        return list(res.get("surfaces") or [])

    def list_panes(self, workspace=None) -> List[dict]:
        workspace_id = self._resolve_workspace_id(workspace)
        res = self._call("pane.list", {"workspace_id": workspace_id}) or {}
        return list(res.get("panes") or [])

    def send(self, text: str, surface=None) -> None:
        params: Dict[str, Any] = {"text": text}
        if surface is not None:
            params["surface_id"] = self._resolve_surface_id(surface)
        self._call("surface.send_text", params)

    def read_text(self, surface=None) -> str:
        params: Dict[str, Any] = {}
        if surface is not None:
            params["surface_id"] = self._resolve_surface_id(surface)
        res = self._call("surface.read_text", params) or {}
        return str(res.get("text"))

    def _resolve_surface_id(self, surface: Union[str, int]) -> str:
        if isinstance(surface, int) or str(surface).isdigit():
            index = int(surface)
            current = self.current_workspace()
            rows = self.list_surfaces(current)
            if index < 0 or index >= len(rows):
                raise cmuxError(f"Surface index {index} out of range", code="not_found")
            return str(rows[index].get("id"))
        value = str(surface)
        if value.startswith("surface:"):
            value = value[len("surface:"):]
        return value

    # ------------------------------------------------------------------
    # Browser automation (M5.2: the agent-browser-port P0 subset)
    # ------------------------------------------------------------------

    def browser_eval(self, expression: str, surface=None) -> Any:
        """Evaluate a JavaScript expression; the reply carries `"value"`."""
        params: Dict[str, Any] = {"expression": expression}
        if surface is not None:
            params["surface_id"] = self._resolve_surface_id(surface)
        return self._call("browser.eval", params)

    def browser_snapshot(self, surface=None) -> dict:
        """The page's text lines plus ephemeral refs (`@eN`)."""
        params: Dict[str, Any] = {}
        if surface is not None:
            params["surface_id"] = self._resolve_surface_id(surface)
        return dict(self._call("browser.snapshot", params) or {})

    def browser_click(self, ref: str = None, surface=None, selector=None) -> dict:
        """One mouse press+release at the element's fresh center."""
        return dict(self._target_call("browser.click", {}, surface, ref, selector) or {})

    def browser_dblclick(self, ref: str = None, surface=None, selector=None) -> dict:
        """A double click (two paired events, clickCount 2)."""
        return dict(self._target_call("browser.dblclick", {}, surface, ref, selector) or {})

    def browser_type(self, ref: str = None, text: str = "", surface=None, selector=None) -> dict:
        """Focus the element, then insert the text (single insert)."""
        return dict(
            self._target_call("browser.type", {"text": text}, surface, ref, selector) or {}
        )

    # ------------------------------------------------------------------
    # Browser automation (M5.3: observation + form/scroll/screenshot family)
    # ------------------------------------------------------------------

    @staticmethod
    def _element_target(ref=None, selector=None) -> Dict[str, str]:
        """The element-verb target: `ref` XOR `selector` (the wire accepts
        both flavors; ambiguity and absence are caller bugs)."""
        if (ref is None) == (selector is None):
            raise cmuxError(
                "exactly one of ref= or selector= is required", code="invalid_params"
            )
        if ref is not None:
            return {"ref": ref}
        return {"selector": selector}

    def _target_call(
        self, method: str, extra: Dict[str, Any], surface=None, ref=None, selector=None
    ) -> Any:
        params = dict(self._element_target(ref, selector))
        params.update(extra)
        if surface is not None:
            params["surface_id"] = self._resolve_surface_id(surface)
        return self._call(method, params)

    def browser_fill(self, text: str, surface=None, ref=None, selector=None) -> dict:
        """Replace the element's value with `text` (`input`+`change`)."""
        return dict(self._target_call("browser.fill", {"text": text}, surface, ref, selector) or {})

    def browser_focus(self, surface=None, ref=None, selector=None) -> dict:
        """Focus the element (`el.focus()`)."""
        return dict(self._target_call("browser.focus", {}, surface, ref, selector) or {})

    def browser_check(self, surface=None, ref=None, selector=None) -> dict:
        """Check the checkbox (dispatches `change`)."""
        return dict(self._target_call("browser.check", {}, surface, ref, selector) or {})

    def browser_uncheck(self, surface=None, ref=None, selector=None) -> dict:
        """Uncheck the checkbox (dispatches `change`)."""
        return dict(self._target_call("browser.uncheck", {}, surface, ref, selector) or {})

    def browser_select(self, value: str, surface=None, ref=None, selector=None) -> dict:
        """Choose the `<option>` whose value or label matches."""
        return dict(self._target_call("browser.select", {"value": value}, surface, ref, selector) or {})

    def browser_hover(self, surface=None, ref=None, selector=None) -> dict:
        """Move the real mouse over the element's fresh center."""
        return dict(self._target_call("browser.hover", {}, surface, ref, selector) or {})

    def browser_scroll_into_view(self, surface=None, ref=None, selector=None) -> dict:
        """Center the element in the viewport."""
        return dict(
            self._target_call("browser.scroll_into_view", {}, surface, ref, selector) or {}
        )

    def browser_get(self, field: str, surface=None, ref=None, selector=None, attr=None, property=None) -> dict:
        """One `browser.get.*` read (text/html/value/attr/count/box/styles);
        the reply carries `value` (or `count` for the count read)."""
        extra: Dict[str, Any] = {}
        if attr is not None:
            extra["attr"] = attr
        if property is not None:
            extra["property"] = property
        return dict(self._target_call(f"browser.get.{field}", extra, surface, ref, selector) or {})

    def browser_is(self, state: str, surface=None, ref=None, selector=None) -> dict:
        """One `browser.is.*` probe (visible/enabled/checked)."""
        return dict(self._target_call(f"browser.is.{state}", {}, surface, ref, selector) or {})

    def browser_scroll(self, dx=None, dy=None, surface=None) -> dict:
        """Window scroll by deltas; the reply carries the new offsets."""
        params: Dict[str, Any] = {}
        if dx is not None:
            params["dx"] = dx
        if dy is not None:
            params["dy"] = dy
        if surface is not None:
            params["surface_id"] = self._resolve_surface_id(surface)
        return dict(self._call("browser.scroll", params) or {})

    # ------------------------------------------------------------------
    # Browser automation (M5.4: key synthesis + predicate waits)
    # ------------------------------------------------------------------

    def _surface_params(self, surface=None) -> Dict[str, Any]:
        params: Dict[str, Any] = {}
        if surface is not None:
            params["surface_id"] = self._resolve_surface_id(surface)
        return params

    def browser_press(self, key: str, surface=None) -> dict:
        """Dispatch one key on the focused element (the down+up pair)."""
        params = {"key": key, **self._surface_params(surface)}
        return dict(self._call("browser.press", params) or {})

    def browser_keydown(self, key: str, surface=None) -> dict:
        """Dispatch the key's down half only."""
        params = {"key": key, **self._surface_params(surface)}
        return dict(self._call("browser.keydown", params) or {})

    def browser_keyup(self, key: str, surface=None) -> dict:
        """Dispatch the key's up half only."""
        params = {"key": key, **self._surface_params(surface)}
        return dict(self._call("browser.keyup", params) or {})

    def browser_wait(
        self,
        surface=None,
        selector=None,
        text_contains=None,
        url_contains=None,
        load_state=None,
        function=None,
        timeout_ms=None,
    ) -> dict:
        """Poll the page until every given predicate holds (the AND of
        what was given); an unsatisfied wait answers the structured
        timeout past its deadline."""
        params: Dict[str, Any] = self._surface_params(surface)
        if selector is not None:
            params["selector"] = selector
        if text_contains is not None:
            params["text_contains"] = text_contains
        if url_contains is not None:
            params["url_contains"] = url_contains
        if load_state is not None:
            params["load_state"] = load_state
        if function is not None:
            params["function"] = function
        if timeout_ms is not None:
            params["timeout_ms"] = timeout_ms
            wait_s = timeout_ms / 1000.0
        else:
            wait_s = 5.0
        # Upstream's CLI gives its socket the deadline plus 8 s headroom
        # so the app handler reports the outcome; the harness mirrors the
        # pattern with its own margin on top.
        return dict(
            self._call("browser.wait", params, timeout_s=wait_s + 12.0) or {}
        )

    def browser_get_title(self, surface=None) -> dict:
        """The live document title (sync lane, like `browser.url.get`)."""
        params: Dict[str, Any] = {}
        if surface is not None:
            params["surface_id"] = self._resolve_surface_id(surface)
        return dict(self._call("browser.get.title", params) or {})

    def browser_screenshot(self, surface=None) -> dict:
        """Viewport PNG; the reply carries the staged file `path`."""
        params: Dict[str, Any] = {}
        if surface is not None:
            params["surface_id"] = self._resolve_surface_id(surface)
        return dict(self._call("browser.screenshot", params) or {})

    def browser_focus_webview(self, surface=None) -> dict:
        """Hand keyboard focus to the browser surface."""
        params: Dict[str, Any] = {}
        if surface is not None:
            params["surface_id"] = self._resolve_surface_id(surface)
        return dict(self._call("browser.focus_webview", params) or {})

    def browser_is_webview_focused(self, surface=None) -> dict:
        """Whether the surface is the active workspace's focused leaf."""
        params: Dict[str, Any] = {}
        if surface is not None:
            params["surface_id"] = self._resolve_surface_id(surface)
        return dict(self._call("browser.is_webview_focused", params) or {})
