#![forbid(unsafe_code)]
//! Live adapters, one module per venue.
//!
//! Each module in here implements [`arb_venue::ExchangeAdapter`] against a
//! [`crate::http::Transport`] and a [`crate::clock::Clock`], and produces
//! snapshots stamped [`arb_types::Provenance::LiveMeasured`].
//!
//! # Why there is no shared "generic exchange" base
//!
//! It is tempting to write one adapter parameterised by a table of field names,
//! because every venue answers with JSON containing a funding rate and two
//! sides of a book. The shapes only look alike from a distance:
//!
//! - Bybit puts the funding interval in **minutes** (`fundingInterval: 480`) in
//!   one endpoint and in **hours** (`fundingIntervalHour: "8"`) in another.
//! - OKX's `fundingTime` is the *upcoming* settlement while `nextFundingTime` is
//!   the one after it, so matching on the more obvious-looking name is wrong by a
//!   whole interval.
//! - Binance's `lastFundingRate`, despite the name, is the *estimate for the
//!   next* settlement, not the last settled one.
//!
//! A shared table would have to encode all of that as exceptions, and an
//! exception in a config table is much easier to get wrong than a line of code
//! with a comment next to it. So: separate modules, duplicated structure,
//! per-venue tests. The shared part is the small amount below.
//!
//! # What is shared, and the line the sharing stops at
//!
//! Two kinds of thing live in this file:
//!
//! - **Seam mapping** — [`transport_error`] and [`wire_error`] decide *once* how
//!   a transport or payload failure becomes a [`VenueError`], because retry
//!   policy must not vary by venue.
//! - **JSON field access** — [`int_field`], [`dec_field`], [`str_field`] and
//!   [`opt_dec_field`]. These carry no venue semantics whatsoever: they answer
//!   "is this field a string or a bare number" and "did the venue leave it
//!   blank", which every venue gets asked and none of them answers specially.
//!
//! What deliberately does **not** live here is anything that reads a *shape*. A
//! shared `levels()` is the obvious candidate and it is the trap: a book side is
//! `[["p","q"]]` on one venue, a four-element array with order counts on
//! another, and an array of objects on a third. One function covering all of
//! them takes a shape parameter, and a shape parameter is the config table this
//! module exists to avoid. Each venue parses its own ladder, in its own file,
//! under its own test.
//!
//! # Module names are [`arb_types::Venue::as_str`]
//!
//! `venues::gate` is [`arb_types::Venue::Gate`], `venues::htx` is
//! [`arb_types::Venue::Htx`]. Every one of the eight is declared below whether or
//! not its adapter is finished, so that adding a venue is a change to one file
//! and never to this one.

pub mod binance;
pub mod bitget;
pub mod bybit;
pub mod gate;
pub mod htx;
pub mod kucoin;
pub mod mexc;
pub mod okx;

use arb_venue::VenueError;

use crate::http::HttpError;
use crate::json::{Json, JsonError};

/// `curl` exit codes where trying again in a moment could plausibly work.
///
/// 6 host resolution, 7 connect, 28 timeout, 35 TLS handshake, 52 empty reply,
/// 55 send failed, 56 receive failed. All of these are "the network was unwell",
/// as opposed to 60 (certificate could not be verified) or 63 (size cap), which
/// will fail identically on every attempt and where retrying only hides a real
/// problem.
///
/// 35 is included deliberately even though a TLS failure *sounds* permanent: in
/// practice it is most often a truncated handshake on a flaky link, and a
/// genuinely bad certificate surfaces as 60.
const RETRYABLE_CURL_EXITS: [i32; 7] = [6, 7, 28, 35, 52, 55, 56];

/// Map a transport failure onto the venue seam, deciding retryability once.
///
/// The decision lives here rather than at each call site so that every adapter
/// backs off on the same rule. Getting it wrong in the lenient direction is
/// expensive in a specific way: retrying a 400 burns rate-limit budget that the
/// retryable cases need, and on a venue that rate-limits by IP that turns one
/// broken symbol into a total outage.
pub(crate) fn transport_error(e: HttpError) -> VenueError {
    let retryable = match &e {
        // A refused scheme is our bug, not the network's.
        HttpError::NotHttps { .. } => false,
        // curl is not installed. It will still not be installed in a second.
        HttpError::LauncherUnavailable { .. } => false,
        HttpError::TransportFailed { code, .. } => {
            code.is_some_and(|c| RETRYABLE_CURL_EXITS.contains(&c))
        }
        HttpError::Status { code, .. } => {
            // 429 means slow down, which is exactly what a retry with backoff
            // does. 5xx is the venue's problem and usually brief. Every 4xx
            // other than 429 describes a request that will not become valid.
            *code == 429 || (500..600).contains(code)
        }
        HttpError::TooLarge { .. } => false,
        HttpError::MalformedTransportOutput => false,
        HttpError::NotUtf8 => false,
        // The venue answered; the recorder could not stamp or store the frame.
        // Retrying would re-fetch a response that would fail to be recorded for
        // the same reason, and spend rate-limit budget doing it.
        HttpError::NotRecorded { .. } => false,
    };
    VenueError::Transport {
        retryable,
        detail: e.to_string(),
    }
}

/// Map a payload failure onto the venue seam.
///
/// Always [`VenueError::Wire`], which is never retryable: a venue that renamed a
/// field will answer with the new name forever.
pub(crate) fn wire_error(e: JsonError) -> VenueError {
    VenueError::Wire(e.to_string())
}

/// A payload that parsed but said something this adapter cannot use.
pub(crate) fn wire(msg: impl Into<String>) -> VenueError {
    VenueError::Wire(msg.into())
}

/// An integer field, whether the venue quoted it or not.
///
/// Venues are inconsistent about this *within a single response* — Bybit quotes
/// every price and size but sends `retCode`, `fundingInterval` and the book's
/// `ts` as bare numbers — so a reader that demanded one or the other would need
/// a per-field table of which is which. [`Json::decimal`] accepts both and hands
/// back the text; this turns that text into an integer or says why it could not.
pub(crate) fn int_field(node: &Json, key: &str) -> Result<i64, VenueError> {
    let text = node
        .field(key)
        .and_then(|v| v.decimal())
        .map_err(wire_error)?;
    text.parse::<i64>()
        .map_err(|_| wire(format!("field {key:?} is not an integer: {text:?}")))
}

/// A decimal field as text, ready for `arb-num`.
///
/// The text is handed on verbatim. Nothing here parses it into a number, which
/// is the point: `arb-num` is the only thing in this workspace allowed to turn a
/// venue's digits into a quantity, and it does so without ever seeing an `f64`.
pub(crate) fn dec_field<'a>(node: &'a Json, key: &str) -> Result<&'a str, VenueError> {
    node.field(key)
        .and_then(|v| v.decimal())
        .map_err(wire_error)
}

/// A field that must be a JSON string: a symbol, an asset code, a status.
///
/// Deliberately stricter than [`dec_field`]. `decimal()` would accept `"Trading"`
/// and `"BTCUSDT"` happily, since it only hands the text back — so using it here
/// would mean a `status` that became a bare enum number, or a `symbol` that became
/// an integer id, parsed without complaint and then compared against a string it
/// could never equal. That failure would surface as "the venue does not list
/// BTCUSDT", which is the least informative possible symptom.
pub(crate) fn str_field<'a>(node: &'a Json, key: &str) -> Result<&'a str, VenueError> {
    node.field(key).and_then(|v| v.string()).map_err(wire_error)
}

/// A decimal field that the venue may leave blank.
///
/// Absence has two spellings on the wire and they are not interchangeable
/// between venues: some write `null` for an inapplicable field, some write `""`
/// (Bybit's `basisRate` and `predictedDeliveryPrice` on a perpetual, for
/// instance), and some omit the key. [`Json::is_absent`] knows the first two and
/// the `None` arm covers the third, so all three arrive here as `Ok(None)`.
///
/// A caller must decide what `None` *means* — whether a missing mark price is a
/// blocking [`arb_types::QualityFlags::PRICE_UNAVAILABLE`] or an informational
/// gap is a per-field judgement and stays with the adapter.
pub(crate) fn opt_dec_field<'a>(node: &'a Json, key: &str) -> Result<Option<&'a str>, VenueError> {
    match node.get(key) {
        None => Ok(None),
        Some(v) if v.is_absent() => Ok(None),
        Some(v) => v.decimal().map(Some).map_err(wire_error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rate_limit_is_retryable_and_a_bad_request_is_not() {
        let limited = transport_error(HttpError::Status {
            code: 429,
            body_prefix: "too many requests".to_string(),
        });
        assert_eq!(
            limited,
            VenueError::Transport {
                retryable: true,
                detail: "HTTP 429: too many requests".to_string(),
            }
        );

        let bad = transport_error(HttpError::Status {
            code: 400,
            body_prefix: "unknown symbol".to_string(),
        });
        match bad {
            VenueError::Transport { retryable, .. } => assert!(!retryable),
            other => panic!("expected Transport, got {other:?}"),
        }
    }

    #[test]
    fn a_timeout_is_retryable_and_a_bad_certificate_is_not() {
        let timeout = transport_error(HttpError::TransportFailed {
            code: Some(28),
            detail: "operation timed out".to_string(),
        });
        match timeout {
            VenueError::Transport { retryable, .. } => assert!(retryable),
            other => panic!("expected Transport, got {other:?}"),
        }

        // 60: peer certificate cannot be authenticated. Retrying this would poll
        // a possible interception attempt in a loop.
        let cert = transport_error(HttpError::TransportFailed {
            code: Some(60),
            detail: "SSL certificate problem".to_string(),
        });
        match cert {
            VenueError::Transport { retryable, .. } => assert!(!retryable),
            other => panic!("expected Transport, got {other:?}"),
        }
    }

    #[test]
    fn a_missing_curl_is_not_something_to_retry() {
        let e = transport_error(HttpError::LauncherUnavailable {
            detail: "No such file or directory".to_string(),
        });
        match e {
            VenueError::Transport { retryable, detail } => {
                assert!(!retryable);
                assert!(detail.contains("could not run curl"), "detail: {detail}");
            }
            other => panic!("expected Transport, got {other:?}"),
        }
    }

    #[test]
    fn a_renamed_field_is_never_retryable() {
        let e = wire_error(JsonError::MissingField {
            key: "fundingRate".to_string(),
        });
        match e {
            VenueError::Wire(detail) => assert!(detail.contains("fundingRate"), "detail: {detail}"),
            other => panic!("expected Wire, got {other:?}"),
        }
    }

    /// The quoted/bare inconsistency every adapter has to tolerate, in one test.
    #[test]
    fn a_number_is_read_whether_the_venue_quoted_it_or_not() {
        let doc = Json::parse(r#"{"quoted":"480","bare":480,"nope":"eight"}"#).expect("parse");
        assert_eq!(int_field(&doc, "quoted"), Ok(480));
        assert_eq!(int_field(&doc, "bare"), Ok(480));
        assert_eq!(dec_field(&doc, "quoted"), Ok("480"));
        assert_eq!(dec_field(&doc, "bare"), Ok("480"));

        // Not an integer is an error and names the field, because the alternative
        // is a silent zero that reads as a real market value.
        match int_field(&doc, "nope") {
            Err(VenueError::Wire(d)) => assert!(d.contains("nope"), "detail: {d}"),
            other => panic!("expected Wire, got {other:?}"),
        }
        // A field nobody sent is an error too, not a default.
        assert!(int_field(&doc, "absent").is_err());
    }

    /// `str_field` is the strict one, and this is why it exists.
    #[test]
    fn a_status_that_became_a_number_is_refused_rather_than_compared() {
        let doc = Json::parse(r#"{"status":1,"symbol":"BTCUSDT"}"#).expect("parse");
        // `decimal()` is happy to hand back "1", and a caller comparing that
        // against "Trading" would silently skip every instrument.
        assert_eq!(dec_field(&doc, "status"), Ok("1"));
        assert!(str_field(&doc, "status").is_err());
        assert_eq!(str_field(&doc, "symbol"), Ok("BTCUSDT"));
    }

    /// All three spellings of absence collapse to `None`; a real value does not.
    #[test]
    fn blank_null_and_missing_are_all_absent_and_zero_is_not() {
        let doc = Json::parse(r#"{"blank":"","nulled":null,"zero":"0","real":"79033.51"}"#)
            .expect("parse");
        assert_eq!(opt_dec_field(&doc, "blank"), Ok(None));
        assert_eq!(opt_dec_field(&doc, "nulled"), Ok(None));
        assert_eq!(opt_dec_field(&doc, "omitted"), Ok(None));

        // The distinction that matters: `"0"` is a value the venue chose to send.
        // Collapsing it into `None` would turn "no scheduled settlement" and "we
        // could not read the field" into the same answer.
        assert_eq!(opt_dec_field(&doc, "zero"), Ok(Some("0")));
        assert_eq!(opt_dec_field(&doc, "real"), Ok(Some("79033.51")));
    }
}
