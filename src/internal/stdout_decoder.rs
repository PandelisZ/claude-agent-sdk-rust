//! NDJSON framing for the CLI's stdout, mirroring the Python SDK's
//! `_LineFramer` + `_parse_stdout_line` (`_internal/transport/subprocess_cli.py`).
//!
//! The CLI writes one JSON message per line. Bytes are buffered raw and split
//! on `b'\n'`, so a multi-byte UTF-8 character straddling two reads is never
//! decoded half-way. Each complete line is trimmed; blank lines and lines not
//! starting with `{` (e.g. `[SandboxDebug] ...`) are skipped; a `{` line that
//! does not parse as exactly one JSON value is an error. At EOF a residual
//! tail is parsed, and dropped (not raised) when it is truncated JSON.

use std::collections::VecDeque;

use serde_json::Value;

use crate::error::{CLIJSONDecodeError, Result};

#[derive(Debug)]
pub(crate) struct StdoutDecoder {
    /// Bytes of the line currently being received (never contains `\n`).
    partial: Vec<u8>,
    pending: VecDeque<Vec<u8>>,
    max_buffer_size: usize,
}

impl StdoutDecoder {
    pub(crate) fn new(max_buffer_size: usize) -> Self {
        Self {
            partial: Vec::new(),
            pending: VecDeque::new(),
            max_buffer_size,
        }
    }

    /// Feed a chunk of text.
    #[allow(dead_code)]
    pub(crate) fn push(&mut self, chunk: &str) -> Result<()> {
        self.push_bytes(chunk.as_bytes())
    }

    /// Feed a raw chunk of stdout bytes; chunk boundaries may fall anywhere,
    /// including inside a multi-byte UTF-8 character.
    pub(crate) fn push_bytes(&mut self, chunk: &[u8]) -> Result<()> {
        let mut rest = chunk;
        while let Some(newline) = rest.iter().position(|byte| *byte == b'\n') {
            self.partial.extend_from_slice(&rest[..newline]);
            rest = &rest[newline + 1..];
            let line = std::mem::take(&mut self.partial);
            self.guard(line.len())?;
            if let Some(json) = parse_stdout_line(&line)? {
                self.pending.push_back(json);
            }
        }
        self.partial.extend_from_slice(rest);
        self.guard(self.partial.len())
    }

    pub(crate) fn next(&mut self) -> Option<Vec<u8>> {
        self.pending.pop_front()
    }

    /// Flush the trailing partial line at EOF. A complete JSON tail (a
    /// producer that omits the final newline) is yielded; a truncated one is
    /// unrecoverable and dropped.
    pub(crate) fn finish(&mut self) -> Result<()> {
        let tail = std::mem::take(&mut self.partial);
        match parse_stdout_line(&tail) {
            Ok(Some(json)) => self.pending.push_back(json),
            Ok(None) => {}
            Err(_) => {
                tracing::debug!(
                    "Dropping truncated JSON at end of CLI stdout: {}",
                    preview(&tail, 200)
                );
            }
        }
        Ok(())
    }

    /// Bound a single message, whether it is complete yet or not.
    fn guard(&self, length: usize) -> Result<()> {
        if length > self.max_buffer_size {
            return Err(CLIJSONDecodeError::new(
                format!(
                    "JSON message exceeded maximum buffer size of {} bytes",
                    self.max_buffer_size
                ),
                format!(
                    "Buffer size {} exceeds limit {}",
                    length, self.max_buffer_size
                ),
            )
            .into());
        }
        Ok(())
    }
}

fn preview(bytes: &[u8], max_chars: usize) -> String {
    String::from_utf8_lossy(bytes)
        .chars()
        .take(max_chars)
        .collect()
}

/// Parse one complete stdout line. Returns `None` for lines carrying no
/// message (blank, or non-JSON output); errors for a corrupt JSON line.
fn parse_stdout_line(line: &[u8]) -> Result<Option<Vec<u8>>> {
    let line = line.trim_ascii();
    if line.is_empty() {
        return Ok(None);
    }
    if !line.starts_with(b"{") {
        tracing::debug!(
            "Skipping non-JSON line from CLI stdout: {}",
            preview(line, 200)
        );
        return Ok(None);
    }
    match serde_json::from_slice::<Value>(line) {
        Ok(_) => Ok(Some(line.to_vec())),
        Err(error) => {
            Err(CLIJSONDecodeError::new(String::from_utf8_lossy(line).into_owned(), error).into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::StdoutDecoder;

    fn next_json(decoder: &mut StdoutDecoder) -> serde_json::Value {
        serde_json::from_slice(&decoder.next().expect("a message")).unwrap()
    }

    #[test]
    fn parses_multiple_json_lines_from_one_chunk() {
        let mut decoder = StdoutDecoder::new(1024);
        decoder
            .push("{\"type\":\"message\",\"id\":\"msg1\"}\n\n{\"type\":\"result\",\"id\":\"res1\"}")
            .unwrap();

        assert_eq!(next_json(&mut decoder)["id"], "msg1");
        // The second line has no trailing newline yet: held until EOF.
        assert!(decoder.next().is_none());
        decoder.finish().unwrap();
        assert_eq!(next_json(&mut decoder)["id"], "res1");
        assert!(decoder.next().is_none());
    }

    #[test]
    fn parses_json_split_across_chunks() {
        let mut decoder = StdoutDecoder::new(1024);
        decoder.push(r#"{"type":"assistant","#).unwrap();
        assert!(decoder.next().is_none());
        decoder.push("\"message\":{\"content\":[]}}\n").unwrap();

        assert_eq!(next_json(&mut decoder)["type"], "assistant");
    }

    #[test]
    fn skips_non_json_debug_lines_and_crlf() {
        let mut decoder = StdoutDecoder::new(1024);
        decoder
            .push("[SandboxDebug] ignored\n   \n{\"type\":\"result\"}\r\n")
            .unwrap();

        assert_eq!(next_json(&mut decoder)["type"], "result");
    }

    #[test]
    fn enforces_max_buffer_size_for_incomplete_json() {
        let mut decoder = StdoutDecoder::new(8);
        let err = decoder.push("{\"data\":\"too long").unwrap_err();

        assert!(err.to_string().contains("exceeded maximum buffer size"));
    }

    #[test]
    fn enforces_max_buffer_size_per_complete_line() {
        let mut decoder = StdoutDecoder::new(8);
        let err = decoder.push("{\"data\":\"too long\"}\n").unwrap_err();
        match err {
            crate::error::ClaudeSDKError::CLIJSONDecode(err) => {
                assert_eq!(
                    err.line,
                    "JSON message exceeded maximum buffer size of 8 bytes"
                );
                assert_eq!(
                    err.original_error.to_string(),
                    "Buffer size 19 exceeds limit 8"
                );
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn multibyte_character_split_across_reads() {
        let mut decoder = StdoutDecoder::new(1024);
        let line = "{\"text\":\"héllo 日本 🎉\"}\n".as_bytes();
        // Split inside the 4-byte emoji.
        let split = line.len() - 5;
        decoder.push_bytes(&line[..split]).unwrap();
        assert!(decoder.next().is_none());
        decoder.push_bytes(&line[split..]).unwrap();
        assert_eq!(next_json(&mut decoder)["text"], "héllo 日本 🎉");
    }

    #[test]
    fn corrupt_line_is_an_error_not_merged_into_next_message() {
        let mut decoder = StdoutDecoder::new(1024);
        let err = decoder
            .push("{\"a\":\n{\"type\":\"result\"}\n")
            .unwrap_err();
        assert!(matches!(
            err,
            crate::error::ClaudeSDKError::CLIJSONDecode(ref e) if e.line == "{\"a\":"
        ));
    }

    #[test]
    fn trailing_garbage_and_two_objects_per_line_are_errors() {
        let mut decoder = StdoutDecoder::new(1024);
        assert!(decoder.push("{\"a\":1}garbage\n").is_err());
        let mut decoder = StdoutDecoder::new(1024);
        assert!(decoder.push("{\"a\":1}{\"b\":2}\n").is_err());
    }

    #[test]
    fn truncated_tail_at_eof_is_dropped() {
        let mut decoder = StdoutDecoder::new(1024);
        decoder
            .push("{\"type\":\"result\"}\n{\"type\":\"assis")
            .unwrap();
        assert_eq!(next_json(&mut decoder)["type"], "result");
        decoder.finish().expect("truncated tail must not raise");
        assert!(decoder.next().is_none());
    }
}
