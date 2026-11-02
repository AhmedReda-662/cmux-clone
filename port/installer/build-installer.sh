#!/usr/bin/env bash
# cmux Windows MSI Build Script
# 
# Usage: ./build-installer.sh [--sign]
# 
# Requires:
# - WiX Toolset v3.14+ in PATH (candle.exe, light.exe)
# - Rust project built: `cargo build --release` in port/
# 
# Options:
#   --sign    Attempt to sign the MSI (requires SIGNTOOL_PATH, CERT_THUMBPRINT, or .pfx)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PORT_DIR="$(dirname "$SCRIPT_DIR")"
BUILD_DIR="$PORT_DIR/target/release"
INSTALLER_DIR="$PORT_DIR/installer"
OUTPUT_DIR="$PORT_DIR/target/installer"

SIGN=false
if [[ "${1:-}" == "--sign" ]]; then
    SIGN=true
fi

echo "=== cmux Windows MSI Build ==="
echo "Port directory: $PORT_DIR"
echo "Build directory: $BUILD_DIR"
echo "Installer directory: $INSTALLER_DIR"
echo "Output directory: $OUTPUT_DIR"
echo ""

# Check prerequisites
echo "Checking prerequisites..."

if ! command -v candle.exe &> /dev/null; then
    echo "ERROR: candle.exe not found in PATH"
    echo "Install WiX Toolset v3.14+ from https://github.com/wixtoolset/wix3/releases"
    exit 1
fi

if ! command -v light.exe &> /dev/null; then
    echo "ERROR: light.exe not found in PATH"
    echo "Install WiX Toolset v3.14+ from https://github.com/wixtoolset/wix3/releases"
    exit 1
fi

echo "WiX found: $(candle.exe -? 2>&1 | head -1)"

# Check build artifacts
echo ""
echo "Checking build artifacts..."
REQUIRED_FILES=(
    "cmux-shell.exe"
    "conpty.dll"
    "OpenConsole.exe"
    "cmux.exe"
)

for file in "${REQUIRED_FILES[@]}"; do
    if [[ -f "$BUILD_DIR/$file" ]]; then
        echo "  ✓ $file"
    else
        echo "  ✗ $file (not found - run 'cargo build --release' in port/)"
        exit 1
    fi
done

# Create output directory
mkdir -p "$OUTPUT_DIR"

# Build WiX object file
echo ""
echo "Compiling WiX source..."
cd "$INSTALLER_DIR"
candle.exe -arch x64 -dSourceDir="$(cygpath -w "$INSTALLER_DIR")" \
    -dBuildDir="$(cygpath -w "$BUILD_DIR")" \
    -dProductVersion="0.0.1" \
    cmux-installer.wxs \
    -out cmux-installer.wixobj

# Link MSI
echo "Linking MSI..."
light.exe cmux-installer.wixobj \
    -out "$OUTPUT_DIR/cmux-installer.msi" \
    -ext WixUIExtension \
    -cultures:en-us

MSI_PATH="$OUTPUT_DIR/cmux-installer.msi"

if [[ ! -f "$MSI_PATH" ]]; then
    echo "ERROR: MSI not created at $MSI_PATH"
    exit 1
fi

echo ""
echo "MSI created: $MSI_PATH"
ls -lh "$MSI_PATH"

# Optional code signing
if [[ "$SIGN" == true ]]; then
    echo ""
    echo "Attempting to sign MSI..."
    
    SIGNTOOL="${SIGNTOOL_PATH:-signtool.exe}"
    CERT_THUMBPRINT="${CERT_THUMBPRINT:-}"
    PFX_PATH="${PFX_PATH:-}"
    CERT_PASSWORD="${CERT_PASSWORD:-}"
    
    if [[ -n "$PFX_PATH" && -f "$PFX_PATH" ]]; then
        echo "Signing with PFX: $PFX_PATH"
        "$SIGNTOOL" sign /f "$PFX_PATH" /p "$CERT_PASSWORD" /tr http://timestamp.digicert.com /td sha256 /fd sha256 "$MSI_PATH"
    elif [[ -n "$CERT_THUMBPRINT" ]]; then
        echo "Signing with certificate thumbprint: $CERT_THUMBPRINT"
        "$SIGNTOOL" sign /sha1 "$CERT_THUMBPRINT" /tr http://timestamp.digicert.com /td sha256 /fd sha256 "$MSI_PATH"
    else
        echo "WARNING: No signing credentials found. Set PFX_PATH or CERT_THUMBPRINT."
        echo "MSI remains unsigned but fully functional."
    fi
fi

echo ""
echo "=== Build Complete ==="
echo "MSI: $MSI_PATH"
echo ""
echo "Install with: msiexec /i \"$MSI_PATH\""
echo "Uninstall with: msiexec /x {ProductCode}"