//! The OS table lib/oslist.conf and its aliases (port of os_normalize / os_lookup).

use std::path::Path;

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
    pub display: String,
}

/// Canonical key for NAME (aliases: 2012r2, ws2019, server2019, windows11, win11_en_x64 ...).
pub fn normalize(name: &str) -> String {
    let k = name.to_ascii_lowercase();
    let k = match k.as_str() {
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
            display: if f.len() > 9 { f[9..].join(" ") } else { String::new() },
        });
    }
    out
}

/// Look NAME (or an alias of it) up in the table file.
pub fn lookup(oslist: &Path, name: &str) -> Option<OsInfo> {
    let want = normalize(name);
    let text = std::fs::read_to_string(oslist).ok()?;
    parse_table(&text).into_iter().find(|o| o.key == want)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases() {
        assert_eq!(normalize("2012R2"), "2012");
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
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../lib/oslist.conf");
        let o = lookup(&p, "2012r2").unwrap();
        assert_eq!((o.build.as_str(), o.kind.as_str(), o.firmware.as_str()), ("9600", "server", "bios"));
        assert_eq!(o.display, "Windows Server 2012 R2");
        let w = lookup(&p, "win11").unwrap();
        assert_eq!((w.kind.as_str(), w.firmware.as_str(), w.arch.as_str()), ("client", "uefi", "x64"));
        let a = lookup(&p, "win11-arm64").unwrap();
        assert_eq!((a.build.as_str(), a.firmware.as_str(), a.arch.as_str()), ("26300", "uefi", "arm64"));
        assert_eq!(a.display, "Windows 11 Pro on Arm");
        for o in parse_table(&std::fs::read_to_string(&p).unwrap()) {
            assert!(o.arch == "x64" || o.arch == "arm64", "{}: arch {:?}", o.key, o.arch);
            assert!(o.min_disk_gb.parse::<u32>().is_ok(), "{}: min_disk_gb {:?}", o.key, o.min_disk_gb);
        }
        assert!(lookup(&p, "xp").is_none());
    }
}
