#![forbid(unsafe_code)]
//! A JSON reader for exchange payloads.
//!
//! # Why this exists at all
//!
//! Hard rule 1 forbids external dependencies, so `serde_json` is not available.
//! That constraint turns out to be a feature here, because the one thing every
//! off-the-shelf JSON parser does by default is the one thing this workspace must
//! never do: **parse a number into `f64`.**
//!
//! # Numbers are text, and that is the whole point
//!
//! [`Json::Num`] holds the venue's digits as the venue sent them. Nothing in this
//! module converts a number to a float, an integer, or a fixed-point value.
//!
//! `f64` has 53 bits of mantissa, so it represents about 15-16 significant
//! decimal digits. A BTC price of `78913.99` is not representable exactly; the
//! nearest `f64` is `78913.990000000005238689482212066650390625`. That error is
//! ~5e-11 relative, which sounds harmless until you notice that this system's
//! entire edge is measured in basis points (1e-4) and that the error is *not*
//! uniform — it depends on the magnitude of the price, so it does not cancel
//! between two venues quoting the same asset at slightly different prices.
//!
//! Worse, the error is silent. `arb_num`'s whole reason for existing is that
//! money arithmetic must be exact and must refuse rather than round. Parsing
//! through `f64` would defeat every guarantee in `arb-num` at the very first
//! step, before a single one of its checked operations ran.
//!
//! So numbers stay as text and go to [`arb_num::Decimal`] / `Price::parse` /
//! `Qty::parse` / `Rate::parse`, which parse exact decimal text into the 1e12
//! fixed-point scale and refuse anything that does not fit.
//!
//! # One accessor for two conventions
//!
//! Venues disagree about whether a number is a JSON string. Binance quotes
//! prices (`"78989.50"`) but leaves timestamps bare (`1787597215564`). Bybit
//! quotes *everything* inside `result.list[0]` including `nextFundingTime`, but
//! leaves the top-level `time` bare. OKX quotes everything, `code` included.
//!
//! Rather than make every adapter branch on that, [`Json::decimal`] accepts both
//! [`Json::Str`] and [`Json::Num`] and hands back the text. An adapter asks for
//! digits and gets digits. The distinction between a quoted and an unquoted
//! number carries no information a venue intends us to act on, so flattening it
//! is not a loss.
//!
//! # Strictness
//!
//! This parser rejects rather than guesses, because every input it sees is a
//! financial quantity and a silently mis-parsed one becomes a trade:
//!
//! - trailing content after the top-level value;
//! - duplicate keys in one object (see [`JsonError::DuplicateKey`]);
//! - leading zeros (`01`), a bare `.5`, a trailing `1.` — all malformed per RFC
//!   8259, and all signs that something truncated or corrupted the payload;
//! - unescaped control characters inside strings;
//! - lone UTF-16 surrogates;
//! - nesting deeper than [`MAX_DEPTH`].
//!
//! The depth limit is not theoretical. This parser is recursive and its input is
//! a network response we do not control. A payload of ten thousand `[` characters
//! would exhaust the stack, and a stack overflow in Rust is an immediate abort —
//! not a `Result` an adapter could handle, not a panic a caller could catch. A
//! process that aborts mid-scan while holding a position is a worse outcome than
//! any parse error, so the limit is checked on the way in.

use core::fmt;

/// Deepest nesting accepted, counting the outermost value as depth 1.
///
/// Real payloads from all eight venues nest at most four deep
/// (`{result:{list:[{...}]}}`). 64 leaves two orders of magnitude of headroom
/// while keeping the recursion far below any plausible stack limit.
pub const MAX_DEPTH: usize = 64;

/// A parsed JSON value.
///
/// `Obj` is a `Vec` of pairs rather than a map: no external dependency is
/// available for an order-preserving map, exchange objects hold at most a few
/// dozen keys so a linear scan is not worth optimising, and preserving order
/// makes a parse round-trippable for diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Json {
    /// `null`.
    Null,
    /// `true` or `false`.
    Bool(bool),
    /// A number, kept as **the exact text the venue sent**. Never a float. See
    /// the module docs for why this is not negotiable.
    Num(String),
    /// A string, with escape sequences already decoded.
    Str(String),
    /// An array.
    Arr(Vec<Json>),
    /// An object, in document order.
    Obj(Vec<(String, Json)>),
}

impl Json {
    /// Parse one complete JSON document.
    ///
    /// Refuses trailing content: a response body with a second value glued to
    /// the end is a framing bug, and reading only the first value would hide it.
    pub fn parse(input: &str) -> Result<Self, JsonError> {
        let mut p = Parser {
            bytes: input.as_bytes(),
            pos: 0,
        };
        p.skip_ws();
        let value = p.value(1)?;
        p.skip_ws();
        if p.pos != p.bytes.len() {
            return Err(JsonError::TrailingContent { at: p.pos });
        }
        Ok(value)
    }

    /// The value at `key`, or `None` when this is not an object or has no such
    /// key.
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(pairs) => pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// The value at `key`, or a [`JsonError::MissingField`] naming it.
    ///
    /// Adapters want this rather than [`Json::get`]: "Bybit stopped sending
    /// `fundingRate`" must surface as an error that names the field, not as a
    /// `None` that a `?` operator turns into a shrug.
    pub fn field(&self, key: &str) -> Result<&Json, JsonError> {
        self.get(key).ok_or_else(|| JsonError::MissingField {
            key: key.to_string(),
        })
    }

    /// The element at `index`, or `None` when this is not an array or is shorter.
    pub fn at(&self, index: usize) -> Option<&Json> {
        match self {
            Json::Arr(items) => items.get(index),
            _ => None,
        }
    }

    /// The elements, or a [`JsonError::WrongType`] when this is not an array.
    pub fn array(&self) -> Result<&[Json], JsonError> {
        match self {
            Json::Arr(items) => Ok(items),
            other => Err(JsonError::WrongType {
                wanted: "array",
                found: other.type_name(),
            }),
        }
    }

    /// The text of a string, or a [`JsonError::WrongType`].
    ///
    /// Deliberately does **not** accept a number: this is for symbols and status
    /// strings, where a bare number would mean the payload changed shape.
    pub fn string(&self) -> Result<&str, JsonError> {
        match self {
            Json::Str(s) => Ok(s),
            other => Err(JsonError::WrongType {
                wanted: "string",
                found: other.type_name(),
            }),
        }
    }

    /// The digits of a number, whether or not the venue quoted them.
    ///
    /// This is the accessor adapters use for every price, quantity, rate and
    /// timestamp. It accepts both [`Json::Str`] and [`Json::Num`] because the
    /// venues disagree and the disagreement is not meaningful — see the module
    /// docs.
    ///
    /// Returns the text unparsed. The caller hands it to `Price::parse`,
    /// `Rate::parse` or an integer parse, all of which are exact and all of
    /// which refuse what does not fit.
    pub fn decimal(&self) -> Result<&str, JsonError> {
        match self {
            Json::Str(s) | Json::Num(s) => Ok(s),
            other => Err(JsonError::WrongType {
                wanted: "number or numeric string",
                found: other.type_name(),
            }),
        }
    }

    /// Whether this is [`Json::Null`], or a string that is empty.
    ///
    /// Both spellings of "absent" occur in real payloads, sometimes for the same
    /// field: OKX sends `"nextFundingRate": ""` when the upcoming rate has not
    /// been published, and Bybit sends `"basisRate": ""` on a perpetual. An
    /// adapter that fed either to `Rate::parse` would get a parse error where the
    /// venue meant "no value", so the two are recognised together.
    pub fn is_absent(&self) -> bool {
        match self {
            Json::Null => true,
            Json::Str(s) => s.is_empty(),
            _ => false,
        }
    }

    /// A name for this variant, for error messages.
    pub const fn type_name(&self) -> &'static str {
        match self {
            Json::Null => "null",
            Json::Bool(_) => "boolean",
            Json::Num(_) => "number",
            Json::Str(_) => "string",
            Json::Arr(_) => "array",
            Json::Obj(_) => "object",
        }
    }
}

/// Why a payload could not be read.
///
/// Every variant carries enough to locate the problem in the body, because the
/// alternative when a venue changes its shape at 3am is reading the whole
/// response by eye.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JsonError {
    /// Input ended mid-value.
    UnexpectedEnd,
    /// A byte that cannot start or continue a value here.
    Unexpected {
        /// Byte offset.
        at: usize,
        /// The offending byte.
        found: u8,
    },
    /// A second value followed the first.
    TrailingContent {
        /// Byte offset where it starts.
        at: usize,
    },
    /// Nesting exceeded [`MAX_DEPTH`]. See the module docs on why this is a
    /// refusal and not a best effort.
    TooDeep {
        /// Byte offset of the value that crossed the limit.
        at: usize,
    },
    /// A number that is not valid JSON: a leading zero, a missing digit after
    /// `.` or `e`, or a bare sign.
    MalformedNumber {
        /// Byte offset where the number starts.
        at: usize,
    },
    /// A raw control character inside a string, which RFC 8259 requires to be
    /// escaped. Usually a truncated or byte-mangled body.
    ControlCharInString {
        /// Byte offset.
        at: usize,
    },
    /// An unknown escape, or a `\u` that is not four hex digits.
    BadEscape {
        /// Byte offset of the backslash.
        at: usize,
    },
    /// A `\u` half of a surrogate pair with no valid partner, which encodes no
    /// character.
    LoneSurrogate {
        /// Byte offset of the backslash.
        at: usize,
    },
    /// One object carried the same key twice.
    ///
    /// RFC 8259 permits this and leaves the meaning to the reader, which is
    /// exactly why it is refused: the two values are usually different, and
    /// picking either silently means the number a trade is priced from was
    /// chosen by parser implementation detail. A venue has never been observed
    /// sending one; if this ever fires it is a corruption or a proxy rewriting
    /// the body, and both deserve to stop the scan.
    DuplicateKey {
        /// The repeated key.
        key: String,
        /// Byte offset of the second occurrence.
        at: usize,
    },
    /// A field the adapter requires was not in the object.
    MissingField {
        /// The field asked for.
        key: String,
    },
    /// A field was present with the wrong shape.
    WrongType {
        /// What the adapter needed.
        wanted: &'static str,
        /// What was there.
        found: &'static str,
    },
}

impl fmt::Display for JsonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JsonError::UnexpectedEnd => f.write_str("payload ended mid-value"),
            JsonError::Unexpected { at, found } => {
                write!(f, "unexpected byte {found:#04x} at offset {at}")
            }
            JsonError::TrailingContent { at } => {
                write!(f, "content after the end of the value at offset {at}")
            }
            JsonError::TooDeep { at } => {
                write!(f, "nesting deeper than {MAX_DEPTH} at offset {at}")
            }
            JsonError::MalformedNumber { at } => {
                write!(f, "malformed number at offset {at}")
            }
            JsonError::ControlCharInString { at } => {
                write!(f, "unescaped control character in string at offset {at}")
            }
            JsonError::BadEscape { at } => write!(f, "bad escape at offset {at}"),
            JsonError::LoneSurrogate { at } => {
                write!(f, "lone UTF-16 surrogate at offset {at}")
            }
            JsonError::DuplicateKey { key, at } => {
                write!(f, "duplicate key {key:?} at offset {at}")
            }
            JsonError::MissingField { key } => write!(f, "missing field {key:?}"),
            JsonError::WrongType { wanted, found } => {
                write!(f, "expected {wanted}, found {found}")
            }
        }
    }
}

impl std::error::Error for JsonError {}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        // RFC 8259 whitespace is exactly these four. Notably not a vertical tab
        // or a form feed, and not a Unicode space: accepting those would mean
        // accepting a body no conforming producer sends.
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn expect(&mut self, byte: u8) -> Result<(), JsonError> {
        match self.peek() {
            Some(b) if b == byte => {
                self.pos += 1;
                Ok(())
            }
            Some(found) => Err(JsonError::Unexpected {
                at: self.pos,
                found,
            }),
            None => Err(JsonError::UnexpectedEnd),
        }
    }

    fn literal(&mut self, word: &[u8], value: Json) -> Result<Json, JsonError> {
        if self.bytes.len() - self.pos >= word.len()
            && &self.bytes[self.pos..self.pos + word.len()] == word
        {
            self.pos += word.len();
            Ok(value)
        } else {
            Err(JsonError::Unexpected {
                at: self.pos,
                found: self.peek().unwrap_or(b' '),
            })
        }
    }

    fn value(&mut self, depth: usize) -> Result<Json, JsonError> {
        if depth > MAX_DEPTH {
            return Err(JsonError::TooDeep { at: self.pos });
        }
        match self.peek() {
            None => Err(JsonError::UnexpectedEnd),
            Some(b'{') => self.object(depth),
            Some(b'[') => self.array(depth),
            Some(b'"') => self.string_value(),
            Some(b't') => self.literal(b"true", Json::Bool(true)),
            Some(b'f') => self.literal(b"false", Json::Bool(false)),
            Some(b'n') => self.literal(b"null", Json::Null),
            Some(b'-') => self.number(),
            Some(b) if b.is_ascii_digit() => self.number(),
            Some(found) => Err(JsonError::Unexpected {
                at: self.pos,
                found,
            }),
        }
    }

    fn object(&mut self, depth: usize) -> Result<Json, JsonError> {
        self.expect(b'{')?;
        let mut pairs: Vec<(String, Json)> = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(Json::Obj(pairs));
        }
        loop {
            self.skip_ws();
            let key_at = self.pos;
            let key = match self.string_value()? {
                Json::Str(s) => s,
                // `string_value` only ever returns `Json::Str`.
                _ => unreachable!("string_value returns Str"),
            };
            if pairs.iter().any(|(k, _)| *k == key) {
                return Err(JsonError::DuplicateKey { key, at: key_at });
            }
            self.skip_ws();
            self.expect(b':')?;
            self.skip_ws();
            let value = self.value(depth + 1)?;
            pairs.push((key, value));
            self.skip_ws();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(Json::Obj(pairs));
                }
                Some(found) => {
                    return Err(JsonError::Unexpected {
                        at: self.pos,
                        found,
                    })
                }
                None => return Err(JsonError::UnexpectedEnd),
            }
        }
    }

    fn array(&mut self, depth: usize) -> Result<Json, JsonError> {
        self.expect(b'[')?;
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(Json::Arr(items));
        }
        loop {
            self.skip_ws();
            items.push(self.value(depth + 1)?);
            self.skip_ws();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b']') => {
                    self.pos += 1;
                    return Ok(Json::Arr(items));
                }
                Some(found) => {
                    return Err(JsonError::Unexpected {
                        at: self.pos,
                        found,
                    })
                }
                None => return Err(JsonError::UnexpectedEnd),
            }
        }
    }

    /// Parse a number and keep its text exactly.
    ///
    /// Grammar per RFC 8259: `-? (0 | [1-9][0-9]*) ('.' [0-9]+)? ([eE] [+-]? [0-9]+)?`
    ///
    /// The leading-zero rule is enforced because `0123` is not JSON, and a body
    /// containing it is a body some non-conforming producer built by string
    /// concatenation — which is a reason to distrust every other number in it.
    fn number(&mut self) -> Result<Json, JsonError> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        match self.peek() {
            Some(b'0') => {
                self.pos += 1;
                if matches!(self.peek(), Some(d) if d.is_ascii_digit()) {
                    return Err(JsonError::MalformedNumber { at: start });
                }
            }
            Some(d) if d.is_ascii_digit() => {
                while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                    self.pos += 1;
                }
            }
            _ => return Err(JsonError::MalformedNumber { at: start }),
        }
        if self.peek() == Some(b'.') {
            self.pos += 1;
            if !matches!(self.peek(), Some(d) if d.is_ascii_digit()) {
                return Err(JsonError::MalformedNumber { at: start });
            }
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            if !matches!(self.peek(), Some(d) if d.is_ascii_digit()) {
                return Err(JsonError::MalformedNumber { at: start });
            }
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        // The slice is ASCII digits, signs and dots by construction, so this
        // cannot fail on UTF-8 boundaries.
        let text = core::str::from_utf8(&self.bytes[start..self.pos])
            .map_err(|_| JsonError::MalformedNumber { at: start })?;
        Ok(Json::Num(text.to_string()))
    }

    fn string_value(&mut self) -> Result<Json, JsonError> {
        self.expect(b'"')?;
        let mut out = String::new();
        loop {
            let at = self.pos;
            match self.peek() {
                None => return Err(JsonError::UnexpectedEnd),
                Some(b'"') => {
                    self.pos += 1;
                    return Ok(Json::Str(out));
                }
                Some(b'\\') => {
                    self.pos += 1;
                    self.escape(at, &mut out)?;
                }
                // RFC 8259: unescaped bytes below 0x20 are forbidden.
                Some(b) if b < 0x20 => {
                    return Err(JsonError::ControlCharInString { at });
                }
                Some(_) => {
                    // Copy one whole UTF-8 scalar. The input is a `&str`, so it
                    // is valid UTF-8 and the sequence is well-formed; stepping
                    // by bytes would split a multi-byte character.
                    let rest = core::str::from_utf8(&self.bytes[self.pos..])
                        .map_err(|_| JsonError::Unexpected { at, found: 0 })?;
                    let ch = rest.chars().next().ok_or(JsonError::UnexpectedEnd)?;
                    out.push(ch);
                    self.pos += ch.len_utf8();
                }
            }
        }
    }

    fn escape(&mut self, at: usize, out: &mut String) -> Result<(), JsonError> {
        let code = self.peek().ok_or(JsonError::UnexpectedEnd)?;
        self.pos += 1;
        let ch = match code {
            b'"' => '"',
            b'\\' => '\\',
            b'/' => '/',
            b'b' => '\u{0008}',
            b'f' => '\u{000C}',
            b'n' => '\n',
            b'r' => '\r',
            b't' => '\t',
            b'u' => return self.unicode_escape(at, out),
            _ => return Err(JsonError::BadEscape { at }),
        };
        out.push(ch);
        Ok(())
    }

    /// Decode `\uXXXX`, joining a surrogate pair when there is one.
    ///
    /// A lone surrogate is refused rather than replaced with U+FFFD. Silently
    /// substituting would corrupt a symbol name, and a corrupted symbol routes
    /// an order to the wrong instrument — the exact failure
    /// `ScanError::FundingKeyMismatch` exists to catch, arriving by a path that
    /// check cannot see.
    fn unicode_escape(&mut self, at: usize, out: &mut String) -> Result<(), JsonError> {
        let first = self.hex4(at)?;
        let scalar = match first {
            // High surrogate: needs a following low surrogate.
            0xD800..=0xDBFF => {
                if self.peek() != Some(b'\\') {
                    return Err(JsonError::LoneSurrogate { at });
                }
                self.pos += 1;
                if self.peek() != Some(b'u') {
                    return Err(JsonError::LoneSurrogate { at });
                }
                self.pos += 1;
                let second = self.hex4(at)?;
                if !(0xDC00..=0xDFFF).contains(&second) {
                    return Err(JsonError::LoneSurrogate { at });
                }
                0x1_0000 + ((u32::from(first) - 0xD800) << 10) + (u32::from(second) - 0xDC00)
            }
            // Low surrogate with no high surrogate before it.
            0xDC00..=0xDFFF => return Err(JsonError::LoneSurrogate { at }),
            other => u32::from(other),
        };
        let ch = char::from_u32(scalar).ok_or(JsonError::LoneSurrogate { at })?;
        out.push(ch);
        Ok(())
    }

    fn hex4(&mut self, at: usize) -> Result<u16, JsonError> {
        if self.pos + 4 > self.bytes.len() {
            return Err(JsonError::UnexpectedEnd);
        }
        let mut value: u16 = 0;
        for _ in 0..4 {
            let b = self.bytes[self.pos];
            let digit = match b {
                b'0'..=b'9' => b - b'0',
                b'a'..=b'f' => b - b'a' + 10,
                b'A'..=b'F' => b - b'A' + 10,
                _ => return Err(JsonError::BadEscape { at }),
            };
            value = value * 16 + u16::from(digit);
            self.pos += 1;
        }
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- the reason this module exists ------------------------------------

    /// The claim in the module docs, demonstrated rather than asserted in prose.
    ///
    /// If this parser went through `f64`, the price Binance sent would not come
    /// back out. The test computes the `f64` round trip explicitly so the failure
    /// mode is visible in the source, not just described.
    #[test]
    fn a_price_survives_because_it_is_never_a_float() {
        let body = r#"{"bids":[["78913.99","4.17840"]]}"#;
        let json = Json::parse(body).expect("parse");
        let price = json
            .field("bids")
            .and_then(|b| b.at(0).ok_or(JsonError::UnexpectedEnd))
            .and_then(|lvl| lvl.at(0).ok_or(JsonError::UnexpectedEnd))
            .and_then(Json::decimal)
            .expect("price text");

        // Exactly what the venue sent, digit for digit.
        assert_eq!(price, "78913.99");

        // What an f64 round trip would have done to it. `78913.99` is not a
        // dyadic rational, so the nearest f64 is strictly greater, and the
        // difference is real rather than a display artefact.
        let through_float: f64 = price.parse().expect("f64");
        assert!(
            through_float > 78913.99_f64.floor(),
            "sanity: the float is in range"
        );
        assert_ne!(
            format!("{through_float:.20}"),
            "78913.99000000000000000000",
            "an f64 cannot hold this price exactly, which is why Num keeps text"
        );
    }

    /// Quoted or bare, an adapter asks for digits and gets digits.
    ///
    /// This is not a stylistic convenience: within a single Bybit response the
    /// top-level `time` is bare while `result.list[0].nextFundingTime` is quoted,
    /// so any adapter would otherwise need to remember which is which per field
    /// per venue.
    #[test]
    fn decimal_reads_through_both_conventions() {
        let json = Json::parse(r#"{"quoted":"1787616000000","bare":1787597404081}"#).expect("parse");
        assert_eq!(json.field("quoted").unwrap().decimal().unwrap(), "1787616000000");
        assert_eq!(json.field("bare").unwrap().decimal().unwrap(), "1787597404081");
        // And a non-numeric shape is still refused, so a payload that changes
        // from a number to an object does not read as digits.
        let obj = Json::parse(r#"{"x":{}}"#).expect("parse");
        assert!(matches!(
            obj.field("x").unwrap().decimal(),
            Err(JsonError::WrongType { .. })
        ));
    }

    /// A number is kept verbatim, including trailing zeros and exponents.
    ///
    /// Trailing zeros matter: `"0.00010000"` from Binance and `"0.0001"` from a
    /// hand-written fixture must parse to the same `Rate`, and that is
    /// `arb_num`'s job — but only if this layer hands over what was actually
    /// sent rather than a normalised form of it.
    #[test]
    fn numbers_keep_their_exact_text() {
        for text in [
            "0",
            "-0",
            "1787597215564",
            "0.00010000",
            "-0.0000101100",
            "0.0000758415248886",
            "1e-8",
            "-1.5E+10",
        ] {
            let json = Json::parse(text).expect(text);
            assert_eq!(json, Json::Num(text.to_string()), "{text}");
            assert_eq!(json.decimal().unwrap(), text);
        }
    }

    // -- real payload shapes ----------------------------------------------

    /// The Binance USD-M depth shape, as captured from the live endpoint.
    ///
    /// Note `E` and `T` as bare numbers beside quoted level values, and the
    /// levels as two-element arrays with no field names. The spot endpoint
    /// (`api/v3/depth`) omits `E` and `T` entirely, which is why an adapter
    /// cannot assume an exchange timestamp is available.
    #[test]
    fn the_binance_depth_shape_reads() {
        let body = r#"{"lastUpdateId":11377592038715,"E":1787597215564,"T":1787597215562,
            "bids":[["78989.50","0.591"],["78989.40","0.002"]],
            "asks":[["78989.60","3.663"],["78989.70","0.001"]]}"#;
        let json = Json::parse(body).expect("parse");
        assert_eq!(json.field("E").unwrap().decimal().unwrap(), "1787597215564");
        let bids = json.field("bids").unwrap().array().unwrap();
        assert_eq!(bids.len(), 2);
        assert_eq!(bids[0].at(0).unwrap().decimal().unwrap(), "78989.50");
        assert_eq!(bids[0].at(1).unwrap().decimal().unwrap(), "0.591");
        let asks = json.field("asks").unwrap().array().unwrap();
        assert_eq!(asks[0].at(0).unwrap().decimal().unwrap(), "78989.60");
    }

    /// The Bybit v5 envelope, which nests two levels before the data and quotes
    /// every value inside `list` while leaving the outer `time` bare.
    #[test]
    fn the_bybit_envelope_reads() {
        let body = r#"{"retCode":0,"retMsg":"OK","result":{"category":"linear","list":[
            {"symbol":"BTCUSDT","fundingRate":"-0.00001011",
             "nextFundingTime":"1787616000000","fundingIntervalHour":"8",
             "markPrice":"78990.10","indexPrice":"78988.55","basisRate":""}]},
            "retExtInfo":{},"time":1787597404081}"#;
        let json = Json::parse(body).expect("parse");
        assert_eq!(json.field("retCode").unwrap().decimal().unwrap(), "0");
        assert_eq!(json.field("retMsg").unwrap().string().unwrap(), "OK");
        let row = json
            .field("result")
            .unwrap()
            .field("list")
            .unwrap()
            .at(0)
            .unwrap();
        assert_eq!(row.field("fundingRate").unwrap().decimal().unwrap(), "-0.00001011");
        // The interval is published, so it never has to be assumed. A wrong
        // interval scales the annualised rate by the ratio of the two.
        assert_eq!(row.field("fundingIntervalHour").unwrap().decimal().unwrap(), "8");
        // And an empty string is recognised as absent rather than fed to a
        // number parser.
        assert!(row.field("basisRate").unwrap().is_absent());
        assert!(!row.field("fundingRate").unwrap().is_absent());
    }

    /// The OKX envelope, where `code` is a quoted number and the payload is
    /// always an array even for a single instrument.
    #[test]
    fn the_okx_envelope_reads() {
        let body = r#"{"code":"0","msg":"","data":[{"instId":"BTC-USDT-SWAP",
            "fundingRate":"0.0000758415248886","fundingTime":"1787616000000",
            "nextFundingTime":"1787644800000","prevFundingTime":"1787587200000",
            "nextFundingRate":"","settState":"settled"}]}"#;
        let json = Json::parse(body).expect("parse");
        assert_eq!(json.field("code").unwrap().decimal().unwrap(), "0");
        assert!(json.field("msg").unwrap().is_absent());
        let row = json.field("data").unwrap().at(0).unwrap();

        // The trap this shape sets: `fundingTime` is the *upcoming* settlement,
        // and `nextFundingTime` is the one after it. Binance's `nextFundingTime`
        // equals OKX's `fundingTime`, not OKX's `nextFundingTime`. An adapter
        // that matched on the name alone would be eight hours out.
        let upcoming: i64 = row.field("fundingTime").unwrap().decimal().unwrap().parse().unwrap();
        let after: i64 = row.field("nextFundingTime").unwrap().decimal().unwrap().parse().unwrap();
        let previous: i64 = row.field("prevFundingTime").unwrap().decimal().unwrap().parse().unwrap();
        assert_eq!(upcoming, 1787616000000);
        assert_eq!(after - upcoming, 28_800_000, "8h in ms");
        assert_eq!(upcoming - previous, 28_800_000, "8h in ms");
        assert!(row.field("nextFundingRate").unwrap().is_absent());
    }

    // -- strictness -------------------------------------------------------

    #[test]
    fn a_duplicate_key_is_refused_rather_than_resolved() {
        let err = Json::parse(r#"{"price":"1.00","price":"2.00"}"#).unwrap_err();
        assert!(matches!(err, JsonError::DuplicateKey { .. }), "{err:?}");
        // The message names the key, because the operator's next question is
        // always "which field".
        assert!(err.to_string().contains("price"));
    }

    #[test]
    fn trailing_content_is_refused() {
        assert!(matches!(
            Json::parse(r#"{"a":1} {"b":2}"#).unwrap_err(),
            JsonError::TrailingContent { .. }
        ));
    }

    #[test]
    fn malformed_numbers_are_refused() {
        for text in ["01", "-", "1.", ".5", "1e", "1e+", "--1", "0x10"] {
            assert!(
                Json::parse(text).is_err(),
                "{text} must not parse as a number"
            );
        }
    }

    #[test]
    fn nesting_is_bounded_so_a_hostile_body_cannot_abort_the_process() {
        // One past the limit fails as a clean error rather than a stack overflow.
        let deep = format!("{}1{}", "[".repeat(MAX_DEPTH), "]".repeat(MAX_DEPTH));
        assert!(matches!(
            Json::parse(&deep).unwrap_err(),
            JsonError::TooDeep { .. }
        ));
        // And a body just inside the limit still parses, so the bound is not
        // so tight that a real payload trips it.
        let ok = format!(
            "{}1{}",
            "[".repeat(MAX_DEPTH - 1),
            "]".repeat(MAX_DEPTH - 1)
        );
        assert!(Json::parse(&ok).is_ok());
    }

    #[test]
    fn control_characters_in_strings_are_refused() {
        assert!(matches!(
            Json::parse("{\"a\":\"one\ttwo\"}").unwrap_err(),
            JsonError::ControlCharInString { .. }
        ));
        // Escaped, the same character is fine.
        let json = Json::parse(r#"{"a":"one\ttwo"}"#).expect("parse");
        assert_eq!(json.field("a").unwrap().string().unwrap(), "one\ttwo");
    }

    #[test]
    fn escapes_decode_including_surrogate_pairs() {
        let json = Json::parse(r#""a\/b\\c\"dA😀""#).expect("parse");
        assert_eq!(json.string().unwrap(), "a/b\\c\"dA\u{1F600}");
    }

    #[test]
    fn a_lone_surrogate_is_refused_rather_than_replaced() {
        for body in [r#""\uD800""#, r#""\uDC00""#, r#""\uD800A""#] {
            assert!(
                matches!(Json::parse(body).unwrap_err(), JsonError::LoneSurrogate { .. }),
                "{body}"
            );
        }
    }

    #[test]
    fn a_missing_field_names_itself() {
        let json = Json::parse(r#"{"a":1}"#).expect("parse");
        let err = json.field("fundingRate").unwrap_err();
        assert_eq!(
            err,
            JsonError::MissingField {
                key: "fundingRate".to_string()
            }
        );
        assert!(err.to_string().contains("fundingRate"));
    }

    #[test]
    fn empty_containers_and_unicode_keys_round_trip() {
        let json = Json::parse(r#"{"empty":{},"none":[],"ключ":"значение"}"#).expect("parse");
        assert_eq!(json.field("empty").unwrap(), &Json::Obj(vec![]));
        assert_eq!(json.field("none").unwrap(), &Json::Arr(vec![]));
        assert_eq!(json.field("ключ").unwrap().string().unwrap(), "значение");
    }

    #[test]
    fn truncated_bodies_fail_rather_than_yielding_a_partial_book() {
        // The realistic failure: a connection drops mid-response. Every prefix
        // of a valid payload must be an error, never a shorter book.
        let full = r#"{"bids":[["78913.99","4.17"],["78913.98","0.02"]]}"#;
        for cut in 1..full.len() {
            assert!(
                Json::parse(&full[..cut]).is_err(),
                "prefix of length {cut} must not parse"
            );
        }
        assert!(Json::parse(full).is_ok());
    }
}
