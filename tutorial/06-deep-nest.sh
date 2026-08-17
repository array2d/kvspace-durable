#!/bin/bash
# expected:
# === deep tree ===
# /deep/a/b/c/d/e/f	int64:6
# a/	index	(1)
# /deep/a/b/c	int64:3
# d/	index	(1)
# c/	index	(1)
# c/
# === deltree mid ===
# /deep/a/b/c/d/e/f	(nil)
# /deep/a/b/c/d/e	(nil)
# /deep/a/b/c	int64:3
# === deltree leaf ===
# /deep/a/b/c	(nil)
# a/	index	(0)
# /end

set -e
KV="$HOME/.local/bin/kvspace"
$KV deltree /deep/

echo "=== deep tree ==="
$KV set /deep/ index:
$KV set /deep/a/ index:
$KV set /deep/a/b/ index:
$KV set /deep/a/b/c/ index:
$KV set /deep/a/b/c/d/ index:
$KV set /deep/a/b/c/d/e/ index:
$KV set /deep/a/b/c/d/e/f int:6
$KV get /deep/a/b/c/d/e/f
$KV list /deep/
$KV set /deep/a/b/c int:3
$KV get /deep/a/b/c
$KV list /deep/a/b/c/
$KV list /deep/a/b/

echo "=== deltree mid ==="
$KV deltree /deep/a/b/c/d/e/
$KV get /deep/a/b/c/d/e/f
$KV get /deep/a/b/c/d/e
$KV get /deep/a/b/c

echo "=== deltree leaf ==="
$KV deltree /deep/a/b/
$KV get /deep/a/b/c
$KV list /deep/






