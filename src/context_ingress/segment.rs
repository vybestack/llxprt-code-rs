//! Deterministic segmentation and structural classification.
//!
//! Segmentation is a pure function of the sanitized bytes: it splits on newlines, keeps
//! every byte (including the newline) in exactly one segment, and assigns each segment a
//! structural class. Total disjoint coverage is checked at ingestion. Structural lanes
//! are the documented fallback when no semantic classification is available.

use std::ops::Range;

/// Structural class of one segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructuralClass {
    ExactSpan,
    Identifier,
    Code,
    TestLog,
    Noise,
    Unknown,
}

impl StructuralClass {
    /// Stable name for reports.
    pub fn name(self) -> &'static str {
        match self {
            StructuralClass::ExactSpan => "exact-span",
            StructuralClass::Identifier => "identifier",
            StructuralClass::Code => "code",
            StructuralClass::TestLog => "test-log",
            StructuralClass::Noise => "noise",
            StructuralClass::Unknown => "unknown",
        }
    }

    /// Documented structural lane fallback for this class.
    pub fn lane(self) -> &'static str {
        match self {
            StructuralClass::ExactSpan => "constraint",
            StructuralClass::Identifier => "constraint",
            StructuralClass::Code => "source",
            StructuralClass::TestLog => "test-log",
            StructuralClass::Noise => "noise",
            StructuralClass::Unknown => "body",
        }
    }

    /// Deterministic class of one line of sanitized bytes.
    pub fn of_line(line: &[u8]) -> Self {
        if contains(line, b"exact error span") {
            return StructuralClass::ExactSpan;
        }
        if contains(line, b"unknown-shaped identifier") {
            return StructuralClass::Identifier;
        }
        if starts(line, b"noise:") || starts(line, b"fill line") {
            return StructuralClass::Noise;
        }
        if contains(line, b"test ") || starts(line, b"running ") {
            return StructuralClass::TestLog;
        }
        if starts(line, b"    ") || contains(line, b"fn ") || contains(line, b"-> {") {
            return StructuralClass::Code;
        }
        StructuralClass::Unknown
    }
}

/// One byte range of the sanitized payload plus its structural class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub span: Range<usize>,
    pub class: StructuralClass,
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.len() > haystack.len() {
        return false;
    }
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn starts(haystack: &[u8], prefix: &[u8]) -> bool {
    haystack.len() >= prefix.len() && haystack[..prefix.len()] == *prefix
}

/// Splits sanitized bytes into newline-delimited segments with total coverage.
pub fn segment(bytes: &[u8]) -> Vec<Segment> {
    let mut out = Vec::new();
    let mut start = 0usize;
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'\n' {
            let span = start..index + 1;
            out.push(Segment {
                class: StructuralClass::of_line(&bytes[span.clone()]),
                span,
            });
            start = index + 1;
        }
    }
    if start < bytes.len() {
        out.push(Segment {
            class: StructuralClass::of_line(&bytes[start..]),
            span: start..bytes.len(),
        });
    }
    out
}

/// True when the segments are ordered, disjoint, and cover `len` bytes exactly.
pub fn coverage_is_total(segments: &[Segment], len: usize) -> bool {
    let mut cursor = 0usize;
    for segment in segments {
        if segment.span.start != cursor {
            return false;
        }
        if segment.span.end < segment.span.start {
            return false;
        }
        cursor = segment.span.end;
    }
    cursor == len
}

/// Byte ranges of the segments that must be preserved verbatim.
pub fn exact_spans(segments: &[Segment]) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    for segment in segments {
        let keep = matches!(
            segment.class,
            StructuralClass::ExactSpan | StructuralClass::Identifier
        );
        if keep {
            out.push(segment.span.clone());
        }
    }
    out
}

/// Whether a byte terminates a secret token run (shared by redactor and launder).
pub fn is_separator(byte: u8) -> bool {
    matches!(
        byte,
        b' ' | b'\n' | b'\r' | b'\t' | b'"' | b'\'' | b')' | b',' | b';'
    )
}
