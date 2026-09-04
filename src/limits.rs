//! Per-turn byte and count bounds shared by the agent loop (request-time enforcement),
//! the session validator, and the tools output caps, plus the FNV-1a prompt digest used
//! for replay identity. This module is a leaf: it must NOT import anything from other
//! crate modules.

/// Cap on the combined assistant text bytes materialized in one turn.
pub const MAX_TURN_ASSISTANT_BYTES: usize = 1024 * 1024;
/// Cap on the combined raw tool-call argument bytes in one turn.
pub const MAX_TURN_ARGS_BYTES: usize = 1024 * 1024;
/// Cap on the combined tool-result bytes materialized in one turn.
pub const MAX_TURN_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
/// Default cap on the number of assistant/tool rounds in one turn: none. Rounds are
/// uncapped unless the profile (`maxTurnsPerPrompt`) or an explicit value caps them;
/// byte/output caps and the declared tool-call and turn-time budgets still bound the
/// run.
pub const MAX_TURN_ROUNDS: usize = usize::MAX;
/// Hard cap on one model reply (bytes). A reply over this bound is refused by the
/// agent's model-call path as a typed `model` failure rather than ever becoming an
/// unbounded assistant round, so it can never be counted toward the aggregate turn caps or
/// touch session-persist. The error path persists the failure (owner cleared, lease
/// released) with a scrubbed bounded diagnostic.
pub const MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;
/// Maximum UTF-8 byte length of one provider-supplied tool-call identifier.
pub const MAX_TOOL_CALL_ID_BYTES: usize = 256;
/// Maximum UTF-8 byte length of one provider-supplied tool name.
pub const MAX_TOOL_NAME_BYTES: usize = 64;

/// FNV-1a hash of a prompt, used as a compact identity for replay detection.
pub fn prompt_digest(prompt: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in prompt.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}
