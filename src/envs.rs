//! Environments: copies of a lab, spawned on the fly from its 0 state (`wtl up LAB --new`).
//!
//! An environment is a state folder of its own, `<state_dir>/<lab>-<suffix>/`, whose 0-state
//! disks are thin overlays on the lab's. Inside, it is the same lab (computer names, domain,
//! IPs, CA); it differs by its name, a random id and its own lab LAN port, so the environments
//! of one lab run side by side without seeing each other. `env.json` in the folder says what it
//! was made from.
//!
//! The lab's 0-state disks an environment uses are hard-linked under
//! `<lab state>/frozen/<0-state id>/` when the first environment is made from them. `wtl
//! rebuild LAB` deletes the lab's own disk files, but the links keep that data alive for the
//! environments (no copy, no rename, a running environment is not touched); a frozen folder
//! goes when the last environment using it is removed ([`gc_frozen`]).

use crate::home::Home;
use crate::util;
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// `<state_dir>/<name>/env.json`
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct EnvFile {
    /// the lab's state name (`school`)
    pub lab: String,
    /// the lab file this is an environment of (absolute)
    pub lab_file: PathBuf,
    /// random, made with the environment
    pub id: String,
    #[serde(default)]
    pub note: String,
    pub created_at: String,
    /// the id of the lab's 0 state it was made from, and the folder of its frozen disks
    pub zero_id: String,
    pub frozen: PathBuf,
    /// its own lab LAN port (a lab with a `network`)
    #[serde(default)]
    pub lan_port: Option<u16>,
}

/// The file name of an environment's description in its state folder.
pub const ENV_JSON: &str = "env.json";

/// The environment called `name`, if there is one.
pub fn load(home: &Home, name: &str) -> Result<Option<EnvFile>> {
    if !util::is_safe_name(name) {
        return Ok(None);
    }
    let p = home.state_dir.join(name).join(ENV_JSON);
    if !p.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&p).with_context(|| format!("cannot read {}", p.display()))?;
    serde_json::from_str(&text).map(Some).map_err(|e| anyhow!("{}: {}", p.display(), e))
}

/// Write `env.json` into an environment's state folder.
pub fn save(state: &Path, e: &EnvFile) -> Result<()> {
    let p = state.join(ENV_JSON);
    std::fs::write(&p, format!("{}\n", serde_json::to_string_pretty(e)?)).with_context(|| format!("cannot write {}", p.display()))
}

/// Every environment in the home, by name.
pub fn all(home: &Home) -> Vec<(String, EnvFile)> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(&home.state_dir) else { return out };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if let Ok(Some(f)) = load(home, &name) {
            out.push((name, f));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// `n` random lowercase hex digits (std only: the process's random hash keys, the time and a
/// counter; plenty for names and ids, not for secrets).
pub fn random_hex(n: usize) -> String {
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut out = String::new();
    while out.len() < n {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
        h.write_u32(std::process::id());
        h.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
        out.push_str(&format!("{:016x}", h.finish()));
    }
    out.truncate(n);
    out
}

/// A random environment suffix: 4 of `a-z 0-9`, starting with a letter.
pub fn random_suffix() -> String {
    const ALPHA: &[u8] = b"abcdefghijklmnopqrstuvwxyz";
    const ALNUM: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let hex = random_hex(16);
    let bytes = u64::from_str_radix(&hex, 16).unwrap_or(0);
    let mut s = String::new();
    s.push(ALPHA[(bytes % 26) as usize] as char);
    let mut rest = bytes / 26;
    for _ in 0..3 {
        s.push(ALNUM[(rest % 36) as usize] as char);
        rest /= 36;
    }
    s
}

/// A note is one line (it goes into the registry, JSON and the status table).
pub fn check_note(note: &str) -> Result<()> {
    if note.chars().any(|c| c.is_control()) {
        bail!("--note must be a single line (no line breaks or control characters)");
    }
    Ok(())
}

/// A lab LAN port no lab and no environment in this home uses, and that is free right now.
pub fn pick_lan_port(home: &Home, labs_ports: &[u16]) -> Result<u16> {
    let mut used: Vec<u16> = labs_ports.to_vec();
    used.extend(all(home).iter().filter_map(|(_, e)| e.lan_port));
    (47100..48000)
        .find(|p| !used.contains(p) && std::net::UdpSocket::bind(("127.0.0.1", *p)).is_ok())
        .ok_or_else(|| anyhow!("no free lab LAN port in 47100-47999"))
}

/// Delete the lab's frozen 0-state folders no environment uses any more.
pub fn gc_frozen(home: &Home, lab_state: &Path) {
    let frozen = lab_state.join("frozen");
    let Ok(rd) = std::fs::read_dir(&frozen) else { return };
    let used: Vec<PathBuf> = all(home).into_iter().map(|(_, e)| util::canon(&e.frozen)).collect();
    for e in rd.flatten() {
        let p = util::canon(&e.path());
        if !used.contains(&p) {
            let _ = std::fs::remove_dir_all(&p);
        }
    }
    let _ = std::fs::remove_dir(&frozen); // only when empty
}

/// The environments made from the lab whose state folder is `lab_state_name`.
pub fn of_lab(home: &Home, lab_state_name: &str) -> Vec<(String, EnvFile)> {
    all(home).into_iter().filter(|(_, e)| e.lab == lab_state_name).collect()
}

/// `C:\WTL\instance.json` and the `.reg` file for `HKLM\SOFTWARE\WinTestLab`: who a machine is.
/// Both are written by the guest exactly as given (the JSON is pure ASCII; the .reg file is
/// written as UTF-16). `wow64` adds the 32-bit registry view, for 32-bit programs on 64-bit
/// Windows.
pub fn instance_files(i: &Instance, machine: &str, run_id: &str, wow64: bool) -> (String, String) {
    let json = crate::pyjson::dumps_indent(
        &serde_json::json!({
            "lab": i.lab,
            "environment": i.environment,
            "id": i.id,
            "machine": machine,
            "note": i.note,
            "run_id": run_id,
        }),
        2,
    );
    let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    let values = format!(
        "\"Lab\"=\"{}\"\r\n\"Environment\"=\"{}\"\r\n\"Id\"=\"{}\"\r\n\"Machine\"=\"{}\"\r\n\"Note\"=\"{}\"\r\n\"RunId\"=\"{}\"\r\n",
        esc(&i.lab),
        esc(&i.environment),
        esc(&i.id),
        esc(machine),
        esc(&i.note),
        esc(run_id)
    );
    let mut reg = format!("Windows Registry Editor Version 5.00\r\n\r\n[HKEY_LOCAL_MACHINE\\SOFTWARE\\WinTestLab]\r\n{}", values);
    if wow64 {
        reg.push_str(&format!("\r\n[HKEY_LOCAL_MACHINE\\SOFTWARE\\Wow6432Node\\WinTestLab]\r\n{}", values));
    }
    (format!("{}\r\n", json.replace('\n', "\r\n")), reg)
}

/// Who a running lab or environment is: the lab, the environment's name (the lab's own name for
/// the lab itself), its id and note.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Instance {
    pub lab: String,
    pub environment: String,
    pub id: String,
    pub note: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_names_and_ids() {
        let a = random_hex(8);
        let b = random_hex(8);
        assert!(a.len() == 8 && a.bytes().all(|c| c.is_ascii_hexdigit()) && a != b, "{} {}", a, b);
        let s = random_suffix();
        assert!(s.len() == 4 && s.as_bytes()[0].is_ascii_lowercase() && util::is_safe_name(&s), "{}", s);
        assert!(check_note("new installer on 2016").is_ok());
        assert!(check_note("two\nlines").is_err());
    }

    #[test]
    fn instance_files_escape_and_views() {
        let i =
            Instance { lab: "school".into(), environment: "school-exp42".into(), id: "c2e5a813".into(), note: "a \"q\" \\ Ünï".into() };
        let (json, reg) = instance_files(&i, "CL1", "20261124-101500-c2e5a813", true);
        assert!(json.is_ascii(), "{}", json);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["environment"], "school-exp42");
        assert_eq!(v["note"], "a \"q\" \\ Ünï");
        assert_eq!(v["machine"], "CL1");
        assert!(reg.starts_with("Windows Registry Editor Version 5.00\r\n"));
        assert!(reg.contains("\"Note\"=\"a \\\"q\\\" \\\\ Ünï\"\r\n"), "{}", reg);
        assert!(reg.contains("[HKEY_LOCAL_MACHINE\\SOFTWARE\\Wow6432Node\\WinTestLab]"));
        let (_, reg32) = instance_files(&i, "WS2003", "", false);
        assert!(!reg32.contains("Wow6432Node"));
    }
}
