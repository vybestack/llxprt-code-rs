//! Typed request failure boundary. Diagnostics are never a classification protocol.

use crate::transport::TransportFailure;
use serdes_ai::models::ModelError;

#[derive(Clone)]
pub enum ModelFailure {
    /// A complete request failed before any result was returned to the agent.
    Transport(TransportFailure),
    /// A streaming backend cannot prove the failed request had no partial response.
    Incomplete(TransportFailure),
    /// The existing one-shot context compaction path, not transport retry.
    ContextLimit(String),
    /// Protocol, validation, cancellation, or other terminal provider failure.
    Terminal(String),
}

impl ModelFailure {
    pub fn from_model_error(error: ModelError) -> Self {
        let error = crate::transport::context_length_400(&error).unwrap_or(error);
        if matches!(error, ModelError::ContextLengthExceeded { .. }) {
            return Self::ContextLimit(error.to_string());
        }
        if let ModelError::Transport(detail) = &error {
            use serdes_ai::models::error::TransportOrigin;
            if matches!(
                detail.origin,
                TransportOrigin::Request
                    | TransportOrigin::Decode
                    | TransportOrigin::Redirect
                    | TransportOrigin::Unknown
            ) {
                return Self::Incomplete(
                    TransportFailure::from_model_error(&error).expect("transport variant"),
                );
            }
        }
        if let Some(failure) = TransportFailure::from_model_error(&error) {
            return Self::Transport(failure);
        }
        let message = match &error {
            ModelError::InvalidResponse(detail) => format!("{error}: {detail}"),
            _ => error.to_string(),
        };
        Self::Terminal(crate::redact::scrub_and_bound_diagnostic(&message))
    }

    pub fn diagnostic(&self) -> String {
        let message = match self {
            Self::Transport(failure) | Self::Incomplete(failure) => failure.diagnostic(),
            Self::ContextLimit(message) | Self::Terminal(message) => message.clone(),
        };
        crate::redact::scrub_and_bound_diagnostic(&message)
    }
}

impl std::fmt::Display for ModelFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.diagnostic())
    }
}

impl std::fmt::Debug for ModelFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for ModelFailure {}
