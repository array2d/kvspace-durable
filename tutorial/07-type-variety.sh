#!/bin/bash
# expected:
# === int ===
# /t/zero	int64:0
# /t/neg	int64:-9223372036854775808
# /t/pos	int64:9223372036854775807
# === float ===
# /t/f0	float64:0.0
# /t/fneg	float64:-3.141592653589793
# /t/fpos	float64:2.718281828459045
# === bool ===
# /t/yes	bool:true
# /t/no	bool:false
# === string ===
# /t/empty	char/utf8:
# /t/space	char/utf8:hello world
# /t/unicode	char/utf8:你好
# === nil ===
# /t/nothing	(nil)
# /end

set -e
KV="$HOME/.local/bin/kvspace"
$KV deltree /t/
$KV set /t/ index:

echo "=== int ==="
$KV set /t/zero int:0
$KV set /t/neg int:-9223372036854775808
$KV set /t/pos int:9223372036854775807
$KV get /t/zero /t/neg /t/pos

echo "=== float ==="
$KV set /t/f0 float:0
$KV set /t/fneg float:-3.141592653589793
$KV set /t/fpos float:2.718281828459045
$KV get /t/f0 /t/fneg /t/fpos

echo "=== bool ==="
$KV set /t/yes bool:true
$KV set /t/no bool:false
$KV get /t/yes /t/no

echo "=== string ==="
$KV set /t/empty string:
$KV set /t/space "string:hello world"
$KV set /t/unicode string:你好
$KV get /t/empty /t/space /t/unicode

echo "=== nil ==="
$KV set /t/nothing nil:
$KV get /t/nothing



