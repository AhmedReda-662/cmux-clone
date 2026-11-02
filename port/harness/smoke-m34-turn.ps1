# M3.4 smoke: prompt-line turn detection in a live ConPTY pane.
# The pane shell is a runner that emits the exact Ollama turn-completion
# byte sequence (submission + model output, paced idle, idle placeholder,
# post-delivery chunk). Assert: exactly one
# `agent turn-complete ollama in workspace 1` line through the arrival
# policy (ring=true flash=true desktop=true), exactly-once despite later
# bytes, and no panic. Hermetic: temp env vars, unique pipe, owned PIDs.
$ErrorActionPreference = 'Stop'
$port = 'D:\workspace\Projects\cmux-clone\port'
$app = Join-Path $port 'target\debug\cmux-shell.exe'
$root = 'C:\Users\noah\AppData\Local\Temp\opencode\cmux-smoke-turn'
$fail = @()

if (-not (Test-Path $app)) { throw 'cmux-shell.exe missing' }
New-Item -ItemType Directory -Force -Path $root, "$root\probe-out", "$root\localappdata" | Out-Null

Remove-Item "$root\app-stderr.log" -Force -ErrorAction SilentlyContinue
Remove-Item "$root\probe-out\*" -Recurse -Force -ErrorAction SilentlyContinue
Remove-Item "$root\session.json" -Force -ErrorAction SilentlyContinue
Remove-Item "$root\done-mark.txt" -Force -ErrorAction SilentlyContinue
Remove-Item "$root\runner-crumb.txt" -Force -ErrorAction SilentlyContinue

# --- residue tap (wedge investigation, 2026-10-19): console-substrate
# leftovers from any earlier killed instance that a fresh launch then
# inherits; reported before every launch AND at fail time.
function Report-Residue {
    $oc = @(Get-Process OpenConsole -ErrorAction SilentlyContinue).Count
    $ch = @(Get-Process conhost -ErrorAction SilentlyContinue).Count
    $cs = @(Get-Process cmux-shell -ErrorAction SilentlyContinue).Count
    return "openconsole=$oc conhost=$ch cmux-shell=$cs"
}
Write-Output ("pre-launch residue: " + (Report-Residue))

function Kill-Tree([int]$pidValue) {
    try { cmd /c "taskkill /PID $pidValue /T /F" 2>&1 | Out-Null } catch { }
    Start-Sleep -Seconds 2
}

# --- emitter: upstream's real Ollama hand-over, powered by cmd only ---
# Detector semantics (upstream PromptLineTurnDetector + port test
# smoke_handover_sequence_announces_exactly_once): a waiting prompt that
# paints and sits — NEVER line-terminated — is the turn boundary; a
# terminated one is invalidated by its own CR. So:
#   t≈0     idle prompt + CRLF  -> detector arms (seeking→ready), no pending
#   t≈1     submission echo + response -> internal phase transitions only
#   t≈2     idle prompt, no CRLF -> pending opens (500 ms debounce)
#   +2s idle: delivery at +0.5s (frame wake -> deadline sweep); then
#   `done` arrives past the deadline — the runtime pre-confirms the stale
#   boundary without re-announcing (tee parity), delivery stays
#   exactly-once. `set /p` prints without a CRLF.
$runner = [IO.Path]::Combine($root, 'runner.cmd')
@'
@echo off
set CRUMB=%~dp0runner-crumb.txt
echo start> "%CRUMB%"
echo ^>^>^> Send a message (^/? for help)
echo arm>> "%CRUMB%"
timeout /t 1 /nobreak >nul
echo paced>> "%CRUMB%"
echo ^>^>^> Return FINAL_OLLAMA_OK
echo FINAL_OLLAMA_OK
echo echoes>> "%CRUMB%"
timeout /t 1 /nobreak >nul
echo before-idle>> "%CRUMB%"
<nul set /p =">>> Send a message (/? for help)"
echo idle>> "%CRUMB%"
timeout /t 2 /nobreak >nul
echo done
echo done>> "%CRUMB%"
> "%~dp0done-mark.txt" type nul
timeout /t 2 /nobreak >nul
echo tail>> "%CRUMB%"
'@ | Set-Content -Path $runner

$config = @{ terminal = @{ shell = @{ program = $runner } } } | ConvertTo-Json -Depth 5
[IO.File]::WriteAllText("$root\cmux.json", $config)

$env:CMUX_SESSION_FILE = "$root\session.json"
$env:CMUX_CONFIG_FILE = "$root\cmux.json"
$env:CMUX_SOCKET_PATH = '\\.\pipe\cmux-smoke-turn-a'
$env:LOCALAPPDATA = "$root\localappdata"
$env:CMUX_DEBUG_READER = '1'   # dev tap: capture exact chunk bytes for diagnosis

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class SmokeUser32 {
    [DllImport("user32.dll", SetLastError = false)]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    public static extern IntPtr FindWindowW(string cls, string title);
    [DllImport("user32.dll")]
    public static extern void keybd_event(byte bVk, byte bScan, uint dwFlags, UIntPtr dwExtraInfo);
}
'@

$p = Start-Process -FilePath $app -RedirectStandardError "$root\app-stderr.log" -PassThru
$sw = [System.Diagnostics.Stopwatch]::StartNew()
Write-Output ("watch: app started at {0:F3}" -f ($sw.Elapsed.TotalSeconds))

# --- focus pin (M4.3 fixture fix, 2026-10-20): the arrival policy keys the
# desktop effect on the app's OS window focus at delivery (upstream parity:
# a focused surface gets inline feedback only). A plain launch occasionally
# puts the new window in the foreground before the delivery instant, which
# flips the expected `desktop=true` to focused-inline — seen in a
# regression re-run (focused-inline t=1.831s vs green desktop=true).
# Pin the scene: wait for the app window, then move the foreground to the
# shell desktop once and for all (the ALT-trick makes SetForegroundWindow
# stick from a caller that is not the foreground process). Harmless when
# the app never took focus on the runs that behave.
$appHwnd = [IntPtr]::Zero
for ($i = 0; $i -lt 40; $i++) {
    Start-Sleep -Milliseconds 100
    $live = Get-Process -Id $p.Id -ErrorAction SilentlyContinue
    if ($live -and $live.MainWindowHandle -ne 0) { $appHwnd = $live.MainWindowHandle; break }
}
Start-Sleep -Milliseconds 500   # let the app's own initial activation settle
$target = [SmokeUser32]::FindWindowW('Progman', $null)
if ($target -eq [IntPtr]::Zero) { $target = [SmokeUser32]::FindWindowW('Shell_TrayWnd', $null) }
if ($target -ne [IntPtr]::Zero) {
    [SmokeUser32]::keybd_event(0x12, 0, 0, [UIntPtr]::Zero)   # ALT down
    $null = [SmokeUser32]::SetForegroundWindow($target)
    Start-Sleep -Milliseconds 80
    [SmokeUser32]::keybd_event(0x12, 0, 2, [UIntPtr]::Zero)   # ALT up
    Write-Output ("watch: foreground pinned away from the app at {0:F3}" -f ($sw.Elapsed.TotalSeconds))
} else {
    Write-Output 'watch: WARNING no desktop/taskbar window to pin the foreground to'
}

try {
    # Plain launch: no hidden window, no activation — the exact app
    # launch a user gets.
    # The emitter finishes ~5.3s after the pane's shell starts.
    $deadline = (Get-Date).AddSeconds(30)
    while ((Get-Date) -lt $deadline -and -not (Test-Path "$root\done-mark.txt")) {
        Start-Sleep -Milliseconds 300
    }
    if (-not (Test-Path "$root\done-mark.txt")) {
        $fail += 'emitter never finished (no done mark within 30s)'
    }
    Start-Sleep -Seconds 2   # let the UI frame loop settle after the mark

    $stderr = if (Test-Path "$root\app-stderr.log") { Get-Content "$root\app-stderr.log" } else { @() }
    Write-Output ("watch: read1 at {0:F3}" -f ($sw.Elapsed.TotalSeconds))
    Write-Output ("log1 bytes: " + (Get-Item "$root\app-stderr.log" -ErrorAction SilentlyContinue).Length)
    $turns = @($stderr | Where-Object { $_ -match 'agent turn-complete ollama' })
    if ($turns.Count -ne 1) {
        # Late-delivery discriminator: re-read once after a settle window;
        # a count that flips to 1 means the delivery happened after the
        # first read (frame-loop wake racing the smoke's own timeline),
        # not absence.
        Start-Sleep -Seconds 5
        $stderr2 = if (Test-Path "$root\app-stderr.log") { Get-Content "$root\app-stderr.log" } else { @() }
        $turns2 = @($stderr2 | Where-Object { $_ -match 'agent turn-complete ollama' })
        Write-Output ("watch: read2 at {0:F3}" -f ($sw.Elapsed.TotalSeconds))
        Write-Output ("log2 bytes: " + (Get-Item "$root\app-stderr.log" -ErrorAction SilentlyContinue).Length)
        if ($turns2.Count -eq 1) { $turns = $turns2; $stderr = $stderr2; $fail += 'LATE: turn-complete appeared only on re-read (+5s)' }
    }
    if ($turns.Count -ne 1) {
        $fail += "expected exactly 1 turn-complete line, got $($turns.Count): $($turns -join ' ;; ')"
    }
    if ($turns.Count -eq 1) {
        if ($turns[0] -notmatch 'desktop=true') { $fail += "desktop effect not in log: $($turns[0])" }
        if ($turns[0] -notmatch 'ring=true') { $fail += "ring (mark_unread) effect not in log: $($turns[0])" }
        if ($turns[0] -notmatch 'in workspace 1 ') { $fail += "wrong workspace in log: $($turns[0])" }
    }
    if (@($stderr | Where-Object { $_ -match 'panicked' }).Count -gt 0) {
        $fail += 'app panicked during the smoke'
    }
    if ($turns.Count -eq 0) {
        Write-Output ("fail-time residue: " + (Report-Residue))
        # Wedge forensics: snapshot the app's process tree at fail time.
        Write-Output "--- fail-time process tree under app pid $($p.Id) ---"
        $all = Get-CimInstance Win32_Process
        $map = @{}
        foreach ($proc in $all) { $map[[UInt32]$proc.ProcessId] = $proc }
        foreach ($proc in $all) {
            $cur = $proc
            $isDescendant = $false
            for ($i = 0; $i -lt 6 -and ($null -ne $cur); $i++) {
                if ($cur.ProcessId -eq $p.Id) { $isDescendant = $true; break }
                if ($cur.ParentProcessId -eq 0) { break }
                $cur = $map[[UInt32]$cur.ParentProcessId]
            }
            if ($isDescendant) {
                Write-Output ("  pid={0} name={1} parent={2} created={3} cmd={4}" -f $proc.ProcessId, $proc.Name, $proc.ParentProcessId, $proc.CreationDate, $proc.CommandLine)
            }
        }
        Write-Output "--- crumb: $((Get-Content "$root\runner-crumb.txt" -ErrorAction SilentlyContinue) -join ' ') ---"
        Write-Output "--- reader tail ---"
        Get-Content "$root\app-stderr.log" -ErrorAction SilentlyContinue | Select-Object -Last 5 | ForEach-Object {
            Write-Output ("  " + ($_ -replace '^cmux-shell: reader chunk .', 'chunk ').Replace('cmux-shell: ',''))
        }
    }
} finally {
    Kill-Tree $p.Id
}

Remove-Item Env:CMUX_SESSION_FILE -ErrorAction SilentlyContinue
Remove-Item Env:CMUX_CONFIG_FILE -ErrorAction SilentlyContinue
Remove-Item Env:CMUX_SOCKET_PATH -ErrorAction SilentlyContinue
Remove-Item Env:LOCALAPPDATA -ErrorAction SilentlyContinue

if ($fail.Count -eq 0) { Write-Output 'SMOKE RESULT: PASS' } else { Write-Output ("SMOKE RESULT: FAIL " + ($fail -join ' | ')) }
exit $(if ($fail.Count -eq 0) { 0 } else { 1 })
