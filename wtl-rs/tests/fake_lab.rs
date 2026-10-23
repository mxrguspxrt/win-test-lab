//! End-to-end test of `wtl-rs lab` without KVM or Windows.
//!
//! This one binary plays three parts:
//!   * the test driver (no args / cargo's args): builds a throwaway win-test-lab tree in a
//!     temp dir and runs `wtl-rs lab ...` against it, checking exit codes, messages and files;
//!   * a fake `qemu-system-x86_64` (argv has `-machine`): serves QMP (unix socket or TCP)
//!     and simulates guest/lab.ps1 by reading the control disc's LAB/lab.json and the mount
//!     discs, writing the WTL serial events a real guest sends;
//!   * a fake `qemu-img` (argv starts with `create`).
//!
//! Scripts on a mounted folder steer the fake guest: a file containing FAKE_EXIT=N exits N,
//! FAKE_LAB_FAIL makes the guest report LAB_FAIL, FAKE_HANG stops the guest, FAKE_CRASH
//! kills the VM.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use base64::Engine;
use serde_json::{json, Value};
use wtl_rs::mkiso::IsoReader;

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
    lan_server: Option<u16>,
}

struct Shared {
    status: String,
}

const NO_ARG: [&str; 3] = ["-enable-kvm", "-usb", "-no-shutdown"];

fn die(msg: &str) -> ! {
    eprintln!("qemu-system-x86_64: {}", msg);
    std::process::exit(1)
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
                    let idx: usize = d.get("index").and_then(|x| x.parse().ok()).unwrap_or(m.cds.len());
                    m.cds.insert(idx, PathBuf::from(file));
                } else if d.get("id").map(String::as_str) == Some("disk0") {
                    m.disk = Some(PathBuf::from(file));
                }
            }
            "-serial" => m.serial = v.strip_prefix("file:").map(PathBuf::from),
            "-qmp" => m.qmp = Some(split_opts(&v)[0].clone()),
            "-netdev" => {
                let d = kv(&v);
                if v.starts_with("stream,") && d.get("server").map(String::as_str) == Some("on") {
                    m.lan_server = d.get("addr.port").and_then(|p| p.parse().ok());
                }
            }
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
    // the DC's end of the lab LAN listens, like QEMU's stream netdev with server=on
    let _lan = m.lan_server.map(|p| std::net::TcpListener::bind(("127.0.0.1", p)).unwrap_or_else(|e| die(&format!("lan port {}: {}", p, e))));

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
                let data = if png_wanted { wtl_rs::qmp::ppm_to_png(&ppm).unwrap() } else { ppm };
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
        let cd = m.cds.get(&0).ok_or("no control disc")?;
        let iso = IsoReader::open(cd, true).map_err(|e| e.to_string())?;
        if iso.volid != "WTL_LAB" {
            return Err(format!("control disc label {} (want WTL_LAB)", iso.volid));
        }
        for f in ["LAB/lab.ps1", "LAB/labsvc.ps1"] {
            iso.read(f).ok_or(format!("{} missing on the control disc", f))?;
        }
        let cfg = iso.read("LAB/lab.json").ok_or("LAB/lab.json missing")?;
        if !cfg.is_ascii() {
            return Err("lab.json is not pure ASCII (PowerShell 4 would misread it)".into());
        }
        let cfg: Value = serde_json::from_slice(cfg).map_err(|e| e.to_string())?;
        if s(&cfg, "fmt") == "multi" {
            return self.run_multi(m, &cfg);
        }
        let role = s(&cfg, "role").to_string();
        let user = format!("{}%5C{}", s(&cfg, "netbios"), s(&cfg, "logon_user"));
        self.emit("WTL AGENT_START os=Microsoft%20Windows%20Server%202019%20(fake) build=17763");
        if role == "dc" {
            self.emit("WTL LAB_START role=dc phase=ready user=LAB%5CAdministrator");
            self.emit(&format!("WTL LAB_DC_READY domain={} | {}", s(&cfg, "domain"), self.name));
            return Ok(());
        }
        if role != "client" {
            return Err(format!("unknown role {}", role));
        }
        if cfg.get("shutdown_when_ready").and_then(Value::as_bool) == Some(true) {
            // the 0-state build: join, reboot, autologon, ready, idle
            self.emit("WTL LAB_START role=client phase=join user=CL%5CAdministrator");
            self.emit("WTL LAB_PHASE name=join");
            self.emit("WTL LAB_JOINED domain=lab.local");
            self.emit("WTL LAB_REBOOT | after the join");
            self.raw("garbage after reboot \u{0}\r\n");
            self.emit("WTL AGENT_START os=fake build=17763");
            self.emit(&format!("WTL LAB_START role=client phase=ready user={}", user));
            self.emit(&format!("WTL LAB_CLIENT_READY user={}", user));
            self.emit("WTL LAB_IDLE");
            return Ok(());
        }
        self.emit(&format!("WTL LAB_START role=client phase=ready user={}", user));
        self.emit(&format!("WTL LAB_CLIENT_READY user={}", user));
        let run = match cfg.get("run") {
            Some(Value::Null) | None => return Ok(()),
            Some(r) => r.clone(),
        };
        // mounts: each needs a disc labelled WTL<LETTER>
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
                return Err(format!("mount {}: no disc labelled WTL{}", letter, letter));
            }
            self.emit(&format!("WTL LAB_MOUNT letter={} host={}", letter, wtl_value(s(mnt, "host"))));
        }
        let (mut n, mut na, mut ns) = (0, 0, 0);
        let steps = run.get("steps").and_then(Value::as_array).cloned().unwrap_or_default();
        for st in steps {
            n += 1;
            let who = s(&st, "who").to_string();
            let path = s(&st, "path").to_string();
            if who == "admin" {
                na += 1;
            } else {
                ns += 1;
            }
            let script = wtl_value(&path);
            let letter = &path[..1];
            let content: Option<Vec<u8>> = if letter == "C" {
                Some(b"(a C: program)".to_vec())
            } else {
                discs.get(&format!("WTL{}", letter)).and_then(|d| d.read(&path[3..].replace('\\', "/")).map(<[u8]>::to_vec))
            };
            let Some(content) = content else {
                self.emit(&format!("WTL LAB_STEP as={} n={} script={} exit=launchfail | The system cannot find the file specified", who, n, script));
                continue;
            };
            let text = String::from_utf8_lossy(&content).into_owned();
            if text.contains("FAKE_HANG") {
                return Ok(());
            }
            if text.contains("FAKE_CRASH") {
                // well after the host's "did it start?" check
                std::thread::sleep(Duration::from_millis(1500));
                fake_exit(1);
            }
            if text.contains("FAKE_LAB_FAIL") {
                return Err(format!("step {} blew up (line 42)", n));
            }
            let code = text
                .split("FAKE_EXIT=")
                .nth(1)
                .map(|r| r.chars().take_while(|c| c.is_ascii_digit() || *c == '-').collect::<String>())
                .unwrap_or_else(|| "0".into());
            let out = format!("ran {} as {}\r\n{}", path, who, text).into_bytes();
            for (seq, chunk) in out.chunks(768).enumerate() {
                self.emit(&format!("WTL OUTPUT stream=step{}out seq={} | {}", n, seq, b64encode(chunk)));
            }
            self.emit(&format!("WTL OUTPUT_END stream=step{}out bytes={} sent={} truncated=0", n, out.len(), out.len()));
            let err = format!("stderr of step {}", n);
            self.emit(&format!("WTL OUTPUT stream=step{}err seq=0 | {}", n, b64encode(err.as_bytes())));
            self.emit(&format!("WTL OUTPUT_END stream=step{}err bytes={} sent={} truncated=0", n, err.len(), err.len()));
            self.emit(&format!("WTL LAB_STEP as={} n={} script={} exit={} secs=0.4", who, n, script, code));
        }
        self.emit(&format!("WTL LAB_RUN_DONE admin={} student={}", na, ns));
        Ok(())
    }

    /// Simulate a multi-machine (fmt=multi) guest: DC promotes, a member joins/logs on, then
    /// each machine with a run runs its steps and reports LAB_STEP machine=.. .
    fn run_multi(&mut self, m: &Machine, cfg: &Value) -> Result<(), String> {
        let role = s(cfg, "role").to_string();
        let machine = s(cfg, "machine").to_string();
        let user = format!("{}%5C{}", s(cfg, "netbios"), s(cfg, "logon_user"));
        self.emit(&format!("WTL AGENT_START os=fake build=17763 machine={}", machine));
        if role == "dc" {
            let is_run = cfg.get("run").map(|r| !r.is_null()).unwrap_or(false);
            if !is_run {
                // 0-state build: rename to the machine key + reboot before promotion (the host's
                // wait_ev must ride through this extra reboot), then promote and report ready
                self.emit(&format!("WTL LAB_START role=dc machine={} phase=new user=DC2019%5CAdministrator", machine));
                self.emit(&format!("WTL LAB_PHASE | renaming this computer DC2019 -> {}, then reboot", machine));
                self.emit("WTL LAB_REBOOT phase=rename");
                self.raw("garbage after reboot \u{0}\r\n");
                self.emit(&format!("WTL AGENT_START os=fake build=17763 machine={}", machine));
            }
            self.emit(&format!("WTL LAB_START role=dc machine={} phase=ready user=LAB%5CAdministrator", machine));
            self.emit(&format!("WTL LAB_DC_READY domain={} dc={} computer={} | {}", s(cfg, "domain"), machine, machine, self.name));
            if is_run {
                return self.run_multi_steps(m, cfg, &machine);
            }
            return Ok(());
        }
        if role != "member" {
            return Err(format!("unknown role {}", role));
        }
        if cfg.get("shutdown_when_ready").and_then(Value::as_bool) == Some(true) {
            // 0-state build: rename+join in one reboot, autologon, ready, idle
            self.emit(&format!("WTL LAB_START role=member machine={} phase=join user={}%5CAdministrator", machine, s(cfg, "netbios")));
            self.emit(&format!("WTL LAB_JOINED domain={} machine={}", s(cfg, "domain"), machine));
            self.emit("WTL LAB_REBOOT | after the join (renamed)");
            self.raw("garbage after reboot \u{0}\r\n");
            self.emit(&format!("WTL AGENT_START os=fake build=17763 machine={}", machine));
            self.emit(&format!("WTL LAB_START role=member machine={} phase=ready user={}", machine, user));
            self.emit(&format!("WTL LAB_CLIENT_READY user={} machine={} computer={}", user, machine, machine));
            self.emit("WTL LAB_IDLE");
            return Ok(());
        }
        self.emit(&format!("WTL LAB_START role=member machine={} phase=ready user={}", machine, user));
        self.emit(&format!("WTL LAB_CLIENT_READY user={} machine={} computer={}", user, machine, machine));
        if cfg.get("run").map(|r| !r.is_null()).unwrap_or(false) {
            return self.run_multi_steps(m, cfg, &machine);
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
                self.emit(&format!("WTL LAB_STEP machine={} as={} n={} script={} exit=launchfail | The system cannot find the file specified", machine, who, n, script));
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
        self.emit(&format!("WTL LAB_RUN_DONE machine={} steps={}", machine, run.get("steps").and_then(Value::as_array).map(|a| a.len()).unwrap_or(0)));
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
        let tmp = std::env::temp_dir().join(format!("wtl-rs-fakelab-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let tmp = std::fs::canonicalize(&tmp).unwrap(); // paths below are compared as text
        let root = tmp.join("lab");
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        for d in ["lib", "guest", "labs", "images"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        for d in ["app/sub dir", "cfg", "pids"] {
            std::fs::create_dir_all(tmp.join(d)).unwrap();
        }
        for f in ["lib/oslist.conf", "guest/lab.ps1", "guest/labsvc.ps1"] {
            std::fs::copy(src.join(f), root.join(f)).unwrap();
        }
        // lab definitions in the same bash style as labs/*.conf, on free ports
        let def = |name: &str, dc_os: &str, cl_os: &str| {
            format!(
                "# AD lab \"{name}\" for the fake-QEMU test\n\
                 LAB_OS={dc_os}              # DC operating system (key from lib/oslist.conf)\n\
                 LAB_CLIENT_OS={cl_os}\n\n\
                 LAB_DOMAIN=lab.local\nLAB_NETBIOS=LAB\nLAB_DC_NAME=DC{name}\nLAB_CLIENT_NAME=\"CL${{LAB_NETBIOS}}\"\n\
                 LAB_NET=10.77.99\nLAB_PREFIX=24\nLAB_LAN_PORT={port}   # host loopback port\n\n\
                 LAB_ADMIN_USER=labadmin\nLAB_ADMIN_USER_PASSWORD='DomAdm!2026'\nLAB_USER=student\nLAB_USER_PASSWORD='Pupil!2026'\n\
                 LAB_DC_MEM=3072\nLAB_CLIENT_MEM=4096\n",
                port = free_port()
            )
        };
        std::fs::write(root.join("labs/t.conf"), def("t", "2019", "2019")).unwrap();
        std::fs::write(root.join("labs/u.conf"), def("u", "ws2019", "win11")).unwrap();
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
        // build-image.sh (here a fake that writes the image and logs its arguments)
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
        let fake_build = "#!/bin/sh\n\
             # fake build-image.sh: OS --name NAME --computer-name CN [--no-updates] [--show]\n\
             echo \"$*\" >> \"$WTL_IMAGES_DIR/build-calls.log\"\n\
             echo \"installing $1 into $3.qcow2\"\n\
             printf 'QFI\\373 fake base image\\n' > \"$WTL_IMAGES_DIR/$3.qcow2\"\n";
        std::fs::write(root.join("build-image.sh"), fake_build).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(root.join("build-image.sh"), std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        for img in ["t-dc", "t-client", "u-dc", "u-client", "2019-dc", "2019-client", "win11-dc", "win11-client"] {
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

    /// Run `wtl-rs lab ARGS` with the fake QEMU; stdout+stderr combined.
    fn lab(&self, args: &[&str], env: &[(&str, &str)]) -> Out {
        let me = std::env::current_exe().unwrap();
        let outf = self.tmp.join("out.txt");
        let f = std::fs::File::create(&outf).unwrap();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_wtl-rs"));
        cmd.arg("lab").args(args).current_dir(self.tmp.join("cfg"));
        for (k, _) in std::env::vars_os() {
            let ks = k.to_string_lossy();
            if ks.starts_with("WTL_") || ks.starts_with("FAKE_") {
                cmd.env_remove(&k);
            }
        }
        cmd.env("WTL_ROOT", &self.root)
            .env("WTL_IMAGES_DIR", self.root.join("images"))
            .env("WTL_QEMU", &me)
            .env("WTL_QEMU_IMG", &me)
            .env("WTL_NO_KVM", "1")
            .env("WTL_TEST_FAST", "1")
            .env("WTL_OVMF_CODE", self.tmp.join("OVMF_CODE.fd"))
            .env("WTL_OVMF_VARS", self.tmp.join("OVMF_VARS.fd"))
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
            let alive = |p: &str| p.parse().is_ok_and(wtl_rs::sys::pid_alive);
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
                wtl_rs::sys::kill_hard(pid);
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
    // a scenario gets the test tree, a checker, and the outputs of the `lab` runs it makes
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
        outs.push((format!("lab {}", args.join(" ")), cx.lab(args, env)));
        outs.len() - 1
    };

    scenario(&mut cx, "usage + help", &|cx, c, outs| {
        let i = run(cx, outs, &["run", "2019"], &[]);
        c.code(&outs[i].1, 2);
        c.has(&outs[i].1, "usage: lab run CONFIG [CONFIG...]");
        let i = run(cx, outs, &["help-config"], &[]);
        c.code(&outs[i].1, 0);
        c.has(&outs[i].1, "run_as_student = Z:\\run.bat");
        let i = run(cx, outs, &["--help"], &[]);
        c.code(&outs[i].1, 0);
        c.has(&outs[i].1, "lab restore|up|stop|status|prepare|rebuild LAB|CONFIG");
        c.has(&outs[i].1, "Exit: 0 all steps passed, 1 a step failed, 2 lab error, 3 timeout.");
        let i = run(cx, outs, &["status"], &[]);
        c.code(&outs[i].1, 2);
        let i = run(cx, outs, &["status", "nope"], &[]);
        c.code(&outs[i].1, 2);
        c.has(&outs[i].1, "no lab config");
        c.has(&outs[i].1, "(labs: t u )");
        // usage errors: clap's wording, lab.sh's exit code 2
        let i = run(cx, outs, &["frobnicate", "t"], &[]);
        c.code(&outs[i].1, 2);
        c.has(&outs[i].1, "unrecognized subcommand 'frobnicate'");
        let i = run(cx, outs, &["status", "t", "--bogus"], &[]);
        c.code(&outs[i].1, 2);
        c.has(&outs[i].1, "unexpected argument '--bogus'");
        let i = run(cx, outs, &["status", "t", "--wait", "0"], &[]);
        c.code(&outs[i].1, 2);
        c.has(&outs[i].1, "invalid value '0' for '--wait <S>': must be a positive integer");
        let i = run(cx, outs, &["status", "t", "u"], &[]);
        c.code(&outs[i].1, 2);
        c.has(&outs[i].1, "ERROR: unknown option u");
    });

    scenario(&mut cx, "config errors are reported before anything starts", &|cx, c, outs| {
        let bad = cx.cfg("bad.conf", "version = t\nfoo = 1\n");
        let i = run(cx, outs, &["run", &bad], &[]);
        c.code(&outs[i].1, 2);
        c.has(&outs[i].1, "error: ");
        c.has(&outs[i].1, ":2: unknown key 'foo'");
        c.has(&outs[i].1, "config error (see above)");
        let other = cx.cfg("other.conf", "version = 2099\nrun_as_admin = C:\\x.bat\n");
        let i = run(cx, outs, &["run", &other], &[]);
        c.code(&outs[i].1, 2);
        c.has(&outs[i].1, "no lab config");
        c.ok(cx.live_fakes().is_empty(), "a VM was started");
    });

    scenario(&mut cx, "status before the 0 state exists", &|cx, c, outs| {
        let i = run(cx, outs, &["status", "t"], &[]);
        c.code(&outs[i].1, 0);
        c.has(&outs[i].1, "lab t (");
        c.has(&outs[i].1, "domain lab.local, DC DCt 10.77.99.10, client CLLAB 10.77.99.20");
        c.has(&outs[i].1, "0 state: not built yet (lab prepare t, or just lab run ...)");
        c.has(&outs[i].1, "  dc: off");
        c.has(&outs[i].1, "disk  t-dc.qcow2");
    });

    scenario(&mut cx, "first run builds the 0 state, then runs the steps", &|cx, c, outs| {
        let conf = cx.cfg(
            "smoke.conf",
            "# like examples/configs/2019-smoke.conf\nversion = t\nmount   = ../app:X\nrun_as_admin   = X:\\setup.bat\nrun_as_student = X:\\smoke.exe\nname    = smoke\n",
        );
        let i = run(cx, outs, &["run", &conf], &[]);
        let o = &outs[i].1;
        c.code(o, 0);
        for s in [
            "=== building the 0 state of lab t (domain lab.local; once, ~15-25 min) ===",
            "DC ready: WTL LAB_DC_READY domain=lab.local",
            "client ready: WTL LAB_CLIENT_READY user=LAB%5Cstudent",
            "shutting the client down cleanly",
            "0 state saved:",
            "lab t: booting from the 0 state",
            "mount X: <- ",
            "client ready: LAB\\student logged on",
            "running config smoke.conf (mounts + steps)",
            "[admin] X:\\setup.bat -> exit 0",
            "[student] X:\\smoke.exe -> exit 0",
            "all steps done",
            "  [guest] WTL LAB_MOUNT letter=X",
            "    DC      DCt  10.77.99.10  VNC 127.0.0.1:59",
            "LAB\\Administrator / WinTest!2026",
            "  config: smoke.conf  ->  ALL STEPS OK",
        ] {
            c.has(o, s);
        }
        c.lacks(o, "[guest] WTL OUTPUT ");
        let st = cx.state("t");
        c.file(&st.join("0-state.json"));
        c.content(&st.join("dc-0.qcow2"), format!("QFI\u{fb} fake qcow2 backing={}\n", cx.root.join("images/t-dc.qcow2").display()).as_bytes());
        c.content(&st.join("client.qcow2"), format!("QFI\u{fb} fake qcow2 backing={}\n", st.join("client-0.qcow2").display()).as_bytes());
        c.no_file(&st.join("dc-prep.iso"));
        c.no_file(&st.join("cd-client"));
        let last = std::fs::read_link(st.join("last")).unwrap_or_default();
        c.ok(last.to_string_lossy().starts_with("runs/20"), format!("last -> {}", last.display()));
        let res = st.join(&last);
        c.content(&res.join("step1out.txt"), b"ran X:\\setup.bat as admin\r\n@echo off\r\necho installing\r\nFAKE_EXIT=0\r\n");
        c.content(&res.join("step2out.txt"), b"ran X:\\smoke.exe as student\r\nMZ fake smoke.exe\r\n");
        c.content(&res.join("step2err.txt"), b"stderr of step 2");
        c.content(&res.join("stdout.txt"), b"");
        c.content(&res.join("config.conf"), std::fs::read(&conf).unwrap().as_slice());
        c.ok(std::fs::read(res.join("screen.png")).unwrap_or_default().starts_with(b"\x89PNG"), "screen.png is not a PNG");
        c.ok(std::fs::read_to_string(res.join("client.serial.log")).unwrap_or_default().contains("WTL LAB_RUN_DONE admin=1 student=1"), "serial log copy");
        // the control disc the client got: lab.json with the run, in file order
        match IsoReader::open(&st.join("client.iso"), true) {
            Ok(iso) => {
                let j: Value = serde_json::from_slice(iso.read("LAB/lab.json").unwrap_or_default()).unwrap();
                let app = cx.tmp.join("app").to_string_lossy().into_owned();
                let want = json!({
                    "role": "client", "ip": "10.77.99.20", "prefix": 24, "dc_ip": "10.77.99.10",
                    "lan_mac": "52:54:00:4c:41:20", "domain": "lab.local", "netbios": "LAB",
                    "admin_password": "WinTest!2026",
                    "users": [{"name": "student", "password": "Pupil!2026", "groups": []},
                              {"name": "labadmin", "password": "DomAdm!2026", "groups": ["Domain Admins"]}],
                    "logon_user": "student", "logon_password": "Pupil!2026", "shutdown_when_ready": false,
                    "run": {"mounts": [{"host": app, "letter": "X"}],
                            "steps": [{"who": "admin", "path": "X:\\setup.bat"}, {"who": "student", "path": "X:\\smoke.exe"}],
                            "wait": 60}
                });
                c.ok(j == want, format!("lab.json {}", j));
            }
            Err(e) => c.ok(false, format!("client.iso: {}", e)),
        }
        match IsoReader::open(&st.join("mount-X.iso"), true) {
            Ok(iso) => {
                c.ok(iso.volid == "WTLX", format!("mount disc label {}", iso.volid));
                c.ok(iso.read("sub dir/Long Name Script.ps1") == Some(b"Write-Host hi\r\n".as_slice()), "long name on the mount disc");
            }
            Err(e) => c.ok(false, format!("mount-X.iso: {}", e)),
        }
        // the VMs keep running after a run
        c.ok(cx.live_fakes().len() == 2, format!("running VMs: {:?}", cx.live_fakes()));
        let argv = std::fs::read_to_string(&cx.fake_log).unwrap_or_default();
        let dc_line = argv.lines().rfind(|l| l.contains("lab-t-dc")).unwrap_or("").replace('\u{1f}', " ");
        let cl_line = argv.lines().rfind(|l| l.contains("lab-t-client")).unwrap_or("").replace('\u{1f}', " ");
        c.ok(!dc_line.contains("user,id=net0") && dc_line.contains("server=on"), format!("dc argv: {}", dc_line));
        c.ok(cl_line.contains("user,id=net0") && cl_line.contains("server=off,reconnect-ms=1000"), format!("client argv: {}", cl_line));
        c.ok(cl_line.contains("mount-X.iso,media=cdrom,if=ide,index=1,readonly=on"), "client mount cd");
        c.ok(cl_line.contains("-cpu max") && !cl_line.contains("-enable-kvm") && cl_line.ends_with("-no-shutdown"), "tcg + -no-shutdown");
        c.file(&st.join("client.qemu.log.cmdline"));
        let i = run(cx, outs, &["status", &conf], &[]);
        c.code(&outs[i].1, 0);
        c.has(&outs[i].1, "  0 state: ready (20");
        c.has(&outs[i].1, "  dc: running (pid ");
        c.has(&outs[i].1, "  client: running (pid ");
        c.has(&outs[i].1, "  last run: ");
    });

    scenario(&mut cx, "steps run in file order; a failing step -> exit 1", &|cx, c, outs| {
        let conf = cx.cfg(
            "order.conf",
            "version=t\nmount=../app:x\nrun_as_admin=X:\\setup.bat\nrun_as_student=x:/fail.bat, X:\\sub dir\\Long Name Script.ps1\n\
             run_as_admin=X:\\missing.exe\nrun_as_admin=C:\\Windows\\System32\\whoami.exe\nwait=5\n",
        );
        let i = run(cx, outs, &["run", &conf, "--wait", "7"], &[]);
        let o = &outs[i].1;
        c.code(o, 1);
        c.lacks(o, "building the 0 state");
        c.has(o, "[admin] X:\\setup.bat -> exit 0");
        c.has(o, "ERROR: [student] X:\\fail.bat -> exit 3");
        c.has(o, "[student] X:\\sub dir\\Long Name Script.ps1 -> exit 0");
        c.has(o, "ERROR: [admin] X:\\missing.exe -> exit launchfail");
        c.has(o, "[admin] C:\\Windows\\System32\\whoami.exe -> exit 0");
        c.has(o, "->  A STEP FAILED");
        let pos = |s: &str| o.text.find(s).unwrap_or(usize::MAX);
        c.ok(
            pos("X:\\setup.bat ->") < pos("X:\\fail.bat ->") && pos("X:\\fail.bat ->") < pos("Script.ps1 ->") && pos("Script.ps1 ->") < pos("missing.exe ->"),
            "steps out of order",
        );
        let st = cx.state("t");
        let res = std::fs::canonicalize(st.join("last")).unwrap_or_default();
        c.content(&res.join("step3out.txt"), b"ran X:\\sub dir\\Long Name Script.ps1 as student\r\nWrite-Host hi\r\n");
        let iso = IsoReader::open(&st.join("client.iso"), true).unwrap();
        let j: Value = serde_json::from_slice(iso.read("LAB/lab.json").unwrap()).unwrap();
        c.ok(j["run"]["wait"] == json!(7), "--wait did not override the config");
    });

    scenario(&mut cx, "layered configs, QMP over TCP (the Windows path)", &|cx, c, outs| {
        let work = cx.cfg("work.conf", "mount = ../app:Y\nrun_as_student = Y:\\smoke.exe\nname = layered\n");
        let os = cx.cfg("os.conf", "version = t\n");
        let i = run(cx, outs, &["run", &work, &os], &[("WTL_QMP", "tcp")]);
        let o = &outs[i].1;
        c.code(o, 0);
        c.has(o, "running config work.conf");
        c.has(o, "[student] Y:\\smoke.exe -> exit 0");
        let st = cx.state("t");
        let addr = std::fs::read_to_string(st.join("client.qmp.addr")).unwrap_or_default();
        c.ok(addr.starts_with("tcp:127.0.0.1:"), format!("client.qmp.addr {:?}", addr));
        let res = std::fs::canonicalize(st.join("last")).unwrap_or_default();
        c.file(&res.join("screen.png"));
        // status/stop/restore find a TCP monitor without being told again
        let i = run(cx, outs, &["status", "t"], &[]);
        c.has(&outs[i].1, "  client: running");
    });

    scenario(&mut cx, "guest reports LAB_FAIL -> exit 2 + screenshot (old QEMU: PPM)", &|cx, c, outs| {
        let conf = cx.cfg("labfail.conf", "version=t\nmount=../app:X\nrun_as_admin=X:\\labfail.bat\n");
        let st = cx.state("t");
        let _ = std::fs::remove_file(st.join("client-fail.png"));
        let i = run(cx, outs, &["run", &conf], &[("FAKE_OLD_QEMU", "1")]);
        let o = &outs[i].1;
        c.code(o, 2);
        c.has(o, "ERROR: config run: client reported: WTL LAB_FAIL | fake guest: step 1 blew up (line 42)");
        c.has(o, "the VMs keep running - lab restore t");
        c.has(o, "screenshot: ");
        c.ok(std::fs::read(st.join("client-fail.png")).unwrap_or_default().starts_with(b"\x89PNG"), "client-fail.png is not a PNG");
    });

    scenario(&mut cx, "guest hangs -> timeout exit 3", &|cx, c, outs| {
        let conf = cx.cfg("hang.conf", "version=t\nmount=../app:X\nrun_as_student=X:\\hang.bat\ntimeout_min=50\n");
        let i = run(cx, outs, &["run", &conf, "--timeout-min", "0"], &[]);
        let o = &outs[i].1;
        c.code(o, 3);
        c.has(o, "ERROR: config run: timeout (--timeout-min 0)");
    });

    scenario(&mut cx, "VM dies mid-run -> exit 2", &|cx, c, outs| {
        let conf = cx.cfg("crash.conf", "version=t\nmount=../app:X\nrun_as_admin=X:\\crash.bat\n");
        let i = run(cx, outs, &["run", &conf], &[]);
        let o = &outs[i].1;
        c.code(o, 2);
        c.has(o, "ERROR: config run: the client VM exited");
    });

    scenario(&mut cx, "up, stop (soft), restore", &|cx, c, outs| {
        let st = cx.state("t");
        let i = run(cx, outs, &["up", "t"], &[]);
        let o = &outs[i].1;
        c.code(o, 0);
        c.has(o, "client ready: LAB\\student logged on");
        c.has(o, "  lab t: domain lab.local");
        c.has(o, "back to the 0 state: lab restore t");
        c.lacks(o, "running config");
        let iso = IsoReader::open(&st.join("client.iso"), true).unwrap();
        let j: Value = serde_json::from_slice(iso.read("LAB/lab.json").unwrap()).unwrap();
        c.ok(j.get("run") == Some(&Value::Null), "up must not carry a run");
        c.ok(!st.join("mount-X.iso").exists(), "old mount discs are removed");
        c.ok(cx.live_fakes().len() == 2, format!("running VMs: {:?}", cx.live_fakes()));
        let i = run(cx, outs, &["stop", "t"], &[]);
        c.code(&outs[i].1, 0);
        c.has(&outs[i].1, "lab t is off");
        c.ok(cx.live_fakes().is_empty(), format!("still running after stop: {:?}", cx.live_fakes()));
        c.file(&st.join("client.qcow2")); // stop keeps the changes
        c.no_file(&st.join("client.pid"));
        let i = run(cx, outs, &["up", "t"], &[]);
        c.code(&outs[i].1, 0);
        let i = run(cx, outs, &["restore", "t"], &[]);
        c.code(&outs[i].1, 0);
        c.has(&outs[i].1, "lab t is back at its 0 state");
        c.ok(cx.live_fakes().is_empty(), format!("still running after restore: {:?}", cx.live_fakes()));
        c.no_file(&st.join("client.qcow2"));
        c.no_file(&st.join("dc.qcow2"));
        c.file(&st.join("client-0.qcow2"));
    });

    scenario(&mut cx, "UEFI client (win11): q35 + OVMF vars kept in the state", &|cx, c, outs| {
        let i = run(cx, outs, &["up", "u"], &[]);
        c.code(&outs[i].1, 0);
        c.has(&outs[i].1, "=== building the 0 state of lab u");
        let st = cx.state("u");
        c.content(&st.join("client-OVMF_VARS.fd"), b"vars");
        c.no_file(&st.join("dc-OVMF_VARS.fd"));
        let argv = std::fs::read_to_string(&cx.fake_log).unwrap_or_default();
        let cl = argv.lines().rfind(|l| l.contains("lab-u-client")).unwrap_or("").replace('\u{1f}', " ");
        let dc = argv.lines().rfind(|l| l.contains("lab-u-dc")).unwrap_or("").replace('\u{1f}', " ");
        c.ok(cl.contains("-machine q35") && cl.contains("if=pflash,format=raw,readonly=on,file=") && cl.contains("client-OVMF_VARS.fd"), format!("client argv: {}", cl));
        c.ok(dc.contains("-machine pc") && !dc.contains("pflash"), format!("dc argv: {}", dc));
        let i = run(cx, outs, &["restore", "u"], &[]);
        c.code(&outs[i].1, 0);
    });

    scenario(&mut cx, "rebuild deletes the 0 state; prepare rebuilds it", &|cx, c, outs| {
        let st = cx.state("t");
        let i = run(cx, outs, &["rebuild", "t"], &[]);
        c.code(&outs[i].1, 0);
        c.has(&outs[i].1, "0 state deleted");
        c.no_file(&st.join("0-state.json"));
        c.no_file(&st.join("dc-0.qcow2"));
        let i = run(cx, outs, &["prepare", "t"], &[]);
        c.code(&outs[i].1, 0);
        c.file(&st.join("0-state.json"));
        c.ok(cx.live_fakes().is_empty(), format!("prepare left VMs running: {:?}", cx.live_fakes()));
    });

    scenario(&mut cx, "invoked as `lab` through a symlink", &|cx, c, outs| {
        std::fs::create_dir_all(cx.tmp.join("bin")).unwrap();
        let link = cx.tmp.join("bin/lab");
        let _ = std::fs::remove_file(&link);
        #[cfg(unix)]
        std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_wtl-rs"), &link).unwrap();
        #[cfg(not(unix))]
        std::fs::copy(env!("CARGO_BIN_EXE_wtl-rs"), &link).unwrap();
        let o = Command::new(&link).arg("status").arg("t").env("WTL_ROOT", &cx.root).output().unwrap();
        let text = String::from_utf8_lossy(&o.stdout).into_owned() + &String::from_utf8_lossy(&o.stderr);
        c.ok(o.status.code() == Some(0), format!("exit {:?}", o.status.code()));
        outs.push(("lab status t".into(), Out { code: o.status.code().unwrap_or(-1), text: text.clone() }));
        c.ok(text.contains("0 state: ready"), "status via the symlink");
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
        c.ok(find("CL1", 4)["user"] == json!("user") && find("CL1", 4)["script"] == json!("X:\\smoke.exe"), format!("CL1 step4 {}", find("CL1", 4)));
        c.ok(find("DC", 1)["machine"] == json!("DC") && find("DC", 1)["exit"] == json!("0"), format!("DC step1 {}", find("DC", 1)));
        // the DC's control disc: fmt=multi, role=dc, and only its own step (whoami on C:)
        let dc_iso = IsoReader::open(&st.join("DC.iso"), true).unwrap();
        let dj: Value = serde_json::from_slice(dc_iso.read("LAB/lab.json").unwrap()).unwrap();
        c.ok(s(&dj, "fmt") == "multi" && s(&dj, "role") == "dc" && s(&dj, "ip") == "10.77.98.10", format!("DC lab.json {}", dj));
        c.ok(dj["run"]["steps"].as_array().map(|a| a.len()) == Some(1), format!("DC should have 1 step: {}", dj["run"]));
        c.ok(dj["run"]["mounts"].as_array().map(|a| a.len()) == Some(0), "DC mounts only C: -> no mount disc");
        // CL1's disc: 3 steps (setup, smoke student1, smoke logon), one mount X
        let cl1_iso = IsoReader::open(&st.join("CL1.iso"), true).unwrap();
        let cj: Value = serde_json::from_slice(cl1_iso.read("LAB/lab.json").unwrap()).unwrap();
        c.ok(s(&cj, "role") == "member" && s(&cj, "logon_user") == "student1", format!("CL1 lab.json {}", cj));
        c.ok(cj["run"]["steps"].as_array().map(|a| a.len()) == Some(4), format!("CL1 steps {}", cj["run"]["steps"]));
        // the student1 step is its own logon user -> as_=logon; teacher is another user -> as_=user, with creds
        let step = |st: &Value, n: i64| st["run"]["steps"].as_array().unwrap().iter().find(|x| x["n"] == json!(n)).cloned().unwrap_or(Value::Null);
        let as_of = |st: &Value, n: i64| step(st, n)["as_"].as_str().unwrap_or("").to_string();
        c.ok(as_of(&cj, 1) == "system", format!("CL1 step1 as_ {}", as_of(&cj, 1)));
        c.ok(as_of(&cj, 2) == "logon", format!("CL1 step2 as_ {}", as_of(&cj, 2)));
        c.ok(as_of(&cj, 4) == "user" && step(&cj, 4)["user"] == json!("teacher") && step(&cj, 4)["password"] == json!("Teach!2026"),
            format!("CL1 step4 (teacher) {}", step(&cj, 4)));
        // multicast LAN argv + per-VM cpus; CL2 is UEFI/q35
        let argv = std::fs::read_to_string(&cx.fake_log).unwrap_or_default();
        let line = |tag: &str| argv.lines().rfind(|l| l.contains(tag)).unwrap_or("").replace('\u{1f}', " ");
        let dc_line = line("lab-tri-DC");
        c.ok(dc_line.contains("-netdev dgram,id=lan0,remote.type=inet,remote.host=239.") && dc_line.contains("local.port="), format!("dc mcast argv: {}", dc_line));
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
        c.lacks(o, "building the 0 state"); // already built by the previous scenario? no, tri was restored but kept its 0 state
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
            lines == ["2016 --name 2016-client --computer-name CL2016 --no-updates", "2016 --name 2016-dc --computer-name DC2016 --no-updates"],
            format!("build-image.sh calls: {:?}", lines),
        );
        c.file(&cx.state("fresh").join("build-2016-dc.log"));
        // already there: a second prepare after a rebuild does not install again
        let _ = run(cx, outs, &["rebuild", "fresh"], &[]);
        let i = run(cx, outs, &["prepare", "fresh"], &[]);
        c.code(&outs[i].1, 0);
        c.lacks(&outs[i].1, "installing Windows");
        let _ = run(cx, outs, &["restore", "fresh"], &[]);
    });

    scenario(&mut cx, "JSON lab: unknown field is rejected before boot", &|cx, c, outs| {
        let bad = cx.cfg("unk.json", "{ \"lab\": \"tri\", \"frobnicate\": 1, \"steps\": [] }");
        let i = run(cx, outs, &["run", &bad], &[]);
        c.code(&outs[i].1, 2);
        c.has(&outs[i].1, "unknown field");
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
