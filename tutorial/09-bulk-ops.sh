#!/bin/bash
# expected:
# === bulk get ===
# /b/a/x	int64:1
# /b/a/y	int64:2
# /b/b/z	int64:3
# === bulk del siblings ===
# /b/c/x	(nil)
# /b/c/y	(nil)
# /b/c/z	int64:3
# === bulk del cross dirs ===
# /b/d/a	(nil)
# /b/e/b	(nil)
# /b/f/c	int64:6
# c	int64	6
# /end

set -e
KV="$HOME/.local/bin/kvspace"
$KV deltree /b/

$KV set /b/ index:
$KV set /b/a/ index:
$KV set /b/b/ index:

echo "=== bulk get ==="
$KV set /b/a/x int:1
$KV set /b/a/y int:2
$KV set /b/b/z int:3
$KV get /b/a/x /b/a/y /b/b/z

echo "=== bulk del siblings ==="
$KV set /b/c/ index:
$KV set /b/c/x int:1
$KV set /b/c/y int:2
$KV set /b/c/z int:3
$KV del /b/c/x /b/c/y
$KV get /b/c/x /b/c/y /b/c/z

echo "=== bulk del cross dirs ==="
$KV set /b/d/ index:
$KV set /b/e/ index:
$KV set /b/f/ index:
$KV set /b/d/a int:4
$KV set /b/e/b int:5
$KV set /b/f/c int:6
$KV del /b/d/a /b/e/b
$KV get /b/d/a /b/e/b /b/f/c
$KV list /b/f/




