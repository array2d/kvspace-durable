#!/bin/bash
# expected:
# === overwrite kind ===
# /e/v	int64:42
# /e/v	char/utf8:hello
# === empty list ===
# === del non-existent ===
# /e/ghost	(nil)
# === deltree non-existent ===
# /end

set -e
KV="$HOME/.local/bin/kvspace"
$KV deltree /e/
$KV set /e/ index:

echo "=== overwrite kind ==="
$KV set /e/v int:42
$KV get /e/v
$KV set /e/v string:hello
$KV get /e/v

echo "=== empty list ==="
$KV list /e/empty/

echo "=== del non-existent ==="
$KV del /e/ghost
$KV get /e/ghost

echo "=== deltree non-existent ==="
$KV deltree /e/nonexist/



