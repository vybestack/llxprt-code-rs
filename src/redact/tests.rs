use super::*;
use std::time::{Duration, Instant};

#[test]
fn redacts_credentials_full_url_no_path() {
    let s = redact_url("https://alice:secret@api.example.com:8443/v1/chat");
    assert!(!s.contains("alice"), "{s}");
    assert!(!s.contains("secret"), "{s}");
    assert!(!s.contains("/v1/chat"), "path must be dropped: {s}");
    assert!(!s.contains("token=abc"), "query dropped: {s}");
    assert_eq!(s, "https://api.example.com:8443");
}

#[test]
fn plaintext_endpoint_is_collapsed() {
    // The dsflash base URL is a remote plaintext HTTP address; only the
    // scheme/host/port survive.
    let u = crate::profile::RedactedUrl::from_unvalidated("http://23.183.40.76:8080/v1");
    assert_eq!(u.as_display(), "http://23.183.40.76:8080");
}

#[test]
fn query_and_fragment_never_survive() {
    let s = crate::profile::RedactedUrl::from_unvalidated(
        "https://api.example.com/v1?api-key=ghp_secret#frag",
    );
    assert_eq!(s.as_display(), "https://api.example.com");
    assert!(
        url_has_rejected_parts("https://api.example.com/v1?k=v"),
        "queries are flagged"
    );
}

/// Key material in a URL query never appears in any rendering.
#[test]
fn api_key_query_value_never_survives_any_rendering() {
    for raw in [
        "https://api.example.com/v1?api-key=ghp_secret",
        "https://api.example.com?key=super-secret",
        "https://api.example.com/v1#token=super-secret",
    ] {
        let rendered = redact_url(raw);
        assert!(!rendered.contains("secret"), "{rendered}");
        let disp = crate::profile::RedactedUrl::from_unvalidated(raw)
            .as_display()
            .to_string();
        assert!(!disp.contains("secret"), "{disp}");
        let safe = safe_for_display(raw);
        let secret_and_host_are_not_both_visible =
            !safe.contains("secret") || !safe.contains("api.example.com");
        assert!(secret_and_host_are_not_both_visible, "{safe}");
    }
    assert!(url_has_rejected_parts(
        "https://api.example.com/v1?state=abc"
    ));
}

/// Full URL with userinfo, path, query, and fragment collapses to scheme://host:port.
#[test]
fn endpoint_userinfo_path_query_fragment_all_redacted() {
    let raw = "https://bob:hunter2@api.example.com:8443/v1/chat?hint=1&token=abc#tail";
    let r = crate::profile::RedactedUrl::from_unvalidated(raw);
    assert_eq!(r.as_display(), "https://api.example.com:8443");
    let s = redact_url(raw);
    assert_eq!(s, "https://api.example.com:8443");
    assert!(!s.contains("bob"));
    assert!(!s.contains("hunter2"));
    assert!(!s.contains("/v1/chat"));
    assert!(!s.contains("token"));
    assert!(!s.contains("#tail"));
}

/// A non-https / non-url string collapses to a bounded rendering, never the raw value.
#[test]
fn non_https_or_unparseable_collapses() {
    assert_eq!(
        redact_url("http://23.183.40.76:8080/v1"),
        "http://23.183.40.76:8080"
    );
    assert_eq!(redact_url("/etc/passwd"), "<redacted url>");
    assert_eq!(redact_url("merely a string"), "<redacted url>");
    assert_eq!(safe_for_display("/etc/passwd"), "<redacted url>");
    assert_eq!(safe_for_display("http://h/a"), "http://h");
}

/// Issue 148 reproduction: the tool-result scrubber rewrote every bare `<Q>` or
/// `#` into a `[r-----` style placeholder, so shell output lost shebangs, Rust
/// attribute lines, backslash escapes, and `<Q>`/`#`, and agents copied that
/// mangled form back into source files. A run with **no scheme** behind the
/// punctuation is ordinary punctuation and round-trips byte-identically. This is the
/// same trade-off as origin/main for scheme-free runs only: a run that *does* carry a
/// scheme (for example `see(https://h.example/a)?note`) still mangles the trailing
/// text, because the rewrite runs to the end of the chunk; that is unchanged from
/// main, not a regression.
#[test]
fn ordinary_punctuation_round_trips_byte_identically() {
    let corpus = concat!(
        "#!/bin/sh\n",
        "#[test]\n",
        "#[cfg_attr(miri, ignore)]\n",
        "path = C:\\Users\\me\\src\\lib.rs\n",
        "let v = s.split('\\n').find(|l| l.starts_with(\"# \"))<Q>;\n",
        "grep -n #TODO\" src\n",
        "whoami? root\n",
    );
    assert_eq!(scrub_secrets(corpus, &[]), corpus);
    let secret = "sk-super-fake-marker-777".to_string();
    assert_eq!(
        scrub_secrets(corpus, &[secret]),
        corpus,
        "an unrelated secret must not drag ordinary punctuation into a rewrite"
    );
}

/// Issue 129: a whole file that mixes ordinary Rust attribute lines with real code
/// must round-trip byte-identically. Every URL in the corpus is an ordinary
/// credential-free URL, so nothing in it is secret-shaped; this is the shape the
/// model actually sees from `read_file` on a `.rs` file. A URL that *does* carry a
/// query or userinfo is still rewritten — see
/// [`rust_attribute_lines_survive_alongside_a_redacted_url`] and
/// [`secret_shaped_url_chunks_are_still_redacted`].
#[test]
fn rust_attribute_lines_round_trip_byte_identically() {
    let corpus = concat!(
        "#[derive(Debug, Clone)]\n",
        "pub struct T {\n",
        "    pub a: u8,\n",
        "}\n",
        "\n",
        "#[test]\n",
        "fn probe() {\n    assert!(1 + 1 == 2);\n}\n",
        "\n",
        "#[cfg(feature = \"zx\")]\n",
        "mod gated {}\n",
        "#[cfg_attr(unix, cfg(feature = \"unix-only\"))]\n",
        "mod attrs {}\n",
        "#![allow(dead_code)]\n",
        "fn main() {}\n",
        "// see the docs at https://h.example/rust/stable/ for details\n",
        "#[derive(PartialEq, Eq)]\n",
        "struct Tail;\n",
    );
    let out = scrub_secrets(corpus, &[]);
    assert_eq!(out, corpus, "attribute lines were rewritten");
}

/// Ordinary attribute lines stay whole even when the same document also contains a
/// URL whose query really is secret-shaped: the rewrite is confined to the URL's own
/// query/fragment suffix and never bleeds backward into an attribute line.
#[test]
fn rust_attribute_lines_survive_alongside_a_redacted_url() {
    let src = concat!(
        "#[derive(Debug)]\n",
        "struct A;\n",
        "// docs: https://h.example/v1?token=sk-fake-query-999\n",
        "#[cfg(test)]\n",
        "mod tests {}\n",
    );
    let out = scrub_secrets(src, &[]);
    assert!(
        out.contains("#[derive(Debug)]"),
        "the attribute line was rewritten: {out}"
    );
    assert!(
        out.contains("#[cfg(test)]"),
        "the cfg line was rewritten: {out}"
    );
    assert!(
        !out.contains("token=sk-fake-query-999"),
        "secret leaked: {out}"
    );
    assert!(!out.contains("token="), "secret leaked: {out}");
}

/// Issue 129: the credential *names* are ordinary Rust when they name a type, a
/// field, or a placeholder, and only become a secret when a credential-shaped value
/// sits behind the separator. Each of these lines is real code a `read_file` returns,
/// and each must round-trip byte-identically.
#[test]
fn ordinary_typed_credential_names_round_trip_byte_identically() {
    let corpus = concat!(
        "fn build(api_key: &str, other: u8) {}\n",
        "pub struct Profile {\n",
        "    pub api_key: Option<String>,\n",
        "    pub api_key: String,\n",
        "}\n",
        "impl Profile {\n",
        "    fn key(&self, api_key: &str) -> &str {\n",
        "        api_key\n",
        "    }\n",
        "}\n",
        "/// api_key: &str is required for the call\n",
        "let x = api_key;\n",
        "the api_key field is required\n",
    );
    let out = scrub_secrets(corpus, &[]);
    assert_eq!(
        out, corpus,
        "an ordinary typed identifier was rewritten: {out}"
    );
}

/// Issue 129 true positives: the same names, in the same document, with a genuine
/// credential behind each separator, are all still replaced whole. URL query and
/// userinfo secrets never depend on the value-shape gate, so they stay redacted for
/// any value, and the neighbouring ordinary Rust is left alone.
#[test]
fn credential_shaped_neighbours_are_still_scrubbed() {
    let corpus = concat!(
        "#[derive(Debug)]\n",
        "struct Client;\n",
        "Authorization: Bearer sk-live-abcdef123456\n",
        "x-api-key: sk-live-abcdef123456\n",
        "api_key: sk-live-abcdef123456\n",
        "api_key = \"sk-live-abcdef123456\"\n",
        "let api_key = \"sk-live-abcdef123456\";\n",
        "https://user:sk-live-abcdef123456@h.example/v1\n",
        "https://h.example/v1?api_key=sk-live-abcdef123456\n",
        "#[test]\n",
        "fn t() {}\n",
    );
    let out = scrub_secrets(corpus, &[]);
    for secret in [
        "sk-live-abcdef123456",
        "Bearer sk-live",
        "user:sk-live",
        "api_key=sk-live",
    ] {
        assert!(!out.contains(secret), "a credential survived: {out}");
    }
    // The surrounding ordinary source is untouched: no bleed into the neighbours.
    assert!(
        out.contains("#[derive(Debug)]\nstruct Client;\n"),
        "attribute bleed: {out}"
    );
    assert!(
        out.contains("#[test]\nfn t() {}\n"),
        "attribute bleed: {out}"
    );
    // The header/assignment rewrite covers the whole span from the name, so the value
    // never survives even in part; only the surrounding ordinary source is preserved.
    assert!(
        out.contains("let [redacted]"),
        "the assignment value survived: {out}"
    );
    assert!(
        out.contains("https://re-----------------------@h.example/v1"),
        "userinfo: {out}"
    );
    assert!(
        !out.contains("?api_key="),
        "url query secret survived: {out}"
    );
}

/// A genuine secret-shaped span is still replaced, and the replacement is a stable
/// marker that ordinary code text can never produce.
#[test]
fn secret_shaped_url_chunks_are_still_redacted() {
    let src = concat!(
        "prefix stays\n",
        "leak https://h.example/v1?token=sk-fake-query-999&x=1#frag\n",
        "leak https://re--@h.example/v1?a=b and c",
    );
    let out = scrub_secrets(src, &[]);
    assert!(!out.contains("token=sk-fake-query-999"), "{out}");
    assert!(!out.contains("frag"), "{out}");
    assert!(!out.contains("u:pw"), "{out}");
    assert!(out.starts_with("prefix stays\n"), "{out}");
    assert!(!out.contains("a=b"), "the query value survived: {out}");
    assert!(!out.contains("sk-fake"), "{out}");

    // Issue 148 review F1: the gate used to walk back over `)`/`}`/`"` demanding a
    // contiguous `://`, but those bytes are legal unencoded URL path bytes and JSON
    // escapes the separator, so each of these genuine URLs was left in the clear.
    // The verdict now comes from the run's content (any `://`, or its JSON-escaped
    // `:\\/\\/` spelling, earlier in the same run), so all three are redacted
    // exactly as origin/main redacted them.
    for src in [
        "https://en.wikipedia.org/wiki/Foo_(bar)?token=SECRET",
        "https://api.example.com/v1/{id}?api_key=SECRET",
        "{\"url\":\"https:\\/\\/h.example\\/v1?token=SECRET\"}",
    ] {
        let out = scrub_secrets(src, &[]);
        assert!(!out.contains("SECRET"), "leaked from {src}: {out}");
        assert!(!out.contains("token="), "leaked from {src}: {out}");
        assert!(!out.contains("api_key="), "leaked from {src}: {out}");
        // The rewrite lands as the `[r-----` placeholder, the exact shape origin/main
        // produced for these URLs, so the whole query/fragment is gone.
        assert!(out.contains("[r"), "no rewrite for {src}: {out}");
    }
}

/// Issue 148 follow-up: a suffix rewrite stops at `)`, `}`, and `"` even though those
/// bytes are deliberately **not** scheme-gate run boundaries. Resetting the run
/// context after every rewrite therefore wiped a proven scheme at exactly the wrong
/// moment, so the `#`/`?` sitting behind the delimiter was gated by a scheme-free run
/// and its secret survived in the clear. The context (run start, memoized scheme
/// verdict, and the monotone scheme-scan cursor) now survives those three delimiters
/// and is dropped only at a real run stop (whitespace, or a quote in a run that has
/// not proven a scheme) or at end of input.
#[test]
fn url_run_context_survives_suffix_delimiters() {
    // The pinned reproductions: the parenthesized, braced, and quoted variants of the
    // same URL, each with a second trigger behind the delimiter.
    for (src, expected) in [
        (
            "https://h.example/v1?next=(b)#token=SECRET",
            "https://h.example/v1[r------)[r-----------",
        ),
        (
            "https://h.example/v1?next={b}#api_key=SECRET",
            "https://h.example/v1[r------}[r-------------",
        ),
        (
            "https://h.example/v1?next=\"b\"#api_key=SECRET",
            "https://h.example/v1[r----\"b\"[r-------------",
        ),
    ] {
        let out = scrub_secrets(src, &[]);
        assert_eq!(out, expected, "wrong rewrite extent for {src}");
        assert!(!out.contains("SECRET"), "secret leaked from {src}: {out}");
        assert!(!out.contains("token="), "secret leaked from {src}: {out}");
        assert!(!out.contains("api_key="), "secret leaked from {src}: {out}");
    }

    // The wider set from the same review: every one must lose its secret material.
    for src in [
        "https://h.example/v1?next=(b)?api_key=SECRET",
        "https://h.example#sec=(a)?api_key=SECRET",
        "https://search.example/api?q=foo(bar)#api_key=SECRET",
        "https://app.example/cb?state=(x)#access_token=SECRET",
        "https://auth.example/oauth/authorize?redirect_uri=https%3A%2F%2Fapp(x)?token=SECRET",
    ] {
        let out = scrub_secrets(src, &[]);
        assert!(!out.contains("SECRET"), "secret leaked from {src}: {out}");
        assert!(!out.contains("token="), "secret leaked from {src}: {out}");
        assert!(!out.contains("api_key="), "secret leaked from {src}: {out}");
        assert!(
            !out.contains("access_token="),
            "secret leaked from {src}: {out}"
        );
        assert!(out.contains("[r"), "no rewrite for {src}: {out}");
    }
}

/// Issue 148 review F2: a punctuation run of length n must cost O(n), not O(n^2).
/// Tool output reaches `MAX_TOOL_OUTPUT_DEFAULT` (16 MiB) before this scrubber sees
/// it, so a multi-hundred-kilobyte run is scrubbed here under a hard wall-clock
/// budget with the whole output still covered.
#[test]
fn long_punctuation_run_scrubs_in_linear_time() {
    let n = 400_000usize;
    // One long run with no scheme anywhere in it: nothing to rewrite.
    let noise = "?".repeat(n);
    let quiet = format!("quiet {noise} end");
    let budget = Duration::from_millis(2_000);
    let scrubbed = time_bounded(&quiet, &[], budget, "scheme-free run");
    assert_eq!(scrubbed, quiet, "a scheme-free run must round-trip");

    // The same run shape with a scheme earlier in it: one rewrite covers it all.
    let url = format!("https://h.example/v1?token=SECRET&{noise}");
    let out = time_bounded(&url, &[], budget, "post-scheme run");
    assert!(!out.contains("SECRET"), "{out}");
    assert!(!out.contains("token="), "{out}");
    assert!(
        !out.contains("?"),
        "punctuation survived the rewrite: {out}"
    );
    assert!(out.contains("[r"), "no rewrite happened: {out}");

    // Issue 148 follow-up: the run context now survives `)`/`}`/`"`, so this shape —
    // a scheme-proven run holding n delimiter-separated segments, each with its own
    // trigger — must stay one linear sweep. `has_scheme` stays memoized and the
    // scheme-scan cursor stays monotone across each preserved boundary, so no
    // backward walk and no rescanning can creep back in.
    let segments = "?a=1)".repeat(n);
    let chunked = format!("https://h.example/v1?first=SECRET{segments}?end=SECRET");
    let out = time_bounded(&chunked, &[], budget, "delimited post-scheme run");
    assert!(!out.contains("SECRET"), "{out}");
    assert!(!out.contains("first="), "{out}");
    assert!(!out.contains("end="), "{out}");
    assert!(out.contains("[r"), "no rewrite happened: {out}");
}

/// Scrub under a wall-clock budget so a super-linear scrubber fails the test instead
/// of hanging the suite.
fn time_bounded(text: &str, secrets: &[String], budget: Duration, label: &str) -> String {
    let started = Instant::now();
    let out = scrub_secrets(text, secrets);
    let elapsed = started.elapsed();
    assert!(
        elapsed <= budget,
        "{label} took {elapsed:?} (budget {budget:?}) on a {} byte input",
        text.len()
    );
    out
}

#[test]
fn scrub_secrets_removes_exact_markers() {
    let key = "sk-super-fake-marker-777";
    let out = scrub_secrets(
        "boom with sk-super-fake-marker-777 again",
        &[key.to_string()],
    );
    assert!(!out.contains(key), "{out}");
    assert_eq!(out, "boom with [redacted] again");
    assert_eq!(scrub_secrets("safe text", &[key.to_string()]), "safe text");
}

#[test]
fn scrub_secrets_removes_auth_and_url_chunks() {
    let key = "sk-fake-888";
    let out = scrub_secrets(
        "Authorization: Bearer sk-fake-888 and https://u:pw@h/x?tok=sk-fake-888#f",
        &[key.to_string()],
    );
    assert!(!out.contains("sk-fake-888"), "{out}");
    assert!(!out.contains("u:pw"), "{out}");
    assert!(!out.contains("tok="), "{out}");
    // Keyfile path values never survive either.
    let p = "/var/lib/llxprt/private/provider_key".to_string();
    let o2 = scrub_secrets("could not open /var/lib/llxprt/private/provider_key", &[p]);
    assert!(!o2.contains("provider_key"), "{o2}");
}

/// Every authorization-like header occurrence in an evolving string is scrubbed,
/// case-insensitively: duplicate headers, mixed-case headers, `api_key` (underscore),
/// and headers split across lines never leave a second distinct value behind.
#[test]
fn scrub_auth_like_scrubs_every_header_occurrence_case_insensitively() {
    let src = concat!(
        "Authorization: Bearer first-secret\n",
        "X-API-Key: second-secret\r\n",
        "aPi_KeY: third-secret\n",
        "Api-Key: fourth-secret\n",
        "AUTHORIZATION : fifth-secret\n",
        "trailing\n",
    );
    let out = scrub_auth_like(src);
    assert_eq!(out.matches("[redacted]").count(), 5, "{out}");
    for value in [
        "first-secret",
        "second-secret",
        "third-secret",
        "fourth-secret",
        "fifth-secret",
    ] {
        assert!(!out.contains(value), "a header value survived: {out}");
    }
    assert!(!out.contains("Authorization"), "{out}");
    assert!(!out.contains("X-API-Key"), "{out}");
    assert!(out.ends_with("trailing\n"), "{out}");
}

/// A standalone `Bearer <token>` (no header) is redacted too, as are a duplicate
/// naive `[redacted]`-already value and a bearer token in a UTF-8 line: the
/// marker is never rescanned (no loop) and the multi-byte line stays valid UTF-8.
#[test]
fn scrub_auth_like_handles_standalone_bearer_utf8_and_redacted_without_loop() {
    let out = scrub_auth_like("Bearer alone-secret and later Bearer other-secret");
    assert_eq!(out.matches("[redacted]").count(), 2, "{out}");
    assert!(!out.contains("alone-secret"), "{out}");
    assert!(!out.contains("other-secret"), "{out}");

    // An already-redacted value must not re-enter the scan forever, and a value with
    // a multi-byte codepoint inside a header value is removed whole (still valid UTF-8).
    let out = scrub_auth_like("please retry: [redacted] with x-api-key: héllo-üütf8-nope");
    assert!(!out.contains("héllo-üütf8-nope"), "{out}");
    assert!(std::str::from_utf8(out.as_bytes()).is_ok());
    assert_eq!(
        out.matches("[redacted]").count(),
        2,
        "the header value and the already-redacted occurrence are both covered: {out}"
    );
    // A bare "bearer" without any token (end of text) makes no change and never loops.
    let out = scrub_auth_like("no credentials; just the word bearer");
    assert_eq!(out, "no credentials; just the word bearer");
}

/// The largest accepted key — exactly [`MAX_SECRET_BYTES`] bytes — is still scrubbed
/// by exact substitution even when the provider text also carries it behind a header, so
/// a full-size credential never survives.
#[test]
fn exact_4096_byte_secret_is_still_scrubbed() {
    let secret = "k".repeat(MAX_SECRET_BYTES);
    let src = format!("Authorization: Bearer {secret} then x-api-key: {secret}");
    let out = scrub_secrets(&src, &[secret]);
    assert!(!out.contains('k'), "the exact-cap secret survives: {out}");
    assert_eq!(out, "[redacted]");
}

#[test]
fn parse_cargo_dependencies_ignores_comments() {
    let mani = r#"
[package]
name = "crypt"
# aes-gcm in a comment must never count
version = "0.1.0"

[dependencies]
aes-gcm = "0.10"
chacha20poly1305 = { version = "0.10", features = ["std"] }

[dev-dependencies]
tempfile = "3"
"#;
    let names = parse_cargo_dep_names(mani);
    assert!(names.contains(&"aes-gcm".to_string()), "{names:?}");
    assert!(names.contains(&"chacha20poly1305".to_string()));
    assert!(!names.contains(&"comment".to_string()));
    let only = r#"[package]
name = "x"
# aes-gcm = "10"
"#;
    assert!(parse_cargo_dep_names(only).is_empty());
}

#[test]
fn redacted_url_preserves_path_prefix_for_transport_but_not_display() {
    let u = crate::profile::RedactedUrl::from_unvalidated("http://127.0.0.1:8000/inference/v1");
    assert_eq!(u.full(), "http://127.0.0.1:8000/inference/v1");
    assert!(!u.as_display().contains("inference"), "{}", u.as_display());
    assert_eq!(u.as_display(), "http://127.0.0.1:8000");
}

/// `truncate_utf8` totals (ASCII): every truncated result is at most `max_bytes`
/// **including** the marker, for cap-1 / cap / cap+1 around several cap values,
/// and a max smaller than the marker returns only a marker prefix.
#[test]
fn truncate_utf8_total_includes_marker_ascii() {
    let marker = TRUNCATION_MARKER;
    let long = "a".repeat(256);
    for max in [31, 32, 33, marker.len() - 1, marker.len(), marker.len() + 1] {
        for len in [max.saturating_sub(1), max, max + 1] {
            let s = "b".repeat(len);
            let out = truncate_utf8(s, max);
            assert!(out.len() <= max, "len {len} cap {max}: {} bytes", out.len());
            assert!(std::str::from_utf8(out.as_bytes()).is_ok());
            if len > max && max >= marker.len() {
                assert_eq!(
                    out.len(),
                    max,
                    "len {len} cap {max}: truncated ASCII fills the cap"
                );
                assert!(out.ends_with(marker));
            }
        }
    }
    // When truncated and ASCII, the content prefix plus marker exactly fill the cap.
    let out = truncate_utf8(long.clone(), marker.len() + 4);
    assert_eq!(out.len(), marker.len() + 4);
    assert!(out.ends_with(marker));
    assert!(out.starts_with("aaaa"));
    // max smaller than the marker: only a marker prefix, still a valid string.
    for max in [0, 1, marker.len() - 1] {
        let out = truncate_utf8(long.clone(), max);
        assert!(out.len() <= max, "cap {max}: {}", out.len());
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
    }
}

/// `truncate_utf8` never splits a multi-byte codepoint and never exceeds `max_bytes`
/// including the marker, for cap-1 / cap / cap+1 windows on a multi-byte string.
#[test]
fn truncate_utf8_preserves_multibyte_within_cap() {
    let marker = TRUNCATION_MARKER;
    let s = "é".repeat(64); // 2 bytes per codepoint
    for max in [13, 14, 15, marker.len(), marker.len() + 1] {
        let out = truncate_utf8(s.clone(), max);
        assert!(out.len() <= max, "cap {max}: {}", out.len());
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
        // No partial 'é' survives: every 'é' is intact or absent.
        for (i, b) in out.as_bytes().iter().enumerate() {
            if *b != b'\xc3' && *b != b'\xa9' {
                continue;
            }
            if *b == b'\xc3' {
                assert_eq!(
                    out.as_bytes().get(i + 1),
                    Some(&0xa9),
                    "split codepoint at {i}"
                );
            }
        }
        let out = truncate_utf8(s.clone(), max - 1);
        let cap = max - 1;
        assert!(out.len() <= cap);
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
    }
    // A string that already fits is returned verbatim even at a tiny cap.
    let short = "ok".to_string();
    assert_eq!(truncate_utf8(short.clone(), 2), short);
}
