//! The lab runner: a lab (`labs/<name>.json`) and a run config (`<app>.json`), see
//! [`crate::labdef`]. [`crate::lab`] is its command line.
//!
//! A lab is one DC plus one or more domain-joined members, all on one private multicast LAN
//! (`-netdev dgram`, which joins N VMs with no root). It is built once into a frozen 0 state
//! (`<state_dir>/<lab>/<machine>-0.qcow2`, see [`crate::home`]) and every run starts from
//! throwaway overlays on top.
//!
//! Steps are **guest-driven**: each machine gets its ordered step list on its control disc and
//! runs its own steps top to bottom, reporting each over its serial log. A step that targets
//! several machines runs on them in parallel; per-machine order is kept. The host has no write
//! channel into a booted guest (mounts are read-only ISOs), so there is no host-enforced
//! barrier *between* different steps across machines — the simple model the user asked for.

use crate::assets;
use crate::envs::{self, EnvFile, Instance};
use crate::home::Home;
use crate::lab::{bound_vnc_display, build_base_images, indent_file, pick_vnc_display, read_trim, sock_path, write_file, BaseBuild};
use crate::labdef::{self, Lab, Role, Run, RunStep, StepAs};
use crate::logx::{err, log, ok, warn};
use crate::oslist::{self, OsInfo};
use crate::qemu::{self, Arch, Env, Lan, Opts};
use crate::qmp::{Addr, Qmp};
use crate::serialwait::{Outcome, Wait};
use crate::{mkiso, outputs, pyjson, sys, util};
use anyhow::{anyhow, bail, Context, Result};
use indexmap::IndexMap;
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Administratively-scoped IPv4 multicast group for the lab LAN (239.0.0.0/8 is site-local
/// and never routed off the host). Labs are isolated by their unique `lan_port`, not the group.
const LAN_GROUP: &str = "239.77.30.1";

/// Ends `wtl` with this exit code; the reason has already been printed.
#[derive(Debug)]
struct Exit(i32);
impl std::fmt::Display for Exit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "exit {}", self.0)
    }
}
impl std::error::Error for Exit {}

// --------------------------------------------------------------- the control-disc lab.json

/// `LAB\lab.json` on a machine's control disc, read by guest/lab.ps1. `fmt: "multi"` names
/// this format; guest/lab.ps1 refuses anything else (e.g. a disc from an older `wtl`).
#[derive(Serialize)]
struct LabJson<'a> {
    fmt: &'a str,
    /// "domain" (AD: a DC, the others join) or "workgroup" (standalone machines, local users)
    mode: &'a str,
    role: &'a str,
    /// the machine's key in the lab file (what run files and events name it by)
    machine: &'a str,
    /// its Windows computer name (the key, a lab file's `hostname`, or an environment's own);
    /// the build renames the machine to it, and lab.ps1 checks the logon against it
    hostname: &'a str,
    /// the machine has a lab LAN NIC (false for a single workgroup machine)
    lan: bool,
    /// the static IP on the lab LAN ("" without one)
    ip: &'a str,
    prefix: i64,
    /// the DC's IP: the DNS server of every machine ("" in workgroup mode)
    dc_ip: &'a str,
    lan_mac: &'a str,
    /// AD mode only ("" in workgroup mode)
    domain: &'a str,
    netbios: &'a str,
    admin_password: &'a str,
    /// every lab user: created on the DC in AD mode, on every machine in workgroup mode
    users: Vec<LabUser<'a>>,
    logon_user: &'a str,
    logon_password: &'a str,
    shutdown_when_ready: bool,
    /// how much longer the guest's own fixed waits are: 1 on the hypervisor, more for a
    /// machine the host emulates (an x64 guest on an Arm Mac starts PowerShell in ~50 s)
    slow: u32,
    /// this machine's server roles (`ad-ds` `ad-cs` `dhcp` `iis`), baked in during the build
    roles: Vec<&'a str>,
    /// Windows feature names to install during the build
    features: Vec<&'a str>,
    /// role options the guest needs during the build (null when the role is absent)
    ad_cs: Option<LabAdCs<'a>>,
    dhcp: Option<LabDhcp>,
    iis: Option<LabIis<'a>>,
    /// setup scripts run once during the build (empty on a run); staged on the control disc
    setup: Vec<LabSetup<'a>>,
    /// the per-machine run (null during the 0-state build and for `up`)
    run: Option<LabRun<'a>>,
    /// who this machine is (lab, environment, id, machine, note, run id), for programs inside
    /// it: the guest writes `json` to C:\WTL\instance.json and imports `reg` (a .reg file) into
    /// HKLM\SOFTWARE\WinTestLab at every `up` / `run` (null during the 0-state build)
    instance: Option<LabInstance>,
    /// the boot that renames an environment's machines (null otherwise)
    rename: Option<LabRename<'a>>,
}

/// One machine's part of renaming an environment (`wtl up LAB --new --rename`): from its
/// 0-state name to `to` (the same for a machine that only re-requests its certificates), and
/// what depends on the name. The guest's rename script does it; see SPEC.md, "Renames".
#[derive(Serialize)]
struct LabRename<'a> {
    from: &'a str,
    to: &'a str,
    /// this machine hosts the CA: uninstall it before the rename, install it again after
    ca: bool,
    /// the lab's CA name, whose certificates are requested again ("" without a CA)
    ca_name: &'a str,
    /// a member whose CA or DHCP server needs the domain Administrator's session after the
    /// rename (phase rename-admin)
    admin: bool,
    /// re-authorize this machine's DHCP server under the new name
    dhcp: bool,
    /// the IIS test page to write again with the new name ("" without IIS)
    iis_site_path: &'a str,
    /// the lab has an auto-enrolling CA: drop the old machine certificate, request a new one
    reenroll: bool,
    /// the CA was reinstalled: also drop the logon user's old certificates
    reenroll_user: bool,
    /// the DC only: the members being renamed (old -> new computer name), whose old DNS
    /// records it deletes once their new ones are registered
    peers: IndexMap<&'a str, &'a str>,
}

#[derive(Serialize)]
struct LabInstance {
    environment: String,
    id: String,
    json: String,
    reg: String,
}

#[derive(Serialize)]
struct LabAdCs<'a> {
    ca_name: &'a str,
    key_length: u32,
    validity_years: u32,
    autoenroll: bool,
}

#[derive(Serialize)]
struct LabDhcp {
    /// first and last IPs of the scope (full addresses, built from the subnet)
    start: String,
    end: String,
    /// the scope's /24 mask
    mask: String,
    /// router + DNS for the scope (the DC's IP)
    router: String,
}

#[derive(Serialize)]
struct LabIis<'a> {
    site_path: &'a str,
}

/// One setup script as the guest sees it: its path on the control disc (D:\LAB\setup\..) and
/// how to run it.
#[derive(Serialize)]
struct LabSetup<'a> {
    /// path on the staged control disc, e.g. `setup/configure.ps1` (under LAB\)
    file: &'a str,
    args: &'a str,
    /// "system" or a user name
    as_: &'a str,
    user: &'a str,
    password: &'a str,
}

#[derive(Serialize)]
struct LabUser<'a> {
    name: &'a str,
    password: &'a str,
    groups: Vec<&'a str>,
}

#[derive(Serialize)]
struct LabRun<'a> {
    /// the drive letters this machine mounts (a subset of the run's mounts: those its steps use)
    mounts: Vec<LabMount<'a>>,
    steps: Vec<LabStep<'a>>,
    wait: i64,
    /// set in every step's environment (`WTL_NAME` -> value): the run's variables, then
    /// WTL_ENVIRONMENT, WTL_ENVIRONMENT_ID, WTL_MACHINE, WTL_RUN_ID
    env: IndexMap<String, String>,
}

#[derive(Serialize)]
struct LabMount<'a> {
    host: &'a str,
    letter: &'a str,
}

/// One step as the guest sees it on a given machine.
#[derive(Serialize)]
struct LabStep<'a> {
    /// 1-based index within this machine's step list
    n: i64,
    /// "system" | "logon" | "user"
    as_: &'a str,
    /// for "user": the account to run as (and its password); empty otherwise
    user: &'a str,
    password: &'a str,
    path: &'a str,
    args: &'a str,
    keep_running: bool,
    wait: i64,
}

// --------------------------------------------------------------- VM bookkeeping

/// One machine's runtime identity (name == the QEMU/state id).
struct Vm {
    name: String,
    /// the Windows computer name: the lab's (`hostname`, else the key) or an environment's own
    hostname: String,
    role: Role,
    os: OsInfo,
    mem: String,
    cpus: String,
    /// static IP on the lab LAN; empty without one
    ip: String,
    mac: String,
    logon: Option<String>,
    /// server roles baked into the 0 state (in declaration order)
    roles: Vec<labdef::LabRole>,
    /// Windows features installed during the build
    features: Vec<String>,
    /// setup scripts run once during the build
    setup: Vec<labdef::Setup>,
    ad_cs: Option<labdef::AdCsOpts>,
    dhcp: Option<labdef::DhcpOpts>,
    iis: Option<labdef::IisOpts>,
    /// this machine's steps (host-side), in order, with their 1-based index
    steps: Vec<(i64, RunStep)>,
    /// drive letters this machine's steps use (host path, letter)
    mounts: Vec<(String, String)>,
}

/// 0-state marker, written when the build is complete.
#[derive(Serialize, serde::Deserialize, Default)]
struct ZeroState {
    #[serde(default)]
    lab: String,
    #[serde(default)]
    built_at: String,
    #[serde(default)]
    machines: Vec<String>,
    /// what the guest scripts baked into it can do (see [`GUEST_FEATURES`]); none for a 0
    /// state built by an older `wtl`
    #[serde(default)]
    guest: Vec<String>,
    /// random, made with the 0 state (an environment's: its own id); empty in one built by an
    /// older `wtl` until something needs it ([`MultiLab::zero_id`])
    #[serde(default)]
    id: String,
}

/// What this program's guest scripts can do once baked into a 0 state: `step-env` = the
/// SYSTEM worker sets the run's vars in a step's environment.
const GUEST_FEATURES: &[&str] = &["step-env"];

/// One step's outcome, written into runs/<id>/result.json.
#[derive(Serialize, Clone)]
struct StepResult {
    machine: String,
    /// who ran it: "system", "logon" (the machine's own user), or a user name
    user: String,
    /// per-machine 1-based step index
    n: i64,
    script: String,
    /// the guest's reported exit: a number, or "running"/"timeout"/"launchfail"
    exit: String,
    expect_exit: i64,
    secs: f64,
    ok: bool,
}

/// The whole run's summary, written as runs/<id>/result.json.
#[derive(Serialize)]
struct RunResult {
    lab: String,
    /// the environment it ran in (the lab's own name for the lab itself) and its id
    environment: String,
    id: String,
    /// every machine's Windows computer name (key -> hostname)
    hostnames: IndexMap<String, String>,
    config: String,
    run_id: String,
    /// the run's variables and the values used (a secret one as `***`)
    vars: IndexMap<String, String>,
    /// "ok" if every step met its expect_exit, else "failed"
    status: String,
    steps: Vec<StepResult>,
}

pub struct MultiLab {
    env: Env,
    /// the WTL_* settings (defaults + environment), passed on to `wtl build-image`
    vars: HashMap<String, String>,
    /// install base images with Windows Update (--updates)
    updates: bool,
    cmd: &'static str,
    /// the state name (`<state_dir>/<name>/`)
    name: String,
    /// how to name this lab on the `wtl` command line in hints: the name for a lab in the
    /// home's labs/ or a built-in one, the lab file's path for one anywhere else
    arg: String,
    lab_file: PathBuf,
    lab: Lab,
    /// the run config file (absolute), if any
    config: Option<PathBuf>,
    /// the run's variables with their values (empty without a run)
    run_vars: Vec<labdef::RunVar>,
    /// set when this is an environment (`wtl up LAB --new`), not the lab itself
    envf: Option<EnvFile>,
    /// `status LAB/MACHINE`: the one machine to show
    only: Option<String>,
    /// this run's id (`<time>-<id>`); empty outside `wtl run`
    run_id: String,
    /// the boot that renames an environment's machines is under way (`rename_machines`)
    renaming: bool,
    /// the machines being renamed by it (key -> old computer name, the parent's)
    rename_from: IndexMap<String, String>,
    show: bool,
    wait_s: i64,
    timeout_min: i64,
    state: PathBuf,
    zero_done: PathBuf,
    deadline: Instant,
    vms: Vec<Vm>,
    /// serial-log offset per machine (each wait continues where the last stopped)
    offsets: HashMap<String, u64>,
    qmp_tcp: bool,
}

impl Vm {
    fn has_steps(&self) -> bool {
        !self.steps.is_empty()
    }
    /// The machine's CPU, from its OS (`win11-arm64` is arm64).
    fn arch(&self) -> Arch {
        Arch::parse(&self.os.arch).unwrap_or_default()
    }
}

/// How much longer the fixed waits (logon, power-off) are for an emulated machine: TCG runs
/// a Windows guest of another CPU (x64 on an Apple Silicon Mac) several times slower.
const EMULATED_SLOWDOWN: u32 = 6;

/// The base image name for a machine: `<os>-dc` for a domain controller, `<os>-client` for
/// any other machine (a member server, a client, a workgroup machine). Any OS key works with
/// either role, so e.g. a 2012 DC and a 2019 member use `2012-dc` and `2019-client`.
fn base_name(vm: &Vm) -> String {
    format!("{}-{}", vm.os.key, if vm.role == Role::Dc { "dc" } else { "client" })
}

/// The lab's name if LAB_FILE is a named lab: `<home>/labs/NAME.json`, or a built-in lab's copy
/// `<state_dir>/NAME/NAME.json` (see [`assets::lab_file`]).
fn named_lab(home: &Home, lab_file: &Path) -> Option<String> {
    let stem = util::stem(util::basename(&lab_file.to_string_lossy())).to_string();
    let dir = util::canon(lab_file).parent()?.to_path_buf();
    let builtin = assets::builtin_lab(&stem).is_some() && dir == util::canon(&home.state_dir.join(&stem));
    (dir == util::canon(&home.labs_dir()) || builtin).then_some(stem)
}

/// The name of a lab's state folder (`<state_dir>/<name>/`) and VMs. An explicit `name` in the
/// lab file wins. Otherwise a named lab (`labs/school.json`, or the built-in `school`) is
/// called by its name, and a lab file anywhere else gets its file name plus a short hash of its
/// absolute path (`/proj/wintest/lab.json` -> `lab-3f9a1c2b`), so two projects' `lab.json`
/// never share a 0 state.
pub fn state_name(home: &Home, lab_file: &Path, lab: &Lab) -> String {
    if let Some(n) = &lab.name {
        return n.clone();
    }
    if let Some(n) = named_lab(home, lab_file) {
        return n;
    }
    let stem = util::stem(util::basename(&lab_file.to_string_lossy())).to_string();
    let abs = util::canon(lab_file);
    // FNV-1a: stable across runs and platforms (unlike std's DefaultHasher)
    let mut h: u32 = 0x811c_9dc5;
    for b in abs.to_string_lossy().bytes() {
        h = (h ^ b as u32).wrapping_mul(0x0100_0193);
    }
    let stem: String = stem.chars().map(|c| if c.is_ascii_alphanumeric() || "._-".contains(c) { c } else { '_' }).collect();
    format!("{}-{:08x}", stem, h)
}

/// An environment to act on instead of the lab itself: its name and `env.json`.
pub type EnvTarget = (String, EnvFile);

/// Build a [`MultiLab`] and dispatch CMD; returns the exit code. `lab_file` is the lab JSON;
/// `config`/`run` are set for `run`; `env` is set to act on one of the lab's environments.
pub fn run(
    cmd: &'static str,
    lab_file: &Path,
    config: Option<PathBuf>,
    run: Option<Run>,
    lab: Lab,
    opts: &RunOpts,
    env: Option<EnvTarget>,
) -> i32 {
    exit_code(MultiLab::new(cmd, lab_file, config, run, lab, opts, env).and_then(|mut l| l.dispatch()))
}

/// The exit code of a command's result: an error that is a known exit code carries it (the
/// reason was printed already), any other error is printed and is 2.
pub fn exit_code(r: Result<i32>) -> i32 {
    match r {
        Ok(code) => code,
        Err(e) => match e.downcast_ref::<Exit>() {
            Some(Exit(code)) => *code,
            None => {
                err(&format!("{:#}", e));
                2
            }
        },
    }
}

/// Command-line options that apply to a JSON lab.
#[derive(Default, Clone)]
pub struct RunOpts {
    /// open the VM windows instead of VNC
    pub show: bool,
    /// install missing base images with Windows Update (hours)
    pub updates: bool,
    /// --wait: overrides the run's `wait`
    pub wait: Option<i64>,
    /// --timeout-min: overrides the run's `timeout_min`
    pub timeout_min: Option<i64>,
    /// `status LAB/MACHINE`: only this machine
    pub machine: Option<String>,
}

// --------------------------------------------------------------- environments

/// The environment `name` (`wtl up LAB --new`), with its lab file and its lab (whose LAN port is
/// the environment's own); None if there is no such environment.
pub fn open_env(home: &Home, name: &str) -> Result<Option<(PathBuf, Lab, EnvTarget)>> {
    let Some(f) = envs::load(home, name)? else { return Ok(None) };
    if !f.lab_file.is_file() {
        bail!("environment {}: its lab file {} is gone (wtl rm {} deletes the environment)", name, f.lab_file.display(), name);
    }
    let mut lab = load_lab(&f.lab_file)?;
    if let (Some(n), Some(p)) = (lab.network.as_mut(), f.lan_port) {
        n.lan_port = p;
    }
    Ok(Some((f.lab_file.clone(), lab, (name.to_string(), f))))
}

/// `wtl up LAB --new`, `wtl run --new`: build the lab's 0 state if it has none, then make a new
/// environment of it (named `<lab>-<name>`, or `<lab>-<4 random>`), or of the environment
/// `from` (a copy of a copy is refused). With `renames` (`--rename` items), its machines are
/// renamed right away, which takes a few minutes. Returns it, ready to boot.
pub fn spawn(
    lab_file: &Path,
    lab: Lab,
    opts: &RunOpts,
    from: Option<EnvTarget>,
    name: Option<&str>,
    note: &str,
    renames: &[String],
) -> Result<(PathBuf, Lab, EnvTarget)> {
    envs::check_note(note)?;
    let mut parent = MultiLab::new("prepare", lab_file, None, None, lab, opts, from)?;
    let code = parent.dispatch()?;
    if code != 0 {
        return Err(Exit(code).into());
    }
    let (env_name, f) = parent.spawn_env(name, note, renames)?;
    let home = parent.env.home.clone();
    if f.rename_state.is_some() {
        let (lf, l, t) = open_env(&home, &env_name)?.ok_or_else(|| anyhow!("environment {} was not saved", env_name))?;
        let mut e = MultiLab::new("rename", &lf, None, None, l, opts, Some(t))?;
        let mut arches: Vec<Arch> = e.vms.iter().map(|v| v.arch()).collect();
        arches.dedup();
        e.env.check_tools(&arches)?;
        sys::install_interrupt_handler();
        e.rename_and_save()?;
    }
    open_env(&home, &env_name)?.ok_or_else(|| anyhow!("environment {} was not saved", env_name))
}

/// `wtl rm ENV`: stop an environment's VMs and delete its folder (disks, results). The lab's
/// frozen 0 state goes with the last environment that uses it.
pub fn remove_env(name: &str) -> i32 {
    exit_code((|| {
        let home = Home::resolve()?;
        let f = match envs::load(&home, name)? {
            Some(f) => f,
            None if home.state_dir.join(name).join("0-state.json").is_file() => {
                bail!(
                    "{} is a lab, not an environment: wtl rm deletes only environments (wtl rebuild {} deletes a lab's 0 state)",
                    name,
                    name
                )
            }
            None => bail!("no environment {} (wtl list shows them)", name),
        };
        let copies: Vec<String> = envs::all(&home).into_iter().filter(|(_, e)| e.parent() == name).map(|(n, _)| n).collect();
        if !copies.is_empty() {
            bail!("{} has copies ({}): wtl rm them first", name, copies.join(", "));
        }
        let state = home.state_dir.join(name);
        match open_env(&home, name) {
            Ok(Some((lab_file, lab, target))) => {
                MultiLab::new("restore", &lab_file, None, None, lab, &RunOpts::default(), Some(target))?.restore()?
            }
            Ok(None) => {}
            Err(e) => warn(&format!("{:#}; deleting it without stopping its VMs", e)),
        }
        std::fs::remove_dir_all(&state).with_context(|| format!("cannot delete {}", state.display()))?;
        envs::gc_frozen(&home, &home.state_dir.join(f.parent()));
        ok(&format!("environment {} deleted (lab {} and its other environments untouched)", name, f.lab));
        Ok(0)
    })())
}

/// `wtl run --new --rm`: move the environment's last results into its lab's runs/ (as
/// `<time>-<environment>`), then delete it. Returns the exit code of the deletion.
pub fn keep_results_and_remove(name: &str) -> i32 {
    exit_code((|| {
        let home = Home::resolve()?;
        let f = envs::load(&home, name)?.ok_or_else(|| anyhow!("no environment {}", name))?;
        let state = home.state_dir.join(name);
        let last = state.join("last");
        let res = match std::fs::symlink_metadata(&last) {
            Ok(md) if md.file_type().is_symlink() => Some(util::canon(&last)),
            Ok(_) => read_trim(&last).map(|t| util::canon(&state.join(t))),
            Err(_) => None,
        };
        if let Some(res) = res.filter(|r| r.is_dir()) {
            let runs = home.state_dir.join(&f.lab).join("runs");
            std::fs::create_dir_all(&runs).with_context(|| format!("cannot create {}", runs.display()))?;
            let dst = runs.join(format!("{}-{}", util::basename(&res.to_string_lossy()), name));
            std::fs::rename(&res, &dst).with_context(|| format!("cannot move {} to {}", res.display(), dst.display()))?;
            ok(&format!("results kept: {}", dst.display()));
        }
        Ok(0)
    })())
    .max(remove_env(name))
}

/// `wtl list`: every lab with a 0 state and every environment, with its id, whether it is
/// running, and its note.
pub fn list() -> i32 {
    exit_code((|| {
        let home = Home::resolve()?;
        let running = |state: &Path, machines: &[String]| {
            machines.iter().any(|m| read_trim(&state.join(format!("{}.pid", m))).and_then(|p| p.parse().ok()).is_some_and(sys::pid_alive))
        };
        let envs = envs::all(&home);
        let mut labs: Vec<String> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&home.state_dir) {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().into_owned();
                if e.path().join("0-state.json").is_file() && !envs.iter().any(|(en, _)| *en == n) {
                    labs.push(n);
                }
            }
        }
        labs.sort();
        // environments of a lab without a 0 state of its own (rebuilt and not built since) too
        for (_, f) in &envs {
            if !labs.contains(&f.lab) {
                labs.push(f.lab.clone());
            }
        }
        if labs.is_empty() {
            println!("no labs with a 0 state yet (wtl prepare LAB, or wtl run CONFIG)");
            return Ok(0);
        }
        println!("{:<24} {:<16} {:<9} {:<8} NOTE", "NAME", "FROM", "ID", "STATE");
        for lab in &labs {
            let state = home.state_dir.join(lab);
            let zero = state.join("0-state.json");
            if zero.is_file() {
                let z: ZeroState = std::fs::read_to_string(&zero).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
                let st = if running(&state, &z.machines) { "running" } else { "off" };
                println!("{:<24} {:<16} {:<9} {:<8}", lab, "-", zero_id_at(&zero), st);
            }
            for (name, f) in envs.iter().filter(|(_, f)| &f.lab == lab) {
                let es = home.state_dir.join(name);
                let z: ZeroState =
                    std::fs::read_to_string(es.join("0-state.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
                let st = if running(&es, &z.machines) { "running" } else { "off" };
                println!("{:<24} {:<16} {:<9} {:<8} {}", name, f.lab, f.id, st, f.note);
            }
        }
        Ok(0)
    })())
}

/// The id in a 0-state.json; one built by an older `wtl` gets one now, saved in the file.
/// Empty when there is no such file.
fn zero_id_at(zero_done: &Path) -> String {
    let Ok(text) = std::fs::read_to_string(zero_done) else { return String::new() };
    let mut z: ZeroState = serde_json::from_str(&text).unwrap_or_default();
    if z.id.is_empty() {
        z.id = envs::random_hex(8);
        let _ = write_file(zero_done, pyjson::dumps(&z));
    }
    z.id
}

/// A machine key as the end of a variable name: `CL1` -> `CL1` (`WTL_HOST_CL1`); anything but
/// A-Z 0-9 becomes `_` (`web.1` -> `WEB_1`).
pub fn host_var(key: &str) -> String {
    key.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_uppercase() } else { '_' }).collect()
}

/// Every lab LAN port the home's own and the built-in labs use.
fn lab_ports(home: &Home) -> Vec<u16> {
    let mut out = Vec::new();
    for name in assets::lab_names(home) {
        if let Ok(Some(f)) = assets::lab_file(home, &name) {
            if let Ok(l) = load_lab(&f) {
                out.extend(l.network.map(|n| n.lan_port));
            }
        }
    }
    out
}

impl MultiLab {
    fn new(
        cmd: &'static str,
        lab_file: &Path,
        config: Option<PathBuf>,
        run: Option<Run>,
        lab: Lab,
        opts: &RunOpts,
        target: Option<EnvTarget>,
    ) -> Result<MultiLab> {
        let (show, updates, wait_override, timeout_override) = (opts.show, opts.updates, opts.wait, opts.timeout_min);
        let home = Home::resolve()?;
        let name = match &target {
            Some((n, _)) => n.clone(),
            None => state_name(&home, lab_file, &lab),
        };
        if !util::is_safe_name(&name) {
            bail!("bad lab name {:?} (from {}; set \"name\" in the lab file)", name, lab_file.display());
        }
        let mut vars = qemu::seed_vars(&home);
        // the JSON lab carries its own admin password; keep WTL_PASSWORD only as the fallback
        // Env still needs the WTL_* machinery, so build it from the seeded vars.
        vars.insert("WTL_PASSWORD".into(), lab.admin_password.clone());
        let env = Env::from_vars(&home, &vars)?;
        let wtl_qmp = std::env::var("WTL_QMP").unwrap_or_default();
        let qmp_tcp = match wtl_qmp.as_str() {
            "tcp" => true,
            "unix" => false,
            "" => !cfg!(unix),
            other => bail!("WTL_QMP={} must be tcp or unix", other),
        };

        let oslist = assets::oslist(&home)?;
        let state = home.state_dir.join(&name);
        std::fs::create_dir_all(&state).with_context(|| format!("cannot create {}", state.display()))?;

        let wait_s = wait_override.or_else(|| run.as_ref().map(|r| r.wait)).unwrap_or(60);
        let timeout_min = timeout_override.or_else(|| run.as_ref().map(|r| r.timeout_min)).unwrap_or(120);
        if wait_s <= 0 {
            bail!("--wait must be a positive integer");
        }

        // one Vm per machine, in lab order, with its resolved OS and its steps/mounts
        let mut vms = Vec::new();
        for (i, m) in lab.machines.iter().enumerate() {
            let os = oslist::lookup(&oslist, &m.os)
                .ok_or_else(|| anyhow!("{}: machine {}: unknown os {:?} (see lib/oslist.conf)", lab_file.display(), m.name, m.os))?;
            // VHD-import OSes (Server 2003 / 2008 R2) run standalone only for now: no domain,
            // no roles/features/setup, no lab LAN. The guest scripts refuse these too; checking
            // here fails the lab cleanly before anything boots.
            if os.is_vhd() {
                if lab.mode != labdef::Mode::Workgroup {
                    bail!("{}: machine {}: {} is supported standalone only (not in an AD domain) yet", lab_file.display(), m.name, m.os);
                }
                if lab.network.is_some() {
                    bail!(
                        "{}: machine {}: {} is supported standalone only, so its lab cannot have a \"network\" yet",
                        lab_file.display(),
                        m.name,
                        m.os
                    );
                }
                for (what, empty) in [("roles", m.roles.is_empty()), ("features", m.features.is_empty()), ("setup", m.setup.is_empty())] {
                    if !empty {
                        bail!("{}: machine {}: {} does not support \"{}\" yet (standalone only)", lab_file.display(), m.name, m.os, what);
                    }
                }
            }
            if m.role == Role::Dc && os.kind != "server" {
                bail!("{}: machine {}: os {} is a client OS and cannot be the domain controller", lab_file.display(), m.name, m.os);
            }
            // server roles and Windows features need a server OS (Install-WindowsFeature is not
            // on a client)
            if os.kind != "server" {
                if let Some(r) = m.roles.first() {
                    bail!(
                        "{}: machine {}: the {:?} role needs a server OS, but {} is a client OS",
                        lab_file.display(),
                        m.name,
                        r.as_str(),
                        m.os
                    );
                }
                if let Some(f) = m.features.first() {
                    bail!(
                        "{}: machine {}: feature {:?} needs a server OS (Install-WindowsFeature), but {} is a client OS",
                        lab_file.display(),
                        m.name,
                        f,
                        m.os
                    );
                }
            }
            let idx = 0x10 + i; // MAC/host-id suffix, DC/first = 0x10
            vms.push(Vm {
                name: m.name.clone(),
                hostname: target.as_ref().and_then(|(_, f)| f.hostnames.get(&m.name).cloned()).unwrap_or_else(|| m.hostname.clone()),
                role: m.role,
                os,
                mem: m.mem.to_string(),
                cpus: m.cpus.to_string(),
                ip: m.ip.clone().unwrap_or_default(),
                mac: format!("52:54:00:4c:41:{:02x}", idx),
                logon: m.logon.clone(),
                roles: m.roles.clone(),
                features: m.features.clone(),
                setup: m.setup.clone(),
                ad_cs: m.ad_cs.clone(),
                dhcp: m.dhcp.clone(),
                iis: m.iis.clone(),
                steps: Vec::new(),
                mounts: Vec::new(),
            });
        }
        if let Some(m) = &opts.machine {
            if !vms.iter().any(|v| v.name.eq_ignore_ascii_case(m)) {
                let names: Vec<&str> = vms.iter().map(|v| v.name.as_str()).collect();
                bail!("{} has no machine {} (machines: {})", name, m, names.join(" "));
            }
        }
        // distribute the run's steps to the machines they target (keeping per-machine order)
        if let Some(r) = &run {
            let mut counter: HashMap<String, i64> = HashMap::new();
            for step in &r.steps {
                for mname in &step.on {
                    let n = counter.entry(mname.clone()).or_insert(0);
                    *n += 1;
                    let vm = vms.iter_mut().find(|v| &v.name == mname).expect("resolve checked targets");
                    vm.steps.push((*n, step.clone()));
                }
            }
            // every machine that runs steps gets all of the run's drives: a step's program may
            // read files from other mounted drives (an install script on Y: installing X:\app.msi)
            for vm in &mut vms {
                if !vm.steps.is_empty() {
                    vm.mounts = r.mounts.clone();
                }
            }
        }

        let arg = match &target {
            Some((n, _)) => n.clone(),
            None => named_lab(&home, lab_file).unwrap_or_else(|| util::canon(lab_file).to_string_lossy().into_owned()),
        };
        let now = Instant::now();
        let limit = Duration::from_secs((timeout_min.max(0) as u64).saturating_mul(60));
        Ok(MultiLab {
            zero_done: state.join("0-state.json"),
            env,
            vars,
            updates,
            cmd,
            arg,
            name,
            lab_file: lab_file.to_path_buf(),
            lab,
            config,
            run_vars: run.as_ref().map(|r| r.vars.clone()).unwrap_or_default(),
            envf: target.map(|(_, f)| f),
            only: opts.machine.clone(),
            run_id: String::new(),
            renaming: false,
            rename_from: IndexMap::new(),
            show,
            wait_s,
            timeout_min,
            state,
            deadline: now.checked_add(limit).unwrap_or(now + Duration::from_secs(100 * 365 * 86400)),
            vms,
            offsets: HashMap::new(),
            qmp_tcp,
        })
    }

    fn st(&self, f: &str) -> PathBuf {
        self.state.join(f)
    }
    fn zero_disk(&self, vm: &str) -> PathBuf {
        self.st(&format!("{}-0.qcow2", vm))
    }
    fn run_disk(&self, vm: &str) -> PathBuf {
        self.st(&format!("{}.qcow2", vm))
    }
    fn vm_names(&self) -> Vec<String> {
        self.vms.iter().map(|v| v.name.clone()).collect()
    }
    fn vm(&self, name: &str) -> &Vm {
        self.vms.iter().find(|v| v.name == name).expect("known vm")
    }
    /// 1 for a machine on the hypervisor, [`EMULATED_SLOWDOWN`] for an emulated one.
    fn slow(&self, name: &str) -> u32 {
        if self.env.accel_for(self.vm(name).arch()) == qemu::Accel::Tcg && !self.env.fast {
            EMULATED_SLOWDOWN
        } else {
            1
        }
    }
    /// The DC's name (None in workgroup mode).
    fn dc_name(&self) -> Option<String> {
        self.vms.iter().find(|v| v.role == Role::Dc).map(|v| v.name.clone())
    }
    /// The machines that are not the DC, in lab order.
    fn member_names(&self) -> Vec<String> {
        self.vms.iter().filter(|v| v.role == Role::Member).map(|v| v.name.clone()).collect()
    }
    /// "domain lab.local" or "workgroup", for log lines.
    fn mode_text(&self) -> String {
        if self.lab.is_ad() {
            format!("domain {}", self.lab.domain())
        } else {
            "workgroup".into()
        }
    }

    // ---------------------------------------------------------------- QMP (per machine)

    fn read_pid(&self, vm: &str) -> Option<u32> {
        read_trim(&self.st(&format!("{}.pid", vm)))?.trim().parse().ok()
    }
    fn vm_pid(&self, vm: &str) -> Option<u32> {
        self.read_pid(vm).filter(|p| sys::pid_alive(*p))
    }

    fn qmp_addr(&self, vm: &str) -> Addr {
        if let Some(a) = read_trim(&self.st(&format!("{}.qmp.addr", vm))).filter(|a| !a.is_empty()) {
            return Addr::parse(&a);
        }
        Addr::Unix(sock_path(self.st(&format!("{}.qmp", vm)), &format!("lab-{}-{}", self.name, vm)))
    }

    fn new_qmp_addr(&self, vm: &str) -> Result<Addr> {
        let addr_file = self.st(&format!("{}.qmp.addr", vm));
        if self.qmp_tcp {
            let port = crate::lab::free_port().context("no free TCP port for QMP")?;
            let a = Addr::Tcp(format!("127.0.0.1:{}", port));
            write_file(&addr_file, format!("{}\n", a))?;
            return Ok(a);
        }
        let _ = std::fs::remove_file(&addr_file);
        let a = self.qmp_addr(vm);
        if let Addr::Unix(p) = &a {
            let _ = std::fs::remove_file(p);
        }
        Ok(a)
    }

    fn qmp(&self, vm: &str) -> Option<Qmp> {
        Qmp::connect(&self.qmp_addr(vm), Duration::from_secs(10)).ok()
    }
    fn vm_status(&self, vm: &str) -> String {
        self.qmp(vm).and_then(|mut q| q.status().ok()).unwrap_or_default()
    }
    fn qmp_quit(&self, vm: &str) -> bool {
        self.qmp(vm).is_some_and(|mut q| q.quit().is_ok())
    }
    fn qmp_cmd(&self, vm: &str, name: &str) {
        if let Some(mut q) = self.qmp(vm) {
            let _ = q.cmd(name, None);
        }
    }
    fn screendump(&self, vm: &str, out: &Path) -> bool {
        self.qmp(vm).is_some_and(|mut q| q.screendump(out).is_ok())
    }

    // ---------------------------------------------------------------- waiting

    fn interrupted(&self) -> anyhow::Error {
        eprintln!();
        warn(&format!("interrupted; VMs may still be running (wtl status {} / wtl restore {})", self.arg, self.arg));
        Exit(2).into()
    }
    fn nap(&self, secs: f64) -> Result<()> {
        if sys::nap(secs) {
            Ok(())
        } else {
            Err(self.interrupted())
        }
    }
    fn settle(&self, secs: f64) -> Result<()> {
        if self.env.fast {
            Ok(())
        } else {
            self.nap(secs)
        }
    }
    fn remaining(&self) -> f64 {
        self.deadline.saturating_duration_since(Instant::now()).as_secs_f64()
    }

    // ---------------------------------------------------------------- stop / restore / status

    fn stop_vms(&self, hard: bool) -> Result<()> {
        // members first, DC last (so the DC is still up while members log off)
        let mut order = self.vm_names();
        order.sort_by_key(|n| if self.vm(n).role == Role::Dc { 1 } else { 0 });
        for vm in &order {
            let Some(pid) = self.vm_pid(vm) else {
                let _ = std::fs::remove_file(self.st(&format!("{}.pid", vm)));
                continue;
            };
            if hard {
                if !self.qmp_quit(vm) {
                    sys::terminate(pid);
                }
            } else {
                self.qmp_cmd(vm, "system_powerdown");
            }
        }
        for vm in &order {
            let Some(pid) = self.vm_pid(vm) else {
                let _ = std::fs::remove_file(self.st(&format!("{}.pid", vm)));
                continue;
            };
            let mut i = 0;
            let limit = 900 * self.slow(vm);
            while sys::pid_alive(pid) && i < limit {
                if self.vm_status(vm) == "shutdown" {
                    break;
                }
                self.nap(0.2)?;
                i += 1;
            }
            if sys::pid_alive(pid) {
                if i >= limit {
                    warn(&format!("{} did not power off in {} min, stopping it hard", vm, 3 * self.slow(vm)));
                }
                if !self.qmp_quit(vm) {
                    sys::kill_hard(pid);
                }
                self.nap(1.0)?;
            }
            let _ = std::fs::remove_file(self.st(&format!("{}.pid", vm)));
        }
        Ok(())
    }

    fn restore(&self) -> Result<()> {
        self.stop_vms(true)?;
        for vm in self.vm_names() {
            let _ = std::fs::remove_file(self.run_disk(&vm));
        }
        Ok(())
    }

    fn freeze_off(&self, vm: &str) -> Result<bool> {
        let Some(pid) = self.read_pid(vm) else {
            return Ok(true);
        };
        self.qmp_cmd(vm, "system_powerdown");
        let mut i = 0;
        let (limit, again) = (1800 * self.slow(vm), 300 * self.slow(vm));
        while sys::pid_alive(pid) && i < limit {
            if self.vm_status(vm) == "shutdown" {
                break;
            }
            if i == again {
                self.qmp_cmd(vm, "system_powerdown");
            }
            self.nap(0.2)?;
            i += 1;
        }
        if !sys::pid_alive(pid) {
            return Ok(true);
        }
        if self.vm_status(vm) != "shutdown" && i >= limit {
            return Ok(false);
        }
        if !self.qmp_quit(vm) {
            sys::terminate(pid);
        }
        let mut j = 0;
        while sys::pid_alive(pid) && j < 50 {
            self.nap(0.2)?;
            j += 1;
        }
        Ok(true)
    }

    /// A run on a 0 state whose SYSTEM worker predates step variables: its `system` and
    /// other-user steps run without the run's vars and WTL_ENVIRONMENT... in their environment
    /// (`${...}`, C:\WTL\instance.json and the registry still work).
    fn warn_old_worker(&self) {
        let runs_by_worker = self.vms.iter().any(|v| v.steps.iter().any(|(_, s)| s.as_ != StepAs::Logon));
        if !runs_by_worker {
            return;
        }
        let z: ZeroState = std::fs::read_to_string(&self.zero_done).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        if !z.guest.iter().any(|f| f == "step-env") {
            let fix = match &self.envf {
                Some(f) => format!("wtl rebuild {}, then wtl rebuild {}, fixes it", f.lab, self.name),
                None => format!("wtl rebuild {} fixes it", self.arg),
            };
            warn(&format!(
                "{}'s 0 state was built before step variables: steps run as \"system\" or another user will not see the run's vars or WTL_ENVIRONMENT, WTL_MACHINE... as environment variables (${{...}} in the config, C:\\WTL\\instance.json and the registry still work); {}",
                self.what(),
                fix
            ));
        }
    }

    /// This 0 state's id, made with it. A 0 state built by an older `wtl` gets one now, saved in
    /// its 0-state.json. Empty while there is no 0 state.
    fn zero_id(&self) -> String {
        zero_id_at(&self.zero_done)
    }

    /// Make a new environment from this lab's (or this environment's) 0 state, which must
    /// exist: its own state folder with thin overlays on those 0-state disks, a random id, its own
    /// LAN port, and the computer names it inherits plus `renames` (`--rename` items; renamed by
    /// [`spawn`] right after). A copy of an environment is one level deep only, and renames come
    /// from a lab.
    fn spawn_env(&self, name: Option<&str>, note: &str, renames: &[String]) -> Result<EnvTarget> {
        let home = &self.env.home;
        if let Some(pf) = &self.envf {
            if pf.parent() != pf.lab {
                bail!("{} is a copy of {} already; copy that one (or the lab {}) instead", self.name, pf.parent(), pf.lab);
            }
            if !renames.is_empty() {
                bail!("--rename makes new names from a lab: wtl up {} --new --rename ...", pf.lab);
            }
        }
        let free = |n: &str| -> Result<bool> { Ok(!home.state_dir.join(n).exists() && assets::lab_file(home, n)?.is_none()) };
        let env_name = match name {
            Some(n) => {
                let e = format!("{}-{}", self.name, n);
                if !util::is_safe_name(n) || !util::is_safe_name(&e) {
                    bail!("--name {:?} must be A-Z a-z 0-9 . _ -", n);
                }
                if !free(&e)? {
                    bail!("{} already exists (wtl list; wtl rm {} first, or pick another --name)", e, e);
                }
                e
            }
            None => {
                let mut pick = None;
                for _ in 0..50 {
                    let e = format!("{}-{}", self.name, envs::random_suffix());
                    if free(&e)? {
                        pick = Some(e);
                        break;
                    }
                }
                pick.ok_or_else(|| anyhow!("no free environment name for {}", self.name))?
            }
        };
        // the names first: a bad --rename must fail before anything is made
        let current: Vec<(String, String)> = self.vms.iter().map(|v| (v.name.clone(), v.hostname.clone())).collect();
        let suffix = env_name.strip_prefix(&format!("{}-", self.name)).unwrap_or(&env_name);
        let renamed = envs::plan_renames(&self.name, &current, suffix, renames)?;
        if !renames.is_empty() && renamed.is_empty() {
            bail!("--rename {}: nothing to rename (every name is already the machine's)", renames.join(","));
        }
        let state = home.state_dir.join(&env_name);
        std::fs::create_dir_all(&state).with_context(|| format!("cannot create {}", state.display()))?;
        let made = (|| {
            let (zero_id, frozen) = self.fill_env(&state)?;
            let lan_port = match &self.lab.network {
                Some(_) => Some(envs::pick_lan_port(home, &lab_ports(home))?),
                None => None,
            };
            let mut hostnames = self.envf.as_ref().map(|pf| pf.hostnames.clone()).unwrap_or_default();
            hostnames.extend(renamed.clone());
            let f = EnvFile {
                lab: self.envf.as_ref().map(|pf| pf.lab.clone()).unwrap_or_else(|| self.name.clone()),
                lab_file: util::canon(&self.lab_file),
                id: envs::random_hex(8),
                note: note.to_string(),
                created_at: chrono::Local::now().format("%Y-%m-%d %H:%M").to_string(),
                zero_id,
                frozen,
                lan_port,
                parent: self.name.clone(),
                hostnames,
                rename_state: (!renamed.is_empty()).then(|| "pending".to_string()),
                renamed,
            };
            envs::save(&state, &f)?;
            self.write_env_zero(&state, &env_name, &f.id, f.rename_state.is_some())?;
            Ok::<_, anyhow::Error>(f)
        })();
        match made {
            Ok(f) => {
                let what = if self.envf.is_some() { "environment" } else { "lab" };
                ok(&format!("environment {} (id {}) made from {} {}'s 0 state", env_name, f.id, what, self.name));
                Ok((env_name, f))
            }
            Err(e) => {
                let _ = std::fs::remove_dir_all(&state);
                Err(e)
            }
        }
    }

    /// Point an environment's state folder at this lab's current 0 state: hard-link the lab's
    /// 0-state disks into `frozen/<0-state id>/` (once per 0 state), make the environment's
    /// 0-state disks thin overlays on those, and copy each UEFI machine's variables (its boot
    /// entries). Returns the 0 state's id and the frozen folder.
    fn fill_env(&self, env_state: &Path) -> Result<(String, PathBuf)> {
        if !self.zero_done.is_file() {
            bail!("lab {} has no 0 state (wtl prepare {})", self.name, self.arg);
        }
        let zero_id = self.zero_id();
        let frozen = self.state.join("frozen").join(&zero_id);
        std::fs::create_dir_all(&frozen).with_context(|| format!("cannot create {}", frozen.display()))?;
        let frozen = util::canon(&frozen);
        for vm in self.vm_names() {
            let link = frozen.join(format!("{}-0.qcow2", vm));
            if !link.is_file() {
                std::fs::hard_link(self.zero_disk(&vm), &link)
                    .with_context(|| format!("cannot link {} to {}", self.zero_disk(&vm).display(), link.display()))?;
            }
            let disk = env_state.join(format!("{}-0.qcow2", vm));
            let _ = std::fs::remove_file(&disk);
            self.qemu_img_create(&link, &disk)?;
            for fw in ["OVMF_VARS", "AAVMF_VARS"] {
                let src = self.st(&format!("{}-{}.fd", vm, fw));
                if src.is_file() {
                    let dst = env_state.join(format!("{}-{}.fd", vm, fw));
                    std::fs::copy(&src, &dst).with_context(|| format!("cannot copy {} to {}", src.display(), dst.display()))?;
                }
            }
        }
        Ok((zero_id, frozen))
    }

    /// An environment's own 0-state.json: the lab's (when built, which machines, what its guest
    /// scripts can do) with the environment's name and id.
    /// With `pending` (its machines are still to be renamed) it is written as
    /// `0-state.json.pending`, made the 0-state.json once the rename is done.
    fn write_env_zero(&self, env_state: &Path, env_name: &str, id: &str, pending: bool) -> Result<()> {
        let z: ZeroState = std::fs::read_to_string(&self.zero_done).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        let ez = ZeroState { lab: env_name.to_string(), built_at: z.built_at, machines: z.machines, guest: z.guest, id: id.to_string() };
        let _ = std::fs::remove_file(env_state.join("0-state.json"));
        let _ = std::fs::remove_file(env_state.join("0-state.json.pending"));
        write_file(&env_state.join(if pending { "0-state.json.pending" } else { "0-state.json" }), pyjson::dumps(&ez))
    }

    /// `wtl rebuild ENV`: this environment fresh again from its parent's current 0 state (the
    /// lab's, or the environment it is a copy of), keeping its name, id, note and LAN port, and
    /// doing its own renames again.
    fn refresh_env(&mut self) -> Result<()> {
        let Some(mut f) = self.envf.clone() else { bail!("not an environment") };
        self.restore()?;
        let parent = match open_env(&self.env.home, f.parent())? {
            Some((lf, l, t)) => MultiLab::new("prepare", &lf, None, None, l, &RunOpts::default(), Some(t))?,
            None => {
                let lab = load_lab(&f.lab_file)?;
                MultiLab::new("prepare", &f.lab_file, None, None, lab, &RunOpts::default(), None)?
            }
        };
        let (zero_id, frozen) = parent.fill_env(&self.state)?;
        f.zero_id = zero_id;
        f.frozen = frozen;
        f.hostnames = parent.envf.as_ref().map(|pf| pf.hostnames.clone()).unwrap_or_default();
        f.hostnames.extend(f.renamed.clone());
        f.rename_state = (!f.renamed.is_empty()).then(|| "pending".to_string());
        envs::save(&self.state, &f)?;
        parent.write_env_zero(&self.state, &self.name, &f.id, f.rename_state.is_some())?;
        envs::gc_frozen(&self.env.home, &parent.state);
        self.envf = Some(f.clone());
        if !f.renamed.is_empty() {
            self.rename_and_save()?;
        }
        let built =
            std::fs::read_to_string(&self.zero_done).ok().and_then(|t| serde_json::from_str::<ZeroState>(&t).ok()).unwrap_or_default();
        ok(&format!("environment {} is fresh again from {}'s 0 state (built {})", self.name, f.parent(), built.built_at));
        Ok(())
    }

    /// Rename this environment's machines ([`rename_machines`](Self::rename_machines)) and
    /// record the outcome: "done" makes the pending 0-state.json its 0 state; "failed" leaves it
    /// usable only for `wtl rebuild` / `wtl rm`.
    fn rename_and_save(&mut self) -> Result<()> {
        let r = self.rename_machines();
        let Some(mut f) = self.envf.clone() else { bail!("not an environment") };
        f.rename_state = Some(if r.is_ok() { "done" } else { "failed" }.to_string());
        envs::save(&self.state, &f)?;
        self.envf = Some(f);
        match r {
            Ok(()) => {
                std::fs::rename(self.st("0-state.json.pending"), &self.zero_done).context("saving the renamed 0 state")?;
                Ok(())
            }
            Err(e) => {
                // the reason is printed already (or follows); say what to do with the environment
                err(&format!(
                    "renaming the machines of environment {} failed (its VMs keep running to look at; wtl rebuild {} tries again, wtl rm {} deletes it)",
                    self.name, self.name, self.name
                ));
                Err(e)
            }
        }
    }

    /// The boot that renames this environment's machines (`renamed` in its env.json), written
    /// straight into its 0-state disks, like a build. In tiers, each finished before the next
    /// boots: the DC (members can only rename through a DC that has its new name), then the CA
    /// hosts (members get their certificates from the CA under its new name), then the rest. A
    /// machine with a rename block reports LAB_RENAMED; the DC then waits until the renamed
    /// members registered their new names and deletes their old DNS records (LAB_DNS_CLEAN).
    /// Then every machine is frozen, as after a build.
    fn rename_machines(&mut self) -> Result<()> {
        let Some(f) = self.envf.clone() else { bail!("not an environment") };
        self.rename_from =
            f.renamed.keys().map(|k| (k.clone(), self.lab.machine(k).map(|m| m.hostname.clone()).unwrap_or_else(|| k.clone()))).collect();
        let plan: Vec<String> = f.renamed.iter().map(|(k, n)| format!("{} -> {}", self.rename_from[k], n)).collect();
        log(&format!("=== renaming the machines of environment {}: {} ===", self.name, plan.join(", ")));
        for (k, _) in &f.renamed {
            if self.vm(k).ad_cs.is_some() {
                log(&format!(
                    "{} hosts the CA {}: it is reinstalled under its new name, and the members get new certificates",
                    k,
                    self.vm(k).ad_cs.as_ref().map(|o| o.ca_name.as_str()).unwrap_or("")
                ));
            }
        }
        self.renaming = true;
        let r = self.rename_boot();
        self.renaming = false;
        r
    }

    fn rename_boot(&mut self) -> Result<()> {
        self.restore()?;
        self.offsets.clear();
        let names = self.vm_names();
        let tier = |l: &MultiLab, n: &str| match (l.vm(n).role, l.vm(n).ad_cs.is_some()) {
            (Role::Dc, _) => 0,
            (_, true) => 1,
            _ => 2,
        };
        let mut first = true;
        for t in 0..3 {
            let group: Vec<String> = names.iter().filter(|n| tier(self, n) == t).cloned().collect();
            for vm in &group {
                self.boot_vm(vm, false, first)?;
                first = false;
            }
            for vm in &group {
                let renamed = self.rename_block(self.vm(vm)).is_some();
                if renamed {
                    // its reboots ride along: LAB_RENAMED comes from the logon after the last one
                    let line = self.wait_ev(vm, "LAB_RENAMED", "LAB_FAIL,ERROR", &format!("{} rename", vm))?;
                    ok(&format!("{}: {}", vm, line));
                }
                if self.vm(vm).role == Role::Dc {
                    self.wait_ev(vm, "LAB_DC_READY", "LAB_FAIL,ERROR", "domain controller")?;
                } else {
                    // a machine that just boots: its logon, with the build's reset for a stuck one
                    if !renamed {
                        self.wait_logon(vm, &format!("{} logon", vm))?;
                    }
                    self.wait_ev(vm, "LAB_CLIENT_READY", "LAB_FAIL,ERROR", vm)?;
                    self.wait_ev(vm, "LAB_IDLE", "LAB_FAIL,ERROR", vm)?;
                }
            }
        }
        // the DC deletes the old DNS records (the renamed members', its own) once all are renamed
        if let Some(dc) = self.dc_name() {
            if self.rename_block(self.vm(&dc)).is_some_and(|r| !r.peers.is_empty() || r.from != r.to) {
                let line = self.wait_ev(&dc, "LAB_DNS_CLEAN", "LAB_FAIL,ERROR", "DNS cleanup")?;
                ok(&format!("{}: {}", dc, line));
            }
        }
        self.freeze_lab()?;
        ok(&format!("environment {}: machines renamed and saved", self.name));
        Ok(())
    }

    /// "lab school" or "environment school-k7q3", for messages.
    fn what(&self) -> String {
        format!("{} {}", if self.envf.is_some() { "environment" } else { "lab" }, self.name)
    }

    /// Who this lab or environment is, for the machines inside it and for the results.
    fn instance(&self) -> Instance {
        let hostnames: IndexMap<String, String> = self.vms.iter().map(|v| (v.name.clone(), v.hostname.clone())).collect();
        match &self.envf {
            Some(e) => Instance { lab: e.lab.clone(), environment: self.name.clone(), id: e.id.clone(), note: e.note.clone(), hostnames },
            None => Instance { lab: self.name.clone(), environment: self.name.clone(), id: self.zero_id(), note: String::new(), hostnames },
        }
    }

    fn status(&self) {
        let inst = self.instance();
        match &self.envf {
            Some(f) => {
                println!(
                    "environment {} (id {}) of lab {} ({}): {}, {} machines",
                    self.name,
                    f.id,
                    f.lab,
                    self.lab_file.display(),
                    self.mode_text(),
                    self.vms.len()
                );
                if !f.note.is_empty() {
                    println!("  note: {}", f.note);
                }
                println!("  made {}{}", f.created_at, f.lan_port.map(|p| format!(", lab LAN port {}", p)).unwrap_or_default());
                if f.parent() != f.lab {
                    println!("  a copy of environment {}", f.parent());
                }
                if let Some(st) = &f.rename_state {
                    let r: Vec<String> = f.renamed.iter().map(|(k, n)| format!("{} -> {}", k, n)).collect();
                    println!("  renamed ({}): {}", st, r.join(", "));
                }
            }
            None => {
                let id = if inst.id.is_empty() { String::new() } else { format!(" (id {})", inst.id) };
                println!("lab {}{} ({}): {}, {} machines", self.name, id, self.lab_file.display(), self.mode_text(), self.vms.len());
            }
        }
        if self.zero_done.is_file() {
            let z: ZeroState =
                std::fs::read_to_string(&self.zero_done).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
            println!("  0 state: ready ({})", z.built_at);
        } else {
            println!("  0 state: not built yet (wtl prepare {}, or just wtl run ...)", self.arg);
        }
        for v in self.vms.iter().filter(|v| self.only.as_ref().is_none_or(|m| m.eq_ignore_ascii_case(&v.name))) {
            let role = match (v.role, self.lab.is_ad()) {
                (Role::Dc, _) => "dc",
                (Role::Member, true) => "member",
                (Role::Member, false) => "single",
            };
            let pid = match self.vm_pid(&v.name) {
                Some(p) => format!("running (pid {}) {}", p, read_trim(&self.st(&format!("{}.vnc", v.name))).unwrap_or_default()),
                None => "off".into(),
            };
            let ip = if v.ip.is_empty() { "-" } else { v.ip.as_str() };
            let host = if v.hostname != v.name { format!("  hostname {}", v.hostname) } else { String::new() };
            println!("  {:<15} {:<6} {:<7} {} {}{}", v.name, role, v.os.key, ip, pid, host);
        }
        if let Some(last) = self.last_run() {
            println!("  last run: {}", last.display());
        }
        if self.envf.is_none() {
            let envs = envs::of_lab(&self.env.home, &self.name);
            if !envs.is_empty() {
                let names: Vec<String> = envs.iter().map(|(n, f)| format!("{} ({})", n, f.id)).collect();
                println!("  environments: {}", names.join(", "));
            }
        }
    }

    fn last_run(&self) -> Option<PathBuf> {
        let p = self.st("last");
        let md = std::fs::symlink_metadata(&p).ok()?;
        if md.file_type().is_symlink() {
            return Some(util::canon(&p));
        }
        read_trim(&p).map(|t| util::canon(&self.state.join(t)))
    }

    fn set_last_run(&self, rel: &str) {
        let p = self.st("last");
        let _ = std::fs::remove_file(&p);
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(rel, &p).is_ok();
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_dir(rel, &p).is_ok();
        #[cfg(not(any(unix, windows)))]
        let made = false;
        if !made {
            let _ = std::fs::write(&p, format!("{}\n", rel));
        }
    }

    fn dispatch(&mut self) -> Result<i32> {
        if let Some(f) = &self.envf {
            if !f.usable() && !matches!(self.cmd, "status" | "rebuild" | "restore" | "stop") {
                bail!(
                    "environment {}'s rename {} (wtl rebuild {} renames it again, wtl rm {} deletes it)",
                    self.name,
                    if f.rename_state.as_deref() == Some("failed") { "failed" } else { "did not finish" },
                    self.name,
                    self.name
                );
            }
        }
        match self.cmd {
            "stop" => {
                self.stop_vms(false)?;
                ok(&format!("{} is off", self.what()));
                return Ok(0);
            }
            "restore" => {
                self.restore()?;
                ok(&format!("{} is back at its 0 state (VMs off; next run/up starts clean)", self.what()));
                return Ok(0);
            }
            "status" => {
                self.status();
                return Ok(0);
            }
            "rebuild" if self.envf.is_some() => {
                self.refresh_env()?;
                return Ok(0);
            }
            "rebuild" => {
                self.restore()?;
                for vm in self.vm_names() {
                    let _ = std::fs::remove_file(self.zero_disk(&vm));
                }
                let _ = std::fs::remove_file(&self.zero_done);
                // the environments' frozen copies of this 0 state stay (hard links), the rest go
                envs::gc_frozen(&self.env.home, &self.state);
                let kept = envs::of_lab(&self.env.home, &self.name).len();
                let also =
                    if kept > 0 { format!("; its {} environment(s) keep the 0 state they were made from", kept) } else { String::new() };
                ok(&format!("lab {}: 0 state deleted; the next run builds a new domain (Windows installs kept){}", self.name, also));
                return Ok(0);
            }
            _ => {}
        }
        let mut arches: Vec<Arch> = self.vms.iter().map(|v| v.arch()).collect();
        arches.dedup();
        self.env.check_tools(&arches)?;
        sys::install_interrupt_handler();
        self.run_lab()
    }

    // ---------------------------------------------------------------- discs

    /// The Windows install a machine boots from: images/<os>-dc.qcow2 for the DC,
    /// images/<os>-client.qcow2 for a member. Machines with the same OS and role share one.
    fn base_image(&self, vm: &Vm) -> PathBuf {
        self.env.images_dir.join(format!("{}.qcow2", base_name(vm)))
    }

    /// Install Windows for every missing base image (`wtl build-image`, in parallel; one build
    /// per image even when several machines share it).
    fn build_bases(&self) -> Result<()> {
        let mut builds: Vec<BaseBuild> = Vec::new();
        for vm in &self.vms {
            let image = base_name(vm);
            if self.base_image(vm).is_file() || builds.iter().any(|b| b.image == image) {
                continue;
            }
            // the computer name baked into the image; the lab renames each machine to its key
            let prefix = if vm.role == Role::Dc { "DC" } else { "CL" };
            builds.push(BaseBuild::new(&image, &vm.os.key, &image, &format!("{}{}", prefix, vm.os.key).to_uppercase()));
        }
        build_base_images(&self.vars, &self.state, &builds, self.updates, self.show, &|s| self.nap(s))?;
        let missing: Vec<String> =
            self.vms.iter().map(|v| self.base_image(v)).filter(|b| !b.is_file()).map(|b| b.display().to_string()).collect();
        if !missing.is_empty() {
            bail!("Windows install failed, missing {} (logs: {}/build-*.log)", missing.join(", "), self.state.display());
        }
        Ok(())
    }

    /// One read-only ISO per drive letter a machine mounts, label WTL<LETTER>.
    fn make_mount_isos(&self, vm: &Vm) -> Result<Vec<PathBuf>> {
        let mut isos = Vec::new();
        for (letter, host) in &vm.mounts {
            let src = PathBuf::from(host);
            if !src.is_dir() {
                bail!("mount source is not a directory: {} (drive {}: on {})", host, letter, vm.name);
            }
            let iso = self.st(&format!("mount-{}-{}.iso", vm.name, letter));
            mkiso::make_iso(&src, &iso, &format!("WTL{}", letter)).with_context(|| format!("mkiso drive {}: for {}", letter, vm.name))?;
            isos.push(iso);
            log(&format!("  {} mount {}: <- {} ({})", vm.name, letter, host, util::human_size(util::disk_usage(&src))));
        }
        Ok(isos)
    }

    /// The control disc for one machine: LAB\lab.ps1 + LAB\labsvc.ps1 + LAB\lab.json.
    /// A machine's part of the rename boot (`rename_machines`), or None: a renamed machine gets
    /// its rename; when a CA is reinstalled, every other member re-requests its certificates
    /// under the new CA (a block with `from` = `to`).
    fn rename_block<'a>(&'a self, vm: &'a Vm) -> Option<LabRename<'a>> {
        if !self.renaming {
            return None;
        }
        let autoenroll = self.vms.iter().any(|v| v.ad_cs.as_ref().is_some_and(|o| o.autoenroll));
        let ca_reinstalled = self.vms.iter().any(|v| v.ad_cs.is_some() && self.rename_from.contains_key(&v.name));
        let members_renamed = self.vms.iter().any(|v| v.role != Role::Dc && self.rename_from.contains_key(&v.name));
        let from = match self.rename_from.get(&vm.name) {
            Some(old) => old.as_str(),
            // a member that only gets certificates from the reinstalled CA
            None if ca_reinstalled && autoenroll && vm.role != Role::Dc && vm.ad_cs.is_none() => vm.hostname.as_str(),
            // the DC deletes the renamed members' old DNS records, renamed itself or not
            None if vm.role == Role::Dc && members_renamed => vm.hostname.as_str(),
            None => return None,
        };
        let renamed = from != vm.hostname;
        let peers = if vm.role == Role::Dc {
            self.vms
                .iter()
                .filter(|v| v.role != Role::Dc)
                .filter_map(|v| self.rename_from.get(&v.name).map(|old| (old.as_str(), v.hostname.as_str())))
                .collect()
        } else {
            IndexMap::new()
        };
        let ca_name = self.vms.iter().find_map(|v| v.ad_cs.as_ref().map(|o| o.ca_name.as_str())).unwrap_or("");
        Some(LabRename {
            from,
            to: &vm.hostname,
            ca: renamed && vm.ad_cs.is_some(),
            ca_name,
            admin: renamed && vm.role != Role::Dc && (vm.ad_cs.is_some() || vm.dhcp.is_some()),
            dhcp: renamed && vm.dhcp.is_some(),
            iis_site_path: if renamed { vm.iis.as_ref().map(|o| o.site_path.as_str()).unwrap_or("") } else { "" },
            reenroll: autoenroll && vm.role != Role::Dc && vm.ad_cs.is_none() && (renamed || ca_reinstalled),
            reenroll_user: autoenroll && ca_reinstalled && vm.role != Role::Dc && vm.ad_cs.is_none(),
            peers,
        })
    }

    fn make_cd(&self, vm: &Vm, out: &Path, build: bool) -> Result<()> {
        let stage = self.st(&format!("cd-{}", vm.name));
        let _ = std::fs::remove_dir_all(&stage);
        let labdir = stage.join("LAB");
        std::fs::create_dir_all(&labdir).with_context(|| format!("cannot create {}", labdir.display()))?;
        // The control disc carries this machine's lab / labsvc / agent scripts (agent too: the
        // base image's baked copy may be older than this program's). An OS with its own guest
        // folder gets those - PowerShell for 2008 R2, VBScript for 2003 - so the file names and
        // language vary by OS; every other OS gets the shared lab.ps1 / labsvc.ps1 / agent.ps1.
        let control = assets::control_scripts_for(&self.env.home, &vm.os.key)?;
        if control.is_empty() {
            bail!("no control-disc scripts (agent/lab/labsvc) for os {}", vm.os.key);
        }
        for (name, data) in control {
            write_file(&labdir.join(&name), data)?;
        }
        let role = if vm.role == Role::Dc { "dc" } else { "member" };
        let (logon_user, logon_password) = match &vm.logon {
            Some(u) => (u.as_str(), self.lab.users.iter().find(|x| &x.name == u).map(|x| x.password.as_str()).unwrap_or("")),
            None => ("", ""),
        };
        let users: Vec<LabUser> = self
            .lab
            .users
            .iter()
            .map(|u| LabUser { name: &u.name, password: &u.password, groups: u.groups.iter().map(String::as_str).collect() })
            .collect();

        // who this machine is: on every disc but the build's and the rename's (a 0 state stays
        // anonymous)
        let inst = if build || self.renaming { None } else { Some(self.instance()) };
        let rename = self.rename_block(vm);
        // the run block (only on a real run, never during the build or `up`)
        let run = if !build && self.cmd == "run" && vm.has_steps() {
            let mounts = vm.mounts.iter().map(|(l, h)| LabMount { host: h, letter: l }).collect();
            let steps = vm
                .steps
                .iter()
                .map(|(n, s)| {
                    let (as_, user, password) = self.step_identity(vm, s);
                    LabStep {
                        n: *n,
                        as_,
                        user,
                        password,
                        path: &s.run,
                        args: s.args.as_deref().unwrap_or(""),
                        keep_running: s.keep_running,
                        wait: s.wait.unwrap_or(self.wait_s),
                    }
                })
                .collect();
            let mut env: IndexMap<String, String> = self.run_vars.iter().map(|v| (v.name.clone(), v.value.clone())).collect();
            if let Some(i) = &inst {
                env.insert("WTL_ENVIRONMENT".into(), i.environment.clone());
                env.insert("WTL_ENVIRONMENT_ID".into(), i.id.clone());
                env.insert("WTL_MACHINE".into(), vm.name.clone());
                env.insert("WTL_RUN_ID".into(), self.run_id.clone());
                env.insert("WTL_HOSTNAME".into(), vm.hostname.clone());
                for v in &self.vms {
                    env.insert(format!("WTL_HOST_{}", host_var(&v.name)), v.hostname.clone());
                }
            }
            Some(LabRun { mounts, steps, wait: self.wait_s, env })
        } else {
            None
        };

        let dc_ip = self.dc_name().map(|d| self.vm(&d).ip.clone()).unwrap_or_default();
        // roles/features/options are baked into the 0 state, so they only matter on the build
        // disc; the guest acts on them in phase "new". Keep them off the run disc.
        let roles: Vec<&str> = if build { vm.roles.iter().map(|r| r.as_str()).collect() } else { Vec::new() };
        let features: Vec<&str> = if build { vm.features.iter().map(String::as_str).collect() } else { Vec::new() };
        // the CA's options: on the build disc, and on the rename disc of a CA host it reinstalls
        let ad_cs = if build || rename.as_ref().is_some_and(|r| r.ca) {
            vm.ad_cs.as_ref().map(|o| LabAdCs {
                ca_name: &o.ca_name,
                key_length: o.key_length,
                validity_years: o.validity_years,
                autoenroll: o.autoenroll,
            })
        } else {
            None
        };
        let subnet = self.lab.network.as_ref().map(|n| n.subnet.clone()).unwrap_or_default();
        let dhcp = if build {
            vm.dhcp.as_ref().map(|o| LabDhcp {
                start: format!("{}.{}", subnet, o.start),
                end: format!("{}.{}", subnet, o.end),
                mask: "255.255.255.0".into(),
                router: dc_ip.clone(),
            })
        } else {
            None
        };
        let iis = if build { vm.iis.as_ref().map(|o| LabIis { site_path: &o.site_path }) } else { None };
        // setup scripts: staged under LAB\setup\ on the build disc, with their identity resolved
        let setup = if build { self.stage_setup(vm, &labdir)? } else { Vec::new() };

        let j = LabJson {
            fmt: "multi",
            mode: if self.lab.is_ad() { "domain" } else { "workgroup" },
            role,
            machine: &vm.name,
            hostname: &vm.hostname,
            lan: self.lab.network.is_some(),
            ip: &vm.ip,
            prefix: 24,
            dc_ip: &dc_ip,
            lan_mac: &vm.mac,
            domain: self.lab.domain(),
            netbios: self.lab.netbios(),
            admin_password: &self.lab.admin_password,
            users,
            logon_user,
            logon_password,
            shutdown_when_ready: (build || self.renaming) && vm.role != Role::Dc,
            slow: self.slow(&vm.name),
            roles,
            features,
            ad_cs,
            dhcp,
            iis,
            setup,
            run,
            instance: inst.map(|i| {
                // Server 2003 is 32-bit only: no 32-bit registry view to fill as well
                let (json, reg) = envs::instance_files(&i, &vm.name, &self.run_id, vm.os.key != "2003");
                LabInstance { environment: i.environment, id: i.id, json, reg }
            }),
            rename,
        };
        write_file(&labdir.join("lab.json"), pyjson::dumps_indent(&j, 1))?;
        mkiso::make_iso(&stage, out, "WTL_LAB").with_context(|| format!("mkiso {} failed", vm.name))?;
        let _ = std::fs::remove_dir_all(&stage);
        Ok(())
    }

    /// Copy a machine's setup scripts (paths relative to the lab file) into the staged control
    /// disc under `LAB\<run>` and return the guest-side descriptors (`file`, args, identity).
    fn stage_setup<'a>(&'a self, vm: &'a Vm, labdir: &Path) -> Result<Vec<LabSetup<'a>>> {
        let lab_dir = util::abspath(&self.lab_file).parent().map(Path::to_path_buf).unwrap_or_default();
        let mut out = Vec::new();
        for s in &vm.setup {
            let src = lab_dir.join(&s.run);
            if !src.is_file() {
                bail!("{}: setup script {} not found ({})", vm.name, s.run, src.display());
            }
            let dst = labdir.join(&s.run);
            if let Some(p) = dst.parent() {
                std::fs::create_dir_all(p).with_context(|| format!("cannot create {}", p.display()))?;
            }
            std::fs::copy(&src, &dst).with_context(|| format!("cannot copy setup script {}", src.display()))?;
            let (as_, user, password) = match &s.as_ {
                StepAs::System | StepAs::Logon => ("system", "", ""),
                StepAs::User(u) => {
                    let pw = self.lab.users.iter().find(|x| &x.name == u).map(|x| x.password.as_str()).unwrap_or("");
                    ("user", u.as_str(), pw)
                }
            };
            out.push(LabSetup { file: &s.run, args: s.args.as_deref().unwrap_or(""), as_, user, password });
        }
        Ok(out)
    }

    /// How a step runs on a machine: ("system"|"logon"|"user", user, password). A user that
    /// equals the machine's own logon user is "logon" (its foreground session); any other is
    /// "user", launched by the SYSTEM worker with that account's credentials.
    fn step_identity<'a>(&'a self, vm: &'a Vm, s: &'a RunStep) -> (&'a str, &'a str, &'a str) {
        match &s.as_ {
            StepAs::System => ("system", "", ""),
            StepAs::Logon => ("logon", "", ""),
            StepAs::User(u) => {
                if vm.logon.as_deref() == Some(u.as_str()) {
                    ("logon", "", "")
                } else {
                    let pw = self.lab.users.iter().find(|x| &x.name == u).map(|x| x.password.as_str()).unwrap_or("");
                    ("user", u.as_str(), pw)
                }
            }
        }
    }

    fn uefi_vars(&self, vm: &Vm) -> Result<Option<PathBuf>> {
        if vm.os.firmware != "uefi" && vm.arch() == Arch::X64 {
            return Ok(None);
        }
        let (name, tmpl, var) = match vm.arch() {
            Arch::X64 => ("OVMF_VARS", &self.env.ovmf_vars, "WTL_OVMF_VARS"),
            Arch::Arm64 => ("AAVMF_VARS", &self.env.aavmf_vars, "WTL_AAVMF_VARS"),
        };
        let vars = self.st(&format!("{}-{}.fd", vm.name, name));
        if !vars.is_file() {
            // the UEFI variables (boot order) live in the base image's copy: the install
            // registered the Windows Boot Manager there
            let base = self.base_image(vm).with_extension("vars.fd");
            let src = if base.is_file() { &base } else { tmpl };
            std::fs::copy(src, &vars).with_context(|| {
                format!("cannot copy the UEFI vars template {} (install QEMU's UEFI firmware or set {})", src.display(), var)
            })?;
        }
        Ok(Some(vars))
    }

    // ---------------------------------------------------------------- boot

    fn qemu_img_create(&self, backing: &Path, out: &Path) -> Result<()> {
        use std::process::{Command, Stdio};
        let st = Command::new(&self.env.qemu_img)
            .args(["create", "-f", "qcow2", "-F", "qcow2", "-b"])
            .arg(backing)
            .arg(out)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .status();
        match st {
            Ok(s) if s.success() => Ok(()),
            _ => bail!("qemu-img create {} failed", out.display()),
        }
    }

    fn start_vm(&self, vm: &Vm, disk: PathBuf, cds: Vec<PathBuf>) -> Result<()> {
        // an imported Hyper-V VHD boots from the legacy IDE controller: four slots in all, the
        // system disk plus at most three CD-ROMs (the control disc and one per mounted drive)
        if vm.os.is_vhd() && cds.len() > 3 {
            bail!(
                "{}: {} boots from IDE, which has room for the control disc and at most two mounted drives ({} discs needed)",
                vm.name,
                vm.os.display,
                cds.len()
            );
        }
        let mut o = Opts {
            name: format!("lab-{}-{}", self.name, vm.name),
            mem: vm.mem.clone(),
            cpus: Some(vm.cpus.clone()),
            disk,
            cds,
            // the private lab LAN (none for a single workgroup machine)
            lan: self.lab.network.as_ref().map(|n| Lan { group: LAN_GROUP.into(), port: n.lan_port }),
            lan_mac: self.lab.network.as_ref().map(|_| vm.mac.clone()),
            // the DC is isolated (no internet); every other machine keeps user-mode NAT
            no_usernet: vm.role == Role::Dc,
            no_shutdown: true,
            // imported Hyper-V VHDs (Server 2003 / 2008 R2) boot from IDE, not AHCI
            disk_ide: vm.os.is_vhd(),
            no_usb: vm.os.no_usb(),
            uefi: self.uefi_vars(vm)?,
            arch: vm.arch(),
            ..Default::default()
        };
        o.qmp = Some(self.new_qmp_addr(&vm.name)?);
        let serial = self.st(&format!("{}.serial.log", vm.name));
        write_file(&serial, b"")?;
        o.serial = Some(serial);
        if self.show {
            o.show = true;
        } else {
            o.vnc = Some(pick_vnc_display().context("no free VNC display (ports 5900-5999 all in use)")?);
        }
        let argv = qemu::command(&self.env, &o)?;
        let pid = qemu::start(&argv, &self.st(&format!("{}.qemu.log", vm.name)))
            .with_context(|| format!("qemu ({}) failed to start", vm.name))?;
        write_file(&self.st(&format!("{}.pid", vm.name)), format!("{}\n", pid))?;
        let vnc = match (o.vnc, &o.qmp) {
            (Some(n), Some(qmp)) => format!("VNC 127.0.0.1:{}", 5900 + bound_vnc_display(qmp, n)),
            (Some(n), None) => format!("VNC 127.0.0.1:{}", 5900 + n),
            (None, _) => "window".to_string(),
        };
        write_file(&self.st(&format!("{}.vnc", vm.name)), format!("{}\n", vnc))?;
        log(&format!("  {} started ({})", vm.name, vnc));
        Ok(())
    }

    /// Boot every machine (DC first so its LAN endpoint and DNS come up before members).
    fn boot_lab(&mut self, build: bool) -> Result<()> {
        self.offsets.clear();
        // DC first, then members in lab order
        let mut order = self.vm_names();
        order.sort_by_key(|n| if self.vm(n).role == Role::Dc { 0 } else { 1 });
        for (i, name) in order.iter().enumerate() {
            self.boot_vm(name, build, i == 0)?;
        }
        Ok(())
    }

    /// Write one machine's control disc (and a run's mount discs) and start it. The build and
    /// the rename write straight into its 0-state disk; a run or `up` into a throwaway overlay.
    fn boot_vm(&mut self, name: &str, build: bool, first: bool) -> Result<()> {
        let vm_idx = self.vms.iter().position(|v| v.name == name).expect("known vm");
        let cd = self.st(&format!("{}.iso", name));
        let disk = if build || self.renaming { self.zero_disk(name) } else { self.run_disk(name) };
        let mut cds = vec![cd.clone()];
        self.make_cd(&self.vms[vm_idx], &cd, build)?;
        // mount discs only matter on a real run
        if !build && self.cmd == "run" {
            cds.extend(self.make_mount_isos(&self.vms[vm_idx])?);
        }
        self.start_vm(&self.vms[vm_idx], disk, cds)?;
        if first {
            self.settle(1.0)?; // let the DC's multicast endpoint come up first
        }
        Ok(())
    }

    /// Let first logons finish, then power every machine off cleanly, members first, DC last:
    /// what is on their 0-state disks is then a 0 state.
    fn freeze_lab(&self) -> Result<()> {
        // let first logons finish (profile, group policy) before the freeze
        self.settle(20.0)?;
        let mut order = self.vm_names();
        order.sort_by_key(|n| if self.vm(n).role == Role::Dc { 1 } else { 0 });
        for vm in &order {
            log(&format!("shutting {} down cleanly", vm));
            if !self.freeze_off(vm)? {
                bail!("{} did not power off; 0 state not saved (wtl rebuild {}, then retry)", vm, self.arg);
            }
            let _ = std::fs::remove_file(self.st(&format!("{}.pid", vm)));
            let _ = std::fs::remove_file(self.st(&format!("{}.iso", vm)));
        }
        Ok(())
    }

    fn serial_wait(&mut self, vm: &str, until: &[&str], fail: &[&str], timeout: f64) -> Result<Outcome> {
        let mut off = self.offsets.get(vm).copied().unwrap_or(0);
        let w = Wait { until, fail, timeout: Some(timeout), pid: self.read_pid(vm), progress: true, events_file: None };
        let o = w.run(&self.st(&format!("{}.serial.log", vm)), &mut off);
        self.offsets.insert(vm.to_string(), off);
        if o == Outcome::Interrupted {
            return Err(self.interrupted());
        }
        Ok(o)
    }

    fn reset_if_off(&self, vm: &str) -> bool {
        let st = self.vm_status(vm);
        if st == "shutdown" || st == "guest-panicked" {
            warn(&format!("{} powered off on a reboot request; resetting it", vm));
            self.qmp_cmd(vm, "system_reset");
            return true;
        }
        false
    }

    /// Wait for one event on a machine's serial log (polling the power state so a guest that
    /// powered off mid-wait is reset and the wait continues).
    fn wait_ev(&mut self, vm: &str, until: &str, fail: &str, label: &str) -> Result<String> {
        let u: Vec<&str> = until.split(',').collect();
        let f: Vec<&str> = fail.split(',').collect();
        loop {
            let r = self.remaining().clamp(1.0, 30.0);
            match self.serial_wait(vm, &u, &f, r)? {
                Outcome::Until(line) => return Ok(line),
                Outcome::Timeout => {
                    self.reset_if_off(vm);
                    if Instant::now() >= self.deadline {
                        return Err(self.fail(vm, Outcome::Timeout, label));
                    }
                }
                other => return Err(self.fail(vm, other, label)),
            }
        }
    }

    fn fail(&self, vm: &str, o: Outcome, label: &str) -> anyhow::Error {
        match &o {
            Outcome::Fail(line) => err(&format!("{}: {} reported: {}", label, vm, line)),
            Outcome::Timeout => err(&format!("{}: timeout (--timeout-min {})", label, self.timeout_min)),
            Outcome::Exited => {
                err(&format!("{}: the {} VM exited", label, vm));
                indent_file(&self.st(&format!("{}.qemu.log", vm)));
            }
            _ => err(&format!("{}: serial wait error", label)),
        }
        if self.vm_pid(vm).is_some() {
            let png = self.st(&format!("{}-fail.png", vm));
            if self.screendump(vm, &png) {
                log(&format!("screenshot: {}", png.display()));
            }
        }
        log(&format!(
            "logs: {} (guest: C:\\WTL\\agent.log); the VMs keep running - wtl restore {}",
            self.st(&format!("{}.serial.log", vm)).display(),
            self.arg
        ));
        Exit(if o == Outcome::Timeout { 3 } else { 2 }).into()
    }

    fn wait_logon(&mut self, vm: &str, label: &str) -> Result<()> {
        let wait = 300.0 * self.slow(vm) as f64;
        for attempt in 1..=3 {
            let t = self.remaining().clamp(1.0, wait);
            match self.serial_wait(vm, &["AGENT_START"], &["LAB_FAIL"], t)? {
                Outcome::Until(_) => return Ok(()),
                Outcome::Timeout if Instant::now() >= self.deadline => return Err(self.fail(vm, Outcome::Timeout, label)),
                Outcome::Timeout => {}
                other => return Err(self.fail(vm, other, label)),
            }
            if !self.reset_if_off(vm) {
                warn(&format!("{}: no logon within {} min (autologon stuck?) - resetting it ({}/3)", vm, wait as u64 / 60, attempt));
                self.qmp_cmd(vm, "system_reset");
            }
        }
        self.wait_ev(vm, "AGENT_START", "LAB_FAIL", label).map(|_| ())
    }

    // ---------------------------------------------------------------- 0 state

    fn prepare(&mut self) -> Result<()> {
        if self.zero_done.is_file() {
            return Ok(());
        }
        if self.envf.is_some() {
            bail!(
                "environment {} has no 0 state (its rename did not finish): wtl rebuild {} makes it again, wtl rm {} deletes it",
                self.name,
                self.name,
                self.name
            );
        }
        log(&format!("=== building the 0 state of lab {} ({}; {} machines, once) ===", self.name, self.mode_text(), self.vms.len()));
        self.restore()?;
        self.build_bases()?;
        for vm in self.vm_names() {
            // the frozen 0-state disk is an overlay on the (read-only) base image; the build
            // boots it directly so the domain/join is baked into the 0 state
            let base = self.base_image(self.vm(&vm));
            let _ = std::fs::remove_file(self.zero_disk(&vm));
            self.qemu_img_create(&base, &self.zero_disk(&vm))?;
        }
        self.boot_lab(true)?;
        if let Some(dc) = self.dc_name() {
            log("creating the domain (DC: AD DS + forest + users, one reboot)");
            let line = self.wait_ev(&dc, "LAB_DC_READY", "LAB_FAIL,ERROR", "domain controller")?;
            ok(&format!("DC ready: {}", line));
        }
        // the other machines: join (AD) or set up local users (workgroup), reboot, autologon,
        // ready, then idle (shutdown_when_ready)
        let members = self.member_names();
        let (doing, label) = if self.lab.is_ad() { ("joining", "domain join") } else { ("setting up", "setup") };
        for m in &members {
            log(&format!("{} {} (one reboot, then autologon)", doing, m));
            self.wait_ev(m, "LAB_JOINED", "LAB_FAIL,ERROR", &format!("{} {}", m, label))?;
        }
        for m in &members {
            self.wait_logon(m, &format!("{} logon after the {}", m, label))?;
            let line = self.wait_ev(m, "LAB_CLIENT_READY", "LAB_FAIL,ERROR", m)?;
            ok(&format!("{} ready: {}", m, line));
            self.wait_ev(m, "LAB_IDLE", "LAB_FAIL,ERROR", m)?;
        }
        self.freeze_lab()?;
        let done = ZeroState {
            lab: self.name.clone(),
            built_at: chrono::Local::now().format("%Y-%m-%d %H:%M").to_string(),
            machines: self.vm_names(),
            guest: GUEST_FEATURES.iter().map(|f| f.to_string()).collect(),
            id: envs::random_hex(8),
        };
        write_file(&self.zero_done, pyjson::dumps(&done))?;
        ok(&format!("0 state saved for lab {} ({} machines)", self.name, self.vms.len()));
        Ok(())
    }

    // ---------------------------------------------------------------- run / up

    fn summary(&self) {
        eprintln!();
        match &self.envf {
            Some(f) => eprintln!("  environment {} (id {}) of lab {}: {}", self.name, f.id, f.lab, self.mode_text()),
            None => eprintln!("  lab {}: {}", self.name, self.mode_text()),
        }
        for v in &self.vms {
            // who is logged on: DOMAIN\user in AD mode, MACHINE\user (a local account) otherwise
            let realm = if self.lab.is_ad() { self.lab.netbios().to_string() } else { v.hostname.clone() };
            let who = match &v.logon {
                Some(u) => format!("logon {}\\{}", realm, u),
                None => format!("{}\\Administrator", realm),
            };
            let role = match (v.role, self.lab.is_ad()) {
                (Role::Dc, _) => "DC    ",
                (Role::Member, true) => "member",
                (Role::Member, false) => "single",
            };
            let ip = if v.ip.is_empty() { "(NAT only)" } else { v.ip.as_str() };
            eprintln!(
                "    {} {:<15} {}  {}  {}",
                role,
                if v.hostname != v.name { format!("{} ({})", v.name, v.hostname) } else { v.name.clone() },
                ip,
                read_trim(&self.st(&format!("{}.vnc", v.name))).unwrap_or_default(),
                who
            );
        }
        eprintln!("  back to the 0 state: wtl restore {}      another run: wtl run CONFIG", self.arg);
    }

    fn run_lab(&mut self) -> Result<i32> {
        let run_id = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
        self.prepare()?;
        if self.cmd == "prepare" {
            return Ok(0);
        }
        if self.cmd == "run" {
            self.run_id = format!("{}-{}", run_id, self.instance().id);
        }
        let res = self.state.join("runs").join(&run_id);
        if self.cmd == "run" {
            std::fs::create_dir_all(&res).with_context(|| format!("cannot create {}", res.display()))?;
            self.set_last_run(&format!("runs/{}", run_id));
        }
        self.restore()?;
        for vm in self.vm_names() {
            self.qemu_img_create(&self.zero_disk(&vm), &self.run_disk(&vm))?;
        }
        // clear old mount discs
        if let Ok(rd) = std::fs::read_dir(&self.state) {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().into_owned();
                if n.starts_with("mount-") && n.ends_with(".iso") {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
        log(&format!("{}: booting {} machines from the 0 state", self.what(), self.vms.len()));
        self.boot_lab(false)?;
        if let Some(dc) = self.dc_name() {
            self.wait_ev(&dc, "LAB_DC_READY", "LAB_FAIL,ERROR", "domain controller")?;
            ok("DC ready");
        }
        for m in &self.member_names() {
            self.wait_logon(m, &format!("{} logon", m))?;
            self.wait_ev(m, "LAB_CLIENT_READY", "LAB_FAIL,ERROR", m)?;
            ok(&format!("{} ready", m));
        }
        if self.cmd == "up" {
            self.summary();
            return Ok(0);
        }

        // ---- config run: each machine with steps runs them and reports; multiplex the waits
        let config = self.config.clone().unwrap_or_default();
        let config_name = config.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        self.warn_old_worker();
        log(&format!("running {} (steps on {} machine(s))", config_name, self.vms.iter().filter(|v| v.has_steps()).count()));
        let (status, step_results) = self.run_steps()?;

        // decode each machine's output into runs/<id>/<machine>/
        for v in &self.vms {
            if !v.has_steps() {
                continue;
            }
            let dir = res.join(&v.name);
            let _ = std::fs::create_dir_all(&dir);
            let serial = self.st(&format!("{}.serial.log", v.name));
            let _ = outputs::decode(&serial, &dir);
            let _ = std::fs::copy(&serial, dir.join("serial.log"));
            let _ = self.screendump(&v.name, &dir.join("screen.png"));
        }
        self.settle(2.0)?;
        if !config.as_os_str().is_empty() {
            let _ = std::fs::copy(&config, res.join("config.json"));
        }
        // runs/<id>/result.json: every step with machine, user, exit, expect, duration
        let inst = self.instance();
        let summary = RunResult {
            lab: inst.lab,
            environment: inst.environment,
            id: inst.id,
            hostnames: inst.hostnames,
            config: config_name.clone(),
            run_id: self.run_id.clone(),
            vars: self.run_vars.iter().map(|v| (v.name.clone(), if v.secret { "***".to_string() } else { v.value.clone() })).collect(),
            status: if status == 0 { "ok" } else { "failed" }.to_string(),
            steps: step_results,
        };
        if let Ok(j) = serde_json::to_string_pretty(&summary) {
            let _ = write_file(&res.join("result.json"), format!("{}\n", j));
        }
        self.summary();
        eprintln!("  config: {}  ->  {}", config_name, if status == 0 { "ALL STEPS OK" } else { "A STEP FAILED" });
        eprintln!("  results: {} (per machine: <name>/step*.txt, screen.png; result.json = summary)", res.display());
        Ok(status)
    }

    /// Wait for every machine-with-steps to reach LAB_RUN_DONE, reporting LAB_STEP lines as
    /// they arrive. The machines run concurrently, so poll each in a short round-robin slice.
    fn run_steps(&mut self) -> Result<(i32, Vec<StepResult>)> {
        let mut pending: Vec<String> = self.vms.iter().filter(|v| v.has_steps()).map(|v| v.name.clone()).collect();
        if pending.is_empty() {
            ok("no steps");
            return Ok((0, Vec::new()));
        }
        let mut status = 0;
        let mut results: Vec<StepResult> = Vec::new();
        while !pending.is_empty() {
            let mut still = Vec::new();
            for vm in pending.clone() {
                if Instant::now() >= self.deadline {
                    return Err(self.fail(&vm, Outcome::Timeout, "config run"));
                }
                // a short slice per machine so all progress roughly together
                match self.serial_wait(&vm, &["LAB_RUN_DONE", "LAB_STEP"], &["LAB_FAIL", "ERROR"], 2.0)? {
                    Outcome::Until(line) if line.contains("LAB_RUN_DONE") => {
                        ok(&format!("[{}] all steps done", vm));
                    }
                    Outcome::Until(line) => {
                        // a LAB_STEP line: report it, keep waiting on this machine
                        results.push(self.report_step(&vm, &line, &mut status));
                        still.push(vm);
                    }
                    Outcome::Timeout => {
                        self.reset_if_off(&vm);
                        still.push(vm);
                    }
                    other => return Err(self.fail(&vm, other, "config run")),
                }
            }
            pending = still;
        }
        // stable order: by machine, then step number
        results.sort_by(|a, b| a.machine.cmp(&b.machine).then(a.n.cmp(&b.n)));
        Ok((status, results))
    }

    /// Report one LAB_STEP line and return its record; mark the run failed if exit != expect_exit.
    fn report_step(&self, vm: &str, line: &str, status: &mut i32) -> StepResult {
        let ev = outputs::parse_event(line);
        let g = |k: &str| ev.as_ref().and_then(|e| e.get(k)).unwrap_or("").to_string();
        let (n, script, exit) = (g("n"), labdef::mask_secrets(&self.run_vars, &g("script")), g("exit"));
        // which host-side step is this (per-machine 1-based index) -> its expect_exit
        let expect =
            util::py_int(&n).and_then(|idx| self.vm(vm).steps.iter().find(|(i, _)| *i == idx)).map(|(_, s)| s.expect_exit).unwrap_or(0);
        let who = g("as");
        let ok_exit = util::py_int(&exit) == Some(expect) || (exit == "running" && self.step_keeps_running(vm, &n));
        if ok_exit {
            ok(&format!("[{} {}] {} -> exit {}", vm, who, script, exit));
        } else {
            err(&format!("[{} {}] {} -> exit {} (want {})", vm, who, script, exit, expect));
            *status = 1;
        }
        StepResult {
            machine: vm.to_string(),
            user: who,
            n: util::py_int(&n).unwrap_or(0),
            script,
            exit,
            expect_exit: expect,
            secs: g("secs").parse().unwrap_or(0.0),
            ok: ok_exit,
        }
    }

    fn step_keeps_running(&self, vm: &str, n: &str) -> bool {
        util::py_int(n).and_then(|idx| self.vm(vm).steps.iter().find(|(i, _)| *i == idx)).map(|(_, s)| s.keep_running).unwrap_or(false)
    }
}

// --------------------------------------------------------------- loading / routing

/// Load + resolve a lab JSON file.
pub fn load_lab(path: &Path) -> Result<Lab> {
    labdef::LabDef::load(path)
}

/// Resolve the run file against its lab: `"lab": "school"` is `labs/school.json` in the home
/// (else the built-in lab `school`), `"lab": "./lab.json"` (anything with a `/` or `\`, or
/// ending in `.json`) is a lab file relative to the run file; a name may also be an
/// environment (`school-k7q3`). `lab_override` (`wtl run --lab`) replaces the file's `lab` (a
/// path in it is relative to the current folder). `extends`, `vars` and `${WTL_NAME}` are
/// applied first ([`crate::runconf`]). Returns the lab file path, the resolved lab, the resolved
/// run, and the environment when the lab is one.
pub fn load_run(config: &Path, lab_override: Option<&str>) -> Result<(PathBuf, Lab, Run, Option<EnvTarget>)> {
    let (value, vars) = crate::runconf::load(config, |k| std::env::var(k).ok())?;
    // a secret's value never shows in an error message
    let (lab_file, lab, mut run, target) =
        resolve_run(config, value, lab_override).map_err(|e| anyhow!("{}", labdef::mask_secrets(&vars, &format!("{:#}", e))))?;
    run.vars = vars;
    Ok((lab_file, lab, run, target))
}

fn resolve_run(config: &Path, value: serde_json::Value, lab_override: Option<&str>) -> Result<(PathBuf, Lab, Run, Option<EnvTarget>)> {
    let mut def = labdef::RunDef::from_value(value, &config.display().to_string())?;
    if let Some(o) = lab_override {
        def.lab = if labdef::lab_is_path(o) { util::abspath(Path::new(o)).to_string_lossy().into_owned() } else { o.to_string() };
    }
    if def.lab.trim().is_empty() {
        bail!("{}: no lab (set \"lab\", here or in the file it extends)", config.display());
    }
    let base_dir = util::abspath(config).parent().unwrap_or(Path::new(".")).to_path_buf();
    let lab_file = if labdef::lab_is_path(&def.lab) {
        let p = Path::new(&def.lab);
        let p = if p.is_absolute() { p.to_path_buf() } else { base_dir.join(p) };
        util::canon(&util::normpath(&p))
    } else {
        if !util::is_safe_name(&def.lab) {
            bail!("{}: lab {:?} must be a lab name (A-Z 0-9 . _ -) or a path to a lab file", config.display(), def.lab);
        }
        let home = Home::resolve()?;
        match assets::lab_file(&home, &def.lab)? {
            Some(f) => f,
            None => match open_env(&home, &def.lab)? {
                Some((lab_file, lab, target)) => return finish_run(config, def, lab_file, lab, Some(target)),
                None => home.labs_dir().join(format!("{}.json", def.lab)),
            },
        }
    };
    if !lab_file.is_file() {
        bail!("{}: lab {:?} not found ({} missing)", config.display(), def.lab, lab_file.display());
    }
    let lab = labdef::LabDef::load(&lab_file)?;
    finish_run(config, def, lab_file, lab, None)
}

/// Resolve a run file's steps and mounts against the lab it boots.
fn finish_run(
    config: &Path,
    def: labdef::RunDef,
    lab_file: PathBuf,
    lab: Lab,
    target: Option<EnvTarget>,
) -> Result<(PathBuf, Lab, Run, Option<EnvTarget>)> {
    let base_dir = util::abspath(config).parent().unwrap_or(Path::new(".")).to_path_buf();
    let host_resolve = |h: &str| {
        let p = Path::new(h);
        if p.is_absolute() {
            util::normpath(p).to_string_lossy().into_owned()
        } else {
            util::normpath(&base_dir.join(p)).to_string_lossy().into_owned()
        }
    };
    // ${WTL_HOST_<KEY>}: a machine's computer name, known only now (the lab's, or the
    // environment's own)
    let mut def = def;
    let hosts: IndexMap<String, String> = lab
        .machines
        .iter()
        .map(|m| {
            let h = target.as_ref().and_then(|(_, f)| f.hostnames.get(&m.name).cloned()).unwrap_or_else(|| m.hostname.clone());
            (format!("WTL_HOST_{}", host_var(&m.name)), h)
        })
        .collect();
    let sub = |v: &str, what: &str| -> Result<String> {
        crate::runconf::substitute_hosts(v, &hosts).map_err(|e| anyhow!("{}: {}: {}", config.display(), what, e))
    };
    for (i, st) in def.steps.iter_mut().enumerate() {
        st.run = sub(&st.run, &format!("step {} run", i + 1))?;
        if let Some(a) = &st.args {
            st.args = Some(sub(a, &format!("step {} args", i + 1))?);
        }
    }
    for (letter, host) in def.mount.iter_mut() {
        *host = sub(host, &format!("mount {}", letter))?;
    }
    let default_name = util::stem(util::basename(&config.to_string_lossy())).to_string();
    let run = def.resolve(&config.display().to_string(), &default_name, &lab, host_resolve)?;
    Ok((lab_file, lab, run, target))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lan_group_is_admin_scoped() {
        assert!(LAN_GROUP.starts_with("239."));
    }

    #[test]
    fn state_names_do_not_collide() {
        let lab = |name: Option<&str>| Lab {
            name: name.map(str::to_string),
            mode: labdef::Mode::Workgroup,
            admin_password: "p".into(),
            network: None,
            machines: vec![],
            users: vec![],
        };
        let root = &Home::fixed(Path::new("/r"));
        // labs/ in the home: the file name
        assert_eq!(state_name(root, Path::new("/r/labs/school.json"), &lab(None)), "school");
        // a built-in lab's copy in its state folder: its name too
        assert_eq!(state_name(root, Path::new("/r/labs/state/school/school.json"), &lab(None)), "school");
        // elsewhere: the file name + a hash of the path, different per project
        let a = state_name(root, Path::new("/p1/wintest/lab.json"), &lab(None));
        let b = state_name(root, Path::new("/p2/wintest/lab.json"), &lab(None));
        assert!(a.starts_with("lab-") && a.len() == 12 && a != b, "{} {}", a, b);
        assert_eq!(a, state_name(root, Path::new("/p1/wintest/lab.json"), &lab(None)), "stable");
        // an explicit name wins
        assert_eq!(state_name(root, Path::new("/p1/wintest/lab.json"), &lab(Some("myapp"))), "myapp");
    }
}
