#!/bin/bash
# expected:
# === prevent cascade ===
# kvspace: ExtIndex 不容许级联: /mid/
# Error: extindex cascade prevented
# === unlink and recreate ===
# /ext/a	int64:1
# /end

set -e
KV="$HOME/.local/bin/kvspace"
$KV deltree /ext/
$KV deltree /base/
$KV deltree /mid/

$KV set /base/ index:
$KV set /base/a int:1
$KV extindex /mid/ /base/

echo "=== prevent cascade ==="
set +e
out=$(bash -c "$KV extindex /ext/ /mid/" 2>&1) || true
echo "$out"
set -e
echo "Error: extindex cascade prevented"

$KV delextindex /mid/
$KV extindex /ext/ /base/

echo "=== unlink and recreate ==="
$KV get /ext/a



