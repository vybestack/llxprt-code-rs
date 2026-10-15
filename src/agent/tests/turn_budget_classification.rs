//! Classification at each terminal budget boundary, including the defensive
//! output-byte check (normal tool ingress clips output before it can exceed it).
use super::*;
use crate::envelope::Code;

#[test]
fn turn_byte_caps_keep_their_keys_messages_and_exit_status() {
    let caps = OutputCaps::default();
    let cases = [
        (
            MAX_TURN_ASSISTANT_BYTES + 1,
            0,
            0,
            format!("turn assistant content would exceed the {MAX_TURN_ASSISTANT_BYTES} byte cap"),
        ),
        (
            0,
            MAX_TURN_ARGS_BYTES + 1,
            0,
            format!("turn tool-call arguments would exceed the {MAX_TURN_ARGS_BYTES} byte cap"),
        ),
        (
            0,
            0,
            caps.turn + 1,
            format!("turn tool output would exceed the {} byte cap", caps.turn),
        ),
    ];
    for (assistant, args, output, message) in cases {
        let root = tempfile::tempdir().unwrap();
        let store =
            SessionStore::load_at(&SessionId::parse("byte-caps").unwrap(), root.path()).unwrap();
        let reserved = store.start_request(None, None, "P", root.path()).unwrap();
        let agent = CodingAgent::with_backend(
            Box::new(MockBackend::new(vec![])),
            root.path().into(),
            false,
        );
        let turn = deadline::Turn::new(&agent).unwrap();
        turn.enforce_turn_caps(
            MAX_TURN_ASSISTANT_BYTES,
            MAX_TURN_ARGS_BYTES,
            caps.turn,
            &store,
            &reserved,
            &[],
        )
        .expect("exact caps are admitted");
        let error = turn
            .enforce_turn_caps(assistant, args, output, &store, &reserved, &[])
            .unwrap_err();
        assert_eq!(error.key, "turn-budget");
        assert_eq!(error.message, message);
        assert_eq!(error.code, Code::Turn);
        assert_eq!(error.code as i32, 6, "CLI exit status");
        assert_eq!(
            store.snapshot().unwrap().branches[0].lifecycle,
            Lifecycle::Failed
        );
        let retry = store.start_request(None, None, "P", root.path()).unwrap();
        assert!(retry.retry, "budget failure releases the reservation");
    }
}
