use super::super::*;
use super::{invalid_message, parse_test, strict_stream, test_limits};
use serdes_ai_core::ModelResponsePart;

#[test]
fn sse_frame_and_event_limits_accept_exact_and_reject_plus_one() {
    let stream = strict_stream("ok", None, &[]);
    let first_end = stream.find("\r\n\r\n").unwrap();
    let payload = &stream[6..first_end];
    let frame_limit = stream
        .split("\r\n")
        .filter_map(|line| line.strip_prefix("data: "))
        .map(str::len)
        .max()
        .unwrap();
    let padding = frame_limit - payload.len();
    let first = format!("data: {payload}{}\r\n", " ".repeat(padding));
    let terminator = "\r\n";
    let rest = &stream[first_end + 4..];
    let mut limits = test_limits();
    limits.frame = frame_limit;
    super::super::sse::parse_chunks_with_limits(
        &[first.as_bytes(), terminator.as_bytes(), rest.as_bytes()],
        limits,
    )
    .unwrap();

    let oversized = format!("data: {payload}{}\r\n\r\n{rest}", " ".repeat(padding + 1));
    assert_eq!(
        invalid_message(parse_test(&oversized, limits).unwrap_err()),
        FRAME_LIMIT_ERROR
    );

    let event_count = stream.matches("data: {").count();
    limits = test_limits();
    limits.events = event_count;
    parse_test(&stream, limits).unwrap();
    limits.events -= 1;
    assert_eq!(
        invalid_message(parse_test(&stream, limits).unwrap_err()),
        EVENT_LIMIT_ERROR
    );
}

#[test]
fn sse_text_and_summary_limits_accept_exact_and_reject_plus_one() {
    let mut limits = test_limits();
    limits.text = 4;
    parse_test(&strict_stream("abcd", None, &[]), limits).unwrap();
    assert_eq!(
        invalid_message(parse_test(&strict_stream("abcde", None, &[]), limits).unwrap_err()),
        TEXT_LIMIT_ERROR
    );

    limits = test_limits();
    limits.summary = 4;
    parse_test(&strict_stream("", Some("abcd"), &[]), limits).unwrap();
    assert_eq!(
        invalid_message(parse_test(&strict_stream("", Some("abcde"), &[]), limits).unwrap_err()),
        SUMMARY_LIMIT_ERROR
    );
}

#[test]
fn sse_argument_and_call_limits_are_independent_at_boundaries() {
    let mut limits = test_limits();
    limits.arguments_per_call = 4;
    parse_test(&strict_stream("", None, &["abcd"]), limits).unwrap();
    assert_eq!(
        invalid_message(parse_test(&strict_stream("", None, &["abcde"]), limits).unwrap_err()),
        CALL_ARGUMENT_LIMIT_ERROR
    );

    limits = test_limits();
    limits.arguments_per_call = 4;
    limits.arguments_total = 6;
    parse_test(&strict_stream("", None, &["abc", "def"]), limits).unwrap();
    assert_eq!(
        invalid_message(
            parse_test(&strict_stream("", None, &["abc", "defg"]), limits).unwrap_err()
        ),
        TOTAL_ARGUMENT_LIMIT_ERROR
    );

    limits = test_limits();
    limits.calls = 2;
    parse_test(&strict_stream("", None, &["a", "b"]), limits).unwrap();
    assert_eq!(
        invalid_message(
            parse_test(&strict_stream("", None, &["a", "b", "c"]), limits).unwrap_err()
        ),
        CALL_LIMIT_ERROR
    );
}

#[test]
fn sse_subdivides_oversized_chunks_and_decodes_utf8_across_internal_slices() {
    let mut small_frames = ": keepalive\r\n\r\n"
        .repeat(crate::response::MAX_STREAM_BUFFER_BYTES / ": keepalive\r\n\r\n".len() + 1);
    small_frames.push_str(&strict_stream("small", None, &[]));
    assert!(small_frames.len() > crate::response::MAX_STREAM_BUFFER_BYTES);
    let response = parse_test(&small_frames, ParserLimits::default()).unwrap();
    assert!(matches!(&response.parts[0], ModelResponsePart::Text(text) if text.content == "small"));

    let slice_limit = crate::response::MAX_STREAM_BUFFER_BYTES - 3;
    let mut bytes = vec![b' '; slice_limit - 1];
    bytes[0] = b':';
    bytes.extend_from_slice("€\r\n\r\n".as_bytes());
    bytes.extend_from_slice(strict_stream("ok", None, &[]).as_bytes());
    assert!(bytes.len() > crate::response::MAX_STREAM_BUFFER_BYTES);
    let response =
        super::super::sse::parse_chunks_with_limits(&[&bytes], ParserLimits::default()).unwrap();
    assert!(matches!(&response.parts[0], ModelResponsePart::Text(text) if text.content == "ok"));
}
