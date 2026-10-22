#!/usr/bin/env python3
"""outputs.py - decode program output captured by the guest agent (exit mode).

    outputs.py SERIAL_LOG RESULTS_DIR

The agent sends   WTL OUTPUT stream=stdout|stderr seq=N | <base64>
followed by       WTL OUTPUT_END stream=.. bytes=TOTAL sent=N truncated=0|1
and finally       WTL PROC_EXIT code=N secs=S   or   WTL PROC_TIMEOUT secs=S

Writes RESULTS_DIR/stdout.txt and stderr.txt (raw bytes, as the program wrote them)
and prints one JSON object on stdout:
  {"exit_code", "exit_hex", "exit_name", "timed_out", "secs",
   "stdout_bytes", "stderr_bytes", "stdout_truncated", "stderr_truncated", "missing_chunks"}
"""
import base64
import binascii
import json
import os
import re
import sys
import urllib.parse

LINE_RE = re.compile(r"WTL ([A-Z][A-Z0-9_]*)((?: [A-Za-z_]+=\S*)*)(?: \| (.*))?$")

# common Windows process exit codes (NTSTATUS) worth naming in a test report
NTSTATUS = {
    0xC0000005: "STATUS_ACCESS_VIOLATION (crash)",
    0xC000001D: "STATUS_ILLEGAL_INSTRUCTION (CPU instruction not supported)",
    0xC0000094: "STATUS_INTEGER_DIVIDE_BY_ZERO",
    0xC00000FD: "STATUS_STACK_OVERFLOW",
    0xC0000135: "STATUS_DLL_NOT_FOUND (a required DLL is missing)",
    0xC0000139: "STATUS_ENTRYPOINT_NOT_FOUND (DLL too old for this Windows)",
    0xC0000142: "STATUS_DLL_INIT_FAILED",
    0xC000007B: "STATUS_INVALID_IMAGE_FORMAT (32/64-bit mismatch or bad exe)",
    0xC0000409: "STATUS_STACK_BUFFER_OVERRUN (fail-fast / abort, e.g. Rust panic=abort)",
    0xC000013A: "STATUS_CONTROL_C_EXIT",
    0x40010004: "DBG_TERMINATE_PROCESS (killed)",
}


def parse(line):
    line = line.replace("\r", "").replace("\0", "")
    i = line.find("WTL ")
    if i < 0:
        return None
    m = LINE_RE.match(line[i:].rstrip())
    if not m:
        return None
    kv = {}
    for part in (m.group(2) or "").split():
        k, _, v = part.partition("=")
        kv[k] = urllib.parse.unquote(v)
    return m.group(1), kv, m.group(3) or ""


def main(argv):
    if len(argv) != 3:
        sys.stderr.write(__doc__)
        return 64
    serial, res = argv[1], argv[2]
    os.makedirs(res, exist_ok=True)
    # stdout/stderr are the quick-form program streams; lab config runs add stepNout/stepNerr.
    chunks = {"stdout": {}, "stderr": {}}
    end = {}
    info = {"exit_code": None, "exit_hex": None, "exit_name": None, "timed_out": False, "secs": None}
    try:
        data = open(serial, "rb").read().decode("utf-8", "replace")
    except OSError:
        data = ""
    for raw in data.split("\n"):
        p = parse(raw)
        if not p:
            continue
        ev, kv, text = p
        if ev == "OUTPUT" and kv.get("stream"):
            s = kv["stream"]
            chunks.setdefault(s, {})
            try:
                chunks[s][int(kv.get("seq", "0"))] = base64.b64decode(text.strip(), validate=False)
            except (ValueError, binascii.Error):
                pass
        elif ev == "OUTPUT_END" and kv.get("stream"):
            chunks.setdefault(kv["stream"], {})
            end[kv["stream"]] = kv
        elif ev == "PROC_EXIT":
            try:
                code = int(kv.get("code", ""))
            except ValueError:
                code = None
            info["exit_code"] = code
            if code is not None:
                u = code & 0xFFFFFFFF
                info["exit_hex"] = "0x%08X" % u
                info["exit_name"] = NTSTATUS.get(u)
            info["secs"] = kv.get("secs")
        elif ev == "PROC_TIMEOUT":
            info["timed_out"] = True
            info["secs"] = kv.get("secs")
    missing = 0
    for name, parts in chunks.items():
        body = b""
        if parts:
            for i in range(max(parts) + 1):
                if i in parts:
                    body += parts[i]
                else:
                    missing += 1
        with open(os.path.join(res, name + ".txt"), "wb") as f:
            f.write(body)
        e = end.get(name, {})
        info[name + "_bytes"] = int(e.get("bytes", len(body)) or 0)
        info[name + "_truncated"] = e.get("truncated") == "1"
    info["missing_chunks"] = missing
    print(json.dumps(info))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
