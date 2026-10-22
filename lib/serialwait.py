#!/usr/bin/env python3
"""serialwait.py - wait for WTL events in a (growing) serial log file.

    serialwait.py LOG --until EV1,EV2 [--fail EV3,EV4] [--timeout S] [--pid PID]
                      [--from-offset N] [--offset-file F] [--progress] [--quiet]
                      [--events-file F]

Lines look like  "WTL <EVENT> [key=value ...] [| text]"  (CRLF ok; leading junk
before "WTL " on a line is tolerated, since the serial port may print garbage at boot).
  --until        comma list of events that end the wait successfully   -> exit 0
  --fail         comma list of events that end the wait with failure   -> exit 1
  --timeout S    seconds (default: wait forever)                       -> exit 3
  --pid PID      stop if that process dies (after draining the log)    -> exit 4
  --from-offset  start reading at byte N (default 0)
  --offset-file  write the byte offset just after the matched line to F
                 (feed it to the next call's --from-offset)
  --progress     echo every WTL line to stderr as it arrives
  --events-file  append every WTL line seen to F (for result.json)
The matched line is printed on stdout.
"""
import os
import re
import sys
import time

LINE_RE = re.compile(r"(?:^|[^A-Za-z0-9_])WTL ([A-Z][A-Z0-9_]*)(?=\s|$)(.*)$")


def pid_alive(pid):
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    # a zombie child still "exists"; check /proc state when available
    try:
        with open("/proc/%d/stat" % pid) as f:
            st = f.read().rsplit(")", 1)[1].split()[0]
        if st in ("Z", "X"):
            return False
    except (OSError, IndexError):
        pass
    return True


def parse_line(raw):
    s = raw.decode("utf-8", "replace").rstrip("\r\n").replace("\r", "").replace("\0", "")
    m = LINE_RE.search(s)
    if not m:
        return None, s
    return m.group(1), "WTL " + m.group(1) + m.group(2)


def main(argv):
    if len(argv) < 2 or argv[1] in ("-h", "--help"):
        sys.stderr.write(__doc__)
        return 64
    log = argv[1]
    until, fail = set(), set()
    timeout = None
    pid = None
    offset = 0
    offset_file = None
    progress = False
    events_file = None
    i = 2
    while i < len(argv):
        a = argv[i]
        v = argv[i + 1] if i + 1 < len(argv) else None
        if a == "--until":
            until |= {x.strip() for x in v.split(",") if x.strip()}
            i += 2
        elif a == "--fail":
            fail |= {x.strip() for x in v.split(",") if x.strip()}
            i += 2
        elif a == "--timeout":
            timeout = float(v)
            i += 2
        elif a == "--pid":
            pid = int(v) if v else None
            i += 2
        elif a == "--from-offset":
            offset = int(v or 0)
            i += 2
        elif a == "--offset-file":
            offset_file = v
            i += 2
        elif a == "--events-file":
            events_file = v
            i += 2
        elif a == "--progress":
            progress = True
            i += 1
        elif a == "--quiet":
            progress = False
            i += 1
        else:
            sys.stderr.write("serialwait: bad argument %s\n" % a)
            return 64
    if not until and not fail:
        sys.stderr.write("serialwait: need --until and/or --fail\n")
        return 64
    deadline = None if timeout is None else time.time() + timeout
    f = None
    pos = offset
    pending = b""
    ev_out = open(events_file, "a") if events_file else None

    def finish(code, line=None, endpos=None):
        if line is not None:
            sys.stdout.write(line + "\n")
            sys.stdout.flush()
        if offset_file:
            try:
                with open(offset_file, "w") as of:
                    of.write("%d\n" % (endpos if endpos is not None else pos))
            except OSError:
                pass
        if ev_out:
            ev_out.close()
        return code

    dead_checks = 0
    while True:
        if f is None:
            try:
                f = open(log, "rb")
                f.seek(pos)
            except OSError:
                f = None
        got = False
        if f is not None:
            # detect truncation / replacement
            try:
                if os.path.getsize(log) < pos:
                    f.close()
                    f = open(log, "rb")
                    pos = 0
                    pending = b""
            except OSError:
                pass
            chunk = f.read(65536)
            if chunk:
                got = True
                data = pending + chunk
                start_pos = pos - len(pending)
                pos += len(chunk)
                lines = data.split(b"\n")
                pending = lines.pop()
                cur = start_pos
                for ln in lines:
                    cur += len(ln) + 1
                    ev, text = parse_line(ln)
                    if ev is None:
                        continue
                    if progress and ev != "OUTPUT":   # base64 chunks of program output: too noisy
                        sys.stderr.write("  [guest] %s\n" % text)
                        sys.stderr.flush()
                    if ev_out:
                        ev_out.write(text + "\n")
                        ev_out.flush()
                    if ev in until:
                        return finish(0, text, cur)
                    if ev in fail:
                        return finish(1, text, cur)
                if got:
                    continue
        if pid is not None and not pid_alive(pid):
            # drain once more after death, then give up (also consider final unterminated line)
            dead_checks += 1
            if dead_checks > 2:
                if pending:
                    ev, text = parse_line(pending)
                    if ev in until:
                        return finish(0, text, pos)
                    if ev in fail:
                        return finish(1, text, pos)
                sys.stderr.write("serialwait: process %d exited\n" % pid)
                return finish(4)
        if deadline is not None and time.time() >= deadline:
            sys.stderr.write("serialwait: timeout waiting for %s\n" % ",".join(sorted(until)))
            return finish(3)
        time.sleep(0.2)


if __name__ == "__main__":
    import signal
    signal.signal(signal.SIGTERM, lambda *a: os._exit(143))
    try:
        sys.exit(main(sys.argv))
    except KeyboardInterrupt:
        sys.exit(130)
