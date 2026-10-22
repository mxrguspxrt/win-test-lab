//! `lab` - Active Directory lab runner (port of lab.sh + the parts of lib/common.sh it
//! uses). Same commands, options, messages and exit codes; same state directory layout
//! (labs/state/<lab>/), so the bash `lab` and this one can be used on the same labs.

use crate::labconf::{self, Mount, RunConfig, Step};
use crate::labrun;
use crate::logx::{err, log, ok, warn};
use crate::oslist::{self, OsInfo};
use crate::qemu::{self, Env, Lan, Opts};
use crate::qmp::{Addr, Qmp};
use crate::serialwait::{Outcome, Wait};
use crate::{mkiso, outputs, pyjson, shconf, sys, util};
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const AFTER_HELP: &str = r#"A CONFIG is a key=value file (see examples/configs/, `lab help-config`):
    version = 2019                      # the lab (labs/<version>.conf) to boot
    mount   = /host/folder:X, /other:Z  # host folder -> read-only drive letter in the guest
    run_as_admin   = X:\install.bat     # a step run as the lab's root (the SYSTEM account)
    run_as_student = Z:\run.bat         # a step run as LAB\student
Steps run in the order written (run_as_admin / run_as_student lines may be interleaved).
Host folders are mounted read-only (a snapshot taken at launch). Paths in the config are
relative to the config file. The LAB commands also accept a CONFIG (it names the lab).

LAB = labs/LAB.conf (e.g. 2012, 2019). The 0 state = Windows installed, domain created,
client joined, LAB\student logging on automatically, both shut down cleanly. It is frozen
(labs/state/LAB/*-0.qcow2); runs write only to small throwaway overlays on top of it.

Exit: 0 all steps passed, 1 a step failed, 2 lab error, 3 timeout."#;

/// `lab CMD LAB|CONFIG... [options]`
#[derive(clap::Args, Debug)]
#[command(
    about = "Active Directory lab: a domain controller + a domain-joined client, on QEMU (lab.sh)",
    override_usage = "lab run CONFIG [CONFIG...] [--show] [--wait S] [--timeout-min M]\n       \
                      lab restore|up|stop|status|prepare|rebuild LAB|CONFIG\n       lab help-config",
    after_help = AFTER_HELP
)]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Cmd,
}

#[derive(clap::Subcommand, Debug)]
pub enum Cmd {
    /// run a config file: boot the lab it names, mount host folders as drives, run its
    /// scripts as admin / as the student (several configs are merged left to right)
    Run(Target),
    /// stop the VMs and throw away everything since the 0 state (~1 s)
    Restore(Target),
    /// boot the lab from its 0 state without deploying anything
    Up(Target),
    /// power the VMs off (changes kept until the next run / restore)
    #[command(visible_alias = "down")]
    Stop(Target),
    /// show the lab's 0 state, disks, VMs and last run
    Status(Target),
    /// build the 0 state (run does this automatically the first time)
    Prepare(Target),
    /// delete the 0 state; the next run builds a new domain (the Windows installs stay)
    #[command(visible_alias = "reset")]
    Rebuild(Target),
    /// describe the run config format
    HelpConfig,
}

#[derive(clap::Args, Debug)]
pub struct Target {
    /// LAB (labs/LAB.conf, e.g. 2019) or CONFIG file(s); `run` needs CONFIG(s)
    #[arg(required = true, value_name = "LAB|CONFIG")]
    pub target: Vec<String>,
    /// open the VM windows (default: headless, VNC addresses printed)
    #[arg(long)]
    pub show: bool,
    /// include Windows Update when a base image has to be installed (hours)
    #[arg(long)]
    pub updates: bool,
    /// seconds a step may run before it counts as "still running" and is left running
    /// (default 60; overrides the config's own 'wait')
    #[arg(long, value_name = "S", value_parser = positive_int)]
    pub wait: Option<i64>,
    /// give up after M minutes (default: the config's timeout_min, else 120)
    #[arg(long, value_name = "M", value_parser = whole_number)]
    pub timeout_min: Option<i64>,
}

fn whole_number(s: &str) -> Result<i64, String> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return Err("must be a whole number".into());
    }
    s.parse().map_err(|_| "is too large".to_string())
}

fn positive_int(s: &str) -> Result<i64, String> {
    match whole_number(s) {
        Ok(n) if n > 0 => Ok(n),
        _ => Err("must be a positive integer".into()),
    }
}

/// Ends `lab` with this exit code; the reason has already been printed.
#[derive(Debug)]
struct Exit(i32);

impl std::fmt::Display for Exit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "exit {}", self.0)
    }
}

impl std::error::Error for Exit {}

/// labs/state/<lab>/0-state.json, written when the 0 state is complete
#[derive(Serialize, Deserialize, Default)]
struct ZeroState {
    #[serde(default)]
    lab: String,
    #[serde(default)]
    built_at: String,
}

/// LAB\lab.json on the control disc, read by guest/lab.ps1 (same fields as lab.sh wrote)
#[derive(Serialize)]
struct LabJson<'a> {
    role: &'a str,
    ip: &'a str,
    prefix: i64,
    dc_ip: &'a str,
    lan_mac: &'a str,
    domain: &'a str,
    netbios: &'a str,
    admin_password: &'a str,
    users: Vec<LabUser<'a>>,
    logon_user: &'a str,
    logon_password: &'a str,
    shutdown_when_ready: bool,
    /// the config run (client only); null otherwise
    run: Option<LabRun<'a>>,
}

#[derive(Serialize)]
struct LabUser<'a> {
    name: &'a str,
    password: &'a str,
    groups: Vec<&'a str>,
}

#[derive(Serialize)]
struct LabRun<'a> {
    mounts: &'a [Mount],
    steps: &'a [Step],
    wait: i64,
}

const DC_MAC: &str = "52:54:00:4c:41:10";
const CL_MAC: &str = "52:54:00:4c:41:20";
const VMS: [&str; 2] = ["dc", "client"];
/// stop order: the client first, so the DC is still there while it logs off
const STOP_ORDER: [&str; 2] = ["client", "dc"];

/// Find the win-test-lab checkout (lib/oslist.conf + guest/lab.ps1): $WTL_ROOT, else a
/// parent of this executable, of the source tree it was built from, or of the cwd.
pub fn find_root() -> Result<PathBuf> {
    let is_root = |d: &Path| d.join("lib").join("oslist.conf").is_file() && d.join("guest").join("lab.ps1").is_file();
    if let Some(r) = std::env::var_os("WTL_ROOT").filter(|v| !v.is_empty()) {
        let r = util::canon(Path::new(&r));
        if is_root(&r) {
            return Ok(r);
        }
        bail!("WTL_ROOT={} is not a win-test-lab checkout (no lib/oslist.conf + guest/lab.ps1)", r.display());
    }
    let mut starts = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        starts.push(util::canon(&exe));
    }
    starts.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")));
    if let Ok(cwd) = std::env::current_dir() {
        starts.push(cwd);
    }
    for s in &starts {
        if let Some(d) = s.ancestors().find(|d| is_root(d)) {
            return Ok(util::canon(d));
        }
    }
    bail!("cannot find the win-test-lab directory (set WTL_ROOT to it)")
}

/// 127.0.0.1:PORT can be bound
fn port_free(port: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
}

/// an ephemeral free TCP port on 127.0.0.1
pub(crate) fn free_port() -> Option<u16> {
    std::net::TcpListener::bind(("127.0.0.1", 0)).ok()?.local_addr().ok().map(|a| a.port())
}

/// N (10..99, then 0..9) such that 127.0.0.1:(5900+N) is free
pub(crate) fn pick_vnc_display() -> Option<u16> {
    (10..100).chain(0..10).find(|n| port_free(5900 + n))
}

/// a unix socket path short enough (<= 100 chars; sun_path is ~108)
pub(crate) fn sock_path(preferred: PathBuf, name: &str) -> PathBuf {
    if preferred.as_os_str().len() <= 100 {
        return preferred;
    }
    let dir = std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()).unwrap_or_else(|| "/tmp".into());
    PathBuf::from(dir).join(format!("wtl-{}.sock", name))
}

pub(crate) fn read_trim(p: &Path) -> Option<String> {
    std::fs::read_to_string(p).ok().map(|s| s.trim_end_matches(['\r', '\n']).to_string())
}

pub(crate) fn indent_file(p: &Path) {
    if let Ok(s) = std::fs::read(p) {
        for l in String::from_utf8_lossy(&s).lines() {
            eprintln!("    {}", l);
        }
    }
}

pub(crate) fn write_file(p: &Path, data: impl AsRef<[u8]>) -> Result<()> {
    std::fs::write(p, data).with_context(|| format!("cannot write {}", p.display()))
}

/// One base image (`images/<image>.qcow2`) to install with build-image.sh.
pub(crate) struct BaseBuild {
    /// short label for the log prefix and STATE/build-<tag>.log
    pub tag: String,
    /// OS key from lib/oslist.conf
    pub os: String,
    /// image name: images/<image>.qcow2
    pub image: String,
    /// the Windows computer name baked into the image (multi-machine labs rename it later)
    pub computer: String,
}

impl BaseBuild {
    pub(crate) fn new(tag: &str, os: &str, image: &str, computer: &str) -> BaseBuild {
        BaseBuild { tag: tag.into(), os: os.into(), image: image.into(), computer: computer.into() }
    }
}

/// Install Windows into each base image with `build-image.sh` (unattended, in parallel). Each
/// build's output is tee'd to LOG_DIR/build-<tag>.log and shown prefixed with `[tag]`. `vars`
/// supplies the WTL_* settings passed on to the script; `nap` sleeps (and fails on Ctrl-C).
/// The caller checks that the images exist afterwards.
pub(crate) fn build_base_images(
    root: &Path,
    vars: &HashMap<String, String>,
    log_dir: &Path,
    builds: &[BaseBuild],
    updates: bool,
    show: bool,
    nap: &dyn Fn(f64) -> Result<()>,
) -> Result<()> {
    if builds.is_empty() {
        return Ok(());
    }
    let names: Vec<&str> = builds.iter().map(|b| b.tag.as_str()).collect();
    log(&format!(
        "installing Windows for: {} (unattended; {})",
        names.join(" "),
        if updates { "with Windows Update, hours" } else { "no updates, ~10-30 min" }
    ));
    let script = root.join("build-image.sh");
    let mut kids = Vec::new();
    for (i, b) in builds.iter().enumerate() {
        let mut cmd = if cfg!(windows) {
            let mut c = Command::new("bash"); // build-image.sh is a bash script
            c.arg(&script);
            c
        } else {
            Command::new(&script)
        };
        cmd.args([&b.os, "--name", &b.image, "--computer-name", &b.computer]);
        if !updates {
            cmd.arg("--no-updates");
        }
        if show {
            cmd.arg("--show");
        }
        for (k, v) in vars {
            if k.starts_with("WTL_") || k == "ROOT" {
                cmd.env(k, v);
            }
        }
        let (reader, writer) = std::io::pipe().context("pipe")?;
        cmd.stdin(Stdio::null()).stdout(writer.try_clone().context("pipe")?).stderr(writer);
        let child = cmd.spawn().with_context(|| format!("cannot run {}", script.display()))?;
        drop(cmd); // closes our copies of the pipe's write end
        let logf = log_dir.join(format!("build-{}.log", b.tag));
        let tag = b.tag.clone();
        let t = std::thread::spawn(move || {
            let mut lf = std::fs::File::create(logf).ok();
            for line in std::io::BufReader::new(reader).split(b'\n').map_while(Result::ok) {
                if let Some(f) = lf.as_mut() {
                    let _ = f.write_all(&line);
                    let _ = f.write_all(b"\n");
                }
                println!("[{}] {}", tag, String::from_utf8_lossy(&line));
            }
        });
        kids.push((child, t));
        if i + 1 < builds.len() {
            nap(10.0)?; // the builds pick their VNC displays one after the other
        }
    }
    for (mut child, t) in kids {
        while let Ok(None) = child.try_wait() {
            nap(0.5)?;
        }
        let _ = t.join();
    }
    Ok(())
}

struct Lab {
    root: PathBuf,
    env: Env,
    vars: HashMap<String, String>,
    cmd: &'static str,
    name: String,
    conf: PathBuf,
    show: bool,
    updates: bool,
    wait_s: i64,
    timeout_min: i64,
    /// the first config file (absolute); mounts were already resolved by labconf
    config: Option<PathBuf>,
    run: Option<RunConfig>,
    dc_os: OsInfo,
    cl_os: OsInfo,
    dc_ip: String,
    cl_ip: String,
    lan_port: u16,
    dc_base: PathBuf,
    cl_base: PathBuf,
    state: PathBuf,
    zero_done: PathBuf,
    deadline: Instant,
    /// serial log offset per VM: each wait continues where the last one stopped
    offsets: HashMap<&'static str, u64>,
    mount_isos: Vec<PathBuf>,
    qmp_tcp: bool,
}

/// `lab CMD ...`; returns the exit code.
pub fn main(cli: &Cli) -> i32 {
    match run_cli(cli) {
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

fn run_cli(cli: &Cli) -> Result<i32> {
    let (cmd, t) = match &cli.cmd {
        Cmd::HelpConfig => {
            println!("{}", labconf::DOC);
            return Ok(0);
        }
        Cmd::Run(t) => ("run", t),
        Cmd::Restore(t) => ("restore", t),
        Cmd::Up(t) => ("up", t),
        Cmd::Stop(t) => ("stop", t),
        Cmd::Status(t) => ("status", t),
        Cmd::Prepare(t) => ("prepare", t),
        Cmd::Rebuild(t) => ("rebuild", t),
    };
    let root = find_root()?;
    // JSON multi-machine labs: `run FOO.json`, or `<cmd> LAB` where labs/LAB.json exists (and
    // no labs/LAB.conf shadows it), or `<cmd> path/to/lab.json`. Routed to the N-machine
    // runner in labrun.rs; the .conf path below is unchanged.
    if let Some(code) = route_json(cmd, t, &root)? {
        return Ok(code);
    }
    // A run always names CONFIG file(s). The other commands take a LAB or CONFIG(s), whose
    // version names the lab. A config = an existing file that is not a bare lab name.
    let is_config = |x: &str| {
        Path::new(x).is_file()
            && (x.ends_with(".conf") || x.contains('/') || (cfg!(windows) && x.contains('\\')))
            && !root.join("labs").join(format!("{}.conf", x)).is_file()
    };
    let first = &t.target[0];
    if cmd == "run" && !is_config(first) {
        err("usage: lab run CONFIG [CONFIG...]  (CONFIG is a .conf file; see 'lab help-config')");
        return Err(Exit(2).into());
    }
    let (mut lab, mut run, mut config) = (first.clone(), None, None);
    if is_config(first) {
        if let Some(x) = t.target.iter().find(|x| !is_config(x)) {
            bail!("unknown option {}", x);
        }
        let abs: Vec<String> = t.target.iter().map(|p| util::canon(Path::new(p)).to_string_lossy().into_owned()).collect();
        config = Some(PathBuf::from(&abs[0]));
        match labconf::load(&abs) {
            Ok(c) => {
                lab = c.version.clone();
                run = Some(c);
            }
            Err(e) => {
                eprintln!("error: {}", e);
                bail!("config error (see above)");
            }
        }
    } else if let Some(x) = t.target.get(1) {
        bail!("unknown option {}", x);
    }
    // --wait / --timeout-min override the config's own values
    let wait_s = t.wait.or(run.as_ref().map(|r| r.wait)).unwrap_or(60);
    let timeout_min = t.timeout_min.or(run.as_ref().map(|r| r.timeout_min)).unwrap_or(120);
    if wait_s <= 0 {
        bail!("--wait must be a positive integer");
    }
    if timeout_min < 0 {
        bail!("--timeout-min must be an integer");
    }
    let mut l = Lab::new(root, cmd, &lab, t, wait_s, timeout_min, config, run)?;
    l.dispatch()
}

/// If the target names a JSON lab/run, hand it to the N-machine runner and return its exit
/// code; otherwise `None` so the two-machine `.conf` path takes over.
fn route_json(cmd: &'static str, t: &Target, root: &Path) -> Result<Option<i32>> {
    let first = &t.target[0];
    // a JSON run config: an existing *.json file (run only)
    let run_json = cmd == "run" && first.ends_with(".json") && Path::new(first).is_file();
    // a JSON lab: `<cmd> LAB` with labs/LAB.json present and no labs/LAB.conf, or a *.json file
    let lab_json_file = cmd != "run" && first.ends_with(".json") && Path::new(first).is_file();
    let lab_by_name = cmd != "run"
        && !first.contains('/')
        && !first.ends_with(".json")
        && util::is_safe_name(first)
        && root.join("labs").join(format!("{}.json", first)).is_file()
        && !root.join("labs").join(format!("{}.conf", first)).is_file();
    if !(run_json || lab_json_file || lab_by_name) {
        return Ok(None);
    }
    if let Some(x) = t.target.get(1) {
        bail!("unknown option {} (JSON labs take a single CONFIG/LAB)", x);
    }
    let opts = labrun::RunOpts { show: t.show, updates: t.updates, wait: t.wait, timeout_min: t.timeout_min };
    if run_json {
        let config = util::canon(Path::new(first));
        let (lab_file, lab, run) = match labrun::load_run(&config) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("error: {:#}", e);
                bail!("config error (see above)");
            }
        };
        return Ok(Some(labrun::run(cmd, &lab_file, Some(config), Some(run), lab, &opts)));
    }
    // a lab command (restore/up/stop/status/prepare/rebuild)
    let lab_file = if lab_json_file { util::canon(Path::new(first)) } else { root.join("labs").join(format!("{}.json", first)) };
    let lab = match labrun::load_lab(&lab_file) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("error: {:#}", e);
            bail!("lab error (see above)");
        }
    };
    Ok(Some(labrun::run(cmd, &lab_file, None, None, lab, &opts)))
}

impl Lab {
    #[allow(clippy::too_many_arguments)]
    fn new(
        root: PathBuf,
        cmd: &'static str,
        lab: &str,
        t: &Target,
        wait_s: i64,
        timeout_min: i64,
        config: Option<PathBuf>,
        run: Option<RunConfig>,
    ) -> Result<Lab> {
        let labs = root.join("labs");
        let conf = labs.join(format!("{}.conf", lab));
        if !util::is_safe_name(lab) || !conf.is_file() {
            let mut names: Vec<String> = std::fs::read_dir(&labs)
                .map(|rd| {
                    rd.flatten()
                        .filter_map(|e| e.file_name().to_str().and_then(|n| n.strip_suffix(".conf")).map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            names.sort();
            let list: String = names.iter().map(|n| format!("{} ", n)).collect();
            bail!("no lab config {} (labs: {})", conf.display(), list);
        }
        // the lab definition is a bash KEY=value file; WTL_* settings it makes apply too
        let mut vars = qemu::seed_vars(&root);
        for k in [
            "LAB_OS", "LAB_CLIENT_OS", "LAB_DOMAIN", "LAB_NETBIOS", "LAB_DC_NAME", "LAB_CLIENT_NAME", "LAB_NET",
            "LAB_LAN_PORT", "LAB_ADMIN_USER", "LAB_ADMIN_USER_PASSWORD", "LAB_USER", "LAB_USER_PASSWORD",
        ] {
            vars.insert(k.to_string(), String::new());
        }
        vars.insert("LAB_PREFIX".into(), "24".into());
        vars.insert("LAB_DC_MEM".into(), "3072".into());
        vars.insert("LAB_CLIENT_MEM".into(), "4096".into());
        let text = std::fs::read(&conf).with_context(|| format!("cannot read {}", conf.display()))?;
        shconf::parse(&String::from_utf8_lossy(&text), &mut vars, &|n| std::env::var(n).ok())
            .with_context(|| conf.display().to_string())?;
        if vars["LAB_CLIENT_OS"].is_empty() {
            let os = vars["LAB_OS"].clone();
            vars.insert("LAB_CLIENT_OS".into(), os);
        }
        for v in [
            "LAB_OS", "LAB_DOMAIN", "LAB_NETBIOS", "LAB_DC_NAME", "LAB_CLIENT_NAME", "LAB_NET", "LAB_LAN_PORT", "LAB_USER",
            "LAB_USER_PASSWORD",
        ] {
            if vars[v].is_empty() {
                bail!("{}: {} is not set", conf.display(), v);
            }
        }
        let oslist = root.join("lib").join("oslist.conf");
        let dc_os = oslist::lookup(&oslist, &vars["LAB_OS"])
            .ok_or_else(|| anyhow!("{}: unknown LAB_OS {}", conf.display(), vars["LAB_OS"]))?;
        if dc_os.kind != "server" {
            bail!(
                "{}: LAB_OS {} is a client OS and cannot be the domain controller (use a Windows Server for LAB_OS)",
                conf.display(),
                vars["LAB_OS"]
            );
        }
        let cl_os = oslist::lookup(&oslist, &vars["LAB_CLIENT_OS"])
            .ok_or_else(|| anyhow!("{}: unknown LAB_CLIENT_OS {}", conf.display(), vars["LAB_CLIENT_OS"]))?;
        let lan_port: u16 = vars["LAB_LAN_PORT"]
            .parse()
            .ok()
            .filter(|p| *p > 0)
            .ok_or_else(|| anyhow!("{}: LAB_LAN_PORT {} is not a TCP port", conf.display(), vars["LAB_LAN_PORT"]))?;
        let env = Env::from_vars(&root, &vars)?;
        let wtl_qmp = vars.get("WTL_QMP").cloned().or_else(|| std::env::var("WTL_QMP").ok()).unwrap_or_default();
        let qmp_tcp = match wtl_qmp.as_str() {
            "tcp" => true,
            "unix" => false,
            "" => !cfg!(unix),
            other => bail!("WTL_QMP={} must be tcp or unix", other),
        };
        let net = vars["LAB_NET"].clone();
        let state = labs.join("state").join(lab);
        std::fs::create_dir_all(&state).with_context(|| format!("cannot create {}", state.display()))?;
        let now = Instant::now();
        let limit = Duration::from_secs((timeout_min.max(0) as u64).saturating_mul(60));
        Ok(Lab {
            dc_base: env.images_dir.join(format!("{}-dc.qcow2", lab)),
            cl_base: env.images_dir.join(format!("{}-client.qcow2", lab)),
            zero_done: state.join("0-state.json"),
            state,
            root,
            env,
            vars,
            cmd,
            name: lab.to_string(),
            conf,
            show: t.show,
            updates: t.updates,
            wait_s,
            timeout_min,
            config,
            run,
            dc_os,
            cl_os,
            dc_ip: format!("{}.10", net),
            cl_ip: format!("{}.20", net),
            lan_port,
            deadline: now.checked_add(limit).unwrap_or(now + Duration::from_secs(100 * 365 * 86400)),
            offsets: HashMap::new(),
            mount_isos: Vec::new(),
            qmp_tcp,
        })
    }

    fn v(&self, k: &str) -> &str {
        self.vars.get(k).map(String::as_str).unwrap_or("")
    }

    fn st(&self, f: &str) -> PathBuf {
        self.state.join(f)
    }

    /// frozen 0 state
    fn zero_disk(&self, vm: &str) -> PathBuf {
        self.st(&format!("{}-0.qcow2", vm))
    }

    /// throwaway overlay of the current run
    fn run_disk(&self, vm: &str) -> PathBuf {
        self.st(&format!("{}.qcow2", vm))
    }

    fn read_pid(&self, vm: &str) -> Option<u32> {
        read_trim(&self.st(&format!("{}.pid", vm)))?.trim().parse().ok()
    }

    fn vm_pid(&self, vm: &str) -> Option<u32> {
        self.read_pid(vm).filter(|p| sys::pid_alive(*p))
    }

    // ------------------------------------------------------------------ QMP

    /// The VM's monitor: the address it was started with (vm.qmp.addr, TCP on Windows),
    /// else the unix socket labs/state/LAB/vm.qmp (what lab.sh uses).
    fn qmp_addr(&self, vm: &str) -> Addr {
        if let Some(a) = read_trim(&self.st(&format!("{}.qmp.addr", vm))).filter(|a| !a.is_empty()) {
            return Addr::parse(&a);
        }
        Addr::Unix(sock_path(self.st(&format!("{}.qmp", vm)), &format!("lab-{}-{}", self.name, vm)))
    }

    /// Pick the monitor address for a VM about to start.
    fn new_qmp_addr(&self, vm: &str) -> Result<Addr> {
        let addr_file = self.st(&format!("{}.qmp.addr", vm));
        if self.qmp_tcp {
            let port = free_port().context("no free TCP port for QMP")?;
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

    /// the QMP run state ("running", "shutdown", ...); empty if unknown
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

    // ------------------------------------------------------------------ waiting

    /// Ctrl-C / SIGTERM: report and end with exit 2 (the VMs keep running).
    fn interrupted(&self) -> anyhow::Error {
        eprintln!();
        warn(&format!("interrupted; VMs may still be running (lab status {} / lab restore {})", self.name, self.name));
        Exit(2).into()
    }

    fn nap(&self, secs: f64) -> Result<()> {
        if sys::nap(secs) { Ok(()) } else { Err(self.interrupted()) }
    }

    /// a fixed settle delay (skipped by the fake-QEMU tests)
    fn settle(&self, secs: f64) -> Result<()> {
        if self.env.fast { Ok(()) } else { self.nap(secs) }
    }

    fn remaining(&self) -> f64 {
        self.deadline.saturating_duration_since(Instant::now()).as_secs_f64()
    }

    // ------------------------------------------------------------------ stop / restore / status

    /// hard = QMP quit (instant; for throwaway runs), soft = ACPI power off
    fn stop_vms(&self, hard: bool) -> Result<()> {
        for vm in STOP_ORDER {
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
        for vm in STOP_ORDER {
            let Some(pid) = self.vm_pid(vm) else {
                // already gone (e.g. right after QMP quit): drop the stale pid file too
                let _ = std::fs::remove_file(self.st(&format!("{}.pid", vm)));
                continue;
            };
            // under -no-shutdown the guest powering off does NOT end QEMU; it reaches QMP
            // status "shutdown". Wait for either that or the pid to die, then quit QEMU.
            let mut i = 0;
            while sys::pid_alive(pid) && i < 900 {
                if self.vm_status(vm) == "shutdown" {
                    break;
                }
                self.nap(0.2)?;
                i += 1;
            }
            if sys::pid_alive(pid) {
                if i >= 900 {
                    warn(&format!("{} did not power off in 3 min, stopping it hard", vm));
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
        let _ = std::fs::remove_file(self.run_disk("dc"));
        let _ = std::fs::remove_file(self.run_disk("client"));
        Ok(())
    }

    /// Cleanly power a VM off for the 0-state freeze: wait for QMP status "shutdown" (or the
    /// pid to exit), then quit QEMU. false if it never powered off.
    fn freeze_off(&self, vm: &str) -> Result<bool> {
        let Some(pid) = self.read_pid(vm) else { return Ok(true) };
        self.qmp_cmd(vm, "system_powerdown");
        let mut i = 0;
        while sys::pid_alive(pid) && i < 1800 {
            if self.vm_status(vm) == "shutdown" {
                break;
            }
            if i == 300 {
                // ~60s: a user may be logged on; a second press shuts it down
                self.qmp_cmd(vm, "system_powerdown");
            }
            self.nap(0.2)?;
            i += 1;
        }
        if !sys::pid_alive(pid) {
            return Ok(true);
        }
        if self.vm_status(vm) != "shutdown" && i >= 1800 {
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

    fn status(&self) {
        println!(
            "lab {} ({}): domain {}, DC {} {}, client {} {}",
            self.name,
            self.conf.display(),
            self.v("LAB_DOMAIN"),
            self.v("LAB_DC_NAME"),
            self.dc_ip,
            self.v("LAB_CLIENT_NAME"),
            self.cl_ip
        );
        if self.zero_done.is_file() {
            let z: ZeroState = std::fs::read_to_string(&self.zero_done)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();
            println!("  0 state: ready ({})", z.built_at);
        } else {
            println!("  0 state: not built yet (lab prepare {}, or just lab run ...)", self.name);
        }
        for f in [
            &self.dc_base,
            &self.cl_base,
            &self.zero_disk("dc"),
            &self.zero_disk("client"),
            &self.run_disk("dc"),
            &self.run_disk("client"),
        ] {
            if f.is_file() {
                let base = f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                println!("  disk  {}  {}", base, util::human_size(util::disk_usage(f)));
            }
        }
        for vm in VMS {
            match self.vm_pid(vm) {
                Some(pid) => println!(
                    "  {}: running (pid {}) {}",
                    vm,
                    pid,
                    read_trim(&self.st(&format!("{}.vnc", vm))).unwrap_or_default()
                ),
                None => println!("  {}: off", vm),
            }
        }
        if let Some(last) = self.last_run() {
            println!("  last run: {}", last.display());
        }
    }

    fn last_run(&self) -> Option<PathBuf> {
        let p = self.st("last");
        let md = std::fs::symlink_metadata(&p).ok()?;
        if md.file_type().is_symlink() {
            return Some(util::canon(&p));
        }
        // where symlinks are not available (Windows without developer mode): a text file
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
        match self.cmd {
            "stop" => {
                self.stop_vms(false)?;
                ok(&format!("lab {} is off", self.name));
                return Ok(0);
            }
            "restore" => {
                self.restore()?;
                ok(&format!("lab {} is back at its 0 state (VMs off; next run/up starts clean)", self.name));
                return Ok(0);
            }
            "status" => {
                self.status();
                return Ok(0);
            }
            "rebuild" => {
                self.restore()?;
                for f in [self.zero_disk("dc"), self.zero_disk("client"), self.zero_done.clone()] {
                    let _ = std::fs::remove_file(f);
                }
                ok(&format!(
                    "lab {}: 0 state deleted; the next run builds a new domain (Windows installs kept)",
                    self.name
                ));
                return Ok(0);
            }
            _ => {} // run, up, prepare
        }
        self.env.check_tools()?;
        sys::install_interrupt_handler();
        self.run_lab()
    }

    // ------------------------------------------------------------------ helpers

    /// One read-only ISO per config mount, volume label WTL<LETTER>, from the host folder.
    fn make_mount_isos(&mut self) -> Result<()> {
        self.mount_isos.clear();
        let mounts = self.run.as_ref().map(|r| r.mounts.clone()).unwrap_or_default();
        for m in mounts {
            let src = PathBuf::from(&m.host);
            if !src.is_dir() {
                bail!("mount source is not a directory: {} (drive {}:)", m.host, m.letter);
            }
            let iso = self.st(&format!("mount-{}.iso", m.letter));
            mkiso::make_iso(&src, &iso, &format!("WTL{}", m.letter))
                .with_context(|| format!("mkiso for drive {}: failed", m.letter))?;
            self.mount_isos.push(iso);
            log(&format!("mount {}: <- {} ({})", m.letter, m.host, util::human_size(util::disk_usage(&src))));
        }
        Ok(())
    }

    /// The control disc: LAB\lab.ps1 + LAB\labsvc.ps1 + LAB\lab.json (+ the run's steps).
    fn make_cd(&self, vm: &str, role: &str, out: &Path, shutdown_when_ready: bool) -> Result<()> {
        let stage = self.st(&format!("cd-{}", vm));
        let _ = std::fs::remove_dir_all(&stage);
        let labdir = stage.join("LAB");
        std::fs::create_dir_all(&labdir).with_context(|| format!("cannot create {}", labdir.display()))?;
        for f in ["lab.ps1", "labsvc.ps1"] {
            let src = self.root.join("guest").join(f);
            std::fs::copy(&src, labdir.join(f)).with_context(|| format!("cannot copy {}", src.display()))?;
        }
        let (ip, mac) = if role == "client" { (&self.cl_ip, CL_MAC) } else { (&self.dc_ip, DC_MAC) };
        let run = match &self.run {
            Some(r) if role == "client" && self.cmd == "run" && !shutdown_when_ready && self.config.is_some() => {
                Some(LabRun { mounts: &r.mounts, steps: &r.steps, wait: self.wait_s })
            }
            _ => None,
        };
        let prefix = util::py_int(self.v("LAB_PREFIX"))
            .ok_or_else(|| anyhow!("{}: LAB_PREFIX {} is not a number", self.conf.display(), self.v("LAB_PREFIX")))?;
        let mut users = vec![LabUser { name: self.v("LAB_USER"), password: self.v("LAB_USER_PASSWORD"), groups: vec![] }];
        if !self.v("LAB_ADMIN_USER").is_empty() {
            users.push(LabUser {
                name: self.v("LAB_ADMIN_USER"),
                password: self.v("LAB_ADMIN_USER_PASSWORD"),
                groups: vec!["Domain Admins"],
            });
        }
        let j = LabJson {
            role,
            ip,
            prefix,
            dc_ip: &self.dc_ip,
            lan_mac: mac,
            domain: self.v("LAB_DOMAIN"),
            netbios: self.v("LAB_NETBIOS"),
            admin_password: &self.env.password,
            users,
            logon_user: self.v("LAB_USER"),
            logon_password: self.v("LAB_USER_PASSWORD"),
            shutdown_when_ready,
            run,
        };
        // json.dump(indent=1) style: pure ASCII, which PowerShell 4 reads without a BOM
        write_file(&labdir.join("lab.json"), pyjson::dumps_indent(&j, 1))?;
        mkiso::make_iso(&stage, out, "WTL_LAB").with_context(|| format!("mkiso {} failed", vm))?;
        let _ = std::fs::remove_dir_all(&stage);
        Ok(())
    }

    /// The per-VM UEFI vars file (kept in STATE, so it is part of the 0 state), seeded from
    /// the OVMF template the first time. None for BIOS guests.
    fn uefi_vars(&self, vm: &str) -> Result<Option<PathBuf>> {
        let os = if vm == "client" { &self.cl_os } else { &self.dc_os };
        if os.firmware != "uefi" {
            return Ok(None);
        }
        let vars = self.st(&format!("{}-OVMF_VARS.fd", vm));
        if !vars.is_file() {
            std::fs::copy(&self.env.ovmf_vars, &vars).with_context(|| {
                format!(
                    "cannot copy the UEFI vars template {} (install 'ovmf' or set WTL_OVMF_VARS)",
                    self.env.ovmf_vars.display()
                )
            })?;
        }
        Ok(Some(vars))
    }

    /// Start one VM (the DC first: its end of the lab LAN listens). CDS: the control disc
    /// first (= D: in the guest), then any mount discs.
    fn start_vm(&self, vm: &str, disk: PathBuf, cds: Vec<PathBuf>, mut o: Opts) -> Result<()> {
        o.name = format!("lab-{}-{}", self.name, vm);
        o.mem = self.v(if vm == "client" { "LAB_CLIENT_MEM" } else { "LAB_DC_MEM" }).to_string();
        o.disk = disk;
        o.cds = cds;
        o.qmp = Some(self.new_qmp_addr(vm)?);
        let serial = self.st(&format!("{}.serial.log", vm));
        write_file(&serial, b"")?;
        o.serial = Some(serial);
        if self.show {
            o.show = true;
        } else {
            o.vnc = Some(pick_vnc_display().context("no free VNC display (ports 5900-5999 all in use)")?);
        }
        let argv = qemu::command(&self.env, &o)?;
        let pid = qemu::start(&argv, &self.st(&format!("{}.qemu.log", vm)))
            .with_context(|| format!("qemu ({}) failed to start", vm))?;
        write_file(&self.st(&format!("{}.pid", vm)), format!("{}\n", pid))?;
        let vnc = match o.vnc {
            Some(n) => format!("VNC 127.0.0.1:{}", 5900 + n),
            None => "window".to_string(),
        };
        write_file(&self.st(&format!("{}.vnc", vm)), format!("{}\n", vnc))?;
        log(&format!("{} started ({})", vm, vnc));
        Ok(())
    }

    /// Boot DC + client. The client also gets the mount ISOs as extra CDs.
    fn boot_lab(&mut self, dc_disk: PathBuf, cl_disk: PathBuf, dc_cd: PathBuf, cl_cd: PathBuf) -> Result<()> {
        if !port_free(self.lan_port) {
            bail!("lab LAN port 127.0.0.1:{} is in use (another lab running? change LAB_LAN_PORT)", self.lan_port);
        }
        self.offsets.clear();
        for vm in VMS {
            let _ = std::fs::remove_file(self.st(&format!(".off-{}", vm))); // lab.sh's offset files
        }
        let dc = Opts {
            lan: Some(Lan::Server(self.lan_port)),
            lan_mac: Some(DC_MAC.into()),
            no_usernet: true,
            no_shutdown: true,
            uefi: self.uefi_vars("dc")?,
            ..Default::default()
        };
        self.start_vm("dc", dc_disk, vec![dc_cd], dc)?;
        self.settle(1.0)?;
        let cl = Opts {
            lan: Some(Lan::Client(self.lan_port)),
            lan_mac: Some(CL_MAC.into()),
            no_shutdown: true,
            uefi: self.uefi_vars("client")?,
            ..Default::default()
        };
        let mut cds = vec![cl_cd];
        cds.extend(self.mount_isos.iter().cloned());
        self.start_vm("client", cl_disk, cds, cl)
    }

    /// If the guest powered itself off (status "shutdown"; it stays that way under
    /// -no-shutdown), system_reset it so it reboots instead. Some clients power off on a
    /// reboot request (e.g. Win10 right after a domain join).
    fn reset_if_off(&self, vm: &str) -> bool {
        let st = self.vm_status(vm);
        if st == "shutdown" || st == "guest-panicked" {
            warn(&format!("{} powered off on a reboot request; resetting it", vm));
            self.qmp_cmd(vm, "system_reset");
            return true;
        }
        false
    }

    fn serial_wait(&mut self, vm: &'static str, until: &[&str], fail: &[&str], timeout: f64) -> Result<Outcome> {
        let mut off = self.offsets.get(vm).copied().unwrap_or(0);
        let w = Wait { until, fail, timeout: Some(timeout), pid: self.read_pid(vm), progress: true, events_file: None };
        let o = w.run(&self.st(&format!("{}.serial.log", vm)), &mut off);
        self.offsets.insert(vm, off);
        if o == Outcome::Interrupted {
            return Err(self.interrupted());
        }
        Ok(o)
    }

    /// Wait for an event on VM's serial log (continues where the last wait stopped). If the
    /// guest powers off mid-wait (-no-shutdown keeps QEMU alive), reset it and continue.
    /// Returns the matched line; anything else is reported and ends the run.
    fn wait_ev(&mut self, vm: &'static str, until: &str, fail: &str, label: &str) -> Result<String> {
        let u: Vec<&str> = until.split(',').collect();
        let f: Vec<&str> = fail.split(',').collect();
        loop {
            // cap each wait at 30s so we can poll the power state (a powered-off guest never
            // emits another serial line and, under -no-shutdown, its pid never dies)
            let r = self.remaining().clamp(1.0, 30.0);
            match self.serial_wait(vm, &u, &f, r)? {
                Outcome::Until(line) => return Ok(line),
                Outcome::Timeout => {
                    self.reset_if_off(vm); // reboot it if it powered off; otherwise keep waiting
                    if Instant::now() >= self.deadline {
                        return Err(self.fail(vm, Outcome::Timeout, label));
                    }
                }
                other => return Err(self.fail(vm, other, label)),
            }
        }
    }

    /// Report a failed wait (+ a screenshot if the VM still runs); the error ends `lab` with
    /// exit 3 for a timeout, else 2.
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
            "logs: {} (guest: C:\\WTL\\agent.log); the VMs keep running - lab restore {}",
            self.st(&format!("{}.serial.log", vm)).display(),
            self.name
        ));
        Exit(if o == Outcome::Timeout { 3 } else { 2 }).into()
    }

    /// Wait for the agent to start (= someone logged on). Windows sometimes leaves a domain
    /// autologon at the Ctrl+Alt+Del screen, or powers off on a reboot request; reset it (up
    /// to 3 times) and wait again.
    fn wait_logon(&mut self, vm: &'static str, label: &str) -> Result<()> {
        for attempt in 1..=3 {
            let t = self.remaining().clamp(1.0, 300.0);
            match self.serial_wait(vm, &["AGENT_START"], &["LAB_FAIL"], t)? {
                Outcome::Until(_) => return Ok(()),
                Outcome::Timeout if Instant::now() >= self.deadline => return Err(self.fail(vm, Outcome::Timeout, label)),
                Outcome::Timeout => {}
                other => return Err(self.fail(vm, other, label)),
            }
            if !self.reset_if_off(vm) {
                warn(&format!(
                    "{}: no logon within 5 min (autologon stuck at the sign-in screen?) - resetting it ({}/3)",
                    vm, attempt
                ));
                self.qmp_cmd(vm, "system_reset");
            }
        }
        self.wait_ev(vm, "AGENT_START", "LAB_FAIL", label).map(|_| ())
    }

    fn qemu_img_create(&self, backing: &Path, out: &Path) -> Result<()> {
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

    // ------------------------------------------------------------------ 0 state

    /// Install Windows for each missing base image (see [`build_base_images`]).
    fn build_bases(&self) -> Result<()> {
        let mut builds = Vec::new();
        if !self.dc_base.is_file() {
            builds.push(BaseBuild::new("dc", &self.dc_os.key, &format!("{}-dc", self.name), self.v("LAB_DC_NAME")));
        }
        if !self.cl_base.is_file() {
            builds.push(BaseBuild::new("client", &self.cl_os.key, &format!("{}-client", self.name), self.v("LAB_CLIENT_NAME")));
        }
        build_base_images(&self.root, &self.vars, &self.state, &builds, self.updates, self.show, &|s| self.nap(s))?;
        if !(self.dc_base.is_file() && self.cl_base.is_file()) {
            bail!("Windows install failed (logs: {}/build-*.log)", self.state.display());
        }
        Ok(())
    }

    fn prepare(&mut self) -> Result<()> {
        if self.zero_done.is_file() {
            return Ok(());
        }
        log(&format!(
            "=== building the 0 state of lab {} (domain {}; once, ~15-25 min) ===",
            self.name,
            self.v("LAB_DOMAIN")
        ));
        self.restore()?;
        self.build_bases()?;
        for vm in VMS {
            let base = if vm == "client" { &self.cl_base } else { &self.dc_base };
            let _ = std::fs::remove_file(self.zero_disk(vm));
            self.qemu_img_create(base, &self.zero_disk(vm))?;
        }
        let (dc_cd, cl_cd) = (self.st("dc-prep.iso"), self.st("client-prep.iso"));
        self.make_cd("dc", "dc", &dc_cd, false)?;
        self.make_cd("client", "client", &cl_cd, true)?;
        self.boot_lab(self.zero_disk("dc"), self.zero_disk("client"), dc_cd.clone(), cl_cd.clone())?;
        log("creating the domain (DC: AD DS + forest + users, one reboot)");
        let line = self.wait_ev("dc", "LAB_DC_READY", "LAB_FAIL,ERROR", "domain controller")?;
        ok(&format!("DC ready: {}", line));
        log(&format!(
            "joining the client (one reboot, then autologon as {}\\{})",
            self.v("LAB_NETBIOS"),
            self.v("LAB_USER")
        ));
        self.wait_ev("client", "LAB_JOINED", "LAB_FAIL,ERROR", "client domain join")?;
        self.wait_logon("client", "client logon after the join")?;
        let line = self.wait_ev("client", "LAB_CLIENT_READY", "LAB_FAIL,ERROR", "client")?;
        ok(&format!("client ready: {}", line));
        // the guests report LAB_IDLE (shutdown_when_ready on the client); clean ACPI
        // power-off, client first
        self.wait_ev("client", "LAB_IDLE", "LAB_FAIL,ERROR", "client")?;
        self.settle(20.0)?; // let the first logon finish (profile, group policy) before it is frozen
        for vm in STOP_ORDER {
            log(&format!("shutting the {} down cleanly", vm));
            if !self.freeze_off(vm)? {
                bail!("{} did not power off; 0 state not saved (lab rebuild {}, then retry)", vm, self.name);
            }
            let _ = std::fs::remove_file(self.st(&format!("{}.pid", vm)));
        }
        let _ = std::fs::remove_file(&dc_cd);
        let _ = std::fs::remove_file(&cl_cd);
        let done = ZeroState {
            lab: self.name.clone(),
            built_at: chrono::Local::now().format("%Y-%m-%d %H:%M").to_string(),
        };
        write_file(&self.zero_done, pyjson::dumps(&done))?;
        ok(&format!("0 state saved: {}, {}", self.zero_disk("dc").display(), self.zero_disk("client").display()));
        Ok(())
    }

    // ------------------------------------------------------------------ run / up

    fn summary(&self) {
        let vnc = |vm: &str| read_trim(&self.st(&format!("{}.vnc", vm))).unwrap_or_default();
        let nb = self.v("LAB_NETBIOS");
        eprintln!();
        eprintln!("  lab {}: domain {}", self.name, self.v("LAB_DOMAIN"));
        eprintln!(
            "    DC      {}  {}  {}   {}\\Administrator / {}",
            self.v("LAB_DC_NAME"),
            self.dc_ip,
            vnc("dc"),
            nb,
            self.env.password
        );
        eprintln!(
            "    client  {}  {}  {}   logged on: {}\\{} / {}",
            self.v("LAB_CLIENT_NAME"),
            self.cl_ip,
            vnc("client"),
            nb,
            self.v("LAB_USER"),
            self.v("LAB_USER_PASSWORD")
        );
        eprintln!(
            "  back to the 0 state: lab restore {}      another run: lab run CONFIG (restores by itself)",
            self.name
        );
    }

    fn run_lab(&mut self) -> Result<i32> {
        let run_id = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
        self.prepare()?;
        if self.cmd == "prepare" {
            return Ok(0);
        }
        // ---- boot from the 0 state
        let res = self.state.join("runs").join(&run_id);
        if self.cmd == "run" {
            std::fs::create_dir_all(&res).with_context(|| format!("cannot create {}", res.display()))?;
            self.set_last_run(&format!("runs/{}", run_id));
        }
        self.restore()?;
        for vm in VMS {
            self.qemu_img_create(&self.zero_disk(vm), &self.run_disk(vm))?;
        }
        let (dc_cd, cl_cd) = (self.st("dc.iso"), self.st("client.iso"));
        self.make_cd("dc", "dc", &dc_cd, false)?;
        self.make_cd("client", "client", &cl_cd, false)?;
        if let Ok(rd) = std::fs::read_dir(&self.state) {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().into_owned();
                if n.starts_with("mount-") && n.ends_with(".iso") {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
        if self.config.is_some() {
            self.make_mount_isos()?;
        }
        log(&format!("lab {}: booting from the 0 state", self.name));
        self.boot_lab(self.run_disk("dc"), self.run_disk("client"), dc_cd, cl_cd)?;
        self.wait_ev("dc", "LAB_DC_READY", "LAB_FAIL,ERROR", "domain controller")?;
        ok("DC ready");
        self.wait_logon("client", "client logon")?;
        self.wait_ev("client", "LAB_CLIENT_READY", "LAB_FAIL,ERROR", "client")?;
        ok(&format!("client ready: {}\\{} logged on", self.v("LAB_NETBIOS"), self.v("LAB_USER")));
        if self.cmd == "up" {
            self.summary();
            return Ok(0);
        }

        // ---- config run: mounts + the steps (run_as_admin / run_as_student), in the order written
        let config = self.config.clone().unwrap_or_default();
        let config_name = config.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        log(&format!("running config {} (mounts + steps)", config_name));
        let mut status = 0;
        // loop over events until LAB_RUN_DONE: LAB_STEP reports each script as it finishes
        loop {
            let line = self.wait_ev("client", "LAB_RUN_DONE,LAB_STEP", "LAB_FAIL,ERROR", "config run")?;
            if line.starts_with("WTL LAB_STEP") {
                let (who, script, exit) = step_fields(&line);
                if exit == "0" {
                    ok(&format!("[{}] {} -> exit 0", who, script));
                } else {
                    err(&format!("[{}] {} -> exit {}", who, script, exit));
                    status = 1;
                }
            } else if line.starts_with("WTL LAB_RUN_DONE") {
                ok("all steps done");
                break;
            }
        }
        // decode the per-step stdout/stderr the guest sent (stepNout / stepNerr streams)
        let _ = std::fs::create_dir_all(&res);
        let serial = self.st("client.serial.log");
        let _ = outputs::decode(&serial, &res);
        self.settle(2.0)?;
        let _ = self.screendump("client", &res.join("screen.png"));
        let _ = std::fs::copy(&serial, res.join("client.serial.log"));
        let _ = std::fs::copy(&config, res.join("config.conf"));
        self.summary();
        eprintln!("  config: {}  ->  {}", config_name, if status == 0 { "ALL STEPS OK" } else { "A STEP FAILED" });
        eprintln!("  results: {}  (step output: {}/step*.txt, screen.png = the desktop)", res.display(), res.display());
        Ok(status)
    }
}

/// (as, script, exit) of a `WTL LAB_STEP as=.. n=.. script=.. exit=..` line; the script is
/// URL-unescaped. Empty strings for missing keys.
fn step_fields(line: &str) -> (String, String, String) {
    if let Some(ev) = outputs::parse_event(line) {
        let g = |k: &str| ev.get(k).unwrap_or("").to_string();
        return (g("as"), g("script"), g("exit"));
    }
    // not the strict grammar: fall back to the last " key=" occurrence (lab.sh's sed)
    let g = |k: &str| {
        let pat = format!(" {}=", k);
        line.rfind(&pat)
            .map(|i| line[i + pat.len()..].split(' ').next().unwrap_or("").to_string())
            .unwrap_or_default()
    };
    (g("as"), util::url_unquote(&g("script")), g("exit"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_line() {
        let (w, s, e) = step_fields("WTL LAB_STEP as=admin n=1 script=X:%5Cmy%20setup.bat exit=0 secs=2.1");
        assert_eq!((w.as_str(), s.as_str(), e.as_str()), ("admin", "X:\\my setup.bat", "0"));
        let (w, s, e) = step_fields("WTL LAB_STEP as=student n=2 script=X:\\a.exe exit=launchfail | not found exit=0");
        assert_eq!((w.as_str(), s.as_str(), e.as_str()), ("student", "X:\\a.exe", "launchfail"));
    }

    #[test]
    fn short_socket_paths() {
        let long = PathBuf::from(format!("/{}/dc.qmp", "x".repeat(120)));
        let p = sock_path(long, "lab-2019-dc");
        assert!(p.as_os_str().len() <= 100, "{}", p.display());
        assert!(p.to_string_lossy().ends_with("wtl-lab-2019-dc.sock"));
        assert_eq!(sock_path(PathBuf::from("/s/dc.qmp"), "x"), PathBuf::from("/s/dc.qmp"));
    }

    #[test]
    fn lab_json_like_lab_sh() {
        let mounts = vec![Mount { host: "/a".into(), letter: "X".into() }];
        let steps = vec![Step { who: labconf::Who::Admin, path: "X:\\i.bat".into() }];
        let j = LabJson {
            role: "client",
            ip: "10.77.19.20",
            prefix: 24,
            dc_ip: "10.77.19.10",
            lan_mac: CL_MAC,
            domain: "lab.local",
            netbios: "LAB",
            admin_password: "WinTest!2026",
            users: vec![LabUser { name: "student", password: "Pupil!2026", groups: vec![] }],
            logon_user: "student",
            logon_password: "Pupil!2026",
            shutdown_when_ready: false,
            run: Some(LabRun { mounts: &mounts, steps: &steps, wait: 60 }),
        };
        // what lab.sh's json.dump(..., indent=1) wrote for the same values
        let want = "{\n \"role\": \"client\",\n \"ip\": \"10.77.19.20\",\n \"prefix\": 24,\n \"dc_ip\": \"10.77.19.10\",\n \
            \"lan_mac\": \"52:54:00:4c:41:20\",\n \"domain\": \"lab.local\",\n \"netbios\": \"LAB\",\n \
            \"admin_password\": \"WinTest!2026\",\n \"users\": [\n  {\n   \"name\": \"student\",\n   \"password\": \"Pupil!2026\",\n   \
            \"groups\": []\n  }\n ],\n \"logon_user\": \"student\",\n \"logon_password\": \"Pupil!2026\",\n \
            \"shutdown_when_ready\": false,\n \"run\": {\n  \"mounts\": [\n   {\n    \"host\": \"/a\",\n    \"letter\": \"X\"\n   }\n  ],\n  \
            \"steps\": [\n   {\n    \"who\": \"admin\",\n    \"path\": \"X:\\\\i.bat\"\n   }\n  ],\n  \"wait\": 60\n }\n}";
        assert_eq!(pyjson::dumps_indent(&j, 1), want);
        let none = LabJson { run: None, ..j };
        assert!(pyjson::dumps_indent(&none, 1).ends_with("\"run\": null\n}"));
    }

    #[test]
    fn option_values() {
        assert_eq!(positive_int("5"), Ok(5));
        assert!(positive_int("0").is_err());
        assert!(positive_int("-1").is_err());
        assert_eq!(whole_number("0"), Ok(0));
        assert!(whole_number("1.5").is_err());
    }
}
