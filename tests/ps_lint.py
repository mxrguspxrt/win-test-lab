#!/usr/bin/env python3
"""ps_lint.py - tiny static checker for PowerShell 4.0 compatibility (win-test-lab guest scripts).

Usage: python3 tests/ps_lint.py [FILE.ps1 ...]   (default: guest/*.ps1)

Flags PS5+ constructs (::new(, Expand-Archive, class keyword, -AsHashtable, using namespace,
?. / ?[ null-conditional, ?? / ??= null-coalescing, ternary a ? b : c, && / || pipeline chains)
and checks (), [], {} balance plus unterminated strings / here-strings, ignoring comments
and string contents. Exit 0 = clean, 1 = problems found.
"""
import glob
import os
import re
import sys

FORBIDDEN = [
    (re.compile(r'::new\s*\(', re.I), '::new( constructor (PS5+)'),
    (re.compile(r'\bExpand-Archive\b', re.I), 'Expand-Archive (PS5+)'),
    (re.compile(r'\bCompress-Archive\b', re.I), 'Compress-Archive (PS5+)'),
    (re.compile(r'^\s*class\s+\w+', re.I), 'class keyword (PS5+)'),
    (re.compile(r'^\s*enum\s+\w+', re.I), 'enum keyword (PS5+)'),
    (re.compile(r'-AsHashtable\b', re.I), '-AsHashtable (PS6+)'),
    (re.compile(r'^\s*using\s+(namespace|module|assembly)\b', re.I), 'using statement (PS5+)'),
    (re.compile(r'\?\.\w|\?\['), 'null-conditional ?. / ?[ (PS7+)'),
    (re.compile(r'\?\?=?'), 'null-coalescing ?? (PS7+)'),
    (re.compile(r'&&|\|\|'), 'pipeline chain && / || (PS7+)'),
    (re.compile(r'\s\?\s[^:]*\s:\s'), 'ternary ?: (PS7+)'),
    (re.compile(r'\bGet-CimInstance\b', re.I), None),  # allowed in PS3+, informational only
]


def strip_code(text):
    """Return (code_lines, errors): text with comments removed and string contents blanked
    (quotes kept), plus a list of lexical errors. Line structure is preserved."""
    out = []
    errors = []
    i, n = 0, len(text)
    line = 1
    while i < n:
        c = text[i]
        if c == '\n':
            out.append(c); line += 1; i += 1; continue
        # block comment
        if text.startswith('<#', i):
            j = text.find('#>', i + 2)
            if j < 0:
                errors.append((line, 'unterminated <# block comment'))
                j = n
            seg = text[i:j + 2]
            out.append('\n' * seg.count('\n')); line += seg.count('\n'); i = j + 2; continue
        # line comment
        if c == '#':
            j = text.find('\n', i)
            if j < 0: j = n
            i = j; continue
        # here-strings
        if text.startswith('@"', i) or text.startswith("@'", i):
            q = text[i + 1]
            rest = text[i + 2:text.find('\n', i) if text.find('\n', i) >= 0 else n]
            if rest.strip() == '':
                m = re.compile(r'\n' + q + '@').search(text, i)
                if not m:
                    errors.append((line, 'unterminated here-string'))
                    j = n
                else:
                    j = m.end()
                seg = text[i:j]
                out.append('@' + q + '\n' * seg.count('\n') + q + '@'); line += seg.count('\n'); i = j; continue
        # strings
        if c in ('"', "'"):
            q = c
            j = i + 1
            start_line = line
            nl = 0
            while j < n:
                d = text[j]
                if d == '\n': nl += 1
                if q == '"' and d == '`':
                    if j + 1 < n and text[j + 1] == '\n': nl += 1
                    j += 2; continue
                if d == q:
                    if j + 1 < n and text[j + 1] == q:  # doubled quote escape
                        j += 2; continue
                    break
                j += 1
            if j >= n:
                errors.append((start_line, 'unterminated string'))
            out.append(q + '\n' * nl + q); line += nl; i = j + 1; continue
        # escaped char outside strings (e.g. line continuation)
        if c == '`':
            out.append(' '); i += 2
            if i - 1 < n and text[i - 1] == '\n': out.append('\n'); line += 1
            continue
        out.append(c); i += 1
    return ''.join(out), errors


def check_balance(code):
    errors = []
    pairs = {')': '(', ']': '[', '}': '{'}
    stack = []
    for ln, l in enumerate(code.split('\n'), 1):
        for ch in l:
            if ch in '([{':
                stack.append((ch, ln))
            elif ch in ')]}':
                if not stack:
                    errors.append((ln, 'unmatched %r' % ch))
                elif stack[-1][0] != pairs[ch]:
                    errors.append((ln, 'mismatched %r (opened %r at line %d)' % (ch, stack[-1][0], stack[-1][1])))
                    stack.pop()
                else:
                    stack.pop()
    for ch, ln in stack:
        errors.append((ln, 'unclosed %r' % ch))
    return errors


def lint(path):
    with open(path, 'r', encoding='utf-8-sig') as f:
        text = f.read().replace('\r\n', '\n')
    code, errors = strip_code(text)
    for ln, l in enumerate(code.split('\n'), 1):
        for rx, msg in FORBIDDEN:
            if msg and rx.search(l):
                errors.append((ln, 'forbidden: ' + msg))
    errors += check_balance(code)
    errors.sort()
    for ln, msg in errors:
        print('%s:%d: %s' % (path, ln, msg))
    return not errors


def main(argv):
    files = argv[1:]
    if not files:
        root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
        files = sorted(glob.glob(os.path.join(root, 'guest', '*.ps1')) +
                       glob.glob(os.path.join(root, 'examples', '**', '*.ps1'), recursive=True))
    ok = True
    for p in files:
        r = lint(p)
        print('%s: %s' % (p, 'OK' if r else 'FAIL'))
        ok = ok and r
    return 0 if ok else 1


if __name__ == '__main__':
    sys.exit(main(sys.argv))
