#!/bin/bash
# expected:
# === map head ===
# /m.	strkeymapindex	ref=0	ro=0	vid=0	ndim=2	dims=[2,3]
# === map list ===
# [0,0]	float32	1.0
# [0,1]	float32	3.140000104904175
# [1,2]	float32	6.28000020980835
# === get ===
# /m.[1,2]	float32:6.28000020980835
# /m.[9,9]	(nil)
# === obj vs coord ===
# x	1
# [0]	2
# /end

set -e
KV="$HOME/.local/bin/kvspace"
$KV deltree /m/
$KV deltree /h/

echo "=== map head ==="
$KV set /m. 'map[2,3]:'
$KV head /m.

echo "=== map list ==="
$KV set '/m.[0,1]' 'float32:3.14'
$KV set '/m.[1,2]' 'float32:6.28'
$KV set '/m.[0,0]' 'float32:1.0'
$KV list /m. --kind --showext=false

echo "=== get ==="
$KV get '/m.[1,2]' '/m.[9,9]'

echo "=== obj vs coord ==="
$KV set /h. 'objindex:'
$KV set /h.x 'int:1'
$KV set '/h.[0]' 'int:2'
$KV list /h. --kind=false --showext=false
