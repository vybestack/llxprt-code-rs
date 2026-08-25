//! Effective-LOC counting for Rust source.
//!
//! "Effective" means: a line counts when it carries at least one real token and is not a
//! comment-only line (a blank line, a `//` line, a block-comment line, or a doc
//! comment line). Comments are never tokens in `proc_macro2`, so a line that only
//! contains a comment contributes nothing. Source doc comments materialize as attributes in
//! the parsed token stream, so their spans are replaced with whitespace before counting. An
//! explicit `#[doc]` attribute remains code.
//!
//! Counts are derived from source spans, so multi-line constructs (signatures, string
//! literals, nested closures) count each line they occupy exactly once.

use proc_macro2::{Span, TokenStream, TokenTree};
use quote::ToTokens;
use std::collections::BTreeSet;
use syn::spanned::Spanned;
use syn::visit::{self, Visit};

/// All 1-based line numbers on which any token of `ts` lies.
pub fn token_lines(ts: &TokenStream) -> BTreeSet<usize> {
    let mut out = BTreeSet::new();
    fill(ts, &mut out);
    out
}

fn fill(ts: &TokenStream, out: &mut BTreeSet<usize>) {
    for tt in ts.clone() {
        match tt {
            TokenTree::Group(group) => {
                let span = group.span();
                out.insert(span.start().line);
                out.insert(span.end().line);
                fill(&group.stream(), out);
            }
            TokenTree::Ident(ident) => {
                out.insert(ident.span().start().line);
            }
            TokenTree::Punct(punct) => {
                out.insert(punct.span().start().line);
            }
            TokenTree::Literal(lit) => mark(lit.span(), out),
        }
    }
}

fn mark(span: Span, out: &mut BTreeSet<usize>) {
    let start = span.start().line;
    let mut end = span.end().line;
    if end < start {
        end = start;
    }
    for line in start..=end {
        out.insert(line);
    }
}

/// Collector of attribute spans that may have originated as source doc comments.
struct DocSpans(Vec<Span>);

impl<'ast> Visit<'ast> for DocSpans {
    fn visit_attribute(&mut self, attr: &'ast syn::Attribute) {
        if attr.path().is_ident("doc") {
            self.0.push(attr.span());
        }
        visit::visit_attribute(self, attr);
    }
}

fn line_starts(source: &str) -> Vec<usize> {
    let mut starts = vec![0];
    for (offset, byte) in source.bytes().enumerate() {
        if byte == b'\n' {
            starts.push(offset + 1);
        }
    }
    starts
}

fn offset(source: &str, starts: &[usize], line: usize, column: usize) -> Option<usize> {
    let start = *starts.get(line.checked_sub(1)?)?;
    let tail = source.get(start..)?;
    let line_text = tail.split_once('\n').map_or(tail, |(text, _)| text);
    if column == line_text.chars().count() {
        Some(start + line_text.len())
    } else {
        line_text
            .char_indices()
            .nth(column)
            .map(|(byte, _)| start + byte)
    }
}

fn without_doc_comments(source: &str, file: &syn::File) -> Result<String, String> {
    let starts = line_starts(source);
    let mut bytes = source.as_bytes().to_vec();
    let mut docs = DocSpans(Vec::new());
    visit::visit_file(&mut docs, file);
    for span in docs.0 {
        let start = offset(source, &starts, span.start().line, span.start().column)
            .ok_or_else(|| "doc-comment start span is outside source".to_string())?;
        let end = offset(source, &starts, span.end().line, span.end().column)
            .ok_or_else(|| "doc-comment end span is outside source".to_string())?;
        let text = source
            .get(start..end)
            .ok_or_else(|| "doc-comment span is not on UTF-8 boundaries".to_string())?;
        if !(text.starts_with("///")
            || text.starts_with("//!")
            || text.starts_with("/**")
            || text.starts_with("/*!"))
        {
            continue;
        }
        for byte in &mut bytes[start..end] {
            if *byte != b'\n' && *byte != b'\r' {
                *byte = b' ';
            }
        }
    }
    String::from_utf8(bytes).map_err(|error| format!("clean doc comments: {error}"))
}

/// The set of lines in `file` that count as effective (code) lines.
pub fn file_effective_lines(source: &str, file: &syn::File) -> Result<BTreeSet<usize>, String> {
    let cleaned = without_doc_comments(source, file)?;
    let cleaned_file = syn::parse_file(&cleaned).map_err(|error| {
        let start = error.span().start();
        format!(
            "parse cleaned source at {}:{}: {error}",
            start.line,
            start.column + 1
        )
    })?;
    let mut lines = token_lines(&cleaned_file.to_token_stream());
    if cleaned_file.shebang.is_some() {
        lines.insert(1);
    }
    Ok(lines)
}

/// Number of effective lines of `file`.
pub fn effective_loc(source: &str, file: &syn::File) -> Result<usize, String> {
    Ok(file_effective_lines(source, file)?.len())
}

/// Number of effective lines within the 1-based inclusive line range `[start, end]` using
/// the same line set as [`file_effective_lines`].
pub fn loc_in_range(lines: &BTreeSet<usize>, start: usize, end: usize) -> usize {
    lines.range(start..=end).count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use syn::parse_file;

    fn loc(src: &str) -> usize {
        let file = parse_file(src).expect("fixture must parse");
        effective_loc(src, &file).expect("fixture LOC must succeed")
    }

    #[test]
    fn blank_and_comment_lines_are_excluded() {
        // Leading blank, `//` line, multi-line block comment, trailing `//` line, and a
        // doc comment must not count; only the three code lines may.
        let src = "\n\
                  // leading comment\n\
                  /* a\n\
                   * block\n\
                   */\n\
                  fn a() {}\n\
                  /// documented\n\
                  fn b() {}\n\
                  fn _c() {}\n";
        assert_eq!(loc(src), 3);
    }

    #[test]
    fn every_code_line_counts_once() {
        // The signature, two expression lines, and closing brace count. Comments and blanks do not.
        let src = "fn f() -> i32 {\n    // no\n    1 +\n    2\n}\n\n/* c1\n  c2 */\n";
        assert_eq!(loc(src), 4);
    }

    #[test]
    fn rust_shebang_is_an_effective_line() {
        assert_eq!(loc("#!/usr/bin/env rust-script\nfn main() {}\n"), 2);
    }

    #[test]
    fn nested_closure_and_signature_lines_count() {
        let src = "fn long_sig(\n  a: i32,\n  b: i32,\n) -> i32 {\n  let f = |x: i32| {\n    x\n      + 1\n  };\n  f(a + b)\n}\n";
        let file = parse_file(src).unwrap();
        let lines = file_effective_lines(src, &file).unwrap();
        // Effective lines: 1..=10 all are code (the closure body lines 6-7 count),
        // matching "nested closures count toward the containing function length".
        assert_eq!(lines.len(), 10);
        assert_eq!(lines.len(), loc(src));
    }

    #[test]
    fn doc_comments_do_not_hide_code_or_explicit_attributes() {
        let src = "/** docs */ fn a() {}\n#[doc = \"docs\"]\nfn b() {}\n/// only docs\nfn c() {}\n";
        assert_eq!(loc(src), 4);
        assert_eq!(loc("//! module docs — UTF-8\nfn d() {}\n"), 1);
    }

    #[test]
    fn raw_strings_macros_and_mixed_comments_count_code_lines() {
        let src = r####"fn f() {
  call!(
    "value", // mixed code and comment

    r#"first

third"#,
  );
}
"####;
        // All lines except the blank line outside the raw string count. The blank line inside
        // the raw string is occupied by the multiline literal and therefore counts.
        assert_eq!(loc(src), 8);
    }
}
