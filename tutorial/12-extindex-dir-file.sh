#!/bin/bash
# expected:
# === tree shows extindex with dir+file ===
# /ext/
# ├── d	val
# ├── d/
# │   └── y	2
# └── k	1
# /end

set -e
KV="$HOME/.local/bin/kvspace"
$KV deltree /ext/
$KV deltree /base/

$KV set /base/ index:
$KV set /base/k int:1
$KV extindex /ext/ /base/
$KV set /ext/d/ index:
$KV set /ext/d string:val
$KV set /ext/d/y int:2

echo "=== tree shows extindex with dir+file ==="
$KV tree /ext/






