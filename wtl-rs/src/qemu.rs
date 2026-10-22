//! Host settings (the WTL_* variables of lib/common.sh) and the QEMU command line
//! (port of qemu_cmd / start_qemu).

use crate::qmp::Addr;
use crate::util::which;
use anyhow::{anyhow, bail, ensure, Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Hardware acceleration backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Accel {
    /// Linux
    Kvm,
    /// Windows (Windows Hypervisor Platform)
    Whpx,
    /// macOS (Hypervisor.framework; Intel Macs only for x86 guests)
    Hvf,
    /// no acceleration (very slow; what the tests use)
    Tcg,
}

impl Accel {
    fn parse(s: &str) -> Option<Accel> {
        Some(match s.to_ascii_lowercase().as_str() {
            "kvm" => Accel::Kvm,
            "whpx" => Accel::Whpx,
            "hvf" => Accel::Hvf,
            "tcg" | "none" => Accel::Tcg,
            _ => return None,
        })
    }

    pub fn host_default() -> Accel {
        if cfg!(target_os = "linux") {
            Accel::Kvm
        } else if cfg!(windows) {
            Accel::Whpx
        } else if cfg!(target_os = "macos") {
            Accel::Hvf
        } else {
            Accel::Tcg
        }
    }
}

/// Settings from the environment (+ a lab definition's own WTL_* lines), with the same
/// defaults as lib/common.sh.
#[derive(Clone, Debug)]
pub struct Env {
    pub root: PathBuf,
    pub iso_dir: PathBuf,
    pub images_dir: PathBuf,
    pub qemu: String,
    pub qemu_img: String,
    pub accel: Accel,
    /// WTL_CPU: overrides the -cpu value
    pub cpu: Option<String>,
    pub cpus: String,
    pub password: String,
    pub nic: String,
    pub ovmf_code: PathBuf,
    pub ovmf_vars: PathBuf,
    /// display used for --show (gtk; cocoa on macOS); WTL_DISPLAY overrides
    pub display: String,
    /// WTL_TEST_FAST=1: skip the fixed settle sleeps (for the fake-QEMU tests)
    pub fast: bool,
}

/// The WTL_* defaults, as `NAME -> value` pairs, given ROOT.
pub fn defaults(root: &Path) -> Vec<(&'static str, String)> {
    let r = |p: &str| root.join(p).to_string_lossy().into_owned();
    vec![
        ("WTL_ISO_DIR", r("iso-files")),
        ("WTL_IMAGES_DIR", r("images")),
        ("WTL_RUNS_DIR", r("runs")),
        ("WTL_RESULTS_DIR", r("results")),
        ("WTL_QEMU", if cfg!(windows) { "qemu-system-x86_64.exe" } else { "qemu-system-x86_64" }.into()),
        ("WTL_QEMU_IMG", if cfg!(windows) { "qemu-img.exe" } else { "qemu-img" }.into()),
        ("WTL_NO_KVM", String::new()),
        ("WTL_MEM", "4096".into()),
        ("WTL_CPUS", "2".into()),
        ("WTL_PASSWORD", "WinTest!2026".into()),
        ("WTL_NIC", "e1000".into()),
        ("WTL_OVMF_CODE", "/usr/share/OVMF/OVMF_CODE_4M.fd".into()),
        ("WTL_OVMF_VARS", "/usr/share/OVMF/OVMF_VARS_4M.fd".into()),
    ]
}

/// Seed a variable map: every WTL_* default, overridden by the process environment.
pub fn seed_vars(root: &Path) -> HashMap<String, String> {
    let mut m = HashMap::new();
    m.insert("ROOT".to_string(), root.to_string_lossy().into_owned());
    m.insert("WTL_LIB".to_string(), root.join("lib").to_string_lossy().into_owned());
    for (k, v) in defaults(root) {
        let v = std::env::var(k).ok().filter(|x| !x.is_empty() || k == "WTL_NO_KVM").unwrap_or(v);
        m.insert(k.to_string(), v);
    }
    m
}

impl Env {
    /// Build from a variable map made by [`seed_vars`] (+ whatever a lab file set).
    pub fn from_vars(root: &Path, vars: &HashMap<String, String>) -> Result<Env> {
        let get = |k: &str| -> String {
            vars.get(k).cloned().or_else(|| std::env::var(k).ok()).unwrap_or_default()
        };
        let nonempty = |k: &str| Some(get(k)).filter(|v| !v.is_empty());
        let accel = match nonempty("WTL_ACCEL") {
            Some(a) => Accel::parse(&a).ok_or_else(|| anyhow!("WTL_ACCEL={} is not one of kvm, whpx, hvf, tcg", a))?,
            None if get("WTL_NO_KVM") == "1" => Accel::Tcg,
            None => Accel::host_default(),
        };
        let display = nonempty("WTL_DISPLAY").unwrap_or_else(|| if cfg!(target_os = "macos") { "cocoa" } else { "gtk" }.into());
        Ok(Env {
            root: root.to_path_buf(),
            iso_dir: PathBuf::from(get("WTL_ISO_DIR")),
            images_dir: PathBuf::from(get("WTL_IMAGES_DIR")),
            qemu: get("WTL_QEMU"),
            qemu_img: get("WTL_QEMU_IMG"),
            accel,
            cpu: nonempty("WTL_CPU"),
            cpus: get("WTL_CPUS"),
            password: get("WTL_PASSWORD"),
            nic: nonempty("WTL_NIC").unwrap_or_else(|| "e1000".into()),
            ovmf_code: PathBuf::from(get("WTL_OVMF_CODE")),
            ovmf_vars: PathBuf::from(get("WTL_OVMF_VARS")),
            display,
            fast: get("WTL_TEST_FAST") == "1",
        })
    }

    /// check_tools: qemu + qemu-img runnable, the accelerator usable.
    pub fn check_tools(&self) -> Result<()> {
        for t in [&self.qemu, &self.qemu_img] {
            ensure!(which(t).is_some(), "'{}' not found (run ./setup-host.sh or set WTL_QEMU / WTL_QEMU_IMG)", t);
        }
        if self.accel == Accel::Kvm && std::fs::OpenOptions::new().read(true).write(true).open("/dev/kvm").is_err() {
            bail!("/dev/kvm missing or not writable (run ./setup-host.sh; or WTL_NO_KVM=1 for very slow TCG)");
        }
        Ok(())
    }
}

/// Where the private VM-to-VM LAN plugs in.
#[derive(Clone, Debug)]
pub enum Lan {
    /// listens on 127.0.0.1:PORT
    Server(u16),
    /// connects to 127.0.0.1:PORT (and reconnects)
    Client(u16),
}

/// The options lib/common.sh qemu_cmd took.
#[derive(Clone, Debug, Default)]
pub struct Opts {
    pub name: String,
    pub mem: String,
    pub disk: PathBuf,
    /// writeback unless set
    pub cache: Option<String>,
    /// CD-ROMs in IDE order
    pub cds: Vec<PathBuf>,
    pub serial: Option<PathBuf>,
    pub qmp: Option<Addr>,
    /// HOST:GUEST TCP port forwards on the user-mode NIC
    pub fwds: Vec<(u16, u16)>,
    pub boot_once_d: bool,
    pub show: bool,
    pub vnc: Option<u16>,
    pub pidfile: Option<PathBuf>,
    pub lan: Option<Lan>,
    pub lan_mac: Option<String>,
    pub no_usernet: bool,
    /// boot UEFI: q35 + OVMF, writable vars file
    pub uefi: Option<PathBuf>,
    /// emulated TPM 2.0 via swtpm listening in this dir
    pub tpm: Option<PathBuf>,
    pub no_shutdown: bool,
}

/// qemu -drive option values escape ',' as ',,'
fn qesc(p: &Path) -> String {
    p.to_string_lossy().replace(',', ",,")
}

/// The argv (program first). Machine: i440fx ("pc") + IDE CD-ROMs, disk on ich9-ahci,
/// e1000 NIC (WTL_NIC), std VGA, usb-tablet, localtime RTC; q35 + OVMF pflash with --uefi.
pub fn command(env: &Env, o: &Opts) -> Result<Vec<String>> {
    let mut a: Vec<String> = Vec::new();
    let mut push = |xs: &[&str]| a.extend(xs.iter().map(|s| s.to_string()));
    let machine = if o.uefi.is_some() { "q35" } else { "pc" };
    push(&[&env.qemu, "-name", &o.name, "-machine", machine, "-m", &o.mem, "-smp", &env.cpus]);
    let cpu = |dflt: &str| env.cpu.clone().unwrap_or_else(|| dflt.to_string());
    match env.accel {
        Accel::Tcg => push(&["-cpu", &cpu("max")]),
        Accel::Kvm => push(&["-enable-kvm", "-cpu", &cpu("host,hv_relaxed,hv_spinlocks=0x1fff,hv_vapic,hv_time")]),
        Accel::Hvf => push(&["-accel", "hvf", "-cpu", &cpu("host")]),
        Accel::Whpx => push(&["-accel", "whpx,kernel-irqchip=off", "-cpu", &cpu("max")]),
    }
    if let Some(vars) = &o.uefi {
        ensure!(
            env.ovmf_code.is_file(),
            "UEFI firmware {} not found (install the 'ovmf' package or set WTL_OVMF_CODE)",
            env.ovmf_code.display()
        );
        push(&["-drive", &format!("if=pflash,format=raw,readonly=on,file={}", qesc(&env.ovmf_code))]);
        push(&["-drive", &format!("if=pflash,format=raw,file={}", qesc(vars))]);
    }
    if let Some(dir) = &o.tpm {
        push(&["-chardev", &format!("socket,id=chrtpm,path={}", dir.join("swtpm.sock").display())]);
        push(&["-tpmdev", "emulator,id=tpm0,chardev=chrtpm", "-device", "tpm-tis,tpmdev=tpm0"]);
    }
    push(&["-rtc", "base=localtime", "-vga", "std", "-usb", "-device", "usb-tablet"]);
    push(&["-device", "ich9-ahci,id=ahci"]);
    let cache = o.cache.as_deref().unwrap_or("writeback");
    push(&[
        "-drive",
        &format!("id=disk0,if=none,format=qcow2,file={},cache={},discard=unmap,detect-zeroes=unmap", qesc(&o.disk), cache),
    ]);
    push(&["-device", "ide-hd,drive=disk0,bus=ahci.0"]);
    for (i, c) in o.cds.iter().enumerate() {
        push(&["-drive", &format!("file={},media=cdrom,if=ide,index={},readonly=on", qesc(c), i)]);
    }
    if !o.no_usernet {
        let mut net = "user,id=net0".to_string();
        for (h, g) in &o.fwds {
            net.push_str(&format!(",hostfwd=tcp:127.0.0.1:{}-:{}", h, g));
        }
        push(&["-netdev", &net, "-device", &format!("{},netdev=net0", env.nic)]);
    }
    if let Some(lan) = &o.lan {
        let addr = |p: u16| format!("addr.type=inet,addr.host=127.0.0.1,addr.port={}", p);
        let nd = match lan {
            Lan::Server(p) => format!("stream,id=lan0,server=on,{}", addr(*p)),
            Lan::Client(p) => format!("stream,id=lan0,server=off,reconnect-ms=1000,{}", addr(*p)),
        };
        let mac = o.lan_mac.as_ref().map(|m| format!(",mac={}", m)).unwrap_or_default();
        push(&["-netdev", &nd, "-device", &format!("{},netdev=lan0{}", env.nic, mac)]);
    }
    if let Some(s) = &o.serial {
        push(&["-serial", &format!("file:{}", s.display())]);
    }
    if let Some(q) = &o.qmp {
        push(&["-qmp", &format!("{},server=on,wait=off", q.qemu_arg())]);
    }
    push(&["-boot", if o.boot_once_d { "once=d" } else { "order=c" }]);
    if o.show {
        push(&["-display", &env.display]);
    } else {
        push(&["-display", "none"]);
        if let Some(n) = o.vnc {
            push(&["-vnc", &format!("127.0.0.1:{}", n)]);
        }
    }
    if let Some(p) = &o.pidfile {
        push(&["-pidfile", &p.to_string_lossy()]);
    }
    // keep QEMU alive if the guest powers off (some clients do that on a reboot request); the
    // host then issues system_reset to continue. A guest reset still reboots normally.
    if o.no_shutdown {
        push(&["-no-shutdown"]);
    }
    Ok(a)
}

/// `printf '%q '`-style quoting, for the .cmdline file (copy-paste into a shell).
pub fn shell_quote(s: &str) -> String {
    if !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"_./:=@%+-".contains(&b)) {
        return s.to_string();
    }
    if s.is_empty() {
        return "''".into();
    }
    let mut out = String::new();
    for c in s.chars() {
        if !(c.is_ascii_alphanumeric() || "_./:=@%+-".contains(c)) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Start ARGV in the background (own session, so a terminal Ctrl-C reaches only us; the VM
/// is stopped over QMP), output to LOGF; ARGV is also saved to LOGF.cmdline. Returns the
/// pid once the process survived its first half second.
pub fn start(argv: &[String], logf: &Path) -> Result<u32> {
    let line: Vec<String> = argv.iter().map(|a| shell_quote(a)).collect();
    let mut cl = logf.as_os_str().to_owned();
    cl.push(".cmdline");
    let _ = std::fs::write(PathBuf::from(cl), format!("{} \n", line.join(" ")));
    let out = std::fs::File::create(logf).with_context(|| format!("cannot write {}", logf.display()))?;
    let err = out.try_clone()?;
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]).stdin(Stdio::null()).stdout(out).stderr(err);
    let pid = crate::sys::spawn_detached(&mut cmd).with_context(|| format!("cannot run {}", argv[0]))?;
    std::thread::sleep(std::time::Duration::from_millis(500));
    if !crate::sys::pid_alive(pid) {
        let log = std::fs::read_to_string(logf).unwrap_or_default();
        let mut msg = String::from("qemu failed to start:");
        for l in log.lines() {
            msg.push_str("\n    ");
            msg.push_str(l);
        }
        bail!(msg);
    }
    Ok(pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> Env {
        let root = PathBuf::from("/r");
        let mut vars = seed_vars(&root);
        vars.insert("WTL_QEMU".into(), "qemu-system-x86_64".into());
        vars.insert("WTL_CPUS".into(), "2".into());
        vars.insert("WTL_NIC".into(), "e1000".into());
        let mut e = Env::from_vars(&root, &vars).unwrap();
        e.accel = Accel::Kvm;
        e.cpu = None;
        e
    }

    /// Same argv lab.sh produced for the 2019 client (see labs/state/2019/client.qemu.log.cmdline).
    #[test]
    fn matches_bash_client_cmdline() {
        let s = "/home/dte/Documents/ISO images/win-test-lab/labs/state/2019";
        let o = Opts {
            name: "lab-2019-client".into(),
            mem: "4096".into(),
            disk: format!("{}/client.qcow2", s).into(),
            cds: vec![format!("{}/client.iso", s).into(), format!("{}/mount-X.iso", s).into()],
            serial: Some(format!("{}/client.serial.log", s).into()),
            qmp: Some(Addr::Unix(format!("{}/client.qmp", s).into())),
            vnc: Some(11),
            lan: Some(Lan::Client(47019)),
            lan_mac: Some("52:54:00:4c:41:20".into()),
            no_shutdown: true,
            ..Default::default()
        };
        let got: Vec<String> = command(&env(), &o).unwrap().iter().map(|a| shell_quote(a)).collect();
        let want = r"qemu-system-x86_64 -name lab-2019-client -machine pc -m 4096 -smp 2 -enable-kvm -cpu host\,hv_relaxed\,hv_spinlocks=0x1fff\,hv_vapic\,hv_time -rtc base=localtime -vga std -usb -device usb-tablet -device ich9-ahci\,id=ahci -drive id=disk0\,if=none\,format=qcow2\,file=/home/dte/Documents/ISO\ images/win-test-lab/labs/state/2019/client.qcow2\,cache=writeback\,discard=unmap\,detect-zeroes=unmap -device ide-hd\,drive=disk0\,bus=ahci.0 -drive file=/home/dte/Documents/ISO\ images/win-test-lab/labs/state/2019/client.iso\,media=cdrom\,if=ide\,index=0\,readonly=on -drive file=/home/dte/Documents/ISO\ images/win-test-lab/labs/state/2019/mount-X.iso\,media=cdrom\,if=ide\,index=1\,readonly=on -netdev user\,id=net0 -device e1000\,netdev=net0 -netdev stream\,id=lan0\,server=off\,reconnect-ms=1000\,addr.type=inet\,addr.host=127.0.0.1\,addr.port=47019 -device e1000\,netdev=lan0\,mac=52:54:00:4c:41:20 -serial file:/home/dte/Documents/ISO\ images/win-test-lab/labs/state/2019/client.serial.log -qmp unix:/home/dte/Documents/ISO\ images/win-test-lab/labs/state/2019/client.qmp\,server=on\,wait=off -boot order=c -display none -vnc 127.0.0.1:11 -no-shutdown";
        assert_eq!(got.join(" "), want);
    }

    #[test]
    fn dc_and_accels() {
        let mut e = env();
        let o = Opts {
            name: "lab-2019-dc".into(),
            mem: "3072".into(),
            disk: "/s/dc.qcow2".into(),
            cds: vec!["/s/dc.iso".into()],
            lan: Some(Lan::Server(47019)),
            lan_mac: Some("52:54:00:4c:41:10".into()),
            no_usernet: true,
            ..Default::default()
        };
        let a = command(&e, &o).unwrap().join(" ");
        assert!(a.contains("-netdev stream,id=lan0,server=on,addr.type=inet,addr.host=127.0.0.1,addr.port=47019 -device e1000,netdev=lan0,mac=52:54:00:4c:41:10"));
        assert!(!a.contains("net0"));
        e.accel = Accel::Tcg;
        assert!(command(&e, &o).unwrap().join(" ").contains(" -smp 2 -cpu max -rtc"));
        e.accel = Accel::Whpx;
        assert!(command(&e, &o).unwrap().join(" ").contains("-accel whpx,kernel-irqchip=off -cpu max"));
        e.cpu = Some("Skylake-Client".into());
        assert!(command(&e, &o).unwrap().join(" ").contains("-cpu Skylake-Client"));
        let t = Opts { qmp: Some(Addr::Tcp("127.0.0.1:4444".into())), ..o };
        assert!(command(&e, &t).unwrap().join(" ").contains("-qmp tcp:127.0.0.1:4444,server=on,wait=off"));
    }
}
