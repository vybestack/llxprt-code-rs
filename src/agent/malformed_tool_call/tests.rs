//! Detector tests: prose stays safe, tag-shaped and stray-DSML shapes fire.

use super::*;

#[test]
fn ordinary_prose_never_fires() {
    for text in [
        "Done. The change is complete and tests pass.",
        "I would use read_file to look at the file first.",
        "The tools available are read_file, write_file, and replace.",
        "Consider a < b comparison before calling search_file_content.",
        "The model emitted </DSML/parameter> in an earlier run.",
        "See the docs for <tooling> and <parameters> in the README.",
        "",
        "   \n  ",
    ] {
        assert_eq!(
            trigger_for(text, false),
            None,
            "prose must not fire: {text}"
        );
        assert_eq!(classify(text, 0, false), None);
    }
}

#[test]
fn tag_shaped_blocks_fire() {
    for text in [
        "<tool_calls></tool_calls>",
        "<function_calls><invoke>read_file</invoke></function_calls>",
        "<read_file path=\"src/lib.rs\"></read_file>",
        "<read_file\n  path=\"src/lib.rs\"\n/>",
        "Let me look at that.\n<tool_call>{\"name\":\"list_directory\"}</tool_call>",
        "prefix prose </function_calls> trailing prose",
        "<invoke name=\"write_file\">\n<parameter>path</parameter>\n</invoke>",
    ] {
        assert!(
            trigger_for(text, false).is_some(),
            "tag-shaped text must fire: {text}"
        );
        let (trigger, message) = classify(text, 0, false).expect("classified");
        assert_eq!(trigger, Trigger::TagBlock);
        assert!(!message.contains("src/lib.rs"), "no model text in message");
    }
}

#[test]
fn stray_dsml_fragment_fires() {
    for text in [
        "</｜DSML｜parameter>",
        "work so far\n</｜DSML｜parameter>\n</｜DSML｜tool_calls>",
        "<｜DSML｜tool_calls></｜DSML｜tool_calls>",
    ] {
        let (trigger, message) = classify(text, 0, false).expect("dsml shape classified");
        assert_eq!(trigger, Trigger::DsmlFragment, "text: {text}");
        assert!(!message.contains('<'), "no model text in message");
    }
    // An ASCII pipe look-alike is not a DSML fragment.
    assert_eq!(classify("</|DSML|parameter>", 0, false), None);
}

#[test]
fn shell_tool_requires_the_shell_opt_in() {
    let text = "<run_shell_command></run_shell_command>";
    assert_eq!(classify(text, 0, false), None);
    assert!(trigger_for(text, true).is_some());
}

#[test]
fn a_parsed_call_is_never_malformed() {
    let text = "<tool_calls></tool_calls>";
    assert_eq!(classify(text, 1, false), None);
}

#[test]
fn unknown_tag_names_are_inert() {
    for text in [
        "<unknown_tool></unknown_tool>",
        "<b>bold</b>",
        "<note>hi</note>",
    ] {
        assert_eq!(classify(text, 0, false), None, "text: {text}");
    }
}

#[test]
fn zero_call_tail_counts_trailing_call_free_rounds() {
    let with_calls = crate::session::RoundRecord {
        assistant: "r".into(),
        calls: vec![crate::session::ToolCallRecord {
            id: "c1".into(),
            name: "list_directory".into(),
            args: "{}".into(),
            result: "[]".into(),
            ok: true,
            refused: false,
        }],
    };
    let bare = |text: &str| crate::session::RoundRecord {
        assistant: text.into(),
        calls: Vec::new(),
    };
    assert_eq!(zero_call_tail(&[]), 0);
    assert_eq!(zero_call_tail(&[bare("a")]), 1);
    assert_eq!(zero_call_tail(&[with_calls.clone(), bare("a")]), 1);
    assert_eq!(
        zero_call_tail(&[bare("a"), with_calls, bare("b"), bare("c")]),
        2
    );
}
