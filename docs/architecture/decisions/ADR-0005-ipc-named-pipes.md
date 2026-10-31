# ADR-0005: Local IPC transport for the port (named pipes)

**Status:** Proposed (M4 scope for the full contract; **slices landed and
verified: M3.3 hook transport (Amendment 1), M4.1 automation subset
(Amendment 2), M4.2 auth modes + DPAPI store (Amendment 3), M4.4 concurrent
connection pool (Amendment 4)**)
**Date:** 2026-10-17 (Amendment 3: 2026-10-20, Amendment 4: 2026-10-20)

## Context

Upstream's automation surface is a **Unix-domain socket**
(`Packages/macOS/CmuxControlSocket`): JSON-lines v2 protocol
(`{"id","method","params"}`), command coordinators, and a security model of
`off | cmuxOnly | automation | password | allowAll` with peer identity
verified via `LOCAL_PEERTOKEN` (audit token), `LOCAL_PEERPID`
(`sysctl` ppid walk, ≤128 levels) and `LOCAL_PEERCRED` (same-UID). Password
secrets live in the macOS Keychain. CLI discovery order:
`--socket` → `CMUX_SOCKET_PATH` → known candidates.

The protocol and dispatch design are portable. The transport and peer-identity
primitives are not: Windows AF_UNIX cannot provide the audit-token/PID
identity guarantees the reference relies on.

## Decision (proposed)

- **Transport: overlapped named pipes** (`tokio::net::windows::named_pipes`),
  per-user well-known name mimicking the reference scheme, e.g.
  `\\.\pipe\cmux\<user-scope>\cmux.pipe` with the same environment
  override (`CMUX_SOCKET_PATH`) and candidate fallbacks implemented in the
  port CLI.
- **Security model, ported by mode:**
  - `cmuxOnly` + owner-only modes: DACL restricting the pipe to the current
    user SID (so no cross-user clients), plus client-identity check via
    `GetNamedPipeClientProcessId` + process-tree ancestry walk to the server
    PID (Windows analog of the reference's LOCAL_PEERPID logic).
  - `password`: same `auth.login` handshake as v2, SHA-256 credential
    fingerprint, constant-time compare, re-verify on rotation (direct port).
    Secret storage: Credential Manager/DPAPI (Keychain analog) — utility
    choice deferred to implementation with a security review.
  - `allowAll`: wide-open DACL equivalent — discouraged default. Default
    mode remains the conservative one (reference parity: `cmuxOnly`).
- **Protocol: unchanged v2 JSON-lines** so the port can be driven by the same
  Python harness pattern `cmux-main/tests_v2/cmux.py` uses (test strategy L4),
  and errors keep the `{ok:false,error:{code,message,action,data}}` shape.

## Consequences

- Process-ancestry checks need a Citooling pass: PID reuse races are narrower
  on Windows (unique process IDs per epoch) but re-opened handles must be
  verified; design and tests in `cmux-ipc` before any unknown client is
  honored (manifest §11: validate/authorize every IPC request; never execute
  commands from IPC without validation).
- CLI `cmux` (cmux-cli crate) implements the contract subset from
  `cmux-main/docs/cli-contract.md` documented in feature-parity #16.

## Alternatives considered

1. **AF_UNIX via `uds_windows`** (already used by some `cmux-tui` crates) —
   rejected as the *security* transport: peer-identity APIs and path
   ownership semantics do not reproduce the reference guarantees; kept as an
   optional internal client convenience, not the control surface.
2. **Local TCP loopback** — rejected: token-free cross-process accessibility
   and firewall surface; strictly worse than pipes.
3. **Reuse `SocketControlSettings` marker-file scheme** — ported as-is where
   applicable (state dir under `%LOCALAPPDATA%`), independent of transport.

## Amendment 1 (2026-10-19): first slice landed — hook transport only

**Status remains Proposed** for the full contract below; the first slice is
landed, verified, and narrower than this ADR:

- `port/crates/cmux-ipc` (tokio free; one synchronous serve loop thread):
  JSON-lines v2 envelope, `ping` + `agent.hook.enqueue` only; DACL
  `D:P(A;;FA;;;<user-SID>)`; ancestry depth capped at 128 via Toolhelp snapshots;
  bounded requests (1 MiB envelope, 64 KiB payload, 0.5 s reply budget).
  Default pipe name `\\.\pipe\cmux-<user-SID>`; `CMUX_SOCKET_PATH` override
  (which also disables the ancestry gate as a smoke/multi-instance
  affordance — documented divergence).
- `port/crates/cmux-cli` (binary `cmux`): `ping` (exit 0 only on `ok:true`),
  `hooks enqueue <agent> <sub>` (always exit 0, fail-open `{}`; the
  spawn/ancestry gates of the upstream shell guard run client-side:
  `CMUX_SURFACE_ID` presence + per-agent disable var) and
  `hooks codex install|uninstall` (see ADR-0011 Amendment 3).
- `cmux-shell` starts the server at startup with the handler pushing into an
  mpsc drained by the same `policy::resolve` pipeline as OSC notifications
  (ADR-0011 Amendment 3); a second instance on the same name loses IPC
  silently (logged) and panes keep pointing at the first instance's pipe —
  matching upstream's single-daemon ownership of the socket file.

Deferred to M4: the rest of the v2 command surface (command coordinators,
auth modes/handshake, `automation`/`password`, Keychain-alike credential
storage) — none of which the hook slice exercises.

## Amendment 2 (2026-10-20): M4.1 automation slice landed — method subset + app request plane + harness

**Status stays Proposed** for the full contract (auth modes/handshake, socket
pool, `automation` mode); the M4.1 slice is landed and live-verified against
the upstream harness's protocol expectations:

- **Method surface** (`cmux-ipc/protocol.rs`): `ping` (V1) and `system.ping`
  (V2 alias) answered inline; `system.capabilities` answered statically
  (`{"protocol":"cmux-socket","version":2,"methods":[sorted]}`); the
  app-plane subset `system.identify`, `workspace.list/current/select/create`,
  `surface.list`, `pane.list`, `surface.send_text`, `surface.read_text`
  parses and validates in `parse_app_request` with the upstream error shape
  `{code,message,action}` (`invalid_params` / `not_found` / `io_error` /
  `timeout`); unknown methods answer `unknown_method` with a capabilities
  hint.
- **Dispatch** (`cmux-ipc/server.rs`): `HandlerFn` widened to
  `Fn(IpcCall) -> Option<Result<Value, RequestProblem>>` where
  `IpcCall = Enqueue(EnqueueRequest) | App(AppCall)`. Enqueue keeps the
  fail-open `queued:true` reply; an app call is forwarded to the handler and
  `None` (no answer within the app's budget) becomes a structured `timeout`
  error envelope — the client always reads a well-formed line.
- **App request plane** (cmux-shell): the server thread parks per app call
  (`APP_REPLY_BUDGET` = 400 ms, deliberately under the CLI client's 500 ms
  response read so the client sees either the result or the timeout), hands
  the call over an mpsc to the frame loop, and wakes it (`FrameWake`); the
  frame loop executes in `poll_children` (`serve_ipc_calls`) on the thread
  that owns all shell/terminal state — no locks added. `workspace.create`
  reuses the exact `Action::NewWorkspace` path (extracted
  `spawn_new_workspace`), so created workspaces behave like upstream
  `newTab` (pushed last, becomes active, focus settles).
- **Identity**: pane ids are minted by a process-global counter (`p1`
  first), making ids unique across workspaces — this repairs the
  pre-existing defect where every workspace reused `p1`, which silently
  broke by-surface hook routing the moment a second workspace existed;
  workspaces carry minted `w{n}` ids (upstream wire uses UUIDs — documented
  divergence), restored from snapshot ids when present, and both counters
  are seeded above every restored id during restore so later mints cannot
  collide.
- **Row shapes** mirror what `cmux-main/tests_v2/cmux.py` consumes:
  `workspace.list` rows `{index,id,title,selected}`; `surface.list` rows
  `{index,id,...}` (the port maps panes 1:1 to terminal surfaces, so
  `pane.list` repeats the surface id); `workspace.create` replies
  `{workspace_id,surface_id}`; `system.identify` carries
  `focused{surface_id,pane_id,workspace_id,...}`. `surface_id` addressing
  accepts exact id, `surface:<id>`, `surface:<row index>`, and bare row
  indexes (rows emitted in workspace/leaf order). `read_text` returns the
  visible grid only — no scrollback, no selection (`read-screen` parity,
  documented divergence from a full-buffer reader).
- **CLI** (`cmux`): `identify [--json]`, `workspace list|new|current|select`,
  `surface list`, `send [--surface <spec>] <text...>`,
  `read-screen [--surface <spec>] [--json]` — upstream conventions: result
  body only by default, complete envelope with `--json`, structured errors
  on stderr with exit 1.
- **Harness** (`port/harness/cmux.py` + `test_automation_m4.py`): a
  stdlib-only ctypes named-pipe client (64-bit-safe `CreateFileW` handles,
  `PeekNamedPipe`-polled deadline reads) mirroring the upstream harness API
  subset and `cmuxError` semantics; the live smoke drives a real
  `cmux-shell.exe` end-to-end (16/16 checks PASS) with isolated
  `CMUX_SOCKET_PATH`/session/config envs and job-object collapse teardown.
- Verified: protocol/dispatch/mint/row unit tests (+12), the harness live
  smoke (16/16), a CLI-drive smoke (8/8), and the full M3 battery (9/9)
  against a rebuilt workspace (fmt/clippy `-D warnings`/cargo test green,
  223 tests).
- Still open for M4.2: `--password`/`automation` auth modes + DPAPI
  credential storage (per the full contract's auth section), the
  multi-instance pipe pool, `surface.type`/selection verbs, and the
  browser-lane methods.

## Amendment 3 (2026-10-20): M4.2 auth slice landed — password/automation modes + DPAPI store

**Status stays Proposed** for the remainder of the full contract (socket
pool, `allowAll`, `surface.type`/selection verbs, browser-lane methods); the
auth slice is landed, security-reviewed with the owner (see Review below),
and upstream-verified down to the exact wire strings:

- **Modes** (`cmux-ipc::AuthMode`, upstream `SocketControlMode` minus
  `off`/`allowAll`): `cmuxOnly` = user-SID DACL **+** ancestry gate
  (day-one default, unchanged); `automation` = user-SID DACL only — any
  same-user client is admitted (upstream `.automation` checks same-UID
  alone); `password` = `automation` admission plus the login handshake and
  per-command credential rotation checks. A started server is never `off`;
  `allowAll` (cross-user DACL) stays deferred — no consumer, contrary to
  the manifest's secure-defaults policy.
- **Credential store** (`cmux-ipc/password.rs`, upstream
  `SocketControlPasswordStore` port): sources in upstream priority —
  explicit (`--password` favorite) → `CMUX_SOCKET_PASSWORD` env
  (`socketPasswordEnvKey`) → password **file**, relocated per the ratified
  security review from upstream's plaintext 0600
  `<state>/socket-control-password` to `%APPDATA%\cmux\socket-control-password`
  as a **DPAPI user-scope blob** (`CryptProtectData`/`CryptUnprotectData`,
  `CRYPTPROTECT_UI_FORBIDDEN`, description
  `cmux.socket-control-password.v1`), relocatable via `CMUX_PASSWORD_FILE`.
  Absent/corrupt/undecryptable reads as *unconfigured* (upstream's
  `(try? loadPassword())` swallow) — `auth_unconfigured`, never fail-open.
  Zero new crates: DPAPI via `crypt32`, SHA-256 fingerprints via CNG
  `bcrypt` (`Win32_Security_Cryptography` feature only).
- **Handshake, before dispatch, per connection** (upstream
  `authResponseIfNeeded` port in `cmux-ipc/server.rs`; a connection now
  speaks multiple request/reply line pairs — the CLI and harness send
  `auth` + request on one connection, which also required the multi-line
  serve loop):
  - V2 `{"id","method":"auth.login","params":{"password":…}}` →
    `{"id","ok":true,"result":{"authenticated":true}}`; errors (upstream
    text verbatim): `invalid_params` "auth.login requires params.password",
    `auth_unconfigured` "Password mode is enabled but no socket password is
    configured in Settings.", `auth_failed` "Invalid password".
  - V1 plain `auth <pw>` → `OK: Authenticated` / `ERROR: Invalid password`
    / `ERROR: Missing password. Usage: auth <password>` / the
    unconfigured text (unconfigured beats missing-password, upstream
    precedence).
  - Any other command from an unauthenticated connection → `auth_required`
    "Authentication required. Send auth <password> first." (structured
    envelope for parsed JSON envelopes; plain `ERROR:` text otherwise).
  - **Rotation**: the connection stores the SHA-256 fingerprint of the
    verified password (`SocketPasswordAuthorization` port); per served
    command the live credential is re-resolved and compared
    constant-time (upstream `constantTimeEquals`: length-independent,
    full-byte scan); on mismatch or clearance the connection is refused
    without a reply — upstream closes the connection.
  - Verification compares the raw wire candidate against the normalized
    configured value — upstream does not trim the candidate either.
  - No lockout counter: failed logins stay connected and may retry
    (upstream behavior; brute-force barring is out of scope for a
    same-user boundary).
- **Non-password modes still serve the login surface** (upstream serves
  `auth.login` in every mode via the system coordinator): envelope
  `auth.login` answers `{"authenticated":true,"required":false}` and
  `system.capabilities` advertises `auth.login` (14 methods now); a plain
  `auth …` line answers upstream's exact `ERROR: Unknown command 'auth'`,
  which is the string the CLI tolerates when a resolved password meets a
  non-password server.
- **CLI** (`cmux-cli`): global flags usable before or after any verb —
  `--socket <pipe>` / `--socket=<pipe>` (flag → `CMUX_SOCKET_PATH` → the
  per-user pipe, upstream discovery head) and `--password <pw>` /
  `--password=<pw>` (explicit → `CMUX_SOCKET_PASSWORD` env → DPAPI file,
  upstream `SocketPasswordResolver` order minus the retired Keychain
  legacy source). With a resolved password every command (including hook
  enqueues) sends the V1 `auth <pw>` probe on its connection and treats
  any `ERROR:` reply as a verb failure **except** the `Unknown command
  'auth'` tolerance. Blank values count as absent; login failures exit 1
  with the server's text.
- **Shell wiring**: `CMUX_SOCKET_MODE=cmuxOnly|automation|password` (exact
  upstream values; unknown values warn and fall back to the default;
  `off` behaves like `CMUX_IPC_DISABLED=1` and disables the server).
  Preserved behavior: `CMUX_SOCKET_PATH` override without an explicit mode
  keeps its M3.3 "no ancestry gate" semantics, which is now the named
  `automation` mode; an explicit mode always wins. The provider resolves
  the credential live on every check (env/file edits rotate without
  restart); log lines carry the mode name only — never any secret.
- **Harness**: `cmux.cmux(..., password=…)` performs the `auth.login`
  handshake before every call on the same connection and raises
  `cmuxError` with the server code on refusal; `test_automation_m42_password.py`
  live-drives both modes plus the three CLI contract checks.
- **Hooks + password mode**: hook CLI invocations resolve the password
  through the same precedence; with no sources configured the enqueue
  still fails open (`{}`, exit 0) — password mode adds no agent-blocking
  path beyond what upstream's hook admission does. `cmuxOnly` remains the
  default, so day-one hook behavior is untouched.
- **Verified** (2026-10-20): fmt/clippy `-D warnings`/`cargo test` green —
  **238 tests** (+15: 8 password-store, 7 server-level auth enforcement
  including rotation-refusal, V1 texts, non-password ack, ancestry
  independence, CLI tolerance); live smokes: M4.2 password/automation CLI
  13/13, M4.1 harness 16/16, CLI 8/8, M3 battery 9/9.
- **Deferred**: multi-instance pipe pool (clients hold connections for
  milliseconds; single-instance serialization unchanged), `allowAll`
  (wide-open DACL, no consumer), upstream's HMAC capability tokens
  (`SocketClientCapabilityAuthority`) — that layer mitigates macOS
  multiplexer reparenting, which has no Windows analog; ancestry +
  password modes cover the port's cases.

### Security review record (manifest §12, owner-approved 2026-10-20)

Questions put to the owner with recommendations and ratification:

1. **At-rest credential store** → ratified **DPAPI-protected file**
   (alternatives: upstream-exact plaintext 0600 file — weaker at rest on
   copied media; Credential Manager — same protection class, more moving
   parts, harder smoke isolation; env-only now — defers parity).
2. **Mode selection plumbing** → ratified **`CMUX_SOCKET_MODE` env**
   (config-file keys when the settings milestone lands; shell stays
   argument-free like today).
3. **CLI global flags** → ratified **both `--socket` and `--password`**
   (upstream pins them; the upstream harness test exercises exactly this).

Threat model notes recorded at implementation time:

- The DACL boundary (other users) is unchanged in every mode; `password`
  mode's boundary is **same-user non-descendant processes** — identical to
  upstream's `.password` intent (POSIX cannot stop same-UID processes
  either; the handshake is the gate there as here).
- Any same-user process can read a DPAPI-user-scope blob (and upstream's
  0600 file); that boundary is not claimable on this platform and is not
  claimed.
- `--password` on a command line is visible to same-user processes via
  process-query surfaces — the same exposure class as upstream's env var;
  documented for steady use: env or DPAPI file, flag for smokes/tests.
- Fingerprints are SHA-256 of the credential (upstream-equivalent),
  never logged; no secret or candidate material reaches any log line;
  rotation refusals disconnect silently.
- Fail-closed paths: no credential → `auth_unconfigured`; wrong credential
  → `auth_failed`; corrupt file → unconfigured; rotated credential →
  connection refused. No mode widens what an unauthenticated caller
  reaches, and nothing new executes without the existing validation
  layers (`parse_app_request`/`validate_enqueue_params`).

## Amendment 4 (2026-10-20): M4.4 concurrent connection pool landed

**Status stays Proposed** for the remainder of the full contract
(`allowAll`, `surface.type`/selection verbs, browser-lane methods); the
connection pool is landed, closing the last deferred M4 item except those
explicitly out of scope:

- **Upstream contract shaped here** (cli-contract.md §"Control-socket
  admission and deadlines"): the app never lets one control command block
  the others; every accepted connection gets a real reply; saturation
  answers an in-band structured `overloaded` error. The port's until-now
  shape — **one** pipe instance (`nMaxInstances = 1`, create → connect →
  serve → disconnect loop) — violated the first two: *every* connection
  was serialized, and a connected-but-silent client parked the whole
  server indefinitely (a same-user peer could wedge IPC with one open
  handle and zero writes).
- **Port shape** (`cmux-ipc/server.rs`): the pipe name holds
  `PIPE_INSTANCES = 8` byte-mode instances, each with its own serve
  thread (`serve_connected`: connect → client-PID → ancestry gate →
  `serve_one_client` → disconnect, unchanged per-connection semantics —
  auth, rotation checks, and validation are per connection exactly as in
  Amendment 3). The **name claim** stays on the main thread before any
  worker exists: the claimant passes `FILE_FLAG_FIRST_PIPE_INSTANCE`, so a
  second app instance still loses with the same "IPC disabled" path
  (workers, which claim=false, would otherwise join the *foreign* pipe and
  serve foreign clients under this server's config), and every create of
  the name passes the same `PIPE_INSTANCES` count (the first instance
  fixes the counts; later creates must agree).
- **Shared state**: `HandlerFn` is now
  `Arc<dyn Fn(IpcCall) -> … + Send + Sync>` (one handler across pool
  threads), `PasswordProvider` likewise `Arc`d, `ServerConfig` `Clone` —
  no new locks; the app request plane is unchanged (concurrent
  connection threads funnel into the same mpsc/frame-loop drain).
- **Saturation divergence, recorded**: beyond 8 concurrent connections a
  client fails at the OS open (`ERROR_PIPE_BUSY`; all instances busy) and
  the shared client retry loop (`open_with_retries`, 40 ms steps within
  the connect budget) returns `Unavailable`. The port **never emits an
  in-band `overloaded` reply** — the equivalent OS-side boundary sits
  *before* the connection exists, matching upstream's pool-capacity
  limiting in effect (8 live slots) if not in wire text. Revisit only if
  a real consumer needs >8 simultaneous connections.
- **Residual, accepted**: a same-user client can still hold all 8 slots
  open silently (a bounded claim on a same-user boundary; DACL/ancestry
  unchanged). Upstream has the same class (bounded pool ⇒
  `pool_saturated`), so this is a compatibility of limits, not a new
  hole; per-peer preauthorization caps (`preauthorization_saturated`)
  have no reachable analog at the Windows open boundary and stay
  unimplemented.
- **Harness client connection shape, recorded — owner decision open**:
  upstream's Python harness client keeps **one persistent socket per
  client instance** (`tests_v2/cmux.py`: `connect()` reuses `_socket`,
  `close()` releases it; the password handshake runs once per client
  lifetime), while the port's harness opens **one connection per call**
  (`port/harness/cmux.py::_call` connects, performs any login handshake,
  and closes in a `finally`). Functional parity holds — upstream's CLI
  is also one connection per command, and the pooled server serves
  either shape — but the per-call model pays a connect + (password mode)
  re-handshake per call and diverges from the harness API's connection
  semantics. **Decision (owner, 2026-10-21): divergence accepted for M4;
  the persistent-socket client shape folds into the M5.2
  automation-lane design**, where bursty `browser.*` calls would
  otherwise repeat connects/handshakes.
- **Verified** (2026-10-20): fmt/clippy `-D warnings`/`cargo test` green —
  **247 tests** (+2: `a_parked_request_does_not_hold_the_pipe` — a 900 ms
  stalled first connection with a second client answered well inside it;
  `every_pooled_connection_receives_its_own_reply` — 4 concurrent
  in-flight requests, each envelope carrying its issuer's id). Live smoke
  `harness/test_pool_m44.py` 8/8 twice on the final binary: 32 pooled
  exchanges, CLI ping in 0.01–0.05 s while a silent peer holds a slot,
  same-name second instance logs `IPC disabled` while the first serves,
  no panic. Baselines re-run on the same binary: M4.1 16/16, M4.2 13/13,
  CLI 8/8, M3.4 9/9, M4.3 terminal-UX 19/19.

## Amendment 5 (2026-10-22): persistent harness client landed (Am. 4 owner decision executed)

**Status: landed.** `port/harness/cmux.py` now keeps **one named-pipe
connection per client instance**, exactly the shape recorded in Am. 4 as
upstream's: `connect()` opens lazily and reuses the pipe, `close()`
releases it, and the password handshake (`auth.login`) runs once per
connection — so a fresh pipe re-handshakes at its next `connect()`, which
is what "once per client lifetime" becomes in a resettable transport.

- **Transport-reset rule** (recorded so the semantics are stable):
  a transport-level failure (write error, read error/timeout, or an
  unparseable reply line) closes the pipe; the next call reconnects and
  re-handshakes. A cleanly parsed non-ok reply (`unknown_method`,
  `invalid_state`, …) is a normal conversation and KEEPS the pipe.
  A mismatched reply id also resets (protocol corruption).
- **Compatibility**: every M4 harness smoke re-ran green on this shape —
  M4.1 16/16, M4.2 13/13 (password flows handshake once and fail fast on
  a refused login, closing the pipe), M4.3 19/19, M4.4 pool 8/8 (the
  pool smoke's one-client-per-thread is unchanged by per-client reuse),
  CLI 8/8, M3.4 turn PASS, browser 60/60. The automation-lane motivator
  from Am. 4 holds: the M5.2 smoke's five back-to-back `browser.eval`
  calls (plus ~30 more automation/listing calls) ride one connection;
  the measured client round trips (10.5–21.0 ms, ADR-0006 Amendment 3)
  would each have paid a connect + handshake under the old shape.
- **Server side**: unchanged. Per-connection auth state and the pool
  already accepted both shapes; this lands only the client divergence
  recorded in Am. 4 (owner decision: fold into M5.2 — executed).
