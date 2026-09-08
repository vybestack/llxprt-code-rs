//! Codex's fixed production endpoint cannot be redirected by a CLI profile.
//! Exercise its real HTTP backend and assembler, without adding a production override.
use super::*;
use serdes_ai::core::messages::ModelRequestPart;
use std::io::Write;

fn raw_field<'a>(body: &'a str, name: &str) -> &'a str {
    let start = body.find(&format!("\"{name}\":")).unwrap() + name.len() + 3;
    let rest = &body[start..];
    let mut stream = serde_json::Deserializer::from_str(rest).into_iter::<serde_json::Value>();
    stream.next().unwrap().unwrap();
    &rest[..stream.byte_offset()]
}

#[test]
fn codex_exact_http_head_rounds_turns_off_and_missing_cache_usage() {
    for enabled in [false, true] {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let mut bodies = Vec::new();
            for round in 0..4 {
                let (mut socket, _) = listener.accept().unwrap();
                let bytes = super::tests::read_codex_request(&mut socket);
                let start = super::tests::find_body_start(&bytes).unwrap();
                bodies.push(String::from_utf8(bytes[start..].to_vec()).unwrap());
                let mut payload = super::tests::codex_turn_sse_payload(round);
                if round == 0 {
                    payload = payload.replace("\"cached_tokens\":6", "\"other_detail\":6");
                }
                socket.write_all(payload.as_bytes()).unwrap();
            }
            bodies
        });
        let mut history = ModelRequest::default();
        history.add_system_prompt(crate::agent::coding_system_prompt(
            std::path::Path::new("/fixture"),
            "",
            false,
            Some(8),
        ));
        history.add_user_prompt("first turn");
        let tools = crate::tools::tool_specs(false);
        for turn in 0..2 {
            let model = OpenResponsesModel::new("fixture", format!("http://{address}/responses"))
                .codex_http()
                .with_prompt_cache_key(enabled.then(|| "session-fixture".to_string()));
            let backend = ResponsesBackend::new(
                model,
                ModelSettings {
                    timeout: Some(std::time::Duration::from_secs(10)),
                    ..Default::default()
                },
            )
            .unwrap();
            for round in 0..2 {
                let reply = backend.request(&[history.clone()], &tools).unwrap();
                let expected = (turn != 0 || round != 0).then_some(6);
                assert_eq!(reply.usage.cache_read_tokens, expected);
                history.parts.push(ModelRequestPart::UserPrompt(
                    serdes_ai::core::messages::UserPromptPart::new(format!(
                        "appended {turn}/{round}"
                    )),
                ));
            }
        }
        let bodies = server.join().unwrap();
        for (index, body) in bodies.iter().enumerate() {
            assert_eq!(raw_field(body, "tools"), raw_field(&bodies[0], "tools"));
            assert_eq!(
                raw_field(body, "instructions"),
                raw_field(&bodies[0], "instructions")
            );
            let value: serde_json::Value = serde_json::from_str(body).unwrap();
            assert_eq!(
                value.get("prompt_cache_key"),
                enabled.then_some(&serde_json::json!("session-fixture"))
            );
            assert!(value.get("previous_response_id").is_none());
            if index > 0 {
                let previous = raw_field(&bodies[index - 1], "input");
                assert!(raw_field(body, "input").starts_with(&previous[..previous.len() - 1]));
            }
        }
        let instructions = raw_field(&bodies[0], "instructions");
        assert_ne!(
            instructions,
            instructions.replace("You", "Timestamp: 1. You")
        );
    }
}
