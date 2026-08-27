use super::*;

struct FixedClock(i64);

impl Clock for FixedClock {
    fn unix_seconds(&self) -> Result<i64, CredentialError> {
        Ok(self.0)
    }
}

struct InMemorySource(Vec<u8>);

impl CredentialSource for InMemorySource {
    fn load(&self, clock: &dyn Clock) -> Result<CodexCredential, CredentialError> {
        parse_credential(&self.0, clock)
    }
}

fn valid_value() -> Value {
    serde_json::json!({
        "access_token": "token-value",
        "account_id": "account-value",
        "expiry": 1_031,
        "token_type": "Bearer"
    })
}

fn parse_value(value: &Value) -> Result<CodexCredential, CredentialError> {
    parse_credential(&serde_json::to_vec(value).unwrap(), &FixedClock(1_000))
}

fn assert_fixed_error(result: Result<CodexCredential, CredentialError>) {
    let error = result.unwrap_err();
    assert_eq!(error, CredentialError::remediation());
    assert_eq!(error.to_string(), CREDENTIAL_REMEDIATION);
    assert!(error.to_string().len() < 160);
}

#[test]
fn parses_exact_required_shape_and_redacts_debug() {
    for token_type in ["Bearer", "bearer"] {
        let mut value = valid_value();
        value["token_type"] = Value::String(token_type.to_string());
        let credential = parse_value(&value).unwrap();
        assert_eq!(credential.access_token(), "token-value");
        assert_eq!(credential.account_id(), "account-value");
        assert_eq!(credential.expiry(), 1_031);
        assert_eq!(credential.secret_values(), ["token-value", "account-value"]);
        let debug = format!("{credential:?}");
        assert!(!debug.contains("token-value"));
        assert!(!debug.contains("account-value"));
        assert!(debug.contains("[REDACTED]"));
    }
}

#[test]
fn in_memory_source_uses_injected_clock_and_exact_parser() {
    let source = InMemorySource(serde_json::to_vec(&valid_value()).unwrap());
    assert!(source.load(&FixedClock(1_000)).is_ok());
    assert_fixed_error(source.load(&FixedClock(1_001)));
}

#[test]
fn total_encoded_size_accepts_exact_cap_and_rejects_plus_one() {
    let mut bytes = serde_json::to_vec(&valid_value()).unwrap();
    let closing = bytes.pop().unwrap();
    bytes.resize(MAX_CREDENTIAL_BYTES - 1, b' ');
    bytes.push(closing);
    assert_eq!(bytes.len(), MAX_CREDENTIAL_BYTES);
    assert!(parse_credential(&bytes, &FixedClock(1_000)).is_ok());
    bytes.insert(bytes.len() - 1, b' ');
    assert_fixed_error(parse_credential(&bytes, &FixedClock(1_000)));
}

#[test]
fn field_count_accepts_32_and_rejects_33() {
    let mut object = valid_value().as_object().unwrap().clone();
    for index in 0..28 {
        object.insert(format!("unknown_{index}"), Value::Bool(true));
    }
    assert!(parse_value(&Value::Object(object.clone())).is_ok());
    object.insert("unknown_28".to_string(), Value::Bool(true));
    assert_fixed_error(parse_value(&Value::Object(object)));
}

#[test]
fn required_fields_and_types_are_enforced() {
    for key in ["access_token", "account_id", "expiry", "token_type"] {
        let mut value = valid_value();
        value.as_object_mut().unwrap().remove(key);
        assert_fixed_error(parse_value(&value));
    }
    for (key, invalid) in [
        ("access_token", serde_json::json!(1)),
        ("account_id", serde_json::json!(false)),
        ("expiry", serde_json::json!("1031")),
        ("token_type", serde_json::json!(null)),
    ] {
        let mut value = valid_value();
        value[key] = invalid;
        assert_fixed_error(parse_value(&value));
    }
}

#[test]
fn required_string_byte_bounds_are_exact() {
    for (key, max) in [
        ("access_token", MAX_ACCESS_TOKEN_BYTES),
        ("account_id", MAX_ACCOUNT_ID_BYTES),
    ] {
        let mut exact = valid_value();
        exact[key] = Value::String("x".repeat(max));
        assert!(parse_value(&exact).is_ok());
        let mut over = valid_value();
        over[key] = Value::String("x".repeat(max + 1));
        assert_fixed_error(parse_value(&over));
        let mut empty = valid_value();
        empty[key] = Value::String(String::new());
        assert_fixed_error(parse_value(&empty));
    }
}

#[test]
fn header_validation_rejects_controls_and_del() {
    for invalid in ['\0', '\n', '\r', '\u{1f}', '\u{7f}'] {
        for key in ["access_token", "account_id"] {
            let mut value = valid_value();
            value[key] = Value::String(format!("prefix{invalid}suffix"));
            assert_fixed_error(parse_value(&value));
        }
    }
    let mut unicode = valid_value();
    unicode["access_token"] = Value::String("tökén".to_string());
    unicode["account_id"] = Value::String("账户".to_string());
    assert!(parse_value(&unicode).is_ok());
}

#[test]
fn expiry_requires_checked_strict_skew() {
    for expiry in [1_000, 1_030] {
        let mut value = valid_value();
        value["expiry"] = serde_json::json!(expiry);
        assert_fixed_error(parse_value(&value));
    }
    let mut valid = valid_value();
    valid["expiry"] = serde_json::json!(1_031);
    assert!(parse_value(&valid).is_ok());
    assert_fixed_error(parse_credential(
        &serde_json::to_vec(&valid).unwrap(),
        &FixedClock(i64::MAX),
    ));
}

#[test]
fn expiry_number_must_be_finite_integral_and_in_range() {
    let mut integral_float = valid_value();
    integral_float["expiry"] = serde_json::from_str("1031.0").unwrap();
    assert!(parse_value(&integral_float).is_ok());
    for raw in ["1031.5", "9223372036854775808", "1e400"] {
        let document = format!(
            "{{\"access_token\":\"t\",\"account_id\":\"a\",\"expiry\":{raw},\"token_type\":\"Bearer\"}}"
        );
        assert_fixed_error(parse_credential(document.as_bytes(), &FixedClock(1_000)));
    }
}

#[test]
fn token_type_is_exact() {
    for invalid in ["BEARER", "bEaReR", " Bearer", "Bearer ", ""] {
        let mut value = valid_value();
        value["token_type"] = Value::String(invalid.to_string());
        assert_fixed_error(parse_value(&value));
    }
}

#[test]
fn known_optional_fields_follow_exact_scalar_rules_and_are_dropped() {
    let mut value = valid_value();
    value["refresh_token"] = Value::String("r".repeat(MAX_OPTIONAL_TOKEN_BYTES));
    value["id_token"] = Value::String("i".repeat(MAX_OPTIONAL_TOKEN_BYTES));
    value["scope"] = Value::Null;
    value["resource_url"] = Value::String("u".repeat(MAX_SCOPE_OR_RESOURCE_BYTES));
    let credential = parse_value(&value).unwrap();
    assert_eq!(credential.secret_values(), ["token-value", "account-value"]);

    for (key, invalid) in [
        ("refresh_token", serde_json::json!(null)),
        ("id_token", serde_json::json!(false)),
        ("scope", serde_json::json!(1)),
        ("resource_url", serde_json::json!(null)),
    ] {
        let mut invalid_value = valid_value();
        invalid_value[key] = invalid;
        assert_fixed_error(parse_value(&invalid_value));
    }
}

#[test]
fn optional_string_caps_are_exact() {
    for (key, max) in [
        ("refresh_token", MAX_OPTIONAL_TOKEN_BYTES),
        ("id_token", MAX_OPTIONAL_TOKEN_BYTES),
        ("scope", MAX_SCOPE_OR_RESOURCE_BYTES),
        ("resource_url", MAX_SCOPE_OR_RESOURCE_BYTES),
    ] {
        let mut exact = valid_value();
        exact[key] = Value::String("x".repeat(max));
        assert!(parse_value(&exact).is_ok());
        let mut over = valid_value();
        over[key] = Value::String("x".repeat(max + 1));
        assert_fixed_error(parse_value(&over));
    }
}

#[test]
fn unknown_scalars_are_accepted_and_containers_rejected() {
    for scalar in [
        Value::Null,
        Value::Bool(true),
        serde_json::json!(1.5),
        Value::String("x".repeat(MAX_UNKNOWN_STRING_BYTES)),
    ] {
        let mut value = valid_value();
        value["future"] = scalar;
        assert!(parse_value(&value).is_ok());
    }
    for invalid in [serde_json::json!([]), serde_json::json!({})] {
        let mut value = valid_value();
        value["future"] = invalid;
        assert_fixed_error(parse_value(&value));
    }
    let mut over = valid_value();
    over["future"] = Value::String("x".repeat(MAX_UNKNOWN_STRING_BYTES + 1));
    assert_fixed_error(parse_value(&over));
}

#[test]
fn malformed_top_level_duplicate_and_trailing_data_share_fixed_error() {
    for bytes in [
        b"[]".as_slice(),
        b"{".as_slice(),
        b"{\"access_token\":\"a\",\"access_token\":\"b\"}".as_slice(),
        b"{} true".as_slice(),
    ] {
        assert_fixed_error(parse_credential(bytes, &FixedClock(1_000)));
    }
}

#[test]
fn private_wrapper_debug_is_always_redacted() {
    let access = AccessToken("do-not-render-token".to_string());
    let account = AccountId("do-not-render-account".to_string());
    assert_eq!(format!("{access:?}"), "AccessToken([REDACTED])");
    assert_eq!(format!("{account:?}"), "AccountId([REDACTED])");
}

#[test]
fn invalid_utf8_maps_to_the_fixed_diagnostic() {
    assert_fixed_error(parse_credential(&[0xff], &FixedClock(1_000)));
}

#[cfg(not(target_os = "macos"))]
#[test]
fn unsupported_source_returns_only_fixed_platform_diagnostic() {
    let error = UnsupportedCredentialSource
        .load(&FixedClock(1_000))
        .unwrap_err();
    assert_eq!(error.to_string(), UNSUPPORTED_PLATFORM);
    assert!(error.to_string().len() < 100);
}
