use crate::tools::output_limits::{MAX_SHELL_OUTPUT_DEFAULT, MAX_TOOL_OUTPUT_DEFAULT};

#[test]
fn tool_output_defaults_preserve_the_existing_exact_bounds() {
    assert_eq!(MAX_SHELL_OUTPUT_DEFAULT, 32 * 1024);
    assert_eq!(MAX_TOOL_OUTPUT_DEFAULT, 16 * 1024 * 1024);
    assert_eq!(MAX_TOOL_OUTPUT_DEFAULT, crate::agent::MAX_TURN_OUTPUT_BYTES);
}
