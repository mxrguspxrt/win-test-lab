#!/usr/bin/env python3
"""isoinfo.py - pure-python (stdlib only) reader for Windows install media.

Reads the UDF file system (1.02 / 2.01 / 2.50 incl. metadata partition) of a
Windows ISO, falling back to ISO9660 (Joliet preferred) when there is no UDF.
Can parse the XML of sources/install.wim (or install.esd).

CLI:
    isoinfo.py ls ISO [PATH]                list dir
    isoinfo.py cat ISO PATH                 write file to stdout
    isoinfo.py wim ISO                      JSON list of images
    isoinfo.py pick ISO EDITION INST_TYPE   print index of matching image (exit 1 if none)
    isoinfo.py build ISO                    print major build of image 1
    isoinfo.py arch ISO                     print the architecture of image 1: x64, arm64, x86

Exit codes: 0 ok, 1 no match (pick), 2 no install.wim/esd on media,
3 unreadable media / path not found, 64 usage.
The ISO is only ever opened read-only.
"""
import json
import os
import struct
import sys
import xml.etree.ElementTree as ET

SECTOR = 2048


class MediaError(Exception):
    pass


class NoWim(Exception):
    pass


class Entry(object):
    """A file/dir: extents is a list of (absolute_byte_offset or None(=zeros), length)."""
    __slots__ = ("name", "is_dir", "size", "extents", "inline", "fs")

    def __init__(self, name, is_dir, size, extents, inline=None, fs=None):
        self.name = name
        self.is_dir = is_dir
        self.size = size
        self.extents = extents
        self.inline = inline
        self.fs = fs


class Image(object):
    def __init__(self, path):
        self.path = path
        self.f = open(path, "rb")
        self.f.seek(0, 2)
        self.size = self.f.tell()

    def pread(self, off, n):
        if off < 0 or off >= self.size:
            return b""
        self.f.seek(off)
        return self.f.read(n)

    def sector(self, lba, count=1):
        return self.pread(lba * SECTOR, count * SECTOR)

    def read_entry(self, e, off=0, n=None):
        """Read n bytes at off from an Entry's data (handles fragmented extents)."""
        if n is None:
            n = e.size - off
        if off >= e.size or n <= 0:
            return b""
        n = min(n, e.size - off)
        if e.inline is not None:
            return e.inline[off:off + n]
        out = []
        pos = 0
        remaining = n
        for (eoff, elen) in e.extents:
            if remaining <= 0:
                break
            if off < pos + elen:
                start = max(off - pos, 0)
                take = min(elen - start, remaining)
                if eoff is None:
                    out.append(b"\0" * take)
                else:
                    data = self.pread(eoff + start, take)
                    if len(data) < take:
                        data += b"\0" * (take - len(data))
                    out.append(data)
                remaining -= take
                off += take
            pos += elen
        res = b"".join(out)
        if len(res) < n:
            res += b"\0" * (n - len(res))
        return res


# ---------------------------------------------------------------- UDF
def _tag_ok(buf, want=None):
    if len(buf) < 16:
        return False
    tid = struct.unpack_from("<H", buf, 0)[0]
    if want is not None and tid != want:
        return False
    s = (sum(buf[0:4]) + sum(buf[5:16])) & 0xFF
    return s == buf[4]


def _tag_id(buf):
    return struct.unpack_from("<H", buf, 0)[0] if len(buf) >= 2 else -1


def _dstring_osta(b):
    """Decode OSTA compressed unicode (comp id 8 or 16)."""
    if not b:
        return ""
    cid = b[0]
    if cid == 8:
        return b[1:].decode("latin-1")
    if cid == 16:
        body = b[1:]
        if len(body) % 2:
            body = body[:-1]
        return body.decode("utf-16-be", "replace")
    if cid in (254, 255):  # deleted-name variants
        return ""
    return b[1:].decode("latin-1", "replace")


class UDF(object):
    def __init__(self, img):
        self.img = img
        avdp = None
        for lba in (256, img.size // SECTOR - 1, img.size // SECTOR - 257):
            if lba <= 0:
                continue
            b = img.sector(lba)
            if _tag_ok(b, 2):
                avdp = b
                break
        if avdp is None:
            raise MediaError("no UDF anchor")
        main_len, main_loc, res_len, res_loc = struct.unpack_from("<IIII", avdp, 16)
        self.pds = {}
        self.lvd = None
        ok = self._read_vds(main_loc, main_len)
        if not ok:
            self._read_vds(res_loc, res_len)
        if self.lvd is None or not self.pds:
            raise MediaError("UDF volume descriptor sequence incomplete")
        lvd = self.lvd
        self.bs = struct.unpack_from("<I", lvd, 212)[0] or SECTOR
        if self.bs != SECTOR:
            raise MediaError("unsupported UDF block size %d" % self.bs)
        self.fsd_ad = self._long_ad(lvd, 248)
        mt_len, n_maps = struct.unpack_from("<II", lvd, 264)
        self.maps = []  # list of dicts
        p = 440
        for _ in range(n_maps):
            mtype, mlen = lvd[p], lvd[p + 1]
            if mtype == 1:
                vsn, pnum = struct.unpack_from("<HH", lvd, p + 2)
                self.maps.append({"type": 1, "pnum": pnum})
            elif mtype == 2:
                ident = lvd[p + 5:p + 28].rstrip(b"\0")
                vsn, pnum = struct.unpack_from("<HH", lvd, p + 36)
                m = {"type": 2, "pnum": pnum, "ident": ident.decode("latin-1", "replace")}
                if ident.startswith(b"*UDF Metadata Partition"):
                    mfl, mml, mbl = struct.unpack_from("<III", lvd, p + 40)
                    m.update(kind="metadata", mfile=mfl, mmirror=mml)
                else:
                    m.update(kind="unsupported")
                self.maps.append(m)
            else:
                self.maps.append({"type": mtype, "kind": "unsupported"})
            p += mlen if mlen else 6
        # resolve metadata partitions
        for i, m in enumerate(self.maps):
            if m.get("kind") == "metadata":
                m["extents"] = None
                phys = self._phys_ref(m["pnum"])
                for loc in (m["mfile"], m["mmirror"]):
                    try:
                        e = self._read_icb_at(phys, loc, "<metadata>")
                        m["extents"] = e.extents
                        break
                    except MediaError:
                        continue
                if m["extents"] is None:
                    raise MediaError("cannot read UDF metadata file")
        # file set descriptor
        fsd_part, fsd_lbn = self.fsd_ad[1], self.fsd_ad[2]
        fsd = self._read_block(fsd_part, fsd_lbn)
        if _tag_id(fsd) != 256:
            raise MediaError("no UDF file set descriptor")
        self.root_ad = self._long_ad(fsd, 400)

    def _read_vds(self, loc, length):
        lba = loc
        end = loc + max(length // SECTOR, 1)
        seen = 0
        while lba < end and seen < 256:
            seen += 1
            b = self.img.sector(lba)
            if not _tag_ok(b):
                return False
            tid = _tag_id(b)
            if tid == 5:
                pnum = struct.unpack_from("<H", b, 22)[0]
                start, plen = struct.unpack_from("<II", b, 188)
                self.pds[pnum] = (start, plen)
            elif tid == 6:
                self.lvd = b
            elif tid == 3:  # volume descriptor pointer
                nlen, nloc = struct.unpack_from("<II", b, 20)
                lba, end = nloc, nloc + max(nlen // SECTOR, 1)
                continue
            elif tid == 8:  # terminating descriptor
                break
            lba += 1
        return self.lvd is not None and bool(self.pds)

    @staticmethod
    def _long_ad(buf, off):
        ln, lbn, pref = struct.unpack_from("<IIH", buf, off)
        return (ln, pref, lbn)

    def _phys_ref(self, pnum):
        for i, m in enumerate(self.maps):
            if m["type"] == 1 and m["pnum"] == pnum:
                return i
        raise MediaError("no type-1 map for partition %d" % pnum)

    def map_extent(self, pref, lbn, length):
        """Map a logical extent to absolute (byte_offset, len) pieces."""
        if pref >= len(self.maps):
            raise MediaError("bad partition reference %d" % pref)
        m = self.maps[pref]
        if m["type"] == 1:
            if m["pnum"] not in self.pds:
                raise MediaError("missing partition descriptor %d" % m["pnum"])
            start = self.pds[m["pnum"]][0]
            return [((start + lbn) * self.bs, length)]
        if m.get("kind") == "metadata":
            want = lbn * self.bs
            out = []
            pos = 0
            remaining = length
            for (eoff, elen) in m["extents"]:
                if remaining <= 0:
                    break
                if want < pos + elen:
                    s = max(want - pos, 0)
                    take = min(elen - s, remaining)
                    out.append((None if eoff is None else eoff + s, take))
                    remaining -= take
                    want += take
                pos += elen
            if remaining > 0:
                raise MediaError("metadata extent out of range")
            return out
        raise MediaError("unsupported partition map %r" % (m,))

    def _read_block(self, pref, lbn):
        pieces = self.map_extent(pref, lbn, self.bs)
        off = pieces[0][0]
        if off is None:
            return b"\0" * self.bs
        return self.img.pread(off, self.bs)

    def _read_icb_at(self, pref, lbn, name):
        b = self._read_block(pref, lbn)
        tid = _tag_id(b)
        if not _tag_ok(b) or tid not in (261, 266):
            raise MediaError("bad file entry at %d:%d (tag %d)" % (pref, lbn, tid))
        ftype = b[27]
        flags = struct.unpack_from("<H", b, 34)[0]
        adtype = flags & 7
        info_len = struct.unpack_from("<Q", b, 56)[0]
        if tid == 261:
            l_ea, l_ad = struct.unpack_from("<II", b, 168)
            base = 176
        else:
            l_ea, l_ad = struct.unpack_from("<II", b, 208)
            base = 216
        ad = b[base + l_ea: base + l_ea + l_ad]
        is_dir = ftype == 4
        e = Entry(name, is_dir, info_len, [], fs=self)
        if adtype == 3:
            e.inline = ad[:info_len]
            return e
        e.extents = self._parse_ads(ad, adtype, pref)
        return e

    def _parse_ads(self, ad, adtype, pref):
        exts = []
        guard = 0
        while True:
            guard += 1
            if guard > 100000:
                raise MediaError("allocation descriptor loop")
            step = 8 if adtype == 0 else 16
            nxt = None
            p = 0
            while p + step <= len(ad):
                if adtype == 0:
                    raw, pos = struct.unpack_from("<II", ad, p)
                    apref = pref
                elif adtype == 1:
                    raw, pos, apref = struct.unpack_from("<IIH", ad, p)
                else:
                    raise MediaError("extended allocation descriptors unsupported")
                p += step
                etype = raw >> 30
                elen = raw & 0x3FFFFFFF
                if elen == 0:
                    break
                if etype == 3:
                    nxt = (apref, pos, elen)
                    break
                if etype == 0:
                    exts.extend(self.map_extent(apref, pos, elen))
                else:
                    exts.append((None, elen))
            if nxt is None:
                break
            blk = self._read_block(nxt[0], nxt[1])
            if _tag_id(blk) != 258:
                raise MediaError("bad allocation extent descriptor")
            l_ad = struct.unpack_from("<I", blk, 20)[0]
            ad = blk[24:24 + l_ad]
            pref = nxt[0]
        return exts

    def root(self):
        ln, pref, lbn = self.root_ad
        return self._read_icb_at(pref, lbn, "")

    def listdir(self, d):
        data = self.img.read_entry(d)
        out = []
        p = 0
        while p + 38 <= len(data):
            if _tag_id(data[p:p + 2]) != 257:
                # skip to next 4-byte boundary hoping to resync; normally never hit
                p += 4
                continue
            chars = data[p + 18]
            l_fi = data[p + 19]
            ln, lbn, pref = struct.unpack_from("<IIH", data, p + 20)
            l_iu = struct.unpack_from("<H", data, p + 36)[0]
            name_b = data[p + 38 + l_iu: p + 38 + l_iu + l_fi]
            total = 38 + l_iu + l_fi
            total = (total + 3) & ~3
            p += total
            if chars & 0x08 or chars & 0x04:  # parent or deleted
                continue
            name = _dstring_osta(name_b)
            if not name:
                continue
            try:
                e = self._read_icb_at(pref, lbn, name)
            except MediaError:
                continue
            if chars & 0x02:
                e.is_dir = True
            out.append(e)
        return out


# ---------------------------------------------------------------- ISO9660
class ISO9660(object):
    def __init__(self, img):
        self.img = img
        pvd = None
        joliet = None
        lba = 16
        while lba < 16 + 64:
            b = img.sector(lba)
            if len(b) < SECTOR or b[1:6] != b"CD001":
                break
            t = b[0]
            if t == 1 and pvd is None:
                pvd = b
            elif t == 2 and b[88:91] in (b"%/@", b"%/C", b"%/E"):
                joliet = b
            elif t == 255:
                break
            lba += 1
        if pvd is None and joliet is None:
            raise MediaError("no ISO9660 volume descriptor")
        self.joliet = joliet is not None
        vd = joliet if joliet is not None else pvd
        self.root_rec = self._parse_rec(vd, 156)

    def _parse_rec(self, buf, p):
        ln = buf[p]
        if ln == 0:
            return None
        ext = struct.unpack_from("<I", buf, p + 2)[0]
        size = struct.unpack_from("<I", buf, p + 10)[0]
        flags = buf[p + 25]
        nlen = buf[p + 32]
        raw = buf[p + 33:p + 33 + nlen]
        return (ln, ext, size, flags, raw)

    def _name(self, raw):
        if raw in (b"\0", b"\1"):
            return None
        if self.joliet:
            n = raw.decode("utf-16-be", "replace")
        else:
            n = raw.decode("latin-1")
        if ";" in n:
            n = n.split(";")[0]
        if not self.joliet and n.endswith(".") :
            n = n[:-1]
        return n

    def root(self):
        ln, ext, size, flags, raw = self.root_rec
        return Entry("", True, size, [(ext * SECTOR, size)], fs=self)

    def listdir(self, d):
        data = self.img.read_entry(d)
        out = []
        p = 0
        last = None
        while p < len(data):
            ln = data[p]
            if ln == 0:
                p = (p // SECTOR + 1) * SECTOR
                continue
            ln, ext, size, flags, raw = self._parse_rec(data, p)
            p += ln
            name = self._name(raw)
            if name is None:
                continue
            if last is not None and last.name == name and last._multi:
                last.extents.append((ext * SECTOR, size))
                last.size += size
                last._multi = bool(flags & 0x80)
                continue
            e = _IsoEntry(name, bool(flags & 0x02), size, [(ext * SECTOR, size)], fs=self)
            e._multi = bool(flags & 0x80)
            out.append(e)
            last = e
        return out


class _IsoEntry(Entry):
    __slots__ = ("_multi",)


# ---------------------------------------------------------------- generic
class Media(object):
    def __init__(self, path):
        self.img = Image(path)
        self.kind = None
        try:
            self.fs = UDF(self.img)
            self.kind = "udf"
        except (MediaError, struct.error, IndexError):
            try:
                self.fs = ISO9660(self.img)
                self.kind = "iso9660+joliet" if self.fs.joliet else "iso9660"
            except (MediaError, struct.error, IndexError):
                raise MediaError("%s: neither UDF nor ISO9660 readable" % path)

    def lookup(self, path):
        cur = self.fs.root()
        for part in [x for x in path.replace("\\", "/").split("/") if x]:
            if not cur.is_dir:
                return None
            found = None
            for e in self.fs.listdir(cur):
                if e.name.lower() == part.lower():
                    found = e
                    break
            if found is None:
                return None
            cur = found
        return cur

    def listdir(self, path=""):
        d = self.lookup(path)
        if d is None:
            raise MediaError("not found: %s" % path)
        if not d.is_dir:
            return [d]
        return self.fs.listdir(d)

    def read(self, e, off=0, n=None):
        return self.img.read_entry(e, off, n)


def read_wim_xml(media):
    e = None
    for cand in ("sources/install.wim", "sources/install.esd", "sources/install.swm"):
        e = media.lookup(cand)
        if e is not None and not e.is_dir:
            break
        e = None
    if e is None:
        raise NoWim("no sources/install.wim or install.esd on media")
    hdr = media.read(e, 0, 208)
    if hdr[:8] != b"MSWIM\0\0\0":
        raise MediaError("install image has no MSWIM header")
    raw_size = struct.unpack_from("<Q", hdr, 0x48)[0]
    size = raw_size & 0x00FFFFFFFFFFFFFF
    off, orig = struct.unpack_from("<QQ", hdr, 0x50)
    n = orig if 0 < orig < 64 * 1024 * 1024 else size
    if n <= 0 or n > 64 * 1024 * 1024:
        raise MediaError("implausible WIM XML size %d" % n)
    data = media.read(e, off, n)
    if data[:2] == b"\xff\xfe":
        txt = data[2:].decode("utf-16-le", "replace")
    elif data[:2] == b"\xfe\xff":
        txt = data[2:].decode("utf-16-be", "replace")
    else:
        txt = data.decode("utf-16-le", "replace")
    txt = txt.rstrip("\0").strip()
    return txt


def _int(s):
    try:
        return int(str(s).strip())
    except (TypeError, ValueError):
        return None


def wim_images(media):
    txt = read_wim_xml(media)
    root = ET.fromstring(txt)
    imgs = []
    for im in root.findall("IMAGE"):
        def g(path):
            el = im.find(path)
            return el.text.strip() if el is not None and el.text else None
        imgs.append({
            "index": _int(im.get("INDEX")),
            "name": g("NAME") or g("DISPLAYNAME"),
            "edition_id": g("WINDOWS/EDITIONID") or g("FLAGS"),
            "installation_type": g("WINDOWS/INSTALLATIONTYPE"),
            "build": _int(g("WINDOWS/VERSION/BUILD")),
            "spbuild": _int(g("WINDOWS/VERSION/SPBUILD")),
            "arch": _int(g("WINDOWS/ARCH")),
        })
    imgs.sort(key=lambda x: x["index"] or 0)
    return imgs


# WIM <ARCH> values (PROCESSOR_ARCHITECTURE_*)
ARCH_NAMES = {0: "x86", 9: "x64", 12: "arm64"}


def pick(imgs, edition, inst_type):
    """Exact EditionID match first; then EDITION+"Eval" (evaluation media report
    e.g. ServerStandardEval). InstallationType must match exactly (case-insensitive):
    "Server" = Desktop Experience, "Server Core" = core."""
    want_t = inst_type.lower()
    for cand in (edition.lower(), edition.lower() + "eval"):
        for im in imgs:
            if (im["edition_id"] or "").lower() == cand and \
               (im["installation_type"] or "").lower() == want_t:
                return im["index"]
    return None


def main(argv):
    if len(argv) < 3:
        sys.stderr.write(__doc__)
        return 64
    cmd, iso = argv[1], argv[2]
    try:
        media = Media(iso)
        if cmd == "ls":
            path = argv[3] if len(argv) > 3 else ""
            for e in media.listdir(path):
                sys.stdout.write("%s %12d %s\n" % ("d" if e.is_dir else "-", e.size,
                                                    e.name + ("/" if e.is_dir else "")))
            return 0
        if cmd == "cat":
            e = media.lookup(argv[3])
            if e is None or e.is_dir:
                raise MediaError("not a file: %s" % argv[3])
            out = sys.stdout.buffer
            off = 0
            while off < e.size:
                chunk = media.read(e, off, min(4 << 20, e.size - off))
                out.write(chunk)
                off += len(chunk)
            return 0
        if cmd == "fstype":
            print(media.kind)
            return 0
        if cmd == "xml":
            print(read_wim_xml(media))
            return 0
        imgs = wim_images(media)
        if cmd == "wim":
            print(json.dumps(imgs, indent=1))
            return 0
        if cmd == "pick":
            if len(argv) < 5:
                sys.stderr.write("usage: isoinfo.py pick ISO EDITION INST_TYPE\n")
                return 64
            idx = pick(imgs, argv[3], argv[4])
            if idx is None:
                sys.stderr.write("isoinfo: no image with edition %s / type %s\n" % (argv[3], argv[4]))
                return 1
            print(idx)
            return 0
        if cmd == "build":
            if not imgs or imgs[0]["build"] is None:
                sys.stderr.write("isoinfo: no build number in WIM XML\n")
                return 2
            print(imgs[0]["build"])
            return 0
        if cmd == "arch":
            if not imgs or imgs[0]["arch"] is None:
                sys.stderr.write("isoinfo: no architecture in WIM XML\n")
                return 2
            print(ARCH_NAMES.get(imgs[0]["arch"], str(imgs[0]["arch"])))
            return 0
        sys.stderr.write("isoinfo: unknown command %s\n" % cmd)
        return 64
    except NoWim as ex:
        if cmd == "wim":
            print("[]")
        sys.stderr.write("isoinfo: %s: %s\n" % (iso, ex))
        return 2
    except (MediaError, ET.ParseError, OSError, struct.error) as ex:
        sys.stderr.write("isoinfo: %s: %s\n" % (iso, ex))
        return 3


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv))
    except BrokenPipeError:
        sys.exit(0)
