//! NDJSON framing for the CLI's stdout, mirroring the Python SDK's
//! `_LineFramer` + `_parse_stdout_line` (`_internal/transport/subprocess_cli.py`).
//!
//! The CLI writes one JSON message per line. Bytes are buffered raw and split
//! on `b'\n'`, so a multi-byte UTF-8 character straddling two reads is never
//! decoded half-way. Each complete line is trimmed; blank lines and lines not
//! starting with `{` (e.g. `[SandboxDebug] ...`) are skipped; a `{` line that
//! does not parse as exactly one JSON value is an error. At EOF a residual
//! tail is parsed, and dropped (not raised) when it is truncated JSON.
//!
//! A bad line does not cost the frames before it: as upstream yields each
//! line as it is parsed, the frames that precede a corrupt or oversized line
//! in the same chunk are delivered first, then the error. Decoding stops at
//! the error.

use std::collections::VecDeque;

use serde_json::Value;

use crate::error::{CLIJSONDecodeError, ClaudeSDKError, Result};

#[derive(Debug)]
pub(crate) struct StdoutDecoder {
    /// Bytes of the line currently being received (never contains `\n`).
    partial: Vec<u8>,
    pending: VecDeque<Vec<u8>>,
    /// The decode error that stopped decoding, delivered once `pending`
    /// (the frames before it) has drained.
    error: Option<ClaudeSDKError>,
    /// Decoding stopped at an error; later input is ignored.
    failed: bool,
    max_buffer_size: usize,
}

impl StdoutDecoder {
    pub(crate) fn new(max_buffer_size: usize) -> Self {
        Self {
            partial: Vec::new(),
            pending: VecDeque::new(),
            error: None,
            failed: false,
            max_buffer_size,
        }
    }

    /// Feed a chunk of text.
    #[cfg(test)]
    pub(crate) fn push(&mut self, chunk: &str) {
        self.push_bytes(chunk.as_bytes())
    }

    /// Feed a raw chunk of stdout bytes; chunk boundaries may fall anywhere,
    /// including inside a multi-byte UTF-8 character. A bad line is queued
    /// as an error behind the frames decoded before it.
    pub(crate) fn push_bytes(&mut self, chunk: &[u8]) {
        if self.failed {
            return;
        }
        if let Err(error) = self.decode(chunk) {
            self.failed = true;
            self.partial.clear();
            self.error = Some(error);
        }
    }

    fn decode(&mut self, chunk: &[u8]) -> Result<()> {
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

    /// The next decoded frame, else the error that stopped decoding (once).
    pub(crate) fn next_item(&mut self) -> Option<Result<Vec<u8>>> {
        match self.pending.pop_front() {
            Some(frame) => Some(Ok(frame)),
            None => self.error.take().map(Err),
        }
    }

    /// Flush the trailing partial line at EOF. A complete JSON tail (a
    /// producer that omits the final newline) is yielded; a truncated one is
    /// unrecoverable and dropped.
    pub(crate) fn finish(&mut self) {
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
    use crate::error::ClaudeSDKError;

    fn next_json(decoder: &mut StdoutDecoder) -> serde_json::Value {
        let frame = decoder.next_item().expect("a message").expect("a frame");
        serde_json::from_slice(&frame).unwrap()
    }

    fn next_err(decoder: &mut StdoutDecoder) -> ClaudeSDKError {
        match decoder.next_item() {
            Some(Err(error)) => error,
            Some(Ok(frame)) => panic!("expected an error, got {}", String::from_utf8_lossy(&frame)),
            None => panic!("expected an error, got nothing"),
        }
    }

    fn is_drained(decoder: &mut StdoutDecoder) -> bool {
        decoder.next_item().is_none()
    }

    #[test]
    fn parses_multiple_json_lines_from_one_chunk() {
        let mut decoder = StdoutDecoder::new(1024);
        decoder.push(
            "{\"type\":\"message\",\"id\":\"msg1\"}\n\n{\"type\":\"result\",\"id\":\"res1\"}",
        );

        assert_eq!(next_json(&mut decoder)["id"], "msg1");
        // The second line has no trailing newline yet: held until EOF.
        assert!(is_drained(&mut decoder));
        decoder.finish();
        assert_eq!(next_json(&mut decoder)["id"], "res1");
        assert!(is_drained(&mut decoder));
    }

    #[test]
    fn parses_json_split_across_chunks() {
        let mut decoder = StdoutDecoder::new(1024);
        decoder.push(r#"{"type":"assistant","#);
        assert!(is_drained(&mut decoder));
        decoder.push("\"message\":{\"content\":[]}}\n");

        assert_eq!(next_json(&mut decoder)["type"], "assistant");
    }

    #[test]
    fn skips_non_json_debug_lines_and_crlf() {
        let mut decoder = StdoutDecoder::new(1024);
        decoder.push("[SandboxDebug] ignored\n   \n{\"type\":\"result\"}\r\n");

        assert_eq!(next_json(&mut decoder)["type"], "result");
    }

    #[test]
    fn enforces_max_buffer_size_for_incomplete_json() {
        let mut decoder = StdoutDecoder::new(8);
        decoder.push("{\"data\":\"too long");
        let err = next_err(&mut decoder);

        assert!(err.to_string().contains("exceeded maximum buffer size"));
    }

    #[test]
    fn enforces_max_buffer_size_per_complete_line() {
        let mut decoder = StdoutDecoder::new(8);
        decoder.push("{\"data\":\"too long\"}\n");
        match next_err(&mut decoder) {
            ClaudeSDKError::CLIJSONDecode(err) => {
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
        decoder.push_bytes(&line[..split]);
        assert!(is_drained(&mut decoder));
        decoder.push_bytes(&line[split..]);
        assert_eq!(next_json(&mut decoder)["text"], "héllo 日本 🎉");
    }

    #[test]
    fn corrupt_line_is_an_error_not_merged_into_next_message() {
        let mut decoder = StdoutDecoder::new(1024);
        decoder.push("{\"a\":\n{\"type\":\"result\"}\n");
        let err = next_err(&mut decoder);
        assert!(matches!(
            err,
            ClaudeSDKError::CLIJSONDecode(ref e) if e.line == "{\"a\":"
        ));
        // Decoding stops at the error.
        assert!(is_drained(&mut decoder));
        decoder.push("{\"type\":\"later\"}\n");
        assert!(is_drained(&mut decoder));
    }

    #[test]
    fn frames_before_a_corrupt_line_in_the_same_chunk_come_first() {
        let mut decoder = StdoutDecoder::new(1024);
        decoder.push("{\"type\":\"assistant\"}\n{\"type\":\"result\"}\n{\"type\": oops\n");
        assert_eq!(next_json(&mut decoder)["type"], "assistant");
        assert_eq!(next_json(&mut decoder)["type"], "result");
        assert!(matches!(
            next_err(&mut decoder),
            ClaudeSDKError::CLIJSONDecode(ref e) if e.line == "{\"type\": oops"
        ));
        assert!(is_drained(&mut decoder));
    }

    #[test]
    fn frames_before_an_oversized_line_in_the_same_chunk_come_first() {
        let mut decoder = StdoutDecoder::new(24);
        decoder.push("{\"type\":\"result\"}\n{\"data\":\"far too long for the limit\"}\n");
        assert_eq!(next_json(&mut decoder)["type"], "result");
        assert!(next_err(&mut decoder)
            .to_string()
            .contains("exceeded maximum buffer size"));
    }

    #[test]
    fn trailing_garbage_and_two_objects_per_line_are_errors() {
        let mut decoder = StdoutDecoder::new(1024);
        decoder.push("{\"a\":1}garbage\n");
        next_err(&mut decoder);
        let mut decoder = StdoutDecoder::new(1024);
        decoder.push("{\"a\":1}{\"b\":2}\n");
        next_err(&mut decoder);
    }

    #[test]
    fn truncated_tail_at_eof_is_dropped() {
        let mut decoder = StdoutDecoder::new(1024);
        decoder.push("{\"type\":\"result\"}\n{\"type\":\"assis");
        assert_eq!(next_json(&mut decoder)["type"], "result");
        // A truncated tail must not raise.
        decoder.finish();
        assert!(is_drained(&mut decoder));
    }
}
