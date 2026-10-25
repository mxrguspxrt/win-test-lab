//! Host settings (the WTL_* variables) and the QEMU command line.

use crate::home::Home;
use crate::qmp::Addr;
use crate::util::which;
use anyhow::{anyhow, bail, ensure, Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// What to do when /dev/kvm cannot be opened.
pub const KVM_HINT: &str = "add yourself to the kvm group: sudo usermod -aG kvm $USER, then log in again";

/// How to install QEMU on this kind of host.
pub const QEMU_HINT: &str = if cfg!(target_os = "macos") {
    "brew install qemu"
} else if cfg!(windows) {
    "winget install SoftwareFreedomConservancy.QEMU"
} else {
    "Debian/Ubuntu: sudo apt install qemu-system-x86 qemu-system-arm qemu-utils ovmf qemu-efi-aarch64; Fedora: sudo dnf install qemu-kvm qemu-img edk2-ovmf"
};

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

/// A machine's CPU: what QEMU binary, machine type and devices it gets.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Arch {
    #[default]
    X64,
    Arm64,
}

impl Arch {
    /// From the OS table's arch column ("x64" or "arm64").
    pub fn parse(s: &str) -> Option<Arch> {
        match s {
            "x64" | "amd64" | "x86_64" => Some(Arch::X64),
            "arm64" | "aarch64" => Some(Arch::Arm64),
            _ => None,
        }
    }

    /// The host's own CPU (a guest of this arch runs on the hypervisor, not emulated).
    pub fn host() -> Arch {
        if cfg!(target_arch = "aarch64") {
            Arch::Arm64
        } else {
            Arch::X64
        }
    }

    /// The name Windows answer files use (processorArchitecture).
    pub fn unattend(self) -> &'static str {
        match self {
            Arch::X64 => "amd64",
            Arch::Arm64 => "arm64",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Arch::X64 => "x64",
            Arch::Arm64 => "arm64",
        }
    }
}

/// Settings: the home's folders and the WTL_* variables (environment, else the defaults).
#[derive(Clone, Debug)]
pub struct Env {
    pub home: Home,
    pub iso_dir: PathBuf,
    pub images_dir: PathBuf,
    pub qemu: String,
    /// WTL_QEMU_ARM64: the emulator for arm64 machines (qemu-system-aarch64)
    pub qemu_arm64: String,
    pub qemu_img: String,
    pub accel: Accel,
    /// WTL_ACCEL / WTL_NO_KVM was set: use `accel` for every machine, even another CPU's
    pub accel_forced: bool,
    /// WTL_CPU: overrides the -cpu value
    pub cpu: Option<String>,
    pub cpus: String,
    pub password: String,
    pub nic: String,
    pub ovmf_code: PathBuf,
    pub ovmf_vars: PathBuf,
    /// WTL_AAVMF_CODE / WTL_AAVMF_VARS: the arm64 UEFI firmware (edk2 for aarch64)
    pub aavmf_code: PathBuf,
    pub aavmf_vars: PathBuf,
    /// display used for --show (gtk; cocoa on macOS); WTL_DISPLAY overrides
    pub display: String,
    /// WTL_TEST_FAST=1: skip the fixed settle sleeps (for the fake-QEMU tests)
    pub fast: bool,
}

/// The WTL_* defaults, as `NAME -> value` pairs, given the home.
pub fn defaults(home: &Home) -> Vec<(&'static str, String)> {
    let mut v: Vec<(&'static str, String)> = home.vars().into_iter().map(|(k, p)| (k, p.to_string_lossy().into_owned())).collect();
    v.extend([
        ("WTL_QEMU", if cfg!(windows) { "qemu-system-x86_64.exe" } else { "qemu-system-x86_64" }.into()),
        ("WTL_QEMU_ARM64", if cfg!(windows) { "qemu-system-aarch64.exe" } else { "qemu-system-aarch64" }.into()),
        ("WTL_QEMU_IMG", if cfg!(windows) { "qemu-img.exe" } else { "qemu-img" }.into()),
        ("WTL_NO_KVM", String::new()),
        ("WTL_MEM", "4096".into()),
        ("WTL_CPUS", "2".into()),
        ("WTL_PASSWORD", "WinTest!2026".into()),
        ("WTL_NIC", "e1000".into()),
        ("WTL_OVMF_CODE", firmware("x86_64", "code")),
        ("WTL_OVMF_VARS", firmware("x86_64", "vars")),
        ("WTL_AAVMF_CODE", firmware("aarch64", "code")),
        ("WTL_AAVMF_VARS", firmware("aarch64", "vars")),
    ]);
    v
}

/// The first UEFI firmware file (ARCH x86_64 or aarch64, KIND code or vars) that exists in the
/// usual places: the distro packages (ovmf / qemu-efi-aarch64), Homebrew's and MacPorts' QEMU
/// (edk2-*.fd; Homebrew ships no aarch64 vars file, the arm one is the same empty store), and
/// QEMU for Windows. The first candidate if none exists, so the error names a real path.
pub fn firmware(arch: &str, kind: &str) -> String {
    let linux: &[&str] = match (arch, kind) {
        ("x86_64", "code") => &[
            "/usr/share/OVMF/OVMF_CODE_4M.fd",
            "/usr/share/OVMF/OVMF_CODE.fd",
            "/usr/share/edk2/ovmf/OVMF_CODE.fd",
            "/usr/share/edk2/x64/OVMF_CODE.4m.fd",
        ],
        ("x86_64", _) => &[
            "/usr/share/OVMF/OVMF_VARS_4M.fd",
            "/usr/share/OVMF/OVMF_VARS.fd",
            "/usr/share/edk2/ovmf/OVMF_VARS.fd",
            "/usr/share/edk2/x64/OVMF_VARS.4m.fd",
        ],
        (_, "code") => {
            &["/usr/share/AAVMF/AAVMF_CODE.fd", "/usr/share/qemu-efi-aarch64/QEMU_EFI.fd", "/usr/share/edk2/aarch64/QEMU_EFI-pflash.raw"]
        }
        _ => &[
            "/usr/share/AAVMF/AAVMF_VARS.fd",
            "/usr/share/qemu-efi-aarch64/QEMU_VARS.fd",
            "/usr/share/edk2/aarch64/vars-template-pflash.raw",
        ],
    };
    let edk2 = match (arch, kind) {
        ("x86_64", "code") => "edk2-x86_64-code.fd",
        ("x86_64", _) => "edk2-i386-vars.fd",
        (_, "code") => "edk2-aarch64-code.fd",
        _ => "edk2-arm-vars.fd",
    };
    let mut cands: Vec<String> = linux.iter().map(|s| s.to_string()).collect();
    for dir in
        ["/opt/homebrew/share/qemu", "/usr/local/share/qemu", "/opt/local/share/qemu", "C:/Program Files/qemu/share", "/usr/share/qemu"]
    {
        cands.push(format!("{}/{}", dir, edk2));
    }
    let first = cands[0].clone();
    cands.into_iter().find(|c| Path::new(c).is_file()).unwrap_or(first)
}

/// Seed a variable map: every WTL_* default, overridden by the process environment. The
/// home's folders are already resolved (environment included) and are taken as they are.
pub fn seed_vars(home: &Home) -> HashMap<String, String> {
    let mut m = HashMap::new();
    let folders: Vec<&str> = home.vars().iter().map(|(k, _)| *k).collect();
    for (k, v) in defaults(home) {
        let v = if folders.contains(&k) { v } else { std::env::var(k).ok().filter(|x| !x.is_empty() || k == "WTL_NO_KVM").unwrap_or(v) };
        m.insert(k.to_string(), v);
    }
    m
}

impl Env {
    /// Build from a variable map made by [`seed_vars`] (+ the lab's own password).
    pub fn from_vars(home: &Home, vars: &HashMap<String, String>) -> Result<Env> {
        let get = |k: &str| -> String { vars.get(k).cloned().or_else(|| std::env::var(k).ok()).unwrap_or_default() };
        let nonempty = |k: &str| Some(get(k)).filter(|v| !v.is_empty());
        let accel_forced = nonempty("WTL_ACCEL").is_some() || get("WTL_NO_KVM") == "1";
        let accel = match nonempty("WTL_ACCEL") {
            Some(a) => Accel::parse(&a).ok_or_else(|| anyhow!("WTL_ACCEL={} is not one of kvm, whpx, hvf, tcg", a))?,
            None if get("WTL_NO_KVM") == "1" => Accel::Tcg,
            None => Accel::host_default(),
        };
        let display = nonempty("WTL_DISPLAY").unwrap_or_else(|| if cfg!(target_os = "macos") { "cocoa" } else { "gtk" }.into());
        Ok(Env {
            home: home.clone(),
            iso_dir: PathBuf::from(get("WTL_ISO_DIR")),
            images_dir: PathBuf::from(get("WTL_IMAGES_DIR")),
            qemu: get("WTL_QEMU"),
            qemu_arm64: get("WTL_QEMU_ARM64"),
            qemu_img: get("WTL_QEMU_IMG"),
            accel,
            accel_forced,
            cpu: nonempty("WTL_CPU"),
            cpus: get("WTL_CPUS"),
            password: get("WTL_PASSWORD"),
            nic: nonempty("WTL_NIC").unwrap_or_else(|| "e1000".into()),
            ovmf_code: PathBuf::from(get("WTL_OVMF_CODE")),
            ovmf_vars: PathBuf::from(get("WTL_OVMF_VARS")),
            aavmf_code: PathBuf::from(get("WTL_AAVMF_CODE")),
            aavmf_vars: PathBuf::from(get("WTL_AAVMF_VARS")),
            display,
            fast: get("WTL_TEST_FAST") == "1",
        })
    }

    /// The emulator binary for ARCH ("x64" or "arm64").
    pub fn qemu_for(&self, arch: Arch) -> &str {
        match arch {
            Arch::X64 => &self.qemu,
            Arch::Arm64 => &self.qemu_arm64,
        }
    }

    /// The accelerator a machine of ARCH gets: the host's (KVM / WHPX / HVF) when the guest
    /// CPU is the host's own, else TCG (emulation, slow). WTL_ACCEL still wins.
    pub fn accel_for(&self, arch: Arch) -> Accel {
        if self.accel_forced || arch == Arch::host() {
            self.accel
        } else {
            Accel::Tcg
        }
    }

    /// check_tools: qemu (for every ARCH in use) + qemu-img runnable, the accelerator usable.
    pub fn check_tools(&self, arches: &[Arch]) -> Result<()> {
        for a in arches {
            let q = self.qemu_for(*a);
            ensure!(
                which(q).is_some(),
                "'{}' not found (install QEMU: {}; or set {})",
                q,
                QEMU_HINT,
                if *a == Arch::Arm64 { "WTL_QEMU_ARM64" } else { "WTL_QEMU" }
            );
        }
        ensure!(which(&self.qemu_img).is_some(), "'{}' not found (install QEMU: {}; or set WTL_QEMU_IMG)", self.qemu_img, QEMU_HINT);
        if arches.iter().any(|a| self.accel_for(*a) == Accel::Kvm)
            && std::fs::OpenOptions::new().read(true).write(true).open("/dev/kvm").is_err()
        {
            bail!("/dev/kvm missing or not writable ({}; or WTL_NO_KVM=1 for very slow TCG)", KVM_HINT);
        }
        Ok(())
    }
}

/// The private VM-to-VM LAN: a UDP multicast group on loopback (`-netdev dgram`). It joins
/// any number of VMs without root; every machine of a lab uses the same group and port, and
/// different labs use different ports.
#[derive(Clone, Debug)]
pub struct Lan {
    /// multicast group address, e.g. 239.77.30.1
    pub group: String,
    /// UDP port (the lab's `network.lan_port`)
    pub port: u16,
}

/// The options lib/common.sh qemu_cmd took.
#[derive(Clone, Debug, Default)]
pub struct Opts {
    pub name: String,
    pub mem: String,
    /// vCPUs for this VM (default: WTL_CPUS from the environment)
    pub cpus: Option<String>,
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
    /// the guest CPU (x64: pc/q35; arm64: the `virt` machine, always UEFI)
    pub arch: Arch,
}

/// qemu -drive option values escape ',' as ',,'
fn qesc(p: &Path) -> String {
    p.to_string_lossy().replace(',', ",,")
}

/// The argv (program first). x64: i440fx ("pc") + IDE CD-ROMs, disk on ich9-ahci, e1000 NIC
/// (WTL_NIC), std VGA, usb-tablet, localtime RTC; q35 + OVMF pflash with --uefi. arm64: see
/// [`command_arm64`].
pub fn command(env: &Env, o: &Opts) -> Result<Vec<String>> {
    if o.arch == Arch::Arm64 {
        return command_arm64(env, o);
    }
    let mut a: Vec<String> = Vec::new();
    let mut push = |xs: &[&str]| a.extend(xs.iter().map(|s| s.to_string()));
    let machine = if o.uefi.is_some() { "q35" } else { "pc" };
    let cpus = o.cpus.as_deref().unwrap_or(&env.cpus);
    push(&[&env.qemu, "-name", &o.name, "-machine", machine, "-m", &o.mem, "-smp", cpus]);
    let cpu = |dflt: &str| env.cpu.clone().unwrap_or_else(|| dflt.to_string());
    match env.accel_for(Arch::X64) {
        // an x64 guest on an Arm host (Apple Silicon): multi-threaded TCG, one host thread
        // per vCPU, is several times faster than the single-threaded default
        Accel::Tcg if Arch::host() != Arch::X64 => push(&["-accel", "tcg,thread=multi", "-cpu", &cpu("max")]),
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
    push(&["-drive", &format!("id=disk0,if=none,format=qcow2,file={},cache={},discard=unmap,detect-zeroes=unmap", qesc(&o.disk), cache)]);
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
    if let Some(Lan { group, port }) = &o.lan {
        // one multicast group carries the whole lab LAN; each VM both sends to and receives
        // on 127.0.0.1:PORT of group GROUP
        let nd = format!(
            "dgram,id=lan0,remote.type=inet,remote.host={group},remote.port={port},local.type=inet,local.host=127.0.0.1,local.port={port}"
        );
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

/// The name of the virtio-serial port an arm64 guest writes its WTL lines to (the guest finds
/// it as `\\.\Global\<name>`; guest/agent.ps1, setup.ps1).
pub const GUEST_CHANNEL: &str = "org.wintestlab.0";

/// The arm64 argv: QEMU's `virt` machine with the aarch64 UEFI firmware (Windows on Arm
/// needs UEFI), ramfb display, xHCI USB keyboard + tablet, the system disk on NVMe and the
/// CD-ROMs as USB mass storage (Windows on Arm has in-box drivers for both, and the arm64
/// UEFI firmware boots no AHCI CD-ROM), the NICs as virtio-net and the guest's report channel
/// as a virtio-serial port (virtio-win's NetKVM / vioserial, staged by the build: Windows on
/// Arm ships no driver for any NIC or UART QEMU emulates).
/// On an Arm host the CPU is `host` under the hypervisor (HVF/KVM/WHPX); elsewhere TCG.
pub fn command_arm64(env: &Env, o: &Opts) -> Result<Vec<String>> {
    let mut a: Vec<String> = Vec::new();
    let mut push = |xs: &[&str]| a.extend(xs.iter().map(|s| s.to_string()));
    let cpus = o.cpus.as_deref().unwrap_or(&env.cpus);
    let cpu = |dflt: &str| env.cpu.clone().unwrap_or_else(|| dflt.to_string());
    push(&[&env.qemu_arm64, "-name", &o.name, "-machine", "virt", "-m", &o.mem, "-smp", cpus]);
    match env.accel_for(Arch::Arm64) {
        Accel::Tcg => push(&["-accel", "tcg,thread=multi", "-cpu", &cpu("max")]),
        Accel::Kvm => push(&["-enable-kvm", "-cpu", &cpu("host")]),
        Accel::Hvf => push(&["-accel", "hvf", "-cpu", &cpu("host")]),
        Accel::Whpx => push(&["-accel", "whpx", "-cpu", &cpu("host")]),
    }
    let vars = o.uefi.as_ref().ok_or_else(|| anyhow!("an arm64 machine needs a UEFI vars file"))?;
    ensure!(
        env.aavmf_code.is_file(),
        "arm64 UEFI firmware {} not found (install QEMU's edk2 firmware, e.g. 'qemu-efi-aarch64', or set WTL_AAVMF_CODE)",
        env.aavmf_code.display()
    );
    push(&["-drive", &format!("if=pflash,format=raw,readonly=on,file={}", qesc(&env.aavmf_code))]);
    push(&["-drive", &format!("if=pflash,format=raw,file={}", qesc(vars))]);
    push(&["-rtc", "base=localtime", "-device", "ramfb", "-device", "qemu-xhci,id=xhci"]);
    push(&["-device", "usb-kbd", "-device", "usb-tablet"]);
    let cache = o.cache.as_deref().unwrap_or("writeback");
    push(&["-drive", &format!("id=disk0,if=none,format=qcow2,file={},cache={},discard=unmap,detect-zeroes=unmap", qesc(&o.disk), cache)]);
    // the disk boots, except on the install boot (boot_once_d), where the first CD does
    let disk_boot = if o.boot_once_d { "1" } else { "0" };
    push(&["-device", &format!("nvme,serial=wtl0,drive=disk0,bootindex={}", disk_boot)]);
    for (i, c) in o.cds.iter().enumerate() {
        push(&["-drive", &format!("id=cd{i},if=none,media=cdrom,readonly=on,file={}", qesc(c))]);
        let boot = if o.boot_once_d && i == 0 { ",bootindex=0" } else { "" };
        push(&["-device", &format!("usb-storage,drive=cd{i},removable=on{boot}")]);
    }
    // The serial log: the firmware's UART output, plus the guest's WTL lines. Windows on Arm
    // has no driver for the PL011 UART, so the guest writes them to a virtio-serial port
    // (vioserial, from virtio-win), which shows up as a COM port; both append to one file.
    if let Some(s) = &o.serial {
        let p = s.to_string_lossy().replace(',', ",,");
        push(&["-chardev", &format!("file,id=ser0,path={},append=on", p), "-serial", "chardev:ser0"]);
        push(&["-chardev", &format!("file,id=wtlch,path={},append=on", p)]);
        push(&[
            "-device",
            "virtio-serial-pci,id=vser0",
            "-device",
            &format!("virtserialport,bus=vser0.0,chardev=wtlch,name={}", GUEST_CHANNEL),
        ]);
    }
    // NICs: virtio-net (NetKVM from virtio-win; Windows on Arm has no in-box driver for any
    // NIC QEMU emulates)
    if !o.no_usernet {
        let mut net = "user,id=net0".to_string();
        for (h, g) in &o.fwds {
            net.push_str(&format!(",hostfwd=tcp:127.0.0.1:{}-:{}", h, g));
        }
        push(&["-netdev", &net, "-device", "virtio-net-pci,netdev=net0"]);
    }
    if let Some(Lan { group, port }) = &o.lan {
        let nd = format!(
            "dgram,id=lan0,remote.type=inet,remote.host={group},remote.port={port},local.type=inet,local.host=127.0.0.1,local.port={port}"
        );
        let mac = o.lan_mac.as_ref().map(|m| format!(",mac={}", m)).unwrap_or_default();
        push(&["-netdev", &nd, "-device", &format!("virtio-net-pci,netdev=lan0{}", mac)]);
    }
    if let Some(q) = &o.qmp {
        push(&["-qmp", &format!("{},server=on,wait=off", q.qemu_arg())]);
    }
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
        let home = Home::fixed(Path::new("/r"));
        let mut vars = seed_vars(&home);
        vars.insert("WTL_QEMU".into(), "qemu-system-x86_64".into());
        vars.insert("WTL_CPUS".into(), "2".into());
        vars.insert("WTL_NIC".into(), "e1000".into());
        let mut e = Env::from_vars(&home, &vars).unwrap();
        e.accel = Accel::Kvm;
        // as if WTL_ACCEL were set: the pinned x64 command lines hold on any host CPU
        e.accel_forced = true;
        e.cpu = None;
        e
    }

    /// The whole argv of a member, as a shell line (what <vm>.qemu.log.cmdline holds).
    #[test]
    fn member_cmdline() {
        let s = "/home/u/my labs/labs/state/school";
        let o = Opts {
            name: "lab-school-CL1".into(),
            mem: "4096".into(),
            disk: format!("{}/CL1.qcow2", s).into(),
            cds: vec![format!("{}/CL1.iso", s).into(), format!("{}/mount-CL1-X.iso", s).into()],
            serial: Some(format!("{}/CL1.serial.log", s).into()),
            qmp: Some(Addr::Unix(format!("{}/CL1.qmp", s).into())),
            vnc: Some(11),
            lan: Some(Lan { group: "239.77.30.1".into(), port: 47030 }),
            lan_mac: Some("52:54:00:4c:41:11".into()),
            no_shutdown: true,
            ..Default::default()
        };
        let got: Vec<String> = command(&env(), &o).unwrap().iter().map(|a| shell_quote(a)).collect();
        let want = r"qemu-system-x86_64 -name lab-school-CL1 -machine pc -m 4096 -smp 2 -enable-kvm -cpu host\,hv_relaxed\,hv_spinlocks=0x1fff\,hv_vapic\,hv_time -rtc base=localtime -vga std -usb -device usb-tablet -device ich9-ahci\,id=ahci -drive id=disk0\,if=none\,format=qcow2\,file=/home/u/my\ labs/labs/state/school/CL1.qcow2\,cache=writeback\,discard=unmap\,detect-zeroes=unmap -device ide-hd\,drive=disk0\,bus=ahci.0 -drive file=/home/u/my\ labs/labs/state/school/CL1.iso\,media=cdrom\,if=ide\,index=0\,readonly=on -drive file=/home/u/my\ labs/labs/state/school/mount-CL1-X.iso\,media=cdrom\,if=ide\,index=1\,readonly=on -netdev user\,id=net0 -device e1000\,netdev=net0 -netdev dgram\,id=lan0\,remote.type=inet\,remote.host=239.77.30.1\,remote.port=47030\,local.type=inet\,local.host=127.0.0.1\,local.port=47030 -device e1000\,netdev=lan0\,mac=52:54:00:4c:41:11 -serial file:/home/u/my\ labs/labs/state/school/CL1.serial.log -qmp unix:/home/u/my\ labs/labs/state/school/CL1.qmp\,server=on\,wait=off -boot order=c -display none -vnc 127.0.0.1:11 -no-shutdown";
        assert_eq!(got.join(" "), want);
    }

    #[test]
    fn dc_and_accels() {
        let mut e = env();
        let o = Opts {
            name: "lab-school-DC1".into(),
            mem: "3072".into(),
            disk: "/s/DC1.qcow2".into(),
            cds: vec!["/s/DC1.iso".into()],
            lan: Some(Lan { group: "239.77.30.1".into(), port: 47030 }),
            lan_mac: Some("52:54:00:4c:41:10".into()),
            no_usernet: true,
            ..Default::default()
        };
        let a = command(&e, &o).unwrap().join(" ");
        assert!(a.contains("-device e1000,netdev=lan0,mac=52:54:00:4c:41:10"), "{}", a);
        // the DC is isolated: no user-mode NAT
        assert!(!a.contains("net0"));
        e.accel = Accel::Tcg;
        // on an Arm host an x64 guest is emulated: multi-threaded TCG
        let tcg = if Arch::host() == Arch::X64 { " -smp 2 -cpu max -rtc" } else { " -smp 2 -accel tcg,thread=multi -cpu max -rtc" };
        assert!(command(&e, &o).unwrap().join(" ").contains(tcg));
        e.accel = Accel::Whpx;
        assert!(command(&e, &o).unwrap().join(" ").contains("-accel whpx,kernel-irqchip=off -cpu max"));
        e.cpu = Some("Skylake-Client".into());
        assert!(command(&e, &o).unwrap().join(" ").contains("-cpu Skylake-Client"));
        let t = Opts { qmp: Some(Addr::Tcp("127.0.0.1:4444".into())), ..o };
        assert!(command(&e, &t).unwrap().join(" ").contains("-qmp tcp:127.0.0.1:4444,server=on,wait=off"));
    }

    /// An arm64 machine: virt + aarch64 UEFI, NVMe disk, USB CDs and NIC; hvf on an Arm Mac,
    /// TCG for it anywhere else; x64 on an Arm host gets multi-threaded TCG.
    #[test]
    fn arm64_cmdline() {
        let mut e = env();
        e.accel_forced = false;
        e.qemu_arm64 = "qemu-system-aarch64".into();
        e.aavmf_code = std::env::current_exe().unwrap(); // any existing file
        e.accel = Accel::Hvf;
        let o = Opts {
            name: "lab-a-WS".into(),
            mem: "4096".into(),
            disk: "/s/WS.qcow2".into(),
            cds: vec!["/s/win.iso".into(), "/s/u.iso".into()],
            uefi: Some("/s/vars.fd".into()),
            boot_once_d: true,
            arch: Arch::Arm64,
            ..Default::default()
        };
        let a = command(&e, &o).unwrap().join(" ");
        assert!(a.starts_with("qemu-system-aarch64 -name lab-a-WS -machine virt -m 4096 -smp 2"), "{}", a);
        let accel = if Arch::host() == Arch::Arm64 { "-accel hvf -cpu host" } else { "-accel tcg,thread=multi -cpu max" };
        assert!(a.contains(accel), "{}", a);
        assert!(a.contains("-device nvme,serial=wtl0,drive=disk0,bootindex=1"), "{}", a);
        assert!(a.contains("-device usb-storage,drive=cd0,removable=on,bootindex=0 "), "{}", a);
        assert!(a.contains("-device usb-storage,drive=cd1,removable=on "), "{}", a);
        assert!(a.contains("-netdev user,id=net0 -device virtio-net-pci,netdev=net0"), "{}", a);
        // with a serial log: UART + the guest's virtio-serial channel, both appending to it
        let ser = command(&e, &Opts { serial: Some("/s/a,b.log".into()), ..o.clone() }).unwrap().join(" ");
        assert!(ser.contains("-chardev file,id=ser0,path=/s/a,,b.log,append=on -serial chardev:ser0"), "{}", ser);
        assert!(ser.contains("-chardev file,id=wtlch,path=/s/a,,b.log,append=on"), "{}", ser);
        assert!(
            ser.contains("-device virtio-serial-pci,id=vser0 -device virtserialport,bus=vser0.0,chardev=wtlch,name=org.wintestlab.0"),
            "{}",
            ser
        );
        assert!(!a.contains("ide-") && !a.contains("e1000") && !a.contains("-boot"), "{}", a);
        // a normal boot: from the disk
        let b = command(&e, &Opts { boot_once_d: false, ..o.clone() }).unwrap().join(" ");
        assert!(b.contains("bootindex=0") && !b.contains("removable=on,bootindex"), "{}", b);
        // no vars file -> error; missing firmware -> error naming the variable
        assert!(command(&e, &Opts { uefi: None, ..o.clone() }).is_err());
        e.aavmf_code = "/nonexistent/edk2-aarch64-code.fd".into();
        assert!(format!("{:#}", command(&e, &o).unwrap_err()).contains("WTL_AAVMF_CODE"));
        // an x64 guest on this host: KVM etc. only when the host is x64; forced accel wins
        let mut x = env();
        x.accel_forced = false;
        x.accel = Accel::Hvf;
        let xa = command(&x, &Opts { arch: Arch::X64, ..o.clone() }).unwrap().join(" ");
        let want = if Arch::host() == Arch::X64 { "-accel hvf -cpu host" } else { "-accel tcg,thread=multi -cpu max" };
        assert!(xa.contains(want), "{}", xa);
        x.accel_forced = true;
        assert!(command(&x, &Opts { arch: Arch::X64, ..o }).unwrap().join(" ").contains("-accel hvf"));
    }

    #[test]
    fn mcast_lan_and_cpus() {
        let e = env();
        let o = Opts {
            name: "lab-school-CL1".into(),
            mem: "4096".into(),
            cpus: Some("4".into()),
            disk: "/s/CL1.qcow2".into(),
            lan: Some(Lan { group: "239.77.30.1".into(), port: 47030 }),
            lan_mac: Some("52:54:00:4c:41:11".into()),
            ..Default::default()
        };
        let a = command(&e, &o).unwrap().join(" ");
        assert!(a.contains("-smp 4"), "{}", a);
        assert!(
            a.contains("-netdev dgram,id=lan0,remote.type=inet,remote.host=239.77.30.1,remote.port=47030,local.type=inet,local.host=127.0.0.1,local.port=47030 -device e1000,netdev=lan0,mac=52:54:00:4c:41:11"),
            "{}", a
        );
        // members still get user-mode NAT
        assert!(a.contains("user,id=net0"), "{}", a);
    }
}
