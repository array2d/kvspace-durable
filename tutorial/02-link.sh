#!/bin/bash
# expected:
# === read through ===
# /lnk/x	int64:42
# /lnk/y	int64:7
# === list through ===
# x	int64	42
# y	int64	7
# z	char/utf8	hello
# === write through ===
# /tgt/w	int64:99
# x	int64	42
# y	int64	7
# z	char/utf8	hello
# w	int64	99
# === del through link ===
# /tgt/y	(nil)
# x	int64	42
# z	char/utf8	hello
# w	int64	99
# === del link body ===
# /lnk	(nil)
# /tgt/x	int64:42
# /end

set -e
KV="$HOME/.local/bin/kvspace"
$KV deltree /tgt/

$KV set /tgt/ index:
$KV set /tgt/x int:42
$KV set /tgt/y int:7
$KV set /tgt/z string:hello
$KV set /lnk "*index:/tgt/"

echo "=== read through ==="
$KV get /lnk/x /lnk/y

echo "=== list through ==="
$KV list /lnk/

echo "=== write through ==="
$KV set /lnk/w int:99
$KV get /tgt/w
$KV list /tgt/

echo "=== del through link ==="
$KV del /lnk/y
$KV get /tgt/y
$KV list /tgt/

echo "=== del link body ==="
$KV del /lnk
$KV get /lnk
$KV get /tgt/x


