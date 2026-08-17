#!/bin/bash
# expected:
# === dir with value and child ===
# /dv	int64:99
# x	int64	1
# /dv/x	int64:1
# /dv/x/y	int64:2
# y	int64	2
# == del value keeps children ===
# /dv2/x	int64:42
# x	int64	42
# /end

set -e
KV="$HOME/.local/bin/kvspace"
$KV deltree /dv/
$KV deltree /dv2/

echo "=== dir with value and child ==="
$KV set /dv/ index:
$KV set /dv int:99
$KV set /dv/x int:1
$KV get /dv
$KV list /dv/
$KV get /dv/x
$KV set /dv/x/ index:
$KV set /dv/x/y int:2
$KV get /dv/x/y
$KV list /dv/x/

echo "== del value keeps children ==="
$KV set /dv2/ index:
$KV set /dv2/x int:42
$KV del /dv2
$KV get /dv2/x
$KV list /dv2/




