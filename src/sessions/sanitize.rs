//! Tag sanitization: a port of upstream `_sanitize_unicode`
//! (`_internal/session_mutations.py`).

use crate::error::{ClaudeSDKError, Result};
use unicode_general_category::{get_general_category, GeneralCategory};
use unicode_normalization::UnicodeNormalization;

fn is_stripped(ch: char) -> bool {
    matches!(
        get_general_category(ch),
        GeneralCategory::Format | GeneralCategory::PrivateUse | GeneralCategory::Unassigned
    ) || matches!(
        ch,
        '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}'
            | '\u{feff}'
            | '\u{e000}'..='\u{f8ff}'
    )
}

/// Iteratively NFKC-normalize and strip format (Cf), private-use (Co) and
/// unassigned (Cn) characters plus the explicit zero-width/bidi ranges, until
/// stable (at most 10 rounds). Control characters (Cc) are kept, like Python.
pub(crate) fn sanitize_unicode(value: &str) -> String {
    let mut current = value.to_string();
    for _ in 0..10 {
        let next: String = current.nfkc().filter(|ch| !is_stripped(*ch)).collect();
        if next == current {
            break;
        }
        current = next;
    }
    current
}

/// Python `str.isspace()`: Rust's `White_Space` plus the U+001C..U+001F
/// information separators, which Python also treats as whitespace.
pub(crate) fn is_py_space(ch: char) -> bool {
    ch.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&ch)
}

/// Python `str.strip()` with no arguments.
pub(crate) fn py_strip(value: &str) -> &str {
    value.trim_matches(is_py_space)
}

/// Python `str.rstrip()` with no arguments.
pub(crate) fn py_rstrip(value: &str) -> &str {
    value.trim_end_matches(is_py_space)
}

/// Tag validation shared by `tag_session` and `tag_session_via_store`.
pub(crate) fn sanitize_tag(tag: &str) -> Result<String> {
    let sanitized = py_strip(&sanitize_unicode(tag)).to_string();
    if sanitized.is_empty() {
        Err(ClaudeSDKError::Session(
            "tag must be non-empty (use None to clear)".to_string(),
        ))
    } else {
        Ok(sanitized)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_format_private_use_and_normalizes() {
        assert_eq!(sanitize_unicode("a\u{200b}b\u{202e}c\u{feff}"), "abc");
        assert_eq!(sanitize_unicode("x\u{e000}y"), "xy");
        // NFKC folds the fullwidth letter and the ligature.
        assert_eq!(sanitize_unicode("\u{ff21}\u{fb01}"), "Afi");
        // Control characters are not stripped (only trimmed at the ends).
        assert_eq!(sanitize_unicode("a\tb"), "a\tb");
        assert_eq!(sanitize_tag("  a\tb \n").unwrap(), "a\tb");
        assert!(sanitize_tag("\u{200b}\u{200d} ").is_err());
        // Python's strip() also removes the U+001C..U+001F separators.
        assert_eq!(sanitize_tag("\u{1c}x\u{1f}").unwrap(), "x");
        assert_eq!(py_rstrip("a \u{3000}"), "a");
    }
}
