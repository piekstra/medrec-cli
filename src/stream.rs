//! The `records-stream/v1` reader, as agreed with medical-common for
//! `pk-med-records` v0.1.0: same types and checks, kept in this one module
//! so pinning the crate replaces it without touching the importer.
//!
//! ```text
//! line 1   {"schema":"records-stream/v1", "source":{…}, "organization":{…}, "patient":{…}, …}
//! line 2…  one FHIR resource per line, verbatim (top-level resourceType)
//! last     {"schema":"records-stream-end/v1", "complete":true, "counts":{…}, "skipped":[…]}
//! ```

use std::collections::BTreeMap;
use std::io::BufRead;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const STREAM_SCHEMA: &str = "records-stream/v1";
pub const END_SCHEMA: &str = "records-stream-end/v1";
pub const PROFILE: &str = "health-records/v1";
/// Longest line the reader accepts (a Binary carries a document inline).
pub const MAX_LINE_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Header {
    pub schema: String,
    pub source: SourceCli,
    pub organization: StreamOrganization,
    pub patient: StreamPatient,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
    pub pulled_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fhir_version: Option<String>,
    /// `fhir-api` (absent) or `portal`: how the producer got the data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceCli {
    pub cli: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamOrganization {
    pub id: String,
    pub name: String,
    pub fhir_base: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamPatient {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relationship: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trailer {
    pub schema: String,
    pub complete: bool,
    #[serde(default)]
    pub counts: BTreeMap<String, u64>,
    #[serde(default)]
    pub skipped: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<Value>,
}

/// One resource line, kept as the server's text: `serde_json::Value` would
/// rewrite a FHIR decimal such as `1.50`.
#[derive(Debug, Clone)]
pub struct RawResource {
    pub resource_type: String,
    pub id: String,
    pub raw: String,
}

impl RawResource {
    pub fn value(&self) -> Result<Value, serde_json::Error> {
        serde_json::from_str(&self.raw)
    }
}

#[derive(Debug)]
pub enum Item {
    Resource(RawResource),
    End(Trailer),
}

#[derive(Debug)]
pub enum StreamError {
    Io(std::io::Error),
    /// The first line is missing or not a `records-stream/v1` header.
    NoHeader(String),
    BadJson {
        line: u64,
        error: String,
    },
    LineTooLong {
        line: u64,
        max: usize,
    },
    /// A line that is neither a resource nor the trailer.
    Unexpected {
        line: u64,
    },
    AfterTrailer {
        line: u64,
    },
    /// The stream ended without its trailer: it was cut short.
    TrailerMissing {
        lines: u64,
    },
    /// The trailer's counts disagree with the resources read.
    CountMismatch {
        resource_type: String,
        trailer: u64,
        read: u64,
    },
    /// The producer says the stream is not complete.
    Incomplete(Box<Trailer>),
}

impl std::fmt::Display for StreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StreamError::Io(e) => write!(f, "reading the stream: {e}"),
            StreamError::NoHeader(why) => {
                write!(f, "the stream has no {STREAM_SCHEMA} header: {why}")
            }
            StreamError::BadJson { line, error } => write!(f, "line {line} is not JSON: {error}"),
            StreamError::LineTooLong { line, max } => {
                write!(f, "line {line} is longer than {max} bytes")
            }
            StreamError::Unexpected { line } => {
                write!(
                    f,
                    "line {line} is neither a FHIR resource nor the stream's trailer"
                )
            }
            StreamError::AfterTrailer { line } => write!(f, "line {line} follows the trailer"),
            StreamError::TrailerMissing { lines } => write!(
                f,
                "the stream ended after {lines} lines without its {END_SCHEMA} trailer (cut short)"
            ),
            StreamError::CountMismatch {
                resource_type,
                trailer,
                read,
            } => write!(
                f,
                "the trailer counts {trailer} {resource_type} but the stream carried {read}"
            ),
            StreamError::Incomplete(t) => write!(
                f,
                "the provider marked the stream incomplete{}",
                t.error
                    .as_ref()
                    .and_then(|e| e.get("message"))
                    .and_then(Value::as_str)
                    .map(|m| format!(": {m}"))
                    .unwrap_or_default()
            ),
        }
    }
}

pub struct RecordsReader<R: BufRead> {
    inner: R,
    max_line: usize,
    line: u64,
    header: Option<Header>,
    counts: BTreeMap<String, u64>,
    done: bool,
}

impl<R: BufRead> RecordsReader<R> {
    pub fn new(inner: R) -> Self {
        RecordsReader {
            inner,
            max_line: MAX_LINE_BYTES,
            line: 0,
            header: None,
            counts: BTreeMap::new(),
            done: false,
        }
    }

    pub fn into_inner(self) -> R {
        self.inner
    }

    pub fn max_line_bytes(mut self, n: usize) -> Self {
        self.max_line = n;
        self
    }

    /// Read and check the header. Call once, first.
    pub fn header(&mut self) -> Result<Header, StreamError> {
        let line = self
            .read_line()?
            .ok_or_else(|| StreamError::NoHeader("the stream is empty".into()))?;
        let v: Value = serde_json::from_slice(&line)
            .map_err(|e| StreamError::NoHeader(format!("line 1 is not JSON: {e}")))?;
        if v.get("schema").and_then(Value::as_str) != Some(STREAM_SCHEMA) {
            return Err(StreamError::NoHeader(format!(
                "line 1 is not tagged {STREAM_SCHEMA}"
            )));
        }
        let h: Header =
            serde_json::from_value(v).map_err(|e| StreamError::NoHeader(format!("line 1: {e}")))?;
        self.header = Some(h.clone());
        Ok(h)
    }

    /// The next resource, or the trailer once (after checking it). `None`
    /// only after the trailer, when the input has ended.
    pub fn next_item(&mut self) -> Option<Result<Item, StreamError>> {
        loop {
            let line = match self.read_line() {
                Ok(Some(l)) => l,
                Ok(None) if self.done => return None,
                Ok(None) => return Some(Err(StreamError::TrailerMissing { lines: self.line })),
                Err(e) => return Some(Err(e)),
            };
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            if self.done {
                return Some(Err(StreamError::AfterTrailer { line: self.line }));
            }
            let text = match String::from_utf8(line) {
                Ok(t) => t,
                Err(_) => {
                    return Some(Err(StreamError::BadJson {
                        line: self.line,
                        error: "not UTF-8".into(),
                    }))
                }
            };
            let v: Value = match serde_json::from_str(&text) {
                Ok(v) => v,
                Err(e) => {
                    return Some(Err(StreamError::BadJson {
                        line: self.line,
                        error: e.to_string(),
                    }))
                }
            };
            if let Some(rt) = v.get("resourceType").and_then(Value::as_str) {
                let id = v
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                *self.counts.entry(rt.to_string()).or_insert(0) += 1;
                return Some(Ok(Item::Resource(RawResource {
                    resource_type: rt.to_string(),
                    id,
                    raw: text.trim_end().to_string(),
                })));
            }
            if v.get("schema").and_then(Value::as_str) == Some(END_SCHEMA) {
                self.done = true;
                let t: Trailer = match serde_json::from_value(v) {
                    Ok(t) => t,
                    Err(e) => {
                        return Some(Err(StreamError::BadJson {
                            line: self.line,
                            error: e.to_string(),
                        }))
                    }
                };
                if !t.complete {
                    return Some(Err(StreamError::Incomplete(Box::new(t))));
                }
                let types: std::collections::BTreeSet<&String> =
                    t.counts.keys().chain(self.counts.keys()).collect();
                for rt in types {
                    let want = t.counts.get(rt).copied().unwrap_or(0);
                    let got = self.counts.get(rt).copied().unwrap_or(0);
                    if want != got {
                        return Some(Err(StreamError::CountMismatch {
                            resource_type: rt.clone(),
                            trailer: want,
                            read: got,
                        }));
                    }
                }
                return Some(Ok(Item::End(t)));
            }
            return Some(Err(StreamError::Unexpected { line: self.line }));
        }
    }

    fn read_line(&mut self) -> Result<Option<Vec<u8>>, StreamError> {
        let mut out = Vec::new();
        loop {
            let buf = self.inner.fill_buf().map_err(StreamError::Io)?;
            if buf.is_empty() {
                if out.is_empty() {
                    return Ok(None);
                }
                break;
            }
            let (take, end) = match buf.iter().position(|&b| b == b'\n') {
                Some(i) => (i + 1, true),
                None => (buf.len(), false),
            };
            if out.len() + take > self.max_line + 1 {
                return Err(StreamError::LineTooLong {
                    line: self.line + 1,
                    max: self.max_line,
                });
            }
            out.extend_from_slice(&buf[..take]);
            self.inner.consume(take);
            if end {
                break;
            }
        }
        self.line += 1;
        while out.last().is_some_and(|b| *b == b'\n' || *b == b'\r') {
            out.pop();
        }
        Ok(Some(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = r#"{"schema":"records-stream/v1","source":{"cli":"fakemed","version":"0.0.1"},"organization":{"id":"example-org","name":"Example Health","fhir_base":"https://fhir.example.org/r4"},"patient":{"id":"pat-0001","organization":"example-org"},"pulled_at":"2026-11-12T00:00:00Z","fhir_version":"4.0.1"}"#;

    fn read(s: &str) -> Result<Vec<Item>, StreamError> {
        let mut r = RecordsReader::new(s.as_bytes());
        r.header()?;
        let mut v = Vec::new();
        while let Some(i) = r.next_item() {
            v.push(i?);
        }
        Ok(v)
    }

    #[test]
    fn a_complete_stream_reads_and_keeps_raw_text() {
        let s = format!(
            "{HEADER}\n{{\"resourceType\":\"Observation\",\"id\":\"o1\",\"valueQuantity\":{{\"value\":1.50}}}}\n\
             {{\"schema\":\"records-stream-end/v1\",\"complete\":true,\"counts\":{{\"Observation\":1}},\"skipped\":[]}}\n"
        );
        let items = read(&s).unwrap();
        assert_eq!(items.len(), 2);
        match &items[0] {
            Item::Resource(r) => {
                assert_eq!(r.resource_type, "Observation");
                assert!(r.raw.contains("1.50"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_stream_cut_short_is_refused() {
        let s = format!("{HEADER}\n{{\"resourceType\":\"Patient\",\"id\":\"p\"}}\n");
        assert!(matches!(
            read(&s),
            Err(StreamError::TrailerMissing { lines: 2 })
        ));
    }

    #[test]
    fn mismatched_counts_incomplete_streams_and_extra_lines_are_refused() {
        let bad_count = format!(
            "{HEADER}\n{{\"resourceType\":\"Patient\",\"id\":\"p\"}}\n\
             {{\"schema\":\"records-stream-end/v1\",\"complete\":true,\"counts\":{{\"Patient\":2}}}}\n"
        );
        assert!(matches!(
            read(&bad_count),
            Err(StreamError::CountMismatch { .. })
        ));
        let incomplete = format!(
            "{HEADER}\n{{\"schema\":\"records-stream-end/v1\",\"complete\":false,\"counts\":{{}},\
             \"error\":{{\"code\":\"upstream\",\"message\":\"server went away\"}}}}\n"
        );
        let err = read(&incomplete).unwrap_err();
        assert!(err.to_string().contains("server went away"), "{err}");
        let after = format!(
            "{HEADER}\n{{\"schema\":\"records-stream-end/v1\",\"complete\":true,\"counts\":{{}}}}\n{{\"x\":1}}\n"
        );
        assert!(matches!(
            read(&after),
            Err(StreamError::AfterTrailer { line: 3 })
        ));
    }

    #[test]
    fn a_missing_header_and_an_oversized_line_are_refused() {
        assert!(matches!(
            read("{\"resourceType\":\"Patient\"}\n"),
            Err(StreamError::NoHeader(_))
        ));
        assert!(matches!(read(""), Err(StreamError::NoHeader(_))));
        let s = format!("{HEADER}\n{}\n", "x".repeat(HEADER.len() + 10));
        let mut r = RecordsReader::new(s.as_bytes()).max_line_bytes(HEADER.len() + 1);
        r.header().unwrap();
        assert!(matches!(
            r.next_item(),
            Some(Err(StreamError::LineTooLong { line: 2, .. }))
        ));
    }
}
