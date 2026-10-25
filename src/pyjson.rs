//! serde_json output formatted like Python's `json.dumps`: ", " / ": " separators,
//! ensure_ascii (non-ASCII as \uXXXX), optional `indent=N`. The control disc's lab.json must
//! be pure ASCII (PowerShell 4 reads it without a BOM), and the `outputs` summary stays
//! byte-identical to lib/outputs.py's.

use serde::Serialize;
use serde_json::ser::Formatter;
use std::io::{self, Write};

struct PyFormatter {
    indent: Option<usize>,
    level: usize,
    has_value: bool,
}

impl PyFormatter {
    fn newline<W: ?Sized + Write>(&self, w: &mut W) -> io::Result<()> {
        if let Some(n) = self.indent {
            w.write_all(b"\n")?;
            w.write_all(" ".repeat(n * self.level).as_bytes())?;
        }
        Ok(())
    }

    fn item<W: ?Sized + Write>(&self, w: &mut W, first: bool) -> io::Result<()> {
        if !first {
            w.write_all(if self.indent.is_some() { b"," } else { b", " })?;
        }
        self.newline(w)
    }

    fn open<W: ?Sized + Write>(&mut self, w: &mut W, c: &[u8]) -> io::Result<()> {
        self.level += 1;
        self.has_value = false;
        w.write_all(c)
    }

    fn close<W: ?Sized + Write>(&mut self, w: &mut W, c: &[u8]) -> io::Result<()> {
        self.level -= 1;
        if self.has_value {
            self.newline(w)?;
        }
        w.write_all(c)
    }
}

impl Formatter for PyFormatter {
    fn begin_array<W: ?Sized + Write>(&mut self, w: &mut W) -> io::Result<()> {
        self.open(w, b"[")
    }

    fn end_array<W: ?Sized + Write>(&mut self, w: &mut W) -> io::Result<()> {
        self.close(w, b"]")
    }

    fn begin_array_value<W: ?Sized + Write>(&mut self, w: &mut W, first: bool) -> io::Result<()> {
        self.item(w, first)
    }

    fn end_array_value<W: ?Sized + Write>(&mut self, _w: &mut W) -> io::Result<()> {
        self.has_value = true;
        Ok(())
    }

    fn begin_object<W: ?Sized + Write>(&mut self, w: &mut W) -> io::Result<()> {
        self.open(w, b"{")
    }

    fn end_object<W: ?Sized + Write>(&mut self, w: &mut W) -> io::Result<()> {
        self.close(w, b"}")
    }

    fn begin_object_key<W: ?Sized + Write>(&mut self, w: &mut W, first: bool) -> io::Result<()> {
        self.item(w, first)
    }

    fn begin_object_value<W: ?Sized + Write>(&mut self, w: &mut W) -> io::Result<()> {
        w.write_all(b": ")
    }

    fn end_object_value<W: ?Sized + Write>(&mut self, _w: &mut W) -> io::Result<()> {
        self.has_value = true;
        Ok(())
    }

    /// serde_json escapes quotes, backslashes and control characters itself (the same way
    /// Python does); everything else outside printable ASCII is escaped here.
    fn write_string_fragment<W: ?Sized + Write>(&mut self, w: &mut W, fragment: &str) -> io::Result<()> {
        let mut start = 0;
        for (i, c) in fragment.char_indices() {
            if (' '..='~').contains(&c) {
                continue;
            }
            w.write_all(&fragment.as_bytes()[start..i])?;
            let mut buf = [0u16; 2];
            for u in c.encode_utf16(&mut buf) {
                write!(w, "\\u{:04x}", u)?;
            }
            start = i + c.len_utf8();
        }
        w.write_all(&fragment.as_bytes()[start..])
    }
}

fn to_string<T: Serialize + ?Sized>(v: &T, indent: Option<usize>) -> String {
    let mut out = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(&mut out, PyFormatter { indent, level: 0, has_value: false });
    v.serialize(&mut ser).expect("JSON values with string keys always serialize");
    String::from_utf8(out).expect("the formatter writes ASCII")
}

/// `json.dumps(v)`
pub fn dumps<T: Serialize + ?Sized>(v: &T) -> String {
    to_string(v, None)
}

/// `json.dumps(v, indent=n)`
pub fn dumps_indent<T: Serialize + ?Sized>(v: &T, n: usize) -> String {
    to_string(v, Some(n))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn like_python() {
        let v = json!({"a": 1, "b": ["x", null, true], "c": {}, "d": [], "e": "é\"\\\n\u{1F600}\u{7f}\u{1}"});
        // python3 -c 'import json; print(json.dumps(...))'; the expected escapes are built with
        // format! so no editor can turn them into the characters they stand for
        let want = format!(
            r#"{{"a": 1, "b": ["x", null, true], "c": {{}}, "d": [], "e": "{b}u00e9{b}"{b}{b}{b}n{b}ud83d{b}ude00{b}u007f{b}u0001"}}"#,
            b = '\\'
        );
        assert_eq!(dumps(&v), want);
        let v = json!({"m": [{"k": 2}, []], "n": 3, "o": {}});
        assert_eq!(dumps_indent(&v, 1), "{\n \"m\": [\n  {\n   \"k\": 2\n  },\n  []\n ],\n \"n\": 3,\n \"o\": {}\n}");
        assert_eq!(dumps_indent(&json!([]), 1), "[]");
    }
}
