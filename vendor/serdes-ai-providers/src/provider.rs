//! Provider trait and common configuration.
//!
//! A provider represents an AI API service with authentication and configuration.

use reqwest::header::HeaderMap;
use reqwest::Client;
use serdes_ai_models::ModelProfile;
use std::sync::Arc;
use std::time::Duration;

/// Provider trait - provides authenticated access to an AI API.
///
/// Providers handle:
/// - Authentication (API keys, OAuth, etc.)
/// - Base URLs and endpoint configuration
/// - HTTP client configuration
/// - Model profile lookup
pub trait Provider: Send + Sync + std::fmt::Debug {
    /// Provider name (e.g., "openai", "anthropic").
    fn name(&self) -> &str;

    /// Base URL for the API.
    fn base_url(&self) -> &str;

    /// Get an HTTP client configured for this provider.
    fn client(&self) -> &Client;

    /// Get default headers for requests.
    fn default_headers(&self) -> HeaderMap;

    /// Get model profile for a specific model.
    fn model_profile(&self, model_name: &str) -> Option<ModelProfile>;

    /// Check if the provider is configured (has credentials).
    fn is_configured(&self) -> bool {
        true
    }

    /// Get alternate names for this provider.
    fn aliases(&self) -> &[&str] {
        &[]
    }
}

/// Type alias for boxed providers.
pub type BoxedProvider = Arc<dyn Provider>;

/// Common configuration for providers.
#[derive(Debug, Clone, Default)]
pub struct ProviderConfig {
    /// API key for authentication.
    pub api_key: Option<String>,
    /// Custom base URL.
    pub base_url: Option<String>,
    /// Request timeout.
    pub timeout: Option<Duration>,
    /// Maximum retry attempts.
    pub max_retries: Option<u32>,
    /// Organization ID (OpenAI, etc.).
    pub organization: Option<String>,
    /// Project ID (Google, etc.).
    pub project: Option<String>,
    /// Region/location (for cloud providers).
    pub region: Option<String>,
}

impl ProviderConfig {
    /// Create a new empty config.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set API key.
    #[must_use]
    pub fn with_api_key(mut self, key: impl Into<String>) -> Self {
        self.api_key = Some(key.into());
        self
    }

    /// Set base URL.
    #[must_use]
    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = Some(url.into());
        self
    }

    /// Set timeout.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Set max retries.
    #[must_use]
    pub fn with_max_retries(mut self, retries: u32) -> Self {
        self.max_retries = Some(retries);
        self
    }

    /// Set organization.
    #[must_use]
    pub fn with_organization(mut self, org: impl Into<String>) -> Self {
        self.organization = Some(org.into());
        self
    }

    /// Set project.
    #[must_use]
    pub fn with_project(mut self, project: impl Into<String>) -> Self {
        self.project = Some(project.into());
        self
    }

    /// Set region.
    #[must_use]
    pub fn with_region(mut self, region: impl Into<String>) -> Self {
        self.region = Some(region.into());
        self
    }

    /// Load from environment variables with given prefix.
    ///
    /// Looks for:
    /// - `{PREFIX}_API_KEY`
    /// - `{PREFIX}_BASE_URL`
    /// - `{PREFIX}_ORGANIZATION`
    /// - `{PREFIX}_PROJECT`
    /// - `{PREFIX}_REGION`
    pub fn from_env(prefix: &str) -> Self {
        Self {
            api_key: std::env::var(format!("{prefix}_API_KEY")).ok(),
            base_url: std::env::var(format!("{prefix}_BASE_URL")).ok(),
            organization: std::env::var(format!("{prefix}_ORGANIZATION")).ok(),
            project: std::env::var(format!("{prefix}_PROJECT")).ok(),
            region: std::env::var(format!("{prefix}_REGION")).ok(),
            timeout: None,
            max_retries: None,
        }
    }

    /// Build an HTTP client with this config.
    pub fn build_client(&self) -> Client {
        let mut builder = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy();

        if let Some(timeout) = self.timeout {
            builder = builder.timeout(timeout);
        }

        builder
            .build()
            .expect("no-redirect provider HTTP client construction must succeed")
    }
}

/// Provider error types.
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    /// Missing API key.
    #[error("Missing API key: {0}")]
    MissingApiKey(&'static str),

    /// Missing required configuration.
    #[error("Missing configuration: {0}")]
    MissingConfig(String),

    /// Unknown provider.
    #[error("Unknown provider: {0}")]
    UnknownProvider(String),

    /// Invalid model string format.
    #[error("Invalid model string: {0}")]
    InvalidModelString(String),

    /// Provider not configured.
    #[error("Provider not configured: {0}")]
    NotConfigured(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provider_config_builder() {
        let config = ProviderConfig::new()
            .with_api_key("sk-test")
            .with_base_url("https://api.example.com")
            .with_timeout(Duration::from_secs(30))
            .with_organization("org-123");

        assert_eq!(config.api_key, Some("sk-test".to_string()));
        assert_eq!(config.base_url, Some("https://api.example.com".to_string()));
        assert_eq!(config.timeout, Some(Duration::from_secs(30)));
        assert_eq!(config.organization, Some("org-123".to_string()));
    }

    #[test]
    fn test_provider_config_from_env() {
        // Set env vars for test
        std::env::set_var("TEST_PROVIDER_API_KEY", "test-key");
        std::env::set_var("TEST_PROVIDER_BASE_URL", "https://test.com");

        let config = ProviderConfig::from_env("TEST_PROVIDER");

        assert_eq!(config.api_key, Some("test-key".to_string()));
        assert_eq!(config.base_url, Some("https://test.com".to_string()));

        // Clean up
        std::env::remove_var("TEST_PROVIDER_API_KEY");
        std::env::remove_var("TEST_PROVIDER_BASE_URL");
    }

    #[test]
    fn test_build_client() {
        let config = ProviderConfig::new().with_timeout(Duration::from_secs(10));
        let _client = config.build_client();
    }

    #[tokio::test]
    async fn configured_client_never_follows_redirects() {
        use std::io::{Read as _, Write as _};
        use std::net::TcpListener;

        for status in [301, 302, 303, 307, 308] {
            let target = TcpListener::bind("127.0.0.1:0").unwrap();
            target.set_nonblocking(true).unwrap();
            let target_url = format!(
                "http://{}/stolen?api_key=query-secret",
                target.local_addr().unwrap()
            );
            let redirect = TcpListener::bind("127.0.0.1:0").unwrap();
            let redirect_url = format!("http://{}/request", redirect.local_addr().unwrap());
            std::thread::spawn(move || {
                let (mut stream, _) = redirect.accept().unwrap();
                let mut request = [0u8; 16 * 1024];
                let _ = stream.read(&mut request);
                write!(
                    stream,
                    "HTTP/1.1 {status} Redirect\r\nLocation: {target_url}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )
                .unwrap();
            });

            let response = ProviderConfig::new()
                .build_client()
                .post(redirect_url)
                .bearer_auth("bearer-secret")
                .header("x-api-key", "header-secret")
                .body("private request body")
                .send()
                .await
                .unwrap();
            assert_eq!(response.status().as_u16(), status);
            std::thread::sleep(Duration::from_millis(20));
            assert_eq!(
                target.accept().unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock,
                "redirect target was contacted for status {status}"
            );
        }
    }
}
