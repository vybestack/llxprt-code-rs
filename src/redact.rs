//! Redaction and secret-scrubbing helpers used across the CLI, model config, and process
//! runner.
//!
//! The invariants are documented in one place so every error and `Debug` surface goes
//! through the same policy:
//! - Credentials (`auth-key` values, keyfile file paths) are never shown verbatim.
//! - URLs never carry userinfo, query, or fragment in errors or `Debug`; the scheme,
//!   host, and port may be reported (the dsflash endpoint host is a deployment
//!   address, not a secret).
//! - Anything that is not proven to be a `https://` public URL (including plaintext
//!   `http://`, a missing host, a non-`http(s)` scheme, or a local-path string)
//!   is considered *not trustworthy*: the full string is never shown, only the
//!   [`redact_url`] form.

use url::Url;

/// Render a URL for user-facing errors and `Debug`, dropping the credentials chunk
/// (`scheme://user:pass@host:port`) down to `scheme://host:port` and the path,
/// query, and fragment entirely.
pub fn redact_url(raw: &str) -> String {
    match Url::parse(raw.trim()) {
        Ok(u) => {
            let host = if let Some(h) = u.host_str() {
                if let Some(p) = u.port() {
                    format!("{h}:{p}")
                } else {
                    h.to_string()
                }
            } else {
                "<no-host>".to_string()
            };
            match u.scheme() {
                "https" | "http" => format!("{}://{host}", u.scheme()),
                other => format!("{other}://{host}"),
            }
        }
        Err(_) => "<redacted url>".to_string(),
    }
}

/// Reject a URL that carries userinfo (including a password-only userinfo where the
/// username is empty but a password is present), a query, or a fragment (the caller
/// decides whether to accept or reject; configuration rejects, test fixtures just
/// assert).
pub fn url_has_rejected_parts(raw: &str) -> bool {
    match Url::parse(raw.trim()) {
        Ok(u) => {
            !u.username().is_empty()
                || u.password().is_some()
                || u.query().is_some()
                || u.fragment().is_some()
        }
        Err(_) => false,
    }
}

/// Whether a value can be shown verbatim in an error: only an `https://` absolute URL
/// with a host and no userinfo (including password-only userinfo), query, or fragment is
/// trustworthy. Everything else (plaintext `http://`, bare hosts, local paths,
/// arbitrary strings) is collapsed to [`redact_url`] if it parses as a URL or to a
/// neutral marker otherwise.
pub fn safe_for_display(s: &str) -> String {
    if !s.starts_with("https://") {
        return redact_url(s);
    }
    match Url::parse(s.trim()) {
        Ok(u)
            if u.username().is_empty()
                && u.password().is_none()
                && u.query().is_none()
                && u.fragment().is_none() =>
        {
            s.to_string()
        }
        _ => redact_url(s),
    }
}

/// A secret scrubber for provider-controlled error text before it reaches CLI output or
/// session persistence. Every occurrence of the resolved key or the inline key bytes, the
/// keyfile path (original and expanded), `Authorization: Bearer <...>` headered values,
/// and URL userinfo/query/fragment chunks is replaced with `[redacted]`. Runs on error
/// strings only; transport bytes and the request path keep the real values.
pub fn scrub_secrets(text: &str, secrets: &[String]) -> String {
    let mut out = text.to_string();
    for secret in secrets {
        if !secret.is_empty() && secret.len() <= MAX_SECRET_BYTES {
            out = out.replace(secret, "[redacted]");
        }
    }
    // Collapse any surviving `scheme://user:pass@host` / query / fragment chunks. Only a
    // real URL-shaped chunk is rewritten; a bare `?` or `#` with no `://` scheme
    // behind it in the same run is ordinary punctuation and round-trips untouched.
    if out.contains('@') || out.contains('?') || out.contains('#') {
        out = scrub_url_parts(&out);
    }
    // Collapse any bare `Authorization: Bearer <token>` style chunks.
    out = scrub_auth_like(&out);
    out
}

/// The fixed cap for an error string reaching the CLI/session (bytes). Provider error
/// text is **scrubbed first**, then truncated to this bound **at a UTF-8 boundary**
/// (see [`scrub_and_bound`]), so a huge provider error can never produce an unbounded
/// CLI/session payload and a secret at the truncation edge is redacted first. The
/// bounded diagnostic ([`scrub_and_bound`]) is what the session fail and the CLI
/// JSON receive, so a huge provider body never becomes session-persist.
pub const MAX_ERROR_TEXT_BYTES: usize = 8192;

/// The literal marker appended by [`truncate_utf8`] when anything was cut.
pub const TRUNCATION_MARKER: &str = "[truncated]";

/// The fixed cap for one scrubbed secret value (bytes). A secret over this is not a
/// value the scrubber can redact by exact substitution; keeping the cap guarantees every
/// accepted secret bounds the scrub. See [`MAX_KEY_BYTES`] /
/// [`MAX_KEYFILE_PATH_BYTES`].
pub const MAX_SECRET_BYTES: usize = 4096;

/// The fixed cap (bytes) for the **total** of every user-facing CLI diagnostic field,
/// including the truncation marker. Every surfaced error message passes through
/// [`scrub_and_bound_diagnostic`], so even a message that embeds an oversized
/// persisted scalar (a corrupt branch id, cwd, or parent id) is at most this many
/// bytes on stdout.
pub const MAX_DIAGNOSTIC_BYTES: usize = MAX_ERROR_TEXT_BYTES;

/// The single final bound for every user-facing CLI diagnostic: scrub secret-like
/// chunks first, then truncate at a safe UTF-8 boundary so the total field
/// **including** the marker is at most [`MAX_DIAGNOSTIC_BYTES`]. The marker's
/// own bytes are reserved inside the budget, so the returned field never exceeds it. This
/// is the last function an error message goes through before it is serialized onto
/// stdout.
pub fn scrub_and_bound_diagnostic(text: &str) -> String {
    let scrubbed = scrub_secrets(text, &[]);
    truncate_utf8(scrubbed, MAX_DIAGNOSTIC_BYTES)
}

/// The fixed cap (bytes) for inline credential material (a profile `auth-key` value or
/// the full content of a keyfile). A value of 4096 bytes exactly is accepted and is
/// scrubbed by exact substitution below ([`MAX_SECRET_BYTES`]); anything at 4097
/// bytes is rejected before the adapter is ever built, with the fixed path-free refusal
/// [`KEY_CAP_MESSAGE`] (the over-limit bytes are a secret surface and never travel).
pub const MAX_KEY_BYTES: usize = 4096;

/// The fixed cap (bytes) for a credential path (a keyfile path from `auth-keyfile`,
/// `authKeyfile`, `apiKeyfile`, or `settings.json` `providerKeyfiles`). A path at
/// most 1024 bytes is scrubbed by exact substitution; a longer path is rejected with the
/// fixed path-free refusal [`KEY_PATH_CAP_MESSAGE`]. (`auth-key-name` is a named
/// secure-store reference, never a keyfile path, and is rejected during profile parsing.)
pub const MAX_KEYFILE_PATH_BYTES: usize = 1024;

/// The fixed cap (bytes) for a named provider key (`ephemeralSettings.auth-key-name`,
/// which selects a credential env var and a secure-store account). A name at most 256
/// bytes is accepted; a longer or malformed name is rejected with the fixed value-free
/// refusal [`KEY_NAME_CAP_MESSAGE`].
pub const MAX_KEY_NAME_BYTES: usize = 256;

/// A fixed, path-free, value-free refusal for a named provider key that is empty or over
/// [`MAX_KEY_NAME_BYTES`]. The name is a credential surface and its bytes never travel.
pub const KEY_NAME_CAP_MESSAGE: &str =
    "auth-key-name is an invalid or over-long named key reference";

/// A fixed, path-free refusal for a keyfile path over [`MAX_KEYFILE_PATH_BYTES`]. The
/// over-limit path is a credential surface and its bytes never travel.
pub const KEY_PATH_CAP_MESSAGE: &str = "the auth keyfile path exceeds the documented byte cap";

/// A path-free, cap-free refusal for an over-limit key/path. The over-limit value is
/// a secret surface and its bytes never travel.
pub const KEY_CAP_MESSAGE: &str = "the auth key exceeds the documented byte cap";

/// The fixed cap (bytes) for a profile-generated prompt note/settings string
/// (`reasoning.effort`, `emojifilter`, etc); anything longer is rejected with
/// [`PROMPT_NOTE_CAP_MESSAGE`].
pub const MAX_PROMPT_NOTE_BYTES: usize = 1024;

/// The fixed cap (bytes) for a profile file. [`crate::profile::Profile::load_file`]
/// applies the same cap before parsing (a profile JSON is typically a few hundred bytes).
pub const MAX_PROFILE_FILE_BYTES: usize = 4096;

/// A fixed, path-free refusal for an over-limit profile prompt setting. The
/// over-limit value is a profile-controlled string that would only ever reach the
/// bounded generated system-prompt note/settings lists, so a fixed message travels
/// instead.
pub const PROMPT_NOTE_CAP_MESSAGE: &str =
    "a profile prompt setting exceeds the documented byte cap";

/// The fixed cap (bytes) for the base endpoint URL string. [`crate::model::parse_base_url`]
/// applies it before the adapter is built.
pub const MAX_ENDPOINT_BYTES: usize = 2048;

/// A fixed, path-free refusal for an over-limit endpoint string.
pub const ENDPOINT_CAP_MESSAGE: &str = "the endpoint URL exceeds the documented byte cap";

/// The fixed cap (bytes) for the `settings.json` file used for named-profile
/// credential defaults. The file is read bounded (`cap + 1`) before any UTF-8/JSON
/// parse; a larger file is rejected with [`SETTINGS_FILE_CAP_MESSAGE`] and the
/// over-limit bytes never travel.
pub const MAX_SETTINGS_FILE_BYTES: usize = 8192;

/// A fixed, path-free, value-free refusal for an oversized `settings.json` file. A
/// settings file is read at most [`MAX_SETTINGS_FILE_BYTES`] + 1 bytes, so the
/// over-limit content (which is a credential-default surface) is never parsed and its
/// bytes never travel.
pub const SETTINGS_FILE_CAP_MESSAGE: &str =
    "the settings.json file exceeds the documented byte cap";

/// A fixed, cap-free refusal for an oversized profile file.
pub const PROFILE_FILE_CAP_MESSAGE: &str = "the profile file exceeds the documented byte cap";

/// The fixed cap (bytes) for a profile `model` name. [`crate::profile`] applies it
/// before the request is built.
pub const MODEL_NAME_CAP_MESSAGE: &str = "the model name exceeds the documented byte cap";

/// Scrub every secret from `text` first (so an exact match is redacted **before** any
/// truncation, and no partial secret survives the cut), then truncate the result to
/// [`MAX_ERROR_TEXT_BYTES`] at a **safe UTF-8 boundary**. Truncation never splits a
/// multi-byte codepoint and appends `[truncated]`.
pub fn scrub_and_bound(text: &str, secrets: &[String]) -> String {
    let scrubbed = scrub_secrets(text, secrets);
    truncate_utf8(scrubbed, MAX_ERROR_TEXT_BYTES)
}

/// Truncate `s` to at most `max_bytes` **total including** [`TRUNCATION_MARKER`],
/// ending at a UTF-8 boundary, so no multi-byte codepoint is split and the marker's
/// own bytes never push the result over the cap. When `max_bytes` is smaller than the
/// marker itself only a marker prefix (still a valid prefix) is returned. The caller
/// must have scrubbed secrets first so no partial secret survives a cut.
pub fn truncate_utf8(s: String, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s;
    }
    let marker = TRUNCATION_MARKER;
    if max_bytes >= marker.len() {
        // Reserve the marker inside the budget; the marker is pure ASCII, so any prefix
        // of it is a valid boundary.
        let mut end = max_bytes - marker.len();
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        let mut out: String = s[..end].to_string();
        out.push_str(marker);
        out
    } else {
        // The marker alone cannot fit; keep at most `max_bytes` bytes of it.
        marker[..max_bytes].to_string()
    }
}

/// Replace userinfo, query, and fragment chunks inside a string with `[redacted]`.
fn scrub_url_parts(text: &str) -> String {
    let mut out: Vec<char> = text.chars().collect();
    let mut i = 0usize;
    while i < out.len() {
        if (out[i] == '?' || out[i] == '#') && inside_url_chunk(&out, i) {
            i = scrub_url_suffix(&mut out, i);
            continue;
        }
        if let Some(next) = scrub_url_userinfo(&mut out, i) {
            i = next;
            continue;
        }
        i += 1;
    }
    out.into_iter().collect()
}

/// Whether the `?` or `#` at `at` sits inside a URL chunk: scanning back over
/// the same run (no whitespace or closing delimiter crossed) reaches a `://` scheme
/// separator. Ordinary punctuation with no scheme around it (a Rust attribute line, a
/// shebang, a trailing `?`) is not a URL chunk and is left byte-identical, so the
/// scrubber never does character-level redaction of ordinary punctuation (issue 148).
fn inside_url_chunk(out: &[char], at: usize) -> bool {
    let mut start = at;
    while start > 0 && !matches!(out[start - 1], ' ' | '\t' | '\n' | '\r' | '"' | ')' | '}') {
        start -= 1;
    }
    out[start..at].windows(3).any(|w| w == [':', '/', '/'])
}

fn scrub_url_suffix(out: &mut [char], start: usize) -> usize {
    let mut end = start;
    while end < out.len() && !matches!(out[end], ' ' | '\t' | '\n' | '\r' | '"' | ')' | '}') {
        end += 1;
    }
    out[start..end].fill('-');
    out[start] = '[';
    if end > start + 1 {
        out[start + 1] = 'r';
    }
    end
}

fn scrub_url_userinfo(out: &mut [char], scheme_end: usize) -> Option<usize> {
    if out.get(scheme_end..scheme_end + 3) != Some(&[':', '/', '/']) {
        return None;
    }
    let authority_start = scheme_end + 3;
    let mut end = authority_start;
    let mut separator = None;
    while end < out.len() && !matches!(out[end], '/' | '?' | '#' | ' ' | '\t' | '\n') {
        if out[end] == '@' {
            separator = Some(end);
        }
        end += 1;
    }
    let Some(separator) = separator else {
        return Some(end);
    };
    out[authority_start..separator].fill('-');
    if separator > authority_start {
        out[authority_start] = 'r';
    }
    if separator > authority_start + 1 {
        out[authority_start + 1] = 'e';
    }
    Some(separator)
}

/// Every `Authorization` / `x-api-key` / `api-key` / `api_key` style header value
/// and every standalone `Bearer <token>` chunk is replaced with `[redacted]`. The header
/// names are matched **case-insensitively** over an ASCII case-folded byte copy (which
/// preserves byte positions), while the output is rebuilt from the real text, so an
/// every-size value — including one containing multi-byte codepoints — is replaced whole and
/// never split. The scan continues **after** each replacement, so duplicate, mixed-case,
/// and multiline occurrences each become `[redacted]` and never a second distinct value;
/// the marker contains none of the needles, so an already-redacted value never re-enters
/// the scan (no slow path and no loop).
fn scrub_auth_like(text: &str) -> String {
    // ASCII byte case-folding never changes the byte count, so a needle's matched byte
    // offset is a valid, UTF-8-aligned offset into the original `text`; the value
    // region we replace also ends at an ASCII delimiter (newline/whitespace/end), so a
    // multi-byte codepoint inside the value is removed whole, never split.
    let low = text.to_ascii_lowercase();
    let bytes = low.as_bytes();
    const NEEDLES: [&[u8]; 5] = [
        b"authorization :",
        b"authorization:",
        b"x-api-key:",
        b"api-key:",
        b"api_key:",
    ];
    let n = bytes.len();
    let mut out = String::with_capacity(text.len());
    let mut i = 0usize;
    while i < n {
        let mut next = None;
        for needle in NEEDLES {
            if let Some(p) = find_bytes(&bytes[i..], needle) {
                let abs = i + p;
                if next.is_none_or(|cur| abs < cur) {
                    next = Some(abs);
                }
            }
        }
        let Some(idx) = next else {
            out.push_str(&text[i..]);
            break;
        };
        out.push_str(&text[i..idx]);
        // The headered value runs to the end of its line (or the end of the text);
        // every occurrence is removed, never just the first. `end` stops at a newline
        // or end, both UTF-8 boundaries.
        let mut end = idx;
        while end < n && bytes[end] != b'\n' && bytes[end] != b'\r' {
            end += 1;
        }
        out.push_str("[redacted]");
        i = end;
    }
    // Any remaining standalone `Bearer <token>` chunk (not attached to a header) has its
    // token replaced; the scan resumes past it, so many standalone tokens all redact.
    let text = out;
    let low = text.to_ascii_lowercase();
    let bytes = low.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0usize;
    let n = bytes.len();
    while i < n {
        let Some(rel) = find_bytes(&bytes[i..], b"bearer ") else {
            out.push_str(&text[i..]);
            break;
        };
        let idx = i + rel;
        // A word-boundary guard keeps `forbearer` / `bearer_token` from matching.
        if idx > 0 && bytes[idx - 1].is_ascii_alphanumeric() {
            out.push_str(&text[i..idx + "bearer".len()]);
            i = idx + "bearer".len();
            continue;
        }
        out.push_str(&text[i..idx + "bearer".len()]);
        let mut end = idx + "bearer ".len();
        while end < n && !bytes[end].is_ascii_whitespace() && bytes[end] != b',' {
            end += 1;
        }
        if end > idx + "bearer ".len() {
            out.push_str("[redacted]");
        }
        i = end;
    }
    out
}

/// Byte-window search used by [`scrub_auth_like`]: the offset of `needle` in `hay`,
/// or `None` when absent.
fn find_bytes(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.len() > hay.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

/// A tiny, targeted `[dependencies]`-table parser for produced `Cargo.toml` files.
/// It strips comments and block tables, and collects bare `name = "…"` and inline
/// `name = { … }` entries of any `dependencies` table. Sufficient to prove a crate
/// depends on an established crypto crate without trusting prose or comments.
pub fn parse_cargo_dep_names(manifest: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut table = String::new();
    for raw in manifest.lines() {
        let line = strip_toml_comment(raw);
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            table = line[1..line.len() - 1].trim().to_string();
            continue;
        }
        if !table.contains("dependencies") {
            continue;
        }
        if let Some(eq) = line.find('=') {
            let key = line[..eq].trim().trim_matches('"').trim_matches('\'');
            if key.is_empty() || key.starts_with('[') {
                continue;
            }
            names.push(key.to_string());
        }
    }
    names
}

/// Strip a `#` comment outside a quoted string (byte-simple; adequate for Cargo.toml).
fn strip_toml_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut in_str = false;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => in_str = !in_str,
            b'#' if !in_str => return &line[..i],
            _ => {}
        }
        i += 1;
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// Issue 148 reproduction: the tool-result scrubber rewrote every bare `?` and
    /// `#` into a `[r-----` style placeholder, so shell output lost shebangs, Rust
    /// attribute lines, backslash escapes, and `?`, and agents copied that mangled
    /// form back into source files. With no secret shape present the corpus must
    /// round-trip byte-identically.
    #[test]
    fn ordinary_punctuation_round_trips_byte_identically() {
        let corpus = concat!(
            "#!/bin/sh\n",
            "#[test]\n",
            "#[cfg_attr(miri, ignore)]\n",
            "path = C:\\Users\\me\\src\\lib.rs\n",
            "let v = s.split('\\n').find(|l| l.starts_with(\"# \"))?;\n",
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

    /// A genuine secret-shaped span is still replaced, and the replacement is a stable
    /// marker that ordinary code text can never produce.
    #[test]
    fn secret_shaped_url_chunks_are_still_redacted() {
        let src = concat!(
            "prefix stays\n",
            "leak https://h.example/v1?token=sk-fake-query-999&x=1#frag\n",
            "leak https://u:pw@h.example/v1?a=b and c",
        );
        let out = scrub_secrets(src, &[]);
        assert!(!out.contains("token=sk-fake-query-999"), "{out}");
        assert!(!out.contains("frag"), "{out}");
        assert!(!out.contains("u:pw"), "{out}");
        assert!(out.starts_with("prefix stays\n"), "{out}");
        assert!(!out.contains("a=b"), "the query value survived: {out}");
        assert!(!out.contains("sk-fake"), "{out}");
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
}
