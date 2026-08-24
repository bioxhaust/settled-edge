//! Fixed-capacity, `Copy`, allocation-free identifier strings.
//!
//! Asset codes and venue symbols are compared, hashed, and copied constantly
//! during a scan. `String` would mean a heap allocation per level per tick.
//! These are inline byte arrays with a validated ASCII charset, so they are
//! `Copy`, `Ord`, and `Hash` with no indirection.
//!
//! [`Asset`] and [`Symbol`] are deliberately *distinct types* rather than
//! aliases of one generic: an asset code (`USDT`) and a venue-native symbol
//! (`BTC-USDT-SWAP`) are not interchangeable, and the compiler should say so.

use crate::TypeError;
use core::fmt;

/// Zero-padding means byte-array ordering is also lexicographic ordering, so
/// the derived `Ord` sorts identifiers the way a human expects.
macro_rules! inline_ascii {
    ($name:ident, $cap:expr, $allow:expr, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
        pub struct $name {
            bytes: [u8; $cap],
            len: u8,
        }

        impl $name {
            /// Maximum number of bytes this type can hold.
            pub const CAPACITY: usize = $cap;

            /// Validate and store, upper-casing ASCII letters.
            ///
            /// Upper-casing is normalisation, not convenience: venues disagree
            /// on case (`btcusdt` vs `BTCUSDT`) and two spellings of one
            /// instrument would silently fail to join across venues.
            pub fn new(s: &str) -> Result<Self, TypeError> {
                let raw = s.as_bytes();
                if raw.is_empty() {
                    return Err(TypeError::Empty);
                }
                if raw.len() > $cap {
                    return Err(TypeError::TooLong);
                }
                let mut bytes = [0u8; $cap];
                let mut i = 0;
                while i < raw.len() {
                    let c = raw[i].to_ascii_uppercase();
                    let allow: fn(u8) -> bool = $allow;
                    if !allow(c) {
                        return Err(TypeError::InvalidChar);
                    }
                    bytes[i] = c;
                    i += 1;
                }
                Ok(Self {
                    bytes,
                    len: raw.len() as u8,
                })
            }

            /// The stored text.
            pub fn as_str(&self) -> &str {
                let end = self.len as usize;
                // `new` admits only single-byte ASCII, so this cannot fail. The
                // fallback keeps the function total: rule 4 forbids `unwrap`.
                core::str::from_utf8(&self.bytes[..end]).unwrap_or("")
            }

            /// Number of bytes stored.
            pub fn len(&self) -> usize {
                self.len as usize
            }

            /// True when no bytes are stored.
            pub fn is_empty(&self) -> bool {
                self.len == 0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({})", stringify!($name), self.as_str())
            }
        }
    };
}

fn is_asset_char(c: u8) -> bool {
    c.is_ascii_uppercase() || c.is_ascii_digit()
}

fn is_symbol_char(c: u8) -> bool {
    c.is_ascii_uppercase() || c.is_ascii_digit() || matches!(c, b'-' | b'_' | b'.' | b'/' | b':')
}

inline_ascii!(
    Asset,
    16,
    is_asset_char,
    "A settlement or trading asset code: `BTC`, `USDT`, `1000PEPE`.\n\nAlphanumeric only, upper-cased. 16 bytes is enough for every code on the\neight venues in scope; leading-multiplier names like `1000PEPE` are why it\nis not 8."
);

inline_ascii!(
    Symbol,
    32,
    is_symbol_char,
    "A venue-native instrument symbol: `BTCUSDT`, `BTC-USDT-SWAP`, `BTCUSDT_UMCBL`.\n\nPermits the separators venues actually use (`-_./:`). Never used as a join\nkey across venues — that is [`crate::InstrumentKey`] — only for talking to\nthe venue that issued it."
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_real_asset_codes() {
        for s in ["BTC", "ETH", "USDT", "USDC", "USD", "1000PEPE", "SHIB", "XBT"] {
            assert_eq!(Asset::new(s).expect("valid").as_str(), s, "failed on {s}");
        }
    }

    #[test]
    fn normalises_case() {
        assert_eq!(Asset::new("btc").expect("valid"), Asset::new("BTC").expect("valid"));
        assert_eq!(
            Symbol::new("btc-usdt-swap").expect("valid").as_str(),
            "BTC-USDT-SWAP"
        );
    }

    #[test]
    fn accepts_real_venue_symbols() {
        for s in [
            "BTCUSDT",
            "BTC-USDT-SWAP",
            "BTCUSDT_UMCBL",
            "BTC_USDT",
            "BTCUSDTM",
            "BTC-USDT",
            "btcusdt",
        ] {
            assert!(Symbol::new(s).is_ok(), "should accept {s}");
        }
    }

    #[test]
    fn rejects_malformed() {
        assert_eq!(Asset::new(""), Err(TypeError::Empty));
        assert_eq!(Asset::new("BTC-USDT"), Err(TypeError::InvalidChar));
        assert_eq!(Asset::new("BTC USDT"), Err(TypeError::InvalidChar));
        assert_eq!(Asset::new("THISASSETNAMEISWAYTOOLONG"), Err(TypeError::TooLong));
        assert_eq!(Symbol::new("BTC USDT"), Err(TypeError::InvalidChar));
        assert_eq!(Symbol::new(""), Err(TypeError::Empty));
    }

    #[test]
    fn capacity_boundaries() {
        let exactly = "A".repeat(Asset::CAPACITY);
        assert!(Asset::new(&exactly).is_ok());
        let one_over = "A".repeat(Asset::CAPACITY + 1);
        assert_eq!(Asset::new(&one_over), Err(TypeError::TooLong));
    }

    #[test]
    fn ordering_is_lexicographic() {
        let btc = Asset::new("BTC").expect("valid");
        let btcusd = Asset::new("BTCUSD").expect("valid");
        let eth = Asset::new("ETH").expect("valid");
        assert!(btc < btcusd, "shorter prefix must sort first");
        assert!(btcusd < eth);
    }

    #[test]
    fn distinct_values_do_not_collide_after_padding() {
        // Ensures `len` participates in equality and the padding is consistent.
        assert_ne!(Asset::new("BTC").expect("v"), Asset::new("BTCB").expect("v"));
        assert_eq!(Asset::new("BTC").expect("v"), Asset::new("BTC").expect("v"));
    }

    #[test]
    fn display_round_trips() {
        let a = Asset::new("1000PEPE").expect("valid");
        assert_eq!(a.to_string(), "1000PEPE");
        assert_eq!(format!("{a:?}"), "Asset(1000PEPE)");
        assert_eq!(a.len(), 8);
        assert!(!a.is_empty());
    }
}
