#!/usr/bin/env python3
"""mkiso.py - pure-python (stdlib only) ISO9660 + Joliet image writer.

    mkiso.py -o out.iso -V VOLID SRC_DIR

* ISO9660 primary tree: level-2 style names (uppercase d-characters, max 30 chars
  for NAME.EXT plus ";1" version suffix for files, 31 chars for directories).
* Joliet supplementary tree (UCS-2 BE, escape "%/E" = level 3), original case,
  names up to 64 characters.
* L- and M-type path tables for both trees, both-endian fields, volume
  descriptor set terminator.
* Files and directories are recursively included (symlinks are followed).
  Single-extent files: anything >= 4 GiB is rejected with an error.
"""
import os
import struct
import sys
import time

SECTOR = 2048
MAX_EXTENT = 0xFFFFFFFF  # bytes in a single extent (32-bit length)


def both16(v):
    return struct.pack("<H", v) + struct.pack(">H", v)


def both32(v):
    return struct.pack("<I", v) + struct.pack(">I", v)


def pad_a(s, n, enc="ascii"):
    b = s.encode(enc, "replace")[:n]
    return b + b" " * (n - len(b))


def pad_j(s, n):
    """UCS-2 BE string padded with UCS-2 spaces, n bytes total (n even or odd)."""
    b = s.encode("utf-16-be")[: n - (n % 2)]
    b = b + " ".encode("utf-16-be") * ((n - len(b)) // 2)
    return b + b"\0" * (n - len(b))


def dt17(t):
    tm = time.gmtime(t)
    return ("%04d%02d%02d%02d%02d%02d00" % tm[:6]).encode("ascii") + b"\0"


DT17_ZERO = b"0" * 16 + b"\0"


def dt7(t):
    tm = time.gmtime(t)
    return struct.pack("BBBBBBb", tm.tm_year - 1900, tm.tm_mon, tm.tm_mday,
                       tm.tm_hour, tm.tm_min, tm.tm_sec, 0)


DCHARS = set("ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_")


def dchars(s):
    return "".join(c if c in DCHARS else "_" for c in s.upper())


class Node(object):
    def __init__(self, name, path, is_dir, parent=None):
        self.name = name          # original name
        self.path = path
        self.is_dir = is_dir
        self.parent = parent
        self.children = []
        self.size = 0 if is_dir else os.stat(path).st_size
        self.mtime = os.stat(path).st_mtime
        self.lba = 0              # file data LBA (files)
        self.iso_name = None      # bytes identifier, ISO9660 tree
        self.jol_name = None      # bytes identifier, Joliet tree
        # per tree directory info: lba, size, number
        self.d = {"iso": {}, "jol": {}}


def build_tree(src):
    root = Node("", src, True)
    stack = [root]
    while stack:
        d = stack.pop()
        try:
            names = sorted(os.listdir(d.path))
        except OSError as ex:
            raise SystemExit("mkiso: cannot list %s: %s" % (d.path, ex))
        for n in names:
            p = os.path.join(d.path, n)
            if os.path.isdir(p):
                c = Node(n, p, True, d)
                stack.append(c)
            elif os.path.isfile(p):
                c = Node(n, p, False, d)
                if c.size > MAX_EXTENT:
                    raise SystemExit("mkiso: %s is %d bytes; files >= 4 GiB are not supported"
                                     % (p, c.size))
            else:
                sys.stderr.write("mkiso: skipping special file %s\n" % p)
                continue
            d.children.append(c)
    return root


def assign_names(d):
    used_iso = set()
    used_jol = set()
    for c in d.children:
        # ---- ISO9660 level 2
        if c.is_dir:
            base = dchars(c.name)[:31] or "_"
            cand = base
            i = 1
            while cand in used_iso:
                suf = "~%d" % i
                cand = base[:31 - len(suf)] + suf
                i += 1
            used_iso.add(cand)
            c.iso_name = cand.encode("ascii")
        else:
            if "." in c.name.lstrip("."):
                stem, ext = c.name.rsplit(".", 1)
            else:
                stem, ext = c.name, ""
            stem = dchars(stem) or "_"
            ext = dchars(ext)[:8]
            maxstem = 30 - 1 - len(ext)
            cand = stem[:maxstem] + "." + ext
            i = 1
            while cand in used_iso:
                suf = "~%d" % i
                cand = stem[:maxstem - len(suf)] + suf + "." + ext
                i += 1
            used_iso.add(cand)
            c.iso_name = (cand + ";1").encode("ascii")
        # ---- Joliet (64 UCS-2 chars; disallowed chars replaced)
        jn = "".join("_" if (ord(ch) < 32 or ch in '*/:;?\\') else ch for ch in c.name)
        if any(ord(ch) > 0xFFFF for ch in jn):
            jn = "".join(ch if ord(ch) <= 0xFFFF else "_" for ch in jn)
        base = jn[:64]
        cand = base
        i = 1
        while cand.lower() in used_jol:
            suf = "~%d" % i
            if "." in base and not c.is_dir:
                st, ex = base.rsplit(".", 1)
                cand = (st[:64 - len(suf) - len(ex) - 1] + suf + "." + ex)[:64]
            else:
                cand = base[:64 - len(suf)] + suf
            i += 1
        used_jol.add(cand.lower())
        c.jol_name = cand.encode("utf-16-be")
    for c in d.children:
        if c.is_dir:
            assign_names(c)


def iso_sort_key(n):
    # ISO9660 9.3: sort by name then extension, padded with spaces (0x20 < any d-char)
    s = n.iso_name.split(b";")[0]
    if b"." in s and not n.is_dir:
        a, b = s.split(b".", 1)
    else:
        a, b = s, b""
    return (a.ljust(64, b" "), b.ljust(8, b" "))


def sorted_children(d, tree):
    if tree == "iso":
        return sorted(d.children, key=iso_sort_key)
    return sorted(d.children, key=lambda n: n.jol_name)


def rec_len(name_len):
    ln = 33 + name_len
    return ln + (ln & 1)


def dir_records_size(d, tree):
    size = 0
    names = [b"\0", b"\1"] + [getattr(c, tree + "_name")
                             for c in sorted_children(d, tree)]
    for nb in names:
        ln = rec_len(len(nb))
        if (size % SECTOR) + ln > SECTOR:
            size = (size // SECTOR + 1) * SECTOR
        size += ln
    return ((size + SECTOR - 1) // SECTOR) * SECTOR


def all_dirs_bfs(root, tree):
    """Directories in path-table order: by level, then parent number, then name."""
    out = [root]
    i = 0
    while i < len(out):
        d = out[i]
        for c in sorted_children(d, tree):
            if c.is_dir:
                out.append(c)
        i += 1
    for num, d in enumerate(out, 1):
        d.d[tree]["num"] = num
    return out


def make_record(name_b, lba, size, is_dir, mtime):
    ln = rec_len(len(name_b))
    r = struct.pack("BB", ln, 0) + both32(lba) + both32(size) + dt7(mtime) + \
        struct.pack("BBB", 0x02 if is_dir else 0x00, 0, 0) + both16(1) + \
        struct.pack("B", len(name_b)) + name_b
    if len(r) < ln:
        r += b"\0" * (ln - len(r))
    return r


def dir_data(d, tree):
    nm = "iso_name" if tree == "iso" else "jol_name"
    me = d.d[tree]
    par = d.parent.d[tree] if d.parent is not None else me
    recs = [make_record(b"\0", me["lba"], me["size"], True, d.mtime),
            make_record(b"\1", par["lba"], par["size"], True,
                        (d.parent or d).mtime)]
    for c in sorted_children(d, tree):
        if c.is_dir:
            recs.append(make_record(getattr(c, nm), c.d[tree]["lba"], c.d[tree]["size"],
                                    True, c.mtime))
        else:
            recs.append(make_record(getattr(c, nm), c.lba if c.size else 0, c.size,
                                    False, c.mtime))
    buf = bytearray()
    for r in recs:
        if (len(buf) % SECTOR) + len(r) > SECTOR:
            buf += b"\0" * (SECTOR - len(buf) % SECTOR)
        buf += r
    buf += b"\0" * (me["size"] - len(buf))
    assert len(buf) == me["size"]
    return bytes(buf)


def path_table(dirs, tree, big):
    nm = "iso_name" if tree == "iso" else "jol_name"
    out = bytearray()
    fmt_i = ">I" if big else "<I"
    fmt_h = ">H" if big else "<H"
    for d in dirs:
        name = b"\0" if d.parent is None else getattr(d, nm)
        parent_num = 1 if d.parent is None else d.parent.d[tree]["num"]
        out += struct.pack("BB", len(name), 0) + struct.pack(fmt_i, d.d[tree].get("lba", 0)) + \
            struct.pack(fmt_h, parent_num) + name
        if len(name) & 1:
            out += b"\0"
    return bytes(out)


def sectors(n):
    return (n + SECTOR - 1) // SECTOR


def volume_descriptor(joliet, volid, vol_sectors, pt_size, l_lba, m_lba, root, now):
    tree = "jol" if joliet else "iso"
    b = bytearray(SECTOR)
    b[0] = 2 if joliet else 1
    b[1:6] = b"CD001"
    b[6] = 1
    if joliet:
        b[8:40] = pad_j("", 32)
        b[40:72] = pad_j(volid[:16], 32)
        b[88:91] = b"%/E"
    else:
        b[8:40] = pad_a("", 32)
        b[40:72] = pad_a(dchars(volid)[:32], 32)
    b[80:88] = both32(vol_sectors)
    b[120:124] = both16(1)
    b[124:128] = both16(1)
    b[128:132] = both16(SECTOR)
    b[132:140] = both32(pt_size)
    struct.pack_into("<I", b, 140, l_lba)
    struct.pack_into("<I", b, 144, 0)
    struct.pack_into(">I", b, 148, m_lba)
    struct.pack_into(">I", b, 152, 0)
    rr = make_record(b"\0", root.d[tree]["lba"], root.d[tree]["size"], True, now)
    assert len(rr) == 34
    b[156:190] = rr
    if joliet:
        b[190:318] = pad_j("", 128)
        b[318:446] = pad_j("", 128)
        b[446:574] = pad_j("WIN-TEST-LAB", 128)
        b[574:702] = pad_j("MKISO.PY", 128)
        b[702:739] = pad_j("", 37)
        b[739:776] = pad_j("", 37)
        b[776:813] = pad_j("", 37)
    else:
        b[190:318] = pad_a("", 128)
        b[318:446] = pad_a("", 128)
        b[446:574] = pad_a("WIN-TEST-LAB", 128)
        b[574:702] = pad_a("MKISO.PY", 128)
        b[702:739] = pad_a("", 37)
        b[739:776] = pad_a("", 37)
        b[776:813] = pad_a("", 37)
    b[813:830] = dt17(now)
    b[830:847] = dt17(now)
    b[847:864] = DT17_ZERO
    b[864:881] = DT17_ZERO
    b[881] = 1
    return bytes(b)


def make_iso(src, out, volid):
    if not os.path.isdir(src):
        raise SystemExit("mkiso: %s is not a directory" % src)
    now = time.time()
    root = build_tree(src)
    assign_names(root)
    trees = ("iso", "jol")
    dirs = {t: all_dirs_bfs(root, t) for t in trees}
    for t in trees:
        for d in dirs[t]:
            d.d[t]["size"] = dir_records_size(d, t)
    pt_size = {t: len(path_table(dirs[t], t, False)) for t in trees}  # lba-independent
    # ---- layout
    lba = 16 + 3   # PVD, SVD, terminator
    pt_lba = {}
    for t in trees:
        pt_lba[t] = (lba, lba + sectors(pt_size[t]))
        lba += 2 * sectors(pt_size[t])
    for t in trees:
        for d in dirs[t]:
            d.d[t]["lba"] = lba
            lba += sectors(d.d[t]["size"])
    files = []
    stack = [root]
    while stack:
        d = stack.pop(0)
        for c in sorted_children(d, "iso"):
            if c.is_dir:
                stack.append(c)
            else:
                files.append(c)
    for f in files:
        if f.size:
            f.lba = lba
            lba += sectors(f.size)
    total = lba
    if total > 0xFFFFFFFF:
        raise SystemExit("mkiso: image too large")
    tmp = out + ".tmp"
    with open(tmp, "wb") as o:
        o.write(b"\0" * (16 * SECTOR))
        o.write(volume_descriptor(False, volid, total, pt_size["iso"], pt_lba["iso"][0],
                                  pt_lba["iso"][1], root, now))
        o.write(volume_descriptor(True, volid, total, pt_size["jol"], pt_lba["jol"][0],
                                  pt_lba["jol"][1], root, now))
        term = bytearray(SECTOR)
        term[0] = 255
        term[1:6] = b"CD001"
        term[6] = 1
        o.write(term)
        for t in trees:
            for big in (False, True):
                pt = path_table(dirs[t], t, big)
                assert o.tell() == pt_lba[t][1 if big else 0] * SECTOR
                o.write(pt + b"\0" * (sectors(len(pt)) * SECTOR - len(pt)))
        for t in trees:
            for d in dirs[t]:
                assert o.tell() == d.d[t]["lba"] * SECTOR
                o.write(dir_data(d, t))
        for f in files:
            if not f.size:
                continue
            assert o.tell() == f.lba * SECTOR
            left = f.size
            with open(f.path, "rb") as fi:
                while left > 0:
                    chunk = fi.read(min(left, 8 << 20))
                    if not chunk:
                        raise SystemExit("mkiso: %s shrank while reading" % f.path)
                    o.write(chunk)
                    left -= len(chunk)
            rem = f.size % SECTOR
            if rem:
                o.write(b"\0" * (SECTOR - rem))
        assert o.tell() == total * SECTOR
    os.replace(tmp, out)
    return total


def main(argv):
    out = None
    volid = "CDROM"
    src = None
    i = 1
    while i < len(argv):
        a = argv[i]
        if a == "-o" and i + 1 < len(argv):
            out = argv[i + 1]
            i += 2
        elif a == "-V" and i + 1 < len(argv):
            volid = argv[i + 1]
            i += 2
        elif a in ("-h", "--help"):
            print(__doc__)
            return 0
        elif src is None and not a.startswith("-"):
            src = a
            i += 1
        else:
            sys.stderr.write("mkiso: bad argument %s\n" % a)
            return 64
    if not out or not src:
        sys.stderr.write("usage: mkiso.py -o out.iso -V VOLID SRC_DIR\n")
        return 64
    make_iso(src, out, volid)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
