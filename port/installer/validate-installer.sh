#!/usr/bin/env bash
# cmux Windows Installer Validation Script
# Validates the MSI structure and contents without installing

set -euo pipefail

MSI_PATH="${1:-port/target/installer/cmux-installer.msi}"

if [[ ! -f "$MSI_PATH" ]]; then
    echo "ERROR: MSI not found at $MSI_PATH"
    echo "Usage: $0 [path-to-msi]"
    exit 1
fi

echo "=== cmux Installer Validation ==="
echo "MSI: $MSI_PATH"
echo ""

# Check file exists and size
size=$(stat -c%s "$MSI_PATH" 2>/dev/null || stat -f%z "$MSI_PATH" 2>/dev/null)
echo "File size: $(numfmt --to=iec $size 2>/dev/null || echo "$size bytes")"

# Extract and validate using msiexec (Windows) or lessmsi (if available)
if command -v msiexec.exe &> /dev/null; then
    echo ""
    echo "Extracting MSI to temp directory for validation..."
    TEMP_DIR=$(mktemp -d)
    msiexec.exe /a "$(cygpath -w "$MSI_PATH")" /qb TARGETDIR="$(cygpath -w "$TEMP_DIR")" 2>&1 | tail -5
    
    echo ""
    echo "=== Extracted Contents ==="
    find "$TEMP_DIR" -type f | sort | while read file; do
        rel="${file#$TEMP_DIR/}"
        size=$(stat -c%s "$file" 2>/dev/null || stat -f%z "$file" 2>/dev/null)
        echo "  $rel ($(numfmt --to=iec $size 2>/dev/null || echo "$size bytes"))"
    done
    
    # Verify required files
    echo ""
    echo "=== Required Files Check ==="
    REQUIRED=(
        "cmux-shell.exe"
        "conpty.dll"
        "OpenConsole.exe"
        "cmux-cli.exe"
        "cmux.json.template"
        "LICENSE"
    )
    
    for file in "${REQUIRED[@]}"; do
        if find "$TEMP_DIR" -name "$file" -type f | grep -q .; then
            echo "  ✓ $file"
        else
            echo "  ✗ $file (MISSING)"
        fi
    done
    
    # Cleanup
    rm -rf "$TEMP_DIR"
    
elif command -v lessmsi &> /dev/null; then
    echo "Using lessmsi for validation..."
    lessmsi l "$MSI_PATH"
else
    echo "WARNING: msiexec.exe not found (not on Windows or not in PATH)"
    echo "Cannot fully validate MSI contents. Basic checks only."
    
    # Basic file type check
    if file "$MSI_PATH" | grep -qi "msi\|microsoft installer"; then
        echo "  ✓ File appears to be a valid MSI"
    else
        echo "  ✗ File does not appear to be a valid MSI"
    fi
fi

echo ""
echo "=== Validation Complete ==="