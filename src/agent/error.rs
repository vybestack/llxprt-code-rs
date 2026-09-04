use crate::session::StoreError;

/// An error surfaced to the CLI.
pub struct AgentError {
    pub key: &'static str,
    pub message: String,
    pub code: crate::envelope::Code,
}

impl AgentError {
    pub fn new(code: crate::envelope::Code, key: &'static str, msg: impl Into<String>) -> Self {
        AgentError {
            code,
            key,
            message: msg.into(),
        }
    }

    pub fn from_store(error: StoreError) -> Self {
        AgentError {
            code: crate::envelope::Code::Session,
            key: "session",
            message: error.to_string(),
        }
    }
}

impl std::fmt::Display for AgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let rendered = format!("{}: {}", self.key, self.message);
        f.write_str(&crate::redact::scrub_and_bound_diagnostic(&rendered))
    }
}

impl std::fmt::Debug for AgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for AgentError {}
