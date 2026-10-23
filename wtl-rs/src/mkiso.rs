//! ISO9660 + Joliet image writer (port of lib/mkiso.py; same layout, byte for byte apart
//! from timestamps).
//!
//! * ISO9660 primary tree: level-2 style names (uppercase d-characters, max 30 chars for
//!   NAME.EXT plus ";1" version suffix for files, 31 chars for directories).
//! * Joliet supplementary tree (UCS-2 BE, escape "%/E" = level 3), original case, names
//!   up to 64 characters.
//! * L- and M-type path tables for both trees, both-endian fields, volume descriptor set
//!   terminator.
//! * Files and directories are recursively included (symlinks are followed). Single-extent
//!   files: anything >= 4 GiB is rejected with an error.

use crate::util::py_slice_to;
use anyhow::{anyhow, bail, ensure, Context, Result};
use chrono::{DateTime, Datelike, Timelike, Utc};
use std::cmp::Ordering;
use std::fs::File;
use std::io::{BufWriter, Read, Seek, Write};
use std::path::{Path, PathBuf};

const SECTOR: u64 = 2048;
const MAX_EXTENT: u64 = 0xFFFF_FFFF; // bytes in a single extent (32-bit length)
const ISO: usize = 0;
const JOL: usize = 1;

fn both16(v: u16) -> [u8; 4] {
    let (l, b) = (v.to_le_bytes(), v.to_be_bytes());
    [l[0], l[1], b[0], b[1]]
}

fn both32(v: u32) -> [u8; 8] {
    let (l, b) = (v.to_le_bytes(), v.to_be_bytes());
    [l[0], l[1], l[2], l[3], b[0], b[1], b[2], b[3]]
}

/// ASCII text, cut to n bytes, padded with spaces ('?' for anything non-ASCII).
fn pad_a(s: &str, n: usize) -> Vec<u8> {
    let mut b: Vec<u8> = s.chars().map(|c| if c.is_ascii() { c as u8 } else { b'?' }).take(n).collect();
    b.resize(n, b' ');
    b
}

fn utf16be(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(|u| u.to_be_bytes()).collect()
}

/// UCS-2 BE string padded with UCS-2 spaces, n bytes total (n even or odd).
fn pad_j(s: &str, n: usize) -> Vec<u8> {
    let mut b = utf16be(s);
    b.truncate(n - n % 2);
    while b.len() + 2 <= n {
        b.extend_from_slice(&[0, b' ']);
    }
    b.resize(n, 0);
    b
}

fn utc(t: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(t, 0).unwrap_or_default()
}

/// 17-byte volume descriptor date: "YYYYMMDDHHMMSS00" + GMT offset 0 (UTC)
fn dt17(t: i64) -> Vec<u8> {
    let mut v = utc(t).format("%Y%m%d%H%M%S00").to_string().into_bytes();
    v.push(0);
    v
}

const DT17_ZERO: &[u8; 17] = b"0000000000000000\0";

/// 7-byte directory record date (years since 1900, UTC)
fn dt7(t: i64) -> [u8; 7] {
    let d = utc(t);
    [(d.year() - 1900).clamp(0, 255) as u8, d.month() as u8, d.day() as u8, d.hour() as u8, d.minute() as u8, d.second() as u8, 0]
}

fn is_dchar(c: char) -> bool {
    c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'
}

/// Upper-case, then anything that is not a d-character becomes '_'.
fn dchars(s: &str) -> String {
    s.to_uppercase().chars().map(|c| if is_dchar(c) { c } else { '_' }).collect()
}

#[derive(Default, Clone, Copy)]
struct DirInfo {
    lba: u32,
    size: u32,
    num: u16,
}

struct Node {
    name: String, // original name
    path: PathBuf,
    is_dir: bool,
    parent: Option<usize>,
    children: Vec<usize>,
    size: u64,
    mtime: i64,
    lba: u32, // file data LBA (files)
    iso_name: Vec<u8>,
    jol_name: Vec<u8>,
    d: [DirInfo; 2], // per tree directory info
}

fn mtime_of(md: &std::fs::Metadata) -> i64 {
    match md.modified() {
        Ok(t) => match t.duration_since(std::time::UNIX_EPOCH) {
            Ok(d) => d.as_secs() as i64,
            Err(e) => -(e.duration().as_secs_f64().ceil() as i64),
        },
        Err(_) => 0,
    }
}

struct Tree {
    nodes: Vec<Node>,
}

impl Tree {
    fn add(&mut self, name: String, path: PathBuf, is_dir: bool, parent: Option<usize>) -> Result<usize> {
        let md = std::fs::metadata(&path).with_context(|| format!("mkiso: cannot stat {}", path.display()))?;
        let id = self.nodes.len();
        self.nodes.push(Node {
            name,
            path,
            is_dir,
            parent,
            children: Vec::new(),
            size: if is_dir { 0 } else { md.len() },
            mtime: mtime_of(&md),
            lba: 0,
            iso_name: Vec::new(),
            jol_name: Vec::new(),
            d: [DirInfo::default(); 2],
        });
        Ok(id)
    }

    fn build(src: &Path) -> Result<Tree> {
        let mut t = Tree { nodes: Vec::new() };
        t.add(String::new(), src.to_path_buf(), true, None)?;
        let mut stack = vec![0usize];
        while let Some(d) = stack.pop() {
            let dpath = t.nodes[d].path.clone();
            let list_err = || format!("mkiso: cannot list {}", dpath.display());
            let rd = std::fs::read_dir(&dpath).with_context(list_err)?;
            let mut names: Vec<String> = Vec::new();
            for e in rd {
                let e = e.with_context(list_err)?;
                names.push(e.file_name().to_string_lossy().into_owned());
            }
            names.sort();
            for n in names {
                let p = dpath.join(&n);
                let c = match std::fs::metadata(&p) {
                    Ok(m) if m.is_dir() => {
                        let c = t.add(n, p, true, Some(d))?;
                        stack.push(c);
                        c
                    }
                    Ok(m) if m.is_file() => {
                        let c = t.add(n, p.clone(), false, Some(d))?;
                        if t.nodes[c].size > MAX_EXTENT {
                            bail!("mkiso: {} is {} bytes; files >= 4 GiB are not supported", p.display(), t.nodes[c].size);
                        }
                        c
                    }
                    _ => {
                        eprintln!("mkiso: skipping special file {}", p.display());
                        continue;
                    }
                };
                t.nodes[d].children.push(c);
            }
        }
        Ok(t)
    }

    fn assign_names(&mut self, d: usize) {
        let mut used_iso: Vec<String> = Vec::new();
        let mut used_jol: Vec<String> = Vec::new();
        for c in self.nodes[d].children.clone() {
            let (name, is_dir) = (self.nodes[c].name.clone(), self.nodes[c].is_dir);
            // ---- ISO9660 level 2
            if is_dir {
                let mut base = py_slice_to(&dchars(&name), 31);
                if base.is_empty() {
                    base = "_".into();
                }
                let mut cand = base.clone();
                let mut i = 1;
                while used_iso.contains(&cand) {
                    let suf = format!("~{}", i);
                    cand = py_slice_to(&base, 31 - suf.len() as isize) + &suf;
                    i += 1;
                }
                self.nodes[c].iso_name = cand.clone().into_bytes();
                used_iso.push(cand);
            } else {
                let (stem, ext) = match name.trim_start_matches('.').contains('.') {
                    true => {
                        let i = name.rfind('.').unwrap_or(name.len());
                        (&name[..i], &name[i + 1..])
                    }
                    false => (name.as_str(), ""),
                };
                let mut stem = dchars(stem);
                if stem.is_empty() {
                    stem = "_".into();
                }
                let ext = py_slice_to(&dchars(ext), 8);
                let maxstem = 30 - 1 - ext.len() as isize;
                let mut cand = format!("{}.{}", py_slice_to(&stem, maxstem), ext);
                let mut i = 1;
                while used_iso.contains(&cand) {
                    let suf = format!("~{}", i);
                    cand = format!("{}{}.{}", py_slice_to(&stem, maxstem - suf.len() as isize), suf, ext);
                    i += 1;
                }
                self.nodes[c].iso_name = format!("{};1", cand).into_bytes();
                used_iso.push(cand);
            }
            // ---- Joliet (64 UCS-2 chars; disallowed chars replaced)
            let jn: String =
                name.chars().map(|ch| if (ch as u32) < 32 || "*/:;?\\".contains(ch) || (ch as u32) > 0xFFFF { '_' } else { ch }).collect();
            let base = py_slice_to(&jn, 64);
            let mut cand = base.clone();
            let mut i = 1;
            while used_jol.contains(&cand.to_lowercase()) {
                let suf = format!("~{}", i);
                let sl = suf.chars().count() as isize;
                cand = match (base.rfind('.'), is_dir) {
                    (Some(p), false) => {
                        let (st, ex) = (&base[..p], &base[p + 1..]);
                        let keep = 64 - sl - ex.chars().count() as isize - 1;
                        py_slice_to(&format!("{}{}.{}", py_slice_to(st, keep), suf, ex), 64)
                    }
                    _ => py_slice_to(&base, 64 - sl) + &suf,
                };
                i += 1;
            }
            used_jol.push(cand.to_lowercase());
            self.nodes[c].jol_name = utf16be(&cand);
        }
        for c in self.nodes[d].children.clone() {
            if self.nodes[c].is_dir {
                self.assign_names(c);
            }
        }
    }

    fn name(&self, n: usize, tree: usize) -> &[u8] {
        if tree == ISO {
            &self.nodes[n].iso_name
        } else {
            &self.nodes[n].jol_name
        }
    }

    /// ISO9660 9.3: sort by name then extension, padded with spaces (0x20 < any d-char)
    fn iso_cmp(&self, x: usize, y: usize) -> Ordering {
        let key = |n: usize| {
            let node = &self.nodes[n];
            let s = node.iso_name.split(|b| *b == b';').next().unwrap_or(&[]);
            let (a, b) = match (node.is_dir, s.iter().position(|b| *b == b'.')) {
                (false, Some(i)) => (&s[..i], &s[i + 1..]),
                _ => (s, &[][..]),
            };
            let mut a = a.to_vec();
            let mut b = b.to_vec();
            if a.len() < 64 {
                a.resize(64, b' ');
            }
            if b.len() < 8 {
                b.resize(8, b' ');
            }
            (a, b)
        };
        key(x).cmp(&key(y))
    }

    fn sorted_children(&self, d: usize, tree: usize) -> Vec<usize> {
        let mut v = self.nodes[d].children.clone();
        if tree == ISO {
            v.sort_by(|a, b| self.iso_cmp(*a, *b));
        } else {
            v.sort_by(|a, b| self.nodes[*a].jol_name.cmp(&self.nodes[*b].jol_name));
        }
        v
    }

    fn dir_records_size(&self, d: usize, tree: usize) -> u64 {
        let mut size = 0u64;
        let mut lens = vec![1usize, 1];
        lens.extend(self.sorted_children(d, tree).iter().map(|c| self.name(*c, tree).len()));
        for nl in lens {
            let ln = rec_len(nl) as u64;
            if size % SECTOR + ln > SECTOR {
                size = (size / SECTOR + 1) * SECTOR;
            }
            size += ln;
        }
        sectors(size) * SECTOR
    }

    /// Directories in path-table order: by level, then parent number, then name.
    fn all_dirs_bfs(&mut self, tree: usize) -> Vec<usize> {
        let mut out = vec![0usize];
        let mut i = 0;
        while i < out.len() {
            for c in self.sorted_children(out[i], tree) {
                if self.nodes[c].is_dir {
                    out.push(c);
                }
            }
            i += 1;
        }
        for (num, d) in out.iter().enumerate() {
            self.nodes[*d].d[tree].num = (num + 1) as u16;
        }
        out
    }

    fn dir_data(&self, d: usize, tree: usize) -> Vec<u8> {
        let node = &self.nodes[d];
        let me = node.d[tree];
        let par_node = node.parent.unwrap_or(d);
        let par = self.nodes[par_node].d[tree];
        let mut recs = vec![
            make_record(&[0], me.lba, me.size, true, node.mtime),
            make_record(&[1], par.lba, par.size, true, self.nodes[par_node].mtime),
        ];
        for c in self.sorted_children(d, tree) {
            let cn = &self.nodes[c];
            if cn.is_dir {
                recs.push(make_record(self.name(c, tree), cn.d[tree].lba, cn.d[tree].size, true, cn.mtime));
            } else {
                let lba = if cn.size > 0 { cn.lba } else { 0 };
                recs.push(make_record(self.name(c, tree), lba, cn.size as u32, false, cn.mtime));
            }
        }
        let mut buf: Vec<u8> = Vec::with_capacity(me.size as usize);
        for r in recs {
            if buf.len() as u64 % SECTOR + r.len() as u64 > SECTOR {
                let pad = SECTOR as usize - buf.len() % SECTOR as usize;
                buf.resize(buf.len() + pad, 0);
            }
            buf.extend_from_slice(&r);
        }
        assert!(buf.len() <= me.size as usize);
        buf.resize(me.size as usize, 0);
        buf
    }

    fn path_table(&self, dirs: &[usize], tree: usize, big: bool) -> Vec<u8> {
        let mut out = Vec::new();
        for &d in dirs {
            let node = &self.nodes[d];
            let name: &[u8] = if node.parent.is_none() { &[0] } else { self.name(d, tree) };
            let parent_num = node.parent.map(|p| self.nodes[p].d[tree].num).unwrap_or(1);
            let lba = node.d[tree].lba;
            out.push(name.len() as u8);
            out.push(0);
            if big {
                out.extend_from_slice(&lba.to_be_bytes());
                out.extend_from_slice(&parent_num.to_be_bytes());
            } else {
                out.extend_from_slice(&lba.to_le_bytes());
                out.extend_from_slice(&parent_num.to_le_bytes());
            }
            out.extend_from_slice(name);
            if name.len() & 1 == 1 {
                out.push(0);
            }
        }
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn volume_descriptor(&self, joliet: bool, volid: &str, vol_sectors: u32, pt_size: u32, l_lba: u32, m_lba: u32, now: i64) -> Vec<u8> {
        let tree = if joliet { JOL } else { ISO };
        let pad: &dyn Fn(&str, usize) -> Vec<u8> = if joliet { &pad_j } else { &pad_a };
        let mut b = vec![0u8; SECTOR as usize];
        b[0] = if joliet { 2 } else { 1 };
        b[1..6].copy_from_slice(b"CD001");
        b[6] = 1;
        b[8..40].copy_from_slice(&pad("", 32));
        if joliet {
            b[40..72].copy_from_slice(&pad_j(&py_slice_to(volid, 16), 32));
            b[88..91].copy_from_slice(b"%/E");
        } else {
            b[40..72].copy_from_slice(&pad_a(&py_slice_to(&dchars(volid), 32), 32));
        }
        b[80..88].copy_from_slice(&both32(vol_sectors));
        b[120..124].copy_from_slice(&both16(1));
        b[124..128].copy_from_slice(&both16(1));
        b[128..132].copy_from_slice(&both16(SECTOR as u16));
        b[132..140].copy_from_slice(&both32(pt_size));
        b[140..144].copy_from_slice(&l_lba.to_le_bytes());
        b[148..152].copy_from_slice(&m_lba.to_be_bytes());
        let root = self.nodes[0].d[tree];
        let rr = make_record(&[0], root.lba, root.size, true, now);
        assert_eq!(rr.len(), 34);
        b[156..190].copy_from_slice(&rr);
        b[190..318].copy_from_slice(&pad("", 128));
        b[318..446].copy_from_slice(&pad("", 128));
        b[446..574].copy_from_slice(&pad("WIN-TEST-LAB", 128));
        b[574..702].copy_from_slice(&pad("MKISO.PY", 128));
        b[702..739].copy_from_slice(&pad("", 37));
        b[739..776].copy_from_slice(&pad("", 37));
        b[776..813].copy_from_slice(&pad("", 37));
        b[813..830].copy_from_slice(&dt17(now));
        b[830..847].copy_from_slice(&dt17(now));
        b[847..864].copy_from_slice(DT17_ZERO);
        b[864..881].copy_from_slice(DT17_ZERO);
        b[881] = 1;
        b
    }
}

fn rec_len(name_len: usize) -> usize {
    let ln = 33 + name_len;
    ln + (ln & 1)
}

fn sectors(n: u64) -> u64 {
    n.div_ceil(SECTOR)
}

fn make_record(name: &[u8], lba: u32, size: u32, is_dir: bool, mtime: i64) -> Vec<u8> {
    let ln = rec_len(name.len());
    let mut r = Vec::with_capacity(ln);
    r.push(ln as u8);
    r.push(0);
    r.extend_from_slice(&both32(lba));
    r.extend_from_slice(&both32(size));
    r.extend_from_slice(&dt7(mtime));
    r.extend_from_slice(&[if is_dir { 0x02 } else { 0x00 }, 0, 0]);
    r.extend_from_slice(&both16(1));
    r.push(name.len() as u8);
    r.extend_from_slice(name);
    r.resize(ln, 0);
    r
}

/// Write SRC directory as an ISO9660+Joliet image OUT with volume id VOLID. Returns the
/// image size in sectors.
pub fn make_iso(src: &Path, out: &Path, volid: &str) -> Result<u64> {
    ensure!(src.is_dir(), "mkiso: {} is not a directory", src.display());
    let now = Utc::now().timestamp();
    let mut t = Tree::build(src)?;
    t.assign_names(0);
    let mut dirs: [Vec<usize>; 2] = [Vec::new(), Vec::new()];
    for tree in [ISO, JOL] {
        dirs[tree] = t.all_dirs_bfs(tree);
        for &d in &dirs[tree] {
            let sz = t.dir_records_size(d, tree);
            t.nodes[d].d[tree].size = u32::try_from(sz).map_err(|_| anyhow!("mkiso: directory too large"))?;
        }
    }
    let pt_size = [t.path_table(&dirs[ISO], ISO, false).len() as u64, t.path_table(&dirs[JOL], JOL, false).len() as u64];
    // ---- layout
    let mut lba: u64 = 16 + 3; // PVD, SVD, terminator
    let mut pt_lba = [(0u64, 0u64); 2];
    for tree in [ISO, JOL] {
        pt_lba[tree] = (lba, lba + sectors(pt_size[tree]));
        lba += 2 * sectors(pt_size[tree]);
    }
    let too_large = |_| anyhow!("mkiso: image too large");
    for tree in [ISO, JOL] {
        for &d in &dirs[tree] {
            t.nodes[d].d[tree].lba = u32::try_from(lba).map_err(too_large)?;
            lba += sectors(t.nodes[d].d[tree].size as u64);
        }
    }
    let mut files = Vec::new();
    let mut queue = std::collections::VecDeque::from([0usize]);
    while let Some(d) = queue.pop_front() {
        for c in t.sorted_children(d, ISO) {
            if t.nodes[c].is_dir {
                queue.push_back(c);
            } else {
                files.push(c);
            }
        }
    }
    for &f in &files {
        if t.nodes[f].size > 0 {
            t.nodes[f].lba = u32::try_from(lba).map_err(too_large)?;
            lba += sectors(t.nodes[f].size);
        }
    }
    let total = lba;
    u32::try_from(total).map_err(too_large)?;

    let mut tmp = out.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let res = write_image(&t, &dirs, &files, &pt_size, &pt_lba, total, volid, now, &tmp);
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res?;
    std::fs::rename(&tmp, out).with_context(|| format!("mkiso: cannot write {}", out.display()))?;
    Ok(total)
}

#[allow(clippy::too_many_arguments)]
fn write_image(
    t: &Tree,
    dirs: &[Vec<usize>; 2],
    files: &[usize],
    pt_size: &[u64; 2],
    pt_lba: &[(u64, u64); 2],
    total: u64,
    volid: &str,
    now: i64,
    tmp: &Path,
) -> Result<()> {
    let werr = || format!("mkiso: cannot write {}", tmp.display());
    let f = File::create(tmp).with_context(werr)?;
    let mut o = BufWriter::with_capacity(1 << 20, f);
    let put = |o: &mut BufWriter<File>, data: &[u8]| o.write_all(data).with_context(werr);
    put(&mut o, &vec![0u8; 16 * SECTOR as usize])?;
    put(&mut o, &t.volume_descriptor(false, volid, total as u32, pt_size[ISO] as u32, pt_lba[ISO].0 as u32, pt_lba[ISO].1 as u32, now))?;
    put(&mut o, &t.volume_descriptor(true, volid, total as u32, pt_size[JOL] as u32, pt_lba[JOL].0 as u32, pt_lba[JOL].1 as u32, now))?;
    let mut term = vec![0u8; SECTOR as usize];
    term[0] = 255;
    term[1..6].copy_from_slice(b"CD001");
    term[6] = 1;
    put(&mut o, &term)?;
    let pad_to_sector = |len: usize| vec![0u8; ((SECTOR - len as u64 % SECTOR) % SECTOR) as usize];
    for tree in [ISO, JOL] {
        for big in [false, true] {
            let pt = t.path_table(&dirs[tree], tree, big);
            put(&mut o, &pt)?;
            put(&mut o, &pad_to_sector(pt.len()))?;
        }
    }
    for tree in [ISO, JOL] {
        for &d in &dirs[tree] {
            put(&mut o, &t.dir_data(d, tree))?;
        }
    }
    let mut chunk = vec![0u8; 8 << 20];
    for &f in files {
        let node = &t.nodes[f];
        if node.size == 0 {
            continue;
        }
        let rerr = || format!("mkiso: cannot read {}", node.path.display());
        let mut fi = File::open(&node.path).with_context(rerr)?;
        let mut left = node.size;
        while left > 0 {
            let want = left.min(chunk.len() as u64) as usize;
            let n = fi.read(&mut chunk[..want]).with_context(rerr)?;
            ensure!(n > 0, "mkiso: {} shrank while reading", node.path.display());
            put(&mut o, &chunk[..n])?;
            left -= n as u64;
        }
        put(&mut o, &pad_to_sector((node.size % SECTOR) as usize))?;
    }
    let mut f = o.into_inner().map_err(|e| e.into_error()).with_context(werr)?;
    let end = f.stream_position().with_context(werr)?;
    ensure!(end == total * SECTOR, "mkiso: internal error: wrote {} bytes, expected {}", end, total * SECTOR);
    f.sync_all().with_context(werr)?;
    Ok(())
}

/// `wtl-rs mkiso -o out.iso -V VOLID SRC_DIR`
#[derive(clap::Args, Debug)]
#[command(about = "Write an ISO9660 + Joliet image of a directory (lib/mkiso.py)")]
pub struct Cli {
    /// the image to write
    #[arg(short = 'o', value_name = "OUT.iso")]
    pub out: PathBuf,
    /// volume label
    #[arg(short = 'V', value_name = "VOLID", default_value = "CDROM")]
    pub volid: String,
    /// the directory to put on the disc
    #[arg(value_name = "SRC_DIR")]
    pub src: PathBuf,
}

pub fn cli(a: &Cli) -> Result<()> {
    make_iso(&a.src, &a.out, &a.volid).map(|_| ())
}

// ------------------------------------------------------------------ reader (tests + fake QEMU)

/// Minimal ISO9660/Joliet reader: enough to read back what [`make_iso`] writes.
pub struct IsoReader {
    pub volid: String,
    /// lower-cased path -> (lba, size, original-case path)
    pub files: Vec<(String, u32, u32, String)>,
    data: Vec<u8>,
}

impl IsoReader {
    pub fn open(path: &Path, prefer_joliet: bool) -> Result<IsoReader> {
        let data = std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
        let mut pvd = None;
        let mut svd = None;
        for sec in 16..64usize {
            let vd = match data.get(sec * 2048..(sec + 1) * 2048) {
                Some(v) if &v[1..6] == b"CD001" => v,
                _ => break,
            };
            match vd[0] {
                1 if pvd.is_none() => pvd = Some(sec),
                2 if &vd[88..91] == b"%/E" || &vd[88..91] == b"%/@" || &vd[88..91] == b"%/C" => svd = Some(sec),
                255 => break,
                _ => {}
            }
        }
        let pvd = pvd.ok_or_else(|| anyhow!("{}: not an ISO9660 image", path.display()))?;
        let volid = String::from_utf8_lossy(&data[pvd * 2048 + 40..pvd * 2048 + 72]).trim().to_string();
        let (use_sec, joliet) = match (svd, prefer_joliet) {
            (Some(s), true) => (s, true),
            _ => (pvd, false),
        };
        let mut r = IsoReader { volid, files: Vec::new(), data };
        let root = r.data[use_sec * 2048 + 156..use_sec * 2048 + 190].to_vec();
        r.walk(&root, joliet, "", 0)?;
        Ok(r)
    }

    fn walk(&mut self, rec: &[u8], joliet: bool, prefix: &str, depth: usize) -> Result<()> {
        ensure!(depth <= 16, "directories nested too deep");
        let lba = u32::from_le_bytes(rec[2..6].try_into().unwrap()) as usize;
        let size = u32::from_le_bytes(rec[10..14].try_into().unwrap()) as usize;
        let dir = self.data.get(lba * 2048..lba * 2048 + size).context("directory outside the image")?.to_vec();
        let mut pos = 0;
        while pos < dir.len() {
            let ln = dir[pos] as usize;
            if ln == 0 {
                pos = (pos / 2048 + 1) * 2048;
                continue;
            }
            let r = &dir[pos..pos + ln];
            pos += ln;
            let nl = r[32] as usize;
            let raw = &r[33..33 + nl];
            if raw == [0] || raw == [1] {
                continue;
            }
            let name = if joliet {
                let u: Vec<u16> = raw.chunks(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
                String::from_utf16_lossy(&u)
            } else {
                String::from_utf8_lossy(raw).into_owned()
            };
            let mut name = name.split(';').next().unwrap_or("").to_string();
            if !joliet {
                name = name.trim_end_matches('.').to_string();
            }
            let full = format!("{}{}", prefix, name);
            if r[25] & 2 != 0 {
                self.walk(r, joliet, &format!("{}/", full), depth + 1)?;
            } else {
                let flba = u32::from_le_bytes(r[2..6].try_into().unwrap());
                let fsize = u32::from_le_bytes(r[10..14].try_into().unwrap());
                self.files.push((full.to_lowercase(), flba, fsize, full));
            }
        }
        Ok(())
    }

    pub fn read(&self, path: &str) -> Option<&[u8]> {
        let want = path.trim_matches('/').to_lowercase();
        let (_, lba, size, _) = self.files.iter().find(|f| f.0 == want)?;
        self.data.get(*lba as usize * 2048..*lba as usize * 2048 + *size as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("wtl-rs-mkiso-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn roundtrip_both_trees() {
        let d = tmpdir("rt");
        let src = d.join("src");
        std::fs::create_dir_all(src.join("LAB/sub dir")).unwrap();
        std::fs::write(src.join("LAB/lab.json"), b"{\"role\": \"dc\"}").unwrap();
        std::fs::write(src.join("LAB/sub dir/Some Long File Name With Spaces.txt"), b"hello").unwrap();
        std::fs::write(src.join("empty.bin"), b"").unwrap();
        std::fs::write(src.join(".hidden"), b"h").unwrap();
        let big: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(src.join("big.dat"), &big).unwrap();
        // two names that collide in the ISO9660 tree
        std::fs::write(src.join("a-b.txt"), b"1").unwrap();
        std::fs::write(src.join("a_b.txt"), b"2").unwrap();
        let out = d.join("o.iso");
        let total = make_iso(&src, &out, "WTL_LAB").unwrap();
        assert_eq!(std::fs::metadata(&out).unwrap().len(), total * 2048);

        let j = IsoReader::open(&out, true).unwrap();
        assert_eq!(j.volid, "WTL_LAB");
        assert_eq!(j.read("LAB/lab.json").unwrap(), b"{\"role\": \"dc\"}");
        assert_eq!(j.read("lab/sub dir/some long file name with spaces.txt").unwrap(), b"hello");
        assert_eq!(j.read("big.dat").unwrap(), &big[..]);
        assert_eq!(j.read("empty.bin").unwrap(), b"");
        assert_eq!(j.read("a-b.txt").unwrap(), b"1");
        assert_eq!(j.read("a_b.txt").unwrap(), b"2");

        let p = IsoReader::open(&out, false).unwrap();
        let mut names: Vec<&str> = p.files.iter().map(|f| f.3.as_str()).collect();
        names.sort();
        assert_eq!(
            names,
            vec!["A_B.TXT", "A_B~1.TXT", "BIG.DAT", "EMPTY.BIN", "LAB/LAB.JSON", "LAB/SUB_DIR/SOME_LONG_FILE_NAME_WITH_S.TXT", "_HIDDEN"]
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn names() {
        assert_eq!(dchars("smoke.exe"), "SMOKE_EXE");
        assert_eq!(pad_j("", 37).len(), 37);
        assert_eq!(&pad_j("AB", 6)[..], &[0, b'A', 0, b'B', 0, b' ']);
        assert_eq!(dt17(0), b"1970010100000000\0".to_vec());
        assert_eq!(rec_len(1), 34);
        assert_eq!(rec_len(2), 36);
    }

    #[test]
    fn not_a_dir() {
        let d = tmpdir("nd");
        let e = make_iso(&d.join("missing"), &d.join("x.iso"), "X").unwrap_err().to_string();
        assert!(e.contains("is not a directory"), "{}", e);
        let _ = std::fs::remove_dir_all(&d);
    }
}
