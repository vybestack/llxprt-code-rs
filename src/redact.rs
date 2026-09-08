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

/// The exact length of a sha256 digest token as the `replace` guard mints it (and as the
/// context records already recognize). This is a property of the digest itself, not a
/// guessed "plausible abbreviation" threshold: a boundary that lands anywhere inside one
/// of these tokens must back off to the token's start so the model is never handed a
/// usable-looking fragment.
pub(crate) const SHA256_HEX_LEN: usize = 64;

/// Move a byte cut back to the start of the digest token it would split, so a truncated
/// string never carries a partial digest. Only a run of exactly [`SHA256_HEX_LEN`]
/// lowercase-hex bytes flanked by non-hex bytes is treated as digest-shaped; that
/// recognition is what keeps ordinary short hex-looking prose (a decimal constant, a hex
/// literal in source, `gate.txt`) un-reshaped, because those runs are shorter than
/// [`SHA256_HEX_LEN`] bytes or not flanked the way a rendered digest is.
pub(crate) fn avoid_partial_hex_run(s: &str, end: usize) -> usize {
    let bytes = s.as_bytes();
    if end == 0 || end >= bytes.len() || !is_lower_hex(bytes[end]) {
        // No cut, or the cut sits between runs: nothing to protect.
        return end;
    }
    let mut start = end;
    while start > 0 && is_lower_hex(bytes[start - 1]) {
        start -= 1;
    }
    let mut stop = end;
    while stop < bytes.len() && is_lower_hex(bytes[stop]) {
        stop += 1;
    }
    // A hex run that is not exactly one whole digest is ordinary prose (a decimal
    // constant, a short hex literal, or repeated filler), so it keeps its exact bytes.
    if stop - start != SHA256_HEX_LEN {
        return end;
    }
    // The run is digest-shaped, and the cut lands inside it: no matter which byte of the
    // token the budget reached, what survives would be a usable-looking abbreviation, so
    // the whole token is omitted instead. Hex bytes are ASCII, so `start` is already a
    // char boundary.
    start
}

/// The character set a rendered sha256 token uses: lowercase hex only.
fn is_lower_hex(byte: u8) -> bool {
    byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
}

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
        // A cut inside a digest token would advertise an abbreviation: back off to the run's
        // start so the whole token is omitted instead (issue 243).
        let end = avoid_partial_hex_run(&s, end);
        let mut out: String = s[..end].to_string();
        out.push_str(marker);
        out
    } else {
        // The marker alone cannot fit; keep at most `max_bytes` bytes of it.
        marker[..max_bytes].to_string()
    }
}

/// Replace userinfo, query, and fragment chunks inside a string with `[redacted]`.
///
/// One forward pass, linear in the input (issue 148 review F2): `i` only ever advances,
/// and everything a `?`/`#` trigger needs — which run it sits in, and whether a
/// scheme separator has appeared in that run so far — lives in `ctx`, advanced once per
/// byte instead of re-derived by a backward walk per trigger character. A punctuation run
/// of length n therefore costs O(n) rather than O(n^2), so a 16 MiB `read_file` of a
/// minified asset stays one pass instead of stalling the turn for hours.
fn scrub_url_parts(text: &str) -> String {
    let mut out: Vec<char> = text.chars().collect();
    let mut i = 0usize;
    let mut ctx = RunContext::default();
    while i < out.len() {
        // Whitespace ends the run unconditionally. A quote ends only a run that has not
        // yet proven a scheme: once the run has proven `://`, a quote inside it is an
        // ordinary URL path byte (`…/v1?next="b"#api_key=…`), not the JSON string boundary
        // a bare quote in ordinary text is. A scheme-free run still cuts at the quote, so
        // a JSON string boundary stays a boundary for ordinary text.
        if is_run_stop(out[i]) && !(out[i] == '"' && ctx.has_scheme) {
            // Whitespace, or a quote in a scheme-free run, ends the run: the run start
            // and the scheme progress never carry across that boundary.
            ctx = RunContext::default();
            i += 1;
            continue;
        }
        if ctx.run_start.is_none() {
            ctx.run_start = Some(i);
            ctx.scanned = i;
        }
        if (out[i] == '?' || out[i] == '#') && i > ctx.run_start.unwrap_or(i) {
            if !ctx.has_scheme {
                let to = i;
                if scan_scheme(&out, &mut ctx.scanned, to) {
                    ctx.has_scheme = true;
                }
            }
            if ctx.has_scheme {
                let next = scrub_url_suffix(&mut out, i);
                // The rewrite stopped at the edge of its chunk. `)`, `}`, and `"` are all
                // legal unencoded URL path bytes and are deliberately *not* scheme-gate
                // run boundaries, so a `?`/`#` further along the same URL can sit behind
                // any of them: the run context — run start, the memoized scheme verdict,
                // and the scheme-scan cursor — must survive that boundary, or the next
                // trigger is gated by a scheme-free run and its secret is left in the
                // clear (issue 148 follow-up).
                match out.get(next) {
                    // A path-byte stop: the run keeps its identity and its verdict, and
                    // the scan resumes on the delimiter itself.
                    Some(')') | Some('}') | Some('"') => i = next,
                    // Whitespace, or the end of the input, really does end the run.
                    _ => {
                        ctx = RunContext::default();
                        i = next;
                    }
                }
                // `ctx.scanned` only ever moves forward, so carrying it across the
                // delimiter rescans nothing and the whole pass stays linear.
                continue;
            }
        }
        if let Some(next) = scrub_url_userinfo(&mut out, i) {
            // The userinfo rewrite resumes inside the *same* run (the `@` belongs to its
            // authority), so `ctx` is kept and a query further along the same URL stays
            // gated by the scheme this run already proved.
            i = next;
            continue;
        }
        i += 1;
    }
    out.into_iter().collect()
}

/// The characters that end a run for the scheme gate: whitespace, or a quote in a run
/// that has not proven a scheme yet. Deliberately **not** `)` or `}` — both are legal
/// unencoded URL path bytes (`…/Foo_(bar)?token=…`, `…/v1/{id}?api_key=…`), so treating them as run
/// boundaries is exactly what hid a genuine query from the gate (issue 148 review F1).
/// A quote ends a scheme-free run because a JSON-escaped URL sits inside one
/// (`"https:\/\/h.example\/v1?token=…"`): the escaped separator
/// must be recognized *within* the quotes, never across them. Inside a run that has
/// already proven its scheme, `)`, `}`, and `"` are all ordinary path bytes and the run
/// (and its memoized verdict) carries across them: see the context preservation after a
/// suffix rewrite in [`scrub_url_parts`].
fn is_run_stop(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r' | '"')
}

/// Forward-scanning run state: where the current run began, how far the scheme scan has
/// advanced inside it, and whether a scheme separator has been seen. `scanned` only ever
/// moves forward within a run, so the whole run costs one linear sweep (issue 148
/// review F2) and the verdict is never a stale `false` for a trigger further along.
#[derive(Default)]
struct RunContext {
    run_start: Option<usize>,
    scanned: usize,
    has_scheme: bool,
}

/// Advance the scheme scan from `*from` up to (not including) `to`, returning whether a
/// separator was seen. The verdict comes from the run's content rather than from the
/// separator being *contiguous* with the trigger: `)`, `}`, and `"` are ordinary URL path
/// bytes, so a real query can sit behind any of them and still belong to a URL whose
/// scheme sits further back in the same run (issue 148 review F1). Both spellings count:
/// the literal `://` and the JSON-escaped `:\/\/`, where each `/` becomes
/// `\/`.
fn scan_scheme(out: &[char], from: &mut usize, to: usize) -> bool {
    let mut i = *from;
    while i + 3 <= to {
        if out[i] == ':' && out[i + 1] == '/' && out[i + 2] == '/' {
            *from = i + 1;
            return true;
        }
        if i + 5 <= to
            && out[i] == ':'
            && out[i + 1] == '\\'
            && out[i + 2] == '/'
            && out[i + 3] == '\\'
            && out[i + 4] == '/'
        {
            *from = i + 1;
            return true;
        }
        i += 1;
    }
    *from = i;
    false
}

/// Replace the query/fragment suffix of a URL chunk with a `[redacted]` placeholder and
/// return the offset just past it (a delimiter, or one past the end). The extent keeps the
/// wider delimiter set (`"`/`)`/`}` plus whitespace) so a rewrite stops at the edge of the
/// chunk it belongs to instead of eating the text that follows it.
fn scrub_url_suffix(out: &mut [char], start: usize) -> usize {
    let mut end = start;
    while end < out.len() && !is_suffix_stop(out[end]) {
        end += 1;
    }
    out[start..end].fill('-');
    out[start] = '[';
    if end > start + 1 {
        out[start + 1] = 'r';
    }
    end
}

/// The delimiters that end a query/fragment rewrite.
fn is_suffix_stop(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r' | '"' | ')' | '}')
}

/// Collapse URL userinfo (`scheme://name@host`) to a fixed placeholder, returning the
/// offset of the `@` separator the scan resumes from (still inside the same run).
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

/// Credential names recognized at a header/config boundary. The name check is exact and
/// case-insensitive (the caller supplies ASCII-folded bytes), including Rust identifier
/// boundaries, so a field such as `my_api_key` is not mistaken for an `api_key` key.
#[derive(Clone, Copy, Eq, PartialEq)]
enum CredentialName {
    Authorization,
    XApiKey,
    ApiKey,
}

/// Every authorization-like header/config value and standalone bearer token is replaced
/// with `[redacted]` before tool output or diagnostics become model-visible.
///
/// This scanner is deliberately one forward pass. In particular, a rejected source-name
/// occurrence advances the cursor rather than searching the remaining suffix again; a
/// tool result containing millions of `api_key` mentions is therefore O(n), not O(n²).
fn scrub_auth_like(text: &str) -> String {
    scrub_auth_like_scanned(text).0
}

/// The implementation also returns the number of input positions examined. Keeping that
/// count local makes the linear scan invariant directly testable without a timing-based
/// performance test.
fn scrub_auth_like_scanned(text: &str) -> (String, usize) {
    let low = text.to_ascii_lowercase();
    let bytes = low.as_bytes();
    let mut state = AuthScanState::default();
    let mut i = 0usize;
    while i < bytes.len() {
        advance_source_state(bytes, i, &mut state);
        if let Some(end) = credential_replacement_end(
            bytes,
            text.as_bytes(),
            i,
            state.rust_source,
            state.doc_comment,
        ) {
            state.replace(text, i, end);
            i = end;
            continue;
        }
        if let Some((start, end)) = bearer_replacement_range(bytes, i) {
            state.out.push_str(&text[state.copied..start]);
            state.out.push_str("[redacted]");
            state.copied = end;
            i = end;
            continue;
        }
        i += 1;
    }
    state.out.push_str(&text[state.copied..]);
    (state.out, bytes.len())
}

#[derive(Default)]
struct AuthScanState {
    out: String,
    copied: usize,
    rust_source: bool,
    doc_comment: bool,
}

impl AuthScanState {
    fn replace(&mut self, text: &str, start: usize, end: usize) {
        self.out.push_str(&text[self.copied..start]);
        self.out.push_str("[redacted]");
        self.copied = end;
    }
}

fn advance_source_state(bytes: &[u8], i: usize, state: &mut AuthScanState) {
    if matches!(bytes[i], b'\n' | b'\r') {
        state.rust_source = false;
        state.doc_comment = false;
    } else if bytes.get(i..i + b"///".len()) == Some(b"///") {
        state.rust_source = true;
        state.doc_comment = true;
    } else if rust_source_marker_at(bytes, i) {
        state.rust_source = true;
    }
}

fn credential_replacement_end(
    bytes: &[u8],
    original: &[u8],
    i: usize,
    rust_source: bool,
    doc_comment: bool,
) -> Option<usize> {
    let (name, name_end) = credential_name_at(bytes, i)?;
    let (separator, value_start) = credential_separator(bytes, name_end)?;
    let rust_type = name == CredentialName::ApiKey
        && separator == b':'
        && rust_source
        && (looks_like_rust_type_annotation(bytes, original, value_start)
            || (doc_comment && looks_like_doc_type_phrase(bytes, value_start)));
    (!rust_type).then(|| line_end(bytes, value_start))
}

fn bearer_replacement_range(bytes: &[u8], i: usize) -> Option<(usize, usize)> {
    if !bearer_at(bytes, i) {
        return None;
    }
    let mut start = i + b"bearer".len();
    while start < bytes.len() && matches!(bytes[start], b' ' | b'\t') {
        start += 1;
    }
    let mut end = start;
    while end < bytes.len() && !bytes[end].is_ascii_whitespace() && bytes[end] != b',' {
        end += 1;
    }
    (start > i + b"bearer".len() && end > start).then_some((start, end))
}

/// Return a credential name starting at `i`, only when both edges are outside a Rust
/// identifier. Treating `_` as an identifier byte is important: `my_api_key` is source,
/// not a config key named `api_key`.
fn credential_name_at(bytes: &[u8], i: usize) -> Option<(CredentialName, usize)> {
    if i > 0 && is_identifier_continue(bytes[i - 1]) {
        return None;
    }
    const NAMES: [(&[u8], CredentialName); 4] = [
        (b"authorization", CredentialName::Authorization),
        (b"x-api-key", CredentialName::XApiKey),
        (b"api-key", CredentialName::XApiKey),
        (b"api_key", CredentialName::ApiKey),
    ];
    for (spelling, name) in NAMES {
        let end = i + spelling.len();
        if bytes.get(i..end) == Some(spelling)
            && bytes
                .get(end)
                .is_none_or(|byte| !is_identifier_continue(*byte))
        {
            return Some((name, end));
        }
    }
    None
}

fn is_identifier_continue(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// Return a real key/value separator after a credential name. Equality and match-arm
/// operators are explicitly not assignments: `api_key == expected` and `api_key =>` must
/// round-trip as Rust source.
fn credential_separator(bytes: &[u8], name_end: usize) -> Option<(u8, usize)> {
    let mut separator = name_end;
    while separator < bytes.len() && matches!(bytes[separator], b' ' | b'\t') {
        separator += 1;
    }
    match bytes.get(separator).copied() {
        Some(b':') => Some((b':', separator + 1)),
        Some(b'=') if !matches!(bytes.get(separator + 1), Some(b'=') | Some(b'>')) => {
            Some((b'=', separator + 1))
        }
        _ => None,
    }
}

fn line_end(bytes: &[u8], start: usize) -> usize {
    let mut end = start;
    while end < bytes.len() && !matches!(bytes[end], b'\n' | b'\r') {
        end += 1;
    }
    end
}

/// Detect source markers as the cursor advances. This state is monotone within a line,
/// so repeated `api_key` fields never re-scan the preceding line prefix.
fn rust_source_marker_at(bytes: &[u8], i: usize) -> bool {
    const MARKERS: [&[u8]; 4] = [b"//", b"pub ", b"fn ", b"impl "];
    MARKERS.into_iter().any(|marker| {
        bytes.get(i..i + marker.len()) == Some(marker)
            && (i == 0 || !is_identifier_continue(bytes[i - 1]))
    })
}

/// A source type has either a standard Rust root or a PascalCase nominal-type root.
/// This is a structural exception, not a credential-value heuristic: type spelling is
/// ordinary source even outside a `pub` or `fn` line, while lowercase unknown values at a
/// credential boundary remain closed. The root scan is bounded to preserve the scanner's
/// monotone linear behavior.
const MAX_RUST_TYPE_ANNOTATION_BYTES: usize = 256;

fn looks_like_rust_type_annotation(bytes: &[u8], original: &[u8], start: usize) -> bool {
    let bounded_end = bytes[start..]
        .iter()
        .take(MAX_RUST_TYPE_ANNOTATION_BYTES)
        .position(|byte| matches!(*byte, b',' | b';' | b')' | b'{' | b'=' | b'\n' | b'\r'))
        .map_or_else(
            || bytes.len().min(start + MAX_RUST_TYPE_ANNOTATION_BYTES),
            |offset| start + offset,
        );
    let mut value = trim_ascii(&bytes[start..bounded_end]);
    let mut original_value = trim_ascii(&original[start..bounded_end]);
    if value.first() == Some(&b'&') {
        original_value = &original_value[1..];
        value = &value[1..];
        if value.first() == Some(&b'\'') {
            while !value.is_empty() && !matches!(value[0], b' ' | b'\t') {
                value = &value[1..];
                original_value = &original_value[1..];
            }
            value = trim_ascii(value);
            original_value = trim_ascii(original_value);
        }
    }
    let root_end = value
        .iter()
        .position(|byte| !is_identifier_continue(*byte))
        .unwrap_or(value.len());
    let root = &value[..root_end];
    let original_root = &original_value[..root_end];
    (is_known_rust_type_root(root) || original_root.first().is_some_and(u8::is_ascii_uppercase))
        && value.get(root_end).is_none_or(|byte| {
            matches!(
                *byte,
                b'<' | b'[' | b':' | b',' | b';' | b')' | b'{' | b'=' | b'\n' | b'\r'
            )
        })
}

/// Doc comments commonly explain a typed field as `api_key: &str is …`. Preserve that
/// source prose, but do not let arbitrary documentation values bypass the closed
/// credential boundary.
fn looks_like_doc_type_phrase(bytes: &[u8], start: usize) -> bool {
    let value = trim_ascii(&bytes[start..]);
    let value = value.strip_prefix(b"&").unwrap_or(value);
    let root_end = value
        .iter()
        .position(|byte| !is_identifier_continue(*byte))
        .unwrap_or(value.len());
    is_known_rust_type_root(&value[..root_end]) && value[root_end..].starts_with(b" is ")
}

fn trim_ascii(mut bytes: &[u8]) -> &[u8] {
    while matches!(bytes.first(), Some(b' ' | b'\t')) {
        bytes = &bytes[1..];
    }
    bytes
}

fn is_known_rust_type_root(root: &[u8]) -> bool {
    matches!(
        root,
        b"str"
            | b"string"
            | b"bool"
            | b"char"
            | b"u8"
            | b"u16"
            | b"u32"
            | b"u64"
            | b"u128"
            | b"usize"
            | b"i8"
            | b"i16"
            | b"i32"
            | b"i64"
            | b"i128"
            | b"isize"
            | b"f32"
            | b"f64"
            | b"option"
            | b"vec"
            | b"result"
            | b"box"
            | b"arc"
            | b"cow"
            | b"hashmap"
            | b"btreemap"
            | b"pathbuf"
            | b"duration"
            | b"self"
    )
}

fn bearer_at(bytes: &[u8], i: usize) -> bool {
    let end = i + b"bearer".len();
    bytes.get(i..end) == Some(b"bearer")
        && (i == 0 || !is_identifier_continue(bytes[i - 1]))
        && matches!(bytes.get(end), Some(b' ' | b'\t'))
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
mod tests;
