#![forbid(unsafe_code)]
//! HTTPS GET, without a dependency.
//!
//! # Why `curl` and not a Rust HTTP client
//!
//! Hard rule 1 forbids crates.io packages, and a TLS 1.3 stack is not something
//! to hand-roll: a bug in it is a silent loss of confidentiality, and there is no
//! test that proves the absence of one. `curl` ships on macOS and on every Linux
//! that could plausibly run this, is maintained by people who do nothing else,
//! and is reached through [`std::process::Command`] — no dependency, no `unsafe`,
//! no cryptography written here.
//!
//! The costs are real and worth naming: one process per request (~5-15ms of
//! fork/exec), no connection reuse, no HTTP/2 multiplexing, and no WebSocket. For
//! a funding-rate scanner polling a handful of venues every few seconds that is
//! irrelevant — funding changes on an eight-hour cycle. For a latency-sensitive
//! taker strategy it would be disqualifying, and at that point this module is
//! replaced by a real client behind the same [`Transport`] trait. That is the
//! whole reason the trait exists.
//!
//! # Security posture
//!
//! Decisions taken here rather than left to a caller:
//!
//! - **HTTPS only.** `http://` is refused by [`Transport`] implementations and by
//!   `--proto '=https'`, so neither this code nor a redirect can downgrade.
//! - **Redirects are not followed.** `-L` is absent deliberately. An exchange
//!   market-data endpoint does not legitimately redirect; a 3xx means a captive
//!   portal, a corporate proxy, or DNS interference, and following it would send
//!   the request to a host we never chose. Better to fail and say so.
//! - **A TLS floor.** `--tlsv1.2` refuses to negotiate anything older.
//! - **No `--insecure`, ever.** There is a test asserting the flag never appears
//!   in a built argument list, because "just for staging" is how it ships.
//! - **Bounded time and size.** A hung or runaway response must not hang a scan
//!   or exhaust memory.
//! - **Arguments, not a shell.** `Command` passes an `argv` array directly, so
//!   there is no shell to inject into. Query values are percent-encoded by
//!   [`Query`] on top of that, so a symbol cannot smuggle in `&limit=1`.
//!
//! # No credentials pass through here
//!
//! Everything this module reaches is a **public market-data endpoint**: books,
//! funding rates, instrument specs. None of it is authenticated, so there is no
//! API key, secret or signature anywhere in this crate, and a leak of this code
//! path leaks nothing but public prices. Private endpoints (balances, orders)
//! need request signing and a credential store, which is a separate concern and
//! deliberately not started here.

use core::fmt;
use std::cell::RefCell;
use std::process::Command;

/// Longest a single request may take, end to end, in seconds.
///
/// A scan holds no position and can retry, so failing fast beats waiting: a
/// venue that has not answered in eight seconds has effectively answered.
pub const DEFAULT_TIMEOUT_SECS: u32 = 8;

/// Longest the TCP+TLS handshake alone may take, in seconds.
pub const DEFAULT_CONNECT_TIMEOUT_SECS: u32 = 4;

/// Largest response body accepted, in bytes.
///
/// A 5000-level order book is roughly 200KB, so 8MB is far above anything real
/// and still far below a size that would matter to the process. The cap exists
/// so a misrouted request that lands on something enormous fails as an error
/// rather than as memory pressure.
pub const DEFAULT_MAX_BYTES: u64 = 8 * 1024 * 1024;

/// Why a request did not produce a body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HttpError {
    /// The URL was not `https://`. See the module docs on downgrade.
    NotHttps {
        /// The URL as given.
        url: String,
    },
    /// `curl` could not be started at all — usually not installed, or not on
    /// `PATH`. Carries the OS message, because "no such file or directory" for a
    /// program the operator never mentioned is otherwise baffling.
    LauncherUnavailable {
        /// What the OS said.
        detail: String,
    },
    /// `curl` ran and failed. Carries its exit code and whatever it wrote to
    /// stderr, which is where its diagnosis lives (DNS failure, TLS failure,
    /// timeout all look identical from the exit code alone).
    TransportFailed {
        /// `curl`'s exit status, when it had one.
        code: Option<i32>,
        /// `curl`'s stderr, trimmed.
        detail: String,
    },
    /// The server answered with a non-2xx status.
    ///
    /// Carries a prefix of the body: exchanges put the actual reason there
    /// (`{"code":"51001","msg":"Instrument ID does not exist"}`) and the status
    /// alone is usually just 400.
    Status {
        /// HTTP status code.
        code: u16,
        /// First part of the body.
        body_prefix: String,
    },
    /// The response was larger than the configured cap.
    TooLarge {
        /// The cap that was exceeded.
        limit: u64,
    },
    /// `curl` exited successfully but its output did not carry the status line
    /// this module appends, so the body cannot be separated from it.
    MalformedTransportOutput,
    /// The body was not valid UTF-8. Every venue sends JSON, which RFC 8259
    /// requires to be UTF-8, so this is corruption rather than a variant to
    /// support.
    NotUtf8,
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HttpError::NotHttps { url } => write!(f, "refusing a non-HTTPS URL: {url}"),
            HttpError::LauncherUnavailable { detail } => {
                write!(f, "could not run curl: {detail}")
            }
            HttpError::TransportFailed { code, detail } => match code {
                Some(c) => write!(f, "curl failed with exit {c}: {detail}"),
                None => write!(f, "curl was terminated by a signal: {detail}"),
            },
            HttpError::Status { code, body_prefix } => {
                write!(f, "HTTP {code}: {body_prefix}")
            }
            HttpError::TooLarge { limit } => {
                write!(f, "response exceeded {limit} bytes")
            }
            HttpError::MalformedTransportOutput => {
                f.write_str("curl output did not carry a status line")
            }
            HttpError::NotUtf8 => f.write_str("response body was not valid UTF-8"),
        }
    }
}

impl std::error::Error for HttpError {}

/// One HTTPS GET.
///
/// The seam that lets every adapter be tested without a network. A real scan
/// uses [`CurlTransport`]; the test suite uses [`StubTransport`] loaded with
/// payloads captured from the live endpoints, so an adapter's parsing is proven
/// against the bytes a venue actually sends while remaining a pure unit test.
pub trait Transport {
    /// Fetch `url` and return the body.
    ///
    /// Implementations must refuse a non-HTTPS URL with [`HttpError::NotHttps`].
    fn get(&self, url: &str) -> Result<String, HttpError>;
}

/// A query string built so a value cannot become a parameter.
///
/// Every value is percent-encoded down to the RFC 3986 unreserved set. That
/// matters because symbols reach this from configuration and, later, from a
/// user: a symbol of `BTCUSDT&limit=5000` appended raw would silently change the
/// depth of the book a size was validated against. Encoded, it stays one value
/// and the venue rejects it as an unknown instrument, which is the correct
/// outcome.
///
/// `Command` already removes shell injection — there is no shell — so this is
/// specifically about URL structure, not about quoting.
#[derive(Debug, Clone, Default)]
pub struct Query {
    parts: Vec<(String, String)>,
}

impl Query {
    /// An empty query.
    pub fn new() -> Self {
        Self { parts: Vec::new() }
    }

    /// Append one parameter.
    pub fn push(mut self, key: &str, value: &str) -> Self {
        self.parts.push((key.to_string(), value.to_string()));
        self
    }

    /// The encoded query, including the leading `?`, or an empty string.
    pub fn build(&self) -> String {
        if self.parts.is_empty() {
            return String::new();
        }
        let mut out = String::from("?");
        for (i, (k, v)) in self.parts.iter().enumerate() {
            if i > 0 {
                out.push('&');
            }
            out.push_str(&percent_encode(k));
            out.push('=');
            out.push_str(&percent_encode(v));
        }
        out
    }
}

/// Percent-encode everything outside the RFC 3986 unreserved set.
///
/// Deliberately aggressive: encoding a character that did not need it costs
/// nothing, while failing to encode one that did changes the request.
pub fn percent_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(*byte as char);
            }
            other => {
                out.push('%');
                out.push_str(&format!("{other:02X}"));
            }
        }
    }
    out
}

/// Split `curl`'s output into body and status.
///
/// This module appends `\n%{http_code}` via `--write-out`, so the status is the
/// text after the final newline. Separated out as a free function because it is
/// the one piece of [`CurlTransport`] that can be tested without a network, and
/// getting it wrong would attach a status digit to the end of a JSON body.
fn split_output(raw: &str) -> Result<(&str, u16), HttpError> {
    let cut = raw
        .rfind('\n')
        .ok_or(HttpError::MalformedTransportOutput)?;
    let code = raw[cut + 1..]
        .trim()
        .parse::<u16>()
        .map_err(|_| HttpError::MalformedTransportOutput)?;
    Ok((&raw[..cut], code))
}

/// Take at most `n` characters, on a character boundary.
fn prefix(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}

/// A [`Transport`] that shells out to `curl`.
#[derive(Debug, Clone)]
pub struct CurlTransport {
    program: String,
    timeout_secs: u32,
    connect_timeout_secs: u32,
    max_bytes: u64,
}

impl Default for CurlTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl CurlTransport {
    /// A transport with the module's default bounds.
    pub fn new() -> Self {
        Self {
            program: "curl".to_string(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            connect_timeout_secs: DEFAULT_CONNECT_TIMEOUT_SECS,
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }

    /// Override the total request timeout, in seconds.
    pub fn with_timeout_secs(mut self, secs: u32) -> Self {
        self.timeout_secs = secs;
        self
    }

    /// Override the response size cap, in bytes.
    pub fn with_max_bytes(mut self, bytes: u64) -> Self {
        self.max_bytes = bytes;
        self
    }

    /// The exact argument list this transport would pass to `curl`.
    ///
    /// Public so the security posture is testable rather than asserted in a
    /// comment: [`tests::the_argument_list_never_weakens_tls`] reads this.
    pub fn args(&self, url: &str) -> Vec<String> {
        vec![
            // No progress meter, but do print errors to stderr so a failure has
            // a diagnosis rather than just an exit code.
            "--silent".to_string(),
            "--show-error".to_string(),
            // Refuse any protocol but HTTPS, including via a redirect. Combined
            // with the absence of --location this makes a downgrade impossible
            // rather than merely unlikely.
            "--proto".to_string(),
            "=https".to_string(),
            "--proto-redir".to_string(),
            "=https".to_string(),
            "--tlsv1.2".to_string(),
            "--max-time".to_string(),
            self.timeout_secs.to_string(),
            "--connect-timeout".to_string(),
            self.connect_timeout_secs.to_string(),
            // Honoured only when the server declares a length; the post-hoc
            // check in `get` covers a chunked response.
            "--max-filesize".to_string(),
            self.max_bytes.to_string(),
            "--header".to_string(),
            "Accept: application/json".to_string(),
            // Appended after the body, read back by `split_output`.
            "--write-out".to_string(),
            "\\n%{http_code}".to_string(),
            url.to_string(),
        ]
    }
}

impl Transport for CurlTransport {
    fn get(&self, url: &str) -> Result<String, HttpError> {
        // Checked here as well as by --proto, because this is the check that
        // also guarantees the URL cannot be read as a leading-dash option.
        if !url.starts_with("https://") {
            return Err(HttpError::NotHttps {
                url: url.to_string(),
            });
        }

        let output = Command::new(&self.program)
            .args(self.args(url))
            .output()
            .map_err(|e| HttpError::LauncherUnavailable {
                detail: e.to_string(),
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            // 63 is curl's --max-filesize refusal, which deserves its own
            // variant so a caller can distinguish "too big" from "network broke".
            if output.status.code() == Some(63) {
                return Err(HttpError::TooLarge {
                    limit: self.max_bytes,
                });
            }
            return Err(HttpError::TransportFailed {
                code: output.status.code(),
                detail: prefix(&stderr, 400),
            });
        }

        let raw = String::from_utf8(output.stdout).map_err(|_| HttpError::NotUtf8)?;
        let (body, code) = split_output(&raw)?;

        // The chunked-response case --max-filesize cannot see.
        if body.len() as u64 > self.max_bytes {
            return Err(HttpError::TooLarge {
                limit: self.max_bytes,
            });
        }

        if !(200..300).contains(&code) {
            return Err(HttpError::Status {
                code,
                body_prefix: prefix(body.trim(), 400),
            });
        }

        Ok(body.to_string())
    }
}

/// A [`Transport`] that answers from a fixed table, for tests.
///
/// Lets every adapter be tested against payloads captured from the live
/// endpoints without a socket, so the test suite stays offline, deterministic and
/// fast while still proving the parsing against real bytes.
#[derive(Debug, Clone, Default)]
pub struct StubTransport {
    responses: Vec<(String, String)>,
    /// Every URL asked for, in order.
    ///
    /// A [`RefCell`] because [`Transport::get`] takes `&self` — a transport must
    /// not need `&mut` access, since an adapter holds one behind a shared
    /// reference. This is test-only scaffolding, so the single-threaded borrow
    /// discipline of `RefCell` is sufficient and a lock would be noise.
    requested: RefCell<Vec<String>>,
}

impl StubTransport {
    /// An empty stub.
    pub fn new() -> Self {
        Self {
            responses: Vec::new(),
            requested: RefCell::new(Vec::new()),
        }
    }

    /// Answer `body` for any URL containing `url_contains`.
    ///
    /// Substring rather than exact match so a test does not have to restate a
    /// full query string, which would make it a test of URL formatting rather
    /// than of parsing.
    pub fn on(mut self, url_contains: &str, body: &str) -> Self {
        self.responses
            .push((url_contains.to_string(), body.to_string()));
        self
    }

    /// Every URL this stub was asked for, in call order.
    ///
    /// Exists so a test can assert on the *request* and not only on the parsed
    /// answer. Substring matching means a stub happily answers a malformed URL,
    /// so without this an adapter could send `?symbol=BTCUSDT&limit=99999`, get
    /// the three-level fixture back, and pass every assertion about the book.
    pub fn requested(&self) -> Vec<String> {
        self.requested.borrow().clone()
    }
}

impl Transport for StubTransport {
    fn get(&self, url: &str) -> Result<String, HttpError> {
        self.requested.borrow_mut().push(url.to_string());
        if !url.starts_with("https://") {
            return Err(HttpError::NotHttps {
                url: url.to_string(),
            });
        }
        for (needle, body) in &self.responses {
            if url.contains(needle.as_str()) {
                return Ok(body.clone());
            }
        }
        // An unmatched URL is a test bug, and a 404 is the clearest way to say
        // so: silently returning an empty body would surface much later as a
        // confusing parse error.
        Err(HttpError::Status {
            code: 404,
            body_prefix: format!("StubTransport has no response for {url}"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The security posture from the module docs, read off the actual argument
    /// list rather than trusted.
    ///
    /// This is the test that has to survive the next person in a hurry. Every
    /// assertion here corresponds to a way TLS gets quietly weakened in a real
    /// codebase.
    #[test]
    fn the_argument_list_never_weakens_tls() {
        let args = CurlTransport::new().args("https://api.binance.com/api/v3/depth");

        // The flags that disable verification. `-k` is the short form and is the
        // one that actually shows up in commit history.
        for forbidden in [
            "--insecure",
            "-k",
            "--proxy-insecure",
            "--ssl-allow-beast",
            "--no-alpn",
        ] {
            assert!(
                !args.iter().any(|a| a == forbidden),
                "{forbidden} must never appear"
            );
        }

        // Redirect following is absent, so a 3xx cannot move the request to a
        // host we did not choose.
        for forbidden in ["-L", "--location"] {
            assert!(
                !args.iter().any(|a| a == forbidden),
                "{forbidden} must never appear: a market-data endpoint does not \
                 legitimately redirect"
            );
        }

        // And the positive requirements.
        assert!(args.iter().any(|a| a == "--proto"));
        assert!(args.iter().any(|a| a == "--proto-redir"));
        assert!(args.iter().any(|a| a == "--tlsv1.2"));
        assert!(args.iter().any(|a| a == "--max-time"));
        assert!(args.iter().any(|a| a == "--connect-timeout"));
        assert!(args.iter().any(|a| a == "--max-filesize"));

        // The URL is last, and it is the only argument that is not a flag or a
        // flag's value.
        assert_eq!(args.last().unwrap(), "https://api.binance.com/api/v3/depth");
    }

    #[test]
    fn a_non_https_url_is_refused_before_any_process_starts() {
        // `program` is a name that does not exist, so if the check did not fire
        // first this would fail as LauncherUnavailable instead.
        let t = CurlTransport {
            program: "definitely-not-a-real-program-xyzzy".to_string(),
            ..CurlTransport::new()
        };
        for url in [
            "http://api.binance.com/api/v3/depth",
            "ftp://example.com/",
            "file:///etc/passwd",
            "//api.binance.com/",
        ] {
            assert!(
                matches!(t.get(url), Err(HttpError::NotHttps { .. })),
                "{url} must be refused"
            );
        }
    }

    #[test]
    fn a_missing_curl_is_reported_as_such() {
        let t = CurlTransport {
            program: "definitely-not-a-real-program-xyzzy".to_string(),
            ..CurlTransport::new()
        };
        let err = t.get("https://api.binance.com/").unwrap_err();
        assert!(
            matches!(err, HttpError::LauncherUnavailable { .. }),
            "{err:?}"
        );
    }

    /// A symbol cannot become a second parameter.
    ///
    /// The concrete attack: `limit` controls how deep a book we validate a size
    /// against, so an injected `&limit=1` would make a large size look fillable
    /// against a single level.
    #[test]
    fn a_query_value_cannot_become_a_parameter() {
        let q = Query::new()
            .push("symbol", "BTCUSDT&limit=1")
            .push("limit", "20");
        let built = q.build();
        assert_eq!(built, "?symbol=BTCUSDT%26limit%3D1&limit=20");
        // Exactly one `&` separating the two real parameters, and one `=` per
        // parameter.
        assert_eq!(built.matches('&').count(), 1);
        assert_eq!(built.matches('=').count(), 2);
    }

    #[test]
    fn percent_encoding_covers_everything_outside_the_unreserved_set() {
        assert_eq!(percent_encode("BTC-USDT_SWAP.v5~x"), "BTC-USDT_SWAP.v5~x");
        assert_eq!(percent_encode("a b"), "a%20b");
        assert_eq!(percent_encode("?#/&=%"), "%3F%23%2F%26%3D%25");
        // Multi-byte input is encoded per UTF-8 byte, not per character.
        assert_eq!(percent_encode("é"), "%C3%A9");
    }

    #[test]
    fn an_empty_query_adds_nothing() {
        assert_eq!(Query::new().build(), "");
    }

    #[test]
    fn the_status_line_is_split_off_the_body() {
        let (body, code) = split_output("{\"a\":1}\n200").expect("split");
        assert_eq!(body, "{\"a\":1}");
        assert_eq!(code, 200);

        // A body containing newlines keeps them: the split is at the *last* one.
        let (body, code) = split_output("{\n  \"a\": 1\n}\n429").expect("split");
        assert_eq!(body, "{\n  \"a\": 1\n}");
        assert_eq!(code, 429);

        // An empty body still yields a status.
        let (body, code) = split_output("\n204").expect("split");
        assert_eq!(body, "");
        assert_eq!(code, 204);

        // And output with no status line is refused rather than read as a body.
        assert_eq!(
            split_output("no newline here"),
            Err(HttpError::MalformedTransportOutput)
        );
        assert_eq!(
            split_output("{\"a\":1}\nnot-a-number"),
            Err(HttpError::MalformedTransportOutput)
        );
    }

    #[test]
    fn the_stub_matches_by_substring_and_refuses_the_unexpected() {
        let t = StubTransport::new().on("/fapi/v1/depth", r#"{"lastUpdateId":1}"#);
        assert_eq!(
            t.get("https://fapi.binance.com/fapi/v1/depth?symbol=BTCUSDT")
                .unwrap(),
            r#"{"lastUpdateId":1}"#
        );
        let err = t.get("https://fapi.binance.com/fapi/v1/premiumIndex").unwrap_err();
        assert!(matches!(err, HttpError::Status { code: 404, .. }), "{err:?}");
        // The stub holds the same HTTPS line as the real transport, so a test
        // cannot accidentally prove an adapter works over plaintext.
        assert!(matches!(
            t.get("http://fapi.binance.com/fapi/v1/depth"),
            Err(HttpError::NotHttps { .. })
        ));
    }

    #[test]
    fn error_messages_carry_the_diagnosis() {
        let status = HttpError::Status {
            code: 400,
            body_prefix: r#"{"code":"51001","msg":"Instrument ID does not exist"}"#.to_string(),
        };
        let text = status.to_string();
        assert!(text.contains("400"));
        // The venue's own explanation survives to the operator, which is the
        // whole reason the body is captured on a failure.
        assert!(text.contains("51001"));
        assert!(text.contains("Instrument ID does not exist"));
    }

    #[test]
    fn a_prefix_never_splits_a_character() {
        // Truncating a byte-oriented slice mid-character would panic; `prefix`
        // counts characters.
        let text = "ошибка инструмента";
        assert_eq!(prefix(text, 6), "ошибка");
        assert_eq!(prefix("short", 400), "short");
    }
}
