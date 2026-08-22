#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""运行 tutorial/*.sh，对比脚本头部注释中的 expected 输出；
并交叉校验 kvspace-c 与 kvspace-durable 的 head 编解码（rw/vid）字节一致。"""

import ctypes
import os
import struct
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DURABLE_SO = ROOT / "target" / "debug" / "libkvspace_durable.so"
KVSPACE_C_DIR = ROOT.parent / "kvspace-c"
KVSPACE_C_SO = KVSPACE_C_DIR / "build" / "libkvspace-c.so"

def extract_expected(script):
    """从脚本头部 # expected: ... # /end 提取预期输出行。"""
    lines = []
    in_block = False
    with open(script) as f:
        for raw in f:
            line = raw.rstrip('\n')
            if in_block:
                if line == '# /end':
                    break
                if line.startswith('# '):
                    lines.append(line[2:])
                elif line == '#':
                    lines.append('')
            elif line == '# expected:':
                in_block = True
    return lines

def run_script(script):
    """先 clear，再执行脚本，返回 stdout 行列表。"""
    kvbin = os.path.expanduser('~/.local/bin/kvspace')
    env = os.environ.copy()
    env.setdefault('KVLANG_KVSPACE', 'redis://127.0.0.1:6379')
    subprocess.run([kvbin, 'clear'], capture_output=True, timeout=10, env=env)
    r = subprocess.run(['bash', script], capture_output=True, text=True, timeout=30, env=env)
    return r.stdout.rstrip('\n').split('\n') if r.stdout.strip() else []

def test_script(script):
    expected = extract_expected(script)
    actual = run_script(script)
    if expected == actual:
        print(f'PASS  {os.path.basename(script)}')
        return True
    print(f'FAIL  {os.path.basename(script)}')
    print(f'  expected ({len(expected)} lines): {expected[:3]}...' if len(expected) > 3 else f'  expected: {expected}')
    print(f'  actual   ({len(actual)} lines):   {actual[:3]}...' if len(actual) > 3 else f'  actual:   {actual}')
    return False


# ── kvspace-c ↔ kvspace-durable 交叉校验（ctypes，字节级对齐） ──────────────

class HeadV(ctypes.Structure):
    _fields_ = [
        ("kind", ctypes.c_uint8 * 32),
        ("is_ptr", ctypes.c_uint8),
        ("ref", ctypes.c_uint8),
        ("ro", ctypes.c_uint8),
        ("array_len", ctypes.c_int32),
        ("body_len", ctypes.c_int32),
        ("body_offset", ctypes.c_int32),
        ("ndim", ctypes.c_int32),
        ("dims", ctypes.c_int32 * 8),
        ("vid", ctypes.c_uint32),
    ]


U8P = ctypes.POINTER(ctypes.c_uint8)
I32P = ctypes.POINTER(ctypes.c_int32)
U32P = ctypes.POINTER(ctypes.c_uint32)
U8PP = ctypes.POINTER(U8P)


def setup_lib(lib):
    lib.kvspaceTlvEncodeMode.argtypes = [ctypes.c_char_p, U8P, ctypes.c_uint32, I32P, ctypes.c_int32,
                                         ctypes.c_int, ctypes.c_uint8, ctypes.c_uint32, U8PP, U32P]
    lib.kvspaceTlvEncodeMode.restype = ctypes.c_int
    lib.kvspaceDecodeHeadV.argtypes = [U8P, ctypes.c_uint32, ctypes.POINTER(HeadV)]
    lib.kvspaceDecodeHeadV.restype = ctypes.c_int
    lib.kvspaceBytesFree.argtypes = [U8P, ctypes.c_uint32]


def encode(lib, kind, raw, dims=(), ref=0, ro=0, vid=0):
    raw_arr = (ctypes.c_uint8 * len(raw)).from_buffer_copy(raw)
    ndim = len(dims)
    dims_arr = (ctypes.c_int32 * ndim)(*dims) if ndim > 0 else None
    out = U8P()
    out_len = ctypes.c_uint32()
    rc = lib.kvspaceTlvEncodeMode(kind.encode(), raw_arr, len(raw), dims_arr, ndim,
                                  ref, ro, vid, ctypes.byref(out), ctypes.byref(out_len))
    assert rc == 0, f"encode({kind}) rc={rc}"
    data = ctypes.string_at(out, out_len.value)
    lib.kvspaceBytesFree(out, out_len.value)
    return data


def decode(lib, data):
    buf = (ctypes.c_uint8 * len(data)).from_buffer_copy(data)
    h = HeadV()
    rc = lib.kvspaceDecodeHeadV(buf, len(data), ctypes.byref(h))
    assert rc == 0, f"decode rc={rc}"
    return {
        "kind": bytes(h.kind).split(b"\0", 1)[0].decode(),
        "is_ptr": h.is_ptr, "ref": h.ref, "ro": h.ro, "vid": h.vid,
        "ndim": h.ndim, "dims": list(h.dims[:h.ndim]), "array_len": h.array_len,
        "body_len": h.body_len, "body_offset": h.body_offset,
    }


def test_kvspace_c_alignment():
    """kvspace-c 与 kvspace-durable 对同一 head 输入产出字节级一致的结果。"""
    if not DURABLE_SO.exists():
        subprocess.run(["cargo", "build"], cwd=ROOT, check=True, capture_output=True)
    if not KVSPACE_C_SO.exists():
        subprocess.run(["make", "kvspace-c"], cwd=KVSPACE_C_DIR / "build", check=True, capture_output=True)

    # kvspace-c 依赖 blockmalloc/slotsboxmalloc，先按绝对路径预加载（RTLD_GLOBAL），
    # 让后续 dlopen kvspace-c 时按 SONAME 命中已加载对象。
    for dep in ["blockmalloc/build/libblockmalloc.so.1", "slotsboxmalloc/build/libslotsboxmalloc.so"]:
        ctypes.CDLL(str(KVSPACE_C_DIR.parent / dep), mode=ctypes.RTLD_GLOBAL)

    lib_d = ctypes.CDLL(str(DURABLE_SO))
    lib_c = ctypes.CDLL(str(KVSPACE_C_SO))
    setup_lib(lib_d)
    setup_lib(lib_c)

    cases = [
        ("int64", struct.pack("<q", 42), (), 0, 0, 0),
        ("int64", struct.pack("<q", 7), (), 0, 1, 0x12345678),
        ("char/utf8", b"hello", (5,), 0, 1, 7),
        ("int32", struct.pack("<6i", 1, 2, 3, 4, 5, 6), (2, 3), 0, 0, 0),
        ("int64", b"/target", (), 1, 0, 9),
        ("int64", b"/gpu/tensor", (), 2, 0, 3),
    ]
    ok = True
    for kind, raw, dims, ref, ro, vid in cases:
        label = f"align {kind} ref={ref} ro={ro} vid={vid}"
        bd = encode(lib_d, kind, raw, dims, ref, ro, vid)
        bc = encode(lib_c, kind, raw, dims, ref, ro, vid)
        if bd != bc:
            print(f"FAIL  {label} (bytes differ)")
            ok = False
            continue
        if decode(lib_d, bd) != decode(lib_c, bc):
            print(f"FAIL  {label} (decode differs)")
            ok = False
            continue
        print(f"PASS  {label}")
    return ok


def main():
    scripts = sorted(
        os.path.join('tutorial', f) for f in os.listdir('tutorial')
        if f.endswith('.sh')
    )
    if not scripts:
        print('no scripts found')
        sys.exit(1)

    results = [test_script(s) for s in scripts]
    results.append(test_kvspace_c_alignment())
    passed = sum(results)
    print(f'\n{passed}/{len(results)} passed')
    sys.exit(0 if passed == len(results) else 1)

if __name__ == '__main__':
    main()
