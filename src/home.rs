//! Where win-test-lab keeps its data, the "home": the Windows ISOs, the installed base images,
//! each lab's 0 state and runs, the base-image build scratch and the arm64 drivers.
//!
//! The home is, first match wins:
//!
//! 1. `--home DIR` (passed on as `WTL_HOME`), or `$WTL_HOME`
//! 2. the current folder or a parent of it that has a `wtl.json` (see `wtl init`) or is a
//!    win-test-lab checkout
//! 3. the checkout this program was built in (`target/release/wtl` of a clone)
//! 4. `home` in the user config file
//! 5. the per-user data folder: `~/.local/share/win-test-lab` (Linux),
//!    `~/Library/Application Support/win-test-lab` (macOS), `%LOCALAPPDATA%\win-test-lab\data`
//!    (Windows)
//!
//! Each folder can also be moved on its own. Its `WTL_*` variable wins over the home's
//! `wtl.json`, which wins over the user config (`$WTL_CONFIG`, else `config.json` in the
//! per-user config folder), which wins over the default under the home. A relative path in a
//! config file is relative to the folder the file is in.

use crate::util;
use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// One folder of the home: its config key, its variable, its default under the home.
struct Folder {
    key: &'static str,
    var: &'static str,
    default: &'static str,
    get: fn(&FileConfig) -> &Option<String>,
}

const FOLDERS: [Folder; 5] = [
    Folder { key: "iso_dir", var: "WTL_ISO_DIR", default: "iso-files", get: |c| &c.iso_dir },
    Folder { key: "images_dir", var: "WTL_IMAGES_DIR", default: "images", get: |c| &c.images_dir },
    Folder { key: "state_dir", var: "WTL_STATE_DIR", default: "labs/state", get: |c| &c.state_dir },
    Folder { key: "runs_dir", var: "WTL_RUNS_DIR", default: "runs", get: |c| &c.runs_dir },
    Folder { key: "drivers_dir", var: "WTL_DRIVERS_DIR", default: "drivers", get: |c| &c.drivers_dir },
];

/// A `wtl.json` in a home, or the user config file.
#[derive(Deserialize, Default, Debug)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    #[serde(rename = "$schema", default)]
    _schema: Option<String>,
    /// the home folder (user config only)
    #[serde(default)]
    home: Option<String>,
    #[serde(default)]
    iso_dir: Option<String>,
    #[serde(default)]
    images_dir: Option<String>,
    #[serde(default)]
    state_dir: Option<String>,
    #[serde(default)]
    runs_dir: Option<String>,
    #[serde(default)]
    drivers_dir: Option<String>,
}

/// The resolved home and its folders.
#[derive(Clone, Debug)]
pub struct Home {
    pub dir: PathBuf,
    /// how `dir` was chosen, for `wtl home`
    pub why: String,
    /// the home is a win-test-lab checkout: guest/, lib/oslist.conf and labs/ are read from
    /// it instead of the copies compiled into the program
    pub checkout: bool,
    /// the Windows installer ISOs
    pub iso_dir: PathBuf,
    /// the installed base images (`<os>-dc.qcow2`, `<os>-client.qcow2`)
    pub images_dir: PathBuf,
    /// each lab's 0 state, run disks, logs and results (`<state_dir>/<lab>/`)
    pub state_dir: PathBuf,
    /// base-image build scratch (`<runs_dir>/build-<image>/`)
    pub runs_dir: PathBuf,
    /// virtio-win drivers for arm64 machines
    pub drivers_dir: PathBuf,
    /// where each folder's value came from, in the order above (for `wtl home`)
    pub sources: Vec<String>,
    /// the home's own config file (it may not exist)
    pub local_config: PathBuf,
    /// the user config file (it may not exist)
    pub user_config: Option<PathBuf>,
}

/// `d` is a win-test-lab checkout (lib/oslist.conf + guest/lab.ps1).
pub fn is_checkout(d: &Path) -> bool {
    d.join("lib").join("oslist.conf").is_file() && d.join("guest").join("lab.ps1").is_file()
}

fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

fn read_config(p: &Path) -> Result<Option<FileConfig>> {
    let text = match std::fs::read_to_string(p) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", p.display())),
    };
    serde_json::from_str(&text).map(Some).map_err(|e| anyhow!("{}: {}", p.display(), e))
}

/// A path from a config file: relative to the file's folder.
fn from_file(file: &Path, v: &str) -> PathBuf {
    let p = Path::new(v);
    if p.is_absolute() {
        util::normpath(p)
    } else {
        util::normpath(&file.parent().unwrap_or(Path::new(".")).join(p))
    }
}

fn project_dirs() -> Option<directories::ProjectDirs> {
    directories::ProjectDirs::from("", "", "win-test-lab")
}

/// The user config file: `$WTL_CONFIG`, else `config.json` in the per-user config folder.
pub fn user_config_path() -> Option<PathBuf> {
    var("WTL_CONFIG").map(PathBuf::from).or_else(|| project_dirs().map(|p| p.config_dir().join("config.json")))
}

impl Home {
    /// Find the home (see the module docs).
    pub fn resolve() -> Result<Home> {
        let user_config = user_config_path();
        let user = match &user_config {
            Some(p) => read_config(p)?,
            None => None,
        };
        let cwd = std::env::current_dir().ok();
        let (dir, why) = if let Some(h) = var("WTL_HOME").or_else(|| var("WTL_ROOT")) {
            (util::canon(Path::new(&h)), "WTL_HOME / --home".to_string())
        } else if let Some((d, why)) = cwd.as_deref().and_then(|c| {
            c.ancestors().find_map(|d| {
                if d.join("wtl.json").is_file() {
                    Some((d.to_path_buf(), format!("{} in the current folder or above", d.join("wtl.json").display())))
                } else if is_checkout(d) {
                    Some((d.to_path_buf(), "the win-test-lab checkout in the current folder or above".to_string()))
                } else {
                    None
                }
            })
        }) {
            (util::canon(&d), why)
        } else if let Some(d) =
            std::env::current_exe().ok().and_then(|e| util::canon(&e).ancestors().find(|d| is_checkout(d)).map(Path::to_path_buf))
        {
            (d, "the checkout this program was built in".to_string())
        } else if let (Some(h), Some(f)) = (user.as_ref().and_then(|u| u.home.clone()), user_config.as_ref()) {
            (from_file(f, &h), format!("\"home\" in {}", f.display()))
        } else {
            let d = project_dirs()
                .map(|p| p.data_local_dir().to_path_buf())
                .ok_or_else(|| anyhow!("cannot find the per-user data folder (no home directory?); set WTL_HOME"))?;
            (d, "the per-user data folder (the default)".to_string())
        };
        let local_config = dir.join("wtl.json");
        let local = read_config(&local_config)?;
        if local.as_ref().is_some_and(|l| l.home.is_some()) {
            bail!("{}: \"home\" belongs in the user config file, not in a home's own wtl.json", local_config.display());
        }
        let mut sources = Vec::new();
        let mut dirs = Vec::new();
        for f in &FOLDERS {
            let (p, from) = if let Some(v) = var(f.var) {
                (util::abspath(Path::new(&v)), f.var.to_string())
            } else if let Some(v) = local.as_ref().and_then(|c| (f.get)(c).clone()) {
                (from_file(&local_config, &v), local_config.display().to_string())
            } else if let (Some(v), Some(file)) = (user.as_ref().and_then(|c| (f.get)(c).clone()), user_config.as_ref()) {
                (from_file(file, &v), file.display().to_string())
            } else {
                (dir.join(f.default), "default".to_string())
            };
            dirs.push(p);
            sources.push(from);
        }
        let mut it = dirs.into_iter();
        let mut next = || it.next().expect("one per folder");
        Ok(Home {
            checkout: is_checkout(&dir),
            iso_dir: next(),
            images_dir: next(),
            state_dir: next(),
            runs_dir: next(),
            drivers_dir: next(),
            dir,
            why,
            sources,
            local_config,
            user_config,
        })
    }

    /// A home at DIR with every folder at its default (tests).
    pub fn fixed(dir: &Path) -> Home {
        let j = |f: &Folder| dir.join(f.default);
        Home {
            dir: dir.to_path_buf(),
            why: "fixed".into(),
            checkout: is_checkout(dir),
            iso_dir: j(&FOLDERS[0]),
            images_dir: j(&FOLDERS[1]),
            state_dir: j(&FOLDERS[2]),
            runs_dir: j(&FOLDERS[3]),
            drivers_dir: j(&FOLDERS[4]),
            sources: vec!["default".into(); FOLDERS.len()],
            local_config: dir.join("wtl.json"),
            user_config: None,
        }
    }

    /// The labs a lab name refers to: `<home>/labs/<name>.json`.
    pub fn labs_dir(&self) -> PathBuf {
        self.dir.join("labs")
    }

    /// The folders as `(variable, path)`: what a child `wtl build-image` is given so it sees
    /// the same home.
    pub fn vars(&self) -> Vec<(&'static str, PathBuf)> {
        let mut v = vec![("WTL_HOME", self.dir.clone())];
        for (f, p) in FOLDERS.iter().zip(self.folders()) {
            v.push((f.var, p.clone()));
        }
        v
    }

    fn folders(&self) -> [&PathBuf; 5] {
        [&self.iso_dir, &self.images_dir, &self.state_dir, &self.runs_dir, &self.drivers_dir]
    }

    /// `wtl home`: the home, how it was found, and each folder with where its value came from.
    pub fn describe(&self) -> String {
        let mut s = format!("home        {}\n            ({})\n", self.dir.display(), self.why);
        s += &format!(
            "files       {}\n",
            if self.checkout {
                "guest scripts, OS table and labs read from this checkout"
            } else {
                "guest scripts and OS table built into wtl"
            }
        );
        let state = |p: &Path| if p.is_file() { "" } else { " (none)" };
        s += &format!("config      {}{}\n", self.local_config.display(), state(&self.local_config));
        match &self.user_config {
            Some(u) => s += &format!("user config {}{}\n", u.display(), state(u)),
            None => s += "user config (no per-user config folder)\n",
        }
        s += &format!("labs        {}\n", self.labs_dir().display());
        for ((f, p), from) in FOLDERS.iter().zip(self.folders()).zip(&self.sources) {
            s += &format!(
                "{:<11} {}  [{}]\n",
                f.key,
                p.display(),
                if from == "default" { format!("default; {} or \"{}\"", f.var, f.key) } else { from.clone() }
            );
        }
        s
    }
}

/// `wtl init [DIR]`: make DIR (default: the current folder) a home by writing its wtl.json.
pub fn init(dir: &Path) -> Result<PathBuf> {
    let p = dir.join("wtl.json");
    if p.exists() {
        bail!("{} already exists", p.display());
    }
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let mut body = String::from("{\n");
    for (i, f) in FOLDERS.iter().enumerate() {
        body += &format!("  \"{}\": \"{}\"{}\n", f.key, f.default, if i + 1 < FOLDERS.len() { "," } else { "" });
    }
    body += "}\n";
    std::fs::write(&p, body).with_context(|| format!("cannot write {}", p.display()))?;
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_paths_are_relative_to_the_file() {
        assert_eq!(from_file(Path::new("/h/wtl.json"), "isos"), PathBuf::from("/h/isos"));
        assert_eq!(from_file(Path::new("/h/wtl.json"), "../big/images"), PathBuf::from("/big/images"));
        assert_eq!(from_file(Path::new("/h/wtl.json"), "/abs/x"), PathBuf::from("/abs/x"));
    }

    #[test]
    fn config_files_are_strict() {
        let d = std::env::temp_dir().join(format!("wtl-home-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("c.json");
        std::fs::write(&p, r#"{ "images_dir": "/big", "frobnicate": 1 }"#).unwrap();
        assert!(read_config(&p).unwrap_err().to_string().contains("frobnicate"));
        std::fs::write(&p, r#"{ "images_dir": "/big" }"#).unwrap();
        assert_eq!(read_config(&p).unwrap().unwrap().images_dir.as_deref(), Some("/big"));
        assert!(read_config(&d.join("none.json")).unwrap().is_none());
        // wtl init writes a config that reads back
        let h = d.join("h");
        let w = init(&h).unwrap();
        let c = read_config(&w).unwrap().unwrap();
        assert_eq!(c.state_dir.as_deref(), Some("labs/state"));
        assert!(init(&h).is_err(), "init twice");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn fixed_home_defaults() {
        let h = Home::fixed(Path::new("/r"));
        assert_eq!(h.images_dir, PathBuf::from("/r/images"));
        assert_eq!(h.state_dir, PathBuf::from("/r/labs/state"));
        assert_eq!(h.labs_dir(), PathBuf::from("/r/labs"));
        let v = h.vars();
        assert_eq!(v[0], ("WTL_HOME", PathBuf::from("/r")));
        assert!(v.contains(&("WTL_DRIVERS_DIR", PathBuf::from("/r/drivers"))));
    }
}
