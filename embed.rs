//! Build script: list the guest scripts (guest/*.ps1, guest/*.tmpl) and the example labs
//! (labs/*.json) for src/assets.rs, which compiles them into the program so an installed `wtl`
//! needs no checkout.

use std::fmt::Write;
use std::path::{Path, PathBuf};

/// The files in DIR with one of EXTS, sorted by name.
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

fn main() {
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let mut code = String::new();
    // (only the listed files are watched: labs/ also holds labs/state/, the lab disks)
    println!("cargo:rerun-if-changed=guest");
    println!("cargo:rerun-if-changed=embed.rs");
    for (dir, exts, name, macro_, ty) in
        [("guest", &["ps1", "tmpl"][..], "GUEST", "include_bytes", "&[u8]"), ("labs", &["json"][..], "LABS", "include_str", "&str")]
    {
        writeln!(code, "/// `{}/` as built into this program: (file name, contents).", dir).unwrap();
        writeln!(code, "pub static {}: &[(&str, {})] = &[", name, ty).unwrap();
        for p in files(&root.join(dir), exts) {
            println!("cargo:rerun-if-changed={}", p.display());
            let file = p.file_name().unwrap().to_string_lossy().into_owned();
            let path = p.to_string_lossy().into_owned();
            writeln!(code, "    ({:?}, {}!({:?})),", file, macro_, path).unwrap();
        }
        writeln!(code, "];").unwrap();
    }
    std::fs::write(out.join("assets.rs"), code).expect("write assets.rs");
}
