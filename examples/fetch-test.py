#!/usr/bin/env python3
"""fetch-test.py - host-side check for examples/fetch-ria (exit mode, stdlib only).

launch-test.sh runs it after the program has exited, with:
    WTL_EXIT_CODE   program exit code
    WTL_STDOUT      file with the program's stdout     WTL_STDERR  same for stderr
    WTL_OS, WTL_RUN_ID, WTL_RESULTS
Exit 0 = all checks passed.

    ./launch-test.sh examples/fetch-ria/fetch-ria.zip fetch-ria.exe 2012 --test examples/fetch-test.py
"""
import json
import os
import sys


def main():
    code = os.environ.get("WTL_EXIT_CODE", "")
    out = open(os.environ["WTL_STDOUT"], "rb").read().decode("utf-8", "replace")
    fields = {}
    for line in out.splitlines():
        k, sep, v = line.partition(": ")
        if sep and k and " " not in k:
            fields[k] = v.strip()
    results = []

    def check(name, ok, detail=""):
        results.append({"name": name, "ok": bool(ok), "detail": detail})
        print("%s  %-10s %s" % ("PASS" if ok else "FAIL", name, detail), flush=True)

    check("exit", code == "0", "exit code %r" % code)
    check("started", out.startswith("fetch-ria "), "first line %r" % (out.splitlines()[:1] or [""])[0])
    check("status", fields.get("status") == "200", "HTTP status %r" % fields.get("status"))
    try:
        n = int(fields.get("bytes", "0"))
    except ValueError:
        n = 0
    check("bytes", n > 1000, "%d bytes" % n)
    check("title", bool(fields.get("title")), repr(fields.get("title")))
    check("result", "RESULT OK" in out, next((l for l in out.splitlines() if l.startswith("RESULT")), "no RESULT line"))

    passed = all(r["ok"] for r in results)
    res = os.environ.get("WTL_RESULTS")
    if res:
        with open(os.path.join(res, "fetch-test.json"), "w") as fh:
            json.dump({"passed": passed, "windows": fields.get("windows"), "checks": results}, fh, indent=2)
    print("fetch-test: %s on %s (windows %s)" % ("PASS" if passed else "FAIL",
          os.environ.get("WTL_OS", "?"), fields.get("windows", "?")), flush=True)
    return 0 if passed else 1


if __name__ == "__main__":
    sys.exit(main())
