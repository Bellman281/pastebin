//! A deliberately small HTTP/1.1 subset: enough to answer requests and be
//! load-tested with `wrk`/`ab`, not a general-purpose HTTP stack.
//!
//! We parse the request line and the few headers we act on (`Content-Length`,
//! `Connection`), consume a `Content-Length` body to stay framed for keep-alive,
//! and reject what we don't support (chunked bodies, oversized heads). Parsing is
//! a pure function over a byte buffer, so it is exhaustively unit-tested without
//! any I/O.

use std::fmt::Write as _;

/// Largest request head (request line + headers) we will buffer before giving up.
pub const MAX_HEAD_BYTES: usize = 8 * 1024;

/// The parsed head of an HTTP request.
#[derive(Debug, PartialEq, Eq)]
pub struct RequestHead {
    pub method: String,
    pub path: String,
    /// Whether the connection should be kept open after this request.
    pub keep_alive: bool,
    /// Declared body length (`0` when absent).
    pub content_length: usize,
}

/// Why a head could not (yet) be parsed.
#[derive(Debug, PartialEq, Eq)]
pub enum ParseError {
    /// The head is not fully received yet — read more bytes and retry.
    Incomplete,
    /// Malformed request line or header.
    Malformed,
    /// The head exceeded [`MAX_HEAD_BYTES`].
    HeadTooLarge,
    /// Unsupported framing (e.g. chunked transfer-encoding).
    Unsupported,
}

/// Try to parse the request head from the front of `buf`. On success returns the
/// head and the number of bytes it occupies (through the terminating blank line).
pub fn parse_head(buf: &[u8]) -> Result<(RequestHead, usize), ParseError> {
    let head_end = match find_subsequence(buf, b"\r\n\r\n") {
        Some(i) => i + 4,
        None => {
            return if buf.len() > MAX_HEAD_BYTES {
                Err(ParseError::HeadTooLarge)
            } else {
                Err(ParseError::Incomplete)
            };
        }
    };
    if head_end > MAX_HEAD_BYTES {
        return Err(ParseError::HeadTooLarge);
    }

    let head = &buf[..head_end - 4]; // exclude the trailing CRLFCRLF
    let text = std::str::from_utf8(head).map_err(|_| ParseError::Malformed)?;
    let mut lines = text.split("\r\n");

    // Request line: METHOD SP PATH SP HTTP/1.x
    let request_line = lines.next().ok_or(ParseError::Malformed)?;
    let mut parts = request_line.split(' ');
    let method = parts
        .next()
        .filter(|s| !s.is_empty())
        .ok_or(ParseError::Malformed)?;
    let path = parts
        .next()
        .filter(|s| !s.is_empty())
        .ok_or(ParseError::Malformed)?;
    let version = parts.next().ok_or(ParseError::Malformed)?;
    if parts.next().is_some() || !version.starts_with("HTTP/1.") {
        return Err(ParseError::Malformed);
    }

    // HTTP/1.1 keeps the connection alive by default; 1.0 closes by default.
    let mut keep_alive = version == "HTTP/1.1";
    let mut content_length = 0usize;

    for line in lines {
        if line.is_empty() {
            continue;
        }
        let (name, value) = line.split_once(':').ok_or(ParseError::Malformed)?;
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim();
        match name.as_str() {
            "content-length" => {
                content_length = value.parse::<usize>().map_err(|_| ParseError::Malformed)?;
            }
            "transfer-encoding" if value.eq_ignore_ascii_case("chunked") => {
                return Err(ParseError::Unsupported);
            }
            "connection" => {
                if value.eq_ignore_ascii_case("close") {
                    keep_alive = false;
                } else if value.eq_ignore_ascii_case("keep-alive") {
                    keep_alive = true;
                }
            }
            _ => {}
        }
    }

    Ok((
        RequestHead {
            method: method.to_owned(),
            path: path.to_owned(),
            keep_alive,
            content_length,
        },
        head_end,
    ))
}

/// Build a full HTTP/1.1 response with a `text/plain` body. `content-length` is
/// always the true body length; `write_body` is `false` for `HEAD` responses
/// (headers only, but with the length the matching `GET` would have returned).
pub fn build_response(
    status: u16,
    reason: &str,
    body: &str,
    keep_alive: bool,
    write_body: bool,
) -> Vec<u8> {
    let conn = if keep_alive { "keep-alive" } else { "close" };
    let mut s = String::with_capacity(body.len() + 128);
    let _ = write!(
        s,
        "HTTP/1.1 {status} {reason}\r\n\
         content-type: text/plain; charset=utf-8\r\n\
         content-length: {len}\r\n\
         connection: {conn}\r\n\
         \r\n",
        len = body.len(),
    );
    if write_body {
        s.push_str(body);
    }
    s.into_bytes()
}

/// First index of `needle` within `haystack`, if present.
fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_simple_get() {
        let raw = b"GET /hello HTTP/1.1\r\nHost: x\r\n\r\n";
        let (head, len) = parse_head(raw).unwrap();
        assert_eq!(head.method, "GET");
        assert_eq!(head.path, "/hello");
        assert!(head.keep_alive); // 1.1 default
        assert_eq!(head.content_length, 0);
        assert_eq!(len, raw.len());
    }

    #[test]
    fn http_1_0_defaults_to_close_and_connection_header_overrides() {
        let (h10, _) = parse_head(b"GET / HTTP/1.0\r\n\r\n").unwrap();
        assert!(!h10.keep_alive);
        let (h10ka, _) = parse_head(b"GET / HTTP/1.0\r\nConnection: keep-alive\r\n\r\n").unwrap();
        assert!(h10ka.keep_alive);
        let (h11c, _) = parse_head(b"GET / HTTP/1.1\r\nConnection: close\r\n\r\n").unwrap();
        assert!(!h11c.keep_alive);
    }

    #[test]
    fn reads_content_length() {
        let (h, len) = parse_head(b"POST /x HTTP/1.1\r\nContent-Length: 5\r\n\r\nhello").unwrap();
        assert_eq!(h.method, "POST");
        assert_eq!(h.content_length, 5);
        // `len` is the head only; the body ("hello") sits after it.
        assert_eq!(len, b"POST /x HTTP/1.1\r\nContent-Length: 5\r\n\r\n".len());
    }

    #[test]
    fn incomplete_head_is_reported() {
        assert_eq!(
            parse_head(b"GET / HTTP/1.1\r\nHost: x"),
            Err(ParseError::Incomplete)
        );
        assert_eq!(parse_head(b""), Err(ParseError::Incomplete));
    }

    #[test]
    fn malformed_request_line() {
        assert_eq!(parse_head(b"GARBAGE\r\n\r\n"), Err(ParseError::Malformed));
        assert_eq!(parse_head(b"GET /\r\n\r\n"), Err(ParseError::Malformed)); // no version
        assert_eq!(
            parse_head(b"GET / HTTP/2\r\n\r\n"),
            Err(ParseError::Malformed)
        );
    }

    #[test]
    fn chunked_is_unsupported() {
        assert_eq!(
            parse_head(b"POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n"),
            Err(ParseError::Unsupported)
        );
    }

    #[test]
    fn oversized_head_is_rejected() {
        let mut raw = b"GET / HTTP/1.1\r\n".to_vec();
        raw.extend(std::iter::repeat(b'a').take(MAX_HEAD_BYTES)); // never terminates the head
        assert_eq!(parse_head(&raw), Err(ParseError::HeadTooLarge));
    }

    #[test]
    fn builds_response_with_and_without_body() {
        let get = build_response(200, "OK", "hi", true, true);
        let text = String::from_utf8(get).unwrap();
        assert!(text.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(text.contains("content-length: 2\r\n"));
        assert!(text.contains("connection: keep-alive\r\n"));
        assert!(text.ends_with("\r\n\r\nhi"));

        // HEAD: same content-length, no body bytes.
        let head = String::from_utf8(build_response(200, "OK", "hi", false, false)).unwrap();
        assert!(head.contains("content-length: 2\r\n"));
        assert!(head.contains("connection: close\r\n"));
        assert!(head.ends_with("\r\n\r\n"));
    }
}
