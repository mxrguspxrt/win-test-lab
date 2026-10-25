//! Decode program output the guest sent over serial (same result as lib/outputs.py, which
//! launch-test.sh still uses).
//!
//! ```text
//! The guest sends   WTL OUTPUT stream=NAME seq=N | <base64>
//! followed by       WTL OUTPUT_END stream=NAME bytes=TOTAL sent=N truncated=0|1
//! and (launch-test) WTL PROC_EXIT code=N secs=S   or   WTL PROC_TIMEOUT secs=S
//! ```
//!
//! A lab run uses streams `stepNout` / `stepNerr`. Each stream is written to
//! `RESULTS_DIR/<stream>.txt` (stdout.txt and stderr.txt always), raw bytes.

use crate::util::{py_int, url_unquote};
use anyhow::{Context, Result};
use base64::Engine;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One OUTPUT chunk. The guest sends [Convert]::ToBase64String output, so standard
/// base64; characters outside the alphabet (serial junk, CR) are dropped first, as
/// Python's lenient b64decode did. A chunk that still does not decode counts as missing.
fn decode_chunk(text: &str) -> Option<Vec<u8>> {
    let clean: String = text.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=')).collect();
    base64::engine::general_purpose::STANDARD.decode(clean).ok()
}

/// common Windows process exit codes (NTSTATUS) worth naming in a test report
pub fn ntstatus_name(code: u32) -> Option<&'static str> {
    Some(match code {
        0xC0000005 => "STATUS_ACCESS_VIOLATION (crash)",
        0xC000001D => "STATUS_ILLEGAL_INSTRUCTION (CPU instruction not supported)",
        0xC0000094 => "STATUS_INTEGER_DIVIDE_BY_ZERO",
        0xC00000FD => "STATUS_STACK_OVERFLOW",
        0xC0000135 => "STATUS_DLL_NOT_FOUND (a required DLL is missing)",
        0xC0000139 => "STATUS_ENTRYPOINT_NOT_FOUND (DLL too old for this Windows)",
        0xC0000142 => "STATUS_DLL_INIT_FAILED",
        0xC000007B => "STATUS_INVALID_IMAGE_FORMAT (32/64-bit mismatch or bad exe)",
        0xC0000409 => "STATUS_STACK_BUFFER_OVERRUN (fail-fast / abort, e.g. Rust panic=abort)",
        0xC000013A => "STATUS_CONTROL_C_EXIT",
        0x40010004 => "DBG_TERMINATE_PROCESS (killed)",
        _ => return None,
    })
}

/// A parsed event line: name, key=value pairs (unescaped, in order), text after " | ".
#[derive(Debug, PartialEq, Eq)]
pub struct Event {
    pub name: String,
    pub kv: Vec<(String, String)>,
    pub text: String,
}

impl Event {
    pub fn get(&self, k: &str) -> Option<&str> {
        self.kv.iter().rev().find(|(key, _)| key == k).map(|(_, v)| v.as_str())
    }
}

/// Strict grammar: `WTL EVENT( key=value)*( | text)?` from the first "WTL " to the end of
/// the line; anything else is not an event.
pub fn parse_event(line: &str) -> Option<Event> {
    let line = line.replace(['\r', '\0'], "");
    let i = line.find("WTL ")?;
    let s = line[i..].trim_end_matches(crate::util::py_isspace);
    let b = s.as_bytes();
    let mut j = 4;
    if !b.get(j).is_some_and(u8::is_ascii_uppercase) {
        return None;
    }
    while j < b.len() && (b[j].is_ascii_uppercase() || b[j].is_ascii_digit() || b[j] == b'_') {
        j += 1;
    }
    let name = s[4..j].to_string();
    let mut kv = Vec::new();
    let mut rest = &s[j..];
    // " key=\S*"
    while let Some(r) = rest.strip_prefix(' ') {
        let klen = r.bytes().take_while(|c| c.is_ascii_alphabetic() || *c == b'_').count();
        if klen == 0 || r.as_bytes().get(klen) != Some(&b'=') {
            break;
        }
        let v = &r[klen + 1..];
        let vlen = v.find(crate::util::py_isspace).unwrap_or(v.len());
        kv.push((r[..klen].to_string(), url_unquote(&v[..vlen])));
        rest = &v[vlen..];
    }
    let text = if rest.is_empty() { String::new() } else { rest.strip_prefix(" | ")?.to_string() };
    Some(Event { name, kv, text })
}

/// Decode SERIAL into RES; returns the summary JSON (keys in outputs.py's order).
pub fn decode(serial: &Path, res: &Path) -> Result<Value> {
    std::fs::create_dir_all(res).with_context(|| format!("cannot create {}", res.display()))?;
    // stream name -> seq -> bytes, in first-seen order (stdout/stderr first, like the Python)
    let mut order: Vec<String> = vec!["stdout".into(), "stderr".into()];
    let mut chunks: BTreeMap<String, BTreeMap<i64, Vec<u8>>> = BTreeMap::new();
    let mut end: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    let mut exit_code: Option<i64> = None;
    let mut timed_out = false;
    let mut secs: Option<String> = None;
    let data = std::fs::read(serial).unwrap_or_default();
    let data = String::from_utf8_lossy(&data);
    let see = |order: &mut Vec<String>, s: &str| {
        if !order.iter().any(|x| x == s) {
            order.push(s.to_string());
        }
    };
    for raw in data.split('\n') {
        let Some(ev) = parse_event(raw) else { continue };
        let stream = ev.get("stream").filter(|s| !s.is_empty()).map(str::to_string);
        match (ev.name.as_str(), stream) {
            ("OUTPUT", Some(s)) => {
                see(&mut order, &s);
                let seq = py_int(ev.get("seq").unwrap_or("0"));
                if let (Some(seq), Some(bytes)) = (seq, decode_chunk(&ev.text)) {
                    chunks.entry(s).or_default().insert(seq, bytes);
                }
            }
            ("OUTPUT_END", Some(s)) => {
                see(&mut order, &s);
                end.insert(s, ev.kv.clone());
            }
            ("PROC_EXIT", _) => {
                exit_code = py_int(ev.get("code").unwrap_or(""));
                secs = ev.get("secs").map(str::to_string);
            }
            ("PROC_TIMEOUT", _) => {
                timed_out = true;
                secs = ev.get("secs").map(str::to_string);
            }
            _ => {}
        }
    }
    let mut info = Map::new();
    info.insert("exit_code".into(), json!(exit_code));
    info.insert("exit_hex".into(), json!(exit_code.map(|c| format!("0x{:08X}", c & 0xFFFF_FFFF))));
    info.insert("exit_name".into(), json!(exit_code.and_then(|c| ntstatus_name((c & 0xFFFF_FFFF) as u32))));
    info.insert("timed_out".into(), json!(timed_out));
    info.insert("secs".into(), json!(secs));
    let mut missing = 0;
    for name in &order {
        let mut body = Vec::new();
        if let Some(parts) = chunks.get(name) {
            if let Some(&max) = parts.keys().next_back() {
                for i in 0..=max.max(-1) {
                    match parts.get(&i) {
                        Some(b) => body.extend_from_slice(b),
                        None => missing += 1,
                    }
                }
            }
        }
        let out = res.join(format!("{}.txt", name));
        std::fs::write(&out, &body).with_context(|| format!("cannot write {}", out.display()))?;
        let e = end.get(name);
        let ekey = |k: &str| e.and_then(|kv| kv.iter().rev().find(|(x, _)| x == k)).map(|(_, v)| v.as_str());
        let bytes = match ekey("bytes") {
            Some(v) => py_int(v).unwrap_or(0),
            None => body.len() as i64,
        };
        info.insert(format!("{}_bytes", name), json!(bytes));
        info.insert(format!("{}_truncated", name), json!(ekey("truncated") == Some("1")));
    }
    info.insert("missing_chunks".into(), json!(missing));
    Ok(Value::Object(info))
}

/// `wtl tool outputs SERIAL_LOG RESULTS_DIR`
#[derive(clap::Args, Debug)]
#[command(
    about = "Decode the guest's OUTPUT streams",
    long_about = "Decode program output captured by the guest agent.\n\n\
        Writes RESULTS_DIR/<stream>.txt for every OUTPUT stream (stdout.txt and stderr.txt always) and \
        prints one JSON object on stdout: exit_code, exit_hex, exit_name, timed_out, secs, \
        <stream>_bytes, <stream>_truncated ..., missing_chunks."
)]
pub struct Cli {
    pub serial_log: PathBuf,
    pub results_dir: PathBuf,
}

pub fn cli(a: &Cli) -> Result<()> {
    let info = decode(&a.serial_log, &a.results_dir)?;
    println!("{}", crate::pyjson::dumps(&info));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b64encode(b: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(b)
    }

    #[test]
    fn chunks() {
        assert_eq!(decode_chunk(" aGVs\r\nbG8= ").unwrap(), b"hello");
        assert_eq!(decode_chunk("").unwrap(), b"");
        assert!(decode_chunk("aGVsbG8").is_none()); // a lost tail counts as a missing chunk
    }

    #[test]
    fn grammar() {
        let e = parse_event("junk WTL LAB_STEP as=admin n=1 script=X:%5Cs%20a.bat exit=0 secs=1.5\r").unwrap();
        assert_eq!(e.name, "LAB_STEP");
        assert_eq!(e.get("script"), Some("X:\\s a.bat"));
        assert_eq!(e.text, "");
        let e = parse_event("WTL OUTPUT stream=step1out seq=0 | aGk=").unwrap();
        assert_eq!((e.get("stream"), e.text.as_str()), (Some("step1out"), "aGk="));
        let e = parse_event("WTL LOG | free text with k=v").unwrap();
        assert!(e.kv.is_empty());
        assert_eq!(e.text, "free text with k=v");
        assert!(parse_event("WTL LOG hello").is_none());
        assert!(parse_event("WTL X k=v |").is_none());
        assert!(parse_event("nothing here").is_none());
        assert_eq!(parse_event("WTL LAB_IDLE   ").unwrap().name, "LAB_IDLE");
    }

    #[test]
    fn decode_streams() {
        let d = std::env::temp_dir().join(format!("wtl-outputs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let big: Vec<u8> = (0..2000u32).map(|i| (i % 256) as u8).collect();
        let mut log = String::from("boot\r\n");
        for (i, c) in big.chunks(768).enumerate() {
            log += &format!("WTL OUTPUT stream=step1out seq={} | {}\r\n", i, b64encode(c));
        }
        log += "WTL OUTPUT_END stream=step1out bytes=2000 sent=2000 truncated=0\r\n";
        log += "WTL OUTPUT stream=step2err seq=1 | eA==\r\n"; // seq 0 lost
        log += "WTL OUTPUT_END stream=step2err bytes=9 sent=2 truncated=1\r\n";
        log += "WTL PROC_EXIT code=-1073741819 secs=2.5\r\n";
        let serial = d.join("serial.log");
        std::fs::write(&serial, log).unwrap();
        let res = d.join("res");
        let info = decode(&serial, &res).unwrap();
        assert_eq!(std::fs::read(res.join("step1out.txt")).unwrap(), big);
        assert_eq!(std::fs::read(res.join("step2err.txt")).unwrap(), b"x");
        assert_eq!(std::fs::read(res.join("stdout.txt")).unwrap(), b"");
        assert_eq!(
            crate::pyjson::dumps(&info),
            "{\"exit_code\": -1073741819, \"exit_hex\": \"0xC0000005\", \"exit_name\": \"STATUS_ACCESS_VIOLATION (crash)\", \
             \"timed_out\": false, \"secs\": \"2.5\", \"stdout_bytes\": 0, \"stdout_truncated\": false, \"stderr_bytes\": 0, \
             \"stderr_truncated\": false, \"step1out_bytes\": 2000, \"step1out_truncated\": false, \"step2err_bytes\": 9, \
             \"step2err_truncated\": true, \"missing_chunks\": 1}"
        );
        let _ = std::fs::remove_dir_all(&d);
    }
}
