//! Model-related error types.

use std::collections::HashMap;
use std::time::Duration;
/// Model-related errors. External text remains available in typed fields but is never rendered
/// by the standard public formatting contracts.
pub enum ModelError {
    /// HTTP error from the API.
    Http {
        /// HTTP status code.
        status: u16,
        /// Response body.
        body: String,
        /// Response headers.
        headers: HashMap<String, String>,
    },

    /// API-level error.
    Api {
        /// Error message.
        message: String,
        /// Error code.
        code: Option<String>,
    },

    /// Request timeout.
    Timeout(Duration),

    /// Rate limited by the API.
    RateLimited {
        /// Suggested retry delay.
        retry_after: Option<Duration>,
    },

    /// Authentication failed.
    Authentication(String),
    /// Invalid response from the API.
    InvalidResponse(String),
    /// Provider response exceeded the configured byte limit.
    ResponseTooLarge {
        /// Maximum accepted response bytes.
        limit: usize,
    },
    /// Model not found.
    NotFound(String),
    /// Feature not supported by the model.
    NotSupported(String),
    /// JSON serialization/deserialization error.
    Serialization(serde_json::Error),
    /// Request cancelled.
    Cancelled,
    /// Connection error.
    Connection(String),
    /// Content filter triggered.
    ContentFiltered(String),
    /// Context length exceeded.
    ContextLengthExceeded {
        /// Maximum allowed tokens.
        max_tokens: u64,
        /// Requested tokens.
        requested_tokens: u64,
    },
    /// Configuration error.
    Configuration(String),
    /// Network error.
    Network(String),
    /// Other error.
    Other(anyhow::Error),
}

impl std::fmt::Display for ModelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Http { status, .. } => write!(f, "Model HTTP error (status {status})"),
            Self::Api { .. } => f.write_str("Model API error"),
            Self::Timeout(duration) => write!(f, "Model request timed out after {duration:?}"),
            Self::RateLimited { retry_after } => {
                write!(f, "Model request rate limited (retry after {retry_after:?})")
            }
            Self::Authentication(_) => f.write_str("Model authentication failed"),
            Self::InvalidResponse(_) => f.write_str("Model returned an invalid response"),
            Self::ResponseTooLarge { limit } => {
                write!(f, "Provider response exceeded the {limit}-byte limit")
            }
            Self::NotFound(_) => f.write_str("Model was not found"),
            Self::NotSupported(_) => f.write_str("Model feature is not supported"),
            Self::Serialization(_) => f.write_str("Model serialization failed"),
            Self::Cancelled => f.write_str("Model request was cancelled"),
            Self::Connection(_) => f.write_str("Model connection failed"),
            Self::ContentFiltered(_) => f.write_str("Model content was filtered"),
            Self::ContextLengthExceeded {
                max_tokens,
                requested_tokens,
            } => write!(
                f,
                "Model context length exceeded ({max_tokens} tokens maximum, {requested_tokens} requested)"
            ),
            Self::Configuration(_) => f.write_str("Model configuration is invalid"),
            Self::Network(_) => f.write_str("Model network request failed"),
            Self::Other(_) => f.write_str("Model operation failed"),
        }
    }
}

impl std::fmt::Debug for ModelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ModelError({self})")
    }
}

impl std::error::Error for ModelError {}

impl From<serde_json::Error> for ModelError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialization(error)
    }
}

impl From<anyhow::Error> for ModelError {
    fn from(error: anyhow::Error) -> Self {
        Self::Other(error)
    }
}

impl ModelError {
    /// Check if this error is retryable.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        match self {
            ModelError::Timeout(_) => true,
            ModelError::RateLimited { .. } => true,
            ModelError::Connection(_) => true,
            ModelError::Http { status, .. } => *status >= 500,
            _ => false,
        }
    }

    /// Get the retry-after duration if applicable.
    #[must_use]
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            ModelError::RateLimited { retry_after } => *retry_after,
            _ => None,
        }
    }

    /// Create an API error.
    pub fn api(message: impl Into<String>) -> Self {
        Self::Api {
            message: message.into(),
            code: None,
        }
    }

    /// Create an API error with code.
    pub fn api_with_code(message: impl Into<String>, code: impl Into<String>) -> Self {
        Self::Api {
            message: message.into(),
            code: Some(code.into()),
        }
    }

    /// Create a rate limited error.
    pub fn rate_limited(retry_after: Option<Duration>) -> Self {
        Self::RateLimited { retry_after }
    }

    /// Create an HTTP error.
    pub fn http(status: u16, body: impl Into<String>) -> Self {
        Self::Http {
            status,
            body: body.into(),
            headers: HashMap::new(),
        }
    }

    /// Create an HTTP error with headers.
    pub fn http_with_headers(
        status: u16,
        body: impl Into<String>,
        headers: HashMap<String, String>,
    ) -> Self {
        Self::Http {
            status,
            body: body.into(),
            headers,
        }
    }

    /// Create an authentication error.
    pub fn auth(message: impl Into<String>) -> Self {
        Self::Authentication(message.into())
    }

    /// Create an invalid response error.
    pub fn invalid_response(message: impl Into<String>) -> Self {
        Self::InvalidResponse(message.into())
    }

    /// Create a not supported error.
    pub fn not_supported(message: impl Into<String>) -> Self {
        Self::NotSupported(message.into())
    }

    /// Create an unsupported content error.
    pub fn unsupported_content(content: impl Into<String>) -> Self {
        Self::NotSupported(format!("Unsupported content type: {}", content.into()))
    }

    /// Create a configuration error.
    pub fn configuration(message: impl Into<String>) -> Self {
        Self::Configuration(message.into())
    }

    /// Create a network error.
    pub fn network(message: impl Into<String>) -> Self {
        Self::Network(message.into())
    }

    /// Create an API error with status code.
    pub fn api_error(status_code: u16, message: impl Into<String>) -> Self {
        Self::Http {
            status: status_code,
            body: message.into(),
            headers: std::collections::HashMap::new(),
        }
    }
}

impl From<reqwest::Error> for ModelError {
    fn from(err: reqwest::Error) -> Self {
        if err.is_timeout() {
            ModelError::Timeout(Duration::from_secs(30)) // Default timeout
        } else if err.is_connect() {
            ModelError::Connection("provider connection failed".to_string())
        } else if let Some(status) = err.status() {
            ModelError::Http {
                status: status.as_u16(),
                body: "provider request failed".to_string(),
                headers: HashMap::new(),
            }
        } else {
            ModelError::Network("provider transport failed".to_string())
        }
    }
}

/// Result type for model operations.
pub type ModelResult<T> = Result<T, ModelError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_retryable() {
        assert!(ModelError::Timeout(Duration::from_secs(30)).is_retryable());
        assert!(ModelError::rate_limited(None).is_retryable());
        assert!(ModelError::Connection("failed".into()).is_retryable());
        assert!(ModelError::http(500, "Server error").is_retryable());
        assert!(ModelError::http(502, "Bad gateway").is_retryable());

        assert!(!ModelError::http(400, "Bad request").is_retryable());
        assert!(!ModelError::http(401, "Unauthorized").is_retryable());
        assert!(!ModelError::auth("Invalid key").is_retryable());
        assert!(!ModelError::api("Error").is_retryable());
    }

    #[test]
    fn test_retry_after() {
        let err = ModelError::rate_limited(Some(Duration::from_secs(60)));
        assert_eq!(err.retry_after(), Some(Duration::from_secs(60)));

        let err = ModelError::rate_limited(None);
        assert_eq!(err.retry_after(), None);

        let err = ModelError::Timeout(Duration::from_secs(30));
        assert_eq!(err.retry_after(), None);
    }

    #[test]
    fn public_formatting_hides_external_text_for_every_variant() {
        let marker = "provider-secret-marker";
        let mut headers = HashMap::new();
        headers.insert(marker.to_string(), marker.to_string());
        let errors = vec![
            ModelError::Http {
                status: 503,
                body: marker.to_string(),
                headers,
            },
            ModelError::Api {
                message: marker.to_string(),
                code: Some(marker.to_string()),
            },
            ModelError::Timeout(Duration::from_secs(7)),
            ModelError::RateLimited {
                retry_after: Some(Duration::from_secs(9)),
            },
            ModelError::Authentication(marker.to_string()),
            ModelError::InvalidResponse(marker.to_string()),
            ModelError::ResponseTooLarge { limit: 123 },
            ModelError::NotFound(marker.to_string()),
            ModelError::NotSupported(marker.to_string()),
            ModelError::Serialization(serde_json::from_str::<serde_json::Value>("{").unwrap_err()),
            ModelError::Cancelled,
            ModelError::Connection(marker.to_string()),
            ModelError::ContentFiltered(marker.to_string()),
            ModelError::ContextLengthExceeded {
                max_tokens: 10,
                requested_tokens: 11,
            },
            ModelError::Configuration(marker.to_string()),
            ModelError::Network(marker.to_string()),
            ModelError::Other(anyhow::anyhow!(marker)),
        ];
        for error in errors {
            let display = error.to_string();
            let debug = format!("{error:?}");
            assert!(!display.contains(marker), "display leaked: {display}");
            assert!(!debug.contains(marker), "debug leaked: {debug}");
            assert!(std::error::Error::source(&error).is_none());
        }

        assert!(ModelError::http(404, marker).to_string().contains("404"));
    }

    #[tokio::test]
    async fn reqwest_errors_do_not_disclose_url_or_query_secrets() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let marker = "query-secret-marker";
        let error = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .unwrap()
            .get(format!("http://{address}/private?token={marker}"))
            .send()
            .await
            .unwrap_err();
        let diagnostic = ModelError::from(error).to_string();
        assert!(!diagnostic.contains(marker));
        assert!(!diagnostic.contains("/private"));
        assert!(!diagnostic.contains(&address.to_string()));
    }
}
