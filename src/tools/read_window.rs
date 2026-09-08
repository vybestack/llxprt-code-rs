//! Framed-read rendering with visible bounds and replayable re-fetch recipes (issue 125).
//!
//! A clamped read used to shrink silently: the agent's remaining turn budget became the
//! per-call cap with no trace in the result, so the model re-read the same file with the
//! same oversized window and received a smaller prefix each time. Every truncated window
//! now states its effective bound and the cause of the clamp, and carries exactly one
//! replayable `re-fetch:` recipe as its last line. The recipe's window is sized to come
//! back verbatim under the bulk seam and the per-result cap — it stays under the digest
//! admission floor AND the seam's fixed 1024-byte bulk threshold, so the re-fetch is not
//! digested on the way back in — subject to path length in the recipe (long paths consume
//! the frame budget), and its bytes are reserved inside the per-call bound so the recipe
//! can never be the part that gets cut.

/// Headroom carved off the digest admission floor when sizing a re-fetch window: the
/// framed header rides in the same budget, so the recipe window must stay strictly under
/// the floor. One eighth of the floor, minimum one byte.
const FLOOR_MARGIN_DIVISOR: usize = 8;
/// Ceiling on a re-fetch window's `max_output_bytes` (issue 125 cycle 2). The checkpoint
/// seam (`compact_tool_result`) digests any result at or above `BULK_RESULT_BYTES` (1024)
/// regardless of the configured floor, so a window that merely stays under a raised floor
/// is still re-digested on the way back in. A re-fetch frame carries its own header and
/// recipe beside the body, so the offered window must leave room for that framing; 512
/// plus the worst realistic framing stays below 1024.
const REFETCH_VERBATIM_MAX: usize = 512;
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
/// re-fetch is never digested on the way back in), at or below [`REFETCH_VERBATIM_MAX`]
/// so the bulk seam's fixed threshold cannot digest the re-framed result either, and
/// deliverable inside the configured per-result cap.
pub(super) fn recipe_window(floor: usize, config_output: usize) -> usize {
    safe_window(floor)
        .min(config_output)
        .clamp(1, REFETCH_VERBATIM_MAX)
}

/// The generic recipe line a CTXDIGEST record carries (issue 125). The digest path cannot
/// know the original path or the caller's cap, so the instruction names the parameters and
/// the safe window. A pure function of the floor, so checkpoint and finalize
/// re-derivations of the same record stay byte-identical.
pub(crate) fn digest_re_fetch_line(floor: usize) -> String {
    let safe = safe_window(floor).clamp(1, REFETCH_VERBATIM_MAX);
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

/// The lossy rendering of the longest RAW prefix of `data` whose rendered length stays
/// within `max_rendered`, together with the raw byte count it covers.
///
/// `String::from_utf8_lossy` cannot answer "how many raw bytes did the rendering cover",
/// and a naive rendering/offset arithmetic drifts: each invalid subsequence renders as one
/// U+FFFD (3 rendered bytes) while consuming a different raw count. This helper walks
/// `std::str::from_utf8`'s error information instead, so the recipe's resume offset is
/// always expressed in RAW file bytes (issue 125 cycle 2).
fn utf8_lossy_prefix(data: &[u8], max_rendered: usize) -> (String, usize) {
    let mut out = String::new();
    let mut raw = 0usize;
    while raw < data.len() {
        let room = max_rendered - out.len();
        if room == 0 {
            break;
        }
        match std::str::from_utf8(&data[raw..]) {
            Ok(valid) => {
                let chunk = utf8_prefix(valid, room);
                if chunk.is_empty() {
                    break;
                }
                out.push_str(chunk);
                raw += chunk.len();
                // The whole remaining input is valid: the decode is complete.
                break;
            }
            Err(error) => {
                let valid_up_to = error.valid_up_to();
                let valid = std::str::from_utf8(&data[raw..raw + valid_up_to])
                    .expect("the reported prefix is valid UTF-8");
                let chunk = utf8_prefix(valid, room);
                out.push_str(chunk);
                raw += chunk.len();
                if chunk.len() < valid.len() {
                    // The budget stopped inside the valid run: the invalid subsequence is
                    // left unconsumed for the next window.
                    break;
                }
                let invalid_len = error.error_len();
                if out.len() + 3 > max_rendered {
                    // The U+FFFD itself would overflow the budget: stop before the
                    // subsequence rather than rendering a partial replacement.
                    break;
                }
                out.push('\u{FFFD}');
                raw += match invalid_len {
                    Some(len) => len,
                    // A subsequence truncated by the end of `data`: the replacement
                    // stands for every remaining raw byte.
                    None => data.len() - raw,
                };
            }
        }
    }
    (out, raw)
}

/// The cause line for one window, chosen from the window numbers (issue 125 cycle 2).
///
/// The cause comes from the numbers of this window, not from the flags the caller
/// happened to pass: a request larger than the bound is not "at requested
/// max_output_bytes" when the bound was smaller — the bound is named.
fn window_header(
    offset: usize,
    fd_len: u64,
    shown: usize,
    limit: Option<u64>,
    limits: &ReadLimits,
    at_eof_complete: bool,
    body_cut: bool,
) -> String {
    let end = offset + shown;
    let at_limit = limit.is_some_and(|n| usize::try_from(n).unwrap_or(usize::MAX) == shown);
    if at_eof_complete {
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
    }
}

/// Renders the window body under the framing reservation and derives its recipe.
///
/// An EOF window needs no recipe while the budget carried its whole body; if the budget
/// abbreviated the body, the cut must be visible and resumable, so the recipe is
/// re-derived with its own reservation (issue 125 cycle 2).
fn render_body(
    data: &[u8],
    shown: usize,
    max_output: usize,
    probe_len: usize,
    at_eof: bool,
    recipe_at: impl Fn(usize) -> String,
    end: usize,
) -> (String, usize, Option<String>) {
    let render = |fixed: usize| utf8_lossy_prefix(data, max_output.saturating_sub(fixed));
    if !at_eof {
        let recipe = recipe_at(end);
        let fixed = probe_len + 1 + CUT_MARKER.len() + recipe.len() + 1;
        let (body, raw_covered) = render(fixed);
        return (body, raw_covered, Some(recipe));
    }
    let (body, raw_covered) = render(probe_len + 1);
    if raw_covered >= shown {
        return (body, raw_covered, None);
    }
    let recipe = recipe_at(end);
    let fixed = probe_len + 1 + CUT_MARKER.len() + recipe.len() + 1;
    let (body, raw_covered) = render(fixed);
    (body, raw_covered, Some(recipe))
}

/// Renders one framed read window. `shown` is the byte count of `data` the window exposes,
/// `fd_len` the whole file, and `limit` the caller's explicit `limit` argument if any.
///
/// The frame is built byte-exactly instead of by reservation guesswork: the body is
/// rendered first into the room the framing leaves, the real header variant is chosen
/// from the window numbers, and the assembled total is checked against `max_output`,
/// degrading to the short truncated header (and refusing, when a recipe is required)
/// rather than emitting a frame the per-result truncate would cut mid-recipe.
pub(super) fn frame_read(
    path: &str,
    offset: usize,
    shown: usize,
    fd_len: u64,
    limit: Option<u64>,
    limits: &ReadLimits,
    data: &[u8],
) -> Result<String, String> {
    let end = offset + shown;
    let at_eof = u64::try_from(end).unwrap_or(u64::MAX) >= fd_len;
    // The probe header is the longest header variant for these window numbers, so
    // reserving it can only over-reserve; the real header is picked after the body is
    // rendered and the assembled total is verified against the bound.
    let probe_header = format!(
        "[{offset}..{end} of {fd_len} bytes; window {shown} bytes at requested max_output_bytes **truncated**]"
    );
    let recipe_at = |at: usize| re_fetch_recipe(path, at, limits);
    let (body, raw_covered, recipe) = render_body(
        &data[..shown],
        shown,
        limits.max_output,
        probe_header.len(),
        at_eof,
        recipe_at,
        end,
    );
    let body_cut = raw_covered < shown;
    // The recipe resumes at the last RAW file byte actually rendered, so no byte of the
    // file is ever skipped: when the framing spent part of the window, the tail it spent
    // is re-read rather than dropped (issue 125).
    let resume = offset + raw_covered;
    let recipe = recipe.map(|recipe| {
        if resume == end {
            recipe
        } else {
            recipe_at(resume)
        }
    });
    let at_eof_complete = at_eof && recipe.is_none();
    let header = window_header(
        offset,
        fd_len,
        shown,
        limit,
        limits,
        at_eof_complete,
        body_cut,
    );
    let assemble = |header: &str, recipe: Option<&str>| {
        let mut out = String::with_capacity(limits.max_output);
        out.push_str(header);
        out.push('\n');
        out.push_str(&body);
        if body_cut {
            out.push_str(CUT_MARKER);
        }
        if let Some(recipe) = recipe {
            out.push('\n');
            out.push_str(recipe);
        }
        out
    };
    let verbose = assemble(&header, recipe.as_deref());
    if verbose.len() <= limits.max_output {
        return Ok(verbose);
    }
    // The header degrades when the bound is tight: the verbose cause variants are the
    // informative form, but the recipe is the one line that must survive, so the short
    // truncated header replaces the verbose one (issue 125). Deterministic: a pure
    // function of the window numbers and the bound.
    let short_header = if at_eof && recipe.is_none() {
        format!("[{offset}..{end} of {fd_len} bytes]")
    } else {
        format!("[{offset}..{end} of {fd_len} bytes **truncated**]")
    };
    let short = assemble(&short_header, recipe.as_deref());
    if short.len() <= limits.max_output || recipe.is_none() {
        return Ok(short);
    }
    Err(format!(
        "output budget {max_output} bytes is too small to render a read window and its re-fetch recipe",
        max_output = limits.max_output
    ))
}

/// The note a budget-clamped `search_file_content` result carries (issue 125).
fn clamp_note(limit: usize) -> String {
    format!(" [output clamped to {limit} bytes by output budget]")
}

/// The search result bound after reserving the clamp-note bytes, so the note is never the
/// part that gets cut. A note that cannot fit (`clamp_note(limit).len() >= limit`) makes
/// the reservation saturate to zero, so it is skipped instead: `limit` is returned
/// unchanged and the outer bound never truncates the note into a false disclosure.
pub(super) fn search_limit(limit: usize, budget_clamped: bool) -> usize {
    if budget_clamped && clamp_note(limit).len() < limit {
        limit - clamp_note(limit).len()
    } else {
        limit
    }
}

/// Appends the clamp note to a bounded search result, but only when the bounded output
/// actually reached its bound: a coarse clamp flag with output far below it was not
/// clamped, and disclosing it would be false (issue 125). A note the bound cannot carry
/// is skipped, not truncated by the outer truncate.
pub(super) fn append_search_clamp(out: String, limit: usize, budget_clamped: bool) -> String {
    if !budget_clamped
        || clamp_note(limit).len() >= limit
        || out.len() < search_limit(limit, budget_clamped)
    {
        return out;
    }
    format!("{out}{}", clamp_note(limit))
}

/// Reads one framed window of a workspace file through the retained descriptor,
/// resolving the path no-follow and rendering it with [`frame_read`].
pub(super) fn read_file_tool(
    cap: &WorkspaceCap,
    args: &BTreeMap<String, JsonValue>,
    max_output: usize,
    digest_size_floor: usize,
    config_output: usize,
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
    frame_read(rel, offset, shown, fd_len, limit, &limits, &data)
}
