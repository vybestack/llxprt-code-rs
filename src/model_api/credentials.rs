use std::collections::BTreeSet;
use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::de::{self, Deserialize, Deserializer, MapAccess, Visitor};
use serde_json::Value;

const MAX_CREDENTIAL_BYTES: usize = 65_536;
const MAX_CREDENTIAL_FIELDS: usize = 32;
const MAX_ACCESS_TOKEN_BYTES: usize = 4_096;
const MAX_ACCOUNT_ID_BYTES: usize = 256;
const MAX_OPTIONAL_TOKEN_BYTES: usize = 16_384;
const MAX_SCOPE_OR_RESOURCE_BYTES: usize = 2_048;
const MAX_UNKNOWN_STRING_BYTES: usize = 16_384;
pub(crate) const CREDENTIAL_EXPIRY_SKEW_SECONDS: i64 = 30;

const CREDENTIAL_REMEDIATION: &str =
    "Codex OAuth credential is unavailable or invalid; sign in again with LLxprt Code using the native macOS keychain";
#[cfg(not(target_os = "macos"))]
const UNSUPPORTED_PLATFORM: &str =
    "Codex OAuth credentials require the native macOS keychain on this platform";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CredentialError {
    diagnostic: &'static str,
}

impl CredentialError {
    pub(crate) const fn remediation() -> Self {
        Self {
            diagnostic: CREDENTIAL_REMEDIATION,
        }
    }

    #[cfg(not(target_os = "macos"))]
    const fn unsupported() -> Self {
        Self {
            diagnostic: UNSUPPORTED_PLATFORM,
        }
    }
}

impl fmt::Display for CredentialError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.diagnostic)
    }
}

impl std::error::Error for CredentialError {}

pub(crate) trait Clock: Send + Sync {
    fn unix_seconds(&self) -> Result<i64, CredentialError>;
}

pub(crate) struct SystemClock;

impl Clock for SystemClock {
    fn unix_seconds(&self) -> Result<i64, CredentialError> {
        let seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| CredentialError::remediation())?
            .as_secs();
        i64::try_from(seconds).map_err(|_| CredentialError::remediation())
    }
}

pub(crate) trait CredentialSource: Send + Sync {
    fn load(&self, clock: &dyn Clock) -> Result<CodexCredential, CredentialError>;
}

#[cfg(not(target_os = "macos"))]
pub(crate) struct UnsupportedCredentialSource;

#[cfg(not(target_os = "macos"))]
impl CredentialSource for UnsupportedCredentialSource {
    fn load(&self, _clock: &dyn Clock) -> Result<CodexCredential, CredentialError> {
        Err(CredentialError::unsupported())
    }
}

#[derive(Clone, PartialEq, Eq)]
struct AccessToken(String);

impl fmt::Debug for AccessToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AccessToken([REDACTED])")
    }
}

#[derive(Clone, PartialEq, Eq)]
struct AccountId(String);

impl fmt::Debug for AccountId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AccountId([REDACTED])")
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct CodexCredential {
    access_token: AccessToken,
    account_id: AccountId,
    expiry: i64,
}

impl CodexCredential {
    pub(crate) fn access_token(&self) -> &str {
        &self.access_token.0
    }

    pub(crate) fn account_id(&self) -> &str {
        &self.account_id.0
    }

    pub(crate) fn expiry(&self) -> i64 {
        self.expiry
    }

    pub(crate) fn secret_values(&self) -> [&str; 2] {
        [self.access_token(), self.account_id()]
    }
}

impl fmt::Debug for CodexCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodexCredential")
            .field("access_token", &"[REDACTED]")
            .field("account_id", &"[REDACTED]")
            .field("expiry", &self.expiry)
            .finish()
    }
}

pub(super) fn parse_credential(
    bytes: &[u8],
    clock: &dyn Clock,
) -> Result<CodexCredential, CredentialError> {
    if bytes.len() > MAX_CREDENTIAL_BYTES {
        return Err(CredentialError::remediation());
    }
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let fields = CredentialFields::deserialize(&mut deserializer)
        .and_then(|fields| {
            deserializer.end()?;
            Ok(fields)
        })
        .map_err(|_| CredentialError::remediation())?;
    fields.into_credential(clock)
}

#[derive(Default)]
struct CredentialFields {
    access_token: Option<String>,
    account_id: Option<String>,
    expiry: Option<i64>,
    token_type_valid: Option<bool>,
}

impl CredentialFields {
    fn into_credential(self, clock: &dyn Clock) -> Result<CodexCredential, CredentialError> {
        let access_token = self.access_token.ok_or_else(CredentialError::remediation)?;
        let account_id = self.account_id.ok_or_else(CredentialError::remediation)?;
        let expiry = self.expiry.ok_or_else(CredentialError::remediation)?;
        if self.token_type_valid != Some(true) {
            return Err(CredentialError::remediation());
        }
        validate_required_string(&access_token, MAX_ACCESS_TOKEN_BYTES)?;
        validate_required_string(&account_id, MAX_ACCOUNT_ID_BYTES)?;
        validate_authorization_header(&access_token)?;
        validate_header_value(account_id.as_bytes())?;
        let minimum_expiry = clock
            .unix_seconds()?
            .checked_add(CREDENTIAL_EXPIRY_SKEW_SECONDS)
            .ok_or_else(CredentialError::remediation)?;
        if expiry <= minimum_expiry {
            return Err(CredentialError::remediation());
        }
        Ok(CodexCredential {
            access_token: AccessToken(access_token),
            account_id: AccountId(account_id),
            expiry,
        })
    }
}

impl<'de> Deserialize<'de> for CredentialFields {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(CredentialVisitor)
    }
}

struct CredentialVisitor;

impl<'de> Visitor<'de> for CredentialVisitor {
    type Value = CredentialFields;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a bounded credential object")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut fields = CredentialFields::default();
        let mut seen = BTreeSet::new();
        let mut count = 0usize;
        while let Some(key) = map.next_key::<String>()? {
            count = count
                .checked_add(1)
                .ok_or_else(|| de::Error::custom("credential field count overflow"))?;
            if count > MAX_CREDENTIAL_FIELDS || !seen.insert(key.clone()) {
                return Err(de::Error::custom("invalid credential object fields"));
            }
            let value = map.next_value::<Value>()?;
            apply_field(&mut fields, &key, value).map_err(de::Error::custom)?;
        }
        Ok(fields)
    }
}

fn apply_field(fields: &mut CredentialFields, key: &str, value: Value) -> Result<(), &'static str> {
    match key {
        "access_token" => fields.access_token = Some(require_string(value)?),
        "account_id" => fields.account_id = Some(require_string(value)?),
        "expiry" => fields.expiry = Some(require_integral_i64(value)?),
        "token_type" => {
            let value = require_string(value)?;
            fields.token_type_valid = Some(matches!(value.as_str(), "Bearer" | "bearer"));
        }
        "refresh_token" | "id_token" => {
            validate_optional_string(value, MAX_OPTIONAL_TOKEN_BYTES)?;
        }
        "scope" => validate_optional_scope(value)?,
        "resource_url" => validate_optional_string(value, MAX_SCOPE_OR_RESOURCE_BYTES)?,
        _ => validate_unknown_scalar(value)?,
    }
    Ok(())
}

fn require_string(value: Value) -> Result<String, &'static str> {
    value.as_str().map(str::to_owned).ok_or("expected string")
}

fn validate_optional_string(value: Value, max_bytes: usize) -> Result<(), &'static str> {
    let value = value.as_str().ok_or("expected optional string")?;
    if value.len() > max_bytes {
        return Err("optional string exceeds byte cap");
    }
    Ok(())
}

fn validate_optional_scope(value: Value) -> Result<(), &'static str> {
    if value.is_null() {
        return Ok(());
    }
    validate_optional_string(value, MAX_SCOPE_OR_RESOURCE_BYTES)
}

fn validate_unknown_scalar(value: Value) -> Result<(), &'static str> {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => Ok(()),
        Value::String(value) if value.len() <= MAX_UNKNOWN_STRING_BYTES => Ok(()),
        Value::String(_) => Err("unknown string exceeds byte cap"),
        Value::Array(_) | Value::Object(_) => Err("unknown container is unsupported"),
    }
}

fn require_integral_i64(value: Value) -> Result<i64, &'static str> {
    let number = value.as_number().ok_or("expected number")?;
    if let Some(value) = number.as_i64() {
        return Ok(value);
    }
    if let Some(value) = number.as_u64() {
        return i64::try_from(value).map_err(|_| "integer is outside i64 range");
    }
    let value = number.as_f64().ok_or("number is not finite")?;
    if !value.is_finite()
        || value.fract() != 0.0
        || value < i64::MIN as f64
        || value >= -(i64::MIN as f64)
    {
        return Err("number is not a finite integral i64");
    }
    Ok(value as i64)
}

fn validate_required_string(value: &str, max_bytes: usize) -> Result<(), CredentialError> {
    if value.is_empty() || value.len() > max_bytes {
        return Err(CredentialError::remediation());
    }
    Ok(())
}

fn validate_authorization_header(access_token: &str) -> Result<(), CredentialError> {
    let length = b"Bearer "
        .len()
        .checked_add(access_token.len())
        .ok_or_else(CredentialError::remediation)?;
    if length == 0
        || !b"Bearer "
            .iter()
            .copied()
            .chain(access_token.as_bytes().iter().copied())
            .all(is_header_value_byte)
    {
        return Err(CredentialError::remediation());
    }
    Ok(())
}

fn validate_header_value(bytes: &[u8]) -> Result<(), CredentialError> {
    if bytes.iter().copied().all(is_header_value_byte) {
        Ok(())
    } else {
        Err(CredentialError::remediation())
    }
}

fn is_header_value_byte(byte: u8) -> bool {
    byte >= 32 && byte != 127
}

#[cfg(test)]
mod tests;
