use std::collections::HashMap;
use std::io::{BufRead, Read};
use std::io::Write;
use std::net::SocketAddr;
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};
use std::path::PathBuf;

use slog::debug;
use slog::error;

const MAX_REQUEST_LINE_BYTES: usize = 8192;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

// A per-read socket timeout alone can be extended indefinitely by a slow sender.
// This deadline is owned by one request and never changes a listener's state.
struct RequestReader {
    stream: TcpStream,
    deadline: Instant,
}

fn remaining_timeout(deadline: Instant, now: Instant) -> std::io::Result<Duration> {
    deadline.checked_duration_since(now).filter(|left| !left.is_zero())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::TimedOut, "request deadline exceeded"))
}

impl Read for RequestReader {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.stream.set_read_timeout(Some(remaining_timeout(self.deadline, Instant::now())?))?;
        self.stream.read(buffer)
    }
}

fn read_request_line(reader: &mut impl BufRead) -> std::io::Result<String> {
    let mut line = String::new();
    // The extra byte distinguishes an exactly full line from an oversized one.
    reader.take((MAX_REQUEST_LINE_BYTES + 1) as u64).read_line(&mut line)?;
    if line.len() > MAX_REQUEST_LINE_BYTES {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "request line too long"));
    }
    if !line.ends_with('\n') {
        return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "incomplete request line"));
    }
    Ok(line)
}

fn receive_request(logger: &slog::Logger, stream: &mut TcpStream) -> Option<String> {
    let result = (|| {
        stream.set_write_timeout(Some(REQUEST_TIMEOUT))?;
        let reader = RequestReader { stream: stream.try_clone()?, deadline: Instant::now() + REQUEST_TIMEOUT };
        read_request_line(&mut std::io::BufReader::new(reader))
    })();
    match result {
        Ok(line) => Some(line),
        Err(error) => {
            error!(logger, "simple_http: request error: {:?}", error);
            let status: &[u8] = match error.kind() {
                std::io::ErrorKind::InvalidInput => b"HTTP/1.0 414 URI Too Long\r\n\r\n",
                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => b"HTTP/1.0 408 Request Timeout\r\n\r\n",
                _ => b"HTTP/1.0 400 Bad Request\r\n\r\n",
            };
            // A broken peer (including setup/write failure) must not stop a service.
            let _ = stream.write_all(status);
            None
        }
    }
}

/// Serves a single file over HTTP.
///
/// This function binds to the given address and serves the provided content
/// to any incoming HTTP request.
pub fn serve(logger: &slog::Logger, addr: SocketAddr, content: &str) -> std::io::Result<()> {
    let listener = TcpListener::bind(addr)?;
    crate::lan_runtime::mark_ready(1);
    let resp = format!(
        "HTTP/1.0 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\n\r\n{}",
        content.len(),
        content
    );
    let resp = resp.as_bytes();
    loop {
        let (mut stream, _addr) = listener.accept()?;
        let Some(path) = receive_request(logger, &mut stream) else { continue; };
        debug!(logger, "Request: {}", path);
        if let Err(e) = stream.write_all(resp) {
            error!(logger, "write error: {:?}", e);
        }
    }
}

/// Serves multiple files over HTTP.
///
/// This function binds to the given address and serves files from the provided
/// `files` map. The keys of the map are the request paths, and the values are
/// the paths to the files on disk.
pub fn serve_many(logger: &slog::Logger, addr: SocketAddr, files: &HashMap<String, PathBuf>) -> std::io::Result<()> {
    let listener = TcpListener::bind(addr)?;
    crate::lan_runtime::mark_ready(2);
    loop {
        let (mut stream, _addr) = listener.accept()?;
        let Some(path) = receive_request(logger, &mut stream) else { continue; };
        debug!(logger, "Request: {}", path);
        let prefix = "GET ";
        let suffix = " HTTP/1.1\r\n";
        if !path.starts_with(prefix) {
            debug!(logger, "Status 405");
            if let Err(e) = stream.write_all(b"HTTP/1.0 405 Method Not Allowed\r\n\r\n") {
                error!(logger, "simple_http: write error: {:?}", e);
            }
            continue;
        }
        if !path.ends_with(suffix) {
            debug!(logger, "Status 400");
            if let Err(e) = stream.write_all(b"HTTP/1.0 400 Bad Request\r\n\r\n") {
                error!(logger, "simple_http: write error: {:?}", e);
            }
            continue;
        }
        let path = &path[prefix.len()..(path.len() - suffix.len())];
        if let Some(path) = files.get(path) {
            let data = match std::fs::read(path) {
                Ok(data) => data,
                Err(error) => {
                    error!(logger, "simple_http: configured content unavailable: {:?}", error);
                    let _ = stream.write_all(b"HTTP/1.0 500 Internal Server Error\r\n\r\n");
                    continue;
                }
            };

            let resp = format!("HTTP/1.0 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\n\r\n", data.len(),);
            debug!(logger, "Status 200");
            let mut resp = resp.into_bytes();
            resp.extend(data);
            if let Err(e) = stream.write_all(&resp) {
                error!(logger, "simple_http: write error: {:?}", e);
            }
            continue;
        }

        if let Err(e) = stream.write_all(b"HTTP/1.0 404 Not Found\r\n\r\n") {
            error!(logger, "simple_http: write error: {:?}", e);
            continue;
        }
        debug!(logger, "Status 404");
    }
}

#[cfg(test)]
#[path = "simple_http_tests.rs"]
mod tests;
