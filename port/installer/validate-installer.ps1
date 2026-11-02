<#
.SYNOPSIS
    Validates the cmux Windows MSI installer structure and contents.
    
.DESCRIPTION
    Extracts the MSI to a temporary directory and validates all required files are present.
    
.PARAMETER MsiPath
    Path to the MSI file to validate.
    
.EXAMPLE
    .\validate-installer.ps1 -MsiPath "target\installer\cmux-installer.msi"
#>

[CmdletBinding()]
param(
    [Parameter(Mandatory=$true)]
    [string]$MsiPath
)

$ErrorActionPreference = "Stop"

if (-not (Test-Path $MsiPath)) {
    Write-Error "MSI not found at $MsiPath"
    exit 1
}

Write-Host "=== cmux Installer Validation ===" -ForegroundColor Cyan
Write-Host "MSI: $MsiPath" -ForegroundColor Cyan

$size = (Get-Item $MsiPath).Length
Write-Host "File size: $([math]::Round($size/1MB, 2)) MB"
Write-Host ""

# Extract MSI to temp directory
$tempDir = [System.IO.Path]::Combine([System.IO.Path]::GetTempPath(), "cmux-msi-validation-$(Get-Random)")
New-Item -ItemType Directory -Force -Path $tempDir | Out-Null

Write-Host "Extracting MSI to $tempDir..." -ForegroundColor Yellow

try {
    $msiexec = Get-Command msiexec.exe -ErrorAction Stop
    $msiFullPath = Resolve-Path $MsiPath
    $targetDir = $tempDir.Replace('\', '\\')
    
    $process = Start-Process -FilePath $msiexec.Source -ArgumentList "/a `"$msiFullPath`" /qb TARGETDIR=`"$targetDir`"" -Wait -PassThru
    
    if ($process.ExitCode -ne 0) {
        Write-Error "msiexec extraction failed with exit code $($process.ExitCode)"
        exit 1
    }
    
    Write-Host "Extraction successful" -ForegroundColor Green
    Write-Host ""
    
    Write-Host "=== Extracted Contents ===" -ForegroundColor Cyan
    Get-ChildItem -Recurse -File $tempDir | ForEach-Object {
        $rel = $_.FullName.Substring($tempDir.Length + 1)
        $sizeMB = [math]::Round($_.Length / 1MB, 2)
        Write-Host "  $rel ($sizeMB MB)"
    }
    
    Write-Host ""
    Write-Host "=== Required Files Check ===" -ForegroundColor Cyan
    
    $requiredFiles = @(
        "cmux-shell.exe",
        "conpty.dll",
        "OpenConsole.exe",
        "cmux.exe",
        "cmux.json.template",
        "NOTICES.txt"
    )
    
    $allFound = $true
    foreach ($file in $requiredFiles) {
        $found = Get-ChildItem -Recurse -File -Filter $file $tempDir -ErrorAction SilentlyContinue
        if ($found) {
            Write-Host "  [OK] $file" -ForegroundColor Green
        } else {
            Write-Host "  [MISSING] $file (MISSING)" -ForegroundColor Red
            $allFound = $false
        }
    }
    
    if ($allFound) {
        Write-Host ""
        Write-Host "=== All Required Files Present ===" -ForegroundColor Green
    } else {
        Write-Host ""
        Write-Host "=== MISSING REQUIRED FILES ===" -ForegroundColor Red
        exit 1
    }
    
} finally {
    # Cleanup
    if (Test-Path $tempDir) {
        Remove-Item -Recurse -Force $tempDir -ErrorAction SilentlyContinue
    }
}

Write-Host ""
Write-Host "=== Validation Complete ===" -ForegroundColor Cyan
