//! Wait for `WTL <EVENT> [key=value ...] [| text]` lines in a growing serial log (port of
//! lib/serialwait.py). Used in-process by `wtl run`, and as `wtl tool serialwait` with the same
//! flags and exit codes as the Python script.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Python's `\s` for the event-name lookahead.
fn is_space(c: char) -> bool {
    crate::util::py_isspace(c)
}

/// Find the first `WTL <EVENT>` on a line (not glued to a preceding word character).
/// Returns `(event, "WTL EVENT<rest of line>")`.
pub fn find_event(s: &str) -> Option<(&str, String)> {
    let b = s.as_bytes();
    let mut from = 0;
    while let Some(off) = s[from..].find("WTL ") {
        let i = from + off;
        from = i + 1;
        if i > 0 && (b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_') {
            continue;
        }
        let ev_start = i + 4;
        if !b.get(ev_start).is_some_and(u8::is_ascii_uppercase) {
            continue;
        }
        let mut j = ev_start + 1;
        while j < b.len() && (b[j].is_ascii_uppercase() || b[j].is_ascii_digit() || b[j] == b'_') {
            j += 1;
        }
        if j < b.len() && !s[j..].chars().next().is_some_and(is_space) {
            continue;
        }
        let ev = &s[ev_start..j];
        return Some((ev, format!("WTL {}{}", ev, &s[j..])));
    }
    None
}

/// Decode one raw serial line: strip CR/LF/NUL, then look for an event.
pub fn parse_line(raw: &[u8]) -> (Option<String>, String) {
    let s = String::from_utf8_lossy(raw);
    let s = s.trim_end_matches(['\r', '\n']).replace(['\r', '\0'], "");
    match find_event(&s) {
        Some((ev, text)) => (Some(ev.to_string()), text),
        None => (None, s),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// an --until event (the line)
    Until(String),
    /// a --fail event (the line)
    Fail(String),
    Timeout,
    /// the watched process died
    Exited,
    /// Ctrl-C / SIGTERM reached us
    Interrupted,
}

impl Outcome {
    /// serialwait.py exit code
    pub fn code(&self) -> i32 {
        match self {
            Outcome::Until(_) => 0,
            Outcome::Fail(_) => 1,
            Outcome::Timeout => 3,
            Outcome::Exited => 4,
            Outcome::Interrupted => 130,
        }
    }
}

pub struct Wait<'a> {
    pub until: &'a [&'a str],
    pub fail: &'a [&'a str],
    pub timeout: Option<f64>,
    pub pid: Option<u32>,
    pub progress: bool,
    pub events_file: Option<PathBuf>,
}

impl Wait<'_> {
    /// Read LOG from `*offset` until an event ends the wait. `*offset` is left just after
    /// the matched line, or after the last complete line read (so a line the guest was
    /// still writing when the wait gave up is read again by the next wait).
    pub fn run(&self, log: &Path, offset: &mut u64) -> Outcome {
        let deadline = self.timeout.map(|t| Instant::now() + Duration::from_secs_f64(t.max(0.0)));
        let mut ev_out = self.events_file.as_ref().and_then(|p| File::options().append(true).create(true).open(p).ok());
        let mut f: Option<File> = None;
        let mut pos = *offset;
        let mut pending: Vec<u8> = Vec::new();
        let mut dead_checks = 0;
        let mut buf = vec![0u8; 65536];
        let finish = |offset: &mut u64, at: u64, out: Outcome| {
            *offset = at;
            out
        };
        loop {
            if crate::sys::interrupted() {
                return finish(offset, pos - pending.len() as u64, Outcome::Interrupted);
            }
            if f.is_none() {
                f = File::open(log).ok().and_then(|mut h| h.seek(SeekFrom::Start(pos)).ok().map(|_| h));
            }
            if let Some(h) = f.as_mut() {
                // detect truncation / replacement
                if std::fs::metadata(log).map(|m| m.len() < pos).unwrap_or(false) {
                    if let Ok(nh) = File::open(log) {
                        *h = nh;
                        pos = 0;
                        pending.clear();
                    }
                }
                let n = h.read(&mut buf).unwrap_or(0);
                if n > 0 {
                    let start_pos = pos - pending.len() as u64;
                    pos += n as u64;
                    pending.extend_from_slice(&buf[..n]);
                    let mut cur = start_pos;
                    let mut consumed = 0;
                    while let Some(nl) = pending[consumed..].iter().position(|b| *b == b'\n') {
                        let line = &pending[consumed..consumed + nl];
                        consumed += nl + 1;
                        cur += nl as u64 + 1;
                        let (ev, text) = parse_line(line);
                        let Some(ev) = ev else { continue };
                        if self.progress && ev != "OUTPUT" {
                            // base64 chunks of program output: too noisy
                            eprintln!("  [guest] {}", text);
                        }
                        if let Some(o) = ev_out.as_mut() {
                            let _ = writeln!(o, "{}", text);
                        }
                        if self.until.contains(&ev.as_str()) {
                            return finish(offset, cur, Outcome::Until(text));
                        }
                        if self.fail.contains(&ev.as_str()) {
                            return finish(offset, cur, Outcome::Fail(text));
                        }
                    }
                    pending.drain(..consumed);
                    continue;
                }
            }
            if let Some(pid) = self.pid {
                if !crate::sys::pid_alive(pid) {
                    // drain once more after death, then give up (also consider a final
                    // unterminated line)
                    dead_checks += 1;
                    if dead_checks > 2 {
                        if !pending.is_empty() {
                            if let (Some(ev), text) = parse_line(&pending) {
                                if self.until.contains(&ev.as_str()) {
                                    return finish(offset, pos, Outcome::Until(text));
                                }
                                if self.fail.contains(&ev.as_str()) {
                                    return finish(offset, pos, Outcome::Fail(text));
                                }
                            }
                        }
                        return finish(offset, pos - pending.len() as u64, Outcome::Exited);
                    }
                }
            }
            if deadline.is_some_and(|d| Instant::now() >= d) {
                return finish(offset, pos - pending.len() as u64, Outcome::Timeout);
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

/// `wtl tool serialwait LOG --until EV1,EV2 [...]`
#[derive(clap::Args, Debug)]
#[command(
    about = "Wait for WTL events in a (growing) serial log file",
    long_about = "Wait for WTL events in a (growing) serial log file.\n\n\
        Lines look like  \"WTL <EVENT> [key=value ...] [| text]\"  (CRLF ok; leading junk before \"WTL \" \
        on a line is tolerated, since the serial port may print garbage at boot). The matched line is \
        printed on stdout.",
    after_help = "Exit: 0 an --until event, 1 a --fail event, 3 timeout, 4 the --pid process died."
)]
pub struct Cli {
    /// the serial log
    pub log: PathBuf,
    /// comma list of events that end the wait successfully (exit 0)
    #[arg(long, value_delimiter = ',', value_name = "EV,..")]
    pub until: Vec<String>,
    /// comma list of events that end the wait with failure (exit 1)
    #[arg(long, value_delimiter = ',', value_name = "EV,..")]
    pub fail: Vec<String>,
    /// seconds (default: wait forever; exit 3)
    #[arg(long, value_name = "S")]
    pub timeout: Option<f64>,
    /// stop if that process dies, after draining the log (exit 4)
    #[arg(long)]
    pub pid: Option<u32>,
    /// start reading at byte N
    #[arg(long, value_name = "N", default_value_t = 0)]
    pub from_offset: u64,
    /// write the byte offset just after the matched line to F (feed it to the next
    /// call's --from-offset)
    #[arg(long, value_name = "F")]
    pub offset_file: Option<PathBuf>,
    /// echo every WTL line to stderr as it arrives
    #[arg(long, overrides_with = "quiet")]
    pub progress: bool,
    /// do not echo (the default)
    #[arg(long)]
    pub quiet: bool,
    /// append every WTL line seen to F
    #[arg(long, value_name = "F")]
    pub events_file: Option<PathBuf>,
}

pub fn cli(a: &Cli) -> i32 {
    let names =
        |v: &[String]| -> Vec<String> { v.iter().map(|x| crate::util::py_strip(x).to_string()).filter(|x| !x.is_empty()).collect() };
    let (until, fail) = (names(&a.until), names(&a.fail));
    if until.is_empty() && fail.is_empty() {
        eprintln!("serialwait: need --until and/or --fail");
        return 64;
    }
    let u: Vec<&str> = until.iter().map(String::as_str).collect();
    let f: Vec<&str> = fail.iter().map(String::as_str).collect();
    let progress = a.progress && !a.quiet;
    let w = Wait { until: &u, fail: &f, timeout: a.timeout, pid: a.pid, progress, events_file: a.events_file.clone() };
    let mut offset = a.from_offset;
    let pid = a.pid;
    let offset_file = a.offset_file.clone();
    let out = w.run(&a.log, &mut offset);
    match &out {
        Outcome::Until(l) | Outcome::Fail(l) => println!("{}", l),
        Outcome::Timeout => {
            let mut s = u.clone();
            s.sort_unstable();
            eprintln!("serialwait: timeout waiting for {}", s.join(","));
        }
        Outcome::Exited => eprintln!("serialwait: process {} exited", pid.unwrap_or(0)),
        Outcome::Interrupted => {}
    }
    if let Some(of) = offset_file {
        let _ = std::fs::write(of, format!("{}\n", offset));
    }
    out.code()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events() {
        assert_eq!(find_event("WTL LAB_STEP as=admin n=1").map(|x| x.0), Some("LAB_STEP"));
        assert_eq!(find_event("\u{0}garbage WTL READY port=5").unwrap().1, "WTL READY port=5");
        assert_eq!(find_event("WTL DONE").unwrap().1, "WTL DONE");
        assert_eq!(find_event("XWTL READY"), None);
        assert_eq!(find_event("WTL ready"), None);
        assert_eq!(find_event("WTL READYx"), None);
        assert_eq!(find_event("WTL READYx WTL GO | t").unwrap().1, "WTL GO | t");
        assert_eq!(find_event(": WTL A1_B\tk=v").unwrap().0, "A1_B");
        let (ev, text) = parse_line(b"WTL LAB_IDLE\r\r\n");
        assert_eq!((ev.as_deref(), text.as_str()), (Some("LAB_IDLE"), "WTL LAB_IDLE"));
    }

    fn tmpfile(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("wtl-sw-{}-{}.log", tag, std::process::id()))
    }

    #[test]
    fn wait_offsets_and_partial_lines() {
        let p = tmpfile("off");
        std::fs::write(&p, "boot junk\nWTL AGENT_START os=x\nWTL LOG | hi\nWTL LAB_ST").unwrap();
        let mut off = 0;
        let w = Wait { until: &["AGENT_START"], fail: &["LAB_FAIL"], timeout: Some(1.0), pid: None, progress: false, events_file: None };
        assert_eq!(w.run(&p, &mut off), Outcome::Until("WTL AGENT_START os=x".into()));
        assert_eq!(off, "boot junk\nWTL AGENT_START os=x\n".len() as u64);
        // the half-written LAB_STEP line is not lost on timeout
        let w2 = Wait { until: &["LAB_STEP"], fail: &[], timeout: Some(0.3), pid: None, progress: false, events_file: None };
        assert_eq!(w2.run(&p, &mut off), Outcome::Timeout);
        let mut f = File::options().append(true).open(&p).unwrap();
        f.write_all(b"EP as=admin exit=0\r\nWTL LAB_FAIL | boom\n").unwrap();
        assert_eq!(w2.run(&p, &mut off), Outcome::Until("WTL LAB_STEP as=admin exit=0".into()));
        assert_eq!(w.run(&p, &mut off), Outcome::Fail("WTL LAB_FAIL | boom".into()));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn wait_missing_file_times_out() {
        let mut off = 0;
        let w = Wait { until: &["X"], fail: &[], timeout: Some(0.2), pid: None, progress: false, events_file: None };
        assert_eq!(w.run(&tmpfile("missing"), &mut off), Outcome::Timeout);
    }
}
