use super::*;
use serde_json::json;

#[test]
fn child_exit_cancels_accept_and_reports_missing_reply_with_cli_output() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let error = run(
        listener,
        vec![json!({})],
        Command::new("/bin/sh").args(["-c", "printf fixture-out; printf fixture-err >&2; exit 7"]),
    )
    .unwrap_err();
    assert!(error.contains("scripted reply 1/1"), "{error}");
    assert!(
        error.contains("CLI exited before scripted reply"),
        "{error}"
    );
    assert!(error.contains("fixture-out"), "{error}");
    assert!(error.contains("fixture-err"), "{error}");
}

#[test]
fn server_failure_kills_and_reaps_live_child_without_hiding_the_cause() {
    let error = supervise(
        Command::new("/bin/sh")
            .args(["-c", "read fixture_input"])
            .stdin(Stdio::piped()),
        |_stop| Err("injected server failure at reply 2/3".into()),
    )
    .unwrap_err();
    assert!(error.contains("scripted server failed"), "{error}");
    assert!(
        error.contains("injected server failure at reply 2/3"),
        "{error}"
    );
}

#[test]
fn child_exit_cancels_buffered_line_and_exact_body_reads() {
    for body in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (stream, _) = listener.accept().unwrap();
        stream.set_nonblocking(true).unwrap();
        client.write_all(b"partial").unwrap();
        let (stop, stopped) = mpsc::channel();
        let (ready, read) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut reader = io::BufReader::new(Connection(stream, &stopped));
            let mut prefix = [0; 7];
            reader.read_exact(&mut prefix).unwrap();
            assert_eq!(&prefix, b"partial");
            ready.send(()).unwrap();
            if body {
                reader.read_exact(&mut [0; 1]).unwrap_err()
            } else {
                reader.read_line(&mut String::new()).unwrap_err()
            }
        });
        read.recv().unwrap();
        // The request is partially consumed, and the peer stays open: neither
        // cancellation before accept nor EOF can satisfy this regression test.
        drop(stop);
        let error = worker.join().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::ConnectionAborted);
        assert!(error
            .to_string()
            .contains("CLI exited before scripted reply"));
        drop(client);
    }
}

#[test]
fn malformed_request_is_a_script_error_not_a_later_connectivity_failure() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    client
        .write_all(b"POST / HTTP/1.1\r\nContent-Length: 1\r\n\r\nx")
        .unwrap();
    let (_stop, stopped) = mpsc::channel();
    let error = serve(listener, vec![json!({})], stopped).unwrap_err();
    assert!(
        error.contains("scripted reply 1/1: request JSON:"),
        "{error}"
    );
}

#[test]
fn complete_script_still_drains_large_stdout_and_stderr_and_checks_exit() {
    let (output, requests) = supervise(
        Command::new("/bin/sh").args(["-c", "dd if=/dev/zero bs=65536 count=2 2>/dev/null; dd if=/dev/zero bs=65536 count=2 >&2 2>/dev/null; exit 7"]),
        |_stop| Ok(vec![]),
    ).unwrap();
    assert_eq!(output.status.code(), Some(7));
    assert_eq!(output.stdout.len(), 131072);
    assert_eq!(output.stderr.len(), 131072);
    assert!(requests.is_empty());
}
