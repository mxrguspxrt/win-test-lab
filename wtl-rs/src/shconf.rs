//! Reader for the bash `KEY=value` files the lab definitions (labs/*.conf) are written in.
//!
//! lab.sh `source`d them. This reads the subset of bash those files use, with the same
//! meaning: `#` comments, `KEY=value` (several per line allowed, optional `export`),
//! 'single' and "double" quotes, backslash escapes, line continuations, `~`, and `$NAME`,
//! `${NAME}`, `${NAME:-default}` / `${NAME-default}` expansion. Anything that would run a
//! command (`$(...)`, backticks, pipes, redirections, plain commands) is rejected with the
//! line number, instead of being silently misread.

use std::collections::HashMap;

/// Parse TEXT, applying each assignment to VARS in order. Names not set in VARS are looked
/// up with FALLBACK (e.g. the process environment). Errors name the line.
pub fn parse(
    text: &str,
    vars: &mut HashMap<String, String>,
    fallback: &dyn Fn(&str) -> Option<String>,
) -> anyhow::Result<()> {
    parse_text(text, vars, fallback).map_err(anyhow::Error::msg)
}

fn parse_text(
    text: &str,
    vars: &mut HashMap<String, String>,
    fallback: &dyn Fn(&str) -> Option<String>,
) -> Result<(), String> {
    let mut p = P { c: text.chars().collect(), i: 0, line: 1 };
    loop {
        // one command = words until newline / ';'
        let mut first = true;
        let mut assignments: Vec<(String, String)> = Vec::new();
        loop {
            p.skip_blanks();
            match p.peek() {
                None => break,
                Some('\n') | Some(';') => {
                    p.bump();
                    break;
                }
                Some('#') => {
                    while !matches!(p.peek(), None | Some('\n')) {
                        p.bump();
                    }
                    continue;
                }
                _ => {}
            }
            let line = p.line;
            if let Some(name) = p.assignment_name() {
                let get = |n: &str| -> Option<String> {
                    if let Some((_, v)) = assignments.iter().rev().find(|(k, _)| k == n) {
                        return Some(v.clone());
                    }
                    vars.get(n).cloned().or_else(|| fallback(n))
                };
                let value = p.word(&get, true)?;
                assignments.push((name, value));
            } else {
                let w = p.word(&|_| None, false)?;
                if first && w == "export" {
                    first = false;
                    continue;
                }
                return Err(format!("line {}: only KEY=value assignments are supported here, not {:?}", line, w));
            }
            first = false;
        }
        // bash applies `A=1 B=$A` left to right, which `get` above already models
        for (k, v) in assignments {
            vars.insert(k, v);
        }
        if p.peek().is_none() {
            return Ok(());
        }
    }
}

struct P {
    c: Vec<char>,
    i: usize,
    line: usize,
}

fn is_name_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

impl P {
    fn peek(&self) -> Option<char> {
        self.c.get(self.i).copied()
    }

    fn peek_at(&self, k: usize) -> Option<char> {
        self.c.get(self.i + k).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let ch = self.c.get(self.i).copied();
        if let Some(c) = ch {
            self.i += 1;
            if c == '\n' {
                self.line += 1;
            }
        }
        ch
    }

    fn skip_blanks(&mut self) {
        loop {
            match self.peek() {
                Some(' ') | Some('\t') | Some('\r') => {
                    self.bump();
                }
                Some('\\') if self.peek_at(1) == Some('\n') => {
                    self.bump();
                    self.bump();
                }
                _ => return,
            }
        }
    }

    /// If the next word starts with `NAME=`, consume that and return NAME.
    fn assignment_name(&mut self) -> Option<String> {
        if !self.peek().is_some_and(is_name_start) {
            return None;
        }
        let mut k = 0;
        while self.peek_at(k).is_some_and(is_name_char) {
            k += 1;
        }
        if self.peek_at(k) != Some('=') {
            return None;
        }
        let name: String = self.c[self.i..self.i + k].iter().collect();
        self.i += k + 1;
        Some(name)
    }

    fn unsupported<T>(&self, what: &str) -> Result<T, String> {
        Err(format!("line {}: {} is not supported in this file", self.line, what))
    }

    /// One shell word (quote removal + parameter expansion; no splitting or globbing, as
    /// in an assignment). GET returns None for an unset name.
    fn word(&mut self, get: &dyn Fn(&str) -> Option<String>, assignment: bool) -> Result<String, String> {
        let mut out = String::new();
        let start = self.i;
        while let Some(c) = self.peek() {
            match c {
                ' ' | '\t' | '\r' | '\n' | ';' => break,
                '#' if self.i == start && !assignment => break,
                '~' if self.i == start && assignment && matches!(self.peek_at(1), None | Some('/' | ' ' | '\t' | '\r' | '\n' | ';')) => {
                    self.bump();
                    out.push_str(&get("HOME").unwrap_or_default());
                }
                '\'' => {
                    self.bump();
                    loop {
                        match self.bump() {
                            None => return Err(format!("line {}: unterminated ' quote", self.line)),
                            Some('\'') => break,
                            Some(ch) => out.push(ch),
                        }
                    }
                }
                '"' => {
                    self.bump();
                    loop {
                        match self.peek() {
                            None => return Err(format!("line {}: unterminated \" quote", self.line)),
                            Some('"') => {
                                self.bump();
                                break;
                            }
                            Some('\\') => {
                                self.bump();
                                match self.bump() {
                                    Some('\n') => {}
                                    Some(ch @ ('$' | '`' | '"' | '\\')) => out.push(ch),
                                    Some(ch) => {
                                        out.push('\\');
                                        out.push(ch);
                                    }
                                    None => return Err(format!("line {}: unterminated \" quote", self.line)),
                                }
                            }
                            Some('$') => self.dollar(&mut out, get)?,
                            Some('`') => return self.unsupported("command substitution (`...`)"),
                            Some(ch) => {
                                self.bump();
                                out.push(ch);
                            }
                        }
                    }
                }
                '\\' => {
                    self.bump();
                    match self.bump() {
                        Some('\n') | None => {}
                        Some(ch) => out.push(ch),
                    }
                }
                '$' => self.dollar(&mut out, get)?,
                '`' => return self.unsupported("command substitution (`...`)"),
                '|' | '&' | '<' | '>' | '(' | ')' => return self.unsupported(&format!("'{}'", c)),
                _ => {
                    self.bump();
                    out.push(c);
                }
            }
        }
        Ok(out)
    }

    fn dollar(&mut self, out: &mut String, get: &dyn Fn(&str) -> Option<String>) -> Result<(), String> {
        self.bump(); // '$'
        match self.peek() {
            Some(c) if is_name_start(c) => {
                let mut name = String::new();
                while let Some(c) = self.peek().filter(|c| is_name_char(*c)) {
                    name.push(c);
                    self.bump();
                }
                out.push_str(&get(&name).unwrap_or_default());
            }
            Some('{') => {
                self.bump();
                let mut name = String::new();
                while let Some(c) = self.peek().filter(|c| is_name_char(*c)) {
                    name.push(c);
                    self.bump();
                }
                if name.is_empty() || !is_name_start(name.chars().next().unwrap_or('0')) {
                    return self.unsupported("this ${...} form");
                }
                let mut default: Option<(bool, String)> = None;
                if self.peek() == Some(':') && self.peek_at(1) == Some('-') {
                    self.i += 2;
                    default = Some((true, String::new()));
                } else if self.peek() == Some('-') {
                    self.i += 1;
                    default = Some((false, String::new()));
                }
                if let Some((_, d)) = default.as_mut() {
                    loop {
                        match self.peek() {
                            None | Some('\n') => return Err(format!("line {}: unterminated ${{", self.line)),
                            Some('}') => break,
                            Some('$' | '`' | '{' | '\'' | '"' | '\\') => {
                                return self.unsupported("expansion or quoting inside ${NAME:-default}")
                            }
                            Some(c) => {
                                d.push(c);
                                self.bump();
                            }
                        }
                    }
                }
                if self.bump() != Some('}') {
                    return self.unsupported("this ${...} form");
                }
                let v = get(&name);
                match (default, v) {
                    (Some((true, d)), None) => out.push_str(&d),
                    (Some((true, d)), Some(v)) if v.is_empty() => out.push_str(&d),
                    (Some((false, d)), None) => out.push_str(&d),
                    (_, v) => out.push_str(&v.unwrap_or_default()),
                }
            }
            Some('(') => return self.unsupported("command substitution $(...)"),
            Some(c) if c.is_ascii_digit() || "@*#?$!-".contains(c) => return self.unsupported(&format!("${}", c)),
            _ => out.push('$'), // a lone '$' is literal
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(text: &str) -> Result<HashMap<String, String>, String> {
        let mut v = HashMap::new();
        v.insert("LAB_OS".to_string(), String::new());
        parse_text(text, &mut v, &|n| if n == "HOME" { Some("/home/u".into()) } else { None })?;
        Ok(v)
    }

    #[test]
    fn lab_conf_forms() {
        let v = run(concat!(
            "# comment\n",
            "LAB_OS=2012              # DC operating system\n",
            "LAB_CLIENT_OS=$LAB_OS\n",
            "LAB_ADMIN_USER_PASSWORD='DomAdm!2026'\n",
            "  LAB_USER=student                     # domain user\n",
            "export LAB_NET=\"10.77.${LAB_OS}\" LAB_X=a\\ b;LAB_Y=${NOPE:-dflt}\n",
            "LAB_Z=\"q\\\"\\$x\" LAB_E= LAB_T=~/x LAB_H=a#b\n",
            "LAB_C=one\\\ntwo\n",
        ))
        .unwrap();
        assert_eq!(v["LAB_OS"], "2012");
        assert_eq!(v["LAB_CLIENT_OS"], "2012");
        assert_eq!(v["LAB_ADMIN_USER_PASSWORD"], "DomAdm!2026");
        assert_eq!(v["LAB_USER"], "student");
        assert_eq!(v["LAB_NET"], "10.77.2012");
        assert_eq!(v["LAB_X"], "a b");
        assert_eq!(v["LAB_Y"], "dflt");
        assert_eq!(v["LAB_Z"], "q\"$x");
        assert_eq!(v["LAB_E"], "");
        assert_eq!(v["LAB_T"], "/home/u/x");
        assert_eq!(v["LAB_H"], "a#b");
        assert_eq!(v["LAB_C"], "onetwo");
    }

    #[test]
    fn rejects_commands() {
        assert!(run("echo hi\n").unwrap_err().contains("line 1"));
        assert!(run("A=1\nB=$(date)\n").unwrap_err().contains("line 2"));
        assert!(run("A=`date`\n").is_err());
        assert!(run("A=1 | x\n").is_err());
        assert!(run("A='open\n").is_err());
    }

    #[test]
    fn real_lab_files_parse() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../labs");
        let mut n = 0;
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) == Some("conf") {
                let v = run(&std::fs::read_to_string(&p).unwrap()).unwrap_or_else(|e| panic!("{}: {}", p.display(), e));
                assert!(!v["LAB_OS"].is_empty(), "{}", p.display());
                n += 1;
            }
        }
        assert!(n >= 3);
    }
}
