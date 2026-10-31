# Build environment for the cmux Windows port workspace.
# Dot-source from port/:  . .\scripts\env.ps1
# Verified prerequisite locations are documented in ../docs/development/building-windows.md

$cargoBin = Join-Path $env:USERPROFILE ".cargo\bin"
if (Test-Path (Join-Path $cargoBin "cargo.exe")) { $env:Path = "$cargoBin;$env:Path" }

# Zig lives on C:\ deliberately: building Ghostty from source fails when Zig and the
# project share a drive letter (uucode cache bug, libghostty-rs issue #9), and this
# project is on D:\.
$zigRoot = "C:\Users\noah\tools"
if (Test-Path $zigRoot) {
    $zig = Get-ChildItem -Path $zigRoot -Directory -Filter "zig-0.15.*" -ErrorAction SilentlyContinue |
        ForEach-Object { Get-ChildItem -Path $_.FullName -Filter "zig.exe" -Recurse -ErrorAction SilentlyContinue } |
        Select-Object -First 1
    if ($zig) { $env:Path = $zig.DirectoryName + ";" + $env:Path }
}

if (Get-Command cargo.exe -ErrorAction SilentlyContinue) {
    Write-Host "cargo $(& cargo.exe --version)"
} else {
    Write-Warning "cargo not found - run .\scripts\env.ps1 or install rustup (see building-windows.md)"
}
if ($zig) { Write-Host "zig   $(& $zig.FullName version)" }
