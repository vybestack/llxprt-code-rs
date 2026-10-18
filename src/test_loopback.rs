//! Shared cancellable loopback support. Fixture lifetime follows the client, not machine speed.
use serde_json::Value;
use std::io::{self, BufRead, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

// A polling interval, NOT a deadline: shutdown wakes the worker immediately.
const POLL: Duration = Duration::from_millis(5);

struct RunningChild(Child);

impl Drop for RunningChild {
    fn drop(&mut self) {
        // Also reap on early I/O errors or unwinding; no fixture child survives a test.
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn stopped() -> io::Error {
    io::Error::new(
        io::ErrorKind::ConnectionAborted,
        "CLI exited before scripted reply",
    )
}

pub fn retry<T>(
    stop: &Receiver<()>,
    mut operation: impl FnMut() -> io::Result<T>,
) -> io::Result<T> {
    loop {
        match stop.try_recv() {
            Err(mpsc::TryRecvError::Empty) => {}
            _ => return Err(stopped()),
        }
        match operation() {
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                match stop.recv_timeout(POLL) {
                    Err(RecvTimeoutError::Timeout) => {}
                    _ => return Err(stopped()),
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => return result,
        }
    }
}

#[derive(Debug)]
pub struct Connection(TcpStream, std::sync::Arc<std::sync::Mutex<Receiver<()>>>);

impl Connection {
    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self(self.0.try_clone()?, self.1.clone()))
    }
}

impl std::ops::Deref for Connection {
    type Target = TcpStream;
    fn deref(&self) -> &TcpStream {
        &self.0
    }
}

impl Read for Connection {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        retry(&self.1.lock().unwrap(), || self.0.read(bytes))
    }
}

impl Write for Connection {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        retry(&self.1.lock().unwrap(), || self.0.write(bytes))
    }

    fn flush(&mut self) -> io::Result<()> {
        retry(&self.1.lock().unwrap(), || self.0.flush())
    }
}

fn exchange(
    stream: TcpStream,
    reply: &Value,
    stop: &std::sync::Arc<std::sync::Mutex<Receiver<()>>>,
) -> Result<Value, String> {
    stream.set_nonblocking(true).map_err(|e| e.to_string())?;
    let mut stream = Connection(stream, stop.clone());
    let mut reader = io::BufReader::new(&mut stream);
    let mut length = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
            return Err("request ended before HTTP headers".into());
        }
        if line == "\r\n" {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            if key.eq_ignore_ascii_case("content-length") {
                length = Some(value.trim().parse::<usize>().map_err(|e| e.to_string())?);
            }
        }
    }
    let mut body = vec![0; length.ok_or("request missing content-length")?];
    reader.read_exact(&mut body).map_err(|e| e.to_string())?;
    let captured = serde_json::from_slice(&body).map_err(|e| format!("request JSON: {e}"))?;
    let body = reply.to_string();
    write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body)
        .map_err(|e| format!("write reply: {e}"))?;
    Ok(captured)
}

fn serve(
    listener: TcpListener,
    replies: Vec<Value>,
    stop: Receiver<()>,
) -> Result<Vec<Value>, String> {
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let stop = std::sync::Arc::new(std::sync::Mutex::new(stop));
    replies
        .iter()
        .enumerate()
        .map(|(index, reply)| {
            let request = || {
                let (stream, _) = retry(&stop.lock().unwrap(), || listener.accept())
                    .map_err(|e| e.to_string())?;
                exchange(stream, reply, &stop)
            };
            request().map_err(|e| format!("scripted reply {}/{}: {e}", index + 1, replies.len()))
        })
        .collect()
}

/// Drain both child pipes concurrently. Supervise the fixture while the child is
/// live, kill/reap on a server error, and cancel accept/read/write when it exits.
pub fn run(
    listener: TcpListener,
    replies: Vec<Value>,
    command: &mut Command,
) -> Result<(Output, Vec<Value>), String> {
    supervise(command, move |stop, _| serve(listener, replies, stop))
}

fn supervise<T: Send>(
    command: &mut Command,
    server: impl FnOnce(Receiver<()>, u32) -> Result<T, String> + Send,
) -> Result<(Output, T), String> {
    let mut child = RunningChild(
        command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?,
    );
    let mut stdout = child.0.stdout.take().ok_or("missing CLI stdout")?;
    let mut stderr = child.0.stderr.take().ok_or("missing CLI stderr")?;
    std::thread::scope(|scope| {
        let capture = |stream: &mut dyn Read| {
            let mut bytes = Vec::new();
            stream
                .read_to_end(&mut bytes)
                .map(|_| bytes)
                .map_err(|e| e.to_string())
        };
        let stdout = scope.spawn(move || capture(&mut stdout));
        let stderr = scope.spawn(move || capture(&mut stderr));
        let (stop, stopped) = mpsc::channel();
        let child_id = child.0.id();
        let mut server = Some(scope.spawn(move || server(stopped, child_id)));
        let mut served = None;
        // Do not use `?` until the worker is cancelled and joined.
        let status = loop {
            match child.0.try_wait() {
                Ok(Some(status)) => break Ok(status),
                Err(error) => break Err(error.to_string()),
                Ok(None) => {}
            }
            if server.as_ref().is_some_and(|worker| worker.is_finished()) {
                served = Some(server.take().unwrap().join());
                if !matches!(served, Some(Ok(Ok(_)))) {
                    let _ = child.0.kill();
                    break child.0.wait().map_err(|e| e.to_string());
                }
            }
            std::thread::sleep(POLL);
        };
        drop(stop);
        if status.is_err() {
            let _ = child.0.kill();
            let _ = child.0.wait();
        }
        let served = match server {
            Some(worker) => worker.join(),
            None => served.unwrap(),
        };
        let stdout = stdout.join().map_err(|_| "stdout reader panicked")??;
        let stderr = stderr.join().map_err(|_| "stderr reader panicked")??;
        let output = Output {
            status: status?,
            stdout,
            stderr,
        };
        let captured = served
            .map_err(|_| format!("scripted server panicked; CLI {output:?}"))?
            .map_err(|error| format!("scripted server failed: {error}; CLI {output:?}"))?;
        Ok((output, captured))
    })
}

/// A cancellable listener. Nonblocking mode is a fixture polling choice, never
/// a deadline. Accepted sockets always use the same cancellation owner.
pub struct Listener {
    socket: TcpListener,
    stop: std::sync::Arc<std::sync::Mutex<Receiver<()>>>,
    nonblocking: std::cell::Cell<bool>,
}
impl Listener {
    pub fn set_nonblocking(&self, value: bool) -> io::Result<()> {
        self.nonblocking.set(value);
        Ok(())
    }
    pub fn expect_no_more_requests(&self) {
        let error = self
            .accept()
            .expect_err("unexpected request after scripted replies");
        assert_eq!(error.kind(), io::ErrorKind::ConnectionAborted);
    }
    pub fn until_client_exits(&self) {
        let _ = self.stop.lock().unwrap().recv();
    }
    pub fn accept(&self) -> io::Result<(Connection, std::net::SocketAddr)> {
        let accept = || {
            let (socket, address) = self.socket.accept()?;
            socket.set_nonblocking(true)?;
            Ok((Connection(socket, self.stop.clone()), address))
        };
        if self.nonblocking.get() {
            accept()
        } else {
            retry(&self.stop.lock().unwrap(), accept)
        }
    }
}

/// Owned server thread: cancellation on join/unwind interrupts accept and all
/// buffered header/body reads and writes. No worker survives the fixture.
pub struct Server<T> {
    stop: Option<mpsc::Sender<()>>,
    worker: Option<std::thread::JoinHandle<T>>,
}
pub fn spawn<T: Send + 'static>(
    listener: TcpListener,
    serve: impl FnOnce(Listener) -> T + Send + 'static,
) -> Server<T> {
    listener.set_nonblocking(true).unwrap();
    let (stop, stopped) = mpsc::channel();
    let listener = Listener {
        socket: listener,
        stop: std::sync::Arc::new(std::sync::Mutex::new(stopped)),
        nonblocking: std::cell::Cell::new(false),
    };
    Server {
        stop: Some(stop),
        worker: Some(std::thread::spawn(move || serve(listener))),
    }
}
impl<T> Server<T> {
    pub fn join(mut self) -> std::thread::Result<T> {
        self.stop.take();
        match self.worker.take().unwrap().join() {
            Ok(result) => Ok(result),
            Err(panic) => std::panic::resume_unwind(panic),
        }
    }
}
impl<T> Drop for Server<T> {
    fn drop(&mut self) {
        self.stop.take();
        if let Some(worker) = self.worker.take() {
            if let Err(panic) = worker.join() {
                if !std::thread::panicking() {
                    std::panic::resume_unwind(panic);
                }
            }
        }
    }
}

/// Supervise a compiled client and an arbitrary scripted peer. A server panic
/// kills/reaps the client and retains the original message alongside its output.
pub fn run_listener<T: Send>(
    listener: TcpListener,
    command: &mut Command,
    serve: impl FnOnce(Listener, u32) -> T + Send,
) -> Result<(Output, T), String> {
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    supervise(command, move |stop, id| {
        let listener = Listener {
            socket: listener,
            stop: std::sync::Arc::new(std::sync::Mutex::new(stop)),
            nonblocking: std::cell::Cell::new(false),
        };
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| serve(listener, id))).map_err(
            |panic| {
                panic
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_else(|| "non-string fixture panic".into())
            },
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn child_exit_cancels_accept_and_reports_missing_reply_with_cli_output() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let error = run(
            listener,
            vec![json!({})],
            Command::new("/bin/sh")
                .args(["-c", "printf fixture-out; printf fixture-err >&2; exit 7"]),
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
            |_stop, _id| Err::<Vec<Value>, _>("injected server failure at reply 2/3".into()),
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
                let mut reader = io::BufReader::new(Connection(
                    stream,
                    std::sync::Arc::new(std::sync::Mutex::new(stopped)),
                ));
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
        |_stop, _id| Ok(Vec::<Value>::new()),
    ).unwrap();
        assert_eq!(output.status.code(), Some(7));
        assert_eq!(output.stdout.len(), 131072);
        assert_eq!(output.stderr.len(), 131072);
        assert!(requests.is_empty());
    }

    #[test]
    fn owned_server_cancels_accept_and_preserves_original_panic() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let server = spawn(listener, |listener| {
            let error = listener.accept().unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::ConnectionAborted);
            listener.until_client_exits();
            listener.expect_no_more_requests();
            "accept cancelled"
        });
        assert_eq!(server.join().unwrap(), "accept cancelled");
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let failure = std::panic::catch_unwind(|| {
            spawn(listener, |_| panic!("original fixture failure"))
                .join()
                .unwrap();
        })
        .unwrap_err();
        assert_eq!(
            failure.downcast_ref::<&str>(),
            Some(&"original fixture failure")
        );
    }

    #[test]
    fn owned_server_cancels_cloned_body_reader_and_nonblocking_checks_still_work() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut peer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        peer.write_all(b"partial").unwrap();
        let (ready, waiting) = mpsc::channel();
        let server = spawn(listener, move |listener| {
            let (socket, _) = listener.accept().unwrap();
            let mut reader = io::BufReader::new(socket.try_clone().unwrap());
            reader.read_exact(&mut [0; 7]).unwrap();
            listener.set_nonblocking(true).unwrap();
            assert_eq!(
                listener.accept().unwrap_err().kind(),
                io::ErrorKind::WouldBlock
            );
            ready.send(()).unwrap();
            let error = reader.read_exact(&mut [0; 1]).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::ConnectionAborted);
        });
        waiting.recv().unwrap();
        server.join().unwrap();
        drop(peer);
    }

    #[test]
    fn arbitrary_script_failure_supervises_the_client_with_original_message() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let error = run_listener(
            listener,
            Command::new("/bin/sh")
                .args(["-c", "read input"])
                .stdin(Stdio::piped()),
            |_, _| panic!("original arbitrary script failure"),
        )
        .unwrap_err();
        assert!(
            error.contains("original arbitrary script failure"),
            "{error}"
        );
    }
    #[test]
    fn client_unwind_cancels_a_partial_request_and_joins_the_server() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut peer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        peer.write_all(b"partial header").unwrap();
        let (ready, waiting) = mpsc::channel();
        let (cleaned, cleanup) = mpsc::channel();
        let failure = std::panic::catch_unwind(|| {
            let _server = spawn(listener, move |listener| {
                let (socket, _) = listener.accept().unwrap();
                let mut reader = io::BufReader::new(socket);
                reader.read_exact(&mut [0; 14]).unwrap();
                ready.send(()).unwrap();
                let error = reader.read_until(b'\n', &mut Vec::new()).unwrap_err();
                assert_eq!(error.kind(), io::ErrorKind::ConnectionAborted);
                cleaned.send(()).unwrap();
            });
            waiting.recv().unwrap();
            panic!("original client failure");
        })
        .unwrap_err();
        assert_eq!(
            failure.downcast_ref::<&str>(),
            Some(&"original client failure")
        );
        cleanup.recv().unwrap(); // Drop joined; no detached request reader remains.
        drop(peer);
    }

    #[test]
    fn cancellation_interrupts_a_backpressured_writer() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let peer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (ready, waiting) = mpsc::channel();
        let server = spawn(listener, move |listener| {
            let (mut socket, _) = listener.accept().unwrap();
            let bytes = [0; 8192];
            // Fill the nonblocking OS buffer before testing the cancellable writer.
            // No timing threshold or sleep is needed to establish backpressure.
            loop {
                match (&*socket).write(&bytes) {
                    Ok(_) => (),
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => (),
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                    Err(error) => panic!("fill send buffer: {error}"),
                }
            }
            ready.send(()).unwrap();
            loop {
                match socket.write_all(&bytes) {
                    Ok(()) => (),
                    Err(error) => {
                        assert_eq!(error.kind(), io::ErrorKind::ConnectionAborted);
                        break;
                    }
                }
            }
        });
        waiting.recv().unwrap();
        server.join().unwrap();
        drop(peer);
    }
    #[test]
    fn returned_listener_can_check_the_socket_queue_after_cancellation() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let listener = spawn(listener, |listener| listener).join().unwrap();
        listener.set_nonblocking(true).unwrap();
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        let _peer = TcpStream::connect(address).unwrap();
        // Polling a returned listener must still detect a real unexpected send,
        // not mistake its disconnected cancellation owner for an empty queue.
        let accepted = loop {
            match listener.accept() {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => std::thread::yield_now(),
                result => break result,
            }
        };
        assert!(accepted.is_ok());
    }
}
