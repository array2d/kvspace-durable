#!/bin/bash
# expected:
# === default (rw, vid 0) ===
# /t14/a	int64	ref=0	ro=0	vid=0	ndim=0	dims=[]
# === read-only (ro=1) ===
# /t14/b	int64	ref=0	ro=1	vid=0	ndim=0	dims=[]
# === ro + vid ===
# /t14/c	char/utf8	ref=0	ro=1	vid=7	ndim=1	dims=[5]
# === soft link (ref=1) + vid ===
# /t14/d	int64	ref=1	ro=0	vid=9	ndim=0	dims=[]
# /end

set -e
KV="$HOME/.local/bin/kvspace"
$KV deltree /t14/

echo "=== default (rw, vid 0) ==="
$KV set /t14/a int:42
$KV head /t14/a

echo "=== read-only (ro=1) ==="
$KV set /t14/b int:7 ro
$KV head /t14/b

echo "=== ro + vid ==="
$KV set /t14/c string:hello ro 7
$KV head /t14/c

echo "=== soft link (ref=1) + vid ==="
$KV set /t14/d '*int64:/target' rw 9
$KV head /t14/d
