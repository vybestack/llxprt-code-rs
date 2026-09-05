//! Shared, typed stdout envelope and its published structural schema.
//!
//! The CLI producer and harness consumer use these same serde types. The schema describes
//! serialized structure only; invocation-dependent rules (exit agreement, identity, digests,
//! and budget relationships) remain harness checks.

use schemars::{generate::SchemaSettings, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Exit codes exposed to the OS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Code {
    Usage = 2,
    Config = 3,
    Session = 4,
    Model = 5,
    Turn = 6,
    Profiling = 7,
}

/// Immutable identifier for the initial stdout wire contract.
pub const SCHEMA_ID: &str =
    "https://github.com/vybestack/llxprt-code-rs/schemas/stdout-envelope-v1";

/// The exactly-one-object stdout shape, discriminated by `status`.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum Envelope {
    /// A completed turn.
    Ok(OkEnvelope),
    /// An invocation failure.
    Error(ErrorEnvelope),
}

/// The required success envelope.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OkEnvelope {
    /// Safe session component: ASCII letters, digits, underscore, or hyphen.
    #[schemars(regex(pattern = "^[A-Za-z0-9_-]{1,64}$"))]
    pub session_id: String,
    pub session_dir: String,
    pub turn: u64,
    /// Attempts are 1-based.
    #[schemars(range(min = 1))]
    pub attempt: u64,
    pub branch_id: String,
    pub branch: bool,
    pub replayed: bool,
    pub summary: String,
    pub tool_calls: u64,
    /// Declared tool-call budget; `-1` means unlimited.
    #[schemars(range(min = -1))]
    pub declared_tool_calls: i64,
    pub budget_exhausted: bool,
    pub prompt_digest: String,
    /// Terminal context-store outcome declared by the runtime, when one exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_outcome: Option<String>,
}

/// The required error envelope.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ErrorEnvelope {
    /// Safe session component: ASCII letters, digits, underscore, or hyphen.
    #[schemars(regex(pattern = "^[A-Za-z0-9_-]{1,64}$"))]
    pub session_id: String,
    pub error: EnvelopeError,
}

/// Error detail nested in an error envelope.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EnvelopeError {
    /// The stable machine token for the failure class: one of the `Code` family keys
    /// (`usage`, `model-config`, `session`, `model`, `turn`, `mem-profile`) or, for a
    /// model transport failure, the finer `model-<class>` transport key the agent
    /// carries on the error, such as `model-quota-exhausted` or
    /// `model-transient-server`. Free-form text never appears here.
    pub code: String,
    pub message: String,
    /// Profiling lifecycle stage; present only for `mem-profile` failures.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,
    /// Outcome of the agent/session independent of profile publication.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_status: Option<String>,
}

impl Envelope {
    /// Construct a typed error document with its final session identity. The `code` is
    /// the stable machine token for the failure class: normally the caller's key (one of
    /// `usage`, `model-config`, `session`, `model`, `turn`, `mem-profile`), and for a
    /// model transport failure the finer
    /// [`crate::agent::transport::TransportFailure::transport_key`] the agent carries on
    /// the error (`model-quota-exhausted`, `model-transient-server`, ...), so a driver
    /// reads the retryability class from `error.code` without matching on message text.
    pub fn error(
        session_id: impl Into<String>,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self::Error(ErrorEnvelope {
            session_id: session_id.into(),
            error: EnvelopeError {
                code: code.into(),
                message: message.into(),
                stage: None,
                session_status: None,
            },
        })
    }

    /// Construct the profiling failure extension of the versioned error envelope.
    pub fn profiling_error(
        session_id: impl Into<String>,
        message: impl Into<String>,
        stage: impl Into<String>,
        session_status: impl Into<String>,
    ) -> Self {
        Self::Error(ErrorEnvelope {
            session_id: session_id.into(),
            error: EnvelopeError {
                code: "mem-profile".into(),
                message: message.into(),
                stage: Some(stage.into()),
                session_status: Some(session_status.into()),
            },
        })
    }

    /// Serialize through `Value`, retaining the CLI's historical sorted-key wire bytes.
    /// If serialization ever becomes fallible, preserve the stdout contract with a minimal
    /// schema-valid error envelope.
    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or_else(|_| {
            let session_id = match self {
                Self::Ok(envelope) => &envelope.session_id,
                Self::Error(envelope) => &envelope.session_id,
            };
            let mut error = Map::new();
            error.insert("code".into(), Value::String("serialization".into()));
            error.insert(
                "message".into(),
                Value::String("envelope serialization failed".into()),
            );
            let mut fallback = Map::new();
            fallback.insert("error".into(), Value::Object(error));
            fallback.insert("session_id".into(), Value::String(session_id.clone()));
            fallback.insert("status".into(), Value::String("error".into()));
            Value::Object(fallback)
        })
    }

    /// Return the exact stdout bytes, including one terminal newline.
    pub fn to_line(&self) -> Vec<u8> {
        let mut bytes = self.to_value().to_string().into_bytes();
        bytes.push(b'\n');
        bytes
    }
}

/// Generate the deterministic Draft 2020-12 serialized-output schema artifact.
pub fn schema_bytes() -> Vec<u8> {
    let settings = SchemaSettings::draft2020_12().for_serialize();
    let schema = settings.into_generator().into_root_schema_for::<Envelope>();
    let mut value = serde_json::to_value(schema).expect("schema is JSON-serializable");
    let object = value.as_object_mut().expect("root schema is an object");
    object.insert("$id".into(), Value::String(SCHEMA_ID.into()));
    object.insert(
        "$comment".into(),
        Value::String("Generated by `cargo xtask envelope-schema`; do not edit by hand.".into()),
    );
    object.insert(
        "title".into(),
        Value::String("llxprt-code-rs stdout envelope v1".into()),
    );
    let variants = object
        .remove("oneOf")
        .expect("schemars 1.2.1 emits oneOf for the tagged Envelope enum");
    object.insert("anyOf".into(), variants);
    object.insert(
        "description".into(),
        Value::String(
            "Structural serialized-output contract. Process exit agreement and relational semantic rules are enforced separately by the harness."
                .into(),
        ),
    );
    sort_json(&mut value);
    let mut bytes = serde_json::to_string_pretty(&value)
        .expect("schema is JSON-serializable")
        .into_bytes();
    bytes.push(b'\n');
    bytes
}

/// Sort every object recursively rather than depending on a map implementation's order.
fn sort_json(value: &mut Value) {
    match value {
        Value::Array(items) => items.iter_mut().for_each(sort_json),
        Value::Object(object) => {
            let old = std::mem::take(object);
            let mut entries: Vec<_> = old.into_iter().collect();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            let mut sorted = Map::new();
            for (key, mut child) in entries {
                sort_json(&mut child);
                sorted.insert(key, child);
            }
            *object = sorted;
        }
        _ => {}
    }
}
