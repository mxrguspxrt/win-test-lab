//! Minimal QEMU Machine Protocol client.
//!
//! The monitor is a unix socket on Linux/macOS (`-qmp unix:PATH,server=on,wait=off`) and a
//! loopback TCP port on Windows (`-qmp tcp:127.0.0.1:PORT,...`), since Windows QEMU
//! builds and Rust's std have no unix sockets there.

use anyhow::{anyhow, bail, ensure, Context, Result};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Addr {
    Unix(PathBuf),
    /// "host:port"
    Tcp(String),
}

impl Addr {
    /// `tcp:HOST:PORT` -> Tcp, anything else is a unix socket path.
    pub fn parse(s: &str) -> Addr {
        match s.strip_prefix("tcp:") {
            Some(hp) => Addr::Tcp(hp.to_string()),
            None => Addr::Unix(PathBuf::from(s)),
        }
    }

    /// The address as QEMU's `-qmp` wants it (without the server options).
    pub fn qemu_arg(&self) -> String {
        match self {
            Addr::Unix(p) => format!("unix:{}", p.to_string_lossy().replace(',', ",,")),
            Addr::Tcp(hp) => format!("tcp:{}", hp),
        }
    }
}

impl std::fmt::Display for Addr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Addr::Unix(p) => write!(f, "{}", p.display()),
            Addr::Tcp(hp) => write!(f, "tcp:{}", hp),
        }
    }
}

/// QEMU answered a command with `{"error": {"class": .., "desc": ..}}`. Any other error
/// from this module is a connect / socket / protocol failure.
#[derive(Debug)]
pub struct ReplyError(pub String);

impl std::fmt::Display for ReplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ReplyError {}

fn is_reply_error(e: &anyhow::Error) -> bool {
    e.downcast_ref::<ReplyError>().is_some()
}

trait Stream: Read + Write + Send {}
impl<T: Read + Write + Send> Stream for T {}

pub struct Qmp {
    r: BufReader<Box<dyn Stream>>,
}

impl Qmp {
    pub fn connect(addr: &Addr, timeout: Duration) -> Result<Qmp> {
        let s: Box<dyn Stream> = match addr {
            Addr::Tcp(hp) => {
                use std::net::ToSocketAddrs;
                let sa = hp.to_socket_addrs()?.next().ok_or_else(|| anyhow!("cannot resolve {}", hp))?;
                let t = std::net::TcpStream::connect_timeout(&sa, timeout)?;
                t.set_read_timeout(Some(timeout))?;
                t.set_write_timeout(Some(timeout))?;
                Box::new(t)
            }
            Addr::Unix(p) => unix_connect(p, timeout)?,
        };
        let mut q = Qmp { r: BufReader::new(s) };
        let greet = q.recv()?;
        ensure!(greet.get("QMP").is_some(), "no QMP greeting: {}", greet);
        q.cmd("qmp_capabilities", None)?;
        Ok(q)
    }

    fn recv(&mut self) -> Result<Value> {
        loop {
            let mut line = Vec::new();
            if self.r.read_until(b'\n', &mut line)? == 0 {
                bail!("connection closed");
            }
            let s = String::from_utf8_lossy(&line);
            let s = s.trim();
            if !s.is_empty() {
                return serde_json::from_str(s).with_context(|| format!("bad QMP message {:?}", s));
            }
        }
    }

    /// Run one command; asynchronous events are skipped.
    pub fn cmd(&mut self, name: &str, args: Option<Value>) -> Result<Value> {
        let mut msg = json!({ "execute": name });
        if let Some(a) = args.filter(|a| a.as_object().is_some_and(|o| !o.is_empty())) {
            msg["arguments"] = a;
        }
        let mut line = msg.to_string();
        line.push('\n');
        self.r.get_mut().write_all(line.as_bytes())?;
        loop {
            let mut r = self.recv()?;
            if r.get("event").is_some() {
                continue;
            }
            if let Some(e) = r.get("error") {
                let field = |k: &str| e.get(k).and_then(Value::as_str).unwrap_or("None").to_string();
                return Err(ReplyError(format!("{}: {}", field("class"), field("desc"))).into());
            }
            if let Some(v) = r.get_mut("return") {
                return Ok(v.take());
            }
        }
    }

    /// "running", "shutdown", "paused", ...
    pub fn status(&mut self) -> Result<String> {
        let r = self.cmd("query-status", None)?;
        Ok(match r.get("status").and_then(Value::as_str) {
            Some(s) => s.to_string(),
            None if r.get("running").and_then(Value::as_bool) == Some(true) => "running".into(),
            None => "unknown".into(),
        })
    }

    /// `quit`: QEMU may close the socket before it answers; that counts as success.
    pub fn quit(&mut self) -> Result<()> {
        match self.cmd("quit", None) {
            Err(e) if is_reply_error(&e) => Err(e),
            _ => Ok(()),
        }
    }

    /// Screenshot as PNG: native PNG on QEMU >= 7.1, else PPM converted here.
    pub fn screendump(&mut self, out: &Path) -> Result<()> {
        let out = crate::util::abspath(out);
        match self.cmd("screendump", Some(json!({ "filename": out, "format": "png" }))) {
            Ok(_) => {
                wait_file(&out, 3.0);
                if std::fs::read(&out).is_ok_and(|b| b.starts_with(PNG_MAGIC)) {
                    return Ok(());
                }
            }
            Err(e) if is_reply_error(&e) => {} // old QEMU: no "format"
            Err(e) => return Err(e),
        }
        // old QEMU: PPM only
        let dir = out.parent().unwrap_or(Path::new("."));
        let tmp = dir.join(format!(".wtl-screendump-{}.ppm", std::process::id()));
        let res = (|| {
            self.cmd("screendump", Some(json!({ "filename": tmp })))?;
            wait_file(&tmp, 5.0);
            let data = std::fs::read(&tmp)?;
            let png = if data.starts_with(PNG_MAGIC) { data } else { ppm_to_png(&data)? };
            std::fs::write(&out, png)?;
            Ok(())
        })();
        let _ = std::fs::remove_file(&tmp);
        res
    }
}

#[cfg(unix)]
fn unix_connect(p: &Path, timeout: Duration) -> Result<Box<dyn Stream>> {
    let s = std::os::unix::net::UnixStream::connect(p)?;
    s.set_read_timeout(Some(timeout))?;
    s.set_write_timeout(Some(timeout))?;
    Ok(Box::new(s))
}

#[cfg(not(unix))]
fn unix_connect(p: &Path, _timeout: Duration) -> Result<Box<dyn Stream>> {
    bail!("unix sockets are not supported on this platform ({}); use tcp:HOST:PORT", p.display())
}

/// Wait until a file exists and its size stopped changing.
fn wait_file(path: &Path, timeout: f64) -> bool {
    let end = Instant::now() + Duration::from_secs_f64(timeout);
    let mut last = None;
    while Instant::now() < end {
        if let Ok(md) = std::fs::metadata(path) {
            if md.len() > 0 && last == Some(md.len()) {
                return true;
            }
            last = Some(md.len());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    path.exists()
}

// ------------------------------------------------------------------ PPM -> PNG

pub const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";

fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for &b in data {
        c ^= b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
    }
    !c
}

/// zlib stream made of stored (uncompressed) deflate blocks: valid PNG data with no
/// compressor needed. Screenshots via this path only happen on QEMU < 7.1.
fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    let mut chunks = data.chunks(65535).peekable();
    if chunks.peek().is_none() {
        out.extend_from_slice(&[1, 0, 0, 0xFF, 0xFF]);
    }
    while let Some(c) = chunks.next() {
        out.push(if chunks.peek().is_none() { 1 } else { 0 });
        let n = c.len() as u16;
        out.extend_from_slice(&n.to_le_bytes());
        out.extend_from_slice(&(!n).to_le_bytes());
        out.extend_from_slice(c);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &x in data {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    out.extend_from_slice(&((b << 16) | a).to_be_bytes());
    out
}

fn png_chunk(out: &mut Vec<u8>, typ: &[u8], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let mut c = typ.to_vec();
    c.extend_from_slice(data);
    out.extend_from_slice(&c);
    out.extend_from_slice(&crc32(&c).to_be_bytes());
}

pub fn ppm_to_png(data: &[u8]) -> Result<Vec<u8>> {
    let mut fields: Vec<&[u8]> = Vec::new();
    let mut p = 0;
    while fields.len() < 4 {
        while p < data.len() && data[p].is_ascii_whitespace() {
            p += 1;
        }
        ensure!(p < data.len(), "truncated PPM header");
        if data[p] == b'#' {
            while p < data.len() && data[p] != b'\n' && data[p] != b'\r' {
                p += 1;
            }
            continue;
        }
        let q = p + data[p..].iter().position(|b| b.is_ascii_whitespace()).unwrap_or(data.len() - p);
        fields.push(&data[p..q]);
        p = q;
    }
    p += 1; // single whitespace after maxval
    ensure!(fields[0] == b"P6", "not a binary PPM (P6)");
    let num = |f: &[u8]| std::str::from_utf8(f).ok().and_then(|s| s.parse::<usize>().ok()).context("bad PPM header");
    let (w, h, maxval) = (num(fields[1])?, num(fields[2])?, num(fields[3])?);
    ensure!(maxval == 255, "unsupported PPM maxval {}", maxval);
    let stride = w * 3;
    let pix = data.get(p..p + stride * h).context("truncated PPM data")?;
    let mut raw = Vec::with_capacity((stride + 1) * h);
    for y in 0..h {
        raw.push(0);
        raw.extend_from_slice(&pix[y * stride..(y + 1) * stride]);
    }
    let mut png = PNG_MAGIC.to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&(w as u32).to_be_bytes());
    ihdr.extend_from_slice(&(h as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    png_chunk(&mut png, b"IHDR", &ihdr);
    png_chunk(&mut png, b"IDAT", &zlib_stored(&raw));
    png_chunk(&mut png, b"IEND", b"");
    Ok(png)
}

// ------------------------------------------------------------------ CLI

/// `wtl tool qmp [--timeout S] SOCK CMD [ARGS]`
#[derive(clap::Args, Debug)]
#[command(about = "Minimal QEMU monitor client", after_help = "Exit: 0 ok, 1 QMP error reply, 2 connect/protocol failure, 64 usage.")]
pub struct Cli {
    /// seconds to wait for QEMU
    #[arg(long, default_value_t = 10.0, value_parser = positive_secs)]
    pub timeout: f64,
    /// the monitor: a unix socket path, or tcp:HOST:PORT
    pub sock: String,
    #[command(subcommand)]
    pub cmd: QmpCmd,
}

#[derive(clap::Subcommand, Debug)]
pub enum QmpCmd {
    /// screenshot as PNG (QEMU >= 7.1 native png, older: ppm converted to png)
    Screendump {
        #[arg(value_name = "OUT.png")]
        out: PathBuf,
    },
    /// system_powerdown (ACPI power button)
    Powerdown,
    /// quit qemu immediately
    Quit,
    /// print the run state (e.g. "running")
    Status,
    /// send an arbitrary {"execute": ...} and print the reply
    Raw {
        #[arg(value_name = "JSON")]
        json: String,
    },
}

fn positive_secs(s: &str) -> Result<f64, String> {
    match s.parse::<f64>() {
        Ok(t) if t > 0.0 && t.is_finite() => Ok(t),
        _ => Err(format!("{} is not a positive number of seconds", s)),
    }
}

pub fn cli(a: &Cli) -> i32 {
    let mut q = match Qmp::connect(&Addr::parse(&a.sock), Duration::from_secs_f64(a.timeout)) {
        Ok(q) => q,
        Err(e) => {
            eprintln!("qmp: cannot connect to {}: {:#}", a.sock, e);
            return 2;
        }
    };
    let res = match &a.cmd {
        QmpCmd::Screendump { out } => q.screendump(out),
        QmpCmd::Powerdown => q.cmd("system_powerdown", None).map(|_| ()),
        QmpCmd::Quit => q.quit(),
        QmpCmd::Status => q.status().map(|s| println!("{}", s)),
        QmpCmd::Raw { json } => serde_json::from_str::<Value>(json)
            .context("bad JSON")
            .and_then(|msg| {
                let name = msg.get("execute").and_then(Value::as_str).unwrap_or("").to_string();
                q.cmd(&name, msg.get("arguments").cloned())
            })
            .map(|r| println!("{}", crate::pyjson::dumps(&r))),
    };
    match res {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("qmp: {:#}", e);
            if is_reply_error(&e) {
                1
            } else {
                2
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_from_ppm() {
        let ppm = [b"P6\n# c\n2 2\n255\n".to_vec(), vec![255, 0, 0, 0, 255, 0, 0, 0, 255, 1, 2, 3]].concat();
        let png = ppm_to_png(&ppm).unwrap();
        assert!(png.starts_with(PNG_MAGIC));
        assert_eq!(&png[12..16], b"IHDR");
        assert_eq!(crc32(b"IEND"), 0xAE42_6082);
        assert!(ppm_to_png(b"P3\n1 1\n255\n").is_err());
    }

    #[test]
    fn zlib_ok() {
        // adler32("abc") = 0x024d0127
        let z = zlib_stored(b"abc");
        assert_eq!(&z[z.len() - 4..], &[0x02, 0x4d, 0x01, 0x27]);
        assert_eq!(z[2], 1); // single final block
    }

    #[test]
    fn addr_parse() {
        assert_eq!(Addr::parse("tcp:127.0.0.1:4444"), Addr::Tcp("127.0.0.1:4444".into()));
        assert_eq!(Addr::parse("/x/dc.qmp"), Addr::Unix("/x/dc.qmp".into()));
        assert_eq!(Addr::Unix("/a,b".into()).qemu_arg(), "unix:/a,,b");
    }
}
