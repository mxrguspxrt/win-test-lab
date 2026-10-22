#!/usr/bin/env bash
# Build examples/demo/program.zip from the demo files (pure python3 zipfile).
# Contents: program.bat, echo-server.ps1 (+ program.exe if you built it from src/program.c:
#   x86_64-w64-mingw32-gcc -O2 -o examples/demo/program.exe examples/demo/src/program.c -lws2_32)
# Usage: examples/make-demo-zip.sh [OUT.zip] [EXTRA_FILE...]
#   EXTRA_FILE entries are added at the zip root (tests use this for fake-behavior.json).
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
demo="$here/demo"
out="${1:-$demo/program.zip}"
shift || true
python3 - "$demo" "$out" "$@" <<'PY'
import os, sys, zipfile
demo, out, extra = sys.argv[1], sys.argv[2], sys.argv[3:]
files = ["program.bat", "echo-server.ps1"]
if os.path.isfile(os.path.join(demo, "program.exe")):
    files.append("program.exe")
tmp = out + ".tmp"
with zipfile.ZipFile(tmp, "w", zipfile.ZIP_DEFLATED) as z:
    for f in files:
        z.write(os.path.join(demo, f), f)
    for e in extra:
        z.write(e, os.path.basename(e))
os.replace(tmp, out)
print("wrote %s (%s)" % (out, ", ".join(files + [os.path.basename(e) for e in extra])))
PY
