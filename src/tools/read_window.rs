//! Framed-read rendering with visible bounds and replayable re-fetch recipes (issue 125).
//!
//! A clamped read used to shrink silently: the agent's remaining turn budget became the
//! per-call cap with no trace in the result, so the model re-read the same file with the
//! same oversized window and received a smaller prefix each time. Every truncated window
//! now states its effective bound and the cause of the clamp, and carries exactly one
//! replayable `re-fetch:` recipe as its last line. The recipe's window stays under the
//! digest admission floor and deliverable inside the per-result cap, so the re-fetch
//! comes back verbatim instead of being digested, and its bytes are reserved inside the
//! per-call bound so the recipe can never be the part that gets cut.

/// Headroom carved off the digest admission floor when sizing a re-fetch window: the
/// framed header rides in the same budget, so the recipe window must stay strictly under
/// the floor. One eighth of the floor, minimum one byte.
const FLOOR_MARGIN_DIVISOR: usize = 8;
use crate::tools::WorkspaceCap;
use serde_json::Value as JsonValue;
use std::collections::BTreeMap;
use std::io::{Seek, SeekFrom};
/// Marker for a body cut by the reserved-recipe budget.
const CUT_MARKER: &str = "...";

/// The effective per-call read bound plus the cause of any clamp (issue 125).
pub(super) struct ReadLimits {
    /// Effective bound on the whole model-visible read result.
    pub(super) max_output: usize,
    /// Active digest admission floor; re-fetch windows stay strictly under it.
    pub(super) digest_size_floor: usize,
    /// Configured per-result output cap (`ToolConfig::max_output_bytes`): the recipe's
    /// window must also be deliverable inside it, not merely under the floor.
    pub(super) config_output: usize,
    /// The caller's explicit `max_output_bytes` as a window bound, when one was passed.
    pub(super) requested_window: Option<usize>,
}

/// A window guaranteed to stay under the digest admission floor.
pub(super) fn safe_window(floor: usize) -> usize {
    floor.saturating_sub(floor / FLOOR_MARGIN_DIVISOR).max(1)
}

/// The re-fetch window the recipe offers: under the digest admission floor (so the
/// re-fetch is never digested on the way back in) and deliverable inside the configured
/// per-result cap, so a later re-fetch with no turn pressure can actually be returned.
pub(super) fn recipe_window(floor: usize, config_output: usize) -> usize {
    safe_window(floor).min(config_output).max(1)
}

/// The generic recipe line a CTXDIGEST record carries (issue 125). The digest path cannot
/// know the original path or the caller's cap, so the instruction names the parameters and
/// the safe window. A pure function of the floor, so checkpoint and finalize
/// re-derivations of the same record stay byte-identical.
pub(crate) fn digest_re_fetch_line(floor: usize) -> String {
    let safe = safe_window(floor);
    format!(
        "re-fetch: read_file windows of at most {safe} bytes via {{\"path\":...,\"offset\":...,\"max_output_bytes\":{safe}}}"
    )
}

/// One replayable recipe line: the exact `read_file` arguments object for the next window,
/// rendered by the JSON encoder so every control character in a path is escaped.
fn re_fetch_recipe(path: &str, offset: usize, limits: &ReadLimits) -> String {
    let safe = recipe_window(limits.digest_size_floor, limits.config_output);
    let path_json = serde_json::to_string(path).unwrap_or_else(|_| format!("\"{path}\""));
    format!("re-fetch: {{\"path\":{path_json},\"offset\":{offset},\"max_output_bytes\":{safe}}}")
}

/// Longest UTF-8-safe prefix of `value` that fits `max_bytes`.
fn utf8_prefix(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

/// Renders one framed read window. `shown` is the byte count of `data` the window exposes,
/// `fd_len` the whole file, and `limit` the caller's explicit `limit` argument if any.
pub(super) fn frame_read(
    path: &str,
    offset: usize,
    shown: usize,
    fd_len: u64,
    limit: Option<u64>,
    limits: &ReadLimits,
    data: &[u8],
) -> String {
    let end = offset + shown;
    let body = String::from_utf8_lossy(&data[..shown]).into_owned();
    let at_eof = u64::try_from(end).unwrap_or(u64::MAX) >= fd_len;
    // The recipe is reserved (with its newline) before the body spends any of the bound,
    // and the cut marker is reserved once: `CUT_MARKER.len()` is already inside the
    // reservation, so the body budget is never subtracted a second time.
    let recipe = if at_eof {
        None
    } else {
        Some(re_fetch_recipe(path, end, limits))
    };
    let reserved_recipe = recipe.as_ref().map(|recipe| recipe.len() + 1).unwrap_or(0);
    // The header is part of the reservation, but its CAUSE text depends on `body_cut`, which
    // depends on the budget. Break the cycle with a probe: the "window ... at requested
    // max_output_bytes" variant is the LONGEST header for a given window, and it is a pure
    // function of the window numbers, so the output stays deterministic. Every other branch
    // (including the at-limit and clamped causes) is strictly shorter, so the framed total
    // never exceeds `max_output`.
    let header_probe = format!(
        "[{offset}..{end} of {fd_len} bytes; window {shown} bytes at requested max_output_bytes **truncated**]"
    );
    let fixed = header_probe.len() + 1 /*newline*/ + CUT_MARKER.len() + reserved_recipe;
    let body_budget = limits.max_output.saturating_sub(fixed);
    let body_rendered = utf8_prefix(&body, body_budget);
    let body_cut = body_rendered.len() < body.len();
    // The recipe resumes at the last body byte actually rendered, so no byte of the file
    // is ever skipped: when the framing spent part of the window, the tail it spent is
    // re-read rather than dropped (issue 125).
    let resume = offset + body_rendered.len();
    let recipe = recipe.map(|recipe| {
        if resume == end {
            recipe
        } else {
            re_fetch_recipe(path, resume, limits)
        }
    });
    // The cause comes from the numbers of this window, not from the flags the caller
    // happened to pass: a request larger than the bound is not "at requested
    // max_output_bytes" when the bound was smaller — the bound is named. Any real header is
    // at most as long as the probe, so the framed total stays within `max_output`.
    let at_limit = limit.is_some_and(|n| usize::try_from(n).unwrap_or(usize::MAX) == shown);
    // The header degrades when the bound is tight: the verbose cause variants are the
    // informative form, but the recipe is the one line that must survive, so when the
    // longest verbose header cannot fit beside the reserved recipe the short truncated
    // header replaces it (issue 125). Deterministic: a pure function of the window
    // numbers, the floor, and the bound.
    let verbose_fits = fixed <= limits.max_output;
    let header = if !verbose_fits {
        if at_eof {
            format!("[{offset}..{end} of {fd_len} bytes]")
        } else {
            format!("[{offset}..{end} of {fd_len} bytes **truncated**]")
        }
    } else if at_eof {
        format!("[{offset}..{end} of {fd_len} bytes]")
    } else if at_limit {
        format!(
            "[{offset}..{end} of {fd_len} bytes; window {shown} bytes at requested limit **truncated**]"
        )
    } else if limits.requested_window.is_some_and(|bound| bound == shown) {
        // The window stopped exactly at the caller's own byte bound: the request is the
        // cause, and a body abbreviated by the framing is what the cut marker and the
        // recipe communicate.
        format!(
            "[{offset}..{end} of {fd_len} bytes; window {shown} bytes at requested max_output_bytes **truncated**]"
        )
    } else if body_cut {
        // The per-call bound cut the body of this window: the bound is the cause.
        format!(
            "[{offset}..{end} of {fd_len} bytes; window clamped to {shown} bytes by output budget **truncated**]"
        )
    } else {
        format!("[{offset}..{end} of {fd_len} bytes **truncated**]")
    };
    let mut out = String::with_capacity(limits.max_output);
    out.push_str(&header);
    out.push('\n');
    out.push_str(body_rendered);
    if body_cut {
        out.push_str(CUT_MARKER);
    }
    if let Some(recipe) = recipe.as_deref() {
        out.push('\n');
        out.push_str(recipe);
    }
    out
}

/// The note a budget-clamped `search_file_content` result carries (issue 125).
fn clamp_note(limit: usize) -> String {
    format!(" [output clamped to {limit} bytes by output budget]")
}

/// The search result bound after reserving the clamp-note bytes, so the note is never the
/// part that gets cut.
pub(super) fn search_limit(limit: usize, budget_clamped: bool) -> usize {
    if budget_clamped {
        limit.saturating_sub(clamp_note(limit).len())
    } else {
        limit
    }
}

/// Appends the clamp note to a bounded search result, but only when the bounded output
/// actually reached its bound: a coarse clamp flag with output far below it was not
/// clamped, and disclosing it would be false (issue 125).
pub(super) fn append_search_clamp(out: String, limit: usize, budget_clamped: bool) -> String {
    if !budget_clamped || out.len() < search_limit(limit, budget_clamped) {
        return out;
    }
    format!("{out}{}", clamp_note(limit))
}

/// The smallest frame a read of this file can produce: the plain truncated header for
/// the widest window the bound allows, the newline, and the whole re-fetch recipe. A
/// per-call bound below it is refused before framing (issue 125).
fn required_output(
    rel: &str,
    fd_len: u64,
    offset: u64,
    max_output: usize,
    digest_size_floor: usize,
    config_output: usize,
) -> usize {
    let shown_max = usize::try_from(fd_len)
        .unwrap_or(usize::MAX)
        .min(super::MAX_FILE_BYTES)
        .min(max_output);
    let start = usize::try_from(offset).unwrap_or(usize::MAX);
    let end = start.saturating_add(shown_max);
    let header = format!("[{start}..{end} of {fd_len} bytes **truncated**]");
    let recipe = re_fetch_recipe(
        rel,
        0,
        &ReadLimits {
            max_output,
            digest_size_floor,
            config_output,
            requested_window: None,
        },
    );
    header.len() + 1 /*newline*/ + recipe.len()
}

/// Reads one framed window of a workspace file through the retained descriptor,
/// resolving the path no-follow and rendering it with [`frame_read`].
pub(super) fn read_file_tool(
    cap: &WorkspaceCap,
    args: &BTreeMap<String, JsonValue>,
    max_output: usize,
    digest_size_floor: usize,
    config_output: usize,
    _budget_clamped: bool,
) -> Result<String, String> {
    super::reject_unknown(args, &["path", "offset", "limit", "max_output_bytes"])?;
    let rel = super::arg_str(args, "path", true)?.ok_or("read_file: path is required")?;
    let offset = super::arg_u64(args, "offset")?;
    let limit = super::arg_u64(args, "limit")?;
    let comps = super::resolve_comps(rel)?;
    if comps.is_empty() {
        return Err("path must name a file".into());
    }
    let (leaf_last, parent_comps) = comps.split_last().unwrap();
    let dir = super::ws_root(cap)?;
    let parent = super::ensure_parent_dir_read(dir, parent_comps)?;
    // The final entry is opened nonblocking/no-follow and its descriptor metadata is
    // checked: a regular file reads, everything else (FIFO, socket, device, directory)
    // is a typed error. A FIFO is opened with `O_NONBLOCK` and we return a typed
    // error without ever reading it (no blocking, no helper writer needed).
    let file =
        super::open_regular_at(&parent, leaf_last).map_err(|e| format!("open {leaf_last}: {e}"))?;
    let meta = file
        .metadata()
        .map_err(|e| format!("fstat {leaf_last}: {e}"))?;
    if !meta.is_file() {
        return Err(format!("{leaf_last} is not a regular file"));
    }
    let fd_len = meta.len();
    let offset = offset.unwrap_or(0);
    if offset > fd_len {
        return Err(format!("offset {offset} is beyond the {fd_len} byte file"));
    }
    // A bound that cannot carry even the plain header, the newline, and the whole
    // recipe would have its recipe cut by the final truncate, leaving an unparseable
    // fragment. Refuse after fstat so the probe uses this file's real numbers
    // (issue 125).
    let needed = required_output(
        rel,
        fd_len,
        offset,
        max_output,
        digest_size_floor,
        config_output,
    );
    if (max_output as u64) < needed as u64 {
        return Err(format!(
            "output budget {max_output} bytes is too small to render a read window and its re-fetch recipe"
        ));
    }
    let offset = offset as usize;
    let mut file = file;
    if offset > 0 {
        file.seek(SeekFrom::Start(offset as u64))
            .map_err(|e| format!("seek {leaf_last}: {e}"))?;
    }
    // Read `cap + 1` when a limit is given so an exact-window request that reached the
    // end is never mistaken for a truncated read; the read stays bounded by the file size,
    // MAX_FILE_BYTES, and max_output.
    let max_window = usize::try_from(fd_len)
        .unwrap_or(usize::MAX)
        .min(super::MAX_FILE_BYTES)
        .min(max_output);
    let cap = match limit {
        Some(n) => usize::try_from(n)
            .unwrap_or(usize::MAX)
            .saturating_add(1)
            .min(max_window),
        None => max_window,
    };
    let data = super::drain_bytes(file, cap).map_err(|error| format!("read {rel}: {error}"))?;
    let shown = match limit {
        Some(n) => usize::try_from(n).unwrap_or(usize::MAX).min(data.len()),
        None => data.len(),
    };
    // Only the turn's remaining output budget can clamp the effective window below what
    // the caller asked for; a caller `limit`/`max_output_bytes` is a request, not a
    // clamp, and is named as such in the header (issue 125).
    let limits = ReadLimits {
        max_output,
        digest_size_floor,
        config_output,
        requested_window: super::arg_u64(args, "max_output_bytes")
            .ok()
            .flatten()
            .and_then(|value| usize::try_from(value).ok()),
    };
    Ok(frame_read(
        rel, offset, shown, fd_len, limit, &limits, &data,
    ))
}
