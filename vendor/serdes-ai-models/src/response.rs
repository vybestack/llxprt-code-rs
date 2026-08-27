//! Bounded HTTP response-body handling shared by provider models.

use crate::error::{ModelError, ModelResult};
#[cfg(any(
    feature = "antigravity",
    feature = "anthropic",
    feature = "cohere",
    feature = "google",
    feature = "huggingface",
    feature = "mistral",
    feature = "ollama",
    feature = "openai"
))]
use serde::de::DeserializeOwned;

/// Maximum successful JSON response body accepted from a provider.
pub(crate) const MAX_SUCCESS_BODY_BYTES: usize = 64 * 1024 * 1024;
/// Maximum error response body consumed for protocol validation.
pub(crate) const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;
/// Maximum unprocessed UTF-8 text retained by a provider stream parser.
#[cfg(any(
    feature = "antigravity",
    feature = "anthropic",
    feature = "chatgpt-oauth",
    feature = "cohere",
    feature = "google",
    feature = "huggingface",
    feature = "openai"
))]
pub(crate) const MAX_STREAM_BUFFER_BYTES: usize = 1024 * 1024;

async fn read_bounded(mut response: reqwest::Response, limit: usize) -> ModelResult<Vec<u8>> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(ModelError::from)? {
        let next = bytes
            .len()
            .checked_add(chunk.len())
            .ok_or(ModelError::ResponseTooLarge { limit })?;
        if next > limit {
            return Err(ModelError::ResponseTooLarge { limit });
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// Stream a successful provider response while enforcing the same aggregate byte cap as a
/// non-streaming response.
#[cfg(any(
    feature = "antigravity",
    feature = "anthropic",
    feature = "chatgpt-oauth",
    feature = "cohere",
    feature = "google",
    feature = "huggingface",
    feature = "openai"
))]
pub(crate) fn stream(
    response: reqwest::Response,
) -> std::pin::Pin<Box<dyn futures::Stream<Item = ModelResult<bytes::Bytes>> + Send>> {
    stream_bounded(response, MAX_SUCCESS_BODY_BYTES)
}

#[cfg(any(
    feature = "antigravity",
    feature = "anthropic",
    feature = "chatgpt-oauth",
    feature = "cohere",
    feature = "google",
    feature = "huggingface",
    feature = "openai"
))]
fn stream_bounded(
    response: reqwest::Response,
    limit: usize,
) -> std::pin::Pin<Box<dyn futures::Stream<Item = ModelResult<bytes::Bytes>> + Send>> {
    use futures::StreamExt as _;

    Box::pin(futures::stream::unfold(
        (Box::pin(response.bytes_stream()), 0usize, false),
        move |(mut inner, total, done)| async move {
            if done {
                return None;
            }
            match inner.next().await {
                Some(Ok(chunk)) => {
                    let next = total.checked_add(chunk.len());
                    match next {
                        Some(next) if next <= limit => Some((Ok(chunk), (inner, next, false))),
                        _ => Some((
                            Err(ModelError::ResponseTooLarge { limit }),
                            (inner, total, true),
                        )),
                    }
                }
                Some(Err(error)) => Some((Err(ModelError::from(error)), (inner, total, true))),
                None => None,
            }
        },
    ))
}

/// Incremental strict UTF-8 decoder for provider streams. Incomplete code points may cross
/// transport chunks; malformed sequences are rejected without exposing their bytes.
#[cfg(any(
    feature = "antigravity",
    feature = "anthropic",
    feature = "chatgpt-oauth",
    feature = "cohere",
    feature = "google",
    feature = "huggingface",
    feature = "openai"
))]
#[derive(Default)]
pub(crate) struct Utf8StreamDecoder {
    pending: Vec<u8>,
}

#[cfg(any(
    feature = "antigravity",
    feature = "anthropic",
    feature = "chatgpt-oauth",
    feature = "cohere",
    feature = "google",
    feature = "huggingface",
    feature = "openai"
))]
impl Utf8StreamDecoder {
    pub(crate) fn push(&mut self, bytes: &[u8], output: &mut String) -> ModelResult<()> {
        let buffered = output
            .len()
            .checked_add(self.pending.len())
            .and_then(|size| size.checked_add(bytes.len()))
            .ok_or(ModelError::ResponseTooLarge {
                limit: MAX_STREAM_BUFFER_BYTES,
            })?;
        if buffered > MAX_STREAM_BUFFER_BYTES {
            return Err(ModelError::ResponseTooLarge {
                limit: MAX_STREAM_BUFFER_BYTES,
            });
        }
        self.pending.extend_from_slice(bytes);
        match std::str::from_utf8(&self.pending) {
            Ok(text) => {
                output.push_str(text);
                self.pending.clear();
                Ok(())
            }
            Err(error) if error.error_len().is_none() => {
                let valid = error.valid_up_to();
                output.push_str(std::str::from_utf8(&self.pending[..valid]).map_err(|_| {
                    ModelError::invalid_response("provider stream contained invalid UTF-8")
                })?);
                self.pending.drain(..valid);
                Ok(())
            }
            Err(_) => Err(ModelError::invalid_response(
                "provider stream contained invalid UTF-8",
            )),
        }
    }

    pub(crate) fn finish(&self) -> ModelResult<()> {
        if self.pending.is_empty() {
            Ok(())
        } else {
            Err(ModelError::invalid_response(
                "provider stream ended with incomplete UTF-8",
            ))
        }
    }
}

/// Deserialize one successful provider response without materializing more than the response cap.
#[cfg(any(
    feature = "antigravity",
    feature = "anthropic",
    feature = "cohere",
    feature = "google",
    feature = "huggingface",
    feature = "mistral",
    feature = "ollama",
    feature = "openai"
))]
pub(crate) async fn json<T: DeserializeOwned>(response: reqwest::Response) -> ModelResult<T> {
    let bytes = read_bounded(response, MAX_SUCCESS_BODY_BYTES).await?;
    serde_json::from_slice(&bytes).map_err(ModelError::from)
}

/// Consume a bounded error response and return a fixed diagnostic that cannot disclose secrets.
pub(crate) async fn error_text(response: reqwest::Response) -> ModelResult<String> {
    let bytes = read_bounded(response, MAX_ERROR_BODY_BYTES).await?;
    std::str::from_utf8(&bytes)
        .map_err(|_| ModelError::InvalidResponse("provider error body is not UTF-8".to_string()))?;
    Ok("provider returned an error response".to_string())
}

/// Convert a provider status to a typed, value-free diagnostic.
pub(crate) fn status_error(status: u16, retry_after: Option<std::time::Duration>) -> ModelError {
    match status {
        401 | 403 => ModelError::auth("provider authentication failed"),
        404 => ModelError::NotFound("provider resource was not found".to_string()),
        429 => ModelError::rate_limited(retry_after),
        _ => ModelError::http(status, "provider returned an error response"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;
    use std::time::Duration;

    async fn chunked_response(chunks: Vec<Vec<u8>>, hold_open: bool) -> reqwest::Response {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
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
        reqwest::Client::builder()
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

    async fn fixed_response(body: Vec<u8>) -> reqwest::Response {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request);
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(&body).unwrap();
        });
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .unwrap()
            .get(format!("http://{address}"))
            .send()
            .await
            .unwrap()
    }

    #[cfg(any(
        feature = "antigravity",
        feature = "anthropic",
        feature = "cohere",
        feature = "google",
        feature = "huggingface",
        feature = "mistral",
        feature = "ollama",
        feature = "openai"
    ))]
    #[tokio::test]
    async fn malformed_success_bodies_do_not_disclose_payload_bytes() {
        let marker = "private-malformed-json-marker";
        let response = fixed_response(format!("{{invalid:{marker}}}").into_bytes()).await;
        let error = json::<serde_json::Value>(response).await.unwrap_err();
        assert!(!error.to_string().contains(marker));
    }

    #[test]
    fn status_errors_are_typed_and_value_free() {
        assert!(matches!(
            status_error(401, None),
            ModelError::Authentication(_)
        ));
        assert!(matches!(
            status_error(403, None),
            ModelError::Authentication(_)
        ));
        assert!(matches!(status_error(404, None), ModelError::NotFound(_)));
        assert!(matches!(
            status_error(429, Some(Duration::from_secs(3))),
            ModelError::RateLimited { retry_after: Some(delay) } if delay == Duration::from_secs(3)
        ));
        let diagnostic = status_error(500, None).to_string();
        assert_eq!(diagnostic, "Model HTTP error (status 500)");
    }

    #[tokio::test]
    async fn bounded_reader_accepts_fixed_length_exact_cap_and_rejects_plus_one() {
        let response = fixed_response(b"abcd".to_vec()).await;
        assert_eq!(read_bounded(response, 4).await.unwrap(), b"abcd");

        let response = fixed_response(b"abcde".to_vec()).await;
        assert!(matches!(
            read_bounded(response, 4).await,
            Err(ModelError::ResponseTooLarge { limit: 4 })
        ));
    }

    #[tokio::test]
    async fn bounded_reader_accepts_exact_chunk_boundary_and_rejects_plus_one() {
        let response = chunked_response(vec![b"ab".to_vec(), b"cd".to_vec()], false).await;
        assert_eq!(read_bounded(response, 4).await.unwrap(), b"abcd");

        let response = chunked_response(vec![b"abcd".to_vec(), b"e".to_vec()], false).await;
        assert!(matches!(
            read_bounded(response, 4).await,
            Err(ModelError::ResponseTooLarge { limit: 4 })
        ));
    }

    #[tokio::test]
    async fn bounded_reader_times_out_on_never_ending_body() {
        let response = chunked_response(vec![b"partial".to_vec()], true).await;
        assert!(matches!(
            read_bounded(response, 64).await,
            Err(ModelError::Timeout(_))
        ));
    }

    #[tokio::test]
    async fn error_diagnostics_reject_invalid_utf8_and_never_echo_valid_body() {
        let response = chunked_response(vec![vec![0xff]], false).await;
        assert!(matches!(
            error_text(response).await,
            Err(ModelError::InvalidResponse(_))
        ));

        let response = chunked_response(vec![b"Bearer secret-marker".to_vec()], false).await;
        let diagnostic = error_text(response).await.unwrap();
        assert!(!diagnostic.contains("secret-marker"));
    }

    #[tokio::test]
    async fn bounded_stream_accepts_exact_cap_and_rejects_cross_chunk_plus_one() {
        use futures::StreamExt as _;

        let response = chunked_response(vec![b"ab".to_vec(), b"cd".to_vec()], false).await;
        let items = stream_bounded(response, 4).collect::<Vec<_>>().await;
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].as_ref().unwrap().as_ref(), b"ab");
        assert_eq!(items[1].as_ref().unwrap().as_ref(), b"cd");

        let response = chunked_response(vec![b"abc".to_vec(), b"de".to_vec()], false).await;
        let items = stream_bounded(response, 4).collect::<Vec<_>>().await;
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].as_ref().unwrap().as_ref(), b"abc");
        assert!(matches!(
            items[1],
            Err(ModelError::ResponseTooLarge { limit: 4 })
        ));
    }

    #[tokio::test]
    async fn bounded_stream_times_out_once_and_terminates() {
        use futures::StreamExt as _;

        let response = chunked_response(vec![b"partial".to_vec()], true).await;
        let items = stream_bounded(response, 64).collect::<Vec<_>>().await;
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].as_ref().unwrap().as_ref(), b"partial");
        assert!(matches!(items[1], Err(ModelError::Timeout(_))));
    }

    #[test]
    fn utf8_decoder_accepts_split_codepoint_and_rejects_malformed_or_incomplete_data() {
        let mut decoder = Utf8StreamDecoder::default();
        let mut output = String::new();
        decoder.push(&[0xe2], &mut output).unwrap();
        decoder.push(&[0x82, 0xac], &mut output).unwrap();
        decoder.finish().unwrap();
        assert_eq!(output, "€");

        let mut decoder = Utf8StreamDecoder::default();
        assert!(matches!(
            decoder.push(&[0xff], &mut String::new()),
            Err(ModelError::InvalidResponse(_))
        ));

        let mut decoder = Utf8StreamDecoder::default();
        decoder.push(&[0xe2], &mut String::new()).unwrap();
        assert!(matches!(
            decoder.finish(),
            Err(ModelError::InvalidResponse(_))
        ));
    }

    #[test]
    fn utf8_decoder_enforces_unprocessed_stream_buffer_cap() {
        let mut decoder = Utf8StreamDecoder::default();
        let mut output = String::new();
        decoder
            .push(&vec![b'a'; MAX_STREAM_BUFFER_BYTES], &mut output)
            .unwrap();
        assert_eq!(output.len(), MAX_STREAM_BUFFER_BYTES);

        let mut decoder = Utf8StreamDecoder::default();
        let error = decoder
            .push(&vec![b'a'; MAX_STREAM_BUFFER_BYTES + 1], &mut String::new())
            .unwrap_err();
        assert!(matches!(
            error,
            ModelError::ResponseTooLarge {
                limit: MAX_STREAM_BUFFER_BYTES
            }
        ));

        for (pending, completion) in [
            (vec![0xc2], 0xa2),
            (vec![0xe2, 0x82], 0xac),
            (vec![0xf0, 0x9f, 0x92], 0xa9),
        ] {
            let remaining = MAX_STREAM_BUFFER_BYTES - pending.len();
            let mut exact = vec![b'a'; remaining];
            exact[0] = completion;
            let mut decoder = Utf8StreamDecoder::default();
            decoder.push(&pending, &mut String::new()).unwrap();
            decoder.push(&exact, &mut String::new()).unwrap();

            exact.push(b'a');
            let mut decoder = Utf8StreamDecoder::default();
            decoder.push(&pending, &mut String::new()).unwrap();
            assert!(matches!(
                decoder.push(&exact, &mut String::new()),
                Err(ModelError::ResponseTooLarge {
                    limit: MAX_STREAM_BUFFER_BYTES
                })
            ));
        }
    }
}
