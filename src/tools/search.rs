use super::*;

#[cfg(test)]
mod tests;

pub(super) struct SearchCounters {
    depth: usize,
    entries: usize,
    source_bytes: usize,
    pub(super) result_bytes: usize,
    results: usize,
    pub(super) reasons: Vec<&'static str>,
    error: Option<String>,
}

impl SearchCounters {
    pub(super) fn new() -> SearchCounters {
        SearchCounters {
            depth: 0,
            entries: 0,
            source_bytes: 0,
            result_bytes: 0,
            results: 0,
            reasons: Vec::new(),
            error: None,
        }
    }

    /// `true` when any cap has already been reached, so traversal stops before the next step.
    fn stopped(&self) -> bool {
        self.error.is_some()
            || self.depth >= MAX_SEARCH_DEPTH
            || self.entries >= MAX_SEARCH_ENTRIES
            || self.source_bytes >= MAX_SEARCH_SOURCE_BYTES
            || self.result_bytes >= MAX_SEARCH_DATA_BYTES
            || self.reasons.contains(&"result_count")
    }

    /// Record that an entry was visited. The cap is checked before the walk descends into
    /// it, so an exact-cap run never opens or reads the (cap+1)-th entry.
    fn visit(&mut self) {
        self.entries = self.entries.saturating_add(1);
        if self.entries >= MAX_SEARCH_ENTRIES && !self.reasons.contains(&"entries") {
            self.reasons.push("entries");
        }
    }

    fn add_source(&mut self, n: usize) {
        self.source_bytes = self.source_bytes.saturating_add(n);
        if self.source_bytes >= MAX_SEARCH_SOURCE_BYTES && !self.reasons.contains(&"source_bytes") {
            self.reasons.push("source_bytes");
        }
    }

    /// Add exact serialized result bytes, including an inter-result newline.
    fn add_result(&mut self, n: usize) {
        self.result_bytes = self.result_bytes.saturating_add(n);
        if self.result_bytes >= MAX_SEARCH_DATA_BYTES {
            self.add_reason("result_bytes");
        }
    }

    fn add_reason(&mut self, reason: &'static str) {
        if !self.reasons.contains(&reason) {
            self.reasons.push(reason);
        }
    }

    fn trunc_note(&self) -> String {
        if self.reasons.is_empty() {
            String::new()
        } else {
            format!(" [truncated reasons: {}]", self.reasons.join(","))
        }
    }
}

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

/// Push a serialized result without crossing `limit`. If only part fits, retain a UTF-8-safe
/// prefix and stop the walk. The separator is part of the accounting.
pub(super) fn push_search_result(
    counters: &mut SearchCounters,
    results: &mut Vec<String>,
    result: String,
    limit: usize,
) -> bool {
    let separator = usize::from(!results.is_empty());
    let remaining = limit.saturating_sub(counters.result_bytes);
    let needed = separator.saturating_add(result.len());
    if needed <= remaining {
        results.push(result);
        counters.add_result(needed);
        return true;
    }

    counters.add_reason("result_bytes");
    if remaining > separator {
        let clipped = utf8_prefix(&result, remaining - separator).to_string();
        if !clipped.is_empty() {
            let added = separator + clipped.len();
            results.push(clipped);
            counters.add_result(added);
        }
    }
    false
}
pub(super) fn render_search_results(results: &[String], note: &str, limit: usize) -> String {
    let joined = results.join("\n");
    let mut out = utf8_prefix(&joined, limit).to_string();
    if !note.is_empty() {
        let remaining = limit.saturating_sub(out.len());
        out.push_str(utf8_prefix(note, remaining));
    }
    out
}

/// Re-open the retained search root descriptor-relative with an independent directory offset.
/// `try_clone`/`dup` would share the directory stream offset and make later searches incomplete.
fn reopen_search_root(root: &openat::Dir) -> Result<openat::Dir, String> {
    reopen_directory(root)
}

/// The start directory of a search, opened descriptor-relative from the retained root with
/// every component no-follow.
fn search_start(cap: &WorkspaceCap, rel: Option<&str>) -> Result<openat::Dir, String> {
    let d = ws_root(cap)?;
    match rel {
        None | Some("") => reopen_search_root(d),
        Some(r) => {
            let comps = resolve_comps(r)?;
            if comps.is_empty() {
                return reopen_search_root(d);
            }
            let (leaf_last, parent_comps) = comps.split_last().unwrap();
            if parent_comps.is_empty() {
                open_named_dir(d, leaf_last)
            } else {
                open_named_dir(&ensure_parent_dir_read(d, parent_comps)?, leaf_last)
            }
        }
    }
}

fn search_prefix(rel: Option<&str>) -> Result<String, String> {
    let Some(relative) = rel.filter(|value| !value.is_empty()) else {
        return Ok(String::new());
    };
    Ok(resolve_comps(relative)?.join("/"))
}
/// Iterate one directory's immediate children, descending into subdirectories one level at a time
/// and reading regular files (both bounded by the hard caps). No symlink component is ever
/// followed; a special file is skipped without opening or reading it; every cap is checked
/// before the step that would cross it (a count of `N` is never exceeded).
struct SearchSpec<'a> {
    regex: &'a regex_lite::Regex,
    max_results: usize,
    result_data_limit: usize,
}

struct SearchLocation<'a> {
    dir: &'a openat::Dir,
    depth: usize,
    prefix: &'a str,
}

fn search_walk(
    location: SearchLocation<'_>,
    counters: &mut SearchCounters,
    spec: &SearchSpec<'_>,
    results: &mut Vec<String>,
) {
    if counters.stopped() {
        return;
    }
    counters.depth = location.depth;
    if location.depth >= MAX_SEARCH_DEPTH {
        counters.add_reason("depth");
        return;
    }
    let entries = match location.dir.list_self() {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries {
        if counters.entries >= MAX_SEARCH_ENTRIES {
            counters.add_reason("entries");
            return;
        }
        let Ok(entry) = entry else {
            continue;
        };
        counters.visit();
        if counters.stopped() {
            return;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if matches!(name.as_str(), "target" | "node_modules" | ".git") {
            continue;
        }
        let Some(kind) = search_entry_type(location.dir, &name, entry.simple_type()) else {
            continue;
        };
        if search_entry(&location, counters, spec, results, &name, kind) {
            return;
        }
    }
}

fn search_entry_type(
    dir: &openat::Dir,
    name: &str,
    known: Option<openat::SimpleType>,
) -> Option<openat::SimpleType> {
    if known.is_some() {
        return known;
    }
    let metadata = dir.metadata(name).ok()?;
    if metadata.is_dir() {
        Some(openat::SimpleType::Dir)
    } else if metadata.is_file() {
        Some(openat::SimpleType::File)
    } else {
        None
    }
}

fn search_entry(
    location: &SearchLocation<'_>,
    counters: &mut SearchCounters,
    spec: &SearchSpec<'_>,
    results: &mut Vec<String>,
    name: &str,
    kind: openat::SimpleType,
) -> bool {
    if kind == openat::SimpleType::Symlink {
        return false;
    }
    let relative = if location.prefix.is_empty() {
        name.to_string()
    } else {
        format!("{}/{name}", location.prefix)
    };
    if kind == openat::SimpleType::Dir {
        if let Ok(child) = open_named_dir(location.dir, name) {
            search_walk(
                SearchLocation {
                    dir: &child,
                    depth: location.depth + 1,
                    prefix: &relative,
                },
                counters,
                spec,
                results,
            );
        }
        return counters.stopped();
    }
    search_regular_file(location.dir, counters, spec, results, name, &relative)
}

fn read_search_source(
    reader: impl std::io::Read,
    read_cap: usize,
    relative: &str,
) -> Result<Vec<u8>, String> {
    drain_bytes(reader, read_cap).map_err(|error| format!("read {relative}: {error}"))
}

fn search_regular_file(
    dir: &openat::Dir,
    counters: &mut SearchCounters,
    spec: &SearchSpec<'_>,
    results: &mut Vec<String>,
    name: &str,
    relative: &str,
) -> bool {
    if counters.source_bytes >= MAX_SEARCH_SOURCE_BYTES {
        counters.add_reason("source_bytes");
        return true;
    }
    let file = match open_regular_at(dir, name) {
        Ok(file) => file,
        Err(_) => return false,
    };
    let metadata = match file.metadata() {
        Ok(metadata) if metadata.is_file() => metadata,
        _ => return false,
    };
    if metadata.len() > MAX_FILE_BYTES as u64 {
        counters.add_reason("file_bytes");
    }
    let read_cap = MAX_FILE_BYTES.min(MAX_SEARCH_SOURCE_BYTES - counters.source_bytes);
    let data = match read_search_source(file, read_cap, relative) {
        Ok(data) => data,
        Err(error) => {
            counters.error = Some(error);
            return true;
        }
    };
    counters.add_source(data.len());
    let stopped = search_lines(
        counters,
        spec,
        results,
        relative,
        &String::from_utf8_lossy(&data),
    );
    stopped || counters.stopped()
}

fn search_lines(
    counters: &mut SearchCounters,
    spec: &SearchSpec<'_>,
    results: &mut Vec<String>,
    name: &str,
    text: &str,
) -> bool {
    for (index, line) in text.lines().enumerate() {
        if counters.results >= spec.max_results {
            counters.add_reason("result_count");
            return true;
        }
        if !spec.regex.is_match(line) {
            continue;
        }
        let result = format!("{name}:{}: {}", index + 1, truncate(line, MAX_LINE_BYTES));
        if !push_search_result(counters, results, result, spec.result_data_limit) {
            return true;
        }
        counters.results = counters.results.saturating_add(1);
        if counters.results >= spec.max_results {
            counters.add_reason("result_count");
            return true;
        }
    }
    counters.stopped()
}

pub(super) fn search_file_content_tool(
    cap: &WorkspaceCap,
    args: &BTreeMap<String, JsonValue>,
    max_output_bytes: usize,
) -> Result<String, String> {
    reject_unknown(args, &["pattern", "max_results", "path"])?;
    let pattern = arg_str(args, "pattern", true)?.unwrap();
    let max = bounded(
        arg_u64(args, "max_results")?,
        DEFAULT_SEARCH_RESULTS,
        MAX_SEARCH_RESULTS,
    );
    if max == 0 {
        return Ok("no matches".into());
    }
    let re =
        regex_lite::Regex::new(pattern).map_err(|e| format!("invalid pattern {pattern:?}: {e}"))?;
    let rel = arg_str(args, "path", false)?;
    let start = search_start(cap, rel)?;
    let prefix = search_prefix(rel)?;
    let result_limit = MAX_SEARCH_RESULT_BYTES.min(max_output_bytes);
    let result_data_limit = result_limit;
    let mut counters = SearchCounters::new();
    let mut results = Vec::new();
    let spec = SearchSpec {
        regex: &re,
        max_results: max,
        result_data_limit,
    };
    search_walk(
        SearchLocation {
            dir: &start,
            depth: 0,
            prefix: &prefix,
        },
        &mut counters,
        &spec,
        &mut results,
    );
    if let Some(error) = counters.error {
        return Err(error);
    }

    let note = counters.trunc_note();
    let final_out = if results.is_empty() {
        if note.is_empty() {
            "no matches".into()
        } else {
            format!("no matches{note}")
        }
    } else {
        let note = truncate(&note, MAX_SEARCH_NOTE_BYTES.min(result_limit));
        render_search_results(&results, &note, result_limit)
    };
    let final_out = truncate(&final_out, result_limit);
    debug_assert!(final_out.len() <= result_limit);
    Ok(final_out)
}
