//! Newline-delimited JSON decoding (used by Ollama and some self-hosted
//! servers that stream `application/x-ndjson`).

use crate::error::{EngineError, Result};
use futures::{Stream, StreamExt};
use std::collections::VecDeque;
use std::pin::Pin;

/// Incremental NDJSON line splitter.
#[derive(Debug, Default)]
pub struct NdjsonDecoder {
    buf: Vec<u8>,
}

impl NdjsonDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed bytes; return every complete non-empty line.
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=pos).collect();
            let mut end = line.len().saturating_sub(1);
            if end > 0 && line[end - 1] == b'\r' {
                end -= 1;
            }
            let s = String::from_utf8_lossy(&line[..end]).trim().to_string();
            if !s.is_empty() {
                out.push(s);
            }
        }
        out
    }

    /// Flush a trailing line at end-of-stream.
    pub fn finish(&mut self) -> Vec<String> {
        let line = std::mem::take(&mut self.buf);
        let s = String::from_utf8_lossy(&line).trim().to_string();
        if s.is_empty() {
            Vec::new()
        } else {
            vec![s]
        }
    }
}

/// Stream of NDJSON lines.
pub type NdjsonStream = Pin<Box<dyn Stream<Item = Result<String>> + Send>>;

/// Convert a response body into a stream of NDJSON line strings.
pub fn ndjson_stream(response: reqwest::Response) -> NdjsonStream {
    let byte_stream = response.bytes_stream();
    map_lines(Box::pin(byte_stream))
}

/// Map a byte stream into NDJSON lines.
pub fn map_lines<S>(stream: S) -> NdjsonStream
where
    S: Stream<Item = std::result::Result<bytes::Bytes, reqwest::Error>> + Send + 'static + Unpin,
{
    Box::pin(futures::stream::unfold(
        (stream, NdjsonDecoder::new(), VecDeque::<String>::new(), false),
        |(mut bs, mut dec, mut pending, mut done)| async move {
            loop {
                if let Some(line) = pending.pop_front() {
                    return Some((Ok(line), (bs, dec, pending, done)));
                }
                if done {
                    return None;
                }
                match bs.next().await {
                    Some(Ok(chunk)) => {
                        for l in dec.feed(&chunk) {
                            pending.push_back(l);
                        }
                    }
                    Some(Err(e)) => {
                        done = true;
                        return Some((
                            Err(EngineError::Network(e.to_string())),
                            (bs, dec, pending, done),
                        ));
                    }
                    None => {
                        for l in dec.finish() {
                            pending.push_back(l);
                        }
                        done = true;
                        if pending.is_empty() {
                            return None;
                        }
                    }
                }
            }
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_lines_across_chunks() {
        let mut d = NdjsonDecoder::new();
        let mut out = d.feed(b"{\"a\":1}\n{\"b\"");
        assert_eq!(out, vec!["{\"a\":1}"]);
        out = d.feed(b":2}\n");
        assert_eq!(out, vec!["{\"b\":2}"]);
        assert!(d.finish().is_empty());
    }

    #[test]
    fn handles_crlf_and_trailing() {
        let mut d = NdjsonDecoder::new();
        let out = d.feed(b"one\r\ntwo");
        assert_eq!(out, vec!["one"]);
        assert_eq!(d.finish(), vec!["two"]);
    }

    #[tokio::test]
    async fn maps_byte_stream_to_lines() {
        let raw = futures::stream::iter(vec![
            Ok(bytes::Bytes::from_static(b"{\"x\":1}\n")),
            Ok(bytes::Bytes::from_static(b"{\"x\":2}\n")),
        ]);
        let mut s = map_lines(raw);
        let mut got = Vec::new();
        while let Some(l) = s.next().await {
            got.push(l.unwrap());
        }
        assert_eq!(got, vec!["{\"x\":1}", "{\"x\":2}"]);
    }
}
