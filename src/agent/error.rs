use crate::session::StoreError;

/// An error surfaced to the CLI.
pub struct AgentError {
    pub key: &'static str,
    pub message: String,
    pub code: crate::envelope::Code,
    /// Terminal outcome the run declared for itself, when it declared one: the malformed
    /// tool-call collapse (issue 146) and the exhausted truncation retry (issue 153) both
    /// stay typed failures but carry a distinct verdict the caller can branch on.
    pub terminal_outcome: Option<&'static str>,
}

impl AgentError {
    pub fn new(code: crate::envelope::Code, key: &'static str, msg: impl Into<String>) -> Self {
        AgentError {
            code,
            key,
            message: msg.into(),
            terminal_outcome: None,
        }
    }

    pub fn from_store(error: StoreError) -> Self {
        AgentError {
            code: crate::envelope::Code::Session,
            key: "session",
            message: error.to_string(),
            terminal_outcome: None,
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
