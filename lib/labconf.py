#!/usr/bin/env python3
"""labconf.py - parse and validate a win-test-lab RUN CONFIG (stdlib only).

A run config describes ONE launch of a lab (which base lab, which host folders to
mount as drives, which scripts to run and as whom). It is a simple key=value file:

    # myapp.conf
    version = 2019                         # the lab (labs/<version>.conf) to boot
    mount   = /home/me/app:X, /srv/data:Z  # host folder -> drive letter in the guest
    run_as_admin   = X:\\install.bat        # run as the lab's root (SYSTEM)
    run_as_student = Z:\\run.bat            # run as LAB\\student
    run_as_admin   = X:\\cleanup.bat        # ...and so on, in the order written
    name    = smoke                        # optional label for the results folder

Rules:
  - version is required.
  - mount entries are HOSTPATH:LETTER, comma-separated. LETTER is a single A-Z,
    not C (system) or D (reserved for the lab control disc). Letters are unique.
  - run_as_admin / run_as_student are comma-separated script references. Each must
    start with a drive letter that is mounted (X:\\.. or X:/..) or C:\\.. ; .bat .cmd
    .ps1 .exe only. run_as_admin runs as the SYSTEM account, run_as_student as LAB\\student.
  - the steps run IN THE ORDER THEY APPEAR: run_as_admin and run_as_student lines may be
    freely interleaved, so you can e.g. install as admin, run as the student, clean up as
    admin. A single line's comma-separated scripts run left to right.
  - unknown keys error.

Several configs can be layered: later files override earlier ones per key (mount / version /
name / wait / timeout_min), and a later file that defines ANY step replaces all earlier steps.
So a workload file + an OS file:
    lab run global.conf win11.conf          # version from win11.conf, the rest from global.conf

    labconf.py json CONFIG...    -> print the merged, parsed config as JSON (exit 2 on error)
    labconf.py check CONFIG...   -> validate only; print "ok: ..." or the error
"""
import json
import os
import re
import sys

SCALAR = {"version", "mount", "name", "wait", "timeout_min"}
STEP_KEYS = {"run_as_admin": "admin", "run_as_student": "student"}
KNOWN = SCALAR | set(STEP_KEYS)
SCRIPT_EXT = (".bat", ".cmd", ".ps1", ".exe")
LETTER_RE = re.compile(r"^[A-Za-z]$")
# a script ref like  X:\path  or  X:/path  or  C:\path
REF_RE = re.compile(r"^([A-Za-z]):[\\/](.*)$")


class ConfError(Exception):
    pass


def _resolve_mount_hosts(value, base_dir):
    """Make each mount's HOSTPATH absolute, relative to the config file's dir, so a merged
    config from several files has no ambiguity about which dir a relative path meant."""
    out = []
    for item in _split_list(value):
        if ":" not in item:
            out.append(item)  # malformed; build() will report it
            continue
        host, letter = item.rsplit(":", 1)
        host = host.strip()
        if host and not os.path.isabs(host):
            host = os.path.normpath(os.path.join(base_dir, host))
        out.append("%s:%s" % (host, letter.strip()))
    return ", ".join(out)


def parse_file(path):
    """-> (scalars dict, steps list). steps is an ordered list of (who, value) in file order."""
    if not os.path.isfile(path):
        raise ConfError("config not found: %s" % path)
    base_dir = os.path.dirname(os.path.abspath(path))
    raw = {}
    steps = []
    with open(path, encoding="utf-8") as f:
        for n, line in enumerate(f, 1):
            line = line.split("#", 1)[0].strip()
            if not line:
                continue
            if "=" not in line:
                raise ConfError("%s:%d: expected key = value" % (path, n))
            k, v = line.split("=", 1)
            k, v = k.strip(), v.strip()
            if k not in KNOWN:
                raise ConfError("%s:%d: unknown key %r (known: %s)"
                                % (path, n, k, ", ".join(sorted(KNOWN))))
            if k in STEP_KEYS:
                steps.append((STEP_KEYS[k], v))   # keep order; validated in build()
            elif k == "mount":
                v = _resolve_mount_hosts(v, base_dir)
                raw[k] = (raw[k] + ", " + v) if k in raw else v
            else:
                raw[k] = v   # scalar: last one in the file wins
    return raw, steps


def merge(parsed):
    """Merge a list of (raw, steps) left->right. Scalars: a later file's key replaces an
    earlier one. Steps: a later file that defines any step replaces all earlier steps."""
    out_raw = {}
    out_steps = []
    for raw, steps in parsed:
        out_raw.update(raw)
        if steps:
            out_steps = steps
    return out_raw, out_steps


def _split_list(v):
    return [x.strip() for x in v.split(",") if x.strip()]


def build(raw, steps, path="<config>", default_name="lab"):
    if "version" not in raw or not raw["version"]:
        raise ConfError("%s: 'version' is required (the lab to boot, e.g. 2019)" % path)
    version = raw["version"]
    if not re.match(r"^[A-Za-z0-9._-]+$", version):
        raise ConfError("%s: bad version %r" % (path, version))

    mounts = []
    seen = set()
    for item in _split_list(raw.get("mount", "")):
        if ":" not in item:
            raise ConfError("%s: mount %r must be HOSTPATH:LETTER" % (path, item))
        host, letter = item.rsplit(":", 1)
        host, letter = host.strip(), letter.strip().upper()
        if not host:
            raise ConfError("%s: mount %r has an empty host path" % (path, item))
        if not LETTER_RE.match(letter):
            raise ConfError("%s: mount drive %r must be a single letter A-Z" % (path, letter))
        if letter in ("C", "D"):
            raise ConfError("%s: drive %s: is reserved (C: system, D: lab disc); pick e.g. X, Y, Z" % (path, letter))
        if letter in seen:
            raise ConfError("%s: drive %s: used by more than one mount" % (path, letter))
        seen.add(letter)
        mounts.append({"host": host, "letter": letter})
    if len(mounts) > 3:
        raise ConfError("%s: at most 3 mounts (the lab disc takes one of the 4 CD slots)" % path)

    out = []
    for who, value in steps:
        key = "run_as_admin" if who == "admin" else "run_as_student"
        for ref in _split_list(value):
            m = REF_RE.match(ref)
            if not m:
                raise ConfError("%s: %s entry %r must be LETTER:\\path (e.g. X:\\run.bat)" % (path, key, ref))
            letter = m.group(1).upper()
            if letter not in seen and letter != "C":
                raise ConfError("%s: %s %r uses drive %s: which is not mounted" % (path, key, ref, letter))
            if not ref.lower().endswith(SCRIPT_EXT):
                raise ConfError("%s: %s %r must end in .bat .cmd .ps1 or .exe" % (path, key, ref))
            # normalise to backslashes for Windows
            out.append({"who": who, "path": letter + ":\\" + m.group(2).replace("/", "\\").lstrip("\\")})
    if not out:
        raise ConfError("%s: nothing to do (set run_as_admin and/or run_as_student)" % path)

    wait = int(raw.get("wait", "60"))
    timeout_min = int(raw.get("timeout_min", "60"))
    name = raw.get("name", "") or default_name
    if not re.match(r"^[A-Za-z0-9._-]+$", name):
        raise ConfError("%s: bad name %r" % (path, name))

    return {"version": version, "name": name, "mounts": mounts,
            "steps": out, "wait": wait, "timeout_min": timeout_min}


def load(paths):
    """paths: a single path or a list of paths (merged left->right)."""
    if isinstance(paths, str):
        paths = [paths]
    if not paths:
        raise ConfError("no config given")
    parsed = [parse_file(p) for p in paths]
    label = " + ".join(os.path.basename(p) for p in paths)
    raw, steps = merge(parsed)
    # the merged config's default name comes from the last file's stem
    return build(raw, steps, label, default_name=os.path.splitext(os.path.basename(paths[-1]))[0])


def main(argv):
    if len(argv) < 3 or argv[1] not in ("json", "check"):
        sys.stderr.write(__doc__)
        return 64
    paths = argv[2:]
    try:
        cfg = load(paths)
    except ConfError as e:
        sys.stderr.write("error: %s\n" % e)
        return 2
    if argv[1] == "json":
        print(json.dumps(cfg, indent=1))
    else:
        n_admin = sum(1 for s in cfg["steps"] if s["who"] == "admin")
        n_student = len(cfg["steps"]) - n_admin
        print("ok: %s -> lab %s, %d mount(s), %d step(s) (%d admin + %d student)"
              % (" + ".join(os.path.basename(p) for p in paths), cfg["version"],
                 len(cfg["mounts"]), len(cfg["steps"]), n_admin, n_student))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
