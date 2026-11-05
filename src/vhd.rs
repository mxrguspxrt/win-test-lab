//! Recognise a Microsoft "Test Drive" `.vhd` by its content, and the list of the ones we know
//! (`lib/vhds.conf`).
//!
//! A VHD ends with a 512-byte footer (a dynamic VHD repeats it at the start) that holds a
//! 16-byte unique ID. Every copy of the same download has the same one, whatever the file is
//! called and wherever it was unpacked, so a renamed or moved VHD is still found. Microsoft's
//! files cannot be shipped with this project; the list only says how to recognise them and where
//! to download them.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// One known download: the OS key it serves, the unique ID in its VHD footer (32 lowercase hex
/// digits), the Microsoft page to download it from and the file name it unpacks to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Known {
    pub key: String,
    pub id: String,
    pub url: String,
    pub file: String,
}

/// The lines of `lib/vhds.conf`: `key  id  url  file`, `#` starts a comment.
pub fn parse_catalog(text: &str) -> Vec<Known> {
    text.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split('#').next()?.split_whitespace().collect();
            if f.len() < 2 {
                return None;
            }
            let col = |i: usize| f.get(i).copied().unwrap_or("").to_string();
            Some(Known { key: col(0), id: col(1).to_ascii_lowercase(), url: col(2), file: col(3) })
        })
        .collect()
}

/// The OS key a known VHD with this unique ID serves.
pub fn owner<'a>(known: &'a [Known], id: &str) -> Option<&'a str> {
    known.iter().find(|k| k.id == id).map(|k| k.key.as_str())
}

/// Where to download the VHD for OS KEY (the first listed).
pub fn download_url<'a>(known: &'a [Known], key: &str) -> Option<&'a str> {
    known.iter().find(|k| k.key == key && !k.url.is_empty()).map(|k| k.url.as_str())
}

/// The unique ID in PATH's VHD footer as 32 lowercase hex digits, or None if it is not a VHD.
/// Reads a few hundred bytes, however big the file is.
pub fn unique_id(path: &Path) -> Option<String> {
    let mut f = File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    // the footer: the last 512 bytes (Virtual PC 2004 wrote 511), else the copy at the start
    let mut spots = Vec::new();
    if len >= 512 {
        spots.push(len - 512);
    }
    if len >= 511 {
        spots.push(len - 511);
    }
    spots.push(0);
    for at in spots {
        let mut b = [0u8; 84];
        if f.seek(SeekFrom::Start(at)).is_ok() && f.read_exact(&mut b).is_ok() && &b[..8] == b"conectix" {
            return Some(b[68..84].iter().map(|x| format!("{:02x}", x)).collect());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file shaped like a VHD: `body` zero bytes, then a footer with this unique ID.
    fn fake_vhd(path: &Path, id: [u8; 16], body: usize, footer_at_start: bool) {
        let mut footer = vec![0u8; 512];
        footer[..8].copy_from_slice(b"conectix");
        footer[68..84].copy_from_slice(&id);
        let mut data = Vec::new();
        if footer_at_start {
            data.extend_from_slice(&footer);
        }
        data.extend(vec![0u8; body]);
        data.extend_from_slice(&footer);
        std::fs::write(path, data).unwrap();
    }

    #[test]
    fn reads_the_footer_id() {
        let d = std::env::temp_dir().join(format!("wtl-vhd-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let id = [0x60, 0x47, 0x8c, 0xe3, 0x59, 0x11, 0x4c, 0xca, 0xb6, 0x2b, 0x70, 0x64, 0xe8, 0xca, 0xb3, 0x39];
        let want = "60478ce359114ccab62b7064e8cab339";
        // fixed VHD (footer at the end only), dynamic VHD (also at the start)
        fake_vhd(&d.join("fixed.vhd"), id, 4096, false);
        fake_vhd(&d.join("dynamic.vhd"), id, 4096, true);
        assert_eq!(unique_id(&d.join("fixed.vhd")).as_deref(), Some(want));
        assert_eq!(unique_id(&d.join("dynamic.vhd")).as_deref(), Some(want));
        // a footer only at the start still counts (a truncated copy of a dynamic VHD)
        let mut cut = std::fs::read(d.join("dynamic.vhd")).unwrap();
        cut.truncate(cut.len() - 512);
        std::fs::write(d.join("cut.vhd"), cut).unwrap();
        assert_eq!(unique_id(&d.join("cut.vhd")).as_deref(), Some(want));
        // not a VHD, too short, missing
        std::fs::write(d.join("text.vhd"), vec![b'x'; 2048]).unwrap();
        std::fs::write(d.join("tiny.vhd"), b"conectix").unwrap();
        assert_eq!(unique_id(&d.join("text.vhd")), None);
        assert_eq!(unique_id(&d.join("tiny.vhd")), None);
        assert_eq!(unique_id(&d.join("nope.vhd")), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn catalog() {
        let k = parse_catalog("# c\n2003 ABCDEF https://x/1 a.vhd  # note\n\n2008 0123 https://x/2\nbad\n");
        assert_eq!(k.len(), 2);
        assert_eq!(k[0], Known { key: "2003".into(), id: "abcdef".into(), url: "https://x/1".into(), file: "a.vhd".into() });
        assert_eq!(owner(&k, "abcdef"), Some("2003"));
        assert_eq!(owner(&k, "ffff"), None);
        assert_eq!(download_url(&k, "2008"), Some("https://x/2"));
        assert_eq!(download_url(&k, "2012"), None);
    }

    /// The list we ship: every entry is for a VHD-import OS, has a 32-digit ID and a Microsoft link.
    #[test]
    fn shipped_catalog() {
        let known = parse_catalog(crate::assets::VHDS);
        let table = crate::oslist::parse_table(crate::assets::OSLIST);
        assert!(!known.is_empty());
        for k in &known {
            assert!(table.iter().any(|o| o.key == k.key && o.is_vhd()), "{}: not a VHD-import OS", k.key);
            assert!(k.id.len() == 32 && k.id.bytes().all(|b| b.is_ascii_hexdigit()), "{}: bad ID {:?}", k.key, k.id);
            assert!(k.url.starts_with("https://www.microsoft.com/"), "{}: {:?}", k.key, k.url);
        }
        // every VHD-import OS has at least one
        for o in table.iter().filter(|o| o.is_vhd()) {
            assert!(known.iter().any(|k| k.key == o.key), "{}: no known VHD in lib/vhds.conf", o.key);
        }
    }
}
