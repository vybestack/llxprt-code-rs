//! Model-related error types.

use std::collections::HashMap;
use std::time::Duration;

/// Retryability classification for a transport failure. The class states a fact about
/// the failure; it is not a retry policy, which is owned by the host.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TransportClass {
    /// Provider throttling: HTTP 429 without quota or billing wording in the body.
    /// Retryable, with or without a `Retry-After` hint.
    RateLimit,
    /// Quota or billing hard stop (a weekly or monthly usage cap, `insufficient_quota`,
    /// a payment requirement). Not retryable: backoff cannot clear it.
    QuotaExhausted,
    /// Transient provider-side failure (5xx). Retryable.
    TransientServer,
    /// Connection-level failure: connect, timeout, or a dropped or undecodable body.
    /// Retryable.
    Connectivity,
    /// Definitive refusal: a 4xx authorization or request-shape error. Not retryable.
    Permanent,
}

impl TransportClass {
    /// Whether an identical request could still succeed. Throttling, transient 5xx, and
    /// connectivity failures can; quota exhaustion and 4xx refusals cannot.
    pub fn is_retryable(self) -> bool {
        matches!(
            self,
            Self::RateLimit | Self::TransientServer | Self::Connectivity
        )
    }

    /// The stable, value-free token used by diagnostics and machine consumers.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RateLimit => "rate-limit",
            Self::QuotaExhausted => "quota-exhausted",
            Self::TransientServer => "transient-server",
            Self::Connectivity => "connectivity",
            Self::Permanent => "permanent",
        }
    }
}

/// Where a transport failure happened. Value-free by construction: no host, port, path,
/// query, or credential ever travels in it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TransportOrigin {
    /// Connection establishment failed: DNS resolution, TCP, or TLS.
    Connect,
    /// The configured request timeout fired. reqwest reports no elapsed duration.
    Timeout,
    /// The request could not be built or sent.
    Request,
    /// The response body transfer failed mid-stream (a dropped connection).
    Body,
    /// The response body could not be decoded.
    Decode,
    /// A redirect was refused.
    Redirect,
    /// The provider answered with this HTTP error status.
    Status(u16),
    /// An unclassified transport failure.
    Unknown,
}

impl TransportOrigin {
    /// The stable, value-free token used by diagnostics.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Connect => "connect",
            Self::Timeout => "timeout",
            Self::Request => "request",
            Self::Body => "body",
            Self::Decode => "decode",
            Self::Redirect => "redirect",
            Self::Status(_) => "status",
            Self::Unknown => "unknown",
        }
    }
}

/// The scheme class of the failed request URL, when reqwest exposed one. Only the scheme
/// travels; the host, path, and query are credential-adjacent surfaces and never do.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UrlClass {
    /// An `https` request.
    Https,
    /// An `http` request.
    Http,
    /// Any other scheme.
    Other,
}

impl UrlClass {
    fn of(url: &reqwest::Url) -> Self {
        match url.scheme() {
            "https" => Self::Https,
            "http" => Self::Http,
            _ => Self::Other,
        }
    }

    /// The stable, value-free token used by diagnostics.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Https => "https",
            Self::Http => "http",
            Self::Other => "other",
        }
    }
}

/// The most response-body text a transport failure retains as a diagnostic prefix. The
/// body was already capped by the bounded reader; this bounds the retained slice again.
/// The prefix is a typed field only: the public formatting contracts never render it.
pub const MAX_TRANSPORT_BODY_PREFIX_BYTES: usize = 512;

/// Structured, value-free facts about a transport failure: where it happened, the URL
/// scheme class, the HTTP status, and a bounded body prefix with its total byte length.
#[derive(Clone, Debug)]
pub struct TransportDetail {
    /// Where the failure happened.
    pub origin: TransportOrigin,
    /// The scheme class of the request URL, when reqwest exposed one.
    pub url_class: Option<UrlClass>,
    /// A bounded, lossily decoded response-body prefix; `None` when no body exists.
    pub body_prefix: Option<String>,
    /// The total response-body byte length the prefix was taken from.
    pub body_bytes: usize,
    /// The provider's `Retry-After` hint, when one was sent.
    pub retry_after: Option<Duration>,
}

impl TransportDetail {
    /// Derive the retryability class from the origin, the status, and the bounded body.
    pub fn classify(&self) -> TransportClass {
        match self.origin {
            TransportOrigin::Status(status) => {
                classify_status(status, self.body_prefix.as_deref().unwrap_or(""))
            }
            _ => TransportClass::Connectivity,
        }
    }

    /// Build the facts for a provider status response.
    pub fn from_status(
        status: u16,
        body_prefix: Option<String>,
        body_bytes: usize,
        retry_after: Option<Duration>,
    ) -> Self {
        Self {
            origin: TransportOrigin::Status(status),
            url_class: None,
            body_prefix,
            body_bytes,
            retry_after,
        }
    }

    /// Extract the value-free facts a reqwest error still carries. No response body is
    /// available at this layer: a reqwest error carries only a failure class and,
    /// sometimes, a request URL.
    fn from_reqwest(error: &reqwest::Error) -> Self {
        let origin = if error.is_timeout() {
            TransportOrigin::Timeout
        } else if error.is_connect() {
            TransportOrigin::Connect
        } else if error.is_request() {
            TransportOrigin::Request
        } else if error.is_body() {
            TransportOrigin::Body
        } else if error.is_decode() {
            TransportOrigin::Decode
        } else if error.is_redirect() {
            TransportOrigin::Redirect
        } else if let Some(status) = error.status() {
            TransportOrigin::Status(status.as_u16())
        } else {
            TransportOrigin::Unknown
        };
        Self {
            origin,
            url_class: error.url().map(UrlClass::of),
            body_prefix: None,
            body_bytes: 0,
            retry_after: None,
        }
    }
}

/// Classify a provider HTTP status together with its bounded body text. Quota and
/// billing wording wins over the status, so a weekly usage limit reported on a 429 or a
/// 403 is a non-retryable exhaustion rather than a transient throttle.
pub fn classify_status(status: u16, body: &str) -> TransportClass {
    if status == 402 || quota_body(body) {
        return TransportClass::QuotaExhausted;
    }
    match status {
        429 => TransportClass::RateLimit,
        408 => TransportClass::Connectivity,
        500..=599 => TransportClass::TransientServer,
        _ => TransportClass::Permanent,
    }
}

/// Whether a bounded provider body reads as a quota or billing hard stop.
fn quota_body(body: &str) -> bool {
    const MARKERS: [&str; 6] = [
        "insufficient_quota",
        "usage limit",
        "usage cap",
        "add extra usage",
        "exceeded your current quota",
        "payment required",
    ];
    let lowered = body.to_ascii_lowercase();
    MARKERS.iter().any(|marker| lowered.contains(marker))
}
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

    /// Request timeout. No duration is carried: reqwest reports only that a timeout
    /// fired, so the configured request timeout stays the single source of truth in the
    /// host configuration.
    Timeout,

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
    /// Network-layer transport failure carrying structured, value-free facts: where it
    /// happened, the URL scheme class, the HTTP status, and a bounded body prefix. The
    /// prefix is a typed field only; the public formatting never renders it, so the host
    /// renders it on its own scrubbed and bounded diagnostic path.
    Transport(TransportDetail),
    /// Other error.
    Other(anyhow::Error),
}

impl std::fmt::Display for ModelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Http { status, .. } => write!(f, "Model HTTP error (status {status})"),
            Self::Api { .. } => f.write_str("Model API error"),
            Self::Timeout => f.write_str("Model request timed out"),
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
            Self::Transport(detail) => {
                let class = detail.classify();
                let site = match detail.origin {
                    TransportOrigin::Status(status) => format!("status {status}"),
                    other => format!("origin {}", other.as_str()),
                };
                let verdict = if class.is_retryable() {
                    "retryable"
                } else {
                    "not retryable"
                };
                write!(
                    f,
                    "Model transport failed ({site}, class {}, {verdict})",
                    class.as_str()
                )
            }
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
            ModelError::Timeout => true,
            ModelError::RateLimited { .. } => true,
            ModelError::Connection(_) => true,
            ModelError::Http { status, .. } => *status >= 500,
            ModelError::Transport(detail) => detail.classify().is_retryable(),
            _ => false,
        }
    }

    /// The retryability classification when this error is a transport-layer failure (a
    /// network, timeout, or provider HTTP status failure); `None` otherwise. This is the
    /// classification a retry policy builds on, while [`Self::is_retryable`] keeps its
    /// historical, coarser outcome for existing callers.
    pub fn transport_class(&self) -> Option<TransportClass> {
        match self {
            ModelError::Timeout | ModelError::Connection(_) | ModelError::Network(_) => {
                Some(TransportClass::Connectivity)
            }
            ModelError::RateLimited { .. } => Some(TransportClass::RateLimit),
            ModelError::Transport(detail) => Some(detail.classify()),
            ModelError::Http { status, body, .. } => Some(classify_status(*status, body)),
            ModelError::Authentication(_) | ModelError::Api { .. } => {
                Some(TransportClass::Permanent)
            }
            _ => None,
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
        // A reqwest timeout error carries no duration, so no number is reported here;
        // the host already knows the configured timeout it passed to the request.
        if err.is_timeout() {
            ModelError::Timeout
        } else if err.is_connect() {
            ModelError::Connection("provider connection failed".to_string())
        } else if let Some(status) = err.status() {
            ModelError::Http {
                status: status.as_u16(),
                body: "provider request failed".to_string(),
                headers: HashMap::new(),
            }
        } else {
            // Every remaining transport failure keeps its structured, value-free facts
            // (the origin, the URL scheme class, and a status when reqwest has one)
            // instead of collapsing into a bare, unclassifiable string.
            ModelError::Transport(TransportDetail::from_reqwest(&err))
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
        assert!(ModelError::Timeout.is_retryable());
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

        let err = ModelError::Timeout;
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
            ModelError::Timeout,
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

    /// A reqwest timeout carries no duration, so the mapped error never reports one: whatever
    /// request timeout was configured, the diagnostic stays the same value-free sentence and
    /// never asserts a number of seconds.
    #[tokio::test]
    async fn reqwest_timeouts_do_not_report_a_fabricated_duration() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else {
                    continue;
                };
                // Read the request, then hold the connection open without ever writing a
                // response head so the client's configured request timeout fires.
                use std::io::Read as _;
                let mut buffer = [0u8; 8192];
                while stream.read(&mut buffer).unwrap_or(0) > 0 {
                    let _ = buffer;
                }
            }
        });

        for configured in [Duration::from_millis(80), Duration::from_millis(160)] {
            let error = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .no_proxy()
                .build()
                .unwrap()
                .get(format!("http://{address}/v1/chat/completions"))
                .timeout(configured)
                .send()
                .await
                .unwrap_err();
            assert!(error.is_timeout(), "expected a timeout: {error:?}");
            let detail = TransportDetail::from_reqwest(&error);
            let mapped = ModelError::from(error);
            let diagnostic = mapped.to_string();
            assert_eq!(diagnostic, "Model request timed out");
            assert!(
                !diagnostic.chars().any(|c| c.is_ascii_digit()),
                "diagnostic asserted a duration: {diagnostic}"
            );
            // A reqwest timeout maps onto the timeout variant, whose classification is
            // the same retryable connectivity family the origin token names.
            assert_eq!(mapped.transport_class(), Some(TransportClass::Connectivity));
            assert!(mapped.is_retryable());
            assert_eq!(detail.origin, TransportOrigin::Timeout);
        }
        drop(server);
    }

    /// Every retained status classifies: quota and billing wording wins over the status,
    /// 429 is a throttle, 408 and 5xx are retryable, and every other 4xx is permanent.
    #[test]
    fn status_classification_covers_retryability_families() {
        let cases = [
            (429u16, TransportClass::RateLimit),
            (408, TransportClass::Connectivity),
            (500, TransportClass::TransientServer),
            (502, TransportClass::TransientServer),
            (503, TransportClass::TransientServer),
            (529, TransportClass::TransientServer),
            (402, TransportClass::QuotaExhausted),
            (400, TransportClass::Permanent),
            (401, TransportClass::Permanent),
            (403, TransportClass::Permanent),
            (404, TransportClass::Permanent),
            (413, TransportClass::Permanent),
            (422, TransportClass::Permanent),
        ];
        for (status, expected) in cases {
            assert_eq!(
                classify_status(status, ""),
                expected,
                "status {status} classified wrong"
            );
            assert_eq!(
                expected.is_retryable(),
                expected != TransportClass::Permanent && expected != TransportClass::QuotaExhausted
            );
            let detail = TransportDetail::from_status(status, None, 0, None);
            assert_eq!(detail.classify(), expected, "status {status} detail class");
            assert_eq!(
                ModelError::Transport(detail).transport_class(),
                Some(expected)
            );
        }
    }

    /// Quota and billing wording in the bounded body wins over the status, so a weekly
    /// usage limit riding on a 429 or a 403 is a non-retryable exhaustion rather than a
    /// throttle or a plain refusal.
    #[test]
    fn quota_wording_wins_over_the_status() {
        for status in [429u16, 403, 400] {
            for body in [
                "insufficient_quota",
                "You have exceeded your current quota",
                "usage limit reached for this plan",
                "weekly usage cap exceeded",
                "please add extra usage to continue",
                "payment required before continuing",
            ] {
                assert_eq!(
                    classify_status(status, body),
                    TransportClass::QuotaExhausted,
                    "status {status} body {body} must classify as quota exhaustion"
                );
            }
        }
        // Unrelated wording keeps the status class.
        assert_eq!(
            classify_status(429, "slow down a little"),
            TransportClass::RateLimit
        );
        assert_eq!(
            classify_status(503, "the model is overloaded"),
            TransportClass::TransientServer
        );
    }

    /// A reqwest error's origin is derived from its failure kind, so a connect refusal
    /// reports `Connect` and no URL ever travels beyond its scheme class.
    #[tokio::test]
    async fn reqwest_kinds_map_to_transport_origins() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let error = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .unwrap()
            .get(format!("https://{address}/v1/chat/completions"))
            .send()
            .await
            .unwrap_err();
        assert!(error.is_connect(), "expected a connect failure: {error:?}");
        // The reqwest kind maps onto the structured, value-free origin and scheme class.
        let detail = TransportDetail::from_reqwest(&error);
        let url_class = detail.url_class;
        let mapped = ModelError::from(error);
        let retryable = mapped.is_retryable();
        assert_eq!(mapped.transport_class(), Some(TransportClass::Connectivity));
        assert_eq!(detail.origin, TransportOrigin::Connect);
        assert_eq!(detail.classify(), TransportClass::Connectivity);
        assert_eq!(url_class, Some(UrlClass::Https));
        assert_eq!(detail.url_class, Some(UrlClass::Https));
        assert!(retryable);
        assert!(detail.body_prefix.is_none() && detail.body_bytes == 0);
        assert!(ModelError::Transport(detail)
            .to_string()
            .starts_with("Model transport failed (origin connect"));
    }
}
