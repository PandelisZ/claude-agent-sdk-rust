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

    fn write_f64<W>(&mut self, writer: &mut W, value: f64) -> io::Result<()>
    where
        W: ?Sized + Write,
    {
        writer.write_all(py_float_repr(value).as_bytes())
    }

    fn write_f32<W>(&mut self, writer: &mut W, value: f32) -> io::Result<()>
    where
        W: ?Sized + Write,
    {
        self.write_f64(writer, f64::from(value))
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

/// Python `float.__repr__` for a finite value: the shortest round-trip
/// digits, in fixed notation when the decimal exponent is in `-4..=15`
/// (always with a fractional part, e.g. `1.0`), otherwise in scientific
/// notation with a signed, at least two-digit exponent (`1e+16`, `1.5e-05`).
fn py_float_repr(value: f64) -> String {
    if !value.is_finite() {
        // Unreachable for `serde_json::Value`, which cannot hold NaN/inf.
        return "null".to_string();
    }
    // Rust's `{:e}` yields the shortest round-trip digits: `-1.2345e-7`.
    let sci = format!("{value:e}");
    let (mantissa, exponent) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let (sign, mantissa) = match mantissa.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", mantissa),
    };
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();

    if value == 0.0 || (-4..16).contains(&exponent) {
        let point = exponent + 1;
        let body = if point <= 0 {
            format!("0.{}{digits}", "0".repeat(point.unsigned_abs() as usize))
        } else if point as usize >= digits.len() {
            format!("{digits}{}.0", "0".repeat(point as usize - digits.len()))
        } else {
            let (int_part, frac_part) = digits.split_at(point as usize);
            format!("{int_part}.{frac_part}")
        };
        return format!("{sign}{body}");
    }
    let exp_sign = if exponent < 0 { '-' } else { '+' };
    format!("{sign}{mantissa}e{exp_sign}{:02}", exponent.unsigned_abs())
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
        // Expected values are Python's `json.dumps(float)` output.
        let floats = [
            (1.0, "1.0"),
            (0.1, "0.1"),
            (1e16, "1e+16"),
            (1e15, "1000000000000000.0"),
            (123.456, "123.456"),
            (1.5e-7, "1.5e-07"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (-0.0, "-0.0"),
            (2.5e300, "2.5e+300"),
            (1234567890123456.7, "1234567890123456.8"),
            (-2.75, "-2.75"),
            (1e22, "1e+22"),
            (5e-324, "5e-324"),
        ];
        for (value, expected) in floats {
            assert_eq!(
                dumps_compact(&serde_json::json!(value)),
                expected,
                "{value}"
            );
        }
        assert_eq!(
            dumps_compact(&serde_json::json!({"n": 1, "f": 2.0})),
            r#"{"n":1,"f":2.0}"#
        );
        assert_eq!(
            dumps_compact(&serde_json::json!("a\u{7f}\n\"")),
            r#""a\u007f\n\"""#
        );
    }
}
