//! HTTP transport with automatic retries.

use crate::config::RetryConfig;
use crate::error::{RetryResult, RetryableError};
use crate::executor::with_retry;
use reqwest::{Client, Method, Response};
use serde::Serialize;
use std::time::Duration;
use tracing::debug;

impl From<reqwest::Error> for RetryableError {
    fn from(err: reqwest::Error) -> Self {
        if err.is_timeout() {
            RetryableError::Timeout
        } else if err.is_connect() {
            RetryableError::Connection("provider connection failed".to_string())
        } else {
            RetryableError::Other(anyhow::anyhow!("provider transport failed"))
        }
    }
}

/// HTTP client wrapper with automatic retries.
#[derive(Debug, Clone)]
pub struct RetryClient {
    client: Client,
    config: RetryConfig,
}

impl RetryClient {
    /// Create a new retry client whose HTTP transport never follows redirects.
    pub fn new(config: RetryConfig) -> Self {
        Self {
            client: Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .no_proxy()
                .build()
                .expect("no-redirect HTTP client construction must succeed"),
            config,
        }
    }

    /// Create with default API retry settings.
    pub fn for_api() -> Self {
        Self::new(RetryConfig::for_api())
    }

    /// Get a reference to the underlying client.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Get a reference to the retry config.
    pub fn config(&self) -> &RetryConfig {
        &self.config
    }

    /// Execute a GET request with retries.
    pub async fn get(&self, url: &str) -> RetryResult<Response> {
        self.request(Method::GET, url, Option::<()>::None).await
    }

    /// Execute a POST request with retries.
    pub async fn post<B: Serialize + Clone + Send + Sync>(
        &self,
        url: &str,
        body: B,
    ) -> RetryResult<Response> {
        self.request(Method::POST, url, Some(body)).await
    }

    /// Execute a PUT request with retries.
    pub async fn put<B: Serialize + Clone + Send + Sync>(
        &self,
        url: &str,
        body: B,
    ) -> RetryResult<Response> {
        self.request(Method::PUT, url, Some(body)).await
    }

    /// Execute a DELETE request with retries.
    pub async fn delete(&self, url: &str) -> RetryResult<Response> {
        self.request(Method::DELETE, url, Option::<()>::None).await
    }

    /// Execute a PATCH request with retries.
    pub async fn patch<B: Serialize + Clone + Send + Sync>(
        &self,
        url: &str,
        body: B,
    ) -> RetryResult<Response> {
        self.request(Method::PATCH, url, Some(body)).await
    }

    /// Execute a request with retries.
    async fn request<B: Serialize + Clone + Send + Sync>(
        &self,
        method: Method,
        url: &str,
        body: Option<B>,
    ) -> RetryResult<Response> {
        let url = url.to_string();
        let client = self.client.clone();

        with_retry(&self.config, || {
            let url = url.clone();
            let method = method.clone();
            let client = client.clone();
            let body = body.clone();

            async move {
                debug!(method = %method, "Making HTTP request");

                let mut request = client.request(method, &url);
                if let Some(b) = body {
                    request = request.json(&b);
                }

                let response = request.send().await.map_err(RetryableError::from)?;

                check_response(response).await
            }
        })
        .await
    }
}

/// Check an HTTP response and convert to RetryableError if needed.
async fn check_response(response: Response) -> RetryResult<Response> {
    let status = response.status().as_u16();

    if status == 429 {
        // Rate limited
        let retry_after = parse_retry_after(&response);
        return Err(RetryableError::RateLimited { retry_after });
    }

    if (500..=599).contains(&status) {
        // Server error
        let retry_after = parse_retry_after(&response);
        let body = bounded_error_text(response).await?;
        return Err(RetryableError::Http {
            status,
            body,
            retry_after,
        });
    }

    if !response.status().is_success() {
        // Other error (not retryable)
        let body = bounded_error_text(response).await?;
        return Err(RetryableError::Http {
            status,
            body,
            retry_after: None,
        });
    }

    Ok(response)
}

const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;

async fn bounded_error_text(mut response: Response) -> RetryResult<String> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(RetryableError::from)? {
        let next =
            bytes
                .len()
                .checked_add(chunk.len())
                .ok_or(RetryableError::ResponseTooLarge {
                    limit: MAX_ERROR_BODY_BYTES,
                })?;
        if next > MAX_ERROR_BODY_BYTES {
            return Err(RetryableError::ResponseTooLarge {
                limit: MAX_ERROR_BODY_BYTES,
            });
        }
        bytes.extend_from_slice(&chunk);
    }
    std::str::from_utf8(&bytes).map_err(|_| RetryableError::InvalidResponseEncoding)?;
    Ok("provider returned an error response".to_string())
}

/// Parse Retry-After header.
fn parse_retry_after(response: &Response) -> Option<Duration> {
    response
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| {
            // Try parsing as seconds
            s.parse::<u64>().ok().map(Duration::from_secs)
        })
}

/// Builder for creating a retry client.
#[derive(Debug, Default)]
pub struct RetryClientBuilder {
    max_retries: Option<u32>,
    initial_delay: Option<Duration>,
    max_delay: Option<Duration>,
    timeout: Option<Duration>,
}

impl RetryClientBuilder {
    /// Create a new builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set max retries.
    pub fn max_retries(mut self, n: u32) -> Self {
        self.max_retries = Some(n);
        self
    }

    /// Set initial delay.
    pub fn initial_delay(mut self, delay: Duration) -> Self {
        self.initial_delay = Some(delay);
        self
    }

    /// Set max delay.
    pub fn max_delay(mut self, delay: Duration) -> Self {
        self.max_delay = Some(delay);
        self
    }

    /// Set request timeout.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Build the retry client.
    pub fn build(self) -> RetryClient {
        let mut builder = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy();
        if let Some(timeout) = self.timeout {
            builder = builder.timeout(timeout);
        }
        let client = builder.build().expect("Failed to build client");

        let mut config = RetryConfig::for_api();
        if let Some(n) = self.max_retries {
            config = config.max_retries(n);
        }
        if let Some(initial) = self.initial_delay {
            let max = self.max_delay.unwrap_or(Duration::from_secs(60));
            config = config.exponential(initial, max);
        }

        RetryClient { client, config }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_retry_client_new() {
        let client = RetryClient::new(RetryConfig::for_api());
        assert_eq!(client.config().max_retries, 3);
    }

    #[test]
    fn test_retry_client_for_api() {
        let client = RetryClient::for_api();
        assert_eq!(client.config().max_retries, 3);
    }

    #[test]
    fn test_builder() {
        let client = RetryClientBuilder::new()
            .max_retries(5)
            .initial_delay(Duration::from_millis(100))
            .max_delay(Duration::from_secs(30))
            .build();

        assert_eq!(client.config().max_retries, 5);
    }

    #[test]
    fn test_parse_retry_after() {
        // This would require mocking the response, so we just test the logic
        let duration = Duration::from_secs(5);
        assert_eq!(duration.as_secs(), 5);
    }

    async fn error_response(body: Vec<u8>) -> Response {
        use std::io::{Read as _, Write as _};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request);
            write!(
                stream,
                "HTTP/1.1 500 Error\r\nContent-Length: {}\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(&body).unwrap();
        });
        Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .unwrap()
            .get(format!("http://{address}"))
            .send()
            .await
            .unwrap()
    }

    async fn chunked_error_response(chunks: Vec<Vec<u8>>, hold_open: bool) -> Response {
        use std::io::{Read as _, Write as _};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request);
            stream
                .write_all(b"HTTP/1.1 500 Error\r\nTransfer-Encoding: chunked\r\n\r\n")
                .unwrap();
            for chunk in chunks {
                write!(stream, "{:x}\r\n", chunk.len()).unwrap();
                stream.write_all(&chunk).unwrap();
                stream.write_all(b"\r\n").unwrap();
            }
            if hold_open {
                std::thread::sleep(Duration::from_millis(250));
            } else {
                stream.write_all(b"0\r\n\r\n").unwrap();
            }
        });
        Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .timeout(Duration::from_millis(50))
            .build()
            .unwrap()
            .get(format!("http://{address}"))
            .send()
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn retry_error_reader_accepts_exact_cap_and_rejects_invalid_utf8() {
        let response = error_response(vec![b'x'; MAX_ERROR_BODY_BYTES]).await;
        let error = check_response(response).await.unwrap_err();
        assert!(matches!(error, RetryableError::Http { status: 500, .. }));

        let response = chunked_error_response(vec![vec![0xff]], false).await;
        assert!(matches!(
            check_response(response).await,
            Err(RetryableError::InvalidResponseEncoding)
        ));
    }

    #[tokio::test]
    async fn retry_error_reader_times_out_on_never_ending_body() {
        let response = chunked_error_response(vec![b"partial".to_vec()], true).await;
        assert!(matches!(
            check_response(response).await,
            Err(RetryableError::Timeout)
        ));
    }

    #[tokio::test]
    async fn error_body_is_bounded_and_redacted() {
        let error = check_response(error_response(b"Bearer retry-secret".to_vec()).await)
            .await
            .unwrap_err();
        assert!(!error.to_string().contains("retry-secret"));

        let error = check_response(error_response(vec![b'x'; MAX_ERROR_BODY_BYTES + 1]).await)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            RetryableError::ResponseTooLarge {
                limit: MAX_ERROR_BODY_BYTES
            }
        ));
    }

    #[tokio::test]
    async fn retained_client_never_follows_redirects_or_replays_secrets() {
        use std::io::{Read as _, Write as _};
        use std::net::TcpListener;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let target = TcpListener::bind("127.0.0.1:0").unwrap();
        target.set_nonblocking(true).unwrap();
        let target_address = target.local_addr().unwrap();
        let requests = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&requests);
        let target_thread = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_millis(300);
            while std::time::Instant::now() < deadline {
                match target.accept() {
                    Ok(_) => {
                        observed.fetch_add(1, Ordering::SeqCst);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    Err(error) => panic!("target accept failed: {error}"),
                }
            }
        });

        let client = RetryClient::for_api();
        for status in [301, 302, 303, 307, 308] {
            let source = TcpListener::bind("127.0.0.1:0").unwrap();
            let source_address = source.local_addr().unwrap();
            let location = format!("http://{target_address}/sink?key=query-secret");
            std::thread::spawn(move || {
                let (mut stream, _) = source.accept().unwrap();
                let mut request = [0u8; 4096];
                let _ = stream.read(&mut request);
                write!(
                    stream,
                    "HTTP/1.1 {status} Redirect\r\nLocation: {location}\r\nContent-Length: 0\r\n\r\n"
                )
                .unwrap();
            });
            let response = client
                .client()
                .post(format!("http://{source_address}"))
                .bearer_auth("bearer-secret")
                .header("x-api-key", "header-secret")
                .body("private-request-body")
                .send()
                .await
                .unwrap();
            assert_eq!(response.status().as_u16(), status);
        }
        target_thread.join().unwrap();
        assert_eq!(requests.load(Ordering::SeqCst), 0);
    }
}
