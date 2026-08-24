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
//! because all three venues answer with JSON containing a funding rate and two
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

pub mod bybit;

use arb_venue::VenueError;

use crate::http::HttpError;
use crate::json::JsonError;

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
}
