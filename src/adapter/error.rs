//! Structured backend errors: retry policy never parses provider diagnostics.
use crate::transport::TransportFailure;

pub enum BackendError {
    RequestBound { limit: u64 },
    Transport(TransportFailure),
    Message(String),
    ProtocolUnsupported(String),
}

impl BackendError {
    pub fn from_model(error: serdes_ai::models::ModelError) -> Self {
        if let serdes_ai::models::ModelError::RequestBound { limit } = error {
            return Self::RequestBound { limit };
        }
        if let serdes_ai::models::ModelError::ProtocolUnsupported(detail) = error {
            return Self::ProtocolUnsupported(crate::redact::scrub_and_bound_diagnostic(&detail));
        }
        let error = crate::transport::context_length_400(&error).unwrap_or(error);
        if let Some(failure) = TransportFailure::from_model_error(&error) {
            return Self::Transport(failure);
        }
        let message = match &error {
            serdes_ai::models::ModelError::InvalidResponse(detail) => format!("{error}: {detail}"),
            serdes_ai::models::ModelError::Api { message, code }
                if code
                    .as_deref()
                    .is_some_and(|code| code.starts_with("sse_provider_")) =>
            {
                message.clone()
            }
            _ => error.to_string(),
        };
        Self::Message(crate::redact::scrub_and_bound_diagnostic(&message))
    }
}

impl From<String> for BackendError {
    fn from(message: String) -> Self {
        Self::Message(message)
    }
}
impl From<&str> for BackendError {
    fn from(message: &str) -> Self {
        Self::Message(message.into())
    }
}
impl std::fmt::Display for BackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::Transport(failure) => failure.diagnostic(),
            Self::Message(message) | Self::ProtocolUnsupported(message) => message.clone(),
            Self::RequestBound { limit } => {
                format!("calibrated request exceeds context limit {limit}; no request sent")
            }
        };
        f.write_str(&crate::redact::scrub_and_bound_diagnostic(&message))
    }
}
impl std::error::Error for BackendError {}

impl std::fmt::Debug for BackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}
