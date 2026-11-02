# M4.1 CLI smoke + the M6 CLI browser section (ADR-0006 Amendment 6) and
# the custom-command section (ADR-0008 Amendment 1):
# the port CLI's automation verbs against a live app; browser verbs run
# against a file:// fixture page (no server dependency).
# Hermetic: unique pipe override, isolated session + config, manual teardown.
$ErrorActionPreference = 'Stop'
$port = 'D:\workspace\Projects\cmux-clone\port'
$shellBin = Join-Path $port 'target\debug\cmux-shell.exe'
$cliBin = Join-Path $port 'target\debug\cmux.exe'
if (-not (Test-Path $shellBin)) { $shellBin = Join-Path $port 'target\release\cmux-shell.exe' }
if (-not (Test-Path $cliBin)) { $cliBin = Join-Path $port 'target\release\cmux.exe' }

$pipe = "\\.\pipe\cmux-m41-cli-smoke-$PID-$(Get-Random)"
$fixture = Join-Path $env:TEMP ("cmux-m41-cli-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $fixture | Out-Null

$pass = 0; $fail = 0
function Check($name, $condition, $detail = '') {
    if ($condition) { $script:pass++; Write-Output "PASS $name" }
    else { $script:fail++; Write-Output "FAIL $name $detail" }
}

$env:CMUX_SOCKET_PATH = $pipe
$env:CMUX_SESSION_FILE = Join-Path $fixture 'session.json'
$env:CMUX_CONFIG_FILE = Join-Path $fixture 'config.json5'
Remove-Item Env:CMUX_SHELL_AUTO_EXIT_MS -ErrorAction SilentlyContinue

# The M6 command section drives the app's config-directed verbs: write
# the fixture config BEFORE the app launches (the startup copy warms
# the template lane; the verbs themselves re-read at call time).
Set-Content -Path (Join-Path $fixture 'config.json5') -Encoding Ascii -Value @'
{
  // JSONC on read: this comment is tolerated like upstream.
  "commands": [
    { "name": "m6-echo", "description": "echo marker", "command": "echo cmd-m6-alpha" },
    { "name": "m6-guarded", "description": "needs --yes", "command": "echo cmd-m6-confirmed", "confirm": true },
    { "name": "m6-space", "workspace": {} }
  ]
}
'@

$log = Join-Path $fixture 'app-stderr.log'
$proc = Start-Process -FilePath $shellBin -WorkingDirectory $fixture -RedirectStandardError $log -PassThru -WindowStyle Hidden
try {
    $ok = $false
    for ($i = 0; $i -lt 150; $i++) {
        & $cliBin ping *> $null
        if ($LASTEXITCODE -eq 0) { $ok = $true; break }
        Start-Sleep -Milliseconds 200
    }
    Check 'cli ping probes the app' $ok

    # The app's shell-startup PTY burst can push the FIRST app-plane call
    # past the reply budget once spawn has begun (measured 478 ms in that
    # window vs 11-15 ms once the app settles); the py harness retries
    # app-plane calls for the same documented reason.
    $identify = ''
    # The burst-timeout stderr line surfaces as a NativeCommandError in
    # the redirected stream; the script's Stop preference turns it
    # terminating. Continue-scoped around the call (the retry loop is
    # the documented handling; the JSON envelope on stdout decides).
    $oldEap = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    for ($attempt = 1; $attempt -le 4; $attempt++) {
        $identify = & $cliBin identify --json 2>$null | Out-String
        if ($identify -match '"ok":true') { break }
        Start-Sleep -Milliseconds 400
    }
    $ErrorActionPreference = $oldEap
    Check 'cli identify --json keeps the envelope' ($identify -match '"ok":true' -and $identify -match '"app":"cmux"') $identify

    $new = & $cliBin workspace new --json | Out-String
    Check 'cli workspace new reports ids' ($new -match '"workspace_id":"w' -and $new -match '"surface_id":"p') $new
    $wsid = if ($new -match '"workspace_id":"([^"]+)"') { $Matches[1] } else { '' }
    $sid = if ($new -match '"surface_id":"([^"]+)"') { $Matches[1] } else { '' }

    $list = & $cliBin workspace list --json | Out-String
    Check 'cli workspace list rows' ($list -match '"selected":true' -and $list -match [regex]::Escape($wsid)) $list

    $current = & $cliBin workspace current | Out-String
    Check 'cli workspace current' ($current -match [regex]::Escape($wsid)) $current

    $null = & $cliBin send --surface $sid "echo cli-m41-MARKER"
    Start-Sleep -Milliseconds 1500
    $screen = & $cliBin read-screen --surface $sid
    $screenText = $screen -join "`n"
    Check 'cli send + read-screen round trip' ($screenText -match 'cli-m41-MARKER') $screenText

    $errout = cmd /c "`"$cliBin`" read-screen --surface p9 2>&1" | Out-String
    Check 'cli read-screen structured failure' ($LASTEXITCODE -eq 1 -and $errout -match 'not_found') $errout

    $null = cmd /c "`"$cliBin`" workspace select nope >nul 2>&1"
    Check 'cli select unknown exits 1' ($LASTEXITCODE -eq 1)

    # ==== M6 CLI browser section (cmux.exe browser grammar; the app binary
    # is unchanged - this exercises the CLI wrappers over the landed wire).
    # Pane spawn/navigate can take past one reply budget on a cold WebView2,
    # so open and navigate get a small retry (retries are idempotent: open
    # makes redundant panes, navigate repeats the same URL).
    $page = Join-Path $fixture 'page.html'
    $html = @'
<!doctype html>
<html>
<head><title>CLI Browser Fixture</title></head>
<body>
  <h1>Fixture</h1>
  <input id="name" name="name" value="">
  <select id="pick"><option value="v1">one</option><option value="v2">two</option></select>
  <button id="send" onclick="window.__clicks=(window.__clicks||0)+1">Send</button>
  <input id="cb" type="checkbox" checked>
  <div id="log"></div>
  <script>
    document.addEventListener('keydown', function (e) {
      if (e.key === 'Enter') { window.__enter = true; }
      var log = document.getElementById('log');
      log.textContent = log.textContent + e.key;
    });
  </script>
</body>
</html>
'@
    Set-Content -Path $page -Value $html -Encoding Ascii
    $page2 = Join-Path $fixture 'page2.html'
    $html2 = @'
<!doctype html>
<html>
<head><title>Fixture Two</title></head>
<body><p id="two">second page</p></body>
</html>
'@
    Set-Content -Path $page2 -Value $html2 -Encoding Ascii
    $fileUrl = ([uri]$page).AbsoluteUri
    $fileUrl2 = ([uri]$page2).AbsoluteUri

    $open = ''
    for ($attempt = 1; $attempt -le 3; $attempt++) {
        $open = & $cliBin browser open --json | Out-String
        if ($open -match '"ok":true') { break }
        Start-Sleep -Milliseconds 400
    }
    Check 'cli browser open --json reports the new browser pane' ($open -match '"ok":true' -and $open -match '"pane_id"' -and $open -match '"workspace_id"' -and $open -match '"type":"browser"') $open
    $bsid = if ($open -match '"surface_id":"([^"]+)"') { $Matches[1] } else { '' }

    $plainOpen = & $cliBin browser open http://example.invalid | Out-String
    Check 'cli browser plain open prints surface/pane/placement' ($plainOpen -match 'OK surface=\S+ pane=\S+ placement=split') $plainOpen

    $nav = ''
    for ($attempt = 1; $attempt -le 3; $attempt++) {
        $nav = & $cliBin browser $bsid navigate $fileUrl | Out-String
        if ($LASTEXITCODE -eq 0) { break }
        Start-Sleep -Milliseconds 400
    }
    Check 'cli browser navigate (positional surface)' ($nav -match 'OK') $nav

    # A cold WebView2 pane answers early calls with "still initializing";
    # poll readiness (idempotent waits) instead of racing one shot.
    # Native stderr here is the documented cold-init retry case — scope
    # Continue so the script prints it without aborting (the check
    # decides).
    $ready = $false
    $oldEap = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    for ($attempt = 1; $attempt -le 20; $attempt++) {
        & $cliBin browser $bsid wait --load-state load --timeout-ms 2000 | Out-Null
        if ($LASTEXITCODE -eq 0) { $ready = $true; break }
        Start-Sleep -Milliseconds 500
    }
    $ErrorActionPreference = $oldEap
    Check 'cli browser wait --load-state (ready poll)' $ready

    # The load-state answer and the page settling live in adjacent but
    # distinct windows: the first handful of calls after navigation can
    # still trip a cold-start stall, so require consecutive under-budget
    # reads before the interaction sequence starts.
    $settled = $false
    for ($attempt = 1; $attempt -le 10; $attempt++) {
        $null = & $cliBin browser $bsid get title 2>$null
        if ($LASTEXITCODE -ne 0) { Start-Sleep -Milliseconds 500; continue }
        $null = & $cliBin browser $bsid get title 2>$null
        if ($LASTEXITCODE -eq 0) { $settled = $true; break }
        Start-Sleep -Milliseconds 500
    }
    Check 'cli browser cheap reads settle the burst' $settled

    # Even settled, a transient app-plane stall can push an INDIVIDUAL
    # call past the 500 ms CLI budget (every verb measures 10-45 ms
    # round trip once the app settles - the diag probes verified; the
    # close runs and later runs only differed in which call paid it).
    # The interaction checks therefore allow two idempotent retries:
    # the app completes the call either way and the check reads the
    # resulting state.
    function Invoke-Settle([scriptblock]$block) {
        for ($attempt = 1; $attempt -le 3; $attempt++) {
            $code = & $block
            if ($code -eq 0) { return $true }
            Start-Sleep -Milliseconds 600
        }
        return $false
    }

    $url = & $cliBin browser --surface $bsid url | Out-String
    Check 'cli browser url via --surface flag' ($url.Trim() -match '^file:///' -and $url.Trim() -match 'page\.html$') $url

    $title = & $cliBin browser $bsid get title | Out-String
    Check 'cli browser get title' ($title.Trim() -eq 'CLI Browser Fixture') $title

    $sumOk = Invoke-Settle { & $cliBin browser $bsid eval "2+3" | Out-Null; $LASTEXITCODE }
    $sum = & $cliBin browser $bsid eval "2+3" | Out-String
    Check 'cli browser eval prints the value' ($sumOk -and $sum.Trim() -eq '5') $sum

    $snapOk = Invoke-Settle { & $cliBin browser $bsid snapshot --json | Out-Null; $LASTEXITCODE }
    $snap = & $cliBin browser $bsid snapshot --json | Out-String
    Check 'cli browser snapshot --json carries refs' ($snapOk -and $snap -match '"ok":true' -and $snap -match '"ref":"@e\d+"') $snap
    $sendRef = if ($snap -match '"ref":"(@e\d+)","selector":"#send"') { $Matches[1] } else { '' }
    Check 'cli browser snapshot names the button ref' ($sendRef -ne '') $snap

    $clickOk = Invoke-Settle { & $cliBin browser $bsid click $sendRef | Out-Null; $LASTEXITCODE }
    Check 'cli browser click by ref' $clickOk
    $clicks = & $cliBin browser $bsid eval "window.__clicks>=1" | Out-String
    Check 'cli browser click registered' ($clicks.Trim() -eq 'true') $clicks

    $null = & $cliBin browser $bsid fill '#name' --text 'Ada'
    Check 'cli browser fill' ($LASTEXITCODE -eq 0)
    $value = & $cliBin browser $bsid eval "document.querySelector('#name').value" | Out-String
    Check 'cli browser fill applied' ($value.Trim() -eq 'Ada') $value

    $null = & $cliBin browser $bsid focus '#name'
    Check 'cli browser focus' ($LASTEXITCODE -eq 0)
    $pressOk = Invoke-Settle { & $cliBin browser $bsid press Enter | Out-Null; $LASTEXITCODE }
    Check 'cli browser press' ($pressOk -and $LASTEXITCODE -eq 0)
    $pressed = & $cliBin browser $bsid eval "window.__enter===true" | Out-String
    Check 'cli browser press reached the page' ($pressed.Trim() -eq 'true') $pressed

    $selectOk = Invoke-Settle { & $cliBin browser $bsid select '#pick' 'v2' | Out-Null; $LASTEXITCODE }
    Check 'cli browser select' $selectOk
    $picked = & $cliBin browser $bsid eval "document.querySelector('#pick').value" | Out-String
    Check 'cli browser select applied' ($picked.Trim() -eq 'v2') $picked

    $scroll = & $cliBin browser $bsid scroll --dx 0 --dy 40 | Out-String
    Check 'cli browser scroll' ($LASTEXITCODE -eq 0 -and $scroll -match 'OK') $scroll

    $visible = & $cliBin browser $bsid is visible '#name' | Out-String
    Check 'cli browser is visible' ($visible.Trim() -eq 'true') $visible
    $checkedOk = Invoke-Settle { & $cliBin browser $bsid is checked '#cb' | Out-Null; $LASTEXITCODE }
    $checked = & $cliBin browser $bsid is checked '#cb' | Out-String
    Check 'cli browser is checked' ($checkedOk -and $checked.Trim() -eq 'true') $checked

    $countOk = Invoke-Settle { & $cliBin browser $bsid get count 'input' | Out-Null; $LASTEXITCODE }
    $count = & $cliBin browser $bsid get count 'input' | Out-String
    Check 'cli browser get count' ($countOk -and $count.Trim() -eq '2') $count

    $waitFn = & $cliBin browser $bsid wait --function "document.querySelector('#name').value==='Ada'" --timeout-ms 3000 | Out-String
    Check 'cli browser wait --function predicate' ($LASTEXITCODE -eq 0 -and $waitFn -match 'OK') $waitFn

    $timeoutReply = cmd /c "`"$cliBin`" browser $bsid wait --selector #nonexistent --timeout-ms 500 2>&1" | Out-String
    Check 'cli browser wait timeout is structured' ($LASTEXITCODE -eq 1 -and $timeoutReply -match 'timeout') $timeoutReply

    $shotTarget = Join-Path $fixture 'shot.png'
    $shotOk = Invoke-Settle {
        & $cliBin browser $bsid screenshot --out $shotTarget | Out-Null
        $LASTEXITCODE
    }
    $shot = & $cliBin browser $bsid screenshot --out $shotTarget | Out-String
    $shotReady = ($shotOk) -and (Test-Path $shotTarget) -and ((Get-Item $shotTarget).Length -gt 500) -and ($shot -match 'OK')
    Check 'cli browser screenshot --out copies' $shotReady $shot

    $plainShot = & $cliBin browser $bsid screenshot | Out-String
    Check 'cli browser screenshot plain prints the path' ($LASTEXITCODE -eq 0 -and $plainShot.Trim() -match '\.png$') $plainShot

    $null = & $cliBin browser $bsid focus-webview
    Check 'cli browser focus-webview' ($LASTEXITCODE -eq 0)
    $focused = & $cliBin browser $bsid is-webview-focused | Out-String
    Check 'cli browser is-webview-focused prints a bool' ($focused.Trim() -eq 'true' -or $focused.Trim() -eq 'false') $focused

    # The back commit lands asynchronously; poll the pane's URL instead
    # of reading it one shot.
    # History has no real entry before the first navigation (about:blank
    # is the app-seeded start), so go two stacked forwards first; then
    # back lands on page1. Poll the URL - the back commit is async.
    $null = & $cliBin browser $bsid navigate $fileUrl2
    for ($attempt = 1; $attempt -le 10; $attempt++) {
        $title2 = & $cliBin browser $bsid get title | Out-String
        if ($title2.Trim() -eq 'Fixture Two') { break }
        Start-Sleep -Milliseconds 300
    }
    $null = & $cliBin browser $bsid back
    $backExit = $LASTEXITCODE
    $backUrl = ''
    $backOk = $false
    for ($attempt = 1; $attempt -le 14; $attempt++) {
        $backUrl = & $cliBin browser $bsid url | Out-String
        if ($backUrl.Trim() -match 'page\.html$' -and $backUrl.Trim() -notmatch 'page2') { $backOk = $true; break }
        Start-Sleep -Milliseconds 300
    }
    Check 'cli browser back navigates history' ($backExit -eq 0 -and $backOk) $backUrl

    # With the upstream positional-handle grammar a scoped verb word in
    # slot 0 becomes the surface handle (upstream misroutes the same
    # way); assert the visible grammar error and exit code.
    $noSurface = cmd /c "`"$cliBin`" browser click '#name' 2>&1" | Out-String
    Check 'cli browser no-handle grammar exits 1' ($LASTEXITCODE -eq 1 -and $noSurface -match 'unknown subcommand') $noSurface

    $badOption = cmd /c "`"$cliBin`" browser $bsid navigate http://x --profile y 2>&1" | Out-String
    Check 'cli browser refuses unknown options' ($LASTEXITCODE -eq 1 -and $badOption -match 'unrecognized option --profile') $badOption

    $unsupported = cmd /c "`"$cliBin`" browser find 2>&1" | Out-String
    Check 'cli browser unsupported verbs answer plainly' ($LASTEXITCODE -eq 1 -and $unsupported -match 'not supported by the port') $unsupported

    # ==== M6 slice 1: the cmux.json custom-command wrappers ====
    $cmdList = & $cliBin command list | Out-String
    Check 'cli command list rows in config order' (
        $cmdList -match 'm6-echo - echo marker  # echo cmd-m6-alpha' -and
        $cmdList -match 'm6-guarded - needs --yes \[confirm\]' -and
        $cmdList -match 'm6-space \(workspace launch - not supported by the port\)'
    ) $cmdList

    $cmdListJson = & $cliBin command list --json | Out-String
    Check 'cli command list --json keeps the envelope' ($cmdListJson -match '"ok":true' -and $cmdListJson -match '"kind":"workspace"') $cmdListJson

    $null = & $cliBin command run m6-echo --surface $sid
    $runExit = $LASTEXITCODE
    Start-Sleep -Milliseconds 600
    $screen = & $cliBin read-screen --surface $sid | Out-String
    Check 'cli command run types the command plus Enter' ($runExit -eq 0 -and $screen -match 'cmd-m6-alpha') $screen

    $guardedNo = cmd /c "`"$cliBin`" command run m6-guarded --surface $sid 2>&1" | Out-String
    Check 'cli command confirm refuses without --yes' ($LASTEXITCODE -eq 1 -and $guardedNo -match 'requires confirmation') $guardedNo

    $null = cmd /c "`"$cliBin`" command run m6-guarded --surface $sid --yes >nul 2>&1"
    $guardedExit = $LASTEXITCODE
    Start-Sleep -Milliseconds 600
    $screenGuarded = & $cliBin read-screen --surface $sid | Out-String
    Check 'cli command confirm runs with --yes' ($guardedExit -eq 0 -and $screenGuarded -match 'cmd-m6-confirmed') $screenGuarded

    $unknown = cmd /c "`"$cliBin`" command run no-such-command 2>&1" | Out-String
    Check 'cli command run unknown is not_found' ($LASTEXITCODE -eq 1 -and $unknown -match 'not defined in the config file') $unknown

    $residual = cmd /c "`"$cliBin`" command run m6-space 2>&1" | Out-String
    Check 'cli command workspace flavor is the residual' ($LASTEXITCODE -eq 1 -and $residual -match 'does not support yet') $residual

    $badFlag = cmd /c "`"$cliBin`" command run m6-echo --profile x 2>&1" | Out-String
    Check 'cli command run refuses unknown options' ($LASTEXITCODE -eq 1 -and $badFlag -match 'unrecognized option --profile') $badFlag
}
finally {
    if (-not $proc.HasExited) {
        Stop-Process -Id $proc.Id -Force
        $proc.WaitForExit(10000) | Out-Null
    }
    Remove-Item Env:CMUX_SOCKET_PATH, Env:CMUX_SESSION_FILE, Env:CMUX_CONFIG_FILE -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 300
    Remove-Item $fixture -Recurse -Force -ErrorAction SilentlyContinue
}
Write-Output ""
Write-Output "cli smoke complete: $pass passed, $fail failed"
exit $(@{ $true = 0; $false = 1 }[$fail -eq 0])
