//! Tool output-cap, schema, and diagnostic ownership.
//!
//! Phase 0 moves existing agent defaults behind this seam. Phase 7 adds per-call
//! output-cap behavior here and its test matrix in `src/tools/tests/output_caps.rs`.

/// Default hard cap on the model-visible bytes of one shell success or error string
/// (framing and combined output included).
pub const MAX_SHELL_OUTPUT_DEFAULT: usize = 32 * 1024;
/// Default cap on one tool result. The aggregate turn-output budget remains the single
/// owner of this value; production also limits each call by the bytes left in that budget.
pub const MAX_TOOL_OUTPUT_DEFAULT: usize = crate::limits::MAX_TURN_OUTPUT_BYTES;
