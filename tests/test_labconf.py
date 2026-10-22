#!/usr/bin/env python3
"""test_labconf.py - unit tests for lib/labconf.py (run config parser/validator).

    python3 tests/test_labconf.py        # or: python3 -m pytest tests/test_labconf.py

No VM, no Windows, stdlib only. Exit 0 = all passed.
"""
import os
import sys
import tempfile
import traceback

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "lib"))
import labconf  # noqa: E402

PASS = FAIL = 0


def check(name, cond, detail=""):
    global PASS, FAIL
    if cond:
        PASS += 1
        print("PASS %s" % name)
    else:
        FAIL += 1
        print("FAIL %s  %s" % (name, detail))


def load_text(text):
    with tempfile.NamedTemporaryFile("w", suffix=".conf", delete=False) as f:
        f.write(text)
        p = f.name
    try:
        return labconf.load(p)
    finally:
        os.unlink(p)


def expect_error(name, text, needle=""):
    try:
        load_text(text)
        check(name, False, "expected a ConfError, got none")
    except labconf.ConfError as e:
        check(name, needle in str(e), "error %r lacks %r" % (str(e), needle))
    except Exception as e:  # noqa: BLE001
        check(name, False, "wrong exception type: %r" % e)


def load_files(texts):
    """Write several configs to temp files and merge-load them (left->right)."""
    paths = []
    try:
        for t in texts:
            with tempfile.NamedTemporaryFile("w", suffix=".conf", delete=False, dir=".") as f:
                f.write(t)
                paths.append(f.name)
        return labconf.load(paths)
    finally:
        for p in paths:
            os.unlink(p)


def main():
    # --- valid: full config ---
    cfg = load_text("""
        version = 2019
        mount = /a:X, /b/c:Z
        run_as_admin = X:\\setup.bat, Z:\\go.ps1
        run_as_student = X:\\run.exe
        name = demo
    """)
    check("version", cfg["version"] == "2019")
    check("name", cfg["name"] == "demo")
    check("two mounts", len(cfg["mounts"]) == 2)
    check("mount letter X", cfg["mounts"][0] == {"host": "/a", "letter": "X"})
    check("mount letter Z", cfg["mounts"][1] == {"host": "/b/c", "letter": "Z"})
    check("steps", cfg["steps"] == [
        {"who": "admin", "path": "X:\\setup.bat"},
        {"who": "admin", "path": "Z:\\go.ps1"},
        {"who": "student", "path": "X:\\run.exe"},
    ])

    # --- steps run in the order written: admin/student may interleave ---
    cfg = load_text("""
        version = 2019
        mount = /a:X
        run_as_admin = X:\\install.bat
        run_as_student = X:\\run.exe
        run_as_admin = X:\\cleanup.bat
    """)
    check("ordered interleave", cfg["steps"] == [
        {"who": "admin", "path": "X:\\install.bat"},
        {"who": "student", "path": "X:\\run.exe"},
        {"who": "admin", "path": "X:\\cleanup.bat"},
    ])

    # --- slashes normalised, letters upper-cased ---
    cfg = load_text("version=2016\nmount=/x:z\nrun_as_student=z:/sub/app.exe\n")
    check("letter upper", cfg["mounts"][0]["letter"] == "Z")
    check("ref normalised", cfg["steps"] == [{"who": "student", "path": "Z:\\sub\\app.exe"}])

    # --- defaults ---
    cfg = load_text("version=2012\nmount=/x:X\nrun_as_student=X:\\a.bat\n")
    check("default wait", cfg["wait"] == 60)
    check("default timeout", cfg["timeout_min"] == 60)
    check("name from filename", cfg["name"])  # non-empty (temp file stem)

    # --- no mounts is allowed if a C: script runs ---
    cfg = load_text("version=2012\nrun_as_admin=C:\\Windows\\System32\\whoami.exe\n")
    check("no mount ok with C:", cfg["mounts"] == [] and cfg["steps"] == [
        {"who": "admin", "path": "C:\\Windows\\System32\\whoami.exe"}])

    # --- errors ---
    expect_error("missing version", "mount=/a:X\nrun_as_student=X:\\a.bat\n", "version")
    expect_error("unknown key", "version=2019\nfoo=bar\n", "unknown key")
    expect_error("no steps", "version=2019\nmount=/a:X\n", "nothing to do")
    expect_error("bad mount form", "version=2019\nmount=/a\nrun_as_student=C:\\x.bat\n", "HOSTPATH:LETTER")
    expect_error("reserved C", "version=2019\nmount=/a:C\nrun_as_student=C:\\x.bat\n", "reserved")
    expect_error("reserved D", "version=2019\nmount=/a:D\nrun_as_student=C:\\x.bat\n", "reserved")
    expect_error("dup letter", "version=2019\nmount=/a:X,/b:X\nrun_as_student=X:\\x.bat\n", "more than one")
    expect_error("two-char letter", "version=2019\nmount=/a:XY\nrun_as_student=C:\\x.bat\n", "single letter")
    expect_error("step bad ext", "version=2019\nmount=/a:X\nrun_as_student=X:\\a.txt\n", ".bat")
    expect_error("step unmounted drive", "version=2019\nmount=/a:X\nrun_as_student=Z:\\a.bat\n", "not mounted")
    expect_error("step no drive", "version=2019\nmount=/a:X\nrun_as_student=run.bat\n", "LETTER")
    expect_error("too many mounts",
                 "version=2019\nmount=/a:W,/b:X,/c:Y,/d:Z\nrun_as_student=W:\\a.bat\n", "at most 3")
    expect_error("missing = line", "version=2019\nmount /a:X\n", "expected key = value")

    # --- multi-file merge (later wins) ---
    cfg = load_files([
        "mount=/a:X\nrun_as_admin=X:\\install.bat\nrun_as_student=X:\\run.exe\n",  # workload
        "version=win11\n",                                                         # OS file
    ])
    check("merge: version from 2nd file", cfg["version"] == "win11")
    check("merge: mount from 1st file", cfg["mounts"] == [{"host": "/a", "letter": "X"}])
    check("merge: steps from 1st file", cfg["steps"] == [
        {"who": "admin", "path": "X:\\install.bat"},
        {"who": "student", "path": "X:\\run.exe"},
    ])

    cfg = load_files([
        "version=2019\nname=a\nrun_as_student=C:\\x.bat\n",
        "version=2022\nname=b\n",
    ])
    check("merge: later version wins", cfg["version"] == "2022")
    check("merge: later name wins", cfg["name"] == "b")
    check("merge: steps kept when 2nd has none", cfg["steps"] == [{"who": "student", "path": "C:\\x.bat"}])

    # mount in the later file replaces the earlier (per-key override, not append)
    cfg = load_files([
        "version=2019\nmount=/a:X\nrun_as_student=X:\\r.exe\n",
        "mount=/b:Z\nrun_as_student=Z:\\r.exe\n",
    ])
    check("merge: mount overridden", cfg["mounts"] == [{"host": "/b", "letter": "Z"}])
    # a later file that defines any step replaces all earlier steps
    check("merge: steps overridden", cfg["steps"] == [{"who": "student", "path": "Z:\\r.exe"}])

    print("\ntest_labconf: %d passed, %d failed" % (PASS, FAIL))
    return 1 if FAIL else 0


# pytest entry points (optional; the script runs standalone too)
def test_labconf_all():
    assert main() == 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception:  # noqa: BLE001
        traceback.print_exc()
        sys.exit(2)
