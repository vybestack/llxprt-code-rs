use crate::test_loopback as loopback;
fn scripted_login(responses: Vec<(u16, Value)>) -> (Protocol, loopback::Server<Vec<String>>) {
    scripted_http(
        responses
            .into_iter()
            .map(|(status, value)| http_response(status, &value.to_string()))
            .collect(),
    )
}

fn http_response(status: u16, body: &str) -> String {
    format!("HTTP/1.1 {status} Response\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
}

fn scripted_http(responses: Vec<String>) -> (Protocol, loopback::Server<Vec<String>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let protocol = Protocol::new(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
    let handle = loopback::spawn(listener, move |listener| {
        let mut requests = Vec::new();
        for response in responses {
            let (mut stream, _) = listener.accept().unwrap();
            let mut bytes = Vec::new();
            loop {
                let mut buffer = [0u8; 1024];
                let count = stream.read(&mut buffer).unwrap();
                assert!(count > 0);
                bytes.extend_from_slice(&buffer[..count]);
                if let Some(index) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    let header = String::from_utf8_lossy(&bytes[..index]);
                    let length: usize = header
                        .lines()
                        .find_map(|line| {
                            line.to_lowercase()
                                .strip_prefix("content-length: ")
                                .map(str::to_owned)
                        })
                        .unwrap()
                        .parse()
                        .unwrap();
                    if bytes.len() >= index + 4 + length {
                        break;
                    }
                }
            }
            requests.push(String::from_utf8(bytes).unwrap());
            stream.write_all(response.as_bytes()).unwrap();
        }
        requests
    });
    (protocol, handle)
}

fn device_response() -> Value {
    json!({"device_auth_id":"same-device", "user_code":"SAME-CODE", "interval":"1"})
}

fn code_response() -> Value {
    json!({"authorization_code":"auth-code", "code_verifier":"verifier"})
}

#[test]
fn device_pending_responses_preserve_code_until_exchange() {
    let mut value = response();
    value.as_object_mut().unwrap().remove("expires_in");
    value["access_token"] = json!(format!(
        "a.{}.b",
        URL_SAFE_NO_PAD.encode(br#"{"exp":5000}"#)
    ));
    let (protocol, server) = scripted_login(vec![
        (200, device_response()),
        (403, json!({})),
        (404, json!({})),
        (200, code_response()),
        (200, value),
    ]);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let bytes = runtime.block_on(protocol.login(&FixedClock)).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&bytes).unwrap()["expiry"],
        5_000
    );
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 5);
    for request in &requests[1..4] {
        assert!(request.starts_with("POST /api/accounts/deviceauth/token "));
        let body = request.split_once("\r\n\r\n").unwrap().1;
        assert_eq!(
            serde_json::from_str::<Value>(body).unwrap(),
            json!({"device_auth_id":"same-device", "user_code":"SAME-CODE"})
        );
    }
    assert!(requests[4].starts_with("POST /oauth/token "));
}

#[test]
fn login_failure_identifies_stage_without_replaying_exchange() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let secret = json!({"error":"provider-secret-marker"});
    for (responses, stage) in [
        (vec![(503, secret.clone())], "device-code request"),
        (
            vec![(200, device_response()), (503, secret.clone())],
            "device authorization polling",
        ),
        (
            vec![
                (200, device_response()),
                (200, code_response()),
                (503, secret.clone()),
            ],
            "authorization-code exchange",
        ),
    ] {
        let count = responses.len();
        let (protocol, server) = scripted_login(responses);
        let error = runtime
            .block_on(protocol.login(&FixedClock))
            .unwrap_err()
            .to_string();
        assert_eq!(error, format!("local OAuth: {stage} returned HTTP 503"));
        assert!(!error.contains("provider-secret-marker"));
        assert_eq!(server.join().unwrap().len(), count);
    }
}

#[test]
fn oauth_exchange_without_expires_in_uses_access_token_expiry() {
    let mut value = response();
    value.as_object_mut().unwrap().remove("expires_in");
    value["access_token"] = json!(format!(
        "a.{}.b",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&json!({"exp": 5_000})).unwrap())
    ));
    let bytes = token_document(value.clone(), None, &FixedClock).unwrap();
    let token: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(token["expiry"], 5_000);
    assert_eq!(
        parse_credential(&bytes, &FixedClock).unwrap().account_id(),
        "account"
    );
    let mut refreshed = value.clone();
    refreshed.as_object_mut().unwrap().remove("id_token");
    refreshed.as_object_mut().unwrap().remove("refresh_token");
    let bytes = token_document(refreshed, Some(&token), &FixedClock).unwrap();
    let updated: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(updated["expiry"], 5_000);
    assert_eq!(updated["refresh_token"], "refresh");
    for exp in [
        json!(1_030),
        json!(0),
        json!("5_000"),
        json!(null),
        json!(5_000.5),
    ] {
        value["access_token"] = json!(format!(
            "a.{}.b",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&json!({"exp": exp})).unwrap())
        ));
        assert!(token_document(value.clone(), None, &FixedClock).is_err());
    }
    value["access_token"] = json!("secret-without-expiry");
    let error = token_document(value, None, &FixedClock)
        .unwrap_err()
        .to_string();
    assert!(!error.contains("secret-without-expiry"));
}

#[test]
fn http_errors_identify_only_stage_and_status() {
    for stage in [
        "device-code request",
        "device authorization polling",
        "authorization-code exchange",
        "token refresh",
    ] {
        let error = require_success(503, stage).unwrap_err().to_string();
        assert_eq!(error, format!("local OAuth: {stage} returned HTTP 503"));
        assert!(require_success(200, stage).is_ok());
    }
}

#[test]
fn loopback_device_login_acquires_and_validates_fresh_token() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let protocol = Protocol::new(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
    let server = loopback::spawn(listener, move |listener| {
        for (route, body, expected) in [
            ("/api/accounts/deviceauth/usercode", json!({"device_auth_id":"device-id", "user_code":"TEST-CODE", "interval":"1"}).to_string(), CLIENT_ID),
            ("/api/accounts/deviceauth/token", json!({"authorization_code":"auth-code", "code_verifier":"verifier", "code_challenge":"challenge"}).to_string(), "device-id"),
            ("/oauth/token", response().to_string(), "grant_type=authorization_code"),
        ] {
            let (mut stream, _) = listener.accept().unwrap();
            let mut bytes = Vec::new();
            loop {
                let mut buffer = [0u8; 1024];
                let count = stream.read(&mut buffer).unwrap();
                assert!(count > 0);
                bytes.extend_from_slice(&buffer[..count]);
                if let Some(index) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    let header = String::from_utf8_lossy(&bytes[..index]);
                    let length: usize = header.lines().find_map(|line| line.to_lowercase().strip_prefix("content-length: ").map(str::to_owned)).unwrap().parse().unwrap();
                    if bytes.len() >= index + 4 + length { break; }
                }
            }
            let request = String::from_utf8(bytes).unwrap();
            assert!(request.starts_with(&format!("POST {route} ")));
            assert!(request.contains(expected));
            if route == "/oauth/token" {
                assert!(request.contains("code_verifier=verifier"));
                assert!(request.contains("redirect_uri=https%3A%2F%2Fauth.openai.com%2Fdeviceauth%2Fcallback"));
            }
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        }
    });
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let token = runtime.block_on(protocol.login(&FixedClock)).unwrap();
    assert_eq!(
        parse_credential(&token, &FixedClock).unwrap().account_id(),
        "account"
    );
    server.join().unwrap();
}

use super::*;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use std::io::{Read, Write};

struct FixedClock;
impl Clock for FixedClock {
    fn unix_seconds(&self) -> Result<i64, CredentialError> {
        Ok(1_000)
    }
}

fn response() -> Value {
    let payload = URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(
            &json!({"https://api.openai.com/auth": {"chatgpt_account_id": "account"}}),
        )
        .unwrap(),
    );
    json!({"access_token": "access", "refresh_token": "refresh", "id_token": format!("a.{payload}.b"),
        "token_type": "Bearer", "expires_in": 3600})
}

#[test]
fn token_mapping_preserves_rotated_refresh_and_id_account() {
    let bytes = token_document(response(), None, &FixedClock).unwrap();
    let stored: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(stored["expiry"], 4_600);
    assert_eq!(stored["account_id"], "account");
    assert_eq!(stored["refresh_token"], "refresh");
    let next = json!({"access_token": "next", "token_type": "bearer", "expires_in": 7200});
    let bytes = token_document(next, Some(&stored), &FixedClock).unwrap();
    let updated: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(updated["refresh_token"], "refresh");
    assert_eq!(updated["account_id"], "account");
    assert_eq!(updated["id_token"], stored["id_token"]);
}

#[test]
fn malformed_or_unsafe_provider_tokens_reject_without_contents() {
    for field in ["access_token", "expires_in", "token_type", "id_token"] {
        let mut value = response();
        value[field] = json!("secret-marker\n");
        let error = token_document(value, None, &FixedClock).unwrap_err();
        assert!(!error.to_string().contains("secret-marker"));
    }
    assert!(token_document(json!({}), None, &FixedClock).is_err());
    assert!(account_id("not.jwt").is_err());
    assert!(polling_interval(&json!({"interval": 0})).is_err());
    assert_eq!(
        polling_interval(&json!({"interval":"5"})).unwrap(),
        Duration::from_secs(5)
    );
}

fn server(
    status: u16,
    body: String,
    assert_request: impl Fn(&str) + Send + 'static,
) -> (Protocol, loopback::Server<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let protocol = Protocol::new(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
    let handle = loopback::spawn(listener, move |listener| {
        let (mut stream, _) = listener.accept().unwrap();
        let mut bytes = Vec::new();
        loop {
            let mut buffer = [0u8; 1024];
            let count = stream.read(&mut buffer).unwrap();
            assert!(count > 0);
            bytes.extend_from_slice(&buffer[..count]);
            if let Some(index) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                let header = String::from_utf8_lossy(&bytes[..index]);
                let length: usize = header
                    .lines()
                    .find_map(|line| {
                        line.to_lowercase()
                            .strip_prefix("content-length: ")
                            .map(str::to_owned)
                    })
                    .unwrap()
                    .parse()
                    .unwrap();
                if bytes.len() >= index + 4 + length {
                    break;
                }
            }
        }
        assert_request(&String::from_utf8(bytes).unwrap());
        write!(stream, "HTTP/1.1 {status} Response\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
    });
    (protocol, handle)
}

#[test]
fn loopback_refresh_wire_and_status_scrubbing() {
    let previous = json!({"refresh_token":"refresh", "account_id":"account"});
    let (protocol, server) = server(200, response().to_string(), |request| {
        assert!(request.starts_with("POST /oauth/token "));
        assert!(request.contains("grant_type=refresh_token"));
        assert!(request.contains("refresh_token=refresh"));
        assert!(request.contains(CLIENT_ID));
    });
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let token = runtime
        .block_on(protocol.refresh(&previous, &FixedClock))
        .unwrap();
    assert!(parse_credential(&token, &FixedClock).is_ok());
    server.join().unwrap();
    let (protocol, server) = self::server(401, "echoed-secret-token".to_owned(), |_| {});
    let error = runtime
        .block_on(protocol.refresh(&previous, &FixedClock))
        .unwrap_err()
        .to_string();
    server.join().unwrap();
    assert!(error.contains("401"));
    assert!(!error.contains("echoed-secret-token"));
}

#[test]
fn overbound_response_is_terminal() {
    let (protocol, server) = server(200, "x".repeat(MAX_BYTES + 1), |_| {});
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    assert!(runtime
        .block_on(protocol.refresh(&json!({"refresh_token":"r"}), &FixedClock))
        .unwrap_err()
        .to_string()
        .contains("byte limit"));
    server.join().unwrap();
}

#[test]
fn response_failures_identify_every_stage_without_disclosing_body() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    for (failure_response, diagnostic) in [
        (http_response(200, "provider-secret-marker"), "invalid OAuth response JSON"),
        (http_response(503, &"x".repeat(MAX_BYTES + 1)), "OAuth response exceeds byte limit"),
        ("HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\nprovider-secret-marker".to_owned(), "OAuth response read failed"),
    ] {
        for (prefix, stage, refresh) in [
            (vec![], "device-code request", false),
            (vec![http_response(200, &device_response().to_string())], "device authorization polling", false),
            (vec![http_response(200, &device_response().to_string()), http_response(200, &code_response().to_string())], "authorization-code exchange", false),
            (vec![], "token refresh", true),
        ] {
            let mut responses = prefix;
            responses.push(failure_response.clone());
            let count = responses.len();
            let (protocol, server) = scripted_http(responses);
            let error = if refresh {
                runtime.block_on(protocol.refresh(&json!({"refresh_token":"refresh"}), &FixedClock))
            } else {
                runtime.block_on(protocol.login(&FixedClock))
            }.unwrap_err().to_string();
            assert_eq!(error, format!("local OAuth: {stage}: {diagnostic}"));
            assert!(!error.contains("provider-secret-marker"));
            assert_eq!(server.join().unwrap().len(), count);
        }
    }
}

#[test]
fn supplied_invalid_lifetime_never_uses_jwt_expiry() {
    for lifetime in [
        json!(null),
        json!("3600"),
        json!(3600.5),
        json!(-1),
        json!(0),
        json!(30),
        json!(i64::MAX),
    ] {
        let mut value = response();
        value["access_token"] = json!(format!(
            "a.{}.b",
            URL_SAFE_NO_PAD.encode(br#"{"exp":5000}"#)
        ));
        value["expires_in"] = lifetime;
        assert!(token_document(value, None, &FixedClock).is_err());
    }
}

#[test]
fn absent_lifetime_requires_valid_integral_future_access_expiry() {
    for claims in [
        json!({}),
        json!({"exp":null}),
        json!({"exp":"5000"}),
        json!({"exp":5000.5}),
        json!({"exp":1030}),
        json!({"exp":u64::MAX}),
    ] {
        let mut value = response();
        value.as_object_mut().unwrap().remove("expires_in");
        value["access_token"] = json!(format!(
            "a.{}.b",
            URL_SAFE_NO_PAD.encode(claims.to_string())
        ));
        assert!(token_document(value, None, &FixedClock).is_err());
    }
    for access in ["provider-secret-marker", "a.@@.b", "a.eA.b"] {
        let mut value = response();
        value.as_object_mut().unwrap().remove("expires_in");
        value["access_token"] = json!(access);
        let error = token_document(value, None, &FixedClock)
            .unwrap_err()
            .to_string();
        assert!(!error.contains(access));
    }
}
