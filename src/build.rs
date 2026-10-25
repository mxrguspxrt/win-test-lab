//! Install Windows into a base image, unattended: `images/<name>.qcow2`.
//!
//! Finds the ISO for the OS key (`images-list.conf` in the home, else any ISO in the ISO folder
//! whose image 1 has the right build and CPU), picks the edition's image index, renders the
//! answer file, writes the WTL_BUILD disc (answer file + guest/*.ps1 + build.json), boots QEMU
//! with the installer, waits for `BUILD_DONE` from guest/setup.ps1 and the guest's power-off,
//! and renames `<name>.qcow2.partial` to `<name>.qcow2`.

use crate::assets;
use crate::home::Home;
use crate::lab::{pick_vnc_display, sock_path, write_file};
use crate::logx::{err, log, ok, warn};
use crate::oslist::OsInfo;
use crate::qemu::{self, Arch, Env, Opts};
use crate::qmp::{Addr, Qmp};
use crate::serialwait::{Outcome, Wait};
use crate::{mkiso, sys, util, winiso};
use anyhow::{anyhow, bail, Context, Result};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// What to build.
pub struct Build<'a> {
    pub os: &'a OsInfo,
    /// `images/<image>.qcow2`
    pub image: &'a str,
    /// the Windows computer name baked into the image
    pub computer: &'a str,
    pub updates: bool,
    pub show: bool,
    /// this ISO instead of searching WTL_ISO_DIR
    pub iso: Option<PathBuf>,
    /// whole hours the install may take (Windows Update can take many)
    pub timeout_h: f64,
}

/// `<KEY> = <file>` from the home's images-list.conf for an OS key (path relative to the ISO
/// folder).
fn catalog_iso(home_dir: &Path, key: &str) -> Option<String> {
    let text = std::fs::read_to_string(home_dir.join("images-list.conf")).ok()?;
    let want = crate::oslist::normalize(key);
    let pairs: Vec<(&str, &str)> =
        text.lines().filter_map(|l| l.split('#').next()?.split_once('=')).map(|(k, v)| (k.trim(), v.trim())).collect();
    // the exact key first, then any variant key of the same OS (win11_en_arm64 -> win11-arm64)
    pairs
        .iter()
        .find(|(k, _)| *k == want)
        .or_else(|| pairs.iter().find(|(k, _)| crate::oslist::normalize(k) == want))
        .map(|(_, v)| v.to_string())
}

/// The ISO for OS: its images-list.conf entry if that ISO's image 1 has the OS's build and
/// CPU, else the newest ISO in ISO_DIR that does. (x64 and arm64 Windows 11 share a build
/// number, so the CPU decides between them.)
pub fn find_iso(home: &Home, iso_dir: &Path, os: &OsInfo) -> Option<PathBuf> {
    let want_build: u32 = os.build.parse().ok()?;
    let fits = |p: &Path| matches!(winiso::iso_build_arch(p), Ok((b, a)) if b == want_build && a == os.arch);
    if let Some(c) = catalog_iso(&home.dir, &os.key) {
        let p = if Path::new(&c).is_absolute() { PathBuf::from(&c) } else { iso_dir.join(&c) };
        if p.is_file() && fits(&p) {
            return Some(p);
        }
    }
    let mut isos: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(iso_dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("iso")))
        .map(|p| (std::fs::metadata(&p).and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH), p))
        .collect();
    isos.sort_by_key(|(t, _)| std::cmp::Reverse(*t));
    isos.into_iter().map(|(_, p)| p).find(|p| fits(p))
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

/// Fill `@NAME@` placeholders; CRLF line ends (Windows Setup reads either). Unreplaced
/// placeholders are an error.
pub fn render_template(tmpl: &str, subs: &[(&str, String)]) -> Result<String> {
    let mut t = tmpl.trim_start_matches('\u{feff}').replace("\r\n", "\n");
    for (k, v) in subs {
        t = t.replace(&format!("@{}@", k), v);
    }
    let left: Vec<&str> = t
        .match_indices('@')
        .map(|(i, _)| &t[i..])
        .filter_map(|r| {
            let end = r[1..].find('@')?;
            let name = &r[1..1 + end];
            (!name.is_empty() && name.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')).then_some(&r[..end + 2])
        })
        .collect();
    if !left.is_empty() {
        bail!("unreplaced placeholders in the answer file: {}", left.join(" "));
    }
    Ok(t.replace('\n', "\r\n"))
}

/// The guest's pinned image index for OS on ISO: the edition, else the table's fallback.
fn image_index(iso: &Path, os: &OsInfo) -> Result<u32> {
    let imgs = match winiso::Media::open(iso)?.images() {
        Ok(i) => i,
        Err(e) if e.downcast_ref::<winiso::NoWim>().is_some() => {
            bail!("{} has no sources/install.wim - not an installation disc", iso.display())
        }
        Err(e) => return Err(e),
    };
    if let Some(i) = winiso::pick(&imgs, &os.edition, &os.inst_type) {
        log(&format!("image index {} ({} / {})", i, os.edition, os.inst_type));
        return Ok(i);
    }
    let i: u32 = os.fallback_index.parse().unwrap_or(1);
    warn(&format!("could not find {}/{} in the WIM; using fallback index {}", os.edition, os.inst_type, i));
    Ok(i)
}

/// Stage + write the WTL_BUILD disc: autounattend.xml and WTL\ (setup.ps1, agent.ps1, every
/// other guest script, build.json).
fn make_build_disc(home: &Home, run_dir: &Path, b: &Build, idx: u32, password: &str) -> Result<PathBuf> {
    let os = b.os;
    let arch = Arch::parse(&os.arch).unwrap_or_default();
    let stage = run_dir.join("stage");
    let _ = std::fs::remove_dir_all(&stage);
    std::fs::create_dir_all(stage.join("WTL")).with_context(|| format!("cannot create {}", stage.display()))?;
    // UEFI machines (Win11, every arm64 machine) get the GPT layout + the Win11 check bypass
    let uefi = os.firmware == "uefi" || arch == Arch::Arm64;
    let tmpl = if uefi { "autounattend-uefi.xml.tmpl" } else { "autounattend.xml.tmpl" };
    let text = String::from_utf8(assets::guest_file(home, tmpl)?).with_context(|| format!("{} is not UTF-8", tmpl))?;
    // Client (Win10/11) retail media refuse to go on without a key; evaluation Server media
    // must NOT get one. Microsoft's published generic Pro key installs, does not activate.
    let prodkey = if os.kind == "client" {
        let pk = std::env::var("WTL_PRODUCT_KEY").ok().filter(|k| !k.is_empty()).unwrap_or_else(|| "W269N-WFGWX-YVC9B-4J6C9-T83GX".into());
        format!("<ProductKey><Key>{}</Key><WillShowUI>OnError</WillShowUI></ProductKey>", pk)
    } else {
        String::new()
    };
    let xml = render_template(
        &text,
        &[
            ("IMAGE_INDEX", idx.to_string()),
            ("PASSWORD", xml_escape(password)),
            ("COMPUTERNAME", b.computer.to_string()),
            ("OS", os.key.clone()),
            ("PRODUCTKEY", prodkey),
            ("ARCH", arch.unattend().to_string()),
        ],
    )?;
    write_file(&stage.join("autounattend.xml"), xml)?;
    for (name, data) in assets::guest_files(home)? {
        if name.ends_with(".ps1") {
            write_file(&stage.join("WTL").join(&name), data)?;
        }
    }
    for f in ["setup.ps1", "agent.ps1"] {
        if !stage.join("WTL").join(f).is_file() {
            bail!("missing guest/{}", f);
        }
    }
    // drivers Windows does not ship for this machine's devices (Windows on Arm has none for any
    // network card or serial port QEMU emulates): Setup installs everything under
    // \$WinPEDriver$ on any attached disc, in WinPE and into the installed system
    let n = stage_drivers(&home.drivers_dir, arch, &stage.join("$WinPEDriver$"))?;
    if n > 0 {
        log(&format!("drivers for {}: {} (from {}, see docs/ISOS.md)", arch.as_str(), n, home.drivers_dir.display()));
    }
    let cfg = serde_json::json!({ "os": os.key, "updates": b.updates, "password": password });
    write_file(&stage.join("WTL/build.json"), cfg.to_string())?;
    let iso = run_dir.join("unattend.iso");
    mkiso::make_iso(&stage, &iso, "WTL_BUILD").context("mkiso of the build disc failed")?;
    Ok(iso)
}

/// The driver folders a machine of ARCH needs, under drivers/ (virtio-win's layout:
/// `<driver>/w11/<ARM64|amd64>/`). arm64 machines need NetKVM (network) and vioserial (the
/// serial channel); x64 machines use in-box drivers and get none.
pub const ARM64_DRIVERS: [&str; 2] = ["NetKVM", "vioserial"];

/// The newest virtio-win-*/ folder in the drivers folder (a full extracted virtio-win ISO, or
/// just the driver folders).
fn virtio_source(drivers: &Path) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(drivers)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir() && util::basename(&p.to_string_lossy()).starts_with("virtio-win"))
        .collect();
    dirs.sort();
    dirs.pop()
}

/// Copy the drivers ARCH needs into DEST; returns how many driver folders were staged. An
/// arm64 build without them fails here, before a 50 min install that could never report back.
fn stage_drivers(drivers: &Path, arch: Arch, dest: &Path) -> Result<usize> {
    if arch != Arch::Arm64 {
        return Ok(0);
    }
    let src = virtio_source(drivers).ok_or_else(|| {
        anyhow!(
            "an arm64 machine needs the virtio-win network and serial drivers in {} (Windows on Arm has no driver for \
             any network card QEMU emulates); see docs/ISOS.md",
            drivers.display()
        )
    })?;
    let mut n = 0;
    for drv in ARM64_DRIVERS {
        let from = src.join(drv).join("w11").join("ARM64");
        let inf = std::fs::read_dir(&from)
            .ok()
            .and_then(|r| r.flatten().map(|e| e.path()).find(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("inf"))))
            .ok_or_else(|| anyhow!("{}: no driver .inf in {} (want virtio-win's {}/w11/ARM64)", drv, from.display(), drv))?;
        // every file the .inf installs must be there, or Setup skips the driver silently
        // (drvload: 0x80070002) and the machine comes up without a network
        for f in inf_source_files(&std::fs::read_to_string(&inf).unwrap_or_default()) {
            if !from.join(&f).is_file() {
                bail!("{}: {} lists {} but it is not in {}", drv, util::basename(&inf.to_string_lossy()), f, from.display());
            }
        }
        let to = dest.join(drv);
        std::fs::create_dir_all(&to)?;
        for e in std::fs::read_dir(&from)?.flatten() {
            let p = e.path();
            // the .pdb debug symbols are most of the size and Setup does not need them
            if p.is_file() && !p.extension().is_some_and(|x| x.eq_ignore_ascii_case("pdb")) {
                std::fs::copy(&p, to.join(e.file_name())).with_context(|| format!("cannot copy {}", p.display()))?;
            }
        }
        n += 1;
    }
    Ok(n)
}

/// The file names in an .inf's [SourceDisksFiles] section (`name = disk,,`).
fn inf_source_files(inf: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut inside = false;
    for line in inf.lines() {
        let l = line.split(';').next().unwrap_or("").trim();
        if l.starts_with('[') {
            inside = l.eq_ignore_ascii_case("[SourceDisksFiles]") || l.to_ascii_lowercase().starts_with("[sourcedisksfiles.arm64");
            continue;
        }
        if inside {
            if let Some((name, _)) = l.split_once('=') {
                let n = name.trim();
                if !n.is_empty() {
                    out.push(n.to_string());
                }
            }
        }
    }
    out
}

/// Windows' UEFI install media show "Press any key to boot from CD or DVD" for ~5 s and then
/// fall through to the firmware shell. Once the firmware says it is starting a boot option
/// (it can take a minute to get there), press Enter over QMP for [`BOOT_KEY_SECS`]: just the
/// prompt's length. Keys pressed after the firmware hands over to Windows wait in the USB
/// keyboard and reach Setup, whose progress page has Cancel focused: with 20 s of presses an
/// arm64 install stopped at "Are you sure you want to quit?".
fn press_boot_key(addr: Addr, serial: PathBuf, pid: u32) {
    std::thread::spawn(move || {
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(600) {
            if !sys::pid_alive(pid) {
                return;
            }
            if std::fs::read(&serial).is_ok_and(|b| b.windows(13).any(|w| w == b"starting Boot")) {
                break;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        let t1 = Instant::now();
        while t1.elapsed() < Duration::from_secs_f64(BOOT_KEY_SECS) && sys::pid_alive(pid) {
            if let Ok(mut q) = Qmp::connect(&addr, Duration::from_secs(2)) {
                let _ = q.cmd("send-key", Some(serde_json::json!({ "keys": [{ "type": "qcode", "data": "ret" }] })));
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    });
}

/// How long [`press_boot_key`] presses Enter (the prompt waits ~5 s).
const BOOT_KEY_SECS: f64 = 7.0;

/// Stop a VM: QMP quit, then TERM, then KILL.
fn stop_vm(pid: u32, addr: &Addr) {
    if let Ok(mut q) = Qmp::connect(addr, Duration::from_secs(5)) {
        let _ = q.quit();
    }
    for (i, hard) in [(0, false), (1, false), (2, true)] {
        let until = Instant::now() + Duration::from_secs(if i == 0 { 10 } else { 5 });
        while Instant::now() < until {
            if !sys::pid_alive(pid) {
                return;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        if hard {
            sys::kill_hard(pid);
        } else {
            sys::terminate(pid);
        }
    }
}

fn wait_exit(pid: u32, secs: u64) -> bool {
    let until = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < until {
        if !sys::pid_alive(pid) {
            return true;
        }
        if !sys::nap(0.5) {
            return false;
        }
    }
    !sys::pid_alive(pid)
}

/// Install one base image. Exit-code style result: Ok(()) built (or already there), Err with
/// [`BuildError`] code 2 (setup/build error) or 3 (timeout).
pub fn build(env: &Env, b: &Build, qmp_tcp: bool) -> Result<()> {
    let os = b.os;
    let arch = Arch::parse(&os.arch).ok_or_else(|| anyhow!("{}: unknown arch {:?} in lib/oslist.conf", os.key, os.arch))?;
    log(&format!("=== building {} ({}, {}) -> {}.qcow2, computer {} ===", os.display, os.key, arch.as_str(), b.image, b.computer));
    env.check_tools(&[arch])?;
    std::fs::create_dir_all(&env.images_dir).with_context(|| format!("cannot create {}", env.images_dir.display()))?;
    let fin = env.images_dir.join(format!("{}.qcow2", b.image));
    if fin.is_file() {
        ok(&format!("{} already exists", fin.display()));
        return Ok(());
    }
    let partial = env.images_dir.join(format!("{}.qcow2.partial", b.image));
    let iso = match &b.iso {
        Some(p) if p.is_file() => p.clone(),
        Some(p) => bail!("ISO not found: {}", p.display()),
        None => find_iso(&env.home, &env.iso_dir, os).ok_or_else(|| {
            anyhow!(
                "no ISO for {} in {} (build {}, {}; see docs/ISOS.md, `wtl check-iso`)",
                os.display,
                env.iso_dir.display(),
                os.build,
                os.arch
            )
        })?,
    };
    let iso = util::canon(&iso);
    log(&format!("ISO: {}", iso.display()));
    let idx = image_index(&iso, os)?;
    let disk_gb: u64 = os.min_disk_gb.parse().unwrap_or(60);
    let avail = util::free_gb(&env.images_dir);
    if avail.is_some_and(|g| g < 30) {
        warn(&format!("only {} GiB free for {}; an install needs about 20-30 GiB", avail.unwrap_or(0), env.images_dir.display()));
    }

    let run_dir = env.home.runs_dir.join(format!("build-{}", b.image));
    let _ = std::fs::remove_dir_all(&run_dir);
    std::fs::create_dir_all(&run_dir).with_context(|| format!("cannot create {}", run_dir.display()))?;
    let unattend = make_build_disc(&env.home, &run_dir, b, idx, &env.password)?;

    let _ = std::fs::remove_file(&partial);
    let st = std::process::Command::new(&env.qemu_img)
        .args(["create", "-f", "qcow2"])
        .arg(&partial)
        .arg(format!("{}G", disk_gb))
        .stdout(std::process::Stdio::null())
        .status();
    if !st.is_ok_and(|s| s.success()) {
        bail!("qemu-img create {} failed", partial.display());
    }

    // UEFI: a writable copy of the firmware's variable store; it records the Windows Boot
    // Manager entry Setup creates, so it is kept next to the image
    let uefi = os.firmware == "uefi" || arch == Arch::Arm64;
    let vars = if uefi {
        let tmpl = if arch == Arch::Arm64 { &env.aavmf_vars } else { &env.ovmf_vars };
        let v = run_dir.join("vars.fd");
        std::fs::copy(tmpl, &v)
            .with_context(|| format!("UEFI vars template {} not found (install QEMU's UEFI firmware)", tmpl.display()))?;
        Some(v)
    } else {
        None
    };
    let serial = run_dir.join("serial.log");
    write_file(&serial, b"")?;
    let addr = if qmp_tcp {
        Addr::Tcp(format!("127.0.0.1:{}", crate::lab::free_port().context("no free TCP port for QMP")?))
    } else {
        let p = sock_path(run_dir.join("qmp.sock"), &format!("build-{}", b.image));
        let _ = std::fs::remove_file(&p);
        Addr::Unix(p)
    };
    let vnc = if b.show { None } else { Some(pick_vnc_display().context("no free VNC display (ports 5900-5999 all in use)")?) };
    let o = Opts {
        name: format!("wtl-build-{}", b.image),
        mem: std::env::var("WTL_MEM").ok().filter(|m| !m.is_empty()).unwrap_or_else(|| "4096".into()),
        disk: partial.clone(),
        cache: Some("unsafe".into()),
        cds: vec![iso.clone(), unattend],
        serial: Some(serial.clone()),
        qmp: Some(addr.clone()),
        boot_once_d: true,
        show: b.show,
        vnc,
        uefi: vars.clone(),
        arch,
        ..Default::default()
    };
    let argv = qemu::command(env, &o)?;
    let pid = qemu::start(&argv, &run_dir.join("qemu.log"))?;
    write_file(&run_dir.join("qemu.pid"), format!("{}\n", pid))?;
    if uefi {
        press_boot_key(addr.clone(), serial.clone(), pid);
    }
    log(&format!("qemu pid {}; serial log {}", pid, serial.display()));
    if let Some(n) = vnc {
        log(&format!("watch it: vnc://127.0.0.1:{}", 5900 + n));
    }
    if env.accel_for(arch) == qemu::Accel::Tcg {
        warn(&format!(
            "{} runs emulated on this host (no hardware acceleration for {}): expect several times slower",
            b.image,
            arch.as_str()
        ));
    }
    log(&format!("waiting for BUILD_DONE (timeout {} h; Windows Update can take hours)", b.timeout_h));
    let t0 = Instant::now();
    let mut off = 0;
    let w = Wait {
        until: &["BUILD_DONE"],
        fail: &["BUILD_FAIL"],
        timeout: Some(b.timeout_h * 3600.0),
        pid: Some(pid),
        progress: true,
        events_file: None,
    };
    let o = w.run(&serial, &mut off);
    if o == Outcome::Interrupted {
        // Ctrl-C: the VM runs in its own session and would outlive us
        warn(&format!("interrupted - stopping the build VM; partial disk kept: {}", partial.display()));
        stop_vm(pid, &addr);
        return Err(BuildError(2).into());
    }
    let mins = t0.elapsed().as_secs() / 60;
    let fail = match &o {
        Outcome::Until(_) => None,
        Outcome::Fail(l) => Some((2, format!("guest reported BUILD_FAIL: {}", l))),
        Outcome::Timeout => Some((3, format!("timeout after {} h", b.timeout_h))),
        Outcome::Exited => Some((2, format!("qemu exited before BUILD_DONE (see {})", run_dir.join("qemu.log").display()))),
        Outcome::Interrupted => Some((2, "interrupted".to_string())),
    };
    if let Some((code, msg)) = fail {
        err(&msg);
        if sys::pid_alive(pid) {
            let png = run_dir.join("fail.png");
            if Qmp::connect(&addr, Duration::from_secs(5)).and_then(|mut q| q.screendump(&png)).is_ok() {
                log(&format!("screenshot: {}", png.display()));
            }
        }
        stop_vm(pid, &addr);
        err(&format!("build of {} failed; partial disk kept: {} ; logs: {}", b.image, partial.display(), run_dir.display()));
        return Err(BuildError(code).into());
    }
    ok(&format!("guest reported BUILD_DONE after {} min; waiting for shutdown", mins));
    if !wait_exit(pid, 900) {
        warn("guest did not power off within 15 min; sending ACPI powerdown");
        if let Ok(mut q) = Qmp::connect(&addr, Duration::from_secs(5)) {
            let _ = q.cmd("system_powerdown", None);
        }
        if !wait_exit(pid, 300) {
            stop_vm(pid, &addr);
        }
    }
    std::fs::rename(&partial, &fin).with_context(|| format!("cannot rename {} to {}", partial.display(), fin.display()))?;
    if let Some(v) = vars {
        let keep = fin.with_extension("vars.fd");
        std::fs::copy(&v, &keep).with_context(|| format!("cannot keep the UEFI vars as {}", keep.display()))?;
    }
    let meta = serde_json::json!({
        "os": os.key, "arch": arch.as_str(), "iso": iso, "built_at": chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%z").to_string(),
        "updates": b.updates, "image_index": idx, "disk_gb": disk_gb, "computer_name": b.computer,
        "install_minutes": t0.elapsed().as_secs_f64() / 60.0,
    });
    write_file(&env.images_dir.join(format!("{}.json", b.image)), serde_json::to_string_pretty(&meta)?)?;
    let _ = std::fs::remove_dir_all(run_dir.join("stage"));
    ok(&format!(
        "built {} ({}) in {} min",
        fin.display(),
        util::human_size(std::fs::metadata(&fin).map(|m| m.len()).unwrap_or(0)),
        t0.elapsed().as_secs() / 60
    ));
    Ok(())
}

/// A failed build, with its exit code (2 error, 3 timeout).
#[derive(Debug)]
pub struct BuildError(pub i32);
impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "base image build failed (exit {})", self.0)
    }
}
impl std::error::Error for BuildError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_templates() {
        let t = "\u{feff}<a>@X@</a>\n<b arch=\"@ARCH@\"/>\r\n";
        let r = render_template(t, &[("X", "1 &amp; 2".into()), ("ARCH", "arm64".into())]).unwrap();
        assert_eq!(r, "<a>1 &amp; 2</a>\r\n<b arch=\"arm64\"/>\r\n");
        assert!(render_template("<a>@NOPE@</a>", &[]).unwrap_err().to_string().contains("@NOPE@"));
        // an e-mail-like @ is not a placeholder
        assert!(render_template("a@b.c and @x@", &[]).is_ok());
    }

    /// The real answer files: every placeholder is filled for both CPUs.
    #[test]
    fn real_templates_render() {
        for (f, arch) in
            [("autounattend.xml.tmpl", "amd64"), ("autounattend-uefi.xml.tmpl", "amd64"), ("autounattend-uefi.xml.tmpl", "arm64")]
        {
            let t = String::from_utf8(assets::GUEST.iter().find(|(n, _)| *n == f).unwrap().1.to_vec()).unwrap();
            let subs: Vec<(&str, String)> = ["IMAGE_INDEX", "PASSWORD", "COMPUTERNAME", "OS", "PRODUCTKEY"]
                .iter()
                .map(|k| (*k, "v".to_string()))
                .chain([("ARCH", arch.to_string())])
                .collect();
            let r = render_template(&t, &subs).unwrap_or_else(|e| panic!("{}: {}", f, e));
            assert!(r.contains(&format!("processorArchitecture=\"{}\"", arch)) || f == "autounattend.xml.tmpl", "{}", f);
        }
    }

    #[test]
    fn inf_files() {
        let inf = "[Version]\nx=1\n[SourceDisksFiles]\nnetkvm.sys  = 1,,\nnetkvmp.exe = 1,, ; tool\n\n[DestinationDirs]\na = 12\n";
        assert_eq!(inf_source_files(inf), ["netkvm.sys", "netkvmp.exe"]);
    }

    #[test]
    fn catalog_lookup() {
        let d = std::env::temp_dir().join(format!("wtl-cat-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("images-list.conf"), "# c\n2019 = a.iso\nwin11 = x64.iso  # x\nwin11-arm64 = arm.iso\n").unwrap();
        assert_eq!(catalog_iso(&d, "2019").as_deref(), Some("a.iso"));
        assert_eq!(catalog_iso(&d, "win11").as_deref(), Some("x64.iso"));
        assert_eq!(catalog_iso(&d, "win11-arm64").as_deref(), Some("arm.iso"));
        assert_eq!(catalog_iso(&d, "2016"), None);
        let _ = std::fs::remove_dir_all(&d);
    }
}
