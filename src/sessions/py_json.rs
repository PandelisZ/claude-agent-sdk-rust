//! Byte-compatible ports of Python's `json.dumps` used by the upstream SDK.
//!
//! The Python SDK writes transcript lines with `json.dumps(obj,
//! separators=(",", ":"))` (and occasionally with the default `", "` / `": "`
//! separators). Both forms use `ensure_ascii=True`, so every code point at or
//! above U+007F is written as a lowercase `\uXXXX` escape (UTF-16 surrogate
//! pairs above the BMP). Matching those bytes matters because the lite
//! session readers slice transcripts at fixed byte offsets and scan for
//! literal prefixes such as `{"type":"tag"`.

use serde::Serialize;
use serde_json::ser::Formatter;
use std::io::{self, Write};

#[derive(Clone, Copy)]
struct PyFormatter {
    compact: bool,
}

impl Formatter for PyFormatter {
    fn begin_array_value<W>(&mut self, writer: &mut W, first: bool) -> io::Result<()>
    where
        W: ?Sized + Write,
    {
        if first {
            Ok(())
        } else if self.compact {
            writer.write_all(b",")
        } else {
            writer.write_all(b", ")
        }
    }

    fn begin_object_key<W>(&mut self, writer: &mut W, first: bool) -> io::Result<()>
    where
        W: ?Sized + Write,
    {
        self.begin_array_value(writer, first)
    }

    fn begin_object_value<W>(&mut self, writer: &mut W) -> io::Result<()>
    where
        W: ?Sized + Write,
    {
        if self.compact {
            writer.write_all(b":")
        } else {
            writer.write_all(b": ")
        }
    }

    fn write_string_fragment<W>(&mut self, writer: &mut W, fragment: &str) -> io::Result<()>
    where
        W: ?Sized + Write,
    {
        let mut start = 0;
        for (idx, ch) in fragment.char_indices() {
            if (ch as u32) < 0x7f {
                continue;
            }
            writer.write_all(&fragment.as_bytes()[start..idx])?;
            let mut units = [0u16; 2];
            for unit in ch.encode_utf16(&mut units) {
                write!(writer, "\\u{unit:04x}")?;
            }
            start = idx + ch.len_utf8();
        }
        writer.write_all(&fragment.as_bytes()[start..])
    }
}

fn dumps_with<T: Serialize + ?Sized>(value: &T, compact: bool) -> String {
    let mut out = Vec::new();
    let mut serializer = serde_json::Serializer::with_formatter(&mut out, PyFormatter { compact });
    // Serializing a `serde_json::Value`/`Map` into memory cannot fail.
    value
        .serialize(&mut serializer)
        .expect("JSON value serialization is infallible");
    // The formatter only ever writes ASCII.
    String::from_utf8(out).expect("ensure_ascii output is ASCII")
}

/// `json.dumps(value, separators=(",", ":"))`.
pub(crate) fn dumps_compact<T: Serialize + ?Sized>(value: &T) -> String {
    dumps_with(value, true)
}

/// `json.dumps(value)` (default `", "` / `": "` separators).
pub(crate) fn dumps_default<T: Serialize + ?Sized>(value: &T) -> String {
    dumps_with(value, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_python_compact_and_default_output() {
        let value = serde_json::json!({"type": "tag", "tag": "caf\u{e9} \u{1f600}", "n": [1, 2]});
        assert_eq!(
            dumps_compact(&value),
            r#"{"type":"tag","tag":"caf\u00e9 \ud83d\ude00","n":[1,2]}"#
        );
        assert_eq!(
            dumps_default(&value),
            r#"{"type": "tag", "tag": "caf\u00e9 \ud83d\ude00", "n": [1, 2]}"#
        );
        assert_eq!(
            dumps_compact(&serde_json::json!("a\u{7f}\n\"")),
            r#""a\u007f\n\"""#
        );
    }
}
