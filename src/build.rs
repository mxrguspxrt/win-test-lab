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
use crate::{mkiso, sys, util, vhd, winiso};
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

/// The .vhd for a VHD-import OS: its images-list.conf entry if set, else the newest *.vhd under
/// the ISO folder that is recognised by its content (the unique ID in its footer is one listed
/// for this OS in `lib/vhds.conf`, so a renamed or moved download still works) or, if the file is
/// not one we know, whose path contains the OS's `vhd:<hint>` (case-insensitive). A VHD known to
/// be another OS's is never used. Microsoft ships Server 2003 / 2008 R2 as VHD Test Drive images;
/// see docs/ISOS.md.
pub fn find_vhd(home: &Home, iso_dir: &Path, os: &OsInfo) -> Option<PathBuf> {
    if let Some(c) = catalog_iso(&home.dir, &os.key) {
        let p = if Path::new(&c).is_absolute() { PathBuf::from(&c) } else { iso_dir.join(&c) };
        if p.is_file() {
            return Some(p);
        }
    }
    let hint = os.vhd_hint().to_ascii_lowercase();
    let known = vhd::parse_catalog(&assets::vhds(home));
    let mut vhds: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    let mut stack = vec![iso_dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("vhd")) {
                let by_name = hint.is_empty() || p.to_string_lossy().to_ascii_lowercase().contains(&hint);
                let fits = match vhd::unique_id(&p).and_then(|id| vhd::owner(&known, &id).map(str::to_string)) {
                    Some(owner) => owner == os.key,
                    None => by_name,
                };
                if fits {
                    let t = std::fs::metadata(&p).and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
                    vhds.push((t, p));
                }
            }
        }
    }
    vhds.sort_by_key(|(t, _)| std::cmp::Reverse(*t));
    vhds.into_iter().map(|(_, p)| p).next()
}

/// Merge the OS's `guest/<os>/image.reg`, if it ships one, into the imported image's offline
/// registry with libguestfs `virt-win-reg --merge`. Server 2003's Test Drive VHD, made on
/// Hyper-V, only lists Hyper-V's IDE controller as boot-critical and stops with STOP 0x7B on
/// QEMU's, so its image.reg adds QEMU's (and the `Run` entry of the build launcher). No-op for an
/// OS without one.
fn merge_image_registry(home: &Home, os: &OsInfo, image: &Path, run_dir: &Path) -> Result<()> {
    let Some(bytes) = assets::guest_file_os_opt(home, &os.key, "image.reg")? else { return Ok(()) };
    let text = String::from_utf8(bytes).context("image.reg is not UTF-8")?;
    let local = run_dir.join("image.reg");
    write_file(&local, text.replace("\r\n", "\n").replace('\n', "\r\n"))?;
    if util::which("virt-win-reg").is_none() {
        bail!("virt-win-reg (libguestfs) is needed to prepare {} but was not found: sudo apt install libguestfs-tools", os.display);
    }
    log("merging registry settings into the image (virt-win-reg): the IDE controller QEMU emulates, the build launcher");
    let out = std::process::Command::new("virt-win-reg")
        .arg("--merge")
        .arg(image)
        .arg(&local)
        .env("LIBGUESTFS_BACKEND", "direct")
        .output()
        .context("running virt-win-reg")?;
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    for l in text.lines().filter(|l| !l.trim().is_empty()) {
        log(&format!("  virt-win-reg: {}", l));
    }
    if !out.status.success() {
        bail!("virt-win-reg --merge failed on {} (exit {:?})", image.display(), out.status.code());
    }
    Ok(())
}

/// An imported image's own `C:\sysprep\sysprep.inf` (libguestfs `virt-cat`), if it has one. The
/// 2003 Test Drive VHD is prepared with its evaluation product key and its Administrator
/// password there; our replacement answer file must carry both over (see `inf_value`).
fn read_image_sysprep_inf(image: &Path) -> Option<String> {
    let out = std::process::Command::new("virt-cat")
        .arg("-a")
        .arg(image)
        .arg("/sysprep/sysprep.inf")
        .env("LIBGUESTFS_BACKEND", "direct")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The value of `KEY=` in sysprep.inf text (case-insensitive key; surrounding spaces and quotes
/// dropped); None if there is no such line or it is empty.
fn inf_value(text: &str, key: &str) -> Option<String> {
    for line in text.lines() {
        let l = line.trim();
        if let Some((k, v)) = l.split_once('=') {
            if k.trim().eq_ignore_ascii_case(key) {
                let v = v.trim().trim_matches('"').trim();
                return if v.is_empty() { None } else { Some(v.to_string()) };
            }
        }
    }
    None
}

/// The imported Test Drive VHDs are sysprepped: on first boot they run mini-setup / OOBE and
/// wait for input. Each VHD OS ships an answer file that drives that unattended AND launches
/// win-test-lab's setup from the WTL disc (2008 R2: an `unattend.xml` with `<FirstLogonCommands>`,
/// read from `%WINDIR%\Panther\unattend.xml`; 2003: a `sysprep.inf` read from
/// `C:\sysprep\sysprep.inf`, plus a launcher `C:\wtl-first.cmd` that `image.reg` puts under
/// `HKLM\...\Run`). We inject it with libguestfs `virt-customize --upload`; no running service or
/// helper (rhsrvany) is needed. `@PASSWORD@` in the template is the build password.
fn inject_oobe(home: &Home, os: &OsInfo, image: &Path, run_dir: &Path, password: &str, computer: &str) -> Result<()> {
    // (template name, guest destination, is XML) for the answer file this OS ships
    let candidates = [("sysprep.inf.tmpl", "/sysprep/sysprep.inf", false), ("oobe.xml.tmpl", "/Windows/Panther/unattend.xml", true)];
    let mut answer: Option<(PathBuf, &str)> = None;
    for (tmpl, dest, is_xml) in candidates {
        if let Some(bytes) = assets::guest_file_os_opt(home, &os.key, tmpl)? {
            let text = String::from_utf8(bytes).with_context(|| format!("{} is not UTF-8", tmpl))?;
            // an XML answer file needs the password escaped; sysprep.inf takes it verbatim
            let pw = if is_xml { xml_escape(password) } else { password.to_string() };
            let mut subs = vec![("PASSWORD", pw), ("COMPUTERNAME", computer.to_string())];
            if text.contains("@PRODUCTKEY@") || text.contains("@IMAGEPASSWORD@") {
                // keep what the image was prepared with, read from its own sysprep.inf rather
                // than shipped here: its product key (an empty key line is dropped below) and
                // its Administrator password. Mini-setup "changes" that password assuming the old
                // one is blank, which fails for any other, so the account keeps this password and
                // the first automatic logon has to use it; the build script sets ours afterwards.
                let existing = read_image_sysprep_inf(image).unwrap_or_default();
                subs.push(("PRODUCTKEY", inf_value(&existing, "ProductKey").unwrap_or_default()));
                subs.push(("IMAGEPASSWORD", inf_value(&existing, "AdminPassword").unwrap_or_else(|| password.to_string())));
            }
            let mut rendered = render_template(&text, &subs)?;
            if rendered.contains("ProductKey=\r\n") {
                rendered = rendered.replace("    ProductKey=\r\n", "").replace("ProductKey=\r\n", "");
            }
            let local = run_dir.join(tmpl.trim_end_matches(".tmpl"));
            write_file(&local, rendered)?;
            answer = Some((local, dest));
            break;
        }
    }
    let Some((local, dest)) = answer else {
        bail!("{} is a VHD-import OS but ships no OOBE answer file (guest/{}/sysprep.inf.tmpl or oobe.xml.tmpl)", os.key, os.key);
    };
    // an optional launcher script that runs at the first logon (2003: registered under
    // HKLM\...\Run by image.reg); it lives in the root of C:, as mini-setup deletes C:\sysprep
    let launcher = match assets::guest_file_os_opt(home, &os.key, "firstrun.cmd")? {
        Some(bytes) => {
            let p = run_dir.join("firstrun.cmd");
            write_file(&p, bytes)?;
            Some(p)
        }
        None => None,
    };

    if util::which("virt-customize").is_none() {
        bail!(
            "virt-customize (libguestfs) is needed to prepare a VHD-import image but was not found. \
             Install it and run again:\n    Debian/Ubuntu: sudo apt install libguestfs-tools\n    \
             Fedora: sudo dnf install guestfs-tools\nIt drops win-test-lab's unattended answer file \
             into the imported Windows so its first boot runs setup; nothing else uses it."
        );
    }
    log(&format!("staging the OOBE answer file ({}) with virt-customize", dest));
    let destdir = dest.rsplit_once('/').map(|(d, _)| d).unwrap_or("/");
    let mut cmd = std::process::Command::new("virt-customize");
    cmd.arg("-a").arg(image).arg("--mkdir").arg(destdir).arg("--upload").arg(format!("{}:{}", local.display(), dest));
    if let Some(l) = &launcher {
        cmd.arg("--upload").arg(format!("{}:/wtl-first.cmd", l.display()));
    }
    let out = cmd.env("LIBGUESTFS_BACKEND", "direct").output().context("running virt-customize")?;
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    for l in text.lines() {
        log(&format!("  virt-customize: {}", l));
    }
    if !out.status.success() {
        // the usual libguestfs failure on Debian/Ubuntu: supermin cannot read the host kernel
        // (/boot/vmlinuz-* is mode 0600), so it cannot build its appliance
        let kernel_unreadable =
            text.contains("supermin") || text.contains("/boot/vmlinuz") || text.contains("febootstrap") || text.contains("appliance");
        let hint = if kernel_unreadable {
            "\nThis is almost always the host kernel being unreadable to non-root on Debian/Ubuntu \
             (/boot/vmlinuz-* is mode 0600), so libguestfs's supermin cannot build its appliance. Fix it with:\n    \
             sudo chmod 0644 /boot/vmlinuz-*\n(./install-deps.sh does this for you.) Then run `wtl build-image` again."
        } else {
            ""
        };
        bail!("virt-customize failed on {} (exit {:?}).{}", image.display(), out.status.code(), hint);
    }
    Ok(())
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
    // An ISO install needs the Windows Setup answer file; a VHD import is already installed, so
    // its OOBE answer file (injected into the image, see inject_oobe) runs setup instead.
    if !os.is_vhd() {
        // UEFI machines (Win11, every arm64 machine) get the GPT layout + the Win11 check bypass
        let uefi = os.firmware == "uefi" || arch == Arch::Arm64;
        let tmpl = if uefi { "autounattend-uefi.xml.tmpl" } else { "autounattend.xml.tmpl" };
        let text = String::from_utf8(assets::guest_file(home, tmpl)?).with_context(|| format!("{} is not UTF-8", tmpl))?;
        // Client (Win10/11) retail media refuse to go on without a key; evaluation Server media
        // must NOT get one. Microsoft's published generic Pro key installs, does not activate.
        let prodkey = if os.kind == "client" {
            let pk =
                std::env::var("WTL_PRODUCT_KEY").ok().filter(|k| !k.is_empty()).unwrap_or_else(|| "W269N-WFGWX-YVC9B-4J6C9-T83GX".into());
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
    }
    // stage this OS's guest scripts: its own guest/<key>/ folder (self-contained, any language -
    // PowerShell for 2008 R2, VBScript + batch for 2003) or the shared top-level *.ps1
    let staged = assets::guest_scripts_for(home, &os.key)?;
    for (name, data) in &staged {
        write_file(&stage.join("WTL").join(name), data.clone())?;
    }
    // every OS needs a setup.* (build phase) and agent.* (logon), whatever the extension
    for stem in ["setup.", "agent."] {
        if !staged.iter().any(|(n, _)| n.starts_with(stem)) {
            bail!("no {}* guest script for {} (checked guest/{}/ and the shared scripts)", stem, os.key, os.key);
        }
    }
    // drivers Windows does not ship for this machine's devices (Windows on Arm has none for any
    // network card or serial port QEMU emulates): Setup installs everything under
    // \$WinPEDriver$ on any attached disc, in WinPE and into the installed system
    let n = stage_drivers(&home.drivers_dir, arch, &stage.join("$WinPEDriver$"))?;
    if n > 0 {
        log(&format!("drivers for {}: {} (from {}, see docs/ISOS.md)", arch.as_str(), n, home.drivers_dir.display()));
    }
    let cfg = serde_json::json!({ "os": os.key, "updates": b.updates, "password": password, "computer": b.computer });
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

/// Some imported images stop in their first-boot setup at a dialog no answer file can skip:
/// Server 2003's evaluation edition shows "This is an evaluation copy of Windows..." and then
/// the Date and Time page. Press Enter over QMP every few seconds until the guest has written
/// its first line to the serial log (that comes from its build script, which runs only after
/// those dialogs), or the VM is gone. Enter on the desktop is harmless; the presses stop as soon
/// as the guest speaks.
fn press_enter_until_guest(addr: Addr, serial: PathBuf, pid: u32) {
    std::thread::spawn(move || {
        let t0 = Instant::now();
        while sys::pid_alive(pid) && t0.elapsed() < Duration::from_secs(3600) {
            if std::fs::metadata(&serial).map(|m| m.len()).unwrap_or(0) > 0 {
                return;
            }
            if let Ok(mut q) = Qmp::connect(&addr, Duration::from_secs(2)) {
                let _ = q.cmd("send-key", Some(serde_json::json!({ "keys": [{ "type": "qcode", "data": "ret" }] })));
            }
            std::thread::sleep(Duration::from_secs(5));
        }
    });
}

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
    let disk_gb: u64 = os.min_disk_gb.parse().unwrap_or(60);
    let avail = util::free_gb(&env.images_dir);
    if avail.is_some_and(|g| g < 30) {
        warn(&format!("only {} GiB free for {}; an install needs about 20-30 GiB", avail.unwrap_or(0), env.images_dir.display()));
    }
    let run_dir = env.home.runs_dir.join(format!("build-{}", b.image));
    let _ = std::fs::remove_dir_all(&run_dir);
    std::fs::create_dir_all(&run_dir).with_context(|| format!("cannot create {}", run_dir.display()))?;
    let _ = std::fs::remove_file(&partial);

    // Two ways to make the base image: install from a Windows ISO (2012+), or import a
    // pre-installed .vhd (Server 2003 / 2008 R2, which have no usable free install ISO). Each
    // produces `partial` and the list of extra CDs to boot with; then the boot/wait is shared.
    let installer_iso: Option<PathBuf>; // the Windows ISO that boots first (ISO install only)
    let image_source: PathBuf; // the ISO or VHD the image was made from (recorded in <image>.json)
    let idx: u32; // the WIM image index (ISO install); 0 for a VHD import (no WIM)
    if os.is_vhd() {
        let vhd = match &b.iso {
            Some(p) if p.is_file() => p.clone(),
            Some(p) => bail!("VHD not found: {}", p.display()),
            None => find_vhd(&env.home, &env.iso_dir, os).ok_or_else(|| {
                let url = vhd::download_url(&vhd::parse_catalog(&assets::vhds(&env.home)), &os.key)
                    .map(|u| format!("\nDownload it from {} and unpack it into that folder.", u))
                    .unwrap_or_default();
                anyhow!(
                    "no .vhd for {} in {} (a download we know, or a file whose path contains {:?}; see docs/ISOS.md).{}",
                    os.display,
                    env.iso_dir.display(),
                    os.vhd_hint(),
                    url
                )
            })?,
        };
        let vhd = util::canon(&vhd);
        log(&format!("VHD: {} (importing, then first boot runs setup)", vhd.display()));
        make_build_disc(&env.home, &run_dir, b, 0, &env.password)?;
        log(&format!("converting the VHD to {} (may take a few minutes)", partial.display()));
        let st = std::process::Command::new(&env.qemu_img)
            .args(["convert", "-O", "qcow2"])
            .arg(&vhd)
            .arg(&partial)
            .status()
            .context("running qemu-img convert")?;
        if !st.success() {
            bail!("qemu-img convert {} -> {} failed", vhd.display(), partial.display());
        }
        // grow the image to the lab's disk size (the Test Drive VHDs are small)
        let _ = std::process::Command::new(&env.qemu_img)
            .arg("resize")
            .arg(&partial)
            .arg(format!("{}G", disk_gb))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        inject_oobe(&env.home, os, &partial, &run_dir, &env.password, b.computer)?;
        merge_image_registry(&env.home, os, &partial, &run_dir)?;
        installer_iso = None;
        image_source = vhd;
        idx = 0;
    } else {
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
        idx = image_index(&iso, os)?;
        make_build_disc(&env.home, &run_dir, b, idx, &env.password)?;
        let st = std::process::Command::new(&env.qemu_img)
            .args(["create", "-f", "qcow2"])
            .arg(&partial)
            .arg(format!("{}G", disk_gb))
            .stdout(std::process::Stdio::null())
            .status();
        if !st.is_ok_and(|s| s.success()) {
            bail!("qemu-img create {} failed", partial.display());
        }
        image_source = iso.clone();
        installer_iso = Some(iso);
    }
    // the build/control disc produced by make_build_disc above
    let unattend = run_dir.join("unattend.iso");
    // the extra CDs: for an ISO install the Windows ISO boots first, then the build disc; for a
    // VHD import only the build disc (the image boots from its own disk)
    let cds: Vec<PathBuf> = match &installer_iso {
        Some(iso) => vec![iso.clone(), unattend],
        None => vec![unattend],
    };
    let boot_d = installer_iso.is_some();

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
        cds,
        serial: Some(serial.clone()),
        qmp: Some(addr.clone()),
        boot_once_d: boot_d,
        show: b.show,
        vnc,
        disk_ide: os.is_vhd(),
        no_usb: os.no_usb(),
        uefi: vars.clone(),
        arch,
        ..Default::default()
    };
    let argv = qemu::command(env, &o)?;
    let pid = qemu::start(&argv, &run_dir.join("qemu.log"))?;
    write_file(&run_dir.join("qemu.pid"), format!("{}\n", pid))?;
    // only the UEFI "press any key to boot from CD" prompt needs the keypress, and only when an
    // installer ISO boots first (never for a VHD import, which boots from its own disk)
    if uefi && boot_d {
        press_boot_key(addr.clone(), serial.clone(), pid);
    }
    if os.is_vhd() && os.press_enter() {
        press_enter_until_guest(addr.clone(), serial.clone(), pid);
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
        "os": os.key, "arch": arch.as_str(), "iso": image_source, "built_at": chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%z").to_string(),
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
    fn values_from_sysprep_inf() {
        let inf = "[GuiUnattended]\r\n    AdminPassword=\"Example-Pass-1\"\r\n[UserData]\r\n    ProductKey=AAAAA-BBBBB-CCCCC-DDDDD-EEEEE\r\n    FullName=\"x\"\r\n";
        assert_eq!(inf_value(inf, "ProductKey").as_deref(), Some("AAAAA-BBBBB-CCCCC-DDDDD-EEEEE"));
        assert_eq!(inf_value(inf, "AdminPassword").as_deref(), Some("Example-Pass-1"));
        assert_eq!(inf_value(inf, "FullName").as_deref(), Some("x"));
        assert_eq!(inf_value("productkey = \"K1-K2\" \n", "ProductKey").as_deref(), Some("K1-K2"));
        assert_eq!(inf_value("[UserData]\nProductKey=\n", "ProductKey"), None);
        assert_eq!(inf_value("[UserData]\nFullName=x\n", "ProductKey"), None);
    }

    #[test]
    fn inf_files() {
        let inf = "[Version]\nx=1\n[SourceDisksFiles]\nnetkvm.sys  = 1,,\nnetkvmp.exe = 1,, ; tool\n\n[DestinationDirs]\na = 12\n";
        assert_eq!(inf_source_files(inf), ["netkvm.sys", "netkvmp.exe"]);
    }

    /// A file shaped like a VHD: zeros, then a footer carrying this unique ID.
    fn put_vhd(path: &Path, id: &[u8; 16]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut footer = vec![0u8; 512];
        footer[..8].copy_from_slice(b"conectix");
        footer[68..84].copy_from_slice(id);
        let mut data = vec![0u8; 1024];
        data.extend(footer);
        std::fs::write(path, data).unwrap();
    }

    /// The unique ID the shipped lib/vhds.conf lists for OS KEY, as bytes.
    fn known_id(key: &str) -> [u8; 16] {
        let known = vhd::parse_catalog(assets::VHDS);
        let hex = &known.iter().find(|k| k.key == key).unwrap().id;
        let mut b = [0u8; 16];
        for (i, x) in b.iter_mut().enumerate() {
            *x = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap();
        }
        b
    }

    /// A Test Drive VHD is found by what is inside it, whatever it is called or wherever it is;
    /// an unknown one only by its path; one known for another OS never.
    #[test]
    fn find_vhd_by_content() {
        let root = std::env::temp_dir().join(format!("wtl-findvhd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let home = Home::fixed(&root);
        let os3 = crate::oslist::lookup(assets::OSLIST, "2003").unwrap();
        let os8 = crate::oslist::lookup(assets::OSLIST, "2008").unwrap();
        let (id3, id8) = (known_id("2003"), known_id("2008"));

        // nothing there
        let a = root.join("a");
        std::fs::create_dir_all(&a).unwrap();
        assert_eq!(find_vhd(&home, &a, &os3), None);

        // a known download, renamed and buried: found for its own OS only
        let b = root.join("b");
        put_vhd(&b.join("x/y/renamed.vhd"), &id3);
        assert_eq!(find_vhd(&home, &b, &os3), Some(b.join("x/y/renamed.vhd")));
        assert_eq!(find_vhd(&home, &b, &os8), None);

        // an unknown VHD counts by its path alone
        let c = root.join("c");
        put_vhd(&c.join("Win2k3-other/disk.vhd"), &[7u8; 16]);
        put_vhd(&c.join("misc/disk.vhd"), &[8u8; 16]);
        assert_eq!(find_vhd(&home, &c, &os3), Some(c.join("Win2k3-other/disk.vhd")));
        assert_eq!(find_vhd(&home, &c, &os8), None);

        // 2008's download under a path that says Win2k3 is still 2008's
        let d = root.join("d");
        put_vhd(&d.join("Win2k3/really-2008.vhd"), &id8);
        assert_eq!(find_vhd(&home, &d, &os3), None);
        assert_eq!(find_vhd(&home, &d, &os8), Some(d.join("Win2k3/really-2008.vhd")));
        let _ = std::fs::remove_dir_all(&root);
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
