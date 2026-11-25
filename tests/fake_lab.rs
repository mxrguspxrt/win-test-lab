//! End-to-end test of `wtl` without KVM or Windows.
//!
//! This one binary plays three parts:
//!   * the test driver (no args / cargo's args): builds a throwaway win-test-lab tree in a
//!     temp dir and runs `wtl ...` against it, checking exit codes, messages and files;
//!   * a fake `qemu-system-x86_64` (argv has `-machine`): serves QMP (unix socket or TCP)
//!     and simulates guest/lab.ps1 by reading the control disc's LAB/lab.json and the mount
//!     discs, writing the WTL serial events a real guest sends;
//!   * a fake `qemu-img` (argv starts with `create`).
//!
//! Scripts on a mounted folder steer the fake guest: a file containing FAKE_EXIT=N exits N,
//! FAKE_LAB_FAIL makes the guest report LAB_FAIL, FAKE_HANG stops the guest, FAKE_CRASH
//! kills the VM. FAKE_VNC_STEAL=1 in the environment takes the asked VNC display before the
//! fake QEMU binds it (another program won the race), so `-vnc ...,to=N` must move on.

use base64::Engine;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use wtl::mkiso::IsoReader;

fn b64encode(b: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(b)
}

/// The guest's ConvertTo-WtlValue: keep `[A-Za-z0-9._~:\\/+,@()-]+`, else
/// `[Uri]::EscapeDataString` (RFC 3986 unreserved kept, everything else %XX).
fn wtl_value(v: &str) -> String {
    let plain = |b: u8| b.is_ascii_alphanumeric() || b"._~:\\/+,@()-".contains(&b);
    if !v.is_empty() && v.bytes().all(plain) {
        return v.to_string();
    }
    v.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{:02X}", b) })
        .collect()
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = if args.first().map(String::as_str) == Some("create") {
        fake_qemu_img(&args)
    } else if args.iter().any(|a| a == "-machine") {
        fake_qemu(&args)
    } else {
        driver(&args)
    };
    std::process::exit(code);
}

// =====================================================================================
// fake qemu-img

fn fake_qemu_img(args: &[String]) -> i32 {
    log_argv("qemu-img", args);
    let (mut backing, mut out) = (None, None);
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-q" => i += 1,
            "-f" | "-F" | "-o" => i += 2,
            "-b" => {
                backing = args.get(i + 1).cloned();
                i += 2;
            }
            a if out.is_none() => {
                out = Some(a.to_string());
                i += 1;
            }
            _ => i += 1, // size
        }
    }
    let Some(out) = out else {
        eprintln!("qemu-img: Expecting image file name");
        return 1;
    };
    if let Some(b) = &backing {
        if !Path::new(b).is_file() {
            eprintln!("qemu-img: {}: Could not open '{}': No such file or directory", out, b);
            return 1;
        }
    }
    if std::fs::write(&out, format!("QFI\u{fb} fake qcow2 backing={}\n", backing.unwrap_or_default())).is_err() {
        eprintln!("qemu-img: {}: Could not create image", out);
        return 1;
    }
    0
}

fn log_argv(prog: &str, args: &[String]) {
    if let Ok(p) = std::env::var("FAKE_LOG") {
        if let Ok(mut f) = std::fs::OpenOptions::new().append(true).create(true).open(p) {
            let _ = writeln!(f, "{}\t{}", prog, args.join("\u{1f}"));
        }
    }
}

// =====================================================================================
// fake qemu-system-x86_64

/// Split a qemu option string on ',' honouring ',,' escapes.
fn split_opts(s: &str) -> Vec<String> {
    let mut parts = vec![String::new()];
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == ',' {
            if it.peek() == Some(&',') {
                it.next();
                parts.last_mut().unwrap().push(',');
            } else {
                parts.push(String::new());
            }
        } else {
            parts.last_mut().unwrap().push(c);
        }
    }
    parts
}

fn kv(s: &str) -> BTreeMap<String, String> {
    split_opts(s)
        .into_iter()
        .map(|p| match p.split_once('=') {
            Some((k, v)) => (k.to_string(), v.to_string()),
            None => (p, "on".to_string()),
        })
        .collect()
}

#[derive(Default)]
struct Machine {
    name: String,
    disk: Option<PathBuf>,
    cds: BTreeMap<usize, PathBuf>,
    serial: Option<PathBuf>,
    qmp: Option<String>,
    no_shutdown: bool,
    /// the VM has a lab LAN NIC (-netdev dgram,id=lan0)
    lan: bool,
    /// -machine (pc, q35, virt)
    machine: String,
}

struct Shared {
    status: String,
}

const NO_ARG: [&str; 3] = ["-enable-kvm", "-usb", "-no-shutdown"];

fn die(msg: &str) -> ! {
    eprintln!("qemu-system-x86_64: {}", msg);
    std::process::exit(1)
}

/// The machine's control disc: the CD whose volume label is WTL_LAB, whatever its drive index.
/// (A real guest scans every CD drive; the index differs between AHCI and the IDE layout used
/// for imported VHDs.)
fn control_disc(m: &Machine) -> Option<PathBuf> {
    m.cds.values().find(|c| IsoReader::open(c, true).map(|iso| iso.volid == "WTL_LAB").unwrap_or(false)).cloned()
}

fn fake_qemu(args: &[String]) -> i32 {
    log_argv("qemu", args);
    let mut m = Machine::default();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if NO_ARG.contains(&a) {
            if a == "-no-shutdown" {
                m.no_shutdown = true;
            }
            i += 1;
            continue;
        }
        let v = args.get(i + 1).cloned().unwrap_or_else(|| die(&format!("{} requires an argument", a)));
        i += 2;
        match a {
            "-name" => m.name = v,
            "-drive" => {
                let d = kv(&v);
                let Some(file) = d.get("file") else { continue };
                if d.get("media").map(String::as_str) == Some("cdrom") {
                    // x64: index=N on IDE; arm64: id=cdN, attached as usb-storage
                    let idx: usize = d
                        .get("index")
                        .or_else(|| d.get("id").and_then(|i| i.strip_prefix("cd").map(|_| i)))
                        .and_then(|x| x.trim_start_matches("cd").parse().ok())
                        .unwrap_or(m.cds.len());
                    m.cds.insert(idx, PathBuf::from(file));
                } else if d.get("id").map(String::as_str) == Some("disk0") {
                    m.disk = Some(PathBuf::from(file));
                }
            }
            "-machine" => m.machine = v,
            // arm64: the serial port is a file chardev (shared with the guest's TCP channel)
            "-chardev" => {
                let d = kv(&v);
                if d.get("id").map(String::as_str) == Some("ser0") {
                    m.serial = d.get("path").map(PathBuf::from);
                }
            }
            "-serial" => {
                if let Some(f) = v.strip_prefix("file:") {
                    m.serial = Some(PathBuf::from(f));
                }
            }
            "-qmp" => m.qmp = Some(split_opts(&v)[0].clone()),
            "-vnc" => fake_vnc(&v),
            "-netdev" if v.starts_with("dgram,id=lan0") => m.lan = true,
            _ => {}
        }
    }
    let disk = m.disk.clone().unwrap_or_else(|| die("no disk"));
    if !disk.is_file() {
        die(&format!("-drive id=disk0: Could not open '{}': No such file or directory", disk.display()));
    }
    for c in m.cds.values() {
        if !c.is_file() {
            die(&format!("-drive file={}: Could not open: No such file or directory", c.display()));
        }
    }
    let serial = m.serial.clone().unwrap_or_else(|| die("no -serial file:"));
    let qmp = m.qmp.clone().unwrap_or_else(|| die("no -qmp"));

    let pidfile = std::env::var("FAKE_PIDDIR").ok().map(|d| PathBuf::from(d).join(std::process::id().to_string()));
    if let Some(p) = &pidfile {
        let _ = std::fs::write(p, &m.name);
    }
    let _ = PIDFILE.set(pidfile);

    // watchdog: never outlive a broken test run by much
    let life: u64 = std::env::var("FAKE_QEMU_MAX_LIFETIME").ok().and_then(|s| s.parse().ok()).unwrap_or(180);
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(life));
        fake_exit(9)
    });

    let shared = Arc::new(Mutex::new(Shared { status: "running".into() }));
    start_qmp(&qmp, shared.clone(), m.no_shutdown);

    let mut g = Guest { serial, name: m.name.clone() };
    g.raw("\u{0}\u{0}SeaBIOS (version fake)\r\nBooting from Hard Disk...\r\n");
    let r = g.run(&m);
    if let Err(e) = r {
        g.emit(&format!("WTL LAB_FAIL | fake guest: {}", e));
    }
    // idle until quit / powerdown / the watchdog
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}

static PIDFILE: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();

/// the VNC port this fake QEMU listens on (for query-vnc), and the listeners it holds
static VNC_PORT: std::sync::OnceLock<u16> = std::sync::OnceLock::new();
static VNC_HELD: std::sync::OnceLock<Vec<std::net::TcpListener>> = std::sync::OnceLock::new();

/// `-vnc HOST:N[,to=L]` like QEMU: listen on the first free port 5900+N .. 5900+L, else die.
fn fake_vnc(v: &str) {
    let mut parts = v.split(',');
    let (host, disp) = parts.next().and_then(|hd| hd.rsplit_once(':')).unwrap_or_else(|| die(&format!("-vnc {}: bad address", v)));
    let disp: u16 = disp.parse().unwrap_or_else(|_| die(&format!("-vnc {}: bad display", v)));
    let to: u16 = parts.find_map(|p| p.strip_prefix("to=")).and_then(|x| x.parse().ok()).unwrap_or(disp).max(disp);
    let mut held = Vec::new();
    if std::env::var("FAKE_VNC_STEAL").is_ok() {
        if let Ok(l) = std::net::TcpListener::bind((host, 5900 + disp)) {
            held.push(l);
        }
    }
    for d in disp..=to {
        if let Ok(l) = std::net::TcpListener::bind((host, 5900 + d)) {
            held.push(l);
            let _ = VNC_PORT.set(5900 + d);
            let _ = VNC_HELD.set(held);
            return;
        }
    }
    die(&format!("-vnc {}: Failed to find an available port: Address already in use", v))
}

/// Exit the fake VM (unregistering it from the leak tracker first).
fn fake_exit(code: i32) -> ! {
    if let Some(Some(p)) = PIDFILE.get() {
        let _ = std::fs::remove_file(p);
    }
    std::process::exit(code)
}

fn start_qmp(addr: &str, shared: Arc<Mutex<Shared>>, no_shutdown: bool) {
    type Conn = Box<dyn ReadWrite>;
    let accept: Box<dyn FnMut() -> Option<Conn> + Send> = if let Some(hp) = addr.strip_prefix("tcp:") {
        let l = std::net::TcpListener::bind(hp).unwrap_or_else(|e| die(&format!("-qmp {}: {}", addr, e)));
        Box::new(move || l.accept().ok().map(|(s, _)| Box::new(s) as Conn))
    } else {
        unix_listener(addr)
    };
    let mut accept = accept;
    std::thread::spawn(move || {
        while let Some(c) = accept() {
            let shared = shared.clone();
            std::thread::spawn(move || qmp_conn(c, shared, no_shutdown));
        }
    });
}

#[cfg(unix)]
fn unix_listener(addr: &str) -> Box<dyn FnMut() -> Option<Box<dyn ReadWrite>> + Send> {
    let Some(p) = addr.strip_prefix("unix:") else { die(&format!("bad -qmp {}", addr)) };
    let _ = std::fs::remove_file(p);
    let l = std::os::unix::net::UnixListener::bind(p).unwrap_or_else(|e| die(&format!("-qmp {}: {}", addr, e)));
    Box::new(move || l.accept().ok().map(|(s, _)| Box::new(s) as Box<dyn ReadWrite>))
}

#[cfg(not(unix))]
fn unix_listener(addr: &str) -> Box<dyn FnMut() -> Option<Box<dyn ReadWrite>> + Send> {
    die(&format!("bad -qmp {} (no unix sockets here)", addr))
}

trait ReadWrite: Read + Write + Send {
    fn dup(&self) -> Box<dyn ReadWrite>;
}

impl ReadWrite for std::net::TcpStream {
    fn dup(&self) -> Box<dyn ReadWrite> {
        Box::new(self.try_clone().unwrap())
    }
}

#[cfg(unix)]
impl ReadWrite for std::os::unix::net::UnixStream {
    fn dup(&self) -> Box<dyn ReadWrite> {
        Box::new(self.try_clone().unwrap())
    }
}

fn qmp_conn(c: Box<dyn ReadWrite>, shared: Arc<Mutex<Shared>>, no_shutdown: bool) {
    let mut w = c.dup();
    let send = |w: &mut Box<dyn ReadWrite>, j: &str| {
        let _ = w.write_all(j.as_bytes());
        let _ = w.write_all(b"\r\n");
        let _ = w.flush();
    };
    send(&mut w, r#"{"QMP": {"version": {"qemu": {"micro": 0, "minor": 1, "major": 10}, "package": "fake"}, "capabilities": ["oob"]}}"#);
    let old = std::env::var("FAKE_OLD_QEMU").is_ok();
    for line in BufReader::new(c).lines() {
        let Ok(line) = line else { return };
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            send(&mut w, r#"{"error": {"class": "GenericError", "desc": "JSON parse error"}}"#);
            continue;
        };
        let cmd = s(&msg, "execute").to_string();
        let args = msg.get("arguments").cloned().unwrap_or(json!({}));
        // an unsolicited event first, which a client must skip
        send(&mut w, r#"{"timestamp": {"seconds": 1, "microseconds": 2}, "event": "RTC_CHANGE", "data": {"offset": 0}}"#);
        match cmd.as_str() {
            "qmp_capabilities" => send(&mut w, r#"{"return": {}}"#),
            "query-status" => {
                let st = shared.lock().unwrap().status.clone();
                send(&mut w, &format!(r#"{{"return": {{"status": "{}", "singlestep": false, "running": {}}}}}"#, st, st == "running"));
            }
            "system_powerdown" => {
                send(&mut w, r#"{"return": {}}"#);
                let shared = shared.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(300));
                    if !no_shutdown {
                        fake_exit(0);
                    }
                    shared.lock().unwrap().status = "shutdown".into();
                });
            }
            "query-vnc" => match VNC_PORT.get() {
                Some(p) => send(
                    &mut w,
                    &format!(
                        r#"{{"return": {{"enabled": true, "host": "127.0.0.1", "family": "ipv4", "service": "{}", "auth": "none", "clients": []}}}}"#,
                        p
                    ),
                ),
                None => send(&mut w, r#"{"return": {"enabled": false}}"#),
            },
            "system_reset" => {
                shared.lock().unwrap().status = "running".into();
                send(&mut w, r#"{"return": {}}"#);
            }
            "quit" => {
                send(&mut w, r#"{"return": {}}"#);
                fake_exit(0);
            }
            "screendump" => {
                let file = s(&args, "filename").to_string();
                let png_wanted = s(&args, "format") == "png";
                if png_wanted && old {
                    send(&mut w, r#"{"error": {"class": "GenericError", "desc": "Parameter 'format' is unexpected"}}"#);
                    continue;
                }
                let ppm = [b"P6\n4 2\n255\n".to_vec(), vec![200u8; 4 * 2 * 3]].concat();
                let data = if png_wanted { wtl::qmp::ppm_to_png(&ppm).unwrap() } else { ppm };
                if std::fs::write(&file, data).is_ok() {
                    send(&mut w, r#"{"return": {}}"#);
                } else {
                    send(&mut w, r#"{"error": {"class": "GenericError", "desc": "Could not open file"}}"#);
                }
            }
            _ => send(&mut w, &format!(r#"{{"error": {{"class": "CommandNotFound", "desc": "The command {} has not been found"}}}}"#, cmd)),
        }
    }
}

struct Guest {
    serial: PathBuf,
    name: String,
}

impl Guest {
    fn raw(&mut self, s: &str) {
        let mut f = std::fs::OpenOptions::new().append(true).create(true).open(&self.serial).unwrap();
        let _ = f.write_all(s.as_bytes());
    }

    fn emit(&mut self, line: &str) {
        self.raw(&format!("{}\r\n", line));
        std::thread::sleep(Duration::from_millis(20));
    }

    fn run(&mut self, m: &Machine) -> Result<(), String> {
        // the control disc is whichever CD is labelled WTL_LAB (a real guest scans every CD
        // drive; its IDE/AHCI index varies - VHD-import OSes push it off index 0)
        let cd = control_disc(m).ok_or("no control disc")?;
        let iso = IsoReader::open(&cd, true).map_err(|e| e.to_string())?;
        // lab / labsvc / agent, in PowerShell (.ps1) or - for Server 2003 - VBScript (.vbs)
        for stem in ["LAB/lab", "LAB/labsvc", "LAB/agent"] {
            if iso.read(&format!("{}.ps1", stem)).is_none() && iso.read(&format!("{}.vbs", stem)).is_none() {
                return Err(format!("{}.ps1/.vbs missing on the control disc", stem));
            }
        }
        let cfg = iso.read("LAB/lab.json").ok_or("LAB/lab.json missing")?;
        if !cfg.is_ascii() {
            return Err("lab.json is not pure ASCII (PowerShell 4 would misread it)".into());
        }
        let cfg: Value = serde_json::from_slice(cfg).map_err(|e| e.to_string())?;
        if s(&cfg, "fmt") != "multi" {
            return Err(format!("lab.json fmt {:?} (want \"multi\")", s(&cfg, "fmt")));
        }
        self.run_multi(m, &cfg)
    }

    /// Simulate a multi-machine (fmt=multi) guest: DC promotes, a member joins/logs on, then
    /// each machine with a run runs its steps and reports LAB_STEP machine=.. .
    fn run_multi(&mut self, m: &Machine, cfg: &Value) -> Result<(), String> {
        let role = s(cfg, "role").to_string();
        let machine = s(cfg, "machine").to_string();
        // the Windows computer name (an environment's own, or the lab's)
        let host = if s(cfg, "hostname").is_empty() { machine.clone() } else { s(cfg, "hostname").to_string() };
        // like guest/lab.ps1: DOMAIN\user in AD mode, COMPUTER\user (a local account) in a
        // workgroup; Administrator when the machine has no logon user
        let realm = if s(cfg, "mode") == "workgroup" { host.clone() } else { s(cfg, "netbios").to_string() };
        let logon = if s(cfg, "logon_user").is_empty() { "Administrator" } else { s(cfg, "logon_user") };
        let user = format!("{}%5C{}", realm, logon);
        // a workgroup machine without a lab LAN must not get a lab NIC
        if !cfg.get("lan").and_then(Value::as_bool).unwrap_or(true) && m.lan {
            return Err(format!("{}: lab.json says lan=false but QEMU got a lab NIC", machine));
        }
        // build-time config (roles beyond ad-ds, features or setup) baked into the 0 state
        let arr = |k: &str| cfg.get(k).and_then(Value::as_array).map(|a| a.len()).unwrap_or(0);
        let roles_non_adds =
            cfg.get("roles").and_then(Value::as_array).map(|a| a.iter().filter(|r| r.as_str() != Some("ad-ds")).count()).unwrap_or(0);
        let has_config = roles_non_adds > 0 || arr("features") > 0 || arr("setup") > 0;
        self.emit(&format!("WTL AGENT_START os=fake build=17763 machine={}", machine));
        // the boot that renames an environment's machine: like guest/lab.ps1's Invoke-LabRename
        let rename = cfg.get("rename").filter(|r| !r.is_null()).cloned();
        if let Some(rn) = &rename {
            let (from, to) = (s(rn, "from").to_string(), s(rn, "to").to_string());
            // what the disc asked for, so the test can check every machine's part
            let flag = |k: &str| if rn[k] == json!(true) { "1" } else { "0" };
            let peers: Vec<String> = rn["peers"]
                .as_object()
                .map(|m| m.iter().map(|(o, n)| format!("{}>{}", o, n.as_str().unwrap_or(""))).collect())
                .unwrap_or_default();
            self.emit(&format!(
                "WTL LAB_PHASE | rename plan machine={} from={} to={} ca={} admin={} dhcp={} iis={} reenroll={} reenroll_user={} ca_name={} peers={}",
                machine, from, to, flag("ca"), flag("admin"), flag("dhcp"), wtl_value(s(rn, "iis_site_path")), flag("reenroll"), flag("reenroll_user"), wtl_value(s(rn, "ca_name")), peers.join(",")
            ));
            if std::env::var("FAKE_RENAME_FAIL").is_ok() && from != to {
                return Err(format!("{}: Rename-Computer {} failed (FAKE_RENAME_FAIL)", machine, to));
            }
            if from != to {
                self.emit(&format!("WTL LAB_PHASE | renaming this computer {} -> {}", from, to));
                if rn["ca"] == json!(true) {
                    self.emit("WTL LAB_PHASE | uninstalling the CA, to install it again under the new name");
                }
                // a DC: netdom add + make primary, reboot; a member: rename, reboot
                self.emit("WTL LAB_REBOOT phase=rename");
                self.raw("garbage after reboot \u{0}\r\n");
                self.emit(&format!("WTL AGENT_START os=fake build=17763 machine={}", machine));
                if rn["admin"] == json!(true) {
                    self.emit(&format!("WTL LAB_PHASE | renamed to {}; as the domain Administrator now", to));
                    self.emit("WTL LAB_REBOOT phase=rename-admin");
                    self.raw("garbage after reboot \u{0}\r\n");
                    self.emit(&format!("WTL AGENT_START os=fake build=17763 machine={}", machine));
                }
                if rn["ca"] == json!(true) {
                    self.emit(&format!("WTL LAB_ROLE machine={} name=ad-cs state=ready ca={}", machine, wtl_value(s(rn, "ca_name"))));
                }
            }
            self.emit(&format!("WTL LAB_RENAMED machine={} from={} to={}", machine, from, to));
        }
        if role == "dc" {
            let is_run = cfg.get("run").map(|r| !r.is_null()).unwrap_or(false);
            if !is_run && rename.is_none() {
                // 0-state build: rename to the machine key + reboot before promotion (the host's
                // wait_ev must ride through this extra reboot), then promote and report ready
                self.emit(&format!("WTL LAB_START role=dc machine={} phase=new user=DC2019%5CAdministrator", machine));
                self.emit(&format!("WTL LAB_PHASE | renaming this computer DC2019 -> {}, then reboot", machine));
                self.emit("WTL LAB_REBOOT phase=rename");
                self.raw("garbage after reboot \u{0}\r\n");
                self.emit(&format!("WTL AGENT_START os=fake build=17763 machine={}", machine));
                // the DC installs its own roles/features/setup inline before reporting ready
                if has_config {
                    self.do_configure(m, cfg, &machine, "dc")?;
                }
            }
            self.emit(&format!("WTL LAB_START role=dc machine={} phase=ready user=LAB%5CAdministrator", machine));
            self.emit(&format!("WTL LAB_DC_READY domain={} dc={} computer={} | {}", s(cfg, "domain"), host, host, self.name));
            let peers = rename.as_ref().and_then(|r| r["peers"].as_object().cloned()).unwrap_or_default();
            let own = rename.as_ref().is_some_and(|r| s(r, "from") != s(r, "to"));
            if !peers.is_empty() || own {
                let mut old: Vec<String> = peers.keys().cloned().collect();
                if own {
                    old.push(s(rename.as_ref().unwrap(), "from").to_string());
                }
                self.emit(&format!("WTL LAB_DNS_CLEAN removed={}", old.join(",")));
            }
            if is_run {
                return self.run_multi_steps(m, cfg, &machine);
            }
            return Ok(());
        }
        if role != "member" {
            return Err(format!("unknown role {}", role));
        }
        if rename.is_some() {
            // after a rename: logged on again, frozen like after a build
            self.emit(&format!("WTL LAB_START role=member machine={} phase=joined user={}", machine, user));
            self.emit(&format!("WTL LAB_CLIENT_READY user={} machine={} computer={}", user, machine, host));
            self.emit("WTL LAB_IDLE");
            return Ok(());
        }
        if cfg.get("shutdown_when_ready").and_then(Value::as_bool) == Some(true) {
            // 0-state build: rename+join in one reboot, autologon
            self.emit(&format!("WTL LAB_START role=member machine={} phase=join user={}%5CAdministrator", machine, s(cfg, "netbios")));
            self.emit(&format!("WTL LAB_JOINED domain={} machine={}", s(cfg, "domain"), machine));
            self.emit("WTL LAB_REBOOT | after the join (renamed)");
            self.raw("garbage after reboot \u{0}\r\n");
            self.emit(&format!("WTL AGENT_START os=fake build=17763 machine={}", machine));
            // a member with config logs on as Administrator for the configure phase, then reboots
            // into its logon user (one more reboot the host's wait_ev must ride through)
            if has_config {
                self.emit(&format!(
                    "WTL LAB_START role=member machine={} phase=configure user={}%5CAdministrator",
                    machine,
                    s(cfg, "netbios")
                ));
                self.do_configure(m, cfg, &machine, "member")?;
                self.emit(&format!("WTL LAB_CONFIGURED machine={} autologon={}", machine, user));
                self.emit("WTL LAB_REBOOT | after configure");
                self.raw("garbage after reboot \u{0}\r\n");
                self.emit(&format!("WTL AGENT_START os=fake build=17763 machine={}", machine));
            }
            self.emit(&format!("WTL LAB_START role=member machine={} phase=ready user={}", machine, user));
            self.emit(&format!("WTL LAB_CLIENT_READY user={} machine={} computer={}", user, machine, machine));
            self.emit("WTL LAB_IDLE");
            return Ok(());
        }
        self.emit(&format!("WTL LAB_START role=member machine={} phase=ready user={}", machine, user));
        self.emit(&format!("WTL LAB_CLIENT_READY user={} machine={} computer={}", user, machine, host));
        if cfg.get("run").map(|r| !r.is_null()).unwrap_or(false) {
            return self.run_multi_steps(m, cfg, &machine);
        }
        Ok(())
    }

    /// The build-only "configure" phase: install roles (beyond ad-ds), features, then run the
    /// setup scripts staged on the control disc. Mirrors guest/lab.ps1's Invoke-LabConfigure;
    /// a setup script containing FAKE_LAB_FAIL fails the build.
    fn do_configure(&mut self, m: &Machine, cfg: &Value, machine: &str, role: &str) -> Result<(), String> {
        self.emit(&format!("WTL LAB_CONFIGURE machine={} role={}", machine, role));
        for r in cfg.get("roles").and_then(Value::as_array).into_iter().flatten() {
            let name = r.as_str().unwrap_or("");
            if name == "ad-ds" {
                continue;
            }
            // echo the resolved option values so the serial log proves what the build disc carried
            let detail = match name {
                "ad-cs" => format!(" ca={}", wtl_value(s(&cfg["ad_cs"], "ca_name"))),
                "dhcp" => format!(" scope={}-{} router={}", s(&cfg["dhcp"], "start"), s(&cfg["dhcp"], "end"), s(&cfg["dhcp"], "router")),
                _ => String::new(),
            };
            self.emit(&format!("WTL LAB_ROLE machine={} name={} state=installing", machine, name));
            self.emit(&format!("WTL LAB_ROLE machine={} name={} state=ready{}", machine, name, detail));
        }
        for f in cfg.get("features").and_then(Value::as_array).into_iter().flatten() {
            self.emit(&format!("WTL LAB_FEATURE machine={} name={}", machine, f.as_str().unwrap_or("")));
        }
        // setup scripts are staged under LAB\ on the control disc (the WTL_LAB-labelled CD)
        let cd = control_disc(m).ok_or("no control disc")?;
        let iso = IsoReader::open(&cd, true).map_err(|e| e.to_string())?;
        let setup = cfg.get("setup").and_then(Value::as_array).cloned().unwrap_or_default();
        for (i, st) in setup.iter().enumerate() {
            let n = i + 1;
            let file = s(st, "file");
            let as_ = if !s(st, "as_").is_empty() { s(st, "as_") } else { s(st, "as") };
            let body = iso
                .read(&format!("LAB/{}", file.replace('\\', "/")))
                .ok_or(format!("{}: setup script {} not staged on the control disc", machine, file))?;
            let text = String::from_utf8_lossy(body);
            self.emit(&format!("WTL LAB_SETUP machine={} n={} as={} script={} state=running", machine, n, as_, wtl_value(file)));
            if text.contains("FAKE_LAB_FAIL") {
                return Err(format!("{}: setup script {} failed (line 42)", machine, file));
            }
            self.emit(&format!("WTL LAB_SETUP machine={} n={} as={} script={} state=ok", machine, n, as_, wtl_value(file)));
        }
        Ok(())
    }

    /// Run this machine's steps (fmt=multi): mount each disc, run each step, report the exit
    /// the step's script asks for (FAKE_EXIT=N, else 0), with machine= on every event.
    fn run_multi_steps(&mut self, m: &Machine, cfg: &Value, machine: &str) -> Result<(), String> {
        let run = cfg.get("run").cloned().unwrap_or(Value::Null);
        // mounts: each needs a disc labelled WTL<LETTER> (indices > 0)
        let mut discs: BTreeMap<String, IsoReader> = BTreeMap::new();
        for (idx, p) in &m.cds {
            if *idx == 0 {
                continue;
            }
            let r = IsoReader::open(p, true).map_err(|e| e.to_string())?;
            discs.insert(r.volid.clone(), r);
        }
        for mnt in run.get("mounts").and_then(Value::as_array).into_iter().flatten() {
            let letter = s(mnt, "letter");
            if !discs.contains_key(&format!("WTL{}", letter)) {
                return Err(format!("{}: mount {}: no disc labelled WTL{}", machine, letter, letter));
            }
            self.emit(&format!("WTL LAB_MOUNT machine={} letter={} host={}", machine, letter, wtl_value(s(mnt, "host"))));
        }
        let steps = run.get("steps").and_then(Value::as_array).cloned().unwrap_or_default();
        for st in steps {
            let n = st.get("n").and_then(Value::as_i64).unwrap_or(0);
            // the host writes the key as "as_" (serde rename) -> fall back to "as"
            let who = if !s(&st, "as_").is_empty() { s(&st, "as_") } else { s(&st, "as") }.to_string();
            let path = s(&st, "path").to_string();
            let script = wtl_value(&path);
            let letter = &path[..1];
            let content: Option<Vec<u8>> = if letter == "C" {
                Some(b"(a C: program)".to_vec())
            } else {
                discs.get(&format!("WTL{}", letter)).and_then(|d| d.read(&path[3..].replace('\\', "/")).map(<[u8]>::to_vec))
            };
            let Some(content) = content else {
                self.emit(&format!(
                    "WTL LAB_STEP machine={} as={} n={} script={} exit=launchfail | The system cannot find the file specified",
                    machine, who, n, script
                ));
                continue;
            };
            let text = String::from_utf8_lossy(&content).into_owned();
            if text.contains("FAKE_HANG") {
                return Ok(());
            }
            if text.contains("FAKE_CRASH") {
                std::thread::sleep(Duration::from_millis(1500));
                fake_exit(1);
            }
            if text.contains("FAKE_LAB_FAIL") {
                return Err(format!("{}: step {} blew up (line 42)", machine, n));
            }
            let code = text
                .split("FAKE_EXIT=")
                .nth(1)
                .map(|r| r.chars().take_while(|c| c.is_ascii_digit() || *c == '-').collect::<String>())
                .unwrap_or_else(|| "0".into());
            let out = format!("ran {} as {} on {}\r\n{}", path, who, machine, text).into_bytes();
            for (seq, chunk) in out.chunks(768).enumerate() {
                self.emit(&format!("WTL OUTPUT stream=step{}out seq={} | {}", n, seq, b64encode(chunk)));
            }
            self.emit(&format!("WTL OUTPUT_END stream=step{}out bytes={} sent={} truncated=0", n, out.len(), out.len()));
            self.emit(&format!("WTL LAB_STEP machine={} as={} n={} script={} exit={} secs=0.4", machine, who, n, script, code));
        }
        self.emit(&format!(
            "WTL LAB_RUN_DONE machine={} steps={}",
            machine,
            run.get("steps").and_then(Value::as_array).map(|a| a.len()).unwrap_or(0)
        ));
        Ok(())
    }
}

// =====================================================================================
// test driver

struct Ctx {
    tmp: PathBuf,
    root: PathBuf,
    piddir: PathBuf,
    fake_log: PathBuf,
    failures: Vec<String>,
    filter: Vec<String>,
}

struct Out {
    code: i32,
    text: String,
}

fn free_port() -> u16 {
    std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap().local_addr().unwrap().port()
}

impl Ctx {
    fn new(filter: Vec<String>) -> Ctx {
        let tmp = std::env::temp_dir().join(format!("wtl-fakelab-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let tmp = std::fs::canonicalize(&tmp).unwrap(); // paths below are compared as text
        let root = tmp.join("lab");
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf();
        for d in ["lib", "guest", "labs", "images"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        for d in ["app/sub dir", "cfg", "pids"] {
            std::fs::create_dir_all(tmp.join(d)).unwrap();
        }
        for f in ["lib/oslist.conf", "guest/lab.ps1", "guest/labsvc.ps1", "guest/agent.ps1"] {
            std::fs::copy(src.join(f), root.join(f)).unwrap();
        }
        // the OS-specific guest folders (Server 2008 R2: PowerShell 2.0; Server 2003: VBScript)
        // so the control disc gets that OS's own scripts, not the shared ones
        for d in ["guest/2003", "guest/2008"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
            for e in std::fs::read_dir(src.join(d)).unwrap().flatten() {
                if e.path().is_file() {
                    std::fs::copy(e.path(), root.join(d).join(e.file_name())).unwrap();
                }
            }
        }
        // a workgroup single machine on Server 2003 (VBScript guest)
        std::fs::write(
            root.join("labs/solo2003.json"),
            r#"{ "machines": { "WS2003": { "os": "2003", "logon": "tester" } },
  "users": { "tester": { "password": "Test!2026" } } }
"#,
        )
        .unwrap();
        std::fs::write(root.join("images/2003-client.qcow2"), "QFI\u{fb} fake base image\n").unwrap();
        // a workgroup single machine on an imported-VHD OS (Server 2008 R2)
        std::fs::write(
            root.join("labs/solo2008.json"),
            r#"{ "machines": { "WS2008": { "os": "2008", "logon": "tester" } },
  "users": { "tester": { "password": "Test!2026" } } }
"#,
        )
        .unwrap();
        // an AD lab that (wrongly) puts a 2008 R2 member in a domain: wtl must refuse it
        std::fs::write(
            root.join("labs/ad2008.json"),
            format!(
                r#"{{ "domain": "lab.local", "netbios": "LAB", "network": {{ "subnet": "10.77.94", "lan_port": {port} }},
  "machines": {{ "DC": {{ "os": "2019", "role": "dc" }}, "M": {{ "os": "2008", "logon": "tester" }} }},
  "users": {{ "tester": {{ "password": "Test!2026" }} }} }}
"#,
                port = free_port()
            ),
        )
        .unwrap();
        // a fake base image for the 2008 workgroup run (the VHD import path is not exercised by
        // the fake builder; the real convert+virt-customize is covered by build.rs itself)
        std::fs::write(root.join("images/2008-client.qcow2"), "QFI\u{fb} fake base image\n").unwrap();
        // a 3-machine JSON lab: 2019 DC + a 2019 member + a win11 (UEFI) member
        let tri = format!(
            r#"{{
  "domain": "lab.local", "netbios": "LAB", "admin_password": "WinTest!2026",
  "network": {{ "subnet": "10.77.98", "lan_port": {port} }},
  "machines": {{
    "DC": {{ "os": "2019", "role": "dc" }},
    "CL1": {{ "os": "2019", "logon": "student1" }},
    "CL2": {{ "os": "win11", "logon": "student2", "cpus": 4 }}
  }},
  "users": {{
    "student1": {{ "password": "Pupil!2026", "groups": ["Students"] }},
    "student2": {{ "password": "Pupil!2026", "groups": ["Students"] }},
    "teacher":  {{ "password": "Teach!2026", "groups": ["Teachers"] }}
  }}
}}
"#,
            port = free_port()
        );
        std::fs::write(root.join("labs/tri.json"), tri).unwrap();
        // a JSON lab whose base images do not exist yet: the runner installs them with
        // `wtl build-image` (here a fake that writes the image and logs its arguments)
        let fresh = format!(
            r#"{{
  "domain": "lab.local", "netbios": "LAB", "admin_password": "WinTest!2026",
  "network": {{ "subnet": "10.77.97", "lan_port": {port} }},
  "machines": {{
    "DC": {{ "os": "2016", "role": "dc" }},
    "A": {{ "os": "2016", "logon": "s1" }},
    "B": {{ "os": "2016", "logon": "s2" }}
  }},
  "users": {{ "s1": {{ "password": "Pupil!2026" }}, "s2": {{ "password": "Pupil!2026" }} }}
}}
"#,
            port = free_port()
        );
        std::fs::write(root.join("labs/fresh.json"), fresh).unwrap();
        // a roles lab: DC (ad-ds + dhcp) + a CA server (ad-cs + iis + a feature + setup scripts)
        // + a plain member. Exercises the build-time "configure" phase and setup staging.
        let roles = format!(
            r#"{{
  "domain": "pki.test", "netbios": "PKI",
  "network": {{ "subnet": "10.77.95", "lan_port": {port} }},
  "machines": {{
    "DC": {{ "os": "2019", "roles": ["ad-ds", "dhcp"], "dhcp": {{ "range": ".100-.199" }} }},
    "CA": {{ "os": "2019", "roles": ["ad-cs", "iis"], "features": ["Telnet-Client"],
            "ad_cs": {{ "ca_name": "PKI-ROOT-CA" }},
            "setup": [ {{ "run": "./setup/first.ps1" }}, {{ "run": "setup/second.bat", "as": "operator" }} ] }},
    "CL": {{ "os": "2019", "logon": "operator" }}
  }},
  "users": {{ "operator": {{ "password": "Oper!2026", "groups": ["Domain Admins"] }} }}
}}
"#,
            port = free_port()
        );
        std::fs::write(root.join("labs/roles.json"), roles).unwrap();
        std::fs::create_dir_all(root.join("labs/setup")).unwrap();
        std::fs::write(root.join("labs/setup/first.ps1"), "Write-Host configuring\r\n").unwrap();
        std::fs::write(root.join("labs/setup/second.bat"), "@echo off\r\necho step two\r\n").unwrap();
        // workgroup mode: one machine, local users, no lab LAN
        std::fs::write(
            root.join("labs/solo.json"),
            r#"{ "machines": { "WS1": { "os": "2019", "logon": "tester" } },
  "users": { "tester": { "password": "Test!2026" }, "admin2": { "password": "Second!2026", "groups": ["Administrators"] } } }
"#,
        )
        .unwrap();
        // mixed CPUs: an x64 Server DC + a Windows 11 on Arm member
        std::fs::write(
            root.join("labs/armmix.json"),
            r#"{ "domain": "lab.local", "netbios": "LAB", "network": { "subnet": "10.77.125", "lan_port": 47125 },
  "machines": { "DC1": { "os": "2019", "role": "dc" }, "ARM1": { "os": "win11-arm64", "logon": "student" } },
  "users": { "student": { "password": "Pupil!2026" } } }
"#,
        )
        .unwrap();
        // a lab carried by a project, outside win-test-lab: a 2016 DC (any Server may be the
        // DC) + a 2019 member server, referred to by path from the project's run file
        let proj = tmp.join("proj/wintest");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(
            proj.join("lab.json"),
            format!(
                r#"{{ "domain": "corp.test", "netbios": "CORP",
  "network": {{ "subnet": "10.77.96", "lan_port": {port} }},
  "machines": {{ "DC1": {{ "os": "2012", "role": "dc" }}, "SRV1": {{ "os": "2019" }} }} }}
"#,
                port = free_port()
            ),
        )
        .unwrap();
        std::fs::write(proj.join("smoke.ps1"), "Write-Host smoke\r\nFAKE_EXIT=0\r\n").unwrap();
        // the project's build output, mounted as a second drive the scripts read from
        std::fs::create_dir_all(tmp.join("proj/build")).unwrap();
        std::fs::write(tmp.join("proj/build/app.exe"), "MZ app\r\n").unwrap();
        std::fs::write(
            proj.join("run.json"),
            r#"{ "lab": "./lab.json", "mount": { "X": ".", "Y": "../build" },
  "steps": [ { "on": "SRV1", "as": "system", "run": "X:/smoke.ps1" }, { "on": "DC1", "as": "system", "run": "X:/smoke.ps1" } ] }
"#,
        )
        .unwrap();
        std::fs::write(root.join("images/2012-dc.qcow2"), "QFI\u{fb} fake base image\n").unwrap();
        let fake_build = "#!/bin/sh\n\
             # fake image builder (WTL_BUILDER): OS --name NAME --computer-name CN [--no-updates] [--show]\n\
             echo \"$*\" >> \"$WTL_IMAGES_DIR/build-calls.log\"\n\
             echo \"installing $1 into $3.qcow2\"\n\
             printf 'QFI\\373 fake base image\\n' > \"$WTL_IMAGES_DIR/$3.qcow2\"\n";
        std::fs::write(tmp.join("fake-build-image"), fake_build).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(tmp.join("fake-build-image"), std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        for img in ["2019-dc", "2019-client", "win11-dc", "win11-client", "win11-arm64-client"] {
            std::fs::write(root.join(format!("images/{}.qcow2", img)), "QFI\u{fb} fake base image\n").unwrap();
        }
        std::fs::write(tmp.join("OVMF_CODE.fd"), "code").unwrap();
        std::fs::write(tmp.join("OVMF_VARS.fd"), "vars").unwrap();
        // the program folder a config mounts
        let app = tmp.join("app");
        std::fs::write(app.join("setup.bat"), "@echo off\r\necho installing\r\nFAKE_EXIT=0\r\n").unwrap();
        std::fs::write(app.join("smoke.exe"), "MZ fake smoke.exe\r\n").unwrap();
        std::fs::write(app.join("fail.bat"), "exit /b 3\r\nFAKE_EXIT=3\r\n").unwrap();
        std::fs::write(app.join("labfail.bat"), "FAKE_LAB_FAIL\r\n").unwrap();
        std::fs::write(app.join("hang.bat"), "FAKE_HANG\r\n").unwrap();
        std::fs::write(app.join("crash.bat"), "FAKE_CRASH\r\n").unwrap();
        std::fs::write(app.join("sub dir/Long Name Script.ps1"), "Write-Host hi\r\n").unwrap();
        Ctx { piddir: tmp.join("pids"), fake_log: tmp.join("fake-argv.log"), tmp, root, failures: Vec::new(), filter }
    }

    fn cfg(&self, name: &str, text: &str) -> String {
        let p = self.tmp.join("cfg").join(name);
        std::fs::write(&p, text).unwrap();
        p.to_string_lossy().into_owned()
    }

    fn state(&self, lab: &str) -> PathBuf {
        self.root.join("labs/state").join(lab)
    }

    /// Run `wtl ARGS` with the fake QEMU; stdout+stderr combined.
    fn lab(&self, args: &[&str], env: &[(&str, &str)]) -> Out {
        let me = std::env::current_exe().unwrap();
        let outf = self.tmp.join("out.txt");
        let f = std::fs::File::create(&outf).unwrap();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_wtl"));
        cmd.args(args).current_dir(self.tmp.join("cfg"));
        for (k, _) in std::env::vars_os() {
            let ks = k.to_string_lossy();
            if ks.starts_with("WTL_") || ks.starts_with("FAKE_") {
                cmd.env_remove(&k);
            }
        }
        cmd.env("WTL_HOME", &self.root)
            .env("WTL_IMAGES_DIR", self.root.join("images"))
            .env("WTL_QEMU", &me)
            .env("WTL_QEMU_IMG", &me)
            .env("WTL_NO_KVM", "1")
            .env("WTL_TEST_FAST", "1")
            .env("WTL_OVMF_CODE", self.tmp.join("OVMF_CODE.fd"))
            .env("WTL_OVMF_VARS", self.tmp.join("OVMF_VARS.fd"))
            .env("WTL_AAVMF_CODE", self.tmp.join("OVMF_CODE.fd"))
            .env("WTL_AAVMF_VARS", self.tmp.join("OVMF_VARS.fd"))
            .env("WTL_QEMU_ARM64", &me)
            .env("WTL_BUILDER", self.tmp.join("fake-build-image"))
            .env("FAKE_PIDDIR", &self.piddir)
            .env("FAKE_LOG", &self.fake_log);
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.stdin(Stdio::null()).stdout(f.try_clone().unwrap()).stderr(f);
        let mut child = cmd.spawn().unwrap();
        let end = Instant::now() + Duration::from_secs(120);
        let code = loop {
            if let Some(st) = child.try_wait().unwrap() {
                break st.code().unwrap_or(-1);
            }
            if Instant::now() > end {
                let _ = child.kill();
                let _ = child.wait();
                break -999;
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        Out { code, text: std::fs::read_to_string(&outf).unwrap_or_default() }
    }

    fn live_fakes(&self) -> Vec<String> {
        let mut v = Vec::new();
        for e in std::fs::read_dir(&self.piddir).unwrap().flatten() {
            let pid = e.file_name().to_string_lossy().into_owned();
            // an exited fake may be a zombie for a moment until init reaps it
            let alive = |p: &str| p.parse().is_ok_and(wtl::sys::pid_alive);
            if alive(&pid) {
                std::thread::sleep(Duration::from_millis(300));
                if alive(&pid) {
                    v.push(format!("{} ({})", pid, std::fs::read_to_string(e.path()).unwrap_or_default()));
                }
            }
        }
        v
    }

    fn kill_fakes(&self) {
        for e in std::fs::read_dir(&self.piddir).unwrap().flatten() {
            if let Ok(pid) = e.file_name().to_string_lossy().parse() {
                wtl::sys::kill_hard(pid);
            }
            let _ = std::fs::remove_file(e.path());
        }
    }

    fn want(&self, name: &str) -> bool {
        self.filter.is_empty() || self.filter.iter().any(|f| name.contains(f.as_str()))
    }
}

/// Collects the problems of one scenario.
struct Check<'a> {
    name: &'a str,
    problems: Vec<String>,
}

impl Check<'_> {
    fn ok(&mut self, cond: bool, what: impl Into<String>) {
        if !cond {
            self.problems.push(what.into());
        }
    }

    fn code(&mut self, o: &Out, want: i32) {
        self.ok(o.code == want, format!("exit {} (want {})", o.code, want));
    }

    fn has(&mut self, o: &Out, needle: &str) {
        self.ok(o.text.contains(needle), format!("output lacks {:?}", needle));
    }

    fn lacks(&mut self, o: &Out, needle: &str) {
        self.ok(!o.text.contains(needle), format!("output has {:?}", needle));
    }

    fn file(&mut self, p: &Path) {
        self.ok(p.exists(), format!("missing {}", p.display()));
    }

    fn no_file(&mut self, p: &Path) {
        self.ok(!p.exists(), format!("should not exist: {}", p.display()));
    }

    fn content(&mut self, p: &Path, want: &[u8]) {
        let got = std::fs::read(p).unwrap_or_default();
        self.ok(got == want, format!("{}: {:?} (want {:?})", p.display(), String::from_utf8_lossy(&got), String::from_utf8_lossy(want)));
    }
}

fn driver(args: &[String]) -> i32 {
    if !cfg!(unix) {
        println!("fake_lab: skipped (the fake QEMU serves QMP on a unix socket)");
        return 0;
    }
    let filter: Vec<String> = args.iter().filter(|a| !a.starts_with('-')).cloned().collect();
    let mut cx = Ctx::new(filter);
    let mut out_of: Vec<(String, Out)> = Vec::new();
    let mut ran = 0;
    // a scenario gets the test tree, a checker, and the outputs of the `wtl` runs it makes
    // (printed when it fails)
    #[allow(clippy::type_complexity)]
    let mut scenario = |cx: &mut Ctx, name: &str, f: &dyn Fn(&Ctx, &mut Check, &mut Vec<(String, Out)>)| {
        if !cx.want(name) {
            return;
        }
        ran += 1;
        let t0 = Instant::now();
        let mut c = Check { name, problems: Vec::new() };
        out_of.clear();
        f(cx, &mut c, &mut out_of);
        let secs = t0.elapsed().as_secs_f64();
        if c.problems.is_empty() {
            println!("PASS {:<58} {:5.1}s", c.name, secs);
        } else {
            println!("FAIL {:<58} {:5.1}s", c.name, secs);
            for p in &c.problems {
                println!("     - {}", p);
            }
            for (label, o) in out_of.iter() {
                println!("     --- {} (exit {}) ---", label, o.code);
                for l in o.text.lines() {
                    println!("     | {}", l);
                }
            }
            cx.failures.push(c.name.to_string());
        }
    };
    let run = |cx: &Ctx, outs: &mut Vec<(String, Out)>, args: &[&str], env: &[(&str, &str)]| -> usize {
        outs.push((format!("wtl {}", args.join(" ")), cx.lab(args, env)));
        outs.len() - 1
    };

    scenario(&mut cx, "usage + help", &|cx, c, outs| {
        let i = run(cx, outs, &["--help"], &[]);
        c.code(&outs[i].1, 0);
        c.has(&outs[i].1, "stop the VMs and throw away everything since the 0 state");
        c.has(&outs[i].1, "--home <DIR>");
        c.has(&outs[i].1, "Exit: 0 all steps passed, 1 a step failed, 2 lab error, 3 timeout.");
        // run needs a config file
        let i = run(cx, outs, &["run", "tri"], &[]);
        c.code(&outs[i].1, 2);
        c.has(&outs[i].1, "usage: wtl run CONFIG");
        let i = run(cx, outs, &["status"], &[]);
        c.code(&outs[i].1, 2);
        let i = run(cx, outs, &["status", "nope"], &[]);
        c.code(&outs[i].1, 2);
        c.has(&outs[i].1, "no lab ");
        // the home's own labs and the built-in ones
        c.has(&outs[i].1, "armmix fresh mixed pki roles school single-2003 single-2008 single-2012");
        c.has(&outs[i].1, "solo solo2003 solo2008 tri win10 win11; environments: wtl list)");
        // usage errors: clap's wording, exit 2
        let i = run(cx, outs, &["frobnicate", "tri"], &[]);
        c.code(&outs[i].1, 2);
        c.has(&outs[i].1, "unrecognized subcommand 'frobnicate'");
        let i = run(cx, outs, &["status", "tri", "--bogus"], &[]);
        c.code(&outs[i].1, 2);
        c.has(&outs[i].1, "unexpected argument '--bogus'");
        let i = run(cx, outs, &["status", "tri", "--wait", "0"], &[]);
        c.code(&outs[i].1, 2);
        c.has(&outs[i].1, "invalid value '0' for '--wait <S>': must be a positive integer");
        let i = run(cx, outs, &["status", "tri", "fresh"], &[]);
        c.code(&outs[i].1, 2);
        c.has(&outs[i].1, "unexpected argument 'fresh'");
    });

    scenario(&mut cx, "config errors are reported before anything starts", &|cx, c, outs| {
        let syntax = cx.cfg("syntax.json", "{ \"lab\": \"tri\", ");
        let i = run(cx, outs, &["run", &syntax], &[]);
        c.code(&outs[i].1, 2);
        c.has(&outs[i].1, "config error");
        c.has(&outs[i].1, "syntax.json: EOF while parsing");
        let other = cx.cfg("other.json", "{ \"lab\": \"nolab\", \"steps\": [ { \"as\": \"system\", \"run\": \"C:/x.bat\" } ] }");
        let i = run(cx, outs, &["run", &other], &[]);
        c.code(&outs[i].1, 2);
        c.has(&outs[i].1, "lab \"nolab\" not found");
        let nostep = cx.cfg("nostep.json", "{ \"lab\": \"tri\", \"mount\": { \"X\": \"../app\" } }");
        let i = run(cx, outs, &["run", &nostep], &[]);
        c.code(&outs[i].1, 2);
        c.has(&outs[i].1, "nothing to do (no steps)");
        // the DC has no logon user, so a step on it needs an explicit `as`
        let dc_as = cx.cfg("dcas.json", "{ \"lab\": \"tri\", \"steps\": [ { \"on\": \"DC\", \"run\": \"C:/x.bat\" } ] }");
        let i = run(cx, outs, &["run", &dc_as], &[]);
        c.code(&outs[i].1, 2);
        c.has(&outs[i].1, "needs an explicit \"as\"");
        c.ok(cx.live_fakes().is_empty(), "a VM was started");
    });

    scenario(&mut cx, "status before the 0 state exists", &|cx, c, outs| {
        let i = run(cx, outs, &["status", "tri"], &[]);
        c.code(&outs[i].1, 0);
        c.has(&outs[i].1, "lab tri (");
        c.has(&outs[i].1, "domain lab.local, 3 machines");
        c.has(&outs[i].1, "0 state: not built yet (wtl prepare tri, or just wtl run ...)");
        c.has(&outs[i].1, "  DC              dc     2019    10.77.98.10 off");
        c.has(&outs[i].1, "  CL2             member win11   10.77.98.21 off");
    });

    scenario(&mut cx, "JSON 3-machine lab: build the 0 state, run per-machine steps", &|cx, c, outs| {
        // a run config living next to the app folder; `on`, `as`, args, expect_exit all exercised
        let conf = cx.cfg(
            "school.json",
            "{\n  \"lab\": \"tri\", \"name\": \"tri-smoke\", \"wait\": 30,\n  \"mount\": { \"X\": \"../app\" },\n  \"steps\": [\n    { \"on\": \"all\", \"as\": \"system\", \"run\": \"X:/setup.bat\" },\n    { \"on\": \"CL1\", \"as\": \"student1\", \"run\": \"X:/smoke.exe\" },\n    { \"on\": [\"CL1\", \"CL2\"], \"run\": \"X:/smoke.exe\", \"args\": \"--quiet\" },\n    { \"on\": \"CL1\", \"as\": \"teacher\", \"run\": \"X:/smoke.exe\" },\n    { \"on\": \"DC\", \"as\": \"system\", \"run\": \"C:\\\\Windows\\\\System32\\\\whoami.exe\" }\n  ]\n}\n",
        );
        let i = run(cx, outs, &["run", &conf], &[]);
        let o = &outs[i].1;
        c.code(o, 0);
        for needle in [
            "=== building the 0 state of lab tri (domain lab.local; 3 machines, once) ===",
            "DC ready: WTL LAB_DC_READY domain=lab.local",
            "CL1 ready:",
            "CL2 ready:",
            "lab tri: booting 3 machines from the 0 state",
            // per-machine steps, with machine tags
            "[DC system] C:\\Windows\\System32\\whoami.exe -> exit 0",
            "[CL1 system] X:\\setup.bat -> exit 0",
            "[CL2 system] X:\\setup.bat -> exit 0",
            // step 2 is `as student1` on CL1, whose own logon user is student1 -> runs as "logon"
            "[CL1 logon] X:\\smoke.exe -> exit 0",
            "[CL2 logon] X:\\smoke.exe -> exit 0",
            // step 4 is `as teacher` on CL1 (not its logon user) -> the worker runs it as that user
            "[CL1 user] X:\\smoke.exe -> exit 0",
            "[DC] all steps done",
            "[CL1] all steps done",
            "[CL2] all steps done",
            "config: school.json  ->  ALL STEPS OK",
        ] {
            c.has(o, needle);
        }
        let st = cx.state("tri");
        c.file(&st.join("0-state.json"));
        for vm in ["DC", "CL1", "CL2"] {
            c.file(&st.join(format!("{}-0.qcow2", vm)));
            c.file(&st.join(format!("{}.qcow2", vm)));
        }
        // the win11 member is UEFI -> its own OVMF vars in the state; the others are BIOS
        c.file(&st.join("CL2-OVMF_VARS.fd"));
        c.no_file(&st.join("DC-OVMF_VARS.fd"));
        // per-machine results folders with decoded step output
        let res = std::fs::canonicalize(st.join("last")).unwrap_or_default();
        c.content(&res.join("CL1/step1out.txt"), b"ran X:\\setup.bat as system on CL1\r\n@echo off\r\necho installing\r\nFAKE_EXIT=0\r\n");
        c.file(&res.join("CL1/screen.png"));
        c.file(&res.join("CL2/serial.log"));
        c.file(&res.join("config.json"));
        // computer names in the serial logs are the machine keys (renamed), not base-image names
        for vm in ["DC", "CL1", "CL2"] {
            let log = std::fs::read_to_string(res.join(format!("{}/serial.log", vm))).unwrap_or_default();
            c.ok(log.contains(&format!("computer={}", vm)), format!("{} serial log should report computer={}", vm, vm));
        }
        // result.json: one record per step with machine/user/exit/expect/duration, status ok
        let rj: Value = serde_json::from_slice(&std::fs::read(res.join("result.json")).unwrap_or_default()).unwrap_or(Value::Null);
        c.ok(s(&rj, "status") == "ok" && s(&rj, "lab") == "tri" && s(&rj, "config") == "school.json", format!("result.json head {}", rj));
        let steps = rj["steps"].as_array().cloned().unwrap_or_default();
        // DC(1) + CL1(4) + CL2(2) = 7 step records
        c.ok(steps.len() == 7, format!("result.json should have 7 steps, has {}", steps.len()));
        c.ok(steps.iter().all(|s| s["ok"] == json!(true)), "every step ok");
        let find = |m: &str, n: i64| steps.iter().find(|s| s["machine"] == json!(m) && s["n"] == json!(n)).cloned().unwrap_or(Value::Null);
        c.ok(
            find("CL1", 4)["user"] == json!("user") && find("CL1", 4)["script"] == json!("X:\\smoke.exe"),
            format!("CL1 step4 {}", find("CL1", 4)),
        );
        c.ok(find("DC", 1)["machine"] == json!("DC") && find("DC", 1)["exit"] == json!("0"), format!("DC step1 {}", find("DC", 1)));
        // the DC's control disc: fmt=multi, role=dc, and only its own step (whoami on C:)
        let dc_iso = IsoReader::open(&st.join("DC.iso"), true).unwrap();
        let dj: Value = serde_json::from_slice(dc_iso.read("LAB/lab.json").unwrap()).unwrap();
        c.ok(s(&dj, "fmt") == "multi" && s(&dj, "role") == "dc" && s(&dj, "ip") == "10.77.98.10", format!("DC lab.json {}", dj));
        c.ok(dj["run"]["steps"].as_array().map(|a| a.len()) == Some(1), format!("DC should have 1 step: {}", dj["run"]));
        // every machine with steps gets every drive of the run (a step may read other drives)
        c.ok(dj["run"]["mounts"].as_array().map(|a| a.len()) == Some(1), format!("DC mounts: {}", dj["run"]["mounts"]));
        c.file(&st.join("mount-DC-X.iso"));
        // CL1's disc: 3 steps (setup, smoke student1, smoke logon), one mount X
        let cl1_iso = IsoReader::open(&st.join("CL1.iso"), true).unwrap();
        let cj: Value = serde_json::from_slice(cl1_iso.read("LAB/lab.json").unwrap()).unwrap();
        c.ok(s(&cj, "role") == "member" && s(&cj, "logon_user") == "student1", format!("CL1 lab.json {}", cj));
        c.ok(cj["run"]["steps"].as_array().map(|a| a.len()) == Some(4), format!("CL1 steps {}", cj["run"]["steps"]));
        // the student1 step is its own logon user -> as_=logon; teacher is another user -> as_=user, with creds
        let step =
            |st: &Value, n: i64| st["run"]["steps"].as_array().unwrap().iter().find(|x| x["n"] == json!(n)).cloned().unwrap_or(Value::Null);
        let as_of = |st: &Value, n: i64| step(st, n)["as_"].as_str().unwrap_or("").to_string();
        c.ok(as_of(&cj, 1) == "system", format!("CL1 step1 as_ {}", as_of(&cj, 1)));
        c.ok(as_of(&cj, 2) == "logon", format!("CL1 step2 as_ {}", as_of(&cj, 2)));
        c.ok(
            as_of(&cj, 4) == "user" && step(&cj, 4)["user"] == json!("teacher") && step(&cj, 4)["password"] == json!("Teach!2026"),
            format!("CL1 step4 (teacher) {}", step(&cj, 4)),
        );
        // multicast LAN argv + per-VM cpus; CL2 is UEFI/q35
        let argv = std::fs::read_to_string(&cx.fake_log).unwrap_or_default();
        let line = |tag: &str| argv.lines().rfind(|l| l.contains(tag)).unwrap_or("").replace('\u{1f}', " ");
        let dc_line = line("lab-tri-DC");
        c.ok(
            dc_line.contains("-netdev dgram,id=lan0,remote.type=inet,remote.host=239.") && dc_line.contains("local.port="),
            format!("dc mcast argv: {}", dc_line),
        );
        c.ok(!dc_line.contains("user,id=net0"), "dc should be isolated (no user NAT)");
        let cl2_line = line("lab-tri-CL2");
        c.ok(cl2_line.contains("user,id=net0"), "members keep user NAT");
        c.ok(cl2_line.contains("-smp 4") && cl2_line.contains("-machine q35"), format!("CL2 cpus/uefi: {}", cl2_line));
        c.ok(cx.live_fakes().len() == 3, format!("running VMs: {:?}", cx.live_fakes()));
        let i = run(cx, outs, &["restore", "tri"], &[]);
        c.code(&outs[i].1, 0);
    });

    scenario(&mut cx, "JSON lab: a failing step and a bad target", &|cx, c, outs| {
        // expect_exit mismatch -> A STEP FAILED
        let conf = cx.cfg(
            "fail.json",
            "{\n  \"lab\": \"tri\", \"mount\": { \"X\": \"../app\" },\n  \"steps\": [\n    { \"on\": \"CL1\", \"as\": \"system\", \"run\": \"X:/fail.bat\" }\n  ]\n}\n",
        );
        let i = run(cx, outs, &["run", &conf], &[]);
        let o = &outs[i].1;
        c.code(o, 1);
        c.lacks(o, "building the 0 state"); // the restore kept the 0 state
        c.has(o, "[CL1 system] X:\\fail.bat -> exit 3 (want 0)");
        c.has(o, "->  A STEP FAILED");
        let _ = run(cx, outs, &["restore", "tri"], &[]);
        // a run naming an unknown machine is rejected before boot
        let bad = cx.cfg("bad.json", "{ \"lab\": \"tri\", \"mount\": {\"X\":\"../app\"}, \"steps\": [ { \"on\": \"NOPE\", \"as\": \"system\", \"run\": \"X:/setup.bat\" } ] }");
        let i = run(cx, outs, &["run", &bad], &[]);
        c.code(&outs[i].1, 2);
        c.has(&outs[i].1, "is not a machine in lab tri");
        c.ok(cx.live_fakes().is_empty(), "a VM was started for an invalid config");
    });

    scenario(&mut cx, "steps run in order per machine; launchfail; --wait overrides", &|cx, c, outs| {
        let conf = cx.cfg(
            "order.json",
            "{ \"lab\": \"tri\", \"wait\": 5, \"mount\": { \"x\": \"../app\" }, \"steps\": [\n\
               { \"on\": \"CL1\", \"as\": \"system\", \"run\": \"X:/setup.bat\" },\n\
               { \"on\": \"CL1\", \"run\": \"x:/fail.bat\", \"expect_exit\": 3 },\n\
               { \"on\": \"CL1\", \"run\": \"X:\\\\sub dir\\\\Long Name Script.ps1\" },\n\
               { \"on\": \"CL1\", \"as\": \"system\", \"run\": \"X:/missing.exe\" } ] }\n",
        );
        let i = run(cx, outs, &["run", &conf, "--wait", "7"], &[]);
        let o = &outs[i].1;
        c.code(o, 1);
        c.has(o, "[CL1 system] X:\\setup.bat -> exit 0");
        // expect_exit 3 makes exit 3 a pass
        c.has(o, "[CL1 logon] X:\\fail.bat -> exit 3");
        c.lacks(o, "fail.bat -> exit 3 (want");
        c.has(o, "[CL1 logon] X:\\sub dir\\Long Name Script.ps1 -> exit 0");
        c.has(o, "ERROR: [CL1 system] X:\\missing.exe -> exit launchfail (want 0)");
        c.has(o, "->  A STEP FAILED");
        let pos = |s: &str| o.text.find(s).unwrap_or(usize::MAX);
        c.ok(
            pos("setup.bat ->") < pos("fail.bat ->")
                && pos("fail.bat ->") < pos("Script.ps1 ->")
                && pos("Script.ps1 ->") < pos("missing.exe ->"),
            "steps out of order",
        );
        let st = cx.state("tri");
        let res = std::fs::canonicalize(st.join("last")).unwrap_or_default();
        c.content(&res.join("CL1/step3out.txt"), b"ran X:\\sub dir\\Long Name Script.ps1 as logon on CL1\r\nWrite-Host hi\r\n");
        let rj: Value = serde_json::from_slice(&std::fs::read(res.join("result.json")).unwrap_or_default()).unwrap_or(Value::Null);
        c.ok(s(&rj, "status") == "failed", format!("result.json status {}", rj["status"]));
        // only CL1 has steps: no results folder for the others
        c.no_file(&res.join("CL2"));
        let iso = IsoReader::open(&st.join("CL1.iso"), true).unwrap();
        let j: Value = serde_json::from_slice(iso.read("LAB/lab.json").unwrap()).unwrap();
        c.ok(j["run"]["wait"] == json!(7), "--wait did not override the config");
        match IsoReader::open(&st.join("mount-CL1-X.iso"), true) {
            Ok(iso) => {
                c.ok(iso.volid == "WTLX", format!("mount disc label {}", iso.volid));
                c.ok(iso.read("sub dir/Long Name Script.ps1") == Some(b"Write-Host hi\r\n".as_slice()), "long name on the mount disc");
            }
            Err(e) => c.ok(false, format!("mount-CL1-X.iso: {}", e)),
        }
        c.no_file(&st.join("mount-CL2-X.iso"));
        let i = run(cx, outs, &["status", &conf], &[]);
        c.code(&outs[i].1, 0);
        c.has(&outs[i].1, "  0 state: ready (20");
        c.has(&outs[i].1, "  CL1             member 2019    10.77.98.20 running (pid ");
        c.has(&outs[i].1, "  last run: ");
    });

    scenario(&mut cx, "QMP over TCP (the Windows path)", &|cx, c, outs| {
        let conf = cx.cfg(
            "tcp.json",
            "{ \"lab\": \"tri\", \"mount\": { \"Y\": \"../app\" }, \"steps\": [ { \"on\": \"CL2\", \"run\": \"Y:/smoke.exe\" } ] }",
        );
        let i = run(cx, outs, &["run", &conf], &[("WTL_QMP", "tcp")]);
        let o = &outs[i].1;
        c.code(o, 0);
        c.has(o, "[CL2 logon] Y:\\smoke.exe -> exit 0");
        let st = cx.state("tri");
        let addr = std::fs::read_to_string(st.join("CL2.qmp.addr")).unwrap_or_default();
        c.ok(addr.starts_with("tcp:127.0.0.1:"), format!("CL2.qmp.addr {:?}", addr));
        c.file(&std::fs::canonicalize(st.join("last")).unwrap_or_default().join("CL2/screen.png"));
        // status/restore find the TCP monitors without being told again
        let i = run(cx, outs, &["status", "tri"], &[]);
        c.has(&outs[i].1, "  CL2             member win11   10.77.98.21 running");
        let i = run(cx, outs, &["restore", "tri"], &[]);
        c.code(&outs[i].1, 0);
        c.ok(cx.live_fakes().is_empty(), format!("still running after restore: {:?}", cx.live_fakes()));
    });

    scenario(&mut cx, "guest reports LAB_FAIL -> exit 2 + screenshot (old QEMU: PPM)", &|cx, c, outs| {
        let conf = cx.cfg("labfail.json", "{ \"lab\": \"tri\", \"mount\": { \"X\": \"../app\" }, \"steps\": [ { \"on\": \"CL1\", \"as\": \"system\", \"run\": \"X:/labfail.bat\" } ] }");
        let st = cx.state("tri");
        let i = run(cx, outs, &["run", &conf], &[("FAKE_OLD_QEMU", "1")]);
        let o = &outs[i].1;
        c.code(o, 2);
        c.has(o, "ERROR: config run: CL1 reported: WTL LAB_FAIL | fake guest: CL1: step 1 blew up (line 42)");
        c.has(o, "the VMs keep running - wtl restore tri");
        c.has(o, "screenshot: ");
        c.ok(std::fs::read(st.join("CL1-fail.png")).unwrap_or_default().starts_with(b"\x89PNG"), "CL1-fail.png is not a PNG");
    });

    scenario(&mut cx, "guest hangs -> timeout exit 3", &|cx, c, outs| {
        let conf = cx.cfg("hang.json", "{ \"lab\": \"tri\", \"timeout_min\": 50, \"mount\": { \"X\": \"../app\" }, \"steps\": [ { \"on\": \"CL1\", \"run\": \"X:/hang.bat\" } ] }");
        let i = run(cx, outs, &["run", &conf, "--timeout-min", "0"], &[]);
        c.code(&outs[i].1, 3);
        c.has(&outs[i].1, "timeout (--timeout-min 0)");
    });

    scenario(&mut cx, "VM dies mid-run -> exit 2", &|cx, c, outs| {
        let conf = cx.cfg("crash.json", "{ \"lab\": \"tri\", \"mount\": { \"X\": \"../app\" }, \"steps\": [ { \"on\": \"CL1\", \"as\": \"system\", \"run\": \"X:/crash.bat\" } ] }");
        let i = run(cx, outs, &["run", &conf], &[]);
        c.code(&outs[i].1, 2);
        c.has(&outs[i].1, "ERROR: config run: the CL1 VM exited");
    });

    scenario(&mut cx, "up, stop (soft), restore", &|cx, c, outs| {
        let st = cx.state("tri");
        let i = run(cx, outs, &["up", "tri"], &[]);
        let o = &outs[i].1;
        c.code(o, 0);
        c.has(o, "CL2 ready");
        c.has(o, "  lab tri: domain lab.local");
        c.has(o, "back to the 0 state: wtl restore tri");
        c.lacks(o, "running ");
        let iso = IsoReader::open(&st.join("CL1.iso"), true).unwrap();
        let j: Value = serde_json::from_slice(iso.read("LAB/lab.json").unwrap()).unwrap();
        c.ok(j.get("run") == Some(&Value::Null), "up must not carry a run");
        c.ok(!st.join("mount-CL1-X.iso").exists(), "old mount discs are removed");
        c.ok(cx.live_fakes().len() == 3, format!("running VMs: {:?}", cx.live_fakes()));
        let i = run(cx, outs, &["stop", "tri"], &[]);
        c.code(&outs[i].1, 0);
        c.has(&outs[i].1, "lab tri is off");
        c.ok(cx.live_fakes().is_empty(), format!("still running after stop: {:?}", cx.live_fakes()));
        c.file(&st.join("CL1.qcow2")); // stop keeps the changes
        c.no_file(&st.join("CL1.pid"));
        let i = run(cx, outs, &["up", "tri"], &[]);
        c.code(&outs[i].1, 0);
        let i = run(cx, outs, &["restore", "tri"], &[]);
        c.code(&outs[i].1, 0);
        c.has(&outs[i].1, "lab tri is back at its 0 state");
        c.ok(cx.live_fakes().is_empty(), format!("still running after restore: {:?}", cx.live_fakes()));
        c.no_file(&st.join("CL1.qcow2"));
        c.file(&st.join("CL1-0.qcow2"));
    });

    scenario(&mut cx, "VNC: a display taken after wtl picked it -> QEMU's next free one is shown", &|cx, c, outs| {
        // another program (another lab booting) takes the display between wtl's check and
        // QEMU's start: QEMU moves on (to=99), and wtl reports the port QEMU really has
        let i = run(cx, outs, &["up", "tri"], &[("FAKE_VNC_STEAL", "1")]);
        let o = &outs[i].1;
        c.code(o, 0);
        let argv = std::fs::read_to_string(&cx.fake_log).unwrap_or_default();
        let line = argv.lines().rfind(|l| l.contains("lab-tri-CL1")).unwrap_or("").replace('\u{1f}', " ");
        let asked: Option<u16> = line.split("-vnc 127.0.0.1:").nth(1).and_then(|r| r.split(",to=99").next()).and_then(|n| n.parse().ok());
        c.ok(asked.is_some(), format!("CL1 argv should have -vnc 127.0.0.1:N,to=99: {}", line));
        let shown: Option<u16> =
            o.text.lines().find_map(|l| l.split("CL1 started (VNC 127.0.0.1:").nth(1)).and_then(|r| r.trim_end_matches(')').parse().ok());
        c.ok(
            matches!((asked, shown), (Some(a), Some(s)) if s > 5900 + a),
            format!("asked display {:?}, shown port {:?}: should be past the stolen one", asked, shown),
        );
        let vnc_file = std::fs::read_to_string(cx.state("tri").join("CL1.vnc")).unwrap_or_default();
        c.ok(shown.is_some_and(|p| vnc_file.trim() == format!("VNC 127.0.0.1:{}", p)), format!("CL1.vnc {:?}", vnc_file));
        let i = run(cx, outs, &["status", "tri"], &[]);
        c.has(&outs[i].1, vnc_file.trim());
        let i = run(cx, outs, &["restore", "tri"], &[]);
        c.code(&outs[i].1, 0);
    });

    scenario(&mut cx, "rebuild deletes the 0 state; prepare rebuilds it", &|cx, c, outs| {
        let st = cx.state("tri");
        let i = run(cx, outs, &["rebuild", "tri"], &[]);
        c.code(&outs[i].1, 0);
        c.has(&outs[i].1, "0 state deleted");
        c.no_file(&st.join("0-state.json"));
        c.no_file(&st.join("DC-0.qcow2"));
        let i = run(cx, outs, &["prepare", "tri"], &[]);
        c.code(&outs[i].1, 0);
        c.file(&st.join("0-state.json"));
        c.ok(cx.live_fakes().is_empty(), format!("prepare left VMs running: {:?}", cx.live_fakes()));
    });

    scenario(&mut cx, "a home that is not a checkout: built-in labs + scripts, wtl.json", &|cx, c, outs| {
        // what `cargo install` users get: no checkout anywhere, only the program
        let h = cx.tmp.join("plain home");
        let _ = std::fs::remove_dir_all(&h);
        std::fs::create_dir_all(&h).unwrap();
        let hs = h.to_string_lossy().into_owned();
        let i = run(cx, outs, &["home"], &[("WTL_HOME", &hs)]);
        c.code(&outs[i].1, 0);
        c.has(&outs[i].1, &format!("home        {}", hs));
        c.has(&outs[i].1, "guest scripts and OS table built into wtl");
        c.has(&outs[i].1, &format!("state_dir   {}/labs/state", hs));
        // a built-in lab, written into its state folder
        let i = run(cx, outs, &["status", "single-2019"], &[("WTL_HOME", &hs)]);
        c.code(&outs[i].1, 0);
        c.has(&outs[i].1, "lab single-2019 (");
        c.has(&outs[i].1, "(wtl prepare single-2019, or just wtl run ...)");
        c.file(&h.join("labs/state/single-2019/single-2019.json"));
        // wtl.json moves a folder, relative to itself; a WTL_* variable beats it
        std::fs::write(h.join("wtl.json"), "{ \"state_dir\": \"../plain state\" }\n").unwrap();
        let i = run(cx, outs, &["home"], &[("WTL_HOME", &hs)]);
        c.has(&outs[i].1, &format!("state_dir   {}  [{}]", cx.tmp.join("plain state").display(), h.join("wtl.json").display()));
        let i = run(cx, outs, &["home"], &[("WTL_HOME", &hs), ("WTL_STATE_DIR", "/elsewhere")]);
        c.has(&outs[i].1, "state_dir   /elsewhere  [WTL_STATE_DIR]");
        // a bad key in wtl.json is an error, not ignored
        std::fs::write(h.join("wtl.json"), "{ \"state_dirr\": \"x\" }\n").unwrap();
        let i = run(cx, outs, &["home"], &[("WTL_HOME", &hs)]);
        c.code(&outs[i].1, 2);
        c.has(&outs[i].1, "unknown field `state_dirr`");
        let _ = std::fs::remove_file(h.join("wtl.json"));
        // --home is WTL_HOME
        let i = run(cx, outs, &["--home", &hs, "home"], &[]);
        c.has(&outs[i].1, &format!("home        {}", hs));
        // `wtl build-image` is what a run starts for a missing base image
        let i = run(cx, outs, &["build-image", "--help"], &[]);
        c.code(&outs[i].1, 0);
        c.has(&outs[i].1, "Install Windows into a base image");
    });

    scenario(&mut cx, "JSON lab: missing base images are installed first", &|cx, c, outs| {
        let i = run(cx, outs, &["prepare", "fresh"], &[]);
        let o = &outs[i].1;
        c.code(o, 0);
        c.has(o, "installing Windows for: 2016-dc 2016-client (unattended; no updates");
        c.has(o, "[2016-dc] installing 2016 into 2016-dc.qcow2");
        c.has(o, "[2016-client] installing 2016 into 2016-client.qcow2");
        c.has(o, "0 state saved for lab fresh (3 machines)");
        // two members on the same OS share one client image: one build per image
        let calls = std::fs::read_to_string(cx.root.join("images/build-calls.log")).unwrap_or_default();
        let mut lines: Vec<&str> = calls.lines().collect();
        lines.sort();
        c.ok(
            lines
                == [
                    "2016 --name 2016-client --computer-name CL2016 --no-updates",
                    "2016 --name 2016-dc --computer-name DC2016 --no-updates",
                ],
            format!("image builder calls: {:?}", lines),
        );
        c.file(&cx.state("fresh").join("build-2016-dc.log"));
        // already there: a second prepare after a rebuild does not install again
        let _ = run(cx, outs, &["rebuild", "fresh"], &[]);
        let i = run(cx, outs, &["prepare", "fresh"], &[]);
        c.code(&outs[i].1, 0);
        c.lacks(&outs[i].1, "installing Windows");
        let _ = run(cx, outs, &["restore", "fresh"], &[]);
    });

    scenario(&mut cx, "mixed CPUs: an x64 DC and an arm64 member in one domain", &|cx, c, outs| {
        let conf = cx.cfg(
            "armmix.json",
            "{ \"lab\": \"armmix\", \"mount\": { \"X\": \"../app\" }, \"steps\": [\n\
               { \"on\": \"ARM1\", \"run\": \"X:/smoke.exe\" } ] }\n",
        );
        let i = run(cx, outs, &["run", &conf], &[]);
        let o = &outs[i].1;
        c.code(o, 0);
        c.has(o, "[ARM1 logon] X:\\smoke.exe -> exit 0");
        let argv = std::fs::read_to_string(&cx.fake_log).unwrap_or_default();
        let line = |vm: &str| argv.lines().rfind(|l| l.contains(vm)).unwrap_or("").replace('\u{1f}', " ");
        let arm = line("lab-armmix-ARM1");
        let dc = line("lab-armmix-DC1");
        // the arm64 machine: virt + aarch64 UEFI, NVMe, USB CDs, virtio NICs + the guest channel
        for want in [
            "-machine virt",
            "-device nvme,serial=wtl0,drive=disk0",
            "-device usb-storage,drive=cd0,removable=on",
            "virtio-net-pci,netdev=lan0,mac=52:54:00:4c:41:11",
            "name=org.wintestlab.0",
            "-serial chardev:ser0",
        ] {
            c.ok(arm.contains(want), format!("ARM1 argv lacks {:?}: {}", want, arm));
        }
        c.ok(!arm.contains("e1000") && !arm.contains("if=ide"), format!("ARM1 argv: {}", arm));
        // the x64 DC is unchanged: pc + IDE + e1000, on the same LAN port
        c.ok(
            dc.contains("-machine pc") && dc.contains("e1000,netdev=lan0") && dc.contains("local.port=47125"),
            format!("DC1 argv: {}", dc),
        );
        c.ok(arm.contains("local.port=47125"), "ARM1 on the lab LAN");
        // each machine's UEFI vars come from its own CPU's template
        c.file(&cx.state("armmix").join("ARM1-AAVMF_VARS.fd"));
        let i = run(cx, outs, &["status", "armmix"], &[]);
        c.has(&outs[i].1, "ARM1            member win11-arm64");
        let _ = run(cx, outs, &["restore", "armmix"], &[]);
    });

    scenario(&mut cx, "workgroup mode: one machine, local users, no lab LAN", &|cx, c, outs| {
        let conf = cx.cfg(
            "solo.json",
            "{ \"lab\": \"solo\", \"mount\": { \"X\": \"../app\" }, \"steps\": [\n\
               { \"as\": \"system\", \"run\": \"X:/setup.bat\" },\n\
               { \"run\": \"X:/smoke.exe\" },\n\
               { \"as\": \"admin2\", \"run\": \"X:/smoke.exe\" } ] }\n",
        );
        let i = run(cx, outs, &["run", &conf], &[]);
        let o = &outs[i].1;
        c.code(o, 0);
        for needle in [
            "=== building the 0 state of lab solo (workgroup; 1 machines, once) ===",
            "setting up WS1 (one reboot, then autologon)",
            "WS1 ready: WTL LAB_CLIENT_READY user=WS1%5Ctester",
            "[WS1 system] X:\\setup.bat -> exit 0",
            "[WS1 logon] X:\\smoke.exe -> exit 0",
            "[WS1 user] X:\\smoke.exe -> exit 0",
            "  lab solo: workgroup",
            "    single WS1             (NAT only)",
            "logon WS1\\tester",
            "config: solo.json  ->  ALL STEPS OK",
        ] {
            c.has(o, needle);
        }
        c.lacks(o, "DC ready");
        let st = cx.state("solo");
        let iso = IsoReader::open(&st.join("WS1.iso"), true).unwrap();
        let j: Value = serde_json::from_slice(iso.read("LAB/lab.json").unwrap()).unwrap();
        c.ok(
            s(&j, "mode") == "workgroup" && j["lan"] == json!(false) && s(&j, "domain").is_empty() && s(&j, "dc_ip").is_empty(),
            format!("lab.json {}", j),
        );
        c.ok(s(&j, "admin_password") == "WinTest!2026", "default admin password");
        let step3 = j["run"]["steps"].as_array().unwrap().iter().find(|x| x["n"] == json!(3)).cloned().unwrap_or(Value::Null);
        c.ok(step3["as_"] == json!("user") && step3["user"] == json!("admin2"), format!("step 3 {}", step3));
        // QEMU: user-mode NAT, no lab NIC
        let argv = std::fs::read_to_string(&cx.fake_log).unwrap_or_default();
        let line = argv.lines().rfind(|l| l.contains("lab-solo-WS1")).unwrap_or("").replace('\u{1f}', " ");
        c.ok(line.contains("user,id=net0") && !line.contains("lan0"), format!("WS1 argv: {}", line));
        let i = run(cx, outs, &["status", "solo"], &[]);
        c.has(&outs[i].1, "lab solo (");
        c.has(&outs[i].1, "workgroup, 1 machines");
        c.has(&outs[i].1, "  WS1             single 2019    - running");
        let i = run(cx, outs, &["restore", "solo"], &[]);
        c.code(&outs[i].1, 0);
    });

    scenario(&mut cx, "run vars: extends, ${WTL_*}, the guest's env, secrets masked", &|cx, c, outs| {
        std::fs::create_dir_all(cx.tmp.join("cfg/base")).unwrap();
        // the base file's mount is relative to the base file (cfg/base/ -> ../../app)
        cx.cfg(
            "base/vars-base.json",
            "{ \"vars\": { \"WTL_DIR\": \"../../app\", \"WTL_KEY\": { \"secret\": true }, \"WTL_TAG\": \"base\" },\n\
               \"lab\": \"solo\", \"mount\": { \"X\": \"${WTL_DIR}\" },\n\
               \"steps\": [ { \"as\": \"system\", \"run\": \"X:/setup.bat\", \"args\": \"-Key ${WTL_KEY} -Tag ${WTL_TAG} $env:PATH\" } ] }\n",
        );
        let conf = cx.cfg("vars.json", "{ \"extends\": \"./base/vars-base.json\", \"vars\": { \"WTL_TAG\": \"child\" } }\n");
        // a required var that is not set: refused before anything starts
        let i = run(cx, outs, &["run", &conf], &[]);
        c.code(&outs[i].1, 2);
        c.has(&outs[i].1, "WTL_KEY is not set");
        let i = run(cx, outs, &["status", &conf], &[]);
        c.code(&outs[i].1, 2);
        c.has(&outs[i].1, "WTL_KEY is not set");

        let env = [("WTL_KEY", "s3cr3t-value"), ("WTL_TAG", "fromenv")];
        let i = run(cx, outs, &["run", &conf], &env);
        let o = &outs[i].1;
        c.code(o, 0);
        c.has(o, "[WS1 system] X:\\setup.bat -> exit 0");
        c.lacks(o, "s3cr3t-value");
        c.lacks(o, "built before step variables");
        let st = cx.state("solo");
        let iso = IsoReader::open(&st.join("WS1.iso"), true).unwrap();
        let j: Value = serde_json::from_slice(iso.read("LAB/lab.json").unwrap()).unwrap();
        // the guest gets every var with its real value, in declaration order
        // ... then wtl's own: who and where the step runs
        let run_env = j["run"]["env"].as_object().cloned().unwrap_or_default();
        let names: Vec<&str> = run_env.keys().map(String::as_str).collect();
        c.ok(
            names
                == [
                    "WTL_DIR",
                    "WTL_KEY",
                    "WTL_TAG",
                    "WTL_ENVIRONMENT",
                    "WTL_ENVIRONMENT_ID",
                    "WTL_MACHINE",
                    "WTL_RUN_ID",
                    "WTL_HOSTNAME",
                    "WTL_HOST_WS1",
                ]
                && run_env["WTL_DIR"] == json!("../../app")
                && run_env["WTL_KEY"] == json!("s3cr3t-value")
                && run_env["WTL_TAG"] == json!("fromenv")
                && run_env["WTL_ENVIRONMENT"] == json!("solo")
                && run_env["WTL_MACHINE"] == json!("WS1"),
            format!("lab.json run.env {}", j["run"]["env"]),
        );
        let step1 = j["run"]["steps"][0].clone();
        c.ok(step1["args"] == json!("-Key s3cr3t-value -Tag fromenv $env:PATH"), format!("step 1 {}", step1));
        // result.json lists the values used, the secret as ***
        let res = std::fs::canonicalize(st.join("last")).unwrap_or_default();
        let raw = std::fs::read_to_string(res.join("result.json")).unwrap_or_default();
        let rj: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
        c.ok(
            rj["vars"] == json!({ "WTL_DIR": "../../app", "WTL_KEY": "***", "WTL_TAG": "fromenv" }),
            format!("result.json vars {}", rj["vars"]),
        );
        c.ok(!raw.contains("s3cr3t-value"), "result.json has no secret");
        // config.json is the run file as written
        c.content(&res.join("config.json"), std::fs::read(&conf).unwrap_or_default().as_slice());

        // a secret in an error message is masked too
        let bad = cx.cfg(
            "vars-bad.json",
            "{ \"extends\": \"./base/vars-base.json\", \"steps\": [ { \"as\": \"system\", \"run\": \"X:/${WTL_KEY}.zip\" } ] }\n",
        );
        let i = run(cx, outs, &["run", &bad], &env);
        c.code(&outs[i].1, 2);
        c.has(&outs[i].1, "X:/***.zip");
        c.lacks(&outs[i].1, "s3cr3t-value");

        // a 0 state built before run vars: the SYSTEM worker cannot set them -> a warning
        let zp = st.join("0-state.json");
        let zero = std::fs::read_to_string(&zp).unwrap_or_default();
        let mut z: Value = serde_json::from_str(&zero).unwrap_or(Value::Null);
        c.ok(z["guest"] == json!(["step-env"]), format!("0-state.json {}", zero));
        if let Some(m) = z.as_object_mut() {
            m.remove("guest");
        }
        std::fs::write(&zp, z.to_string()).unwrap();
        let i = run(cx, outs, &["run", &conf], &env);
        c.code(&outs[i].1, 0);
        c.has(&outs[i].1, "built before step variables");
        std::fs::write(&zp, zero).unwrap();
        let i = run(cx, outs, &["restore", "solo"], &[]);
        c.code(&outs[i].1, 0);
    });

    scenario(&mut cx, "Server 2008 R2 (VHD import): workgroup run uses its own PowerShell 2.0 scripts", &|cx, c, outs| {
        let conf = cx.cfg(
            "s2008.json",
            "{ \"lab\": \"solo2008\", \"mount\": { \"X\": \"../app\" }, \"steps\": [\n\
               { \"as\": \"system\", \"run\": \"X:/setup.bat\" },\n\
               { \"run\": \"X:/smoke.exe\" } ] }\n",
        );
        let i = run(cx, outs, &["run", &conf], &[]);
        let o = &outs[i].1;
        c.code(o, 0);
        c.has(o, "WS2008 ready: WTL LAB_CLIENT_READY");
        c.has(o, "ALL STEPS OK");
        // the control disc must carry the 2008 (PowerShell 2.0) guest scripts, not the shared
        // PS4 ones: guest_file_for picked guest/2008/ over guest/
        let st = cx.state("solo2008");
        let iso = IsoReader::open(&st.join("WS2008.iso"), true).unwrap();
        let lab = String::from_utf8_lossy(iso.read("LAB/lab.ps1").unwrap()).into_owned();
        c.ok(lab.contains("ConvertFrom-WtlJson"), "the 2008 control disc carries the PS2 lab.ps1");
        let svc = String::from_utf8_lossy(iso.read("LAB/labsvc.ps1").unwrap()).into_owned();
        c.ok(svc.contains("ConvertFrom-WtlJson"), "the 2008 control disc carries the PS2 labsvc.ps1");
        let j: Value = serde_json::from_slice(iso.read("LAB/lab.json").unwrap()).unwrap();
        c.ok(s(&j, "mode") == "workgroup", format!("lab.json {}", j));
        // an imported Hyper-V VHD boots from IDE, not AHCI (else old Windows 0x7Bs)
        let argv = std::fs::read_to_string(&cx.fake_log).unwrap_or_default();
        let line = argv.lines().rfind(|l| l.contains("lab-solo2008-WS2008")).unwrap_or("").replace('\u{1f}', " ");
        c.ok(line.contains("id=disk0,if=ide") && !line.contains("ich9-ahci"), format!("WS2008 argv: {}", line));
        c.ok(line.contains("usb-tablet"), format!("WS2008 keeps the USB tablet: {}", line));
        let i = run(cx, outs, &["restore", "solo2008"], &[]);
        c.code(&outs[i].1, 0);
    });

    scenario(&mut cx, "Server 2003 (VHD import, no PowerShell): workgroup run carries the VBScript guest", &|cx, c, outs| {
        let conf = cx.cfg(
            "s2003.json",
            "{ \"lab\": \"solo2003\", \"mount\": { \"X\": \"../app\" }, \"steps\": [\n\
               { \"as\": \"system\", \"run\": \"X:/setup.bat\" },\n\
               { \"run\": \"X:/smoke.exe\" } ] }\n",
        );
        let i = run(cx, outs, &["run", &conf], &[]);
        let o = &outs[i].1;
        c.code(o, 0);
        c.has(o, "WS2003 ready: WTL LAB_CLIENT_READY");
        c.has(o, "ALL STEPS OK");
        // the control disc carries the VBScript agent / lab / worker and their shared library,
        // and none of the PowerShell scripts
        let st = cx.state("solo2003");
        let iso = IsoReader::open(&st.join("WS2003.iso"), true).unwrap();
        for f in ["LAB/agent.vbs", "LAB/lab.vbs", "LAB/labsvc.vbs", "LAB/common.vbs", "LAB/lab.json"] {
            c.ok(iso.read(f).is_some(), format!("{} on the 2003 control disc", f));
        }
        for f in ["LAB/agent.ps1", "LAB/lab.ps1", "LAB/labsvc.ps1"] {
            c.ok(iso.read(f).is_none(), format!("{} must not be on the 2003 control disc", f));
        }
        // batch / VBScript files are staged with CRLF line ends
        let lab = String::from_utf8_lossy(iso.read("LAB/lab.vbs").unwrap()).into_owned();
        c.ok(
            lab.contains("InvokeLab") && lab.contains("\r\n") && !lab.replace("\r\n", "").contains('\n'),
            "2003 lab.vbs has CRLF line ends",
        );
        // booted from IDE like every imported VHD
        let argv = std::fs::read_to_string(&cx.fake_log).unwrap_or_default();
        let line = argv.lines().rfind(|l| l.contains("lab-solo2003-WS2003")).unwrap_or("").replace('\u{1f}', " ");
        c.ok(line.contains("id=disk0,if=ide") && !line.contains("ich9-ahci"), format!("WS2003 argv: {}", line));
        c.ok(!line.contains("usb"), format!("WS2003 gets no USB controller (it hangs at its second boot with one): {}", line));
        let i = run(cx, outs, &["restore", "solo2003"], &[]);
        c.code(&outs[i].1, 0);
    });

    scenario(&mut cx, "Server 2008 R2 is refused in an AD lab (standalone only for now)", &|cx, c, outs| {
        let i = run(cx, outs, &["status", "ad2008"], &[]);
        c.code(&outs[i].1, 2);
        c.has(&outs[i].1, "supported standalone only");
    });

    scenario(&mut cx, "a project's own lab file: a 2012 DC + a 2019 member server", &|cx, c, outs| {
        let conf = cx.tmp.join("proj/wintest/run.json").to_string_lossy().into_owned();
        let i = run(cx, outs, &["run", &conf], &[]);
        let o = &outs[i].1;
        c.code(o, 0);
        c.has(o, "(domain corp.test; 2 machines, once)");
        c.has(o, "[SRV1 system] X:\\smoke.ps1 -> exit 0");
        c.has(o, "[DC1 system] X:\\smoke.ps1 -> exit 0");
        // a member without a logon user logs on as the domain Administrator
        c.has(o, "SRV1            10.77.96.20");
        c.has(o, "CORP\\Administrator");
        // the state is named after the file + a hash of its path (not just "lab")
        let states: Vec<String> = std::fs::read_dir(cx.root.join("labs/state"))
            .map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
            .unwrap_or_default();
        let st = states.iter().find(|n| n.starts_with("lab-") && n.len() == 12).cloned();
        c.ok(st.is_some(), format!("no lab-<hash> state dir: {:?}", states));
        if let Some(st) = st {
            let dir = cx.root.join("labs/state").join(&st);
            c.file(&dir.join("DC1-0.qcow2"));
            // the hints name the lab file (what `wtl` takes for a project lab)
            c.has(o, &format!("wtl restore {}", cx.tmp.join("proj/wintest/lab.json").display()));
            c.has(o, &format!("lab {}: domain corp.test", st));
            // the steps only name X:, but Y: (the build output) is mounted too
            c.file(&dir.join("mount-SRV1-Y.iso"));
            // a 2012 DC and a 2019 member: images 2012-dc and 2019-client
            c.content(
                &dir.join("DC1-0.qcow2"),
                format!("QFI\u{fb} fake qcow2 backing={}\n", cx.root.join("images/2012-dc.qcow2").display()).as_bytes(),
            );
            c.content(
                &dir.join("SRV1-0.qcow2"),
                format!("QFI\u{fb} fake qcow2 backing={}\n", cx.root.join("images/2019-client.qcow2").display()).as_bytes(),
            );
        }
        // the project file is also accepted where a LAB goes
        let i = run(cx, outs, &["restore", &conf], &[]);
        c.code(&outs[i].1, 0);
        c.ok(cx.live_fakes().is_empty(), format!("still running: {:?}", cx.live_fakes()));
    });

    scenario(&mut cx, "domain set without a DC is rejected before boot", &|cx, c, outs| {
        let lab = cx.tmp.join("proj/nodc.json");
        std::fs::write(&lab, r#"{ "domain": "x.test", "netbios": "X", "machines": { "A": { "os": "2019" } } }"#).unwrap();
        let conf = cx.cfg(
            "nodc-run.json",
            &format!("{{ \"lab\": \"{}\", \"steps\": [ {{ \"as\": \"system\", \"run\": \"C:/x.bat\" }} ] }}", lab.display()),
        );
        let i = run(cx, outs, &["run", &conf], &[]);
        c.code(&outs[i].1, 2);
        c.has(&outs[i].1, "domain/netbios is set but no machine has the \"ad-ds\" role");
        let missing =
            cx.cfg("missing-run.json", "{ \"lab\": \"./nope/lab.json\", \"steps\": [ { \"as\": \"system\", \"run\": \"C:/x.bat\" } ] }");
        let i = run(cx, outs, &["run", &missing], &[]);
        c.code(&outs[i].1, 2);
        c.has(&outs[i].1, "lab \"./nope/lab.json\" not found");
        c.ok(cx.live_fakes().is_empty(), "a VM was started");
    });

    scenario(&mut cx, "JSON lab: unknown field is rejected before boot", &|cx, c, outs| {
        let bad = cx.cfg("unk.json", "{ \"lab\": \"tri\", \"frobnicate\": 1, \"steps\": [] }");
        let i = run(cx, outs, &["run", &bad], &[]);
        c.code(&outs[i].1, 2);
        c.has(&outs[i].1, "unknown field");
    });

    scenario(&mut cx, "roles: role+roles clash, ad-cs in a workgroup, features on a client", &|cx, c, outs| {
        // setting both role and roles is an error
        let both = cx.tmp.join("proj/both.json");
        std::fs::write(&both, r#"{ "domain": "d.test", "netbios": "D", "network": {"subnet":"10.1.1","lan_port":41111}, "machines": { "DC": { "os": "2019", "role": "dc", "roles": ["ad-ds"] } } }"#).unwrap();
        let conf = cx.cfg(
            "both-run.json",
            &format!(
                "{{ \"lab\": \"{}\", \"steps\": [ {{ \"on\": \"DC\", \"as\": \"system\", \"run\": \"C:/x.bat\" }} ] }}",
                both.display()
            ),
        );
        let i = run(cx, outs, &["run", &conf], &[]);
        c.code(&outs[i].1, 2);
        c.has(&outs[i].1, "set either \"role\" or \"roles\"");
        // ad-cs needs an AD lab (a standalone CA is not supported yet)
        let wg = cx.tmp.join("proj/wgca.json");
        std::fs::write(&wg, r#"{ "machines": { "S": { "os": "2019", "roles": ["ad-cs"] } } }"#).unwrap();
        let conf = cx.cfg(
            "wgca-run.json",
            &format!("{{ \"lab\": \"{}\", \"steps\": [ {{ \"as\": \"system\", \"run\": \"C:/x.bat\" }} ] }}", wg.display()),
        );
        let i = run(cx, outs, &["run", &conf], &[]);
        c.code(&outs[i].1, 2);
        c.has(&outs[i].1, "the \"ad-cs\" role needs an AD lab");
        // a client OS cannot carry a server role or a feature
        let cl = cx.tmp.join("proj/clientrole.json");
        std::fs::write(&cl, r#"{ "domain": "d.test", "netbios": "D", "network": {"subnet":"10.1.2","lan_port":41112}, "machines": { "DC": { "os": "2019", "role": "dc" }, "W": { "os": "win11", "roles": ["iis"] } } }"#).unwrap();
        let conf = cx.cfg(
            "clientrole-run.json",
            &format!("{{ \"lab\": \"{}\", \"steps\": [ {{ \"on\": \"W\", \"as\": \"system\", \"run\": \"C:/x.bat\" }} ] }}", cl.display()),
        );
        let i = run(cx, outs, &["run", &conf], &[]);
        c.code(&outs[i].1, 2);
        c.has(&outs[i].1, "role needs a server OS");
        c.ok(cx.live_fakes().is_empty(), "a VM was started for an invalid roles lab");
    });

    scenario(&mut cx, "roles: build the 0 state with dhcp/ad-cs/iis, features and ordered setup", &|cx, c, outs| {
        let st = cx.state("roles");
        let i = run(cx, outs, &["prepare", "roles"], &[]);
        let o = &outs[i].1;
        c.code(o, 0);
        for needle in [
            "=== building the 0 state of lab roles (domain pki.test; 3 machines, once) ===",
            // the DC installs ad-ds (inline) + dhcp; the CA member configures ad-cs + iis
            "[guest] WTL LAB_CONFIGURE machine=DC role=dc",
            "[guest] WTL LAB_ROLE machine=DC name=dhcp state=ready",
            "[guest] WTL LAB_CONFIGURE machine=CA role=member",
            // the resolved option values the build disc carried, echoed back by the fake guest
            "[guest] WTL LAB_ROLE machine=DC name=dhcp state=ready scope=10.77.95.100-10.77.95.199 router=10.77.95.10",
            "[guest] WTL LAB_ROLE machine=CA name=ad-cs state=ready ca=PKI-ROOT-CA",
            "[guest] WTL LAB_ROLE machine=CA name=iis state=ready",
            "[guest] WTL LAB_FEATURE machine=CA name=Telnet-Client",
            // setup scripts staged on the control disc, run in order, the second as operator
            "[guest] WTL LAB_SETUP machine=CA n=1 as=system script=setup/first.ps1 state=ok",
            "[guest] WTL LAB_SETUP machine=CA n=2 as=user script=setup/second.bat state=ok",
            "0 state saved for lab roles (3 machines)",
        ] {
            c.has(o, needle);
        }
        // first.ps1 before second.bat
        c.ok(
            o.text.find("script=setup/first.ps1 state=ok").unwrap_or(usize::MAX)
                < o.text.find("script=setup/second.bat state=running").unwrap_or(0),
            "setup scripts out of order",
        );
        c.file(&st.join("0-state.json"));
        // the plain member CL carries no build config: no configure event for it
        c.lacks(o, "LAB_CONFIGURE machine=CL");
        let _ = run(cx, outs, &["restore", "roles"], &[]);
    });

    scenario(&mut cx, "roles: a failing setup script fails the build", &|cx, c, outs| {
        // a fresh build is needed (prepare is a no-op once the 0 state exists)
        let _ = run(cx, outs, &["rebuild", "roles"], &[]);
        // point second.bat at a FAKE_LAB_FAIL body for this run
        std::fs::write(cx.root.join("labs/setup/second.bat"), "FAKE_LAB_FAIL\r\n").unwrap();
        let i = run(cx, outs, &["prepare", "roles"], &[]);
        let o = &outs[i].1;
        c.code(o, 2);
        c.has(o, "CA: setup script setup/second.bat failed");
        std::fs::write(cx.root.join("labs/setup/second.bat"), "@echo off\r\necho step two\r\n").unwrap();
        let _ = run(cx, outs, &["restore", "roles"], &[]);
        c.no_file(&cx.state("roles").join("0-state.json"));
    });

    scenario(&mut cx, "environments: up --new, address, list, run --lab, rebuild keeps theirs, rm", &|cx, c, outs| {
        // "environment <name> (id <id>) made from lab ..." -> (name, id)
        let made = |o: &Out| -> (String, String) {
            let l = o.text.lines().find_map(|l| l.split("environment ").nth(1).filter(|r| r.contains(" made from lab "))).unwrap_or("");
            let name = l.split(' ').next().unwrap_or("").to_string();
            let id = l.split("(id ").nth(1).and_then(|r| r.split(')').next()).unwrap_or("").to_string();
            (name, id)
        };
        let env_json = |name: &str| -> Value {
            serde_json::from_slice(&std::fs::read(cx.state(name).join("env.json")).unwrap_or_default()).unwrap_or(Value::Null)
        };
        let i = run(cx, outs, &["up", "solo", "--new", "--name", "exp42", "--note", "first try: new installer"], &[]);
        let o = &outs[i].1;
        c.code(o, 0);
        let (name, id) = made(o);
        c.ok(name == "solo-exp42" && id.len() == 8, format!("made {:?} {:?}", name, id));
        c.has(o, "environment solo-exp42 (id ");
        c.has(o, "WS1 ready");
        let ej = env_json("solo-exp42");
        c.ok(
            ej["lab"] == json!("solo") && ej["id"] == json!(id) && ej["note"] == json!("first try: new installer"),
            format!("env.json {}", ej),
        );
        // its 0-state disk is an overlay on the lab's, through the frozen hard link
        let frozen = PathBuf::from(s(&ej, "frozen"));
        let overlay = std::fs::read_to_string(cx.state("solo-exp42").join("WS1-0.qcow2")).unwrap_or_default();
        c.ok(overlay.contains(&format!("backing={}", frozen.join("WS1-0.qcow2").display())), format!("WS1-0.qcow2: {}", overlay));
        c.file(&frozen.join("WS1-0.qcow2"));
        // the VM is the environment's; its control disc has no instance record (steps get who
        // they are as variables)
        let argv = std::fs::read_to_string(&cx.fake_log).unwrap_or_default();
        c.ok(argv.contains("lab-solo-exp42-WS1"), "the VM is named after the environment");
        let iso = IsoReader::open(&cx.state("solo-exp42").join("WS1.iso"), true).unwrap();
        let lj: Value = serde_json::from_slice(iso.read("LAB/lab.json").unwrap()).unwrap();
        c.ok(lj["machine"] == json!("WS1") && lj.get("instance").is_none(), format!("up disc {}", lj));
        // the lab itself is untouched
        let i = run(cx, outs, &["up", "solo"], &[]);
        c.code(&outs[i].1, 0);
        // a second, random-named one runs next to both
        let i = run(cx, outs, &["up", "solo", "--new"], &[]);
        c.code(&outs[i].1, 0);
        let (rnd, rnd_id) = made(&outs[i].1);
        c.ok(rnd.starts_with("solo-") && rnd.len() == 9 && rnd_id != id, format!("random environment {:?}", rnd));
        c.ok(cx.live_fakes().len() == 3, format!("solo + 2 environments running: {:?}", cx.live_fakes()));
        let i = run(cx, outs, &["list"], &[]);
        let o = &outs[i].1;
        c.code(o, 0);
        for needle in ["NAME", "solo-exp42", &id, "first try: new installer", &rnd] {
            c.has(o, needle);
        }
        c.ok(o.text.lines().any(|l| l.starts_with("solo-exp42 ") && l.contains("running")), "solo-exp42 is listed running");
        // the lab itself has an id of its own
        let lab_id = o.text.lines().find(|l| l.starts_with("solo ")).and_then(|l| l.split_whitespace().nth(2)).unwrap_or("");
        c.ok(lab_id.len() == 8 && lab_id != id, format!("solo's id {:?}", lab_id));
        // one machine
        let i = run(cx, outs, &["status", "solo-exp42/WS1"], &[]);
        c.code(&outs[i].1, 0);
        c.has(&outs[i].1, "environment solo-exp42 (id ");
        c.has(&outs[i].1, "note: first try: new installer");
        let i = run(cx, outs, &["status", "solo-exp42/NOPE"], &[]);
        c.code(&outs[i].1, 2);
        c.has(&outs[i].1, "has no machine NOPE");
        let i = run(cx, outs, &["status", "solo"], &[]);
        c.has(&outs[i].1, "environments: ");
        c.has(&outs[i].1, "solo-exp42");
        // run in one: the steps get its identity, result.json carries it
        let conf = cx.cfg(
            "env-run.json",
            "{ \"lab\": \"solo\", \"mount\": { \"X\": \"../app\" }, \"steps\": [ { \"as\": \"system\", \"run\": \"X:/setup.bat\" } ] }\n",
        );
        let i = run(cx, outs, &["run", &conf, "--lab", "solo-exp42"], &[]);
        c.code(&outs[i].1, 0);
        let res = std::fs::canonicalize(cx.state("solo-exp42").join("last")).unwrap_or_default();
        let rj: Value = serde_json::from_slice(&std::fs::read(res.join("result.json")).unwrap_or_default()).unwrap_or(Value::Null);
        c.ok(
            rj["lab"] == json!("solo")
                && rj["environment"] == json!("solo-exp42")
                && rj["id"] == json!(id)
                && s(&rj, "run_id").ends_with(&format!("-{}", id)),
            format!("result.json {} {} {} {}", rj["lab"], rj["environment"], rj["id"], rj["run_id"]),
        );
        let iso = IsoReader::open(&cx.state("solo-exp42").join("WS1.iso"), true).unwrap();
        let lj: Value = serde_json::from_slice(iso.read("LAB/lab.json").unwrap()).unwrap();
        let env = &lj["run"]["env"];
        c.ok(
            env["WTL_ENVIRONMENT"] == json!("solo-exp42")
                && env["WTL_ENVIRONMENT_ID"] == json!(id)
                && env["WTL_MACHINE"] == json!("WS1")
                && env["WTL_RUN_ID"] == rj["run_id"],
            format!("step env {}", env),
        );
        // a copy of an environment (one level deep): its own, from solo-exp42's 0 state
        let i = run(cx, outs, &["up", "solo-exp42", "--new", "--name", "copy"], &[]);
        c.code(&outs[i].1, 0);
        c.has(&outs[i].1, "environment solo-exp42-copy (id ");
        c.has(&outs[i].1, "made from environment solo-exp42's 0 state");
        let cj = env_json("solo-exp42-copy");
        c.ok(cj["lab"] == json!("solo") && cj["parent"] == json!("solo-exp42"), format!("copy env.json {}", cj));
        let i = run(cx, outs, &["up", "solo-exp42-copy", "--new"], &[]);
        c.code(&outs[i].1, 2);
        c.has(&outs[i].1, "is a copy of solo-exp42 already");
        // the environment a copy was made from cannot go first
        let i = run(cx, outs, &["rm", "solo-exp42"], &[]);
        c.code(&outs[i].1, 2);
        c.has(&outs[i].1, "has copies (solo-exp42-copy)");
        let i = run(cx, outs, &["rm", "solo-exp42-copy"], &[]);
        c.code(&outs[i].1, 0);
        // --new and --lab are refused where they make no sense
        let i = run(cx, outs, &["status", "solo", "--name", "x"], &[]);
        c.code(&outs[i].1, 2);
        let i = run(cx, outs, &["rm", "solo"], &[]);
        c.code(&outs[i].1, 2);
        c.has(&outs[i].1, "is a lab, not an environment");
        // rebuilding the lab keeps the environments' 0 state; a run --new builds a new one first
        let i = run(cx, outs, &["rebuild", "solo"], &[]);
        c.code(&outs[i].1, 0);
        c.has(&outs[i].1, "its 2 environment(s) keep the 0 state they were made from");
        c.no_file(&cx.state("solo").join("WS1-0.qcow2"));
        c.file(&frozen.join("WS1-0.qcow2"));
        let i = run(cx, outs, &["up", "solo-exp42"], &[]);
        c.code(&outs[i].1, 0);
        let i = run(cx, outs, &["run", &conf, "--new"], &[]);
        let o = &outs[i].1;
        c.code(o, 0);
        c.has(o, "=== building the 0 state of lab solo");
        let (third, _) = made(o);
        c.ok(third.starts_with("solo-"), format!("run --new made {:?}", third));
        // an environment made fresh again: from the lab's new 0 state, same id
        let i = run(cx, outs, &["rebuild", "solo-exp42"], &[]);
        c.code(&outs[i].1, 0);
        c.has(&outs[i].1, "environment solo-exp42 is fresh again from solo's 0 state");
        let ej2 = env_json("solo-exp42");
        c.ok(ej2["id"] == json!(id) && ej2["frozen"] != ej["frozen"], format!("refreshed env.json {}", ej2));
        // the old frozen 0 state goes with the last environment that uses it
        c.file(&frozen.join("WS1-0.qcow2"));
        let i = run(cx, outs, &["rm", &rnd], &[]);
        c.code(&outs[i].1, 0);
        c.ok(!frozen.exists(), format!("{} should be gone", frozen.display()));
        for e in ["solo-exp42", third.as_str()] {
            let i = run(cx, outs, &["rm", e], &[]);
            c.code(&outs[i].1, 0);
            c.ok(!cx.state(e).exists(), format!("{} deleted", e));
        }
        let i = run(cx, outs, &["restore", "solo"], &[]);
        c.code(&outs[i].1, 0);
        // an AD lab's environment gets its own lab LAN port
        let i = run(cx, outs, &["up", "tri", "--new", "--name", "lan"], &[]);
        c.code(&outs[i].1, 0);
        let argv = std::fs::read_to_string(&cx.fake_log).unwrap_or_default();
        let port_of = |vm: &str| -> String {
            let line = argv.lines().map(|l| l.replace('\u{1f}', " ")).rfind(|l| l.contains(vm)).unwrap_or_default();
            line.split("local.port=").nth(1).map(|r| r.chars().take_while(|ch| ch.is_ascii_digit()).collect()).unwrap_or_default()
        };
        let (lab_port, env_port) = (port_of("lab-tri-DC "), port_of("lab-tri-lan-DC "));
        c.ok(!env_port.is_empty() && env_port != lab_port, format!("tri port {:?}, tri-lan port {:?}", lab_port, env_port));
        c.ok(
            env_json("tri-lan")["lan_port"] == json!(env_port.parse::<u16>().unwrap_or(0)),
            format!("env.json lan_port {}", env_json("tri-lan")["lan_port"]),
        );
        let i = run(cx, outs, &["rm", "tri-lan"], &[]);
        c.code(&outs[i].1, 0);
    });

    scenario(&mut cx, "renames: --rename, tiers, CA reinstall plan, state, copies, ${WTL_HOST_*}, --rm", &|cx, c, outs| {
        let env_json = |name: &str| -> Value {
            serde_json::from_slice(&std::fs::read(cx.state(name).join("env.json")).unwrap_or_default()).unwrap_or(Value::Null)
        };
        // bad names fail before anything is made
        let i = run(cx, outs, &["up", "solo", "--new", "--rename", "NOPE"], &[]);
        c.code(&outs[i].1, 2);
        c.has(&outs[i].1, "has no machine NOPE");
        let i = run(cx, outs, &["up", "solo", "--new", "--name", "averyverylongname", "--rename", "WS1"], &[]);
        c.code(&outs[i].1, 2);
        c.has(&outs[i].1, "characters (Windows allows 15)");
        c.ok(!cx.state("solo-averyverylongname").exists(), "nothing made for a bad name");
        let i = run(cx, outs, &["up", "solo", "--rename", "WS1"], &[]);
        c.code(&outs[i].1, 2); // clap: --rename needs --new

        // a workgroup machine: renamed for real, then saved
        let i = run(cx, outs, &["up", "solo", "--new", "--name", "rn", "--rename", "all"], &[]);
        let o = &outs[i].1;
        c.code(o, 0);
        c.has(o, "=== renaming the machines of environment solo-rn: WS1 -> WS1-RN ===");
        c.has(o, "WS1: WTL LAB_RENAMED machine=WS1 from=WS1 to=WS1-RN");
        c.has(o, "environment solo-rn: machines renamed and saved");
        c.has(o, "single WS1 (WS1-RN)");
        let ej = env_json("solo-rn");
        c.ok(ej["hostnames"] == json!({ "WS1": "WS1-RN" }) && ej["rename_state"] == json!("done"), format!("env.json {}", ej));
        c.file(&cx.state("solo-rn").join("0-state.json"));
        c.no_file(&cx.state("solo-rn").join("0-state.json.pending"));
        let iso = IsoReader::open(&cx.state("solo-rn").join("WS1.iso"), true).unwrap();
        let lj: Value = serde_json::from_slice(iso.read("LAB/lab.json").unwrap()).unwrap();
        c.ok(lj["hostname"] == json!("WS1-RN") && lj["rename"].is_null(), format!("up disc {} {}", lj["hostname"], lj["rename"]));
        let i = run(cx, outs, &["status", "solo-rn"], &[]);
        c.has(&outs[i].1, "hostname WS1-RN");
        c.has(&outs[i].1, "renamed (done): WS1 -> WS1-RN");
        // run files use ${WTL_HOST_<KEY>}; steps get WTL_HOSTNAME
        let conf = cx.cfg(
            "hosts-run.json",
            "{ \"lab\": \"solo\", \"mount\": { \"X\": \"../app\" }, \"steps\": [ { \"as\": \"system\", \"run\": \"X:/setup.bat\", \"args\": \"--host ${WTL_HOST_WS1}\" } ] }\n",
        );
        let i = run(cx, outs, &["run", &conf, "--lab", "solo-rn"], &[]);
        c.code(&outs[i].1, 0);
        let iso = IsoReader::open(&cx.state("solo-rn").join("WS1.iso"), true).unwrap();
        let lj: Value = serde_json::from_slice(iso.read("LAB/lab.json").unwrap()).unwrap();
        c.ok(lj["run"]["steps"][0]["args"] == json!("--host WS1-RN"), format!("step args {}", lj["run"]["steps"][0]["args"]));
        c.ok(
            lj["run"]["env"]["WTL_HOSTNAME"] == json!("WS1-RN") && lj["run"]["env"]["WTL_HOST_WS1"] == json!("WS1-RN"),
            format!("env {}", lj["run"]["env"]),
        );
        let res = std::fs::canonicalize(cx.state("solo-rn").join("last")).unwrap_or_default();
        let rj: Value = serde_json::from_slice(&std::fs::read(res.join("result.json")).unwrap_or_default()).unwrap_or(Value::Null);
        c.ok(rj["hostnames"] == json!({ "WS1": "WS1-RN" }), format!("result.json hostnames {}", rj["hostnames"]));
        let i = run(cx, outs, &["run", &conf, "--lab", "solo"], &[]);
        c.code(&outs[i].1, 0);
        // a copy of a renamed environment keeps its names and needs no rename
        let i = run(cx, outs, &["up", "solo-rn", "--new", "--name", "c"], &[]);
        c.code(&outs[i].1, 0);
        c.lacks(&outs[i].1, "renaming the machines");
        c.ok(env_json("solo-rn-c")["hostnames"] == json!({ "WS1": "WS1-RN" }), format!("copy {}", env_json("solo-rn-c")));
        let i = run(cx, outs, &["up", "solo-rn", "--new", "--rename", "WS1=X1"], &[]);
        c.code(&outs[i].1, 2);
        c.has(&outs[i].1, "--rename makes new names from a lab");
        // rebuild replays the rename
        let i = run(cx, outs, &["rebuild", "solo-rn-c"], &[]);
        c.code(&outs[i].1, 0);
        c.lacks(&outs[i].1, "renaming the machines");
        let i = run(cx, outs, &["rm", "solo-rn-c"], &[]);
        c.code(&outs[i].1, 0);
        let i = run(cx, outs, &["rebuild", "solo-rn"], &[]);
        c.code(&outs[i].1, 0);
        c.has(&outs[i].1, "=== renaming the machines of environment solo-rn: WS1 -> WS1-RN ===");
        // a failed rename: the environment is kept for a look, but only rebuild / rm act on it
        let i = run(cx, outs, &["up", "solo", "--new", "--name", "bad", "--rename", "WS1"], &[("FAKE_RENAME_FAIL", "1")]);
        c.code(&outs[i].1, 2);
        c.has(&outs[i].1, "renaming the machines of environment solo-bad failed");
        c.ok(env_json("solo-bad")["rename_state"] == json!("failed"), format!("{}", env_json("solo-bad")));
        let i = run(cx, outs, &["up", "solo-bad"], &[]);
        c.code(&outs[i].1, 2);
        c.has(&outs[i].1, "environment solo-bad's rename failed");
        let i = run(cx, outs, &["rebuild", "solo-bad"], &[]);
        c.code(&outs[i].1, 0);
        c.ok(env_json("solo-bad")["rename_state"] == json!("done"), "rebuild renamed it");
        for e in ["solo-bad", "solo-rn"] {
            let i = run(cx, outs, &["rm", e], &[]);
            c.code(&outs[i].1, 0);
        }
        // run --new --rename: the run file's ${WTL_HOST_*} are the new environment's names
        let i = run(cx, outs, &["run", &conf, "--new", "--name", "hz", "--rename", "WS1"], &[]);
        c.code(&outs[i].1, 0);
        let iso = IsoReader::open(&cx.state("solo-hz").join("WS1.iso"), true).unwrap();
        let lj: Value = serde_json::from_slice(iso.read("LAB/lab.json").unwrap()).unwrap();
        c.ok(lj["run"]["steps"][0]["args"] == json!("--host WS1-HZ"), format!("run --new --rename args {}", lj["run"]["steps"][0]["args"]));
        let i = run(cx, outs, &["rm", "solo-hz"], &[]);
        c.code(&outs[i].1, 0);
        // a throwaway renamed run: results kept in the lab's runs/, the environment gone
        let i = run(cx, outs, &["run", &conf, "--new", "--rename", "WS1", "--rm"], &[]);
        let o = &outs[i].1;
        c.code(o, 0);
        c.has(o, "results kept: ");
        let kept = o.text.lines().find_map(|l| l.split("results kept: ").nth(1)).unwrap_or("").trim().to_string();
        let rj: Value =
            serde_json::from_slice(&std::fs::read(Path::new(&kept).join("result.json")).unwrap_or_default()).unwrap_or(Value::Null);
        let env = s(&rj, "environment").to_string();
        c.ok(kept.contains("/solo/runs/") && env.starts_with("solo-") && !cx.state(&env).exists(), format!("kept {} env {}", kept, env));
        c.ok(
            rj["hostnames"]["WS1"].as_str().is_some_and(|h| h.starts_with("WS1-") && h != "WS1"),
            format!("hostnames {}", rj["hostnames"]),
        );
        let i = run(cx, outs, &["restore", "solo"], &[]);
        c.code(&outs[i].1, 0);

        // an AD lab with a CA: the DC first, then the CA host, then the member; each part right
        let i = run(cx, outs, &["up", "roles", "--new", "--name", "x1", "--rename", "all"], &[]);
        let o = &outs[i].1;
        c.code(o, 0);
        c.has(o, "DC -> DC-X1, CA -> CA-X1, CL -> CL-X1");
        c.has(o, "CA hosts the CA PKI-ROOT-CA: it is reinstalled under its new name");
        c.has(o, "rename plan machine=DC from=DC to=DC-X1 ca=0 admin=0 dhcp=1 iis= reenroll=0 reenroll_user=0 ca_name=PKI-ROOT-CA peers=CA>CA-X1,CL>CL-X1");
        c.has(o, "rename plan machine=CA from=CA to=CA-X1 ca=1 admin=1 dhcp=0 iis=C:\\inetpub\\wwwroot reenroll=0 reenroll_user=0");
        c.has(o, "rename plan machine=CL from=CL to=CL-X1 ca=0 admin=0 dhcp=0 iis= reenroll=1 reenroll_user=1");
        c.has(o, "DC: WTL LAB_DNS_CLEAN removed=CA,CL,DC");
        // positions after the rename began (the same command built the lab's 0 state first)
        let start = o.text.find("=== renaming the machines").unwrap_or(0);
        let pos = |needle: &str| o.text[start..].find(needle).unwrap_or(usize::MAX);
        c.ok(
            pos("DC: WTL LAB_RENAMED") < pos("CA started") && pos("CA: WTL LAB_RENAMED") < pos("CL started"),
            "tiers: the DC renamed before the CA boots, the CA before the member",
        );
        // only the member: the CA keeps its name (no reinstall), the member still re-enrolls
        let i = run(cx, outs, &["up", "roles", "--new", "--name", "x2", "--rename", "CL"], &[]);
        let o = &outs[i].1;
        c.code(o, 0);
        c.lacks(o, "is reinstalled under its new name");
        c.has(o, "rename plan machine=CL from=CL to=CL-X2 ca=0 admin=0 dhcp=0 iis= reenroll=1 reenroll_user=0");
        c.has(o, "peers=CL>CL-X2");
        for e in ["roles-x1", "roles-x2"] {
            let i = run(cx, outs, &["rm", e], &[]);
            c.code(&outs[i].1, 0);
        }
        let i = run(cx, outs, &["restore", "roles"], &[]);
        c.code(&outs[i].1, 0);
    });

    let leaks = cx.live_fakes();
    if !leaks.is_empty() {
        println!("FAIL fake QEMUs left running: {:?}", leaks);
        cx.failures.push("leaks".into());
    }
    cx.kill_fakes();
    println!();
    if cx.failures.is_empty() {
        println!("fake_lab: {} scenarios passed", ran);
        let _ = std::fs::remove_dir_all(&cx.tmp);
        0
    } else {
        println!("fake_lab: {} of {} scenarios failed (temp dir kept: {})", cx.failures.len(), ran, cx.tmp.display());
        1
    }
}
