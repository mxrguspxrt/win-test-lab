//! Build script: list the guest scripts (guest/**/*.ps1, guest/*.tmpl) and the example labs
//! (labs/*.json) for src/assets.rs, which compiles them into the program so an installed `wtl`
//! needs no checkout.
//!
//! guest/ is walked recursively, so an OS-specific override folder such as guest/2008/ is
//! embedded too; its files are keyed by the path relative to guest/ ("2008/lab.ps1"), with
//! forward slashes on every host. labs/ stays flat.

use std::fmt::Write;
use std::path::{Path, PathBuf};

/// The files in DIR with one of EXTS, sorted by name (non-recursive).
fn files(dir: &Path, exts: &[&str]) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{}: {}", dir.display(), e))
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| exts.iter().any(|e| x == *e)))
        .collect();
    v.sort();
    v
}

/// Every file under DIR (recursively) with one of EXTS, sorted by path.
fn files_rec(dir: &Path, exts: &[&str]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap_or_else(|e| panic!("{}: {}", d.display(), e)).flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.is_file() && p.extension().is_some_and(|x| exts.iter().any(|e| x == *e)) {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// The path of FILE relative to ROOT, with forward slashes ("2008/lab.ps1").
fn rel_name(root: &Path, file: &Path) -> String {
    file.strip_prefix(root).unwrap_or(file).components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/")
}

fn main() {
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let mut code = String::new();
    // (only the listed files are watched: labs/ also holds labs/state/, the lab disks)
    println!("cargo:rerun-if-changed=guest");
    println!("cargo:rerun-if-changed=embed.rs");

    // guest/: recursive, so guest/2008/*.ps1 etc. are included, keyed by their path under guest/
    let guest_dir = root.join("guest");
    writeln!(code, "/// `guest/` as built into this program: (path under guest/, contents).").unwrap();
    writeln!(code, "pub static GUEST: &[(&str, &[u8])] = &[").unwrap();
    for p in files_rec(&guest_dir, &["ps1", "tmpl", "vbs", "cmd", "bat", "reg"]) {
        println!("cargo:rerun-if-changed={}", p.display());
        let name = rel_name(&guest_dir, &p);
        let path = p.to_string_lossy().into_owned();
        writeln!(code, "    ({:?}, include_bytes!({:?})),", name, path).unwrap();
    }
    writeln!(code, "];").unwrap();

    // labs/: flat
    let labs_dir = root.join("labs");
    writeln!(code, "/// `labs/` as built into this program: (file name, contents).").unwrap();
    writeln!(code, "pub static LABS: &[(&str, &str)] = &[").unwrap();
    for p in files(&labs_dir, &["json"]) {
        println!("cargo:rerun-if-changed={}", p.display());
        let file = p.file_name().unwrap().to_string_lossy().into_owned();
        let path = p.to_string_lossy().into_owned();
        writeln!(code, "    ({:?}, include_str!({:?})),", file, path).unwrap();
    }
    writeln!(code, "];").unwrap();

    std::fs::write(out.join("assets.rs"), code).expect("write assets.rs");
}
