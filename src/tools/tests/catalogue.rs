use super::*;

#[test]
fn tool_name_catalogue_is_shared_by_dispatch_and_persistence() {
    let dispatchable_without_shell = TOOL_CATALOGUE
        .iter()
        .filter(|n| known_tool(n, false))
        .count();
    let persistable = TOOL_CATALOGUE
        .iter()
        .filter(|n| is_known_tool_name(n))
        .count();
    assert_eq!(TOOL_CATALOGUE.len() - 1, dispatchable_without_shell);
    assert_eq!(TOOL_CATALOGUE.len(), persistable);

    for name in TOOL_CATALOGUE {
        assert!(known_tool(name, true));
        assert!(is_known_tool_name(name));
    }

    assert!(!known_tool("run_shell_command", false));
    assert!(is_known_tool_name("run_shell_command"));

    for unknown in ["grep", "sed", "readFile", ""] {
        assert!(!known_tool(unknown, true));
        assert!(!is_known_tool_name(unknown));
    }
}
