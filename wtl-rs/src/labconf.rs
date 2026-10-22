//! Run-config parser and validator (port of lib/labconf.py; same keys, same errors).
//!
//! A run config describes ONE launch of a lab: which base lab, which host folders to mount
//! as drives, which scripts to run and as whom. See [`DOC`] for the file format.

use crate::util::{abspath, basename, is_safe_name, normpath, py_int, py_repr, py_strip, rsplit1, stem};
use serde::Serialize;
use std::collections::HashMap;
use std::fmt;
use std::path::Path;

/// The format description shown by `lab help-config` (the docstring of labconf.py).
pub const DOC: &str = r#"labconf - parse and validate a win-test-lab RUN CONFIG.

A run config describes ONE launch of a lab (which base lab, which host folders to
mount as drives, which scripts to run and as whom). It is a simple key=value file:

    # myapp.conf
    version = 2019                         # the lab (labs/<version>.conf) to boot
    mount   = /home/me/app:X, /srv/data:Z  # host folder -> drive letter in the guest
    run_as_admin   = X:\install.bat         # run as the lab's root (SYSTEM)
    run_as_student = Z:\run.bat             # run as LAB\student
    run_as_admin   = X:\cleanup.bat         # ...and so on, in the order written
    name    = smoke                        # optional label for the results folder

Rules:
  - version is required.
  - mount entries are HOSTPATH:LETTER, comma-separated. LETTER is a single A-Z,
    not C (system) or D (reserved for the lab control disc). Letters are unique.
  - run_as_admin / run_as_student are comma-separated script references. Each must
    start with a drive letter that is mounted (X:\.. or X:/..) or C:\.. ; .bat .cmd
    .ps1 .exe only. run_as_admin runs as the SYSTEM account, run_as_student as LAB\student.
  - the steps run IN THE ORDER THEY APPEAR: run_as_admin and run_as_student lines may be
    freely interleaved, so you can e.g. install as admin, run as the student, clean up as
    admin. A single line's comma-separated scripts run left to right.
  - unknown keys error.

Several configs can be layered: later files override earlier ones per key (mount / version /
name / wait / timeout_min), and a later file that defines ANY step replaces all earlier steps.
So a workload file + an OS file:
    lab run global.conf win11.conf          # version from win11.conf, the rest from global.conf

    wtl-rs labconf json CONFIG...    -> print the merged, parsed config as JSON (exit 2 on error)
    wtl-rs labconf check CONFIG...   -> validate only; print "ok: ..." or the error"#;

const SCALAR: [&str; 5] = ["version", "mount", "name", "wait", "timeout_min"];
const STEP_KEYS: [(&str, Who); 2] = [("run_as_admin", Who::Admin), ("run_as_student", Who::Student)];
const SCRIPT_EXT: [&str; 4] = [".bat", ".cmd", ".ps1", ".exe"];

/// A problem with the config itself (as opposed to e.g. an I/O error); the message is the
/// text labconf.py printed after "error: ".
#[derive(Debug, Clone)]
pub struct ConfError(pub String);

impl fmt::Display for ConfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConfError {}

macro_rules! bail {
    ($($t:tt)*) => { return Err(ConfError(format!($($t)*))) };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Who {
    Admin,
    Student,
}

impl Who {
    pub fn as_str(self) -> &'static str {
        match self {
            Who::Admin => "admin",
            Who::Student => "student",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Who::Admin => "run_as_admin",
            Who::Student => "run_as_student",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Mount {
    pub host: String,
    pub letter: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Step {
    pub who: Who,
    pub path: String,
}

/// The merged, validated config; serializes to the object `labconf.py json` printed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunConfig {
    pub version: String,
    pub name: String,
    pub mounts: Vec<Mount>,
    pub steps: Vec<Step>,
    pub wait: i64,
    pub timeout_min: i64,
}

/// One file's raw content: scalar keys (last one wins; `mount` lines append) and the step
/// lines in file order.
#[derive(Debug, Clone, Default)]
pub struct Parsed {
    pub raw: HashMap<String, String>,
    pub steps: Vec<(Who, String)>,
}

/// `[x.strip() for x in v.split(",") if x.strip()]`
fn split_list(v: &str) -> Vec<&str> {
    v.split(',').map(py_strip).filter(|x| !x.is_empty()).collect()
}

/// Make each mount's HOSTPATH absolute, relative to the config file's dir, so a merged
/// config from several files has no ambiguity about which dir a relative path meant.
fn resolve_mount_hosts(value: &str, base_dir: &Path) -> String {
    let mut out = Vec::new();
    for item in split_list(value) {
        let Some((host, letter)) = rsplit1(item, ':') else {
            out.push(item.to_string()); // malformed; build() will report it
            continue;
        };
        let mut host = py_strip(host).to_string();
        if !host.is_empty() && !Path::new(&host).is_absolute() {
            host = normpath(&base_dir.join(&host)).to_string_lossy().into_owned();
        }
        out.push(format!("{}:{}", host, py_strip(letter)));
    }
    out.join(", ")
}

pub fn parse_file(path: &str) -> Result<Parsed, ConfError> {
    let p = Path::new(path);
    if !p.is_file() {
        bail!("config not found: {}", path);
    }
    let abs = abspath(p);
    let base_dir = abs.parent().unwrap_or(Path::new("/")).to_path_buf();
    let bytes = std::fs::read(p).map_err(|e| ConfError(format!("{}: cannot read: {}", path, e)))?;
    let text = String::from_utf8(bytes).map_err(|_| ConfError(format!("{}: not valid UTF-8 text", path)))?;
    // universal newlines, like Python's text mode
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut out = Parsed::default();
    for (i, line) in text.split('\n').enumerate() {
        let n = i + 1;
        let line = py_strip(line.split('#').next().unwrap_or(""));
        if line.is_empty() {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            bail!("{}:{}: expected key = value", path, n);
        };
        let (k, v) = (py_strip(k), py_strip(v));
        if let Some((_, who)) = STEP_KEYS.iter().find(|(name, _)| *name == k) {
            out.steps.push((*who, v.to_string())); // keep order; validated in build()
        } else if k == "mount" {
            let v = resolve_mount_hosts(v, &base_dir);
            let merged = match out.raw.get(k) {
                Some(prev) => format!("{}, {}", prev, v),
                None => v,
            };
            out.raw.insert(k.to_string(), merged);
        } else if SCALAR.contains(&k) {
            out.raw.insert(k.to_string(), v.to_string()); // scalar: last one in the file wins
        } else {
            let mut known: Vec<&str> = SCALAR.iter().copied().chain(STEP_KEYS.iter().map(|(n, _)| *n)).collect();
            known.sort_unstable();
            bail!("{}:{}: unknown key {} (known: {})", path, n, py_repr(k), known.join(", "));
        }
    }
    Ok(out)
}

/// Merge parsed files left->right. Scalars: a later file's key replaces an earlier one.
/// Steps: a later file that defines any step replaces all earlier steps.
pub fn merge(parsed: Vec<Parsed>) -> Parsed {
    let mut out = Parsed::default();
    for p in parsed {
        out.raw.extend(p.raw);
        if !p.steps.is_empty() {
            out.steps = p.steps;
        }
    }
    out
}

/// `^([A-Za-z]):[\\/](.*)$` -> (letter, rest)
fn split_ref(r: &str) -> Option<(char, &str)> {
    let mut it = r.chars();
    let letter = it.next().filter(char::is_ascii_alphabetic)?;
    if it.next() != Some(':') {
        return None;
    }
    let sep = it.next().filter(|c| *c == '\\' || *c == '/')?;
    let _ = sep;
    Some((letter, &r[3..]))
}

fn int_key(raw: &HashMap<String, String>, key: &str, default: &str, path: &str) -> Result<i64, ConfError> {
    let v = raw.get(key).map(String::as_str).unwrap_or(default);
    match py_int(v) {
        Some(n) => Ok(n),
        None => bail!("{}: {} {} must be a whole number", path, key, py_repr(v)),
    }
}

pub fn build(p: &Parsed, path: &str, default_name: &str) -> Result<RunConfig, ConfError> {
    let raw = &p.raw;
    let version = raw.get("version").map(String::as_str).unwrap_or("");
    if version.is_empty() {
        bail!("{}: 'version' is required (the lab to boot, e.g. 2019)", path);
    }
    if !is_safe_name(version) {
        bail!("{}: bad version {}", path, py_repr(version));
    }

    let mut mounts = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for item in split_list(raw.get("mount").map(String::as_str).unwrap_or("")) {
        let Some((host, letter)) = rsplit1(item, ':') else {
            bail!("{}: mount {} must be HOSTPATH:LETTER", path, py_repr(item));
        };
        let host = py_strip(host);
        let letter = py_strip(letter).to_uppercase();
        if host.is_empty() {
            bail!("{}: mount {} has an empty host path", path, py_repr(item));
        }
        if !(letter.len() == 1 && letter.as_bytes()[0].is_ascii_alphabetic()) {
            bail!("{}: mount drive {} must be a single letter A-Z", path, py_repr(&letter));
        }
        if letter == "C" || letter == "D" {
            bail!("{}: drive {}: is reserved (C: system, D: lab disc); pick e.g. X, Y, Z", path, letter);
        }
        if seen.contains(&letter) {
            bail!("{}: drive {}: used by more than one mount", path, letter);
        }
        seen.push(letter.clone());
        mounts.push(Mount { host: host.to_string(), letter });
    }
    if mounts.len() > 3 {
        bail!("{}: at most 3 mounts (the lab disc takes one of the 4 CD slots)", path);
    }

    let mut steps = Vec::new();
    for (who, value) in &p.steps {
        let key = who.key();
        for r in split_list(value) {
            let Some((letter, rest)) = split_ref(r) else {
                bail!("{}: {} entry {} must be LETTER:\\path (e.g. X:\\run.bat)", path, key, py_repr(r));
            };
            let letter = letter.to_ascii_uppercase().to_string();
            if !seen.contains(&letter) && letter != "C" {
                bail!("{}: {} {} uses drive {}: which is not mounted", path, key, py_repr(r), letter);
            }
            let lower = r.to_lowercase();
            if !SCRIPT_EXT.iter().any(|e| lower.ends_with(e)) {
                bail!("{}: {} {} must end in .bat .cmd .ps1 or .exe", path, key, py_repr(r));
            }
            // normalise to backslashes for Windows
            let tail = rest.replace('/', "\\");
            steps.push(Step { who: *who, path: format!("{}:\\{}", letter, tail.trim_start_matches('\\')) });
        }
    }
    if steps.is_empty() {
        bail!("{}: nothing to do (set run_as_admin and/or run_as_student)", path);
    }

    let wait = int_key(raw, "wait", "60", path)?;
    let timeout_min = int_key(raw, "timeout_min", "60", path)?;
    let name = match raw.get("name").map(String::as_str) {
        Some(n) if !n.is_empty() => n,
        _ => default_name,
    };
    if !is_safe_name(name) {
        bail!("{}: bad name {}", path, py_repr(name));
    }

    Ok(RunConfig { version: version.to_string(), name: name.to_string(), mounts, steps, wait, timeout_min })
}

/// Parse and merge one or more config files (left->right) into a validated run config.
pub fn load<S: AsRef<str>>(paths: &[S]) -> Result<RunConfig, ConfError> {
    let Some(last) = paths.last() else {
        bail!("no config given");
    };
    let mut parsed = Vec::with_capacity(paths.len());
    for p in paths {
        parsed.push(parse_file(p.as_ref())?);
    }
    let label: Vec<&str> = paths.iter().map(|p| basename(p.as_ref())).collect();
    // the merged config's default name comes from the last file's stem
    build(&merge(parsed), &label.join(" + "), stem(basename(last.as_ref())))
}

/// `wtl-rs labconf json|check CONFIG...`
#[derive(clap::Args, Debug)]
#[command(about = "Parse / validate run configs (lib/labconf.py)", long_about = DOC)]
pub struct Cli {
    /// json: print the merged config as JSON; check: validate only
    pub action: Action,
    /// config files, merged left to right
    #[arg(required = true, value_name = "CONFIG")]
    pub paths: Vec<String>,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Json,
    Check,
}

/// Exit 0 ok, 2 config error (printed as "error: ..."), like labconf.py.
pub fn cli(a: &Cli) -> i32 {
    let paths = &a.paths;
    let cfg = match load(paths) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {}", e);
            return 2;
        }
    };
    if a.action == Action::Json {
        println!("{}", crate::pyjson::dumps_indent(&cfg, 1));
    } else {
        let n_admin = cfg.steps.iter().filter(|s| s.who == Who::Admin).count();
        let label: Vec<&str> = paths.iter().map(|p| basename(p)).collect();
        println!(
            "ok: {} -> lab {}, {} mount(s), {} step(s) ({} admin + {} student)",
            label.join(" + "),
            cfg.version,
            cfg.mounts.len(),
            cfg.steps.len(),
            n_admin,
            cfg.steps.len() - n_admin
        );
    }
    0
}
