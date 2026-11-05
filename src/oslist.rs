//! The OS table lib/oslist.conf and its aliases.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OsInfo {
    pub key: String,
    pub build: String,
    pub edition: String,
    /// "Server", "Server Core", "Client" (underscores in the file mean spaces)
    pub inst_type: String,
    /// "server" (can be a DC) or "client" (Win10/11)
    pub kind: String,
    /// "bios" or "uefi"
    pub firmware: String,
    /// "x64" or "arm64": the QEMU binary and machine, and the ISO it installs from
    pub arch: String,
    pub fallback_index: String,
    pub min_disk_gb: String,
    /// how the base image is made: "iso" (install from an ISO, the default) or "vhd:<hint>"
    /// (import a pre-installed .vhd whose path contains <hint>, for OSes with no usable install
    /// ISO: Server 2003, 2008 R2). See [`OsInfo::vhd_hint`]. A vhd source can carry `+option`s:
    /// `+enter` ([`OsInfo::press_enter`]), `+nousb` ([`OsInfo::no_usb`]).
    pub source: String,
    pub display: String,
}

impl OsInfo {
    /// The base image is imported from a .vhd rather than installed from an ISO.
    pub fn is_vhd(&self) -> bool {
        self.source.starts_with("vhd")
    }

    /// For a "vhd:<hint>[+option...]" source, the <hint> (a substring of the .vhd's path); empty if none.
    pub fn vhd_hint(&self) -> &str {
        let s = self.source.strip_prefix("vhd:").unwrap_or("");
        s.split('+').next().unwrap_or("")
    }

    /// The source says "+enter": the image shows a dialog during its first-boot setup that no
    /// answer file can skip (Server 2003's evaluation edition), so `wtl build-image` presses Enter
    /// over QMP until the guest reports in.
    pub fn press_enter(&self) -> bool {
        self.has_option("enter")
    }

    /// The source says "+nousb": the guest hangs at its first boot after setup when QEMU gives it
    /// a USB controller (Server 2003 stops at an empty Winlogon screen), so its VMs get none
    /// (PS/2 keyboard and mouse only, no usb-tablet).
    pub fn no_usb(&self) -> bool {
        self.has_option("nousb")
    }

    fn has_option(&self, name: &str) -> bool {
        self.source.split('+').skip(1).any(|o| o == name)
    }
}

/// Canonical key for NAME (aliases: 2012r2, ws2019, server2019, windows11, win11_en_x64 ...).
pub fn normalize(name: &str) -> String {
    let k = name.to_ascii_lowercase();
    let k = match k.as_str() {
        "2003r2"
        | "2003-r2"
        | "2003_r2"
        | "win2003"
        | "win2003r2"
        | "server2003"
        | "ws2003"
        | "windowsserver2003"
        | "windowsserver2003r2" => "2003".to_string(),
        "2008r2"
        | "2008-r2"
        | "2008_r2"
        | "win2008"
        | "win2008r2"
        | "server2008"
        | "server2008r2"
        | "ws2008"
        | "ws2008r2"
        | "windowsserver2008r2" => "2008".to_string(),
        "2012r2" | "2012-r2" | "2012_r2" | "win2012r2" | "windowsserver2012r2" => "2012".to_string(),
        "win2016" | "server2016" | "ws2016" => "2016".into(),
        "win2019" | "server2019" | "ws2019" => "2019".into(),
        "win2022" | "server2022" | "ws2022" => "2022".into(),
        "win2025" | "server2025" | "ws2025" => "2025".into(),
        "windows10" | "windows_10" => "win10".into(),
        "windows11" | "windows_11" => "win11".into(),
        _ => k,
    };
    // variant aliases fold to the base client key: win11_en_x64 -> win11, but anything that
    // names arm64 (win11_arm64, win11_en_arm64, windows11-arm64) -> win11-arm64
    let arm = k.contains("arm64") || k.contains("aarch64");
    if (k.starts_with("win11") || k.starts_with("windows11") || k.starts_with("windows_11")) && arm {
        "win11-arm64".into()
    } else if k.starts_with("win10_") {
        "win10".into()
    } else if k.starts_with("win11_") {
        "win11".into()
    } else {
        k
    }
}

/// Every row of the table text.
pub fn parse_table(text: &str) -> Vec<OsInfo> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("");
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.is_empty() {
            continue;
        }
        let col = |i: usize| f.get(i).copied().unwrap_or("").to_string();
        out.push(OsInfo {
            key: col(0),
            build: col(1),
            edition: col(2),
            inst_type: col(3).replace('_', " "),
            kind: col(4),
            firmware: col(5),
            arch: col(6),
            fallback_index: col(7),
            min_disk_gb: col(8),
            source: {
                let s = col(9);
                if s.is_empty() {
                    "iso".to_string()
                } else {
                    s
                }
            },
            display: if f.len() > 10 { f[10..].join(" ") } else { String::new() },
        });
    }
    out
}

/// Look NAME (or an alias of it) up in the table text.
pub fn lookup(table: &str, name: &str) -> Option<OsInfo> {
    let want = normalize(name);
    parse_table(table).into_iter().find(|o| o.key == want)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases() {
        assert_eq!(normalize("2012R2"), "2012");
        assert_eq!(normalize("2003R2"), "2003");
        assert_eq!(normalize("WindowsServer2003R2"), "2003");
        assert_eq!(normalize("2008r2"), "2008");
        assert_eq!(normalize("ws2008r2"), "2008");
        assert_eq!(normalize("ws2019"), "2019");
        assert_eq!(normalize("Windows11"), "win11");
        assert_eq!(normalize("win10_de_arm64"), "win10");
        assert_eq!(normalize("2019"), "2019");
        assert_eq!(normalize("win11-arm64"), "win11-arm64");
        assert_eq!(normalize("win11_en_arm64"), "win11-arm64");
        assert_eq!(normalize("Windows11-ARM64"), "win11-arm64");
        assert_eq!(normalize("win11_en_x64"), "win11");
    }

    #[test]
    fn real_table() {
        let p = crate::assets::OSLIST;
        let o = lookup(p, "2012r2").unwrap();
        assert_eq!((o.build.as_str(), o.kind.as_str(), o.firmware.as_str()), ("9600", "server", "bios"));
        assert_eq!(o.display, "Windows Server 2012 R2");
        assert_eq!(o.source, "iso");
        assert!(!o.is_vhd());
        // the VHD-import server OSes
        let v = lookup(p, "2008").unwrap();
        assert_eq!((v.kind.as_str(), v.firmware.as_str(), v.arch.as_str()), ("server", "bios", "x64"));
        assert!(v.is_vhd() && v.vhd_hint() == "WS2008R2", "{:?}", v.source);
        assert_eq!(v.display, "Windows Server 2008 R2");
        let v3 = lookup(p, "2003").unwrap();
        assert!(v3.is_vhd() && v3.vhd_hint() == "Win2k3", "{:?}", v3.source);
        assert!(v3.press_enter() && !v.press_enter(), "only 2003 needs Enter pressed ({:?} / {:?})", v3.source, v.source);
        let no_usb: Vec<String> = parse_table(p).into_iter().filter(|o| o.no_usb()).map(|o| o.key).collect();
        assert_eq!(no_usb, ["2003"], "only 2003 runs without USB");
        assert_eq!(v3.vhd_hint(), "Win2k3", "options after the hint are not part of it");
        assert_eq!(v3.display, "Windows Server 2003 R2");
        let w = lookup(p, "win11").unwrap();
        assert_eq!((w.kind.as_str(), w.firmware.as_str(), w.arch.as_str()), ("client", "uefi", "x64"));
        let a = lookup(p, "win11-arm64").unwrap();
        assert_eq!((a.build.as_str(), a.firmware.as_str(), a.arch.as_str()), ("26300", "uefi", "arm64"));
        assert_eq!(a.display, "Windows 11 Pro on Arm");
        for o in parse_table(p) {
            assert!(o.arch == "x64" || o.arch == "arm64", "{}: arch {:?}", o.key, o.arch);
            assert!(o.min_disk_gb.parse::<u32>().is_ok(), "{}: min_disk_gb {:?}", o.key, o.min_disk_gb);
        }
        assert!(lookup(p, "xp").is_none());
    }
}
