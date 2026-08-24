//! The eight venues in scope.

use crate::TypeError;
use core::fmt;

/// A trading venue.
///
/// A closed enum rather than a string: a typo'd venue name must not silently
/// become a new venue with no fee schedule and no adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Venue {
    /// Binance.
    Binance,
    /// Bybit.
    Bybit,
    /// OKX.
    Okx,
    /// Bitget.
    Bitget,
    /// MEXC.
    Mexc,
    /// Gate.io.
    Gate,
    /// KuCoin.
    Kucoin,
    /// HTX (formerly Huobi).
    Htx,
}

impl Venue {
    /// Every venue, in a stable order. Used for deterministic table output.
    pub const ALL: [Venue; 8] = [
        Venue::Binance,
        Venue::Bybit,
        Venue::Okx,
        Venue::Bitget,
        Venue::Mexc,
        Venue::Gate,
        Venue::Kucoin,
        Venue::Htx,
    ];

    /// Lower-case machine-readable name, used in fixture paths and CLI output.
    pub const fn as_str(self) -> &'static str {
        match self {
            Venue::Binance => "binance",
            Venue::Bybit => "bybit",
            Venue::Okx => "okx",
            Venue::Bitget => "bitget",
            Venue::Mexc => "mexc",
            Venue::Gate => "gate",
            Venue::Kucoin => "kucoin",
            Venue::Htx => "htx",
        }
    }

    /// Parse the name produced by [`Venue::as_str`], case-insensitively.
    ///
    /// Accepts `huobi` as an alias for [`Venue::Htx`] because the rename is
    /// recent and older recorded data uses the old name.
    pub fn parse(s: &str) -> Result<Venue, TypeError> {
        let mut buf = [0u8; 16];
        let raw = s.as_bytes();
        if raw.is_empty() {
            return Err(TypeError::Empty);
        }
        if raw.len() > buf.len() {
            return Err(TypeError::TooLong);
        }
        let mut i = 0;
        while i < raw.len() {
            buf[i] = raw[i].to_ascii_lowercase();
            i += 1;
        }
        let lower = core::str::from_utf8(&buf[..raw.len()]).unwrap_or("");
        match lower {
            "binance" => Ok(Venue::Binance),
            "bybit" => Ok(Venue::Bybit),
            "okx" => Ok(Venue::Okx),
            "bitget" => Ok(Venue::Bitget),
            "mexc" => Ok(Venue::Mexc),
            "gate" | "gateio" => Ok(Venue::Gate),
            "kucoin" => Ok(Venue::Kucoin),
            "htx" | "huobi" => Ok(Venue::Htx),
            _ => Err(TypeError::InvalidChar),
        }
    }
}

impl fmt::Display for Venue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_venues_round_trip_through_their_name() {
        for v in Venue::ALL {
            assert_eq!(Venue::parse(v.as_str()), Ok(v), "round trip failed for {v}");
        }
    }

    #[test]
    fn all_is_complete_and_has_no_duplicates() {
        // If someone adds a variant without extending ALL, deterministic output
        // silently loses a venue. This catches it.
        assert_eq!(Venue::ALL.len(), 8);
        let mut sorted = Venue::ALL;
        sorted.sort();
        for w in sorted.windows(2) {
            assert_ne!(w[0], w[1], "duplicate venue in ALL");
        }
    }

    #[test]
    fn parsing_is_case_insensitive() {
        assert_eq!(Venue::parse("BINANCE"), Ok(Venue::Binance));
        assert_eq!(Venue::parse("BiNaNcE"), Ok(Venue::Binance));
    }

    #[test]
    fn known_aliases_resolve() {
        assert_eq!(Venue::parse("huobi"), Ok(Venue::Htx));
        assert_eq!(Venue::parse("HUOBI"), Ok(Venue::Htx));
        assert_eq!(Venue::parse("gateio"), Ok(Venue::Gate));
    }

    #[test]
    fn unknown_venue_is_rejected_not_invented() {
        assert!(Venue::parse("ftx").is_err());
        assert_eq!(Venue::parse(""), Err(TypeError::Empty));
        assert_eq!(
            Venue::parse("averyveryverylongvenuename"),
            Err(TypeError::TooLong)
        );
    }

    #[test]
    fn names_are_stable_and_lowercase() {
        // Fixture directory layout depends on these exact strings.
        assert_eq!(Venue::Binance.as_str(), "binance");
        assert_eq!(Venue::Htx.as_str(), "htx");
        for v in Venue::ALL {
            assert_eq!(v.as_str(), v.as_str().to_ascii_lowercase());
        }
    }
}
