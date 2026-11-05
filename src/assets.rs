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

/// `lib/vhds.conf` (the known Test Drive VHDs, see `vhd.rs`) as built into this program.
pub const VHDS: &str = include_str!("../lib/vhds.conf");

/// The known-VHD list text: the checkout's copy if the home is one, else the built-in one.
pub fn vhds(home: &Home) -> String {
    if home.checkout {
        if let Ok(t) = std::fs::read_to_string(home.dir.join("lib").join("vhds.conf")) {
            return t;
        }
    }
    VHDS.to_string()
}

/// The OS table text.
pub fn oslist(home: &Home) -> Result<String> {
    if home.checkout {
        let p = home.dir.join("lib").join("oslist.conf");
        return std::fs::read_to_string(&p).with_context(|| format!("cannot read {}", p.display()));
    }
    Ok(OSLIST.to_string())
}

/// Read one guest file by its path under `guest/` (e.g. `lab.ps1` or `2008/lab.ps1`), or None
/// if there is no such file. A checkout reads from disk; otherwise the copy built in.
fn read_guest(home: &Home, rel: &str) -> Result<Option<Vec<u8>>> {
    if home.checkout {
        let p = home.dir.join("guest").join(rel);
        return match std::fs::read(&p) {
            Ok(b) => Ok(Some(normalize_script(rel, b))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("cannot read {}", p.display())),
        };
    }
    Ok(GUEST.iter().find(|(n, _)| *n == rel).map(|(_, b)| normalize_script(rel, b.to_vec())))
}

/// Windows batch files and Windows Script Host scripts want CRLF line ends (an LF-only `.cmd`
/// can mis-parse labels and `goto`), whatever the checkout's line ends are. Other guest files
/// (PowerShell, templates) are returned as they are.
fn normalize_script(name: &str, bytes: Vec<u8>) -> Vec<u8> {
    let lower = name.to_ascii_lowercase();
    if !(lower.ends_with(".vbs") || lower.ends_with(".cmd") || lower.ends_with(".bat")) {
        return bytes;
    }
    let text = String::from_utf8_lossy(&bytes).replace("\r\n", "\n").replace('\n', "\r\n");
    text.into_bytes()
}

/// Every top-level guest file (`*.ps1`, `*.tmpl`) as (file name, contents), sorted by name.
/// The OS-specific override folders (`guest/2008/` ...) are not included here.
pub fn guest_files(home: &Home) -> Result<Vec<(String, Vec<u8>)>> {
    if !home.checkout {
        return Ok(GUEST.iter().filter(|(n, _)| !n.contains('/')).map(|(n, b)| (n.to_string(), b.to_vec())).collect());
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

/// One guest file, e.g. `lab.ps1` or `autounattend.xml.tmpl` (the top-level default).
pub fn guest_file(home: &Home, name: &str) -> Result<Vec<u8>> {
    read_guest(home, name)?.ok_or_else(|| anyhow!("no guest file {} built into wtl", name))
}

/// One guest file for OS_KEY: `guest/<os_key>/<name>` when it exists, else the top-level
/// `guest/<name>`. So an OS with its own folder (2003, 2008 R2) overrides only the scripts it
/// ships, and every other OS keeps the shared ones unchanged.
pub fn guest_file_for(home: &Home, os_key: &str, name: &str) -> Result<Vec<u8>> {
    if !os_key.is_empty() {
        if let Some(b) = read_guest(home, &format!("{}/{}", os_key, name))? {
            return Ok(b);
        }
    }
    guest_file(home, name)
}

/// An OS-specific file `guest/<os_key>/<name>` if it exists, else None. Unlike
/// [`guest_file_for`] this never falls back to a top-level `guest/<name>` — for files only some
/// OSes ship (the 2008 R2 OOBE answer file `oobe.xml.tmpl`).
pub fn guest_file_os_opt(home: &Home, os_key: &str, name: &str) -> Result<Option<Vec<u8>>> {
    if os_key.is_empty() {
        return Ok(None);
    }
    read_guest(home, &format!("{}/{}", os_key, name))
}

/// A guest script's extension? (the files staged on a build / control disc). `.tmpl` answer-file
/// templates are handled separately, not staged.
fn is_guest_script(name: &str) -> bool {
    [".ps1", ".vbs", ".cmd", ".bat"].iter().any(|e| name.to_ascii_lowercase().ends_with(e))
}

/// The guest script files in `guest/<os_key>/` as (base name, contents), sorted. Empty when the
/// OS has no folder of its own. An OS folder is self-contained: it holds that OS's whole guest
/// side (PowerShell for 2008 R2, VBScript + batch for 2003).
pub fn guest_dir_scripts(home: &Home, os_key: &str) -> Result<Vec<(String, Vec<u8>)>> {
    if os_key.is_empty() {
        return Ok(Vec::new());
    }
    let mut v = Vec::new();
    if home.checkout {
        let dir = home.dir.join("guest").join(os_key);
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                let p = e.path();
                let name = e.file_name().to_string_lossy().into_owned();
                if p.is_file() && is_guest_script(&name) {
                    let bytes = std::fs::read(&p).with_context(|| format!("cannot read {}", p.display()))?;
                    v.push((name.clone(), normalize_script(&name, bytes)));
                }
            }
        }
    } else {
        let prefix = format!("{}/", os_key);
        for (n, b) in GUEST.iter() {
            if let Some(base) = n.strip_prefix(&prefix) {
                if !base.contains('/') && is_guest_script(base) {
                    v.push((base.to_string(), normalize_script(base, b.to_vec())));
                }
            }
        }
    }
    v.sort();
    Ok(v)
}

/// The guest scripts to stage on OS_KEY's build / control disc, as (base name, contents): the
/// OS's own folder if it has one (self-contained), else the shared top-level `*.ps1`. They land
/// directly in `WTL\` (never `WTL\<os>\`).
pub fn guest_scripts_for(home: &Home, os_key: &str) -> Result<Vec<(String, Vec<u8>)>> {
    let own = guest_dir_scripts(home, os_key)?;
    if !own.is_empty() {
        return Ok(own);
    }
    let mut v: Vec<(String, Vec<u8>)> = Vec::new();
    for (n, _) in guest_files(home)?.into_iter().filter(|(n, _)| n.ends_with(".ps1")) {
        let data = guest_file(home, &n)?;
        v.push((n, data));
    }
    v.sort();
    Ok(v)
}

/// The control-disc scripts for OS_KEY: the agent / lab / labsvc entry files (whatever their
/// extension), staged into `LAB\` so the logon agent can find and run them.
pub fn control_scripts_for(home: &Home, os_key: &str) -> Result<Vec<(String, Vec<u8>)>> {
    // common.* is the shared library the VBScript agent / worker load (VBScript has no include)
    let stems = ["agent.", "lab.", "labsvc.", "common."];
    Ok(guest_scripts_for(home, os_key)?.into_iter().filter(|(n, _)| stems.iter().any(|s| n.starts_with(s))).collect())
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
        std::fs::write(d.join("lib/oslist.conf"), "2019 17763 x y server bios x64 2 60 iso Windows Server 2019\n").unwrap();
        let h = Home::fixed(&d);
        assert!(h.checkout);
        assert_eq!(guest_file(&h, "lab.ps1").unwrap(), b"edited");
        assert_eq!(guest_files(&h).unwrap().len(), 1);
        // an OS-specific override folder: guest_file_for prefers it, else falls back to the top
        std::fs::create_dir_all(d.join("guest/2008")).unwrap();
        std::fs::write(d.join("guest/2008/lab.ps1"), "ps2").unwrap();
        assert_eq!(guest_file_for(&h, "2008", "lab.ps1").unwrap(), b"ps2");
        assert_eq!(guest_file_for(&h, "2019", "lab.ps1").unwrap(), b"edited"); // no 2019 folder -> fallback
        assert_eq!(guest_file_for(&h, "", "lab.ps1").unwrap(), b"edited");
        // guest_files stays top-level only (the "2008" subdir is not counted)
        assert_eq!(guest_files(&h).unwrap().len(), 1);
        // an OS with its own folder stages that folder (self-contained); an OS without one
        // stages the shared top-level *.ps1
        let s2008 = guest_scripts_for(&h, "2008").unwrap();
        assert_eq!(s2008.iter().find(|(n, _)| n == "lab.ps1").unwrap().1, b"ps2");
        let s2019 = guest_scripts_for(&h, "2019").unwrap();
        assert_eq!(s2019.iter().find(|(n, _)| n == "lab.ps1").unwrap().1, b"edited");
        // a VBScript OS folder: its .vbs scripts are staged, and control_scripts_for picks the
        // agent/lab/labsvc entry files (not setup)
        std::fs::create_dir_all(d.join("guest/2003")).unwrap();
        for f in ["agent.vbs", "lab.vbs", "labsvc.vbs", "setup.vbs"] {
            std::fs::write(d.join("guest/2003").join(f), "'vbs").unwrap();
        }
        let s2003 = guest_scripts_for(&h, "2003").unwrap();
        assert_eq!(s2003.len(), 4);
        let ctrl: Vec<String> = control_scripts_for(&h, "2003").unwrap().into_iter().map(|(n, _)| n).collect();
        assert_eq!(ctrl, ["agent.vbs", "lab.vbs", "labsvc.vbs"]);
        assert!(oslist(&h).unwrap().starts_with("2019 17763"));
        let _ = std::fs::remove_dir_all(&d);
    }
}
