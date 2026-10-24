//! `lab` - the command line of the Active Directory lab runner, and the helpers the runner
//! ([`crate::labrun`]) is built from: finding the checkout, ports, files, and installing the
//! Windows base images ([`crate::build`]).
//!
//! A lab is `labs/<name>.json` (see [`crate::labdef`]); a run is a `.json` run config that
//! names its lab. `lab run CONFIG` builds the lab's 0 state once (installing Windows first if
//! a base image is missing), then every run starts from it.

use crate::labrun::{self, RunOpts};
use crate::logx::{err, log};
use crate::util;
use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const AFTER_HELP: &str = r#"A LAB is labs/LAB.json (e.g. school, 2019): the domain, the machines, the users.
A CONFIG is a run file (e.g. examples/configs/school-smoke.json): the lab it boots, the host
folders it mounts as read-only drives, and the steps it runs on which machine as whom.
See docs/CONFIG.md, and wtl-rs/schema/ for the JSON Schema of both files.

The 0 state = Windows installed, the domain created, every member joined and its user
logging on automatically, all shut down cleanly. It is built once (by the first run, or
`lab prepare LAB`) and frozen in labs/state/LAB/; every run writes only to throwaway disks
on top of it, so a run starts from the 0 state and `lab restore` takes a second.

Exit: 0 all steps passed, 1 a step failed, 2 lab error, 3 timeout."#;

/// `lab CMD LAB|CONFIG [options]`
#[derive(clap::Args, Debug)]
#[command(
    about = "Active Directory lab: a domain controller + domain-joined members, on QEMU",
    override_usage = "lab run CONFIG [--show] [--wait S] [--timeout-min M]\n       \
                      lab up|restore|stop|status|prepare|rebuild LAB|CONFIG\n       \
                      lab doctor\n       \
                      lab check-iso [FILE --sha256 HASH]",
    after_help = AFTER_HELP
)]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Cmd,
}

/// The `lab` subcommands.
#[derive(clap::Subcommand, Debug)]
pub enum Cmd {
    /// run a config: restore the lab it names to its 0 state, boot it, mount the host
    /// folders, run the steps and report each one (builds the 0 state the first time)
    Run(Target),
    /// stop the VMs and throw away everything since the 0 state (~1 s)
    Restore(Target),
    /// boot the lab from its 0 state without running anything (the VMs stay up)
    Up(Target),
    /// power the VMs off (changes kept until the next run / restore)
    #[command(visible_alias = "down")]
    Stop(Target),
    /// show the lab's 0 state, machines and last run
    Status(Target),
    /// build the 0 state (run does this automatically the first time)
    Prepare(Target),
    /// delete the 0 state; the next run builds a new domain (the Windows installs stay)
    #[command(visible_alias = "reset")]
    Rebuild(Target),
    /// check this host: QEMU and acceleration per CPU, UEFI firmware, disk space, and which
    /// ISO serves which OS (by the Windows build and CPU inside it)
    Doctor,
    /// check the ISOs you downloaded by hand: the Windows build and CPU inside each one, and
    /// with --sha256 HASH that one file's SHA-256 (from Microsoft's download page)
    CheckIso(CheckIso),
}

/// `lab check-iso [FILE] [--sha256 HASH]`
#[derive(clap::Args, Debug)]
pub struct CheckIso {
    /// one ISO (default: every ISO in WTL_ISO_DIR)
    pub file: Option<PathBuf>,
    /// the SHA-256 the download page lists for FILE; the file must match it
    #[arg(long, value_name = "HASH", requires = "file")]
    pub sha256: Option<String>,
}

/// What a command acts on, and its options.
#[derive(clap::Args, Debug)]
pub struct Target {
    /// LAB (labs/LAB.json, e.g. school) or a CONFIG run file; `run` needs a CONFIG
    #[arg(value_name = "LAB|CONFIG")]
    pub target: String,
    /// open the VM windows (default: headless, VNC addresses printed)
    #[arg(long)]
    pub show: bool,
    /// include Windows Update when a base image has to be installed (hours)
    #[arg(long)]
    pub updates: bool,
    /// seconds a step may run before it counts as "still running" (default 60; overrides
    /// the config's own `wait`)
    #[arg(long, value_name = "S", value_parser = positive_int)]
    pub wait: Option<i64>,
    /// give up after M minutes (default: the config's `timeout_min`, else 120)
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

/// `lab CMD ...`; returns the exit code.
pub fn main(cli: &Cli) -> i32 {
    match run_cli(cli) {
        Ok(code) => code,
        Err(e) => {
            err(&format!("{:#}", e));
            2
        }
    }
}

fn run_cli(cli: &Cli) -> Result<i32> {
    let (cmd, t) = match &cli.cmd {
        Cmd::Doctor => return doctor(),
        Cmd::CheckIso(c) => return check_iso(c),
        Cmd::Run(t) => ("run", t),
        Cmd::Restore(t) => ("restore", t),
        Cmd::Up(t) => ("up", t),
        Cmd::Stop(t) => ("stop", t),
        Cmd::Status(t) => ("status", t),
        Cmd::Prepare(t) => ("prepare", t),
        Cmd::Rebuild(t) => ("rebuild", t),
    };
    let root = find_root()?;
    let opts = RunOpts { show: t.show, updates: t.updates, wait: t.wait, timeout_min: t.timeout_min };
    let target = t.target.as_str();
    // a run config: an existing .json file that is not itself a lab
    let is_file = Path::new(target).is_file();
    if cmd == "run" {
        if !is_file {
            bail!("usage: lab run CONFIG  (a run .json file, e.g. examples/configs/school-smoke.json; not found: {})", target);
        }
        let config = util::canon(Path::new(target));
        let (lab_file, lab, run) = labrun::load_run(&config).context("config error")?;
        return Ok(labrun::run(cmd, &lab_file, Some(config), Some(run), lab, &opts));
    }
    // the other commands take a LAB name, a lab .json file, or a run config (which names its lab)
    let lab_file = if is_file {
        let p = util::canon(Path::new(target));
        match labrun::load_run(&p) {
            Ok((lab_file, _, _)) => lab_file,
            Err(_) => p, // not a run config: treat it as a lab file
        }
    } else {
        if !util::is_safe_name(target) {
            bail!("bad lab name {:?}", target);
        }
        let f = root.join("labs").join(format!("{}.json", target));
        if !f.is_file() {
            bail!("no lab {} (labs: {})", f.display(), lab_names(&root).join(" "));
        }
        f
    };
    let lab = labrun::load_lab(&lab_file).context("lab error")?;
    Ok(labrun::run(cmd, &lab_file, None, None, lab, &opts))
}

/// `lab doctor`: what this host can run, and which ISO serves which OS. Exit 0 if the host
/// can run at least one OS it has an ISO for, else 1.
fn doctor() -> Result<i32> {
    use crate::qemu::{Accel, Arch, Env};
    let root = find_root()?;
    let vars = crate::qemu::seed_vars(&root);
    let env = Env::from_vars(&root, &vars)?;
    let mark = |good: bool| if good { "ok  " } else { "MISS" };
    println!("host: {} {}, win-test-lab {}", std::env::consts::OS, std::env::consts::ARCH, root.display());
    let mut runnable = Vec::new();
    for arch in [Arch::X64, Arch::Arm64] {
        let q = env.qemu_for(arch);
        let have = util::which(q).is_some();
        let accel = env.accel_for(arch);
        let how = match accel {
            Accel::Tcg => "emulated (TCG): works, several times slower".to_string(),
            a => format!("hardware-accelerated ({:?})", a).to_lowercase(),
        };
        println!("{} {:5} guests: {} - {}", mark(have), arch.as_str(), q, if have { how } else { "not found (install QEMU)".into() });
        let (code, vars_f) = match arch {
            Arch::X64 => (&env.ovmf_code, &env.ovmf_vars),
            Arch::Arm64 => (&env.aavmf_code, &env.aavmf_vars),
        };
        let fw = code.is_file() && vars_f.is_file();
        println!("     {:5} UEFI firmware: {} {}", arch.as_str(), mark(fw), if fw { code.display().to_string() } else { format!("{} / {} not found", code.display(), vars_f.display()) });
        if have {
            runnable.push(arch);
        }
    }
    if env.accel == Accel::Kvm {
        let kvm = std::fs::OpenOptions::new().read(true).write(true).open("/dev/kvm").is_ok();
        println!("{} /dev/kvm {}", mark(kvm), if kvm { "is writable" } else { "missing or not writable (./setup-host.sh)" });
    }
    let qi = util::which(&env.qemu_img).is_some();
    println!("{} qemu-img: {}", mark(qi), env.qemu_img);
    match util::free_gb(&env.images_dir) {
        Some(g) => println!("{} {} GiB free for {} (an install needs 20-30 GiB, a lab's 0 state a few more)", mark(g >= 30), g, env.images_dir.display()),
        None => println!("     free space for {}: unknown", env.images_dir.display()),
    }
    // the ISOs
    println!("ISOs in {}:", env.iso_dir.display());
    let mut isos: Vec<PathBuf> = std::fs::read_dir(&env.iso_dir)
        .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("iso"))).collect())
        .unwrap_or_default();
    isos.sort();
    for p in &isos {
        match crate::winiso::iso_build_arch(p) {
            Ok((b, a)) => println!("     {:6} {:5} {}", b, a, util::basename(&p.to_string_lossy())),
            Err(e) => println!("     -      -     {} ({:#})", util::basename(&p.to_string_lossy()), e),
        }
    }
    if isos.is_empty() {
        println!("     none (see docs/ISOS.md)");
    }
    println!("OS keys (lib/oslist.conf):");
    let table = std::fs::read_to_string(root.join("lib").join("oslist.conf")).unwrap_or_default();
    let mut usable = 0;
    for os in crate::oslist::parse_table(&table) {
        let arch = Arch::parse(&os.arch).unwrap_or_default();
        let iso = crate::build::find_iso(&root, &env.iso_dir, &os);
        let image = env.images_dir.join(format!("{}-client.qcow2", os.key)).is_file() || env.images_dir.join(format!("{}-dc.qcow2", os.key)).is_file();
        let speed = if env.accel_for(arch) == Accel::Tcg { "emulated" } else { "native" };
        let ok_ = iso.is_some() && runnable.contains(&arch);
        if ok_ {
            usable += 1;
        }
        println!(
            "{} {:12} {:5} {:8} {}{}",
            mark(ok_),
            os.key,
            os.arch,
            speed,
            iso.map(|p| util::basename(&p.to_string_lossy()).to_string()).unwrap_or_else(|| "no ISO".into()),
            if image { "  (base image installed)" } else { "" }
        );
    }
    Ok(if usable > 0 { 0 } else { 1 })
}

/// `lab check-iso`: what each ISO is (build, CPU, editions) and which OS key it serves; with
/// --sha256, that the file is byte-for-byte the one Microsoft published. Exit 1 on a mismatch
/// or an unreadable file.
fn check_iso(c: &CheckIso) -> Result<i32> {
    let root = find_root()?;
    let vars = crate::qemu::seed_vars(&root);
    let env = crate::qemu::Env::from_vars(&root, &vars)?;
    let files: Vec<PathBuf> = match &c.file {
        Some(f) => vec![f.clone()],
        None => {
            let mut v: Vec<PathBuf> = std::fs::read_dir(&env.iso_dir)
                .with_context(|| format!("cannot read {}", env.iso_dir.display()))?
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("iso")))
                .collect();
            v.sort();
            v
        }
    };
    let table = crate::oslist::parse_table(&std::fs::read_to_string(root.join("lib").join("oslist.conf")).unwrap_or_default());
    let mut bad = 0;
    for f in &files {
        println!("{}", f.display());
        match crate::winiso::Media::open(f).and_then(|mut m| m.images()) {
            Ok(imgs) => {
                let first = imgs.first();
                let (b, a) = (first.and_then(|i| i.build).unwrap_or(0), first.map(|i| i.arch_name()).unwrap_or_default());
                let keys: Vec<&str> = table.iter().filter(|o| o.build == b.to_string() && o.arch == a).map(|o| o.key.as_str()).collect();
                println!("  Windows build {}, {}; serves: {}", b, a, if keys.is_empty() { "no OS key (not in lib/oslist.conf)".to_string() } else { keys.join(" ") });
                for i in &imgs {
                    println!("    {:2} {:24} {:12} {}", i.index, i.edition_id, i.installation_type, i.name);
                }
            }
            Err(e) => {
                println!("  not a Windows install disc: {:#}", e);
                bad += 1;
            }
        }
        if let Some(want) = &c.sha256 {
            print!("  SHA-256 ");
            let _ = std::io::stdout().flush();
            let got = crate::sha256::file_hex(f)?;
            if got.eq_ignore_ascii_case(want.trim()) {
                println!("{} matches", got);
            } else {
                println!("{} DOES NOT MATCH {}", got, want.trim());
                bad += 1;
            }
        }
    }
    Ok(if bad > 0 { 1 } else { 0 })
}

/// The lab names in labs/ (each labs/NAME.json), sorted.
fn lab_names(root: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(root.join("labs"))
        .map(|rd| rd.flatten().filter_map(|e| e.file_name().to_str().and_then(|n| n.strip_suffix(".json")).map(str::to_string)).collect())
        .unwrap_or_default();
    names.sort();
    names
}

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

/// A small text file's content without its trailing newline.
pub(crate) fn read_trim(p: &Path) -> Option<String> {
    std::fs::read_to_string(p).ok().map(|s| s.trim_end_matches(['\r', '\n']).to_string())
}

/// Print a file to stderr, indented (e.g. QEMU's log after it died).
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
    /// the Windows computer name baked into the image (the lab renames each machine later)
    pub computer: String,
}

impl BaseBuild {
    pub(crate) fn new(tag: &str, os: &str, image: &str, computer: &str) -> BaseBuild {
        BaseBuild { tag: tag.into(), os: os.into(), image: image.into(), computer: computer.into() }
    }
}

/// Install Windows into each base image (unattended, in parallel): each build is this program
/// again (`wtl-rs build-image`, see [`crate::build`]), its output tee'd to
/// LOG_DIR/build-<tag>.log and shown prefixed with `[tag]`. `vars` supplies the WTL_* settings
/// passed on; `nap` sleeps (and fails on Ctrl-C). The caller checks that the images exist
/// afterwards.
pub(crate) fn build_base_images(
    _root: &Path,
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
    // WTL_BUILDER: what runs one build (the tests point it at a fake); default this program
    let me = std::env::current_exe().context("cannot find this program's path")?;
    let builder = vars.get("WTL_BUILDER").cloned().or_else(|| std::env::var("WTL_BUILDER").ok()).filter(|b| !b.is_empty());
    let mut kids = Vec::new();
    for (i, b) in builds.iter().enumerate() {
        let mut cmd = match &builder {
            Some(x) => Command::new(x),
            None => {
                let mut c = Command::new(&me);
                c.arg("build-image");
                c
            }
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
        let child = cmd.spawn().with_context(|| format!("cannot run the image builder ({})", builder.as_deref().unwrap_or("wtl-rs build-image")))?;
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

/// `wtl-rs build-image OS [--name NAME] [--computer-name CN] [--no-updates] [--show] [--iso PATH]`
#[derive(clap::Args, Debug)]
#[command(
    about = "Install Windows into a base image, unattended: images/<NAME>.qcow2",
    after_help = "OS is a key of lib/oslist.conf (2012 2016 2019 2022 2025 win10 win11 win11-arm64).\n\
                  The ISO is found in WTL_ISO_DIR by the Windows build and CPU inside it (or\n\
                  images-list.conf, or --iso). `lab` runs this itself for a missing base image.\n\
                  Exit: 0 built (or already there), 2 error, 3 timeout."
)]
pub struct BuildCli {
    /// the OS key
    pub os: String,
    /// image file images/NAME.qcow2 (default: the OS key)
    #[arg(long)]
    pub name: Option<String>,
    /// the Windows computer name (default WTL-<os>, max 15 chars)
    #[arg(long, value_name = "CN")]
    pub computer_name: Option<String>,
    /// skip Windows Update
    #[arg(long)]
    pub no_updates: bool,
    /// open the VM window (default: headless, VNC address printed)
    #[arg(long)]
    pub show: bool,
    /// use this ISO
    #[arg(long, value_name = "PATH")]
    pub iso: Option<PathBuf>,
    /// give up after H hours (default 12)
    #[arg(long, value_name = "H", default_value_t = 12.0)]
    pub timeout_h: f64,
}

/// `wtl-rs build-image ...`; returns the exit code.
pub fn build_main(a: &BuildCli) -> i32 {
    let r = (|| -> Result<()> {
        let root = find_root()?;
        let os = crate::oslist::lookup(&root.join("lib").join("oslist.conf"), &a.os).ok_or_else(|| {
            let keys: Vec<String> = std::fs::read_to_string(root.join("lib").join("oslist.conf"))
                .map(|t| crate::oslist::parse_table(&t).into_iter().map(|o| o.key).collect())
                .unwrap_or_default();
            anyhow::anyhow!("unknown OS '{}' (known: {})", a.os, keys.join(" "))
        })?;
        let image = a.name.clone().unwrap_or_else(|| os.key.clone());
        if !util::is_safe_name(&image) {
            bail!("--name: letters, digits, . _ - only");
        }
        let computer = a.computer_name.clone().unwrap_or_else(|| format!("WTL-{}", os.key));
        if computer.is_empty() || computer.len() > 15 || !computer.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
            bail!("--computer-name: 1-15 letters, digits, -");
        }
        let vars = crate::qemu::seed_vars(&root);
        let env = crate::qemu::Env::from_vars(&root, &vars)?;
        let qmp_tcp = match std::env::var("WTL_QMP").unwrap_or_default().as_str() {
            "tcp" => true,
            "unix" => false,
            _ => !cfg!(unix),
        };
        crate::sys::install_interrupt_handler();
        crate::build::build(
            &env,
            &crate::build::Build {
                os: &os,
                image: &image,
                computer: &computer,
                updates: !a.no_updates,
                show: a.show,
                iso: a.iso.clone(),
                timeout_h: a.timeout_h,
            },
            qmp_tcp,
        )
    })();
    match r {
        Ok(()) => 0,
        Err(e) => match e.downcast_ref::<crate::build::BuildError>() {
            Some(b) => b.0,
            None => {
                err(&format!("{:#}", e));
                2
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_socket_paths() {
        let long = PathBuf::from(format!("/{}/dc.qmp", "x".repeat(120)));
        let p = sock_path(long, "lab-2019-dc");
        assert!(p.as_os_str().len() <= 100, "{}", p.display());
        assert!(p.to_string_lossy().ends_with("wtl-lab-2019-dc.sock"));
        assert_eq!(sock_path(PathBuf::from("/s/dc.qmp"), "x"), PathBuf::from("/s/dc.qmp"));
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
