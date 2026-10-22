//! Timestamped, coloured log lines on stderr (same look as lib/common.sh log/ok/warn/err).

use std::io::IsTerminal;
use std::sync::OnceLock;

fn color() -> bool {
    static C: OnceLock<bool> = OnceLock::new();
    *C.get_or_init(|| std::io::stderr().is_terminal())
}

fn paint(code: &str) -> &str {
    if color() { code } else { "" }
}

fn stamp() -> String {
    format!("{}[{}]{}", paint("\x1b[2m"), chrono::Local::now().format("%H:%M:%S"), paint("\x1b[0m"))
}

pub fn log(msg: &str) {
    eprintln!("{} {}", stamp(), msg);
}

pub fn ok(msg: &str) {
    eprintln!("{} {}{}{}", stamp(), paint("\x1b[32m"), msg, paint("\x1b[0m"));
}

pub fn warn(msg: &str) {
    eprintln!("{} {}WARNING:{} {}", stamp(), paint("\x1b[33m"), paint("\x1b[0m"), msg);
}

pub fn err(msg: &str) {
    eprintln!("{} {}ERROR:{} {}", stamp(), paint("\x1b[31m"), paint("\x1b[0m"), msg);
}
