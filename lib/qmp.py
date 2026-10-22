#!/usr/bin/env python3
"""qmp.py - minimal QEMU Machine Protocol client (stdlib only).

    qmp.py SOCK screendump OUT.png   screenshot as PNG (QEMU >= 7.1 native png,
                                     older: ppm converted to png in pure python)
    qmp.py SOCK powerdown            system_powerdown (ACPI power button)
    qmp.py SOCK quit                 quit qemu immediately
    qmp.py SOCK status               print run state (e.g. "running")
    qmp.py SOCK raw JSON             send arbitrary {"execute":...} and print reply

Options: --timeout S (default 10).
Exit: 0 ok, 1 QMP error reply, 2 connect/protocol failure, 64 usage.
"""
import json
import os
import socket
import struct
import sys
import tempfile
import time
import zlib


class QMPError(Exception):
    pass


class QMP(object):
    def __init__(self, path, timeout=10.0):
        self.s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.s.settimeout(timeout)
        self.s.connect(path)
        self.buf = b""
        greet = self._recv()
        if "QMP" not in greet:
            raise QMPError("no QMP greeting: %r" % greet)
        self.cmd("qmp_capabilities")

    def _recv(self):
        while b"\n" not in self.buf:
            chunk = self.s.recv(65536)
            if not chunk:
                raise QMPError("connection closed")
            self.buf += chunk
        line, self.buf = self.buf.split(b"\n", 1)
        line = line.strip()
        if not line:
            return self._recv()
        return json.loads(line.decode("utf-8", "replace"))

    def cmd(self, name, args=None, allow_close=False):
        msg = {"execute": name}
        if args:
            msg["arguments"] = args
        self.s.sendall(json.dumps(msg).encode() + b"\n")
        while True:
            try:
                r = self._recv()
            except (QMPError, OSError):
                if allow_close:
                    return {}
                raise
            if "event" in r:
                continue
            if "error" in r:
                raise QMPError("%s: %s" % (r["error"].get("class"), r["error"].get("desc")))
            if "return" in r:
                return r["return"]

    def close(self):
        try:
            self.s.close()
        except OSError:
            pass


# ------------------------------------------------------------ PPM -> PNG
def _png_chunk(typ, data):
    c = typ + data
    return struct.pack(">I", len(data)) + c + struct.pack(">I", zlib.crc32(c) & 0xFFFFFFFF)


def ppm_to_png(ppm_bytes):
    data = ppm_bytes
    fields = []
    p = 0
    while len(fields) < 4:
        while p < len(data) and data[p:p + 1].isspace():
            p += 1
        if data[p:p + 1] == b"#":
            while p < len(data) and data[p:p + 1] not in (b"\n", b"\r"):
                p += 1
            continue
        q = p
        while q < len(data) and not data[q:q + 1].isspace():
            q += 1
        fields.append(data[p:q])
        p = q
    p += 1  # single whitespace after maxval
    if fields[0] != b"P6":
        raise ValueError("not a binary PPM (P6)")
    w, h, maxval = int(fields[1]), int(fields[2]), int(fields[3])
    if maxval != 255:
        raise ValueError("unsupported PPM maxval %d" % maxval)
    pix = data[p:p + w * h * 3]
    stride = w * 3
    raw = bytearray()
    for y in range(h):
        raw.append(0)
        raw += pix[y * stride:(y + 1) * stride]
    png = b"\x89PNG\r\n\x1a\n"
    png += _png_chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
    png += _png_chunk(b"IDAT", zlib.compress(bytes(raw), 6))
    png += _png_chunk(b"IEND", b"")
    return png


def _wait_file(path, timeout=5.0):
    end = time.time() + timeout
    last = -1
    while time.time() < end:
        try:
            sz = os.path.getsize(path)
            if sz > 0 and sz == last:
                return True
            last = sz
        except OSError:
            pass
        time.sleep(0.1)
    return os.path.exists(path)


def screendump(q, out):
    out = os.path.abspath(out)
    try:
        q.cmd("screendump", {"filename": out, "format": "png"})
        _wait_file(out, 3)
        with open(out, "rb") as f:
            if f.read(8) == b"\x89PNG\r\n\x1a\n":
                return
    except QMPError:
        pass
    except OSError:
        pass
    # old QEMU: PPM only
    fd, tmp = tempfile.mkstemp(suffix=".ppm", dir=os.path.dirname(out) or ".")
    os.close(fd)
    try:
        q.cmd("screendump", {"filename": tmp})
        _wait_file(tmp, 5)
        with open(tmp, "rb") as f:
            data = f.read()
        if data[:8] == b"\x89PNG\r\n\x1a\n":
            png = data
        else:
            png = ppm_to_png(data)
        with open(out, "wb") as f:
            f.write(png)
    finally:
        try:
            os.unlink(tmp)
        except OSError:
            pass


def main(argv):
    timeout = 10.0
    args = []
    i = 1
    while i < len(argv):
        if argv[i] == "--timeout" and i + 1 < len(argv):
            timeout = float(argv[i + 1])
            i += 2
            continue
        args.append(argv[i])
        i += 1
    if len(args) < 2:
        sys.stderr.write(__doc__)
        return 64
    sock, cmd = args[0], args[1]
    try:
        q = QMP(sock, timeout)
    except (OSError, QMPError, ValueError) as ex:
        sys.stderr.write("qmp: cannot connect to %s: %s\n" % (sock, ex))
        return 2
    try:
        if cmd == "screendump":
            if len(args) < 3:
                sys.stderr.write("usage: qmp.py SOCK screendump OUT.png\n")
                return 64
            screendump(q, args[2])
        elif cmd == "powerdown":
            q.cmd("system_powerdown")
        elif cmd == "quit":
            q.cmd("quit", allow_close=True)
        elif cmd == "status":
            r = q.cmd("query-status")
            print(r.get("status", "running" if r.get("running") else "unknown"))
        elif cmd == "raw":
            msg = json.loads(args[2])
            print(json.dumps(q.cmd(msg["execute"], msg.get("arguments"))))
        else:
            sys.stderr.write("qmp: unknown command %s\n" % cmd)
            return 64
    except QMPError as ex:
        sys.stderr.write("qmp: %s\n" % ex)
        return 1
    except (OSError, ValueError) as ex:
        sys.stderr.write("qmp: %s\n" % ex)
        return 2
    finally:
        q.close()
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
