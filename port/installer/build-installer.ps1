<#
.SYNOPSIS
    cmux Windows MSI Build Script
    
.DESCRIPTION
    Builds the cmux Windows MSI installer using WiX Toolset.
    
.PARAMETER Sign
    Attempt to sign the MSI (requires signing credentials).
    
.PARAMETER OutputDir
    Output directory for the MSI (default: target/installer).
    
.EXAMPLE
    .\build-installer.ps1
    
.EXAMPLE
    .\build-installer.ps1 -Sign
    
.EXAMPLE
    .\build-installer.ps1 -Sign -OutputDir "C:\artifacts"
#>

[CmdletBinding()]
param(
    [switch]$Sign,
    [string]$OutputDir = "target\installer"
)

$ErrorActionPreference = "Stop"

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Definition
$PortDir = Split-Path -Parent $ScriptDir
$BuildDir = Join-Path $PortDir "target\release"
$InstallerDir = Join-Path $PortDir "installer"
$OutputDir = Join-Path $PortDir $OutputDir

Write-Host "=== cmux Windows MSI Build ===" -ForegroundColor Cyan
Write-Host "Port directory: $PortDir"
Write-Host "Build directory: $BuildDir"
Write-Host "Installer directory: $InstallerDir"
Write-Host "Output directory: $OutputDir"
Write-Host ""

# Check prerequisites
Write-Host "Checking prerequisites..." -ForegroundColor Yellow

$candle = Get-Command candle.exe -ErrorAction SilentlyContinue
if (-not $candle) {
    Write-Error "candle.exe not found in PATH"
    Write-Host "Install WiX Toolset v3.14+ from https://github.com/wixtoolset/wix3/releases"
    exit 1
}

$light = Get-Command light.exe -ErrorAction SilentlyContinue
if (-not $light) {
    Write-Error "light.exe not found in PATH"
    Write-Host "Install WiX Toolset v3.14+ from https://github.com/wixtoolset/wix3/releases"
    exit 1
}

Write-Host "WiX found: $($candle.Source)" -ForegroundColor Green

# Check build artifacts
Write-Host ""
Write-Host "Checking build artifacts..." -ForegroundColor Yellow

$requiredFiles = @(
    "cmux-shell.exe",
    "conpty.dll",
    "OpenConsole.exe",
    "cmux-cli.exe"
)

$allFound = $true
foreach ($file in $requiredFiles) {
    $path = Join-Path $BuildDir $file
    if (Test-Path $path) {
        Write-Host "  [✓] $file" -ForegroundColor Green
    } else {
        Write-Host "  [✗] $file (not found - run 'cargo build --release' in port/)" -ForegroundColor Red
        $allFound = $false
    }
}

if (-not $allFound) {
    exit 1
}

# Create output directory
New-Item -ItemType Directory -Force -Path $OutputDir | Out-Null

# Copy build artifacts to installer directory for WiX
Write-Host ""
Write-Host "Staging files for WiX..." -ForegroundColor Yellow
Copy-Item -Path (Join-Path $BuildDir "cmux-shell.exe") -Destination $InstallerDir -Force
Copy-Item -Path (Join-Path $BuildDir "conpty.dll") -Destination $InstallerDir -Force
Copy-Item -Path (Join-Path $BuildDir "OpenConsole.exe") -Destination $InstallerDir -Force
Copy-Item -Path (Join-Path $BuildDir "cmux-cli.exe") -Destination $InstallerDir -Force

# Build WiX object file
Write-Host ""
Write-Host "Compiling WiX source..." -ForegroundColor Yellow
Set-Location $InstallerDir
$sourceDir = Resolve-Path $InstallerDir
$buildDirWin = Resolve-Path $BuildDir

& candle.exe -dSourceDir="$sourceDir" `
    -dBuildDir="$buildDirWin" `
    -dProductVersion="0.1.0" `
    cmux-installer.wxs `
    -out cmux-installer.wixobj

# Link MSI
Write-Host "Linking MSI..." -ForegroundColor Yellow
$msiPath = Join-Path $OutputDir "cmux-installer.msi"
& light.exe cmux-installer.wixobj `
    -out $msiPath `
    -ext WixUIExtension `
    -cultures:en-us

if (-not (Test-Path $msiPath)) {
    Write-Error "MSI not created at $msiPath"
    exit 1
}

Write-Host ""
Write-Host "MSI created: $msiPath" -ForegroundColor Cyan
(Get-Item $msiPath).Length | ForEach-Object { Write-Host "  Size: $([math]::Round($_/1MB, 2)) MB" }

# Optional code signing
if ($Sign) {
    Write-Host ""
    Write-Host "Attempting to sign MSI..." -ForegroundColor Yellow
    
    $signTool = $env:SIGNTOOL_PATH ?? "signtool.exe"
    $certThumbprint = $env:CERT_THUMBPRINT
    $pfxPath = $env:PFX_PATH
    $certPassword = $env:CERT_PASSWORD
    
    $msiPathFull = Resolve-Path $msiPath
    
    if ($env:PFX_PATH -and (Test-Path $env:PFX_PATH)) {
        Write-Host "Signing with PFX: $env:PFX_PATH"
        & $signTool sign /f $env:PFX_PATH /p $env:CERT_PASSWORD /tr http://timestamp.digicert.com /td sha256 /fd sha256 $msiPathFull
    } elseif ($env:CERT_THUMBPRINT) {
        Write-Host "Signing with certificate thumbprint: $env:CERT_THUMBPRINT"
        & $signTool sign /sha1 $env:CERT_THUMBPRINT /tr http://timestamp.digicert.com /td sha256 /fd sha256 $msiPathFull
    } else {
        Write-Warning "No signing credentials found. Set PFX_PATH or CERT_THUMBPRINT environment variables."
        Write-Host "MSI remains unsigned but fully functional." -ForegroundColor Yellow
    }
}

Write-Host ""
Write-Host "=== Build Complete ===" -ForegroundColor Cyan
Write-Host "MSI: $msiPath" -ForegroundColor Cyan
Write-Host ""
Write-Host "Install with: msiexec /i `"$msiPath`""
Write-Host "Uninstall with: msiexec /x {ProductCode}"