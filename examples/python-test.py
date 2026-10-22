#!/usr/bin/env python3
"""python-test.py - example host-side test for win-test-lab (stdlib only).

TEMPLATE: replace with your own test. launch-test.sh runs it as
    python3 python-test.py [--test-arg ...]
with these environment variables:
    WTL_HOST     host to connect to (127.0.0.1)
    WTL_PORT     host port forwarded to the first --port guest port
    WTL_PORT_<P> host port for guest port P
    WTL_OS, WTL_RUN_ID, WTL_RESULTS (directory for extra result files)
Exit code: 0 = all checks passed, 1 = something failed.

This one talks to the demo line-chat server (examples/demo/echo-server.ps1).
"""
import json
import os
import socket
import sys
import time

HOST = os.environ.get("WTL_HOST", "127.0.0.1")
PORT = int(os.environ.get("WTL_PORT", "5000"))
CONNECT_TIMEOUT = 60   # seconds to keep retrying the connection
IO_TIMEOUT = 10        # seconds per reply


def connect():
    """Connect with retry. The guest may still be warming up. Also make sure
    we get a greeting, because QEMU's user-net accepts the TCP connection on
    the host side even when nothing is listening in the guest yet."""
    deadline = time.time() + CONNECT_TIMEOUT
    last = None
    while time.time() < deadline:
        try:
            s = socket.create_connection((HOST, PORT), timeout=5)
            s.settimeout(IO_TIMEOUT)
            f = s.makefile("rwb")
            greeting = f.readline()
            if greeting:
                return s, f, greeting.decode("utf-8", "replace").rstrip("\r\n")
            s.close()
            last = "connection closed without greeting"
        except OSError as e:
            last = e
        time.sleep(1)
    raise RuntimeError("could not connect to %s:%d within %ds: %s" % (HOST, PORT, CONNECT_TIMEOUT, last))


def ask(f, line):
    f.write((line + "\r\n").encode("utf-8"))
    f.flush()
    reply = f.readline()
    if not reply:
        raise RuntimeError("connection closed after %r" % line)
    return reply.decode("utf-8", "replace").rstrip("\r\n")


def main():
    results = []

    def check(name, ok, detail=""):
        results.append({"name": name, "ok": bool(ok), "detail": detail})
        print("%s  %-10s %s" % ("PASS" if ok else "FAIL", name, detail), flush=True)

    print("python-test: connecting to %s:%d (os=%s)" % (HOST, PORT, os.environ.get("WTL_OS", "?")), flush=True)
    try:
        s, f, greeting = connect()
        check("greeting", greeting.startswith("HELLO"), repr(greeting))
        # (command, expected reply) - deterministic checks
        for name, cmd, want in [
            ("ping", "PING", "PONG"),
            ("echo", "ECHO hello world", "hello world"),
            ("add", "ADD 2 40", "42"),
            ("bye", "BYE", "BYE"),
        ]:
            try:
                got = ask(f, cmd)
                check(name, got == want, "%s -> %r (want %r)" % (cmd, got, want))
            except Exception as e:  # timeout / closed
                check(name, False, "%s -> %s" % (cmd, e))
        s.close()
    except Exception as e:
        check("connect", False, str(e))

    passed = all(r["ok"] for r in results)
    out_dir = os.environ.get("WTL_RESULTS")
    if out_dir:
        try:
            os.makedirs(out_dir, exist_ok=True)
            with open(os.path.join(out_dir, "python-test.json"), "w") as fh:
                json.dump({"host": HOST, "port": PORT, "passed": passed, "checks": results}, fh, indent=2)
        except OSError as e:
            print("warning: cannot write results: %s" % e, file=sys.stderr)
    print("python-test: %s (%d/%d checks ok)" % ("PASS" if passed else "FAIL",
          sum(r["ok"] for r in results), len(results)), flush=True)
    return 0 if passed else 1


if __name__ == "__main__":
    sys.exit(main())
