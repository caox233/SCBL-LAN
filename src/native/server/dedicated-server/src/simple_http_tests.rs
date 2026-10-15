use super::*;
use std::io::{Cursor, ErrorKind};

#[test]
fn native_request_bytes_are_preserved() {
    for line in ["GET /data/mp_balancing.ini HTTP/1.1\r\n", "GET / HTTP/1.0\r\n", "GET /\n"] {
        assert_eq!(read_request_line(&mut Cursor::new(line)).unwrap(), line);
    }
}

#[test]
fn request_line_exact_limit_and_one_byte_over() {
    let mut exact = vec![b'x'; MAX_REQUEST_LINE_BYTES - 2];
    exact.extend_from_slice(b"\r\n");
    assert_eq!(read_request_line(&mut Cursor::new(&exact)).unwrap().as_bytes(), exact);
    let mut oversized = vec![b'x'; MAX_REQUEST_LINE_BYTES + 1];
    oversized.extend_from_slice(b"\r\n");
    let mut input = Cursor::new(oversized);
    assert_eq!(read_request_line(&mut input).unwrap_err().kind(), ErrorKind::InvalidInput);
    assert_eq!(input.position(), (MAX_REQUEST_LINE_BYTES + 1) as u64);
}

#[test]
fn large_unterminated_line_is_not_fully_consumed() {
    let mut input = Cursor::new(vec![b'x'; 1024 * 1024]);
    assert_eq!(read_request_line(&mut input).unwrap_err().kind(), ErrorKind::InvalidInput);
    assert_eq!(input.position(), (MAX_REQUEST_LINE_BYTES + 1) as u64);
}

#[test]
fn empty_or_incomplete_request_is_not_a_valid_config_fetch() {
    for input in [b"".as_slice(), b"GET / HTTP/1.1\r".as_slice()] {
        assert_eq!(read_request_line(&mut Cursor::new(input)).unwrap_err().kind(), ErrorKind::UnexpectedEof);
    }
}

#[test]
fn invalid_utf8_is_rejected_without_panicking() {
    assert_eq!(read_request_line(&mut Cursor::new(b"GET /\xff HTTP/1.1\r\n")).unwrap_err().kind(), ErrorKind::InvalidData);
}

#[test]
fn request_deadline_never_resets_per_read() {
    let start = Instant::now();
    let deadline = start + REQUEST_TIMEOUT;
    assert_eq!(remaining_timeout(deadline, start).unwrap(), REQUEST_TIMEOUT);
    assert_eq!(remaining_timeout(deadline, start + Duration::from_millis(1500)).unwrap(), Duration::from_millis(500));
    assert_eq!(remaining_timeout(deadline, deadline).unwrap_err().kind(), ErrorKind::TimedOut);
    assert_eq!(remaining_timeout(deadline, deadline + Duration::from_nanos(1)).unwrap_err().kind(), ErrorKind::TimedOut);
}

#[test]
fn io_errors_are_not_reinterpreted_as_valid_lines() {
    struct Broken;
    impl Read for Broken {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> { Err(ErrorKind::ConnectionReset.into()) }
    }
    impl BufRead for Broken {
        fn fill_buf(&mut self) -> std::io::Result<&[u8]> { Err(ErrorKind::ConnectionReset.into()) }
        fn consume(&mut self, _: usize) { unreachable!() }
    }
    assert_eq!(read_request_line(&mut Broken).unwrap_err().kind(), ErrorKind::ConnectionReset);
}
