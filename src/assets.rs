//! The files `wtl` needs besides itself: the guest scripts and answer-file templates
//! (`guest/`), the OS table (`lib/oslist.conf`) and the example labs (`labs/*.json`).
//!
//! They are compiled into the program (see `embed.rs`), so `cargo install win-test-lab` works
//! without a checkout. When the home is a win-test-lab checkout they are read from it instead,
//! so an edited guest script is used on the next run without rebuilding `wtl`.

use crate::home::Home;
use anyhow::{anyhow, Context, Result};
use std::path::PathBuf;

include!(concat!(env!("OUT_DIR"), "/assets.rs"));

/// `lib/oslist.conf` as built into this program.
pub const OSLIST: &str = include_str!("../lib/oslist.conf");

/// The OS table text.
pub fn oslist(home: &Home) -> Result<String> {
    if home.checkout {
        let p = home.dir.join("lib").join("oslist.conf");
        return std::fs::read_to_string(&p).with_context(|| format!("cannot read {}", p.display()));
    }
    Ok(OSLIST.to_string())
}

/// Every guest file (`*.ps1`, `*.tmpl`) as (file name, contents), sorted by name.
pub fn guest_files(home: &Home) -> Result<Vec<(String, Vec<u8>)>> {
    if !home.checkout {
        return Ok(GUEST.iter().map(|(n, b)| (n.to_string(), b.to_vec())).collect());
    }
    let dir = home.dir.join("guest");
    let mut v = Vec::new();
    for e in std::fs::read_dir(&dir).with_context(|| format!("cannot read {}", dir.display()))?.flatten() {
        let p = e.path();
        if p.is_file() && p.extension().is_some_and(|x| x == "ps1" || x == "tmpl") {
            v.push((
                e.file_name().to_string_lossy().into_owned(),
                std::fs::read(&p).with_context(|| format!("cannot read {}", p.display()))?,
            ));
        }
    }
    v.sort();
    Ok(v)
}

/// One guest file, e.g. `lab.ps1` or `autounattend.xml.tmpl`.
pub fn guest_file(home: &Home, name: &str) -> Result<Vec<u8>> {
    if home.checkout {
        let p = home.dir.join("guest").join(name);
        return std::fs::read(&p).with_context(|| format!("cannot read {}", p.display()));
    }
    GUEST.iter().find(|(n, _)| *n == name).map(|(_, b)| b.to_vec()).ok_or_else(|| anyhow!("no guest file {} built into wtl", name))
}

/// The built-in lab NAME (labs/NAME.json as built into this program).
pub fn builtin_lab(name: &str) -> Option<&'static str> {
    LABS.iter().find(|(n, _)| n.strip_suffix(".json") == Some(name)).map(|(_, t)| *t)
}

/// The lab names a `wtl` command accepts: the home's `labs/*.json` and the built-in labs.
pub fn lab_names(home: &Home) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(home.labs_dir())
        .map(|rd| rd.flatten().filter_map(|e| e.file_name().to_str().and_then(|n| n.strip_suffix(".json")).map(str::to_string)).collect())
        .unwrap_or_default();
    names.extend(LABS.iter().filter_map(|(n, _)| n.strip_suffix(".json")).map(str::to_string));
    names.sort();
    names.dedup();
    names
}

/// The file of lab NAME: `<home>/labs/NAME.json`, else the built-in lab NAME, written into its
/// state folder as `<state_dir>/NAME/NAME.json` (so it has a path, like any lab file). None if
/// there is no such lab.
pub fn lab_file(home: &Home, name: &str) -> Result<Option<PathBuf>> {
    let own = home.labs_dir().join(format!("{}.json", name));
    if own.is_file() {
        return Ok(Some(own));
    }
    let Some(text) = builtin_lab(name) else { return Ok(None) };
    let dir = home.state_dir.join(name);
    std::fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let p = dir.join(format!("{}.json", name));
    if std::fs::read_to_string(&p).ok().as_deref() != Some(text) {
        std::fs::write(&p, text).with_context(|| format!("cannot write {}", p.display()))?;
    }
    Ok(Some(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_files() {
        for f in ["agent.ps1", "lab.ps1", "labsvc.ps1", "setup.ps1", "autounattend.xml.tmpl", "autounattend-uefi.xml.tmpl"] {
            assert!(GUEST.iter().any(|(n, b)| *n == f && !b.is_empty()), "{} not built in", f);
        }
        assert!(OSLIST.contains("win11-arm64"));
        for l in ["school", "2019", "single-2019", "pki"] {
            assert!(builtin_lab(l).is_some(), "lab {} not built in", l);
        }
        assert!(builtin_lab("state").is_none());
    }

    /// A home that is not a checkout uses the built-in files; a checkout its own.
    #[test]
    fn checkout_or_built_in() {
        let d = std::env::temp_dir().join(format!("wtl-assets-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let h = Home::fixed(&d);
        assert!(!h.checkout);
        assert_eq!(guest_file(&h, "lab.ps1").unwrap(), GUEST.iter().find(|(n, _)| *n == "lab.ps1").unwrap().1);
        assert_eq!(oslist(&h).unwrap(), OSLIST);
        // a built-in lab is written into its state folder; the home's own labs/ wins
        let p = lab_file(&h, "school").unwrap().unwrap();
        assert_eq!(p, d.join("labs/state/school/school.json"));
        assert_eq!(std::fs::read_to_string(&p).unwrap(), builtin_lab("school").unwrap());
        assert!(lab_file(&h, "nope").unwrap().is_none());
        std::fs::create_dir_all(d.join("labs")).unwrap();
        std::fs::write(d.join("labs/school.json"), "{}").unwrap();
        assert_eq!(lab_file(&h, "school").unwrap().unwrap(), d.join("labs/school.json"));
        assert!(lab_names(&h).contains(&"single-2019".to_string()));
        // a checkout: guest/ and lib/ from disk
        std::fs::create_dir_all(d.join("guest")).unwrap();
        std::fs::create_dir_all(d.join("lib")).unwrap();
        std::fs::write(d.join("guest/lab.ps1"), "edited").unwrap();
        std::fs::write(d.join("lib/oslist.conf"), "2019 17763 x y server bios x64 2 60 Windows Server 2019\n").unwrap();
        let h = Home::fixed(&d);
        assert!(h.checkout);
        assert_eq!(guest_file(&h, "lab.ps1").unwrap(), b"edited");
        assert_eq!(guest_files(&h).unwrap().len(), 1);
        assert!(oslist(&h).unwrap().starts_with("2019 17763"));
        let _ = std::fs::remove_dir_all(&d);
    }
}
