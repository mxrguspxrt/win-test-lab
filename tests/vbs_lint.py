#!/usr/bin/env python3
"""vbs_lint.py - a small static checker for the VBScript guest scripts (guest/2003/*.vbs).

Usage: python3 tests/vbs_lint.py [FILE.vbs ...]   (default: guest/**/*.vbs)

There is no VBScript interpreter on the Linux host, so this catches the mistakes that would
otherwise only show up in a VM run: unbalanced block statements (If / Sub / Function / For /
Do / While / Select), a block closed with the wrong keyword, a line continuation at the end of
the file, and any non-ASCII character (Windows Script Host reads these files as ANSI). Strings
and comments are ignored. Exit 0 = clean, 1 = problems found.
"""
import glob
import os
import re
import sys


def strip_line(line):
    """Return LINE without its comment and with string contents blanked (quotes kept)."""
    out = []
    i, n = 0, len(line)
    in_str = False
    while i < n:
        c = line[i]
        if in_str:
            if c == '"':
                if i + 1 < n and line[i + 1] == '"':  # "" is an escaped quote
                    out.append('  ')
                    i += 2
                    continue
                in_str = False
                out.append('"')
            else:
                out.append(' ')
        else:
            if c == '"':
                in_str = True
                out.append('"')
            elif c == "'":
                break
            else:
                out.append(c)
        i += 1
    return ''.join(out)


def statements(code):
    """Split one logical line into statements at ':' outside strings."""
    parts, cur, in_str = [], [], False
    for c in code:
        if c == '"':
            in_str = not in_str
        if c == ':' and not in_str:
            parts.append(''.join(cur))
            cur = []
        else:
            cur.append(c)
    parts.append(''.join(cur))
    return [p.strip() for p in parts if p.strip()]


def lint(path):
    problems = []
    with open(path, 'rb') as f:
        raw = f.read()
    for i, b in enumerate(raw):
        if b > 127:
            line = raw[:i].count(b'\n') + 1
            problems.append((line, 'non-ASCII byte 0x%02x (use ASCII only)' % b))
            break
    text = raw.decode('latin-1').replace('\r\n', '\n')
    lines = text.split('\n')

    stack = []  # (kind, line)
    pending = ''
    pending_line = 0
    for no, line in enumerate(lines, 1):
        code = strip_line(line).rstrip()
        if pending:
            code = pending + ' ' + code.lstrip()
            no_report = pending_line
        else:
            no_report = no
        if code.endswith('_'):
            pending = code[:-1].rstrip()
            pending_line = no_report
            continue
        pending = ''
        for st in statements(code):
            low = st.lower()
            first = re.match(r'[a-z_]+', low)
            word = first.group(0) if first else ''
            if word == 'if':
                m = re.search(r'\bthen\b(.*)$', low)
                if not m:
                    problems.append((no_report, "'If' without 'Then'"))
                elif m.group(1).strip() == '':
                    stack.append(('if', no_report))
                # else: a single-line If, no block
            elif word == 'elseif':
                if not stack or stack[-1][0] != 'if':
                    problems.append((no_report, "'ElseIf' outside an If block"))
            elif word == 'else':
                if not stack or stack[-1][0] != 'if':
                    problems.append((no_report, "'Else' outside an If block"))
            elif word == 'end':
                kind = low.split()[1] if len(low.split()) > 1 else ''
                if kind in ('if', 'sub', 'function', 'select', 'property', 'with', 'class'):
                    want = 'select' if kind == 'select' else kind
                    if not stack or stack[-1][0] != want:
                        got = stack[-1] if stack else None
                        problems.append((no_report, "'End %s' does not match %s" % (kind.title(), got if got else 'an empty block stack')))
                    else:
                        stack.pop()
            elif word in ('sub', 'function'):
                stack.append((word, no_report))
            elif word == 'for':
                stack.append(('for', no_report))
            elif word == 'next':
                if not stack or stack[-1][0] != 'for':
                    problems.append((no_report, "'Next' without a matching 'For'"))
                else:
                    stack.pop()
            elif word == 'do':
                stack.append(('do', no_report))
            elif word == 'loop':
                if not stack or stack[-1][0] != 'do':
                    problems.append((no_report, "'Loop' without a matching 'Do'"))
                else:
                    stack.pop()
            elif word == 'while':
                stack.append(('while', no_report))
            elif word == 'wend':
                if not stack or stack[-1][0] != 'while':
                    problems.append((no_report, "'Wend' without a matching 'While'"))
                else:
                    stack.pop()
            elif word == 'select':
                stack.append(('select', no_report))
    if pending:
        problems.append((pending_line, 'line continuation "_" at the end of the file'))
    for kind, line in stack:
        problems.append((line, "unclosed '%s'" % kind))
    problems.sort()
    for line, msg in problems:
        print('%s:%d: %s' % (path, line, msg))
    return not problems


def main(argv):
    files = argv[1:]
    if not files:
        root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
        files = sorted(glob.glob(os.path.join(root, 'guest', '**', '*.vbs'), recursive=True))
    ok = True
    for p in files:
        r = lint(p)
        print('%s: %s' % (p, 'OK' if r else 'FAIL'))
        ok = ok and r
    return 0 if ok else 1


if __name__ == '__main__':
    sys.exit(main(sys.argv))
