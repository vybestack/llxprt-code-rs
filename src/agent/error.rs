use crate::session::StoreError;

/// An error surfaced to the CLI.
pub struct AgentError {
    pub key: &'static str,
    pub message: String,
    pub code: crate::envelope::Code,
    /// The stable `error.code` token written to the stdout envelope. `None` means the
    /// [`Self::key`] is already the envelope token. A model transport failure carries its
    /// finer `model-<class>` transport key here (for example `model-quota-exhausted`)
    /// while `Self::key` stays `model`, so the process exit code family is unchanged.
    pub envelope_code: Option<&'static str>,
}

impl AgentError {
    pub fn new(code: crate::envelope::Code, key: &'static str, msg: impl Into<String>) -> Self {
        AgentError {
            code,
            key,
            message: msg.into(),
            envelope_code: None,
        }
    }

    /// Carry a finer envelope `error.code` token than [`Self::key`], leaving the process
    /// exit code family alone.
    pub fn with_envelope_code(mut self, envelope_code: &'static str) -> Self {
        self.envelope_code = Some(envelope_code);
        self
    }

    pub fn from_store(error: StoreError) -> Self {
        AgentError {
            code: crate::envelope::Code::Session,
            key: "session",
            message: error.to_string(),
            envelope_code: None,
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
