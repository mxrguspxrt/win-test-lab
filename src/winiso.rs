//! Read Windows install media: the UDF file system of a Windows ISO (1.02 / 2.01 / 2.50,
//! including the metadata partition), falling back to ISO9660 (Joliet preferred), and the
//! image list in `sources/install.wim` / `install.esd`.
//!
//! Only reads what picking an image needs: directory lookups and the WIM's XML header. The
//! ISO is opened read-only.

use anyhow::{anyhow, bail, Context, Result};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const SECTOR: u64 = 2048;

/// One image in the WIM (one edition).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WimImage {
    pub index: u32,
    pub name: String,
    pub edition_id: String,
    pub installation_type: String,
    pub build: Option<u32>,
    /// WIM `<ARCH>`: 0 x86, 9 x64, 12 arm64
    pub arch: Option<u32>,
}

impl WimImage {
    /// "x86", "x64", "arm64", or the raw number.
    pub fn arch_name(&self) -> String {
        match self.arch {
            Some(0) => "x86".into(),
            Some(9) => "x64".into(),
            Some(12) => "arm64".into(),
            Some(n) => n.to_string(),
            None => "?".into(),
        }
    }
}

/// The media has no `sources/install.wim` (or .esd): not an installation disc.
#[derive(Debug)]
pub struct NoWim;
impl std::fmt::Display for NoWim {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "no sources/install.wim or install.esd on the media (not an installation disc)")
    }
}
impl std::error::Error for NoWim {}

/// A file or directory: its data as (absolute byte offset or None = zeros, length) extents,
/// or inline bytes (UDF embedded data).
#[derive(Clone, Debug)]
struct Entry {
    name: String,
    is_dir: bool,
    size: u64,
    extents: Vec<(Option<u64>, u64)>,
    inline: Option<Vec<u8>>,
}

struct Image {
    f: File,
    size: u64,
}

impl Image {
    fn pread(&mut self, off: u64, n: usize) -> Vec<u8> {
        if off >= self.size {
            return Vec::new();
        }
        let n = n.min((self.size - off) as usize);
        let mut buf = vec![0u8; n];
        if self.f.seek(SeekFrom::Start(off)).is_err() || self.f.read_exact(&mut buf).is_err() {
            return Vec::new();
        }
        buf
    }

    fn sector(&mut self, lba: u64) -> Vec<u8> {
        self.pread(lba * SECTOR, SECTOR as usize)
    }

    /// N bytes at OFF of an entry's data (fragmented extents and sparse runs handled).
    fn read_entry(&mut self, e: &Entry, off: u64, n: u64) -> Vec<u8> {
        if off >= e.size || n == 0 {
            return Vec::new();
        }
        let n = n.min(e.size - off);
        if let Some(b) = &e.inline {
            let end = ((off + n) as usize).min(b.len());
            return b.get(off as usize..end).unwrap_or(&[]).to_vec();
        }
        let mut out = Vec::with_capacity(n as usize);
        let (mut pos, mut off, mut remaining) = (0u64, off, n);
        for &(eoff, elen) in &e.extents {
            if remaining == 0 {
                break;
            }
            if off < pos + elen {
                let start = off.saturating_sub(pos);
                let take = (elen - start).min(remaining);
                match eoff {
                    None => out.resize(out.len() + take as usize, 0),
                    Some(o) => {
                        let mut d = self.pread(o + start, take as usize);
                        d.resize(take as usize, 0);
                        out.extend_from_slice(&d);
                    }
                }
                remaining -= take;
                off += take;
            }
            pos += elen;
        }
        out.resize(n as usize, 0);
        out
    }
}

fn u16le(b: &[u8], o: usize) -> u16 {
    b.get(o..o + 2).map(|x| u16::from_le_bytes([x[0], x[1]])).unwrap_or(0)
}
fn u32le(b: &[u8], o: usize) -> u32 {
    b.get(o..o + 4).map(|x| u32::from_le_bytes([x[0], x[1], x[2], x[3]])).unwrap_or(0)
}
fn u64le(b: &[u8], o: usize) -> u64 {
    b.get(o..o + 8).map(|x| u64::from_le_bytes(x.try_into().unwrap())).unwrap_or(0)
}

// ---------------------------------------------------------------- UDF

fn tag_id(b: &[u8]) -> u16 {
    u16le(b, 0)
}

/// A valid descriptor tag (checksum over bytes 0-3 and 5-15), optionally of type WANT.
fn tag_ok(b: &[u8], want: Option<u16>) -> bool {
    if b.len() < 16 || want.is_some_and(|w| tag_id(b) != w) {
        return false;
    }
    let sum = b[0..4].iter().chain(&b[5..16]).fold(0u8, |a, x| a.wrapping_add(*x));
    sum == b[4]
}

/// OSTA compressed unicode (compression id 8 = 8-bit, 16 = UTF-16BE).
fn dstring_osta(b: &[u8]) -> String {
    match b.first() {
        None => String::new(),
        Some(8) => b[1..].iter().map(|&c| c as char).collect(),
        Some(16) => {
            let u: Vec<u16> = b[1..].chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
            String::from_utf16_lossy(&u)
        }
        Some(254) | Some(255) => String::new(),
        Some(_) => b[1..].iter().map(|&c| c as char).collect(),
    }
}

#[derive(Clone, Debug)]
enum PartMap {
    /// type 1: a physical partition number
    Physical(u16),
    /// type 2 "*UDF Metadata Partition": the metadata file's extents (absolute bytes)
    Metadata {
        pnum: u16,
        file: u32,
        mirror: u32,
        extents: Vec<(Option<u64>, u64)>,
    },
    Unsupported,
}

struct Udf {
    pds: std::collections::HashMap<u16, u32>,
    maps: Vec<PartMap>,
    /// root directory ICB (partition ref, logical block)
    root: (u16, u32),
}

impl Udf {
    fn open(img: &mut Image) -> Result<Udf> {
        let last = img.size / SECTOR;
        let avdp = [256, last.saturating_sub(1), last.saturating_sub(257)]
            .into_iter()
            .filter(|&l| l > 0)
            .map(|l| img.sector(l))
            .find(|b| tag_ok(b, Some(2)))
            .ok_or_else(|| anyhow!("no UDF anchor"))?;
        let (main_len, main_loc, res_len, res_loc) = (u32le(&avdp, 16), u32le(&avdp, 20), u32le(&avdp, 24), u32le(&avdp, 28));
        let mut u = Udf { pds: Default::default(), maps: Vec::new(), root: (0, 0) };
        let mut lvd = u.read_vds(img, main_loc, main_len);
        if lvd.is_none() || u.pds.is_empty() {
            lvd = u.read_vds(img, res_loc, res_len);
        }
        let lvd = match lvd {
            Some(l) if !u.pds.is_empty() => l,
            _ => bail!("UDF volume descriptor sequence incomplete"),
        };
        let bs = u32le(&lvd, 212);
        if bs != 0 && bs as u64 != SECTOR {
            bail!("unsupported UDF block size {}", bs);
        }
        let (fsd_pref, fsd_lbn) = (u16le(&lvd, 248 + 8), u32le(&lvd, 248 + 4));
        let n_maps = u32le(&lvd, 268);
        let mut p = 440usize;
        for _ in 0..n_maps {
            let (mtype, mlen) = (lvd.get(p).copied().unwrap_or(0), lvd.get(p + 1).copied().unwrap_or(0) as usize);
            let m = match mtype {
                1 => PartMap::Physical(u16le(&lvd, p + 4)),
                2 => {
                    let ident = lvd.get(p + 5..p + 28).unwrap_or(&[]);
                    if ident.starts_with(b"*UDF Metadata Partition") {
                        PartMap::Metadata {
                            pnum: u16le(&lvd, p + 38),
                            file: u32le(&lvd, p + 40),
                            mirror: u32le(&lvd, p + 44),
                            extents: Vec::new(),
                        }
                    } else {
                        PartMap::Unsupported
                    }
                }
                _ => PartMap::Unsupported,
            };
            u.maps.push(m);
            p += if mlen > 0 { mlen } else { 6 };
        }
        // resolve the metadata partitions: read the metadata file (or its mirror)
        for i in 0..u.maps.len() {
            if let PartMap::Metadata { pnum, file, mirror, .. } = u.maps[i].clone() {
                let phys = u.phys_ref(pnum)?;
                let e = [file, mirror]
                    .into_iter()
                    .find_map(|loc| u.read_icb(img, phys, loc, "<metadata>").ok())
                    .ok_or_else(|| anyhow!("cannot read the UDF metadata file"))?;
                if let PartMap::Metadata { extents, .. } = &mut u.maps[i] {
                    *extents = e.extents;
                }
            }
        }
        let fsd = u.read_block(img, fsd_pref, fsd_lbn)?;
        if tag_id(&fsd) != 256 {
            bail!("no UDF file set descriptor");
        }
        u.root = (u16le(&fsd, 400 + 8), u32le(&fsd, 400 + 4));
        Ok(u)
    }

    /// Walk a volume descriptor sequence; returns the logical volume descriptor.
    fn read_vds(&mut self, img: &mut Image, loc: u32, len: u32) -> Option<Vec<u8>> {
        let mut lba = loc as u64;
        let mut end = lba + (len as u64 / SECTOR).max(1);
        let mut lvd = None;
        let mut seen = 0;
        while lba < end && seen < 256 {
            seen += 1;
            let b = img.sector(lba);
            if !tag_ok(&b, None) {
                break;
            }
            match tag_id(&b) {
                5 => {
                    self.pds.insert(u16le(&b, 22), u32le(&b, 188));
                }
                6 => lvd = Some(b),
                3 => {
                    let (nlen, nloc) = (u32le(&b, 20), u32le(&b, 24));
                    lba = nloc as u64;
                    end = lba + (nlen as u64 / SECTOR).max(1);
                    continue;
                }
                8 => break,
                _ => {}
            }
            lba += 1;
        }
        lvd
    }

    fn phys_ref(&self, pnum: u16) -> Result<u16> {
        self.maps
            .iter()
            .position(|m| matches!(m, PartMap::Physical(p) if *p == pnum))
            .map(|i| i as u16)
            .ok_or_else(|| anyhow!("no type-1 map for UDF partition {}", pnum))
    }

    /// A logical extent -> absolute (byte offset or None, length) pieces.
    fn map_extent(&self, pref: u16, lbn: u32, len: u64) -> Result<Vec<(Option<u64>, u64)>> {
        match self.maps.get(pref as usize) {
            Some(PartMap::Physical(p)) => {
                let start = *self.pds.get(p).ok_or_else(|| anyhow!("missing UDF partition descriptor {}", p))?;
                Ok(vec![(Some((start as u64 + lbn as u64) * SECTOR), len)])
            }
            Some(PartMap::Metadata { extents, .. }) => {
                let (mut want, mut pos, mut remaining) = (lbn as u64 * SECTOR, 0u64, len);
                let mut out = Vec::new();
                for &(eoff, elen) in extents {
                    if remaining == 0 {
                        break;
                    }
                    if want < pos + elen {
                        let s = want.saturating_sub(pos);
                        let take = (elen - s).min(remaining);
                        out.push((eoff.map(|o| o + s), take));
                        remaining -= take;
                        want += take;
                    }
                    pos += elen;
                }
                if remaining > 0 {
                    bail!("UDF metadata extent out of range");
                }
                Ok(out)
            }
            _ => bail!("unsupported UDF partition reference {}", pref),
        }
    }

    fn read_block(&self, img: &mut Image, pref: u16, lbn: u32) -> Result<Vec<u8>> {
        Ok(match self.map_extent(pref, lbn, SECTOR)?.first() {
            Some((Some(off), _)) => img.pread(*off, SECTOR as usize),
            _ => vec![0; SECTOR as usize],
        })
    }

    /// A file entry (tag 261) or extended file entry (tag 266).
    fn read_icb(&self, img: &mut Image, pref: u16, lbn: u32, name: &str) -> Result<Entry> {
        let b = self.read_block(img, pref, lbn)?;
        let tid = tag_id(&b);
        if !tag_ok(&b, None) || (tid != 261 && tid != 266) {
            bail!("bad UDF file entry at {}:{} (tag {})", pref, lbn, tid);
        }
        let is_dir = b[27] == 4;
        let adtype = u16le(&b, 34) & 7;
        let size = u64le(&b, 56);
        let (l_ea, l_ad, base) = if tid == 261 { (u32le(&b, 168), u32le(&b, 172), 176) } else { (u32le(&b, 208), u32le(&b, 212), 216) };
        let a0 = (base + l_ea as usize).min(b.len());
        let ad = b[a0..(a0 + l_ad as usize).min(b.len())].to_vec();
        let mut e = Entry { name: name.into(), is_dir, size, extents: Vec::new(), inline: None };
        if adtype == 3 {
            e.inline = Some(ad[..(size as usize).min(ad.len())].to_vec());
        } else {
            e.extents = self.parse_ads(img, ad, adtype, pref)?;
        }
        Ok(e)
    }

    /// Short (0) or long (1) allocation descriptors, following continuation extents.
    fn parse_ads(&self, img: &mut Image, mut ad: Vec<u8>, adtype: u16, mut pref: u16) -> Result<Vec<(Option<u64>, u64)>> {
        if adtype > 1 {
            bail!("extended UDF allocation descriptors unsupported");
        }
        let step = if adtype == 0 { 8 } else { 16 };
        let mut exts = Vec::new();
        for _ in 0..100_000 {
            let mut next = None;
            let mut p = 0;
            while p + step <= ad.len() {
                let (raw, pos) = (u32le(&ad, p), u32le(&ad, p + 4));
                let apref = if adtype == 0 { pref } else { u16le(&ad, p + 8) };
                p += step;
                let (etype, elen) = (raw >> 30, (raw & 0x3FFF_FFFF) as u64);
                if elen == 0 {
                    break;
                }
                match etype {
                    3 => {
                        next = Some((apref, pos));
                        break;
                    }
                    0 => exts.extend(self.map_extent(apref, pos, elen)?),
                    _ => exts.push((None, elen)),
                }
            }
            let Some((np, nlbn)) = next else { return Ok(exts) };
            let blk = self.read_block(img, np, nlbn)?;
            if tag_id(&blk) != 258 {
                bail!("bad UDF allocation extent descriptor");
            }
            let l = u32le(&blk, 20) as usize;
            ad = blk[24..(24 + l).min(blk.len())].to_vec();
            pref = np;
        }
        bail!("UDF allocation descriptor loop")
    }

    fn root(&self, img: &mut Image) -> Result<Entry> {
        self.read_icb(img, self.root.0, self.root.1, "")
    }

    fn listdir(&self, img: &mut Image, d: &Entry) -> Vec<Entry> {
        let data = img.read_entry(d, 0, d.size);
        let mut out = Vec::new();
        let mut p = 0;
        while p + 38 <= data.len() {
            if tag_id(&data[p..]) != 257 {
                p += 4;
                continue;
            }
            let chars = data[p + 18];
            let l_fi = data[p + 19] as usize;
            let (lbn, pref) = (u32le(&data, p + 24), u16le(&data, p + 28));
            let l_iu = u16le(&data, p + 36) as usize;
            let n0 = (p + 38 + l_iu).min(data.len());
            let name_b = &data[n0..(n0 + l_fi).min(data.len())];
            p += (38 + l_iu + l_fi + 3) & !3;
            if chars & 0x0C != 0 {
                continue; // parent or deleted
            }
            let name = dstring_osta(name_b);
            if name.is_empty() {
                continue;
            }
            if let Ok(mut e) = self.read_icb(img, pref, lbn, &name) {
                if chars & 0x02 != 0 {
                    e.is_dir = true;
                }
                out.push(e);
            }
        }
        out
    }
}

// ---------------------------------------------------------------- ISO9660

struct Iso9660 {
    joliet: bool,
    root: Entry,
}

impl Iso9660 {
    fn open(img: &mut Image) -> Result<Iso9660> {
        let (mut pvd, mut joliet) = (None, None);
        for lba in 16..16 + 64 {
            let b = img.sector(lba);
            if b.len() < SECTOR as usize || &b[1..6] != b"CD001" {
                break;
            }
            match b[0] {
                1 if pvd.is_none() => pvd = Some(b),
                2 if [b"%/@", b"%/C", b"%/E"].iter().any(|e| &b[88..91] == *e) => joliet = Some(b),
                255 => break,
                _ => {}
            }
        }
        let is_joliet = joliet.is_some();
        let vd = joliet.or(pvd).ok_or_else(|| anyhow!("no ISO9660 volume descriptor"))?;
        let (ext, size) = (u32le(&vd, 156 + 2) as u64, u32le(&vd, 156 + 10) as u64);
        Ok(Iso9660 {
            joliet: is_joliet,
            root: Entry { name: String::new(), is_dir: true, size, extents: vec![(Some(ext * SECTOR), size)], inline: None },
        })
    }

    fn listdir(&self, img: &mut Image, d: &Entry) -> Vec<Entry> {
        let data = img.read_entry(d, 0, d.size);
        let mut out: Vec<Entry> = Vec::new();
        let mut multi = false;
        let mut p = 0;
        while p < data.len() {
            let ln = data[p] as usize;
            if ln == 0 {
                p = (p / SECTOR as usize + 1) * SECTOR as usize;
                continue;
            }
            let rec = &data[p..(p + ln).min(data.len())];
            p += ln;
            if rec.len() < 34 {
                continue;
            }
            let (ext, size, flags, nlen) = (u32le(rec, 2) as u64, u32le(rec, 10) as u64, rec[25], rec[32] as usize);
            let raw = &rec[33..(33 + nlen).min(rec.len())];
            if raw == [0] || raw == [1] {
                continue;
            }
            let mut name = if self.joliet {
                let u: Vec<u16> = raw.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
                String::from_utf16_lossy(&u)
            } else {
                raw.iter().map(|&c| c as char).collect()
            };
            if let Some(i) = name.find(';') {
                name.truncate(i);
            }
            if !self.joliet && name.ends_with('.') {
                name.pop();
            }
            // a file of several extents: consecutive records with the same name
            if multi {
                if let Some(last) = out.last_mut().filter(|l| l.name == name) {
                    last.extents.push((Some(ext * SECTOR), size));
                    last.size += size;
                    multi = flags & 0x80 != 0;
                    continue;
                }
            }
            multi = flags & 0x80 != 0;
            out.push(Entry { name, is_dir: flags & 0x02 != 0, size, extents: vec![(Some(ext * SECTOR), size)], inline: None });
        }
        out
    }
}

// ---------------------------------------------------------------- media

enum Fs {
    Udf(Udf),
    Iso(Iso9660),
}

/// A Windows ISO opened for reading.
pub struct Media {
    img: Image,
    fs: Fs,
}

impl Media {
    pub fn open(path: &Path) -> Result<Media> {
        let f = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
        let size = f.metadata()?.len();
        let mut img = Image { f, size };
        let fs = match Udf::open(&mut img) {
            Ok(u) => Fs::Udf(u),
            Err(_) => Fs::Iso(Iso9660::open(&mut img).map_err(|_| anyhow!("{}: neither UDF nor ISO9660 readable", path.display()))?),
        };
        Ok(Media { img, fs })
    }

    fn listdir(&mut self, d: &Entry) -> Vec<Entry> {
        match &self.fs {
            Fs::Udf(u) => u.listdir(&mut self.img, d),
            Fs::Iso(i) => i.listdir(&mut self.img, d),
        }
    }

    /// A path like `sources/install.wim` (case-insensitive).
    fn lookup(&mut self, path: &str) -> Option<Entry> {
        let mut cur = match &self.fs {
            Fs::Udf(u) => u.root(&mut self.img).ok()?,
            Fs::Iso(i) => i.root.clone(),
        };
        for part in path.split(['/', '\\']).filter(|p| !p.is_empty()) {
            if !cur.is_dir {
                return None;
            }
            cur = self.listdir(&cur).into_iter().find(|e| e.name.eq_ignore_ascii_case(part))?;
        }
        Some(cur)
    }

    /// The XML image list from the WIM header (error [`NoWim`] if there is no install image).
    pub fn wim_xml(&mut self) -> Result<String> {
        let e = ["sources/install.wim", "sources/install.esd", "sources/install.swm"]
            .iter()
            .find_map(|p| self.lookup(p).filter(|e| !e.is_dir))
            .ok_or(NoWim)?;
        let hdr = self.img.read_entry(&e, 0, 208);
        if !hdr.starts_with(b"MSWIM\0\0\0") {
            bail!("the install image has no MSWIM header");
        }
        let size = u64le(&hdr, 0x48) & 0x00FF_FFFF_FFFF_FFFF;
        let (off, orig) = (u64le(&hdr, 0x50), u64le(&hdr, 0x58));
        let n = if orig > 0 && orig < 64 << 20 { orig } else { size };
        if n == 0 || n > 64 << 20 {
            bail!("implausible WIM XML size {}", n);
        }
        let data = self.img.read_entry(&e, off, n);
        let txt = if data.starts_with(&[0xFF, 0xFE]) {
            let u: Vec<u16> = data[2..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            String::from_utf16_lossy(&u)
        } else if data.starts_with(&[0xFE, 0xFF]) {
            let u: Vec<u16> = data[2..].chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
            String::from_utf16_lossy(&u)
        } else {
            let u: Vec<u16> = data.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            String::from_utf16_lossy(&u)
        };
        Ok(txt.trim_end_matches('\0').trim().to_string())
    }

    /// Every image in the WIM, by index.
    pub fn images(&mut self) -> Result<Vec<WimImage>> {
        Ok(parse_wim_xml(&self.wim_xml()?))
    }
}

/// The text of the first `<TAG>...</TAG>` in S (the WIM XML is flat and machine-written:
/// no attributes on these tags, no CDATA).
fn tag<'a>(s: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{}>", name);
    let close = format!("</{}>", name);
    let a = s.find(&open)? + open.len();
    let b = a + s[a..].find(&close)?;
    Some(s[a..b].trim())
}

fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

/// The `<IMAGE INDEX="n">` blocks of the WIM XML.
pub fn parse_wim_xml(xml: &str) -> Vec<WimImage> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(i) = rest.find("<IMAGE ") {
        let block_start = &rest[i..];
        let end = block_start.find("</IMAGE>").map(|e| e + 8).unwrap_or(block_start.len());
        let block = &block_start[..end];
        rest = &block_start[end..];
        let index = block.split_once("INDEX=\"").and_then(|(_, r)| r.split('"').next()).and_then(|n| n.parse().ok()).unwrap_or(0);
        let windows = tag(block, "WINDOWS").unwrap_or("");
        let version = tag(windows, "VERSION").unwrap_or("");
        let num = |s: Option<&str>| s.and_then(|v| v.parse::<u32>().ok());
        out.push(WimImage {
            index,
            name: xml_unescape(tag(block, "NAME").or_else(|| tag(block, "DISPLAYNAME")).unwrap_or("")),
            edition_id: xml_unescape(tag(windows, "EDITIONID").or_else(|| tag(block, "FLAGS")).unwrap_or("")),
            installation_type: xml_unescape(tag(windows, "INSTALLATIONTYPE").unwrap_or("")),
            build: num(tag(version, "BUILD")),
            arch: num(tag(windows, "ARCH")),
        });
    }
    out.sort_by_key(|i| i.index);
    out
}

/// The image index for EDITION / INST_TYPE: an exact EditionID match first, then
/// EDITION + "Eval" (evaluation media say e.g. ServerStandardEval). The installation type
/// must match exactly ("Server" = Desktop Experience, "Server Core" = core).
pub fn pick(imgs: &[WimImage], edition: &str, inst_type: &str) -> Option<u32> {
    let want_t = inst_type.to_ascii_lowercase();
    for cand in [edition.to_ascii_lowercase(), format!("{}eval", edition.to_ascii_lowercase())] {
        if let Some(i) =
            imgs.iter().find(|i| i.edition_id.to_ascii_lowercase() == cand && i.installation_type.to_ascii_lowercase() == want_t)
        {
            return Some(i.index);
        }
    }
    None
}

/// (build, arch name) of image 1 of an ISO, e.g. (26300, "arm64").
pub fn iso_build_arch(path: &Path) -> Result<(u32, String)> {
    let imgs = Media::open(path)?.images()?;
    let first = imgs.first().ok_or_else(|| anyhow!("{}: the WIM lists no images", path.display()))?;
    Ok((first.build.ok_or_else(|| anyhow!("{}: no build number in the WIM", path.display()))?, first.arch_name()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const XML: &str = r#"<WIM><TOTALBYTES>1</TOTALBYTES>
<IMAGE INDEX="2"><NAME>Windows Server 2025 Standard Evaluation (Desktop Experience)</NAME>
 <WINDOWS><ARCH>9</ARCH><EDITIONID>ServerStandardEval</EDITIONID><INSTALLATIONTYPE>Server</INSTALLATIONTYPE>
 <VERSION><MAJOR>10</MAJOR><BUILD>26100</BUILD><SPBUILD>32230</SPBUILD></VERSION></WINDOWS></IMAGE>
<IMAGE INDEX="1"><NAME>Windows Server 2025 Standard Evaluation</NAME>
 <WINDOWS><ARCH>9</ARCH><EDITIONID>ServerStandardEval</EDITIONID><INSTALLATIONTYPE>Server Core</INSTALLATIONTYPE>
 <VERSION><MAJOR>10</MAJOR><BUILD>26100</BUILD></VERSION></WINDOWS></IMAGE>
<IMAGE INDEX="3"><NAME>Windows 11 Pro &amp; more</NAME>
 <WINDOWS><ARCH>12</ARCH><EDITIONID>Professional</EDITIONID><INSTALLATIONTYPE>Client</INSTALLATIONTYPE>
 <VERSION><BUILD>26300</BUILD></VERSION></WINDOWS></IMAGE></WIM>"#;

    #[test]
    fn wim_xml_and_pick() {
        let imgs = parse_wim_xml(XML);
        assert_eq!(imgs.iter().map(|i| i.index).collect::<Vec<_>>(), [1, 2, 3]);
        assert_eq!(imgs[1].installation_type, "Server");
        assert_eq!((imgs[1].build, imgs[1].arch_name().as_str()), (Some(26100), "x64"));
        assert_eq!(imgs[2].name, "Windows 11 Pro & more");
        assert_eq!(imgs[2].arch_name(), "arm64");
        assert_eq!(pick(&imgs, "ServerStandard", "Server"), Some(2));
        assert_eq!(pick(&imgs, "ServerStandard", "Server Core"), Some(1));
        assert_eq!(pick(&imgs, "Professional", "Client"), Some(3));
        assert_eq!(pick(&imgs, "ServerDatacenter", "Server"), None);
    }

    /// The ISO9660/Joliet path: a disc written by our own mkiso with a fake WIM in it.
    #[test]
    fn reads_a_joliet_disc() {
        let dir = std::env::temp_dir().join(format!("wtl-winiso-{}", std::process::id()));
        let src = dir.join("src");
        std::fs::create_dir_all(src.join("sources")).unwrap();
        let xml: Vec<u8> = std::iter::once(0xFEFFu16).chain(XML.encode_utf16()).flat_map(|u| u.to_le_bytes()).collect();
        let mut wim = vec![0u8; 208];
        wim[..8].copy_from_slice(b"MSWIM\0\0\0");
        wim[0x48..0x50].copy_from_slice(&(xml.len() as u64).to_le_bytes());
        wim[0x50..0x58].copy_from_slice(&208u64.to_le_bytes());
        wim[0x58..0x60].copy_from_slice(&(xml.len() as u64).to_le_bytes());
        wim.extend_from_slice(&xml);
        std::fs::write(src.join("sources/install.wim"), &wim).unwrap();
        let iso = dir.join("t.iso");
        crate::mkiso::make_iso(&src, &iso, "TEST").unwrap();
        let mut m = Media::open(&iso).unwrap();
        let imgs = m.images().unwrap();
        assert_eq!(imgs.len(), 3);
        assert_eq!(iso_build_arch(&iso).unwrap(), (26100, "x64".to_string()));
        std::fs::remove_file(src.join("sources/install.wim")).unwrap();
        std::fs::write(src.join("readme.txt"), "x").unwrap();
        crate::mkiso::make_iso(&src, &iso, "TEST").unwrap();
        assert!(Media::open(&iso).unwrap().images().unwrap_err().downcast_ref::<NoWim>().is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The real UDF media in the checkout's iso-files/ (skipped when there are none): a
    /// plausible build and CPU.
    #[test]
    fn real_isos_if_present() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("iso-files");
        let Ok(rd) = std::fs::read_dir(&dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("iso")) {
                if let Ok((b, a)) = iso_build_arch(&p) {
                    assert!(b > 9000, "{}: build {}", p.display(), b);
                    assert!(a == "x64" || a == "arm64", "{}: arch {}", p.display(), a);
                }
            }
        }
    }
}
