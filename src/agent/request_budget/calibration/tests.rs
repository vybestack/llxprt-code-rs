use super::*;
use serde_json::json;

// Test-only tokenizer/data contract, not a model tokenizer. Unicode scalars are tokens;
// the declared ASCII sequence `fn` is one token in the second pinned revision.
struct FixtureTokenizer(bool);
impl Tokenizer for FixtureTokenizer {
    fn identity(&self) -> (&str, &str) {
        (
            "fixture-unicode",
            if self.0 { "fn-merge" } else { "scalar" },
        )
    }
    fn count(&self, wire: &[u8]) -> Result<u64, CalibrationError> {
        let text = std::str::from_utf8(wire).map_err(|_| CalibrationError::Projection)?;
        Ok(text.chars().count() as u64
            - if self.0 {
                text.matches("fn").count() as u64
            } else {
                0
            })
    }
}
fn contract(tokenizer: Option<&dyn Tokenizer>) -> RequestContract {
    RequestContract {
        identity: "a".repeat(64),
        serializer_version: 1,
        render_version: 1,
        tokenizer: tokenizer.map(|t| t.identity().0.into()),
        tokenizer_version: tokenizer.map(|t| t.identity().1.into()),
        input_accounting: "inclusive".into(),
        margin_version: 1,
    }
}
fn wire(prompt: &str) -> Vec<u8> {
    serde_json::to_vec(
        &json!({"model":"fixture", "messages":[{"role":"user","content":prompt}],
        "tools":[],"max_tokens":100}),
    )
    .unwrap()
}
#[test]
fn similar_bytes_have_tokenizer_specific_counts() {
    let tokenizer = FixtureTokenizer(false);
    let measure = |text| {
        measure(
            contract(Some(&tokenizer)),
            &wire(text),
            Some(&tokenizer),
            None,
            &[],
        )
        .unwrap()
    };
    let english = measure("abcabc");
    let multilingual = measure("中文");
    assert_eq!(english.wire_bytes, multilingual.wire_bytes);
    assert_eq!(
        english.estimated_input.unwrap() - multilingual.estimated_input.unwrap(),
        4
    );
    let merge = FixtureTokenizer(true);
    let code = wire("fn fn");
    assert_eq!(
        tokenizer.count(&code).unwrap() - merge.count(&code).unwrap(),
        2
    );
    assert!(matches!(
        super::measure(contract(Some(&tokenizer)), &code, Some(&merge), None, &[]),
        Err(CalibrationError::TokenizerMismatch)
    ));
}
#[test]
fn system_tool_reserve_terms_change_independently_and_digest_is_exact() {
    let tokenizer = FixtureTokenizer(false);
    let value = json!({"messages":[{"role":"system","content":"head"},{"role":"user","content":"tail"}],
        "tools":[{"name":"read","parameters":{"type":"object"}}],"max_tokens":100});
    let measure = |v: &serde_json::Value| {
        super::measure(
            contract(Some(&tokenizer)),
            &serde_json::to_vec(v).unwrap(),
            Some(&tokenizer),
            Some(8000),
            &[],
        )
        .unwrap()
    };
    let base = measure(&value);
    let mut system = value.clone();
    system["messages"][0]["content"] = json!("longer head");
    let mut tools = value.clone();
    tools["tools"][0]["parameters"]["properties"] = json!({"p":{"type":"string"}});
    let mut reserve = value.clone();
    reserve["max_tokens"] = json!(200);
    assert!(measure(&system).terms.system > base.terms.system);
    assert_eq!(measure(&system).terms.tools, base.terms.tools);
    assert!(measure(&tools).terms.tools > base.terms.tools);
    assert_eq!(measure(&tools).terms.body, base.terms.body);
    assert_eq!(measure(&reserve).terms.output_reserve, Some(200));
    assert_eq!(
        base.wire_digest,
        format!("{:x}", Sha256::digest(serde_json::to_vec(&value).unwrap()))
    );
    assert_ne!(
        base.host_memory_cap_bytes,
        base.terms.output_reserve.unwrap()
    );
}

// Corpus pinned in source before execution: calibration inputs + observed scalar-token
// residuals, held-out inputs + residuals. Provider observations here are synthetic only.
const TRAIN: [(&str, u64); 3] = [("English", 3), ("中文 العربية", 7), ("fn main() {}", 5)];
const HELD_OUT: [(&str, u64); 3] = [
    ("日本語", 6),
    ("fn test(x: &str) {}", 7),
    (r#"{"type":"object","properties":{}}"#, 4),
];
#[test]
fn pinned_corpus_zero_underbounds_restart_and_typed_failure() {
    let tokenizer = FixtureTokenizer(true);
    let contract = contract(Some(&tokenizer));
    let train: Vec<_> = TRAIN
        .iter()
        .map(|(input, residual)| {
            let mut bound =
                measure(contract.clone(), &wire(input), Some(&tokenizer), None, &[]).unwrap();
            bound.observed_input = Some(bound.estimated_input.unwrap() + residual);
            bound
        })
        .collect();
    let restored: Vec<RequestBound> =
        serde_json::from_slice(&serde_json::to_vec(&train).unwrap()).unwrap();
    for (input, residual) in HELD_OUT {
        let mut bound = measure(
            contract.clone(),
            &wire(input),
            Some(&tokenizer),
            None,
            &train,
        )
        .unwrap();
        assert_eq!(
            bound,
            measure(
                contract.clone(),
                &wire(input),
                Some(&tokenizer),
                None,
                &restored
            )
            .unwrap()
        );
        bound.observed_input = Some(bound.estimated_input.unwrap() + residual);
        eprintln!(
            "held-out input={input:?} estimate={:?} observed={:?} margin={:?} bounded={:?}",
            bound.estimated_input, bound.observed_input, bound.margin, bound.bounded_input
        );
        assert_eq!(bound.margin, Some(7));
        assert_eq!(check_held_out(&bound), Ok(()));
        bound.observed_input = bound.bounded_input.map(|n| n + 1);
        assert_eq!(check_held_out(&bound), Err(CalibrationError::Underbound));
    }
}
#[test]
fn exact_contract_changes_invalidate_calibration_and_absence_is_visible() {
    let mut bound = measure(contract(None), &wire("unknown model"), None, None, &[]).unwrap();
    assert_eq!(bound.capability, "tokenizer_unavailable_byte_heuristic");
    assert_eq!(bound.margin, None);
    assert_eq!(bound.observed_input, None);
    assert_eq!(check_held_out(&bound), Err(CalibrationError::Unavailable));
    bound.observed_input = Some(10000);
    let base = bound.contract.clone();
    for change in 0..6 {
        let mut changed = base.clone();
        match change {
            0 => changed.identity = "b".repeat(64),
            1 => changed.serializer_version += 1,
            2 => changed.render_version += 1,
            3 => changed.tokenizer = Some("other".into()),
            4 => changed.input_accounting = "anthropic".into(),
            _ => changed.margin_version += 1,
        }
        assert_eq!(margin(&changed, &[bound.clone()]), None);
    }
    assert!(margin(&base, &[bound]).unwrap() > 0);
}

#[test]
fn wire_serializer_settings_and_tools_invalidate_margin() {
    let wire = wire("first");
    let mut bound = measure(contract(None), &wire, None, None, &[]).unwrap();
    bound.observed_input = Some(10000);
    let mut changed: serde_json::Value = serde_json::from_slice(&wire).unwrap();
    changed["tools"] = json!([{"name":"new tool"}]);
    let changed = measure(
        contract(None),
        &serde_json::to_vec(&changed).unwrap(),
        None,
        None,
        &[bound.clone()],
    )
    .unwrap();
    assert_eq!(changed.margin, None);
    assert_ne!(changed.contract, bound.contract);
}

#[test]
fn external_u64_usage_overflow_is_explicit_unavailable_not_zero_or_wrapped() {
    let mut train = measure(contract(None), &wire("first"), None, None, &[]).unwrap();
    train.observed_input = Some(u64::MAX);
    let next = measure(
        contract(None),
        &wire("longer second request"),
        None,
        None,
        &[train],
    )
    .unwrap();
    assert_eq!(next.capability, "calibration_overflow_unavailable");
    assert!(next.margin.is_some());
    assert_eq!(next.bounded_input, None);
    assert_eq!(check_held_out(&next), Err(CalibrationError::Unavailable));
}
