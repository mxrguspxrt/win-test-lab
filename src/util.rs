//! Small string and path helpers. Several mirror the Python standard library (str.strip,
//! int(), os.path.normpath, urllib.parse.unquote), because the serial protocol and the ISO
//! writer must match the Python tools build-image.sh / launch-test.sh still use; plus
//! `du`- and `which`-like helpers.

use std::path::{Component, Path, PathBuf};

/// Python's `str.isspace` for one character (Rust's White_Space plus U+001C..U+001F).
pub fn py_isspace(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// Python's `str.strip()`.
pub fn py_strip(s: &str) -> &str {
    s.trim_matches(py_isspace)
}

/// Python's `int(str)` for base-10 ASCII input (sign, surrounding whitespace, `1_000`).
pub fn py_int(s: &str) -> Option<i64> {
    let s = py_strip(s);
    let (neg, digits) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    if digits.is_empty() || digits.starts_with('_') || digits.ends_with('_') || digits.contains("__") {
        return None;
    }
    let mut v: i64 = 0;
    for b in digits.bytes() {
        match b {
            b'_' => continue,
            b'0'..=b'9' => v = v.checked_mul(10)?.checked_add((b - b'0') as i64)?,
            _ => return None,
        }
    }
    Some(if neg { -v } else { v })
}

/// `^[A-Za-z0-9._-]+$`
pub fn is_safe_name(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
}

/// Lexical `os.path.normpath` (no symlink resolution).
pub fn normpath(p: &Path) -> PathBuf {
    let mut out: Vec<Component> = Vec::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => match out.last() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                Some(Component::RootDir) | Some(Component::Prefix(_)) => {}
                _ => out.push(c),
            },
            _ => out.push(c),
        }
    }
    if out.is_empty() {
        return PathBuf::from(".");
    }
    out.iter().collect()
}

/// `os.path.abspath`: join with the current directory, then normpath.
pub fn abspath(p: &Path) -> PathBuf {
    if p.is_absolute() {
        normpath(p)
    } else {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        normpath(&cwd.join(p))
    }
}

/// `readlink -f` for an existing path; on Windows the `\\?\` prefix is dropped again so the
/// result can be handed to QEMU.
pub fn canon(p: &Path) -> PathBuf {
    match std::fs::canonicalize(p) {
        Ok(c) => strip_verbatim(c),
        Err(_) => abspath(p),
    }
}

#[cfg(windows)]
fn strip_verbatim(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        if rest.as_bytes().get(1) == Some(&b':') {
            return PathBuf::from(rest);
        }
    }
    p
}

#[cfg(not(windows))]
fn strip_verbatim(p: PathBuf) -> PathBuf {
    p
}

fn is_sep(c: char) -> bool {
    c == '/' || (cfg!(windows) && c == '\\')
}

/// `os.path.basename` (text after the last separator; "" for a trailing separator).
pub fn basename(p: &str) -> &str {
    p.rsplit(is_sep).next().unwrap_or(p)
}

/// `os.path.splitext(name)[0]`; leading dots do not start an extension.
pub fn stem(name: &str) -> &str {
    if let Some(i) = name.rfind('.') {
        if name[..i].chars().any(|c| c != '.') {
            return &name[..i];
        }
    }
    name
}

/// Python-style `s[:n]` on characters; a negative n counts from the end.
pub fn py_slice_to(s: &str, n: isize) -> String {
    let len = s.chars().count() as isize;
    let end = if n < 0 { (len + n).max(0) } else { n.min(len) };
    s.chars().take(end as usize).collect()
}

/// `urllib.parse.unquote` (UTF-8, invalid sequences replaced, a stray '%' kept).
pub fn url_unquote(s: &str) -> String {
    percent_encoding::percent_decode_str(s).decode_utf8_lossy().into_owned()
}

/// `du -h`-style size: powers of 1024, rounded up, one decimal below 10.
pub fn human_size(bytes: u64) -> String {
    if bytes == 0 {
        return "0".into();
    }
    let units = ["K", "M", "G", "T", "P"];
    let mut v = bytes as f64 / 1024.0;
    let mut u = 0;
    while v.ceil() >= 1024.0 && u + 1 < units.len() {
        v /= 1024.0;
        u += 1;
    }
    let tenths = (v * 10.0).ceil() / 10.0;
    if tenths < 10.0 {
        format!("{:.1}{}", tenths, units[u])
    } else {
        format!("{}{}", v.ceil() as u64, units[u])
    }
}

/// Disk space actually used by a file or a whole tree (like `du`; symlinks not followed).
pub fn disk_usage(p: &Path) -> u64 {
    let md = match std::fs::symlink_metadata(p) {
        Ok(m) => m,
        Err(_) => return 0,
    };
    let mut n = allocated(&md);
    if md.is_dir() {
        if let Ok(rd) = std::fs::read_dir(p) {
            for e in rd.flatten() {
                n += disk_usage(&e.path());
            }
        }
    }
    n
}

#[cfg(unix)]
fn allocated(md: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    md.blocks() * 512
}

#[cfg(not(unix))]
fn allocated(md: &std::fs::Metadata) -> u64 {
    md.len()
}

/// Whole GiB free on the file system holding P (its nearest existing ancestor); None where
/// it cannot be read.
pub fn free_gb(p: &Path) -> Option<u64> {
    let d = p.ancestors().find(|a| a.is_dir())?;
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let c = std::ffi::CString::new(d.as_os_str().as_bytes()).ok()?;
        let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
            return None;
        }
        Some((st.f_bavail as u64).saturating_mul(st.f_frsize as u64) >> 30)
    }
    #[cfg(not(unix))]
    {
        let _ = d;
        None
    }
}

/// Find an executable like `command -v` (PATH search; PATHEXT on Windows).
pub fn which(name: &str) -> Option<PathBuf> {
    let exts: Vec<String> = if cfg!(windows) {
        let pe = std::env::var("PATHEXT").unwrap_or_else(|_| ".EXE;.CMD;.BAT;.COM".into());
        std::iter::once(String::new()).chain(pe.split(';').map(|s| s.to_string())).collect()
    } else {
        vec![String::new()]
    };
    let try_path = |p: &Path| -> Option<PathBuf> {
        for e in &exts {
            let mut c = p.as_os_str().to_owned();
            c.push(e);
            let c = PathBuf::from(c);
            if is_executable(&c) {
                return Some(c);
            }
        }
        None
    };
    if name.contains('/') || (cfg!(windows) && name.contains('\\')) {
        return try_path(Path::new(name));
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|d| try_path(&d.join(name)))
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn int_like_python() {
        assert_eq!(py_int(" 60 "), Some(60));
        assert_eq!(py_int("-3"), Some(-3));
        assert_eq!(py_int("+1_000"), Some(1000));
        assert_eq!(py_int(""), None);
        assert_eq!(py_int("1__0"), None);
        assert_eq!(py_int("1.5"), None);
    }

    #[test]
    fn paths() {
        assert_eq!(normpath(Path::new("/a/b/../c/./d/")), PathBuf::from("/a/c/d"));
        assert_eq!(normpath(Path::new("/../x")), PathBuf::from("/x"));
        assert_eq!(normpath(Path::new("../x/..")), PathBuf::from(".."));
        assert_eq!(basename("/x/y/z.conf"), "z.conf");
        assert_eq!(stem("z.conf"), "z");
        assert_eq!(stem(".conf"), ".conf");
        assert_eq!(stem("a.b.conf"), "a.b");
        assert_eq!(py_slice_to("abcdef", -2), "abcd");
        assert_eq!(py_slice_to("abc", 10), "abc");
    }

    #[test]
    fn url() {
        assert_eq!(url_unquote("X%3A%5Csmoke.exe"), "X:\\smoke.exe");
        assert_eq!(url_unquote("100%"), "100%");
        assert_eq!(url_unquote("%zz%41"), "%zzA");
    }

    #[test]
    fn sizes() {
        assert_eq!(human_size(4096), "4.0K");
        assert_eq!(human_size(977_076_224), "932M");
        assert_eq!(human_size(1_263_271_936), "1.2G");
        assert_eq!(human_size(10_178), "10K"); // 9.94K rounds up to 10K, as du -h shows it
        assert_eq!(human_size(1_048_000), "1.0M");
    }
}
