use super::*;
use crate::adapter::{ChatBackend, LlmResult};

fn snapshot() -> Snapshot {
    Snapshot {
        call: Observation {
            event: "prompt_cache_call",
            call: Some(1),
            reported_input_tokens: None,
            cached_input_tokens: None,
            uncached_input_tokens: None,
            cache_creation_input_tokens: None,
            total_input_tokens: None,
            input_accounting: "input_includes_cache_reads",
        },
        run: RunCache {
            calls: Some(1),
            measured_calls: Some(0),
            measured_input_tokens: Some(0),
            measured_cached_tokens: Some(0),
            aggregate_valid: true,
            hit_ratio: None,
        },
    }
}

#[test]
fn regular_private_output_retains_descriptor_and_value_free_write_error() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let work = tempfile::tempdir().unwrap();
    let path = work.path().join("private-secret-file");
    let output = Output::create(&path).unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let original = std::fs::metadata(&path).unwrap().ino();
    let moved = work.path().join("moved");
    std::fs::rename(&path, &moved).unwrap();
    std::fs::write(&path, "replacement sentinel").unwrap();
    output.publish(&snapshot()).unwrap();
    assert_eq!(std::fs::metadata(&moved).unwrap().ino(), original);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "replacement sentinel"
    );
    let bytes = std::fs::read_to_string(&moved).unwrap();
    assert_eq!(bytes.lines().count(), 2);
    let first: serde_json::Value = serde_json::from_str(bytes.lines().next().unwrap()).unwrap();
    assert!(first["cached_input_tokens"].is_null());
    output.refuse_writes_for_test();
    let error = output.publish(&snapshot()).unwrap_err();
    assert_eq!(error.stage, "write");
    assert!(!error.to_string().contains("Bad file descriptor"));
    assert!(!error.to_string().contains("private-secret-file"));
    assert!(error.to_string().len() < 128);
    assert_eq!(std::fs::read_to_string(&moved).unwrap(), bytes);
}

struct Completed(std::rc::Rc<std::cell::Cell<usize>>);
impl ChatBackend for Completed {
    fn request<'a>(
        &'a self,
        _: &'a [serdes_ai::core::ModelRequest],
        _: &'a [crate::tools::ToolSpec],
    ) -> crate::adapter::ModelFuture<'a> {
        Box::pin(async move {
            self.0.set(self.0.get() + 1);
            Ok(LlmResult {
                thinking: String::new(),
                text: "done".into(),
                calls: vec![],
                usage: Default::default(),
                finish_reason: Some(serdes_ai::core::messages::FinishReason::Stop),
            })
        })
    }
    fn cache_observation(&self) -> Option<Snapshot> {
        Some(snapshot())
    }
}

#[test]
fn output_failure_terminally_persists_typed_turn_error_without_request_replay() {
    use crate::session::{Lifecycle, SessionId, SessionStore};
    let work = tempfile::tempdir().unwrap();
    let output = Output::create(&work.path().join("cache.jsonl")).unwrap();
    output.refuse_writes_for_test();
    let calls = std::rc::Rc::new(std::cell::Cell::new(0));
    let agent = crate::agent::CodingAgent::new_with_backend(
        Box::new(Completed(calls.clone())),
        work.path(),
        false,
    )
    .unwrap()
    .with_cache_output(Some(output));
    let store = SessionStore::load_at(&SessionId::parse("failure").unwrap(), work.path()).unwrap();
    let request = store
        .start_request(None, None, "hello", work.path())
        .unwrap();
    let error = agent.run(&store, &request).unwrap_err();
    assert_eq!(error.code, crate::envelope::Code::Turn);
    assert_eq!(error.key, "cache-output-write");
    assert_eq!(calls.get(), 1);
    let state = store.snapshot().unwrap();
    assert_eq!(state.branches[0].lifecycle, Lifecycle::Failed);
    assert!(state.branches[0].rounds.is_empty());
}
