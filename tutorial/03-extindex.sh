#!/bin/bash
# expected:
# === read fallthrough ===
# /merge/a	int64:1
# /merge/b	int64:2
# /merge/c	int64:3
# === write new key ===
# /merge/z	char/utf8:upper_only
# /base/z	(nil)
# === list merge ===
# z	char/utf8	upper_only
# a	int64	1
# b	int64	2
# c	int64	3
# === del upper key ===
# /merge/z	(nil)
# a	int64	1
# b	int64	2
# c	int64	3
# === nested write ===
# /merge/nest/x	int64:42
# /merge/nest/y	int64:43
# /base/nest/x	(nil)
# x	int64	42
# y	int64	43
# === deltree preserves base ===
# /merge/a	(nil)
# /base/a	int64:1
# a	int64	1
# b	int64	2
# c	int64	3
# /end

set -e
KV="$HOME/.local/bin/kvspace"
$KV deltree /base/
$KV deltree /merge/

$KV set /base/ index:
$KV set /base/a int:1
$KV set /base/b int:2
$KV set /base/c int:3
$KV extindex /merge/ /base/

echo "=== read fallthrough ==="
$KV get /merge/a /merge/b /merge/c

echo "=== write new key ==="
$KV set /merge/z string:upper_only
$KV get /merge/z
$KV get /base/z

echo "=== list merge ==="
$KV list /merge/

echo "=== del upper key ==="
$KV del /merge/z
$KV get /merge/z
$KV list /merge/

echo "=== nested write ==="
$KV set /merge/nest/ index:
$KV set /merge/nest/x int:42
$KV set /merge/nest/y int:43
$KV get /merge/nest/x /merge/nest/y
$KV get /base/nest/x
$KV list /merge/nest/

echo "=== deltree preserves base ==="
$KV deltree /merge/
$KV get /merge/a
$KV get /base/a
$KV list /base/




