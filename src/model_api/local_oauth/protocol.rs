use super::super::credentials::{parse_credential, Clock, CredentialError};
use super::store::MAX_BYTES;
use base64::Engine as _;
use serde_json::{json, Value};
use std::time::{Duration, Instant};

const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const ISSUER: &str = "https://auth.openai.com";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const LOGIN_TIMEOUT: Duration = Duration::from_secs(900);

pub(super) struct Protocol {
    client: reqwest::Client,
    issuer: String,
}

impl Protocol {
    pub(super) fn production() -> Result<Self, CredentialError> {
        Self::new(ISSUER)
    }

    fn new(issuer: &str) -> Result<Self, CredentialError> {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| failure("cannot construct OAuth HTTP client"))?;
        Ok(Self {
            client,
            issuer: issuer.to_owned(),
        })
    }

    pub(super) async fn login(&self, clock: &dyn Clock) -> Result<Vec<u8>, CredentialError> {
        let response = self
            .client
            .post(format!("{}/api/accounts/deviceauth/usercode", self.issuer))
            .json(&json!({"client_id": CLIENT_ID}));
        let (status, value) = consume(response).await?;
        require_success(status)?;
        let id = required_string(&value, "device_auth_id")?;
        let code = required_string(&value, "user_code")?;
        if code.len() > 32 || !code.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-') {
            return Err(failure("invalid device user code"));
        }
        let interval = polling_interval(&value)?;
        eprintln!("Open https://auth.openai.com/codex/device and enter code {code}. Authorization can wait up to 15 minutes.");
        let deadline = Instant::now() + LOGIN_TIMEOUT;
        loop {
            if Instant::now() >= deadline {
                return Err(failure("device authorization timed out; start login again"));
            }
            let poll = self
                .client
                .post(format!("{}/api/accounts/deviceauth/token", self.issuer))
                .json(&json!({"device_auth_id": id, "user_code": code}));
            let (status, value) = consume(poll).await?;
            if status == 403 || status == 404 {
                tokio::time::sleep(
                    interval.min(deadline.saturating_duration_since(Instant::now())),
                )
                .await;
                continue;
            }
            require_success(status)?;
            let response = self
                .client
                .post(format!("{}/oauth/token", self.issuer))
                .form(&[
                    ("grant_type", "authorization_code"),
                    ("code", required_string(&value, "authorization_code")?),
                    ("code_verifier", required_string(&value, "code_verifier")?),
                    (
                        "redirect_uri",
                        "https://auth.openai.com/deviceauth/callback",
                    ),
                    ("client_id", CLIENT_ID),
                ]);
            let (status, value) = consume(response).await?;
            require_success(status)?;
            return token_document(value, None, clock);
        }
    }

    pub(super) async fn refresh(
        &self,
        previous: &Value,
        clock: &dyn Clock,
    ) -> Result<Vec<u8>, CredentialError> {
        let refresh = required_string(previous, "refresh_token")?;
        let response = self
            .client
            .post(format!("{}/oauth/token", self.issuer))
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh),
                ("client_id", CLIENT_ID),
            ]);
        let (status, value) = consume(response).await?;
        require_success(status)?;
        token_document(value, Some(previous), clock)
    }
}

async fn consume(request: reqwest::RequestBuilder) -> Result<(u16, Value), CredentialError> {
    let mut response = request
        .send()
        .await
        .map_err(|_| failure("OAuth request failed"))?;
    let status = response.status().as_u16();
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| failure("OAuth response read failed"))?
    {
        if chunk.len() > MAX_BYTES.saturating_sub(bytes.len()) {
            return Err(failure("OAuth response exceeds byte limit"));
        }
        bytes.extend_from_slice(&chunk);
    }
    // Provider error bodies can contain tokens. Surface only status, never their contents.
    let value = if (200..300).contains(&status) {
        serde_json::from_slice(&bytes).map_err(|_| failure("invalid OAuth response JSON"))?
    } else {
        Value::Null
    };
    Ok((status, value))
}

fn require_success(status: u16) -> Result<(), CredentialError> {
    if (200..300).contains(&status) {
        Ok(())
    } else {
        Err(CredentialError::local(&format!(
            "OAuth endpoint returned HTTP {status}; login may need to be renewed"
        )))
    }
}

pub(super) fn required_string<'a>(
    value: &'a Value,
    name: &str,
) -> Result<&'a str, CredentialError> {
    value
        .get(name)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.len() <= 16_384)
        .ok_or_else(|| failure("required OAuth field is missing or invalid"))
}

fn polling_interval(value: &Value) -> Result<Duration, CredentialError> {
    let seconds = match value.get("interval") {
        None => 5,
        Some(Value::String(s)) => s
            .trim()
            .parse()
            .map_err(|_| failure("invalid device polling interval"))?,
        Some(v) => v
            .as_u64()
            .ok_or_else(|| failure("invalid device polling interval"))?,
    };
    if !(1..=60).contains(&seconds) {
        return Err(failure("invalid device polling interval"));
    }
    Ok(Duration::from_secs(seconds))
}

fn token_document(
    response: Value,
    previous: Option<&Value>,
    clock: &dyn Clock,
) -> Result<Vec<u8>, CredentialError> {
    let seconds = response
        .get("expires_in")
        .and_then(Value::as_i64)
        .filter(|n| *n > 30)
        .ok_or_else(|| failure("invalid token lifetime"))?;
    let expiry = clock
        .unix_seconds()?
        .checked_add(seconds)
        .ok_or_else(|| failure("token lifetime overflow"))?;
    let account = match response.get("id_token") {
        Some(value) => account_id(value.as_str().ok_or_else(|| failure("invalid ID token"))?)?,
        None => required_string(
            previous.ok_or_else(|| failure("ID token missing"))?,
            "account_id",
        )?
        .to_owned(),
    };
    let mut token = json!({
        "access_token": required_string(&response, "access_token")?,
        "token_type": required_string(&response, "token_type")?,
        "expiry": expiry, "account_id": account,
    });
    for key in ["refresh_token", "id_token"] {
        if let Some(value) = response
            .get(key)
            .or_else(|| previous.and_then(|v| v.get(key)))
        {
            token[key] = value.clone();
        }
    }
    let bytes = serde_json::to_vec(&token).map_err(|_| failure("cannot encode OAuth token"))?;
    parse_credential(&bytes, clock).map_err(|_| failure("invalid OAuth token"))?;
    Ok(bytes)
}

fn account_id(token: &str) -> Result<String, CredentialError> {
    if token.len() > 16_384 {
        return Err(failure("ID token exceeds byte limit"));
    }
    let parts: Vec<_> = token.split('.').collect();
    if parts.len() != 3 {
        return Err(failure("invalid ID token"));
    }
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[1])
        .map_err(|_| failure("invalid ID token encoding"))?;
    let claims: Value =
        serde_json::from_slice(&bytes).map_err(|_| failure("invalid ID token claims"))?;
    let account = [
        claims.pointer("/https:~1~1api.openai.com~1auth/chatgpt_account_id"),
        claims.pointer("/https:~1~1api.openai.com~1auth/account_id"),
        claims.get("account_id"),
    ]
    .into_iter()
    .flatten()
    .filter_map(Value::as_str)
    .find(|s| !s.is_empty())
    .map(str::to_owned)
    .ok_or_else(|| failure("ID token does not identify an account"));
    account
}

fn failure(message: &str) -> CredentialError {
    CredentialError::local(message)
}

#[cfg(test)]
mod tests;
