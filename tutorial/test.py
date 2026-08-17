#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""运行 tutorial/*.sh，对比脚本头部注释中的 expected 输出。"""

import os, subprocess, sys

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

def main():
    scripts = sorted(
        os.path.join('tutorial', f) for f in os.listdir('tutorial')
        if f.endswith('.sh')
    )
    if not scripts:
        print('no scripts found')
        sys.exit(1)

    results = [test_script(s) for s in scripts]
    passed = sum(results)
    print(f'\n{passed}/{len(results)} passed')
    sys.exit(0 if passed == len(results) else 1)

if __name__ == '__main__':
    main()
