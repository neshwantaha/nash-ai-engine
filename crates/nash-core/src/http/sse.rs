//! A minimal, allocation-conscious Server-Sent Events decoder.
//!
//! Implemented locally (rather than pulled from a crate) so we control
//! buffering, error mapping and back-pressure precisely, and so we do not
//! buffer an entire response.

use crate::error::{EngineError, Result};
use bytes::Bytes;
use futures::{Stream, StreamExt};
use std::collections::VecDeque;
use std::pin::Pin;

/// One decoded SSE event.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
    pub id: Option<String>,
    pub retry: Option<u64>,
}

impl SseEvent {
    /// Whether the payload is the OpenAI `[DONE]` sentinel.
    pub fn is_done(&self) -> bool {
        self.data.trim() == "[DONE]"
    }
}

/// Incremental, chunk-safe SSE decoder. Feed arbitrary byte chunks; it returns
/// every complete event. UTF-8 sequences split across chunks are handled
/// because we only decode complete lines.
#[derive(Debug, Default)]
pub struct SseDecoder {
    buf: Vec<u8>,
    event: Option<String>,
    data: String,
    id: Option<String>,
    retry: Option<u64>,
    has_data: bool,
}

impl SseDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a chunk of bytes and return all complete events.
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=pos).collect();
            // strip trailing \n and optional \r
            let mut end = line.len().saturating_sub(1);
            if end > 0 && line[end - 1] == b'\r' {
                end -= 1;
            }
            let text = String::from_utf8_lossy(&line[..end]).into_owned();
            if let Some(ev) = self.handle_line(&text) {
                out.push(ev);
            }
        }
        out
    }

    /// Flush any trailing buffered line at end-of-stream.
    pub fn finish(&mut self) -> Vec<SseEvent> {
        let mut out = Vec::new();
        if !self.buf.is_empty() {
            let line = std::mem::take(&mut self.buf);
            let text = String::from_utf8_lossy(&line).into_owned();
            if let Some(ev) = self.handle_line(&text) {
                out.push(ev);
            }
        }
        if let Some(ev) = self.dispatch() {
            out.push(ev);
        }
        out
    }

    fn handle_line(&mut self, line: &str) -> Option<SseEvent> {
        if line.is_empty() {
            return self.dispatch();
        }
        // Comment / keep-alive.
        if let Some(rest) = line.strip_prefix(':') {
            let _ = rest;
            return None;
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "event" => self.event = Some(value.to_string()),
            "data" => {
                if self.has_data {
                    self.data.push('\n');
                }
                self.data.push_str(value);
                self.has_data = true;
            }
            "id" => self.id = Some(value.to_string()),
            "retry" => self.retry = value.parse().ok(),
            _ => {}
        }
        None
    }

    fn dispatch(&mut self) -> Option<SseEvent> {
        if !self.has_data && self.event.is_none() {
            return None;
        }
        let ev = SseEvent {
            event: self.event.take(),
            data: std::mem::take(&mut self.data),
            id: self.id.take(),
            retry: self.retry.take(),
        };
        self.has_data = false;
        Some(ev)
    }
}

/// Stream of decoded SSE events from an HTTP response body.
pub type SseStream = Pin<Box<dyn Stream<Item = Result<SseEvent>> + Send>>;

/// Convert a response's byte stream into a stream of SSE events without
/// buffering the body.
pub fn sse_stream(response: reqwest::Response) -> SseStream {
    let byte_stream = response.bytes_stream();
    Box::pin(futures::stream::unfold(
        (
            byte_stream,
            SseDecoder::new(),
            VecDeque::<SseEvent>::new(),
            false,
        ),
        |(mut bs, mut dec, mut pending, mut done)| async move {
            loop {
                if let Some(ev) = pending.pop_front() {
                    return Some((Ok(ev), (bs, dec, pending, done)));
                }
                if done {
                    return None;
                }
                match bs.next().await {
                    Some(Ok(chunk)) => {
                        for ev in dec.feed(&chunk) {
                            pending.push_back(ev);
                        }
                    }
                    Some(Err(e)) => {
                        done = true;
                        return Some((Err(map_stream_error(e)), (bs, dec, pending, done)));
                    }
                    None => {
                        for ev in dec.finish() {
                            pending.push_back(ev);
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

/// Convenience: reinterpret a raw chunk stream (already split into `Bytes`) as
/// SSE. Used for tests and for custom endpoints.
pub fn sse_from_bytes<S>(stream: S) -> SseStream
where
    S: Stream<Item = Result<Bytes>> + Send + 'static,
{
    Box::pin(futures::stream::unfold(
        (Box::pin(stream), SseDecoder::new(), VecDeque::<SseEvent>::new(), false),
        |(mut bs, mut dec, mut pending, mut done)| async move {
            loop {
                if let Some(ev) = pending.pop_front() {
                    return Some((Ok(ev), (bs, dec, pending, done)));
                }
                if done {
                    return None;
                }
                match bs.next().await {
                    Some(Ok(chunk)) => {
                        for ev in dec.feed(&chunk) {
                            pending.push_back(ev);
                        }
                    }
                    Some(Err(e)) => {
                        done = true;
                        return Some((Err(e), (bs, dec, pending, done)));
                    }
                    None => {
                        for ev in dec.finish() {
                            pending.push_back(ev);
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

fn map_stream_error(e: reqwest::Error) -> EngineError {
    if e.is_timeout() {
        EngineError::Timeout(std::time::Duration::from_secs(0))
    } else if e.is_decode() {
        EngineError::Decode(e.to_string())
    } else {
        EngineError::Network(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    fn collect(dec: &mut SseDecoder, chunks: &[&str]) -> Vec<SseEvent> {
        let mut out = Vec::new();
        for c in chunks {
            out.extend(dec.feed(c.as_bytes()));
        }
        out.extend(dec.finish());
        out
    }

    #[test]
    fn parses_simple_data_events() {
        let mut d = SseDecoder::new();
        let evs = collect(&mut d, &["data: {\"a\":1}\n\n", "data: [DONE]\n\n"]);
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[0].data, "{\"a\":1}");
        assert!(evs[1].is_done());
    }

    #[test]
    fn handles_event_field_and_id() {
        let mut d = SseDecoder::new();
        let evs = collect(
            &mut d,
            &["event: message_start\n", "id: 42\n", "data: hello\n\n"],
        );
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].event.as_deref(), Some("message_start"));
        assert_eq!(evs[0].id.as_deref(), Some("42"));
        assert_eq!(evs[0].data, "hello");
    }

    #[test]
    fn handles_chunk_boundaries() {
        let mut d = SseDecoder::new();
        let evs = collect(&mut d, &["da", "ta: par", "tial\n", "\n"]);
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].data, "partial");
    }

    #[test]
    fn handles_crlf_and_multiline_data() {
        let mut d = SseDecoder::new();
        let evs = collect(&mut d, &["data: line1\r\n", "data: line2\r\n\r\n"]);
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].data, "line1\nline2");
    }

    #[test]
    fn ignores_comments() {
        let mut d = SseDecoder::new();
        let evs = collect(&mut d, &[": keep-alive\n\n", "data: x\n\n"]);
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].data, "x");
    }

    #[test]
    fn utf8_split_across_chunks() {
        let mut d = SseDecoder::new();
        // "é" is 0xC3 0xA9 — split the bytes.
        let bytes = "data: héllo\n\n".as_bytes();
        let (a, b) = bytes.split_at(7);
        let mut evs = d.feed(a);
        evs.extend(d.feed(b));
        evs.extend(d.finish());
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].data, "héllo");
    }

    #[tokio::test]
    async fn streams_from_byte_stream() {
        let raw = futures::stream::iter(vec![
            Ok(Bytes::from_static(b"data: one\n\n")),
            Ok(Bytes::from_static(b"data: two\n\n")),
        ]);
        let mut s = sse_from_bytes(raw);
        let mut got = Vec::new();
        while let Some(ev) = s.next().await {
            got.push(ev.unwrap().data);
        }
        assert_eq!(got, vec!["one", "two"]);
    }
}
