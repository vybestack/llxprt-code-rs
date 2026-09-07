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

/// The per-call output caps one agent turn applies to every tool call, resolved from
/// the settings layers. `Default` reproduces the built-in constants byte-for-byte, so
/// an unconfigured run behaves exactly as before they were raisable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutputLimits {
    /// Model-visible byte cap for one tool result.
    pub max_tool_output: usize,
    /// Model-visible byte cap for one shell success or error string.
    pub max_shell_output: usize,
}

impl Default for OutputLimits {
    fn default() -> Self {
        Self {
            max_tool_output: MAX_TOOL_OUTPUT_DEFAULT,
            max_shell_output: MAX_SHELL_OUTPUT_DEFAULT,
        }
    }
}
