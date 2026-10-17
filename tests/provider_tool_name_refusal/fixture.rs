//! Scripted HTTP lifetime is bounded by the child, not by machine speed.
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

fn retry<T>(stop: &Receiver<()>, mut operation: impl FnMut() -> io::Result<T>) -> io::Result<T> {
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

struct Connection<'a>(TcpStream, &'a Receiver<()>);

impl Read for Connection<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        retry(self.1, || self.0.read(bytes))
    }
}

impl Write for Connection<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        retry(self.1, || self.0.write(bytes))
    }

    fn flush(&mut self) -> io::Result<()> {
        retry(self.1, || self.0.flush())
    }
}

fn exchange(stream: TcpStream, reply: &Value, stop: &Receiver<()>) -> Result<Value, String> {
    stream.set_nonblocking(true).map_err(|e| e.to_string())?;
    let mut stream = Connection(stream, stop);
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
    replies
        .iter()
        .enumerate()
        .map(|(index, reply)| {
            let request = || {
                let (stream, _) = retry(&stop, || listener.accept()).map_err(|e| e.to_string())?;
                exchange(stream, reply, &stop)
            };
            request().map_err(|e| format!("scripted reply {}/{}: {e}", index + 1, replies.len()))
        })
        .collect()
}

/// Drain both child pipes concurrently. Supervise the fixture while the child is
/// live, kill/reap on a server error, and cancel accept/read/write when it exits.
pub(super) fn run(
    listener: TcpListener,
    replies: Vec<Value>,
    command: &mut Command,
) -> Result<(Output, Vec<Value>), String> {
    supervise(command, move |stop| serve(listener, replies, stop))
}

fn supervise(
    command: &mut Command,
    server: impl FnOnce(Receiver<()>) -> Result<Vec<Value>, String> + Send,
) -> Result<(Output, Vec<Value>), String> {
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
        let mut server = Some(scope.spawn(move || server(stopped)));
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

#[cfg(test)]
#[path = "fixture_tests.rs"]
mod tests;
