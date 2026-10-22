//! fetch-ria - win-test-lab example "program": download a web page over HTTPS and
//! report what came back. Used to check that a Rust exe starts and has working
//! network + TLS on each Windows version.
//!
//!     fetch-ria.exe [URL]          default URL: https://www.ria.ee/
//!
//! Uses only the Rust standard library + Windows' own WinHTTP (no crates), so the TLS
//! stack being tested is the one that Windows version really has (Schannel).
//!
//! Output (stdout, one "key: value" per line, easy to check from python):
//!     fetch-ria 0.1.0
//!     windows: 6.3.9600
//!     url: https://www.ria.ee/
//!     attempt: 1
//!     status: 200
//!     bytes: 54321
//!     title: <contents of the page's <title>>
//!     RESULT OK
//! Exit codes: 0 ok, 1 bad usage, 2 network/TLS error, 3 HTTP status not 2xx, 4 no <title>.
#![allow(non_snake_case, clippy::upper_case_acronyms)]

use std::ffi::c_void;
use std::io::Write;
use std::process::exit;
use std::ptr::{null, null_mut};
use std::thread::sleep;
use std::time::Duration;

type HINTERNET = *mut c_void;
type BOOL = i32;

#[link(name = "winhttp")]
extern "system" {
    fn WinHttpOpen(agent: *const u16, access: u32, proxy: *const u16, bypass: *const u16, flags: u32) -> HINTERNET;
    fn WinHttpSetOption(h: HINTERNET, option: u32, buf: *mut c_void, len: u32) -> BOOL;
    fn WinHttpSetTimeouts(h: HINTERNET, resolve: i32, connect: i32, send: i32, receive: i32) -> BOOL;
    fn WinHttpConnect(h: HINTERNET, server: *const u16, port: u16, reserved: u32) -> HINTERNET;
    fn WinHttpOpenRequest(
        h: HINTERNET, verb: *const u16, object: *const u16, version: *const u16,
        referrer: *const u16, accept: *const *const u16, flags: u32,
    ) -> HINTERNET;
    fn WinHttpSendRequest(
        h: HINTERNET, headers: *const u16, headers_len: u32, optional: *mut c_void,
        optional_len: u32, total_len: u32, context: usize,
    ) -> BOOL;
    fn WinHttpReceiveResponse(h: HINTERNET, reserved: *mut c_void) -> BOOL;
    fn WinHttpQueryHeaders(
        h: HINTERNET, level: u32, name: *const u16, buf: *mut c_void, len: *mut u32, index: *mut u32,
    ) -> BOOL;
    fn WinHttpQueryDataAvailable(h: HINTERNET, avail: *mut u32) -> BOOL;
    fn WinHttpReadData(h: HINTERNET, buf: *mut c_void, len: u32, read: *mut u32) -> BOOL;
    fn WinHttpCloseHandle(h: HINTERNET) -> BOOL;
}

#[link(name = "kernel32")]
extern "system" {
    fn GetLastError() -> u32;
}

#[repr(C)]
struct OSVERSIONINFOW {
    size: u32,
    major: u32,
    minor: u32,
    build: u32,
    platform: u32,
    csd: [u16; 128],
}

#[link(name = "ntdll")]
extern "system" {
    // the real version (GetVersionEx lies to programs without a manifest)
    fn RtlGetVersion(info: *mut OSVERSIONINFOW) -> i32;
}

const WINHTTP_ACCESS_TYPE_DEFAULT_PROXY: u32 = 0;
const WINHTTP_OPTION_SECURE_PROTOCOLS: u32 = 84;
const WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_2: u32 = 0x0000_0800;
const WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_3: u32 = 0x0000_2000;
const WINHTTP_FLAG_SECURE: u32 = 0x0080_0000;
const WINHTTP_QUERY_STATUS_CODE: u32 = 19;
const WINHTTP_QUERY_FLAG_NUMBER: u32 = 0x2000_0000;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// closes the WinHTTP handle when dropped
struct Handle(HINTERNET);
impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { WinHttpCloseHandle(self.0) };
        }
    }
}

struct Failure {
    code: u32,
    step: &'static str,
}

fn last_error(step: &'static str) -> Failure {
    Failure { code: unsafe { GetLastError() }, step }
}

fn winhttp_error_name(code: u32) -> &'static str {
    match code {
        12002 => "ERROR_WINHTTP_TIMEOUT",
        12005 => "ERROR_WINHTTP_INVALID_URL",
        12006 => "ERROR_WINHTTP_UNRECOGNIZED_SCHEME",
        12007 => "ERROR_WINHTTP_NAME_NOT_RESOLVED (DNS)",
        12009 => "ERROR_WINHTTP_INVALID_OPTION",
        12029 => "ERROR_WINHTTP_CANNOT_CONNECT",
        12030 => "ERROR_WINHTTP_CONNECTION_ERROR",
        12037 => "ERROR_WINHTTP_SECURE_CERT_DATE_INVALID (check the VM clock)",
        12038 => "ERROR_WINHTTP_SECURE_CERT_CN_INVALID",
        12044 => "ERROR_WINHTTP_CLIENT_AUTH_CERT_NEEDED",
        12045 => "ERROR_WINHTTP_SECURE_INVALID_CA (root certificate not trusted)",
        12057 => "ERROR_WINHTTP_SECURE_CERT_REV_FAILED (revocation check failed)",
        12152 => "ERROR_WINHTTP_INVALID_SERVER_RESPONSE",
        12156 => "ERROR_WINHTTP_REDIRECT_FAILED",
        12175 => "ERROR_WINHTTP_SECURE_FAILURE (TLS handshake failed: protocol/cipher/cert)",
        _ => "",
    }
}

/// errors worth retrying: the VM network may still be coming up right after logon
fn retryable(code: u32) -> bool {
    matches!(code, 12002 | 12007 | 12029 | 12030)
}

struct Url {
    secure: bool,
    host: String,
    port: u16,
    path: String,
}

fn parse_url(url: &str) -> Option<Url> {
    let (secure, rest) = if let Some(r) = url.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = url.strip_prefix("http://") {
        (false, r)
    } else {
        return None;
    };
    let (hostport, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match hostport.rfind(':') {
        Some(i) => (&hostport[..i], hostport[i + 1..].parse().ok()?),
        None => (hostport, if secure { 443 } else { 80 }),
    };
    if host.is_empty() {
        return None;
    }
    Some(Url { secure, host: host.to_string(), port, path: path.to_string() })
}

fn fetch(url: &Url) -> Result<(u32, Vec<u8>), Failure> {
    unsafe {
        let agent = wide("fetch-ria/0.1 (win-test-lab)");
        let session = Handle(WinHttpOpen(agent.as_ptr(), WINHTTP_ACCESS_TYPE_DEFAULT_PROXY, null(), null(), 0));
        if session.0.is_null() {
            return Err(last_error("WinHttpOpen"));
        }
        // ask for TLS 1.2 + 1.3; older Windows rejects the 1.3 flag -> fall back to 1.2 only
        let mut protos: u32 = WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_2 | WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_3;
        if WinHttpSetOption(session.0, WINHTTP_OPTION_SECURE_PROTOCOLS, &mut protos as *mut u32 as *mut c_void, 4) == 0 {
            protos = WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_2;
            if WinHttpSetOption(session.0, WINHTTP_OPTION_SECURE_PROTOCOLS, &mut protos as *mut u32 as *mut c_void, 4) == 0 {
                return Err(last_error("WinHttpSetOption(SECURE_PROTOCOLS)"));
            }
        }
        WinHttpSetTimeouts(session.0, 15_000, 15_000, 30_000, 30_000);

        let host = wide(&url.host);
        let conn = Handle(WinHttpConnect(session.0, host.as_ptr(), url.port, 0));
        if conn.0.is_null() {
            return Err(last_error("WinHttpConnect"));
        }
        let verb = wide("GET");
        let path = wide(&url.path);
        let flags = if url.secure { WINHTTP_FLAG_SECURE } else { 0 };
        let req = Handle(WinHttpOpenRequest(conn.0, verb.as_ptr(), path.as_ptr(), null(), null(), null(), flags));
        if req.0.is_null() {
            return Err(last_error("WinHttpOpenRequest"));
        }
        if WinHttpSendRequest(req.0, null(), 0, null_mut(), 0, 0, 0) == 0 {
            return Err(last_error("WinHttpSendRequest"));
        }
        if WinHttpReceiveResponse(req.0, null_mut()) == 0 {
            return Err(last_error("WinHttpReceiveResponse"));
        }
        let mut status: u32 = 0;
        let mut len: u32 = 4;
        if WinHttpQueryHeaders(
            req.0, WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER, null(),
            &mut status as *mut u32 as *mut c_void, &mut len, null_mut(),
        ) == 0
        {
            return Err(last_error("WinHttpQueryHeaders"));
        }
        let mut body = Vec::new();
        loop {
            let mut avail: u32 = 0;
            if WinHttpQueryDataAvailable(req.0, &mut avail) == 0 {
                return Err(last_error("WinHttpQueryDataAvailable"));
            }
            if avail == 0 {
                break;
            }
            let mut buf = vec![0u8; avail as usize];
            let mut got: u32 = 0;
            if WinHttpReadData(req.0, buf.as_mut_ptr() as *mut c_void, avail, &mut got) == 0 {
                return Err(last_error("WinHttpReadData"));
            }
            if got == 0 {
                break;
            }
            body.extend_from_slice(&buf[..got as usize]);
        }
        Ok((status, body))
    }
}

fn find_ci(hay: &str, needle: &str, from: usize) -> Option<usize> {
    hay.get(from..)?.to_ascii_lowercase().find(needle).map(|i| i + from)
}

fn page_title(html: &str) -> Option<String> {
    let start = find_ci(html, "<title", 0)?;
    let gt = html[start..].find('>')? + start + 1;
    let end = find_ci(html, "</title", gt)?;
    let raw = &html[gt..end];
    let text = raw
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ");
    let t = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.is_empty() { None } else { Some(t) }
}

fn windows_version() -> String {
    let mut v = OSVERSIONINFOW { size: std::mem::size_of::<OSVERSIONINFOW>() as u32, major: 0, minor: 0, build: 0, platform: 0, csd: [0; 128] };
    if unsafe { RtlGetVersion(&mut v) } == 0 {
        format!("{}.{}.{}", v.major, v.minor, v.build)
    } else {
        "unknown".to_string()
    }
}

fn main() {
    let url_s = std::env::args().nth(1).unwrap_or_else(|| "https://www.ria.ee/".to_string());
    let out = std::io::stdout();
    let mut o = out.lock();
    let _ = writeln!(o, "fetch-ria {}", env!("CARGO_PKG_VERSION"));
    let _ = writeln!(o, "windows: {}", windows_version());
    let _ = writeln!(o, "url: {}", url_s);
    let url = match parse_url(&url_s) {
        Some(u) => u,
        None => {
            let _ = writeln!(o, "error: cannot parse URL (want http(s)://host[:port]/path)");
            let _ = writeln!(o, "RESULT FAIL usage");
            exit(1);
        }
    };
    let _ = o.flush();

    let attempts = 6;
    let mut result = Err(Failure { code: 0, step: "none" });
    for attempt in 1..=attempts {
        let _ = writeln!(o, "attempt: {}", attempt);
        let _ = o.flush();
        result = fetch(&url);
        match &result {
            Err(f) if retryable(f.code) && attempt < attempts => {
                let _ = writeln!(o, "retry: {} failed with {} {}", f.step, f.code, winhttp_error_name(f.code));
                let _ = o.flush();
                sleep(Duration::from_secs(5));
            }
            _ => break,
        }
    }

    match result {
        Err(f) => {
            let _ = writeln!(o, "error: {} failed: {} {}", f.step, f.code, winhttp_error_name(f.code));
            let _ = writeln!(o, "RESULT FAIL network");
            let _ = o.flush();
            exit(2);
        }
        Ok((status, body)) => {
            let html = String::from_utf8_lossy(&body);
            let _ = writeln!(o, "status: {}", status);
            let _ = writeln!(o, "bytes: {}", body.len());
            let title = page_title(&html);
            let _ = writeln!(o, "title: {}", title.clone().unwrap_or_default());
            if !(200..300).contains(&status) {
                let _ = writeln!(o, "RESULT FAIL http {}", status);
                let _ = o.flush();
                exit(3);
            }
            if title.is_none() {
                let _ = writeln!(o, "RESULT FAIL no-title");
                let _ = o.flush();
                exit(4);
            }
            let _ = writeln!(o, "RESULT OK");
            let _ = o.flush();
        }
    }
}
