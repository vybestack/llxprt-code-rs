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

/// The byte length a value must reach before a bare `name:` span can be treated as a
/// credential. Ordinary typed identifiers are short (`&str`, `String`, `Option<u8>`), so
/// a floor keeps every one of them out of the rewrite while a real credential — whose
/// whole point is unguessable length — still clears it.
const MIN_CREDENTIAL_VALUE_BYTES: usize = 12;

/// The credential shapes a bare `name:` value can take and still be redacted.
///
/// This is the syntax gate that keeps ordinary Rust (and ordinary prose) intact when it
/// merely *names* a credential field: `api_key: &str`, `pub api_key: Option<String>,`,
/// and `the api_key: field is required` are all declarations of a **type**, a **field**, or
/// a **placeholder**, never a value, so nothing in them is a secret and the bytes must
/// round-trip. A value is only treated as a credential when its own bytes are
/// credential-shaped: a quoted string, a classic token prefix, a `Bearer`/`Basic` scheme,
/// an assignment, a shell reference, or a `name: token` pair inside a URL. URL query and
/// userinfo secrets never depend on this gate — they are rewritten by [`scrub_url_parts`]
/// from the `://` of the URL alone, so `?api_key=<anything>` stays fully redacted whether
/// or not this predicate would call the value credential-shaped.
fn is_credential_value(bytes: &[u8], start: usize, end: usize) -> bool {
    let mut i = start;
    // A value region begins at the first non-space byte; the needle's trailing byte was
    // the `:` (or the space of `authorization `), so any run of spaces is a separator.
    while i < end && bytes[i] == b' ' {
        i += 1;
    }
    if i >= end {
        // `Authorization:` with nothing behind it is a bare word, not a value.
        return false;
    }
    let first = bytes[i];
    // A quoted scalar is a value: `"sk-live-…"`, `"********"`. A quote also closes a JSON
    // key (`"api_key": "…"`), where the value begins at the *next* quote, so the scan
    // looks past a `":"`/`": ` boundary instead of stopping at the key's own closing
    // quote and treating `"api_key"` as an empty value.
    if first == b'"' {
        let mut j = i + 1;
        while j < end && bytes[j] != b'"' {
            j += 1;
        }
        // The value is the inner span; empty quotes are a placeholder, not a secret.
        return j > i + 1;
    }
    // The token prefixes the ecosystem actually uses, either bare or quoted.
    if bytes[i..end].starts_with(b"sk-")
        || bytes[i..end].starts_with(b"sk_")
        || bytes[i..end].starts_with(b"ghp_")
        || bytes[i..end].starts_with(b"gho_")
        || bytes[i..end].starts_with(b"github_pat_")
        || bytes[i..end].starts_with(b"glpat-")
        || bytes[i..end].starts_with(b"xox")
    {
        return true;
    }
    // An HTTP auth scheme at the head of the value is a credential by construction.
    if bytes[i..end].starts_with(b"bearer ") || bytes[i..end].starts_with(b"basic ") {
        return true;
    }
    // An assignment (`api_key=sk-…`, `?api_key=…`, `token=`) is a value, never a type
    // annotation; `=` is not part of Rust's type syntax at all.
    if bytes[i..end].contains(&b'=') {
        return true;
    }
    // A shell reference (`$LLXPRT_KEY`, `${KEY}`) is a value.
    if first == b'$' {
        return true;
    }
    // A credential inside URL credentials (`https://name:token@host`) is a value. The
    // colon there separates a pair, so the run after it is the secret itself.
    if bytes[i..end].contains(&b'@') {
        return true;
    }
    // Otherwise, a bare unquoted span is only treated as a credential when it looks like
    // one *and* is long enough to be one. Ordinary typed identifiers and placeholders are
    // short (`&str`, `String`, `Option<u8>`, `field`, `value`), so they stay readable. A
    // longer span is only a credential when it carries no ordinary-word separator: a
    // comma/space-separated list is English prose or a field list (`the api_key: field is
    // required`, `Option<String>,`), while a real token pasted behind a bare `api_key:` is
    // one unbroken run (`sk-live-abcdef123456`, `ghp_…`) and clears both bars.
    let span = &bytes[i..end];
    let bare = end - i;
    bare >= MIN_CREDENTIAL_VALUE_BYTES
        && !span
            .iter()
            .any(|b| matches!(b, b' ' | b',' | b';' | b'(' | b')' | b'<' | b'>'))
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
///
/// A needle only triggers a rewrite when the span behind it is itself
/// [`is_credential_value`] **credential-shaped** (issue 129). Matching the *name* alone
/// rewrote ordinary Rust: `api_key: &str` in a function signature, `pub api_key:
/// Option<String>,` in a struct field, and `the api_key: field is required` in prose each
/// lost the rest of their line to a `[redacted]` marker, so a `read_file` of an ordinary
/// `.rs` file reached the model with its source corrupted. Those spans name a credential
/// without carrying one; a quoted token, a `sk-`-style prefix, a `Bearer`/`Basic` scheme,
/// an assignment, a shell reference, URL credentials, or a long-enough bare value does
/// carry one and is still replaced whole.
fn scrub_auth_like(text: &str) -> String {
    // ASCII byte case-folding never changes the byte count, so a needle's matched byte
    // offset is a valid, UTF-8-aligned offset into the original `text`; the value
    // region we replace also ends at an ASCII delimiter (newline/whitespace/end), so a
    // multi-byte codepoint inside the value is removed whole, never split.
    let low = text.to_ascii_lowercase();
    let bytes = low.as_bytes();
    let n = bytes.len();
    // The credential **names**, matched case-insensitively and without a separator: the
    // separator itself is found after the name, so a folded `api_key =`, `api_key:`,
    // `api-key=`, or `authorization :` all reach the same one syntax gate. Matching a
    // bare name is safe because a separator is *required* — `the api_key field is
    // required` never has one behind the name, so it is not a candidate at all.
    const NAMES: [&[u8]; 4] = [b"authorization", b"x-api-key", b"api-key", b"api_key"];
    // Find the separator (`:` for a header/name, `=` for an assignment) that begins the
    // value, allowing the spaces a folded config line or an aligned header block puts
    // around it. `None` means the name occurrence carries no value at all.
    let value_start = |idx: usize, name_len: usize| -> Option<usize> {
        let mut j = idx + name_len;
        while j < n && bytes[j] == b' ' {
            j += 1;
        }
        match bytes.get(j) {
            Some(b':') | Some(b'=') => Some(j + 1),
            _ => None,
        }
    };
    let mut out = String::with_capacity(text.len());
    let mut i = 0usize;
    while i < n {
        let Some((idx, name_len)) =
            next_credential_name(&bytes[i..], NAMES).map(|(p, l)| (i + p, l))
        else {
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
        // A name with no separator behind it (`the api_key field is required`) is not a
        // value-bearing occurrence at all; the bytes round-trip and the scan moves past
        // the name.
        let Some(start) = value_start(idx, name_len) else {
            out.push_str(&text[idx..idx + name_len]);
            i = idx + name_len;
            continue;
        };
        // Syntax gate (issue 129): a name with an ordinary typed identifier, a field
        // type, or a placeholder behind it names a credential without carrying one, so
        // the bytes round-trip untouched instead of losing their line to the marker.
        // `i` still advances past the name so the scan never reconsiders it.
        if !is_credential_value(bytes, start, end) {
            out.push_str(&text[idx..idx + name_len]);
            i = idx + name_len;
            continue;
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

/// The earliest credential **name** in `bytes`, with its length, or `None` when there is
/// none. A word-boundary guard on both edges keeps `x-api-keyboard`, `my_api_key`, and
/// `authorizationheader` from matching, so only a whole name is ever a candidate.
fn next_credential_name(bytes: &[u8], names: [&[u8]; 4]) -> Option<(usize, usize)> {
    let n = bytes.len();
    let mut best: Option<(usize, usize)> = None;
    for name in names {
        let Some(p) = find_bytes(bytes, name) else {
            continue;
        };
        let abs = p;
        let after = abs + name.len();
        let boundary_before =
            abs == 0 || !bytes[abs - 1].is_ascii_alphanumeric() || bytes[abs - 1] == b'-';
        if !boundary_before || (after < n && bytes[after].is_ascii_alphanumeric()) {
            continue;
        }
        if best.is_none_or(|(cur, _)| abs < cur) {
            best = Some((abs, name.len()));
        }
    }
    best
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
