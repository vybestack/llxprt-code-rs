//! Conservative detector for assistant text that carries tool-call intent as malformed
//! prose instead of a parsed call (issue 146).
//!
//! A reply that parses to zero calls is usually an honest final answer, so the detector
//! only fires on structurally tag-shaped text: an angle-bracket delimited block whose
//! name is a known invoke/parameter wrapper or a known tool name, or a stray DSML
//! close delimiter (`</｜DSML｜...>`). Prose that merely mentions a tool name in a
//! sentence never fires, because a bare word is not a tag.

/// Known wrapper tag names models use when they emit a tool call as pseudo-XML text.
const WRAPPER_TAGS: [&str; 6] = [
    "tool_call",
    "tool_calls",
    "function_call",
    "function_calls",
    "invoke",
    "parameter",
];

/// Whether one angle-bracket delimited tag looks like tool-call syntax.
///
/// `raw` is the text between the `<` and the next `>` or `<`, with the optional closing
/// slash and any attributes still in place. Wrapper names and known tool names both
/// count; the attribute payload is never inspected, so no model text is captured.
fn tag_is_tool_shaped(raw: &str, allow_shell: bool) -> bool {
    let trimmed = raw.trim();
    let name: String = trimmed
        .strip_prefix('/')
        .unwrap_or(trimmed)
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    let name = name.as_str();
    !name.is_empty()
        && (WRAPPER_TAGS.contains(&name) || crate::agent::known_tool(name, allow_shell))
}

/// Whether the reply carries a stray DSML fragment delimiter: a `</｜DSML｜...>` close
/// tag emitted as prose, the partial-call shape observed in session journals.
///
/// The vendor tag marker is matched exactly; an ASCII `｜` look-alike is not a DSML
/// fragment and must not fire.
fn has_stray_dsml_tag(text: &str) -> bool {
    text.contains("</｜DSML｜")
}

/// Whether the reply carries an angle-bracket delimited block that names an invoke
/// wrapper or a known tool. Only tag names are inspected, and the scan walks tag by tag
/// so a long reply costs one bounded pass.
fn has_tag_block(text: &str, allow_shell: bool) -> bool {
    let mut cursor = 0;
    while let Some(open) = text[cursor..].find('<') {
        let start = cursor + open;
        let rest = &text[start + 1..];
        let end = rest.find(['>', '<']).unwrap_or(rest.len());
        if tag_is_tool_shaped(&rest[..end], allow_shell) {
            return true;
        }
        // Step past the tag body; a zero-length body still advances past the second `<`.
        cursor = start + 1 + end.max(1);
    }
    false
}

/// Which structural shape fired the detector.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Trigger {
    /// An angle-bracket delimited invoke/parameter block or a tagged tool name.
    TagBlock,
    /// A stray `</｜DSML｜...>` close delimiter with no complete call.
    DsmlFragment,
}

/// Which structural shape fired, for a value-free diagnostic. The DSML fragment is
/// checked first so the shape a journal reader expects is named even when the text
/// also carries an invoke-shaped tag.
pub fn trigger_for(text: &str, allow_shell: bool) -> Option<Trigger> {
    if has_stray_dsml_tag(text) {
        return Some(Trigger::DsmlFragment);
    }
    has_tag_block(text, allow_shell).then_some(Trigger::TagBlock)
}

/// A bounded, value-free description of one detected malformed reply: the trigger class
/// plus the reply's byte count. No model text travels in the message.
fn diagnostic(trigger: Trigger, text_len: usize) -> String {
    match trigger {
        Trigger::TagBlock => format!(
            "final assistant text resembles tool-call syntax as an angle-bracket delimited \
             invoke block ({text_len} bytes) but parsed to zero tool calls"
        ),
        Trigger::DsmlFragment => format!(
            "final assistant text carries a stray DSML close delimiter ({text_len} bytes) \
             but parsed to zero tool calls"
        ),
    }
}

/// Classify a terminal reply that parsed to zero calls. `None` means the text does not
/// look like a tool call and the turn is an ordinary wrap-up.
pub fn classify(text: &str, calls: usize, allow_shell: bool) -> Option<(Trigger, String)> {
    if calls != 0 {
        return None;
    }
    trigger_for(text, allow_shell).map(|trigger| (trigger, diagnostic(trigger, text.len())))
}

/// Count the consecutive trailing rounds of this attempt whose recorded assistant text
/// carried no parsed tool call.
///
/// The persisted tail holds only zero-call rounds (a round with calls is followed by
/// another model reply), so this is the trailing zero-call run the loop actually
/// observed. The final summary round is a zero-call round by construction and counts,
/// which keeps the metric meaningful for a turn that ends on its own text.
pub fn zero_call_tail(rounds: &[crate::session::RoundRecord]) -> u64 {
    u64::try_from(
        rounds
            .iter()
            .rev()
            .take_while(|round| round.calls.is_empty())
            .count(),
    )
    .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests;
