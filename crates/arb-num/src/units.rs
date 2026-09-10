//! The five money types and their dimensionally-typed operations.
//!
//! Cross-type operations are named after their *dimensions*, not their
//! implementation: `Price::mul_qty` returns [`Money`], and `Money::div_qty`
//! returns [`Price`] (that second one is VWAP). This makes "the fee was charged
//! on the wrong quantity" and "the notional was computed from the wrong side"
//! into compile errors rather than a number that is quietly 4 bps optimistic.

use crate::decimal::{format_scaled, parse_scaled, parse_scaled_rounded};
use crate::error::NumError;
use crate::muldiv::mul_div;
use crate::round::RoundMode;
use core::fmt;

/// Decimal places carried by [`Price`], [`Qty`], [`Money`] and [`Rate`].
pub const SCALE_DIGITS: u32 = 12;
/// The multiplier implied by [`SCALE_DIGITS`].
pub const SCALE: i128 = 1_000_000_000_000;

/// Decimal places carried by [`Bps`].
pub const BPS_SCALE_DIGITS: u32 = 8;
/// The multiplier implied by [`BPS_SCALE_DIGITS`].
pub const BPS_SCALE: i64 = 100_000_000;

macro_rules! fixed_i128 {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        ///
        /// Backed by `i128` at scale 1e12. See the crate docs for why there are
        /// no arithmetic operator impls on this type.
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
        pub struct $name(i128);

        impl $name {
            /// The additive identity.
            pub const ZERO: Self = Self(0);
            /// The value `1.0`.
            pub const ONE: Self = Self(SCALE);
            /// Smallest representable value.
            pub const MIN: Self = Self(i128::MIN);
            /// Largest representable value.
            pub const MAX: Self = Self(i128::MAX);
            /// One unit in the last place: `1e-12`.
            pub const EPSILON: Self = Self(1);

            /// Wrap a pre-scaled integer. The caller asserts the scaling.
            #[inline]
            pub const fn from_raw(raw: i128) -> Self {
                Self(raw)
            }

            /// The underlying scale-1e12 integer.
            #[inline]
            pub const fn raw(self) -> i128 {
                self.0
            }

            /// Exact conversion from a whole number.
            pub fn from_int(n: i64) -> Result<Self, NumError> {
                i128::from(n)
                    .checked_mul(SCALE)
                    .map(Self)
                    .ok_or(NumError::Overflow)
            }

            /// Parse an exchange-supplied decimal string.
            ///
            /// Refuses to truncate: see [`NumError::PrecisionLoss`].
            pub fn parse(s: &str) -> Result<Self, NumError> {
                parse_scaled(s, SCALE_DIGITS).map(Self)
            }

            /// Parse an exchange-supplied decimal string, rounding digits below
            /// scale 1e12 rather than refusing them.
            ///
            /// [`Self::parse`] is the correct function almost everywhere, and the
            /// refusal it performs is load-bearing: more decimal places than this
            /// type carries usually means the field was misidentified, not that the
            /// venue is being precise. Reach for this only when a specific field's
            /// published precision genuinely exceeds [`SCALE_DIGITS`] — see
            /// [`crate::parse_scaled_rounded`], which names the one such field in
            /// this workspace today and states the resulting error bound.
            pub fn parse_rounded(s: &str, mode: RoundMode) -> Result<Self, NumError> {
                parse_scaled_rounded(s, SCALE_DIGITS, mode).map(Self)
            }

            /// Checked addition.
            #[inline]
            pub fn checked_add(self, rhs: Self) -> Result<Self, NumError> {
                self.0.checked_add(rhs.0).map(Self).ok_or(NumError::Overflow)
            }

            /// Checked subtraction.
            #[inline]
            pub fn checked_sub(self, rhs: Self) -> Result<Self, NumError> {
                self.0.checked_sub(rhs.0).map(Self).ok_or(NumError::Overflow)
            }

            /// Checked negation. Fails only on [`Self::MIN`].
            #[inline]
            pub fn checked_neg(self) -> Result<Self, NumError> {
                self.0.checked_neg().map(Self).ok_or(NumError::Overflow)
            }

            /// Checked absolute value. Fails only on [`Self::MIN`].
            #[inline]
            pub fn checked_abs(self) -> Result<Self, NumError> {
                self.0.checked_abs().map(Self).ok_or(NumError::Overflow)
            }

            /// Multiply by a plain integer count. Exact, no rounding possible.
            #[inline]
            pub fn checked_mul_int(self, n: i64) -> Result<Self, NumError> {
                self.0
                    .checked_mul(i128::from(n))
                    .map(Self)
                    .ok_or(NumError::Overflow)
            }

            /// Divide by a plain integer count.
            #[inline]
            pub fn checked_div_int(self, n: i64, mode: RoundMode) -> Result<Self, NumError> {
                mul_div(self.0, 1, i128::from(n), mode).map(Self)
            }

            /// `-1`, `0`, or `1`.
            #[inline]
            pub const fn signum(self) -> i32 {
                if self.0 > 0 {
                    1
                } else if self.0 < 0 {
                    -1
                } else {
                    0
                }
            }

            /// True when exactly zero.
            #[inline]
            pub const fn is_zero(self) -> bool {
                self.0 == 0
            }

            /// True when strictly greater than zero.
            #[inline]
            pub const fn is_positive(self) -> bool {
                self.0 > 0
            }

            /// True when strictly less than zero.
            #[inline]
            pub const fn is_negative(self) -> bool {
                self.0 < 0
            }

            /// Format with exactly `places` decimals, for tables.
            pub fn to_places(self, places: u32) -> String {
                crate::decimal::format_scaled_places(self.0, SCALE_DIGITS, places)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&format_scaled(self.0, SCALE_DIGITS))
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                // Show the decimal value, not the raw integer: a failing
                // assertion that prints `68123450000000000` costs the reader a
                // minute of mental long division.
                write!(f, "{}({})", stringify!($name), format_scaled(self.0, SCALE_DIGITS))
            }
        }
    };
}

fixed_i128!(Price, "Quote-currency units per one base unit.");
fixed_i128!(Qty, "An amount: base units, coins, or contracts.");
fixed_i128!(Money, "An amount of quote or settlement currency.");
fixed_i128!(
    Rate,
    "A dimensionless ratio. `0.0001` is one basis point, not one percent."
);

// ---------------------------------------------------------------------------
// Dimensioned cross-operations.
//
// Every one of these is a mul_div, so every one can lose a sub-unit remainder
// and therefore every one demands an explicit RoundMode. There is no default.
// ---------------------------------------------------------------------------

impl Price {
    /// `price * qty` -> notional. The definition of notional in this workspace.
    pub fn mul_qty(self, qty: Qty, mode: RoundMode) -> Result<Money, NumError> {
        mul_div(self.0, qty.0, SCALE, mode).map(Money)
    }

    /// Scale a price by a ratio, e.g. applying a slippage or funding factor.
    pub fn mul_rate(self, rate: Rate, mode: RoundMode) -> Result<Price, NumError> {
        mul_div(self.0, rate.0, SCALE, mode).map(Price)
    }

    /// `self / other` as a ratio. This is how basis is computed: a mark price
    /// divided by an index price yields a [`Rate`], not another `Price`.
    pub fn div_price(self, other: Price, mode: RoundMode) -> Result<Rate, NumError> {
        mul_div(self.0, SCALE, other.0, mode).map(Rate)
    }

    /// Reinterpret a price as a money amount. Needed for inverse contracts,
    /// where the contract size is denominated in the quote currency.
    ///
    /// Deliberately verbose: crossing a dimension boundary should be visible in
    /// a diff.
    #[inline]
    pub const fn reinterpret_as_money(self) -> Money {
        Money(self.0)
    }
}

impl Qty {
    /// `qty * price` -> notional. Same arithmetic as [`Price::mul_qty`],
    /// provided so call sites can read in their natural order.
    pub fn mul_price(self, price: Price, mode: RoundMode) -> Result<Money, NumError> {
        mul_div(self.0, price.0, SCALE, mode).map(Money)
    }

    /// Scale a quantity by a ratio, e.g. a fill fraction.
    pub fn mul_rate(self, rate: Rate, mode: RoundMode) -> Result<Qty, NumError> {
        mul_div(self.0, rate.0, SCALE, mode).map(Qty)
    }

    /// `contracts * contract_size` -> base units. Both operands are `Qty`
    /// because a contract count and a contract size are both amounts; the
    /// distinction is enforced in `arb-types`, not here.
    pub fn mul_qty(self, other: Qty, mode: RoundMode) -> Result<Qty, NumError> {
        mul_div(self.0, other.0, SCALE, mode).map(Qty)
    }

    /// `base units / contract_size` -> contract count. The dimensional inverse
    /// of [`Qty::mul_qty`].
    ///
    /// Deliberately *not* [`Qty::div_qty`], which divides two amounts of the
    /// same thing and yields a dimensionless [`Rate`] such as filled/requested.
    /// Dividing an amount by a *size per unit* yields another amount — a count —
    /// and conflating the two is how a contract count ends up expressed as a
    /// fill fraction. See `decisions/014`.
    pub fn div_size(self, size: Qty, mode: RoundMode) -> Result<Qty, NumError> {
        mul_div(self.0, SCALE, size.0, mode).map(Qty)
    }

    /// `self / other` as a ratio, e.g. filled / requested.
    pub fn div_qty(self, other: Qty, mode: RoundMode) -> Result<Rate, NumError> {
        mul_div(self.0, SCALE, other.0, mode).map(Rate)
    }

    /// Reinterpret a quantity as a money amount, for quote-denominated sizes.
    #[inline]
    pub const fn reinterpret_as_money(self) -> Money {
        Money(self.0)
    }
}

impl Money {
    /// `notional * rate` -> money. This is how every fee is computed.
    pub fn mul_rate(self, rate: Rate, mode: RoundMode) -> Result<Money, NumError> {
        mul_div(self.0, rate.0, SCALE, mode).map(Money)
    }

    /// `notional / price` -> size. This is how a target notional becomes an
    /// order quantity.
    pub fn div_price(self, price: Price, mode: RoundMode) -> Result<Qty, NumError> {
        mul_div(self.0, SCALE, price.0, mode).map(Qty)
    }

    /// `notional / size` -> price. **This is VWAP.** Consuming levels of a book
    /// accumulates `Money` and `Qty`; dividing them gives the achieved price.
    pub fn div_qty(self, qty: Qty, mode: RoundMode) -> Result<Price, NumError> {
        mul_div(self.0, SCALE, qty.0, mode).map(Price)
    }

    /// `self / other` as a ratio, e.g. profit over margin posted.
    pub fn div_money(self, other: Money, mode: RoundMode) -> Result<Rate, NumError> {
        mul_div(self.0, SCALE, other.0, mode).map(Rate)
    }

    /// Reinterpret a money amount as a quantity, for quote-denominated sizes.
    #[inline]
    pub const fn reinterpret_as_qty(self) -> Qty {
        Qty(self.0)
    }
}

impl Rate {
    /// Compose two ratios.
    pub fn mul_rate(self, other: Rate, mode: RoundMode) -> Result<Rate, NumError> {
        mul_div(self.0, other.0, SCALE, mode).map(Rate)
    }

    /// `self / other`.
    pub fn div_rate(self, other: Rate, mode: RoundMode) -> Result<Rate, NumError> {
        mul_div(self.0, SCALE, other.0, mode).map(Rate)
    }

    /// Reciprocal, `1 / self`.
    pub fn recip(self, mode: RoundMode) -> Result<Rate, NumError> {
        mul_div(SCALE, SCALE, self.0, mode).map(Rate)
    }

    /// Convert to basis points. Narrows `i128` to `i64`, hence fallible.
    ///
    /// Lossless: a `Rate` and a `Bps` denoting the same quantity have the same
    /// raw integer. See crate docs, decision 002.
    pub fn to_bps(self) -> Result<Bps, NumError> {
        i64::try_from(self.0)
            .map(Bps)
            .map_err(|_| NumError::Overflow)
    }

    /// Build a ratio from basis points. Always exact.
    #[inline]
    pub const fn from_bps(bps: Bps) -> Rate {
        Rate(bps.0 as i128)
    }
}

/// Basis points, with eight decimal places of sub-bp precision.
///
/// One basis point is `Bps::from_raw(100_000_000)`. Fee tiers like 0.018% (1.8
/// bps) and our ~12 bps edge are both exactly representable.
///
/// Backed by `i64` at scale 1e8, chosen so that the raw integer is *identical*
/// to the raw integer of the equivalent [`Rate`]. Read decision 002 before
/// changing either scale.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Bps(i64);

impl Bps {
    /// Zero basis points.
    pub const ZERO: Self = Self(0);
    /// Exactly one basis point.
    pub const ONE: Self = Self(BPS_SCALE);
    /// One hundred basis points, i.e. one percent.
    pub const ONE_PERCENT: Self = Self(100 * BPS_SCALE);

    /// Wrap a pre-scaled integer (scale 1e8).
    #[inline]
    pub const fn from_raw(raw: i64) -> Self {
        Self(raw)
    }

    /// The underlying scale-1e8 integer.
    #[inline]
    pub const fn raw(self) -> i64 {
        self.0
    }

    /// From a whole number of basis points: `from_whole(12)` is 12 bps.
    pub fn from_whole(bps: i64) -> Result<Self, NumError> {
        bps.checked_mul(BPS_SCALE).map(Self).ok_or(NumError::Overflow)
    }

    /// Parse a string *already denominated in basis points*: `"12.5"` is
    /// 12.5 bps.
    ///
    /// Contrast [`Bps::parse_ratio`]. The two are named this asymmetrically on
    /// purpose — picking the wrong one is a factor-of-10000 error, and that is
    /// the bug this crate exists to prevent.
    pub fn parse_bps(s: &str) -> Result<Self, NumError> {
        let raw = parse_scaled(s, BPS_SCALE_DIGITS)?;
        i64::try_from(raw).map(Self).map_err(|_| NumError::Overflow)
    }

    /// Parse a string denominated as a *ratio* and express it in basis points:
    /// `"0.0001"` is 1 bp.
    ///
    /// This is the one to use for exchange fee-rate and funding-rate fields,
    /// which are ratios.
    pub fn parse_ratio(s: &str) -> Result<Self, NumError> {
        Rate::parse(s)?.to_bps()
    }

    /// As a dimensionless ratio. Exact and free.
    #[inline]
    pub const fn to_rate(self) -> Rate {
        Rate(self.0 as i128)
    }

    /// Checked addition.
    #[inline]
    pub fn checked_add(self, rhs: Self) -> Result<Self, NumError> {
        self.0.checked_add(rhs.0).map(Self).ok_or(NumError::Overflow)
    }

    /// Checked subtraction.
    #[inline]
    pub fn checked_sub(self, rhs: Self) -> Result<Self, NumError> {
        self.0.checked_sub(rhs.0).map(Self).ok_or(NumError::Overflow)
    }

    /// Checked negation.
    #[inline]
    pub fn checked_neg(self) -> Result<Self, NumError> {
        self.0.checked_neg().map(Self).ok_or(NumError::Overflow)
    }

    /// Multiply by a plain integer count.
    #[inline]
    pub fn checked_mul_int(self, n: i64) -> Result<Self, NumError> {
        self.0.checked_mul(n).map(Self).ok_or(NumError::Overflow)
    }

    /// True when exactly zero.
    #[inline]
    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }

    /// True when strictly less than zero.
    #[inline]
    pub const fn is_negative(self) -> bool {
        self.0 < 0
    }

    /// True when strictly greater than zero.
    ///
    /// Present because every other type in this module has it, and its absence
    /// pushed callers into writing `!x.is_negative() && !x.is_zero()`, which
    /// reads as the same thing and is one dropped clause away from treating zero
    /// as positive. See `decisions/014`.
    #[inline]
    pub const fn is_positive(self) -> bool {
        self.0 > 0
    }

    /// Format with exactly `places` decimals, for tables.
    pub fn to_places(self, places: u32) -> String {
        crate::decimal::format_scaled_places(i128::from(self.0), BPS_SCALE_DIGITS, places)
    }
}

impl fmt::Display for Bps {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&format_scaled(i128::from(self.0), BPS_SCALE_DIGITS))
    }
}

impl fmt::Debug for Bps {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Bps({})", format_scaled(i128::from(self.0), BPS_SCALE_DIGITS))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_constants_are_consistent() {
        assert_eq!(SCALE, crate::decimal::pow10_i128(SCALE_DIGITS).expect("fits"));
        assert_eq!(
            i128::from(BPS_SCALE),
            crate::decimal::pow10_i128(BPS_SCALE_DIGITS).expect("fits")
        );
        assert_eq!(Price::ONE.raw(), SCALE);
        assert_eq!(Rate::ONE.raw(), SCALE);
    }

    #[test]
    fn bps_and_rate_share_a_raw_representation() {
        // The whole point of decision 002. One basis point:
        assert_eq!(Bps::ONE.raw() as i128, Rate::parse("0.0001").expect("ok").raw());
        // One percent:
        assert_eq!(
            Bps::ONE_PERCENT.raw() as i128,
            Rate::parse("0.01").expect("ok").raw()
        );
        // Round-trip in both directions, no rounding mode needed.
        let r = Rate::parse("0.000375").expect("ok");
        assert_eq!(Rate::from_bps(r.to_bps().expect("fits")), r);
    }

    #[test]
    fn bps_constructors_are_not_interchangeable() {
        // 1 bp expressed two ways must agree.
        assert_eq!(Bps::parse_bps("1").expect("ok"), Bps::ONE);
        assert_eq!(Bps::parse_ratio("0.0001").expect("ok"), Bps::ONE);
        // And the wrong one is off by exactly 10_000 — the error we are
        // designing against. Asserted so the asymmetry is documented in code.
        assert_eq!(
            Bps::parse_bps("0.0001").expect("ok").raw() * 10_000,
            Bps::parse_ratio("0.0001").expect("ok").raw()
        );
    }

    #[test]
    fn fee_tiers_are_exactly_representable() {
        // 0.018% VIP taker = 1.8 bps.
        assert_eq!(Bps::parse_ratio("0.00018").expect("ok"), Bps::parse_bps("1.8").expect("ok"));
        // 0.1% spot taker = 10 bps.
        assert_eq!(Bps::parse_ratio("0.001").expect("ok"), Bps::from_whole(10).expect("ok"));
        // 0.04% perp taker = 4 bps.
        assert_eq!(Bps::parse_ratio("0.0004").expect("ok"), Bps::from_whole(4).expect("ok"));
        // Our claimed edge.
        assert_eq!(Bps::from_whole(12).expect("ok").to_rate(), Rate::parse("0.0012").expect("ok"));
    }

    #[test]
    fn notional_is_price_times_qty() {
        // 0.5 BTC at 68,123.45 = 34,061.725 USD. Computed by hand.
        let p = Price::parse("68123.45").expect("ok");
        let q = Qty::parse("0.5").expect("ok");
        let n = p.mul_qty(q, RoundMode::Trunc).expect("ok");
        assert_eq!(n, Money::parse("34061.725").expect("ok"));
        // Commutative through the other spelling.
        assert_eq!(q.mul_price(p, RoundMode::Trunc).expect("ok"), n);
    }

    #[test]
    fn vwap_is_money_over_qty() {
        // Fill 1 unit at 100 and 2 units at 103 -> 306 notional / 3 units = 102.
        let notional = Money::parse("306").expect("ok");
        let filled = Qty::parse("3").expect("ok");
        assert_eq!(
            notional.div_qty(filled, RoundMode::Trunc).expect("ok"),
            Price::parse("102").expect("ok")
        );
    }

    #[test]
    fn sizing_is_money_over_price() {
        // $10,000 at $68,123.45 = 0.146791... BTC. Hand-computed to 12 places:
        // 10000 / 68123.45 = 0.146791224...
        let m = Money::parse("10000").expect("ok");
        let p = Price::parse("68123.45").expect("ok");
        let q = m.div_price(p, RoundMode::Trunc).expect("ok");
        // Verify by multiplying back: q * p must not exceed the budget.
        let back = q.mul_price(p, RoundMode::AwayFromZero).expect("ok");
        assert!(back <= m, "truncated sizing must never exceed the budget");
        // And one more unit of size would exceed it.
        let q_plus = Qty::from_raw(q.raw() + 1);
        let over = q_plus.mul_price(p, RoundMode::Trunc).expect("ok");
        assert!(over >= m);
    }

    #[test]
    fn fee_is_charged_on_notional() {
        // 4 bps taker on 34,061.725 USD = 13.62469 USD. By hand:
        // 34061.725 * 0.0004 = 13.62469
        let notional = Money::parse("34061.725").expect("ok");
        let fee_rate = Bps::from_whole(4).expect("ok").to_rate();
        let fee = notional.mul_rate(fee_rate, RoundMode::COST).expect("ok");
        assert_eq!(fee, Money::parse("13.62469").expect("ok"));
    }

    #[test]
    fn cost_rounding_never_favours_us() {
        // A notional that produces a non-terminating fee remainder.
        let notional = Money::from_raw(1); // 1e-12 USD
        let fee_rate = Bps::from_whole(4).expect("ok").to_rate(); // 0.0004
        // True fee is 4e-16, far below one representable unit.
        let cost = notional.mul_rate(fee_rate, RoundMode::COST).expect("ok");
        let revenue = notional.mul_rate(fee_rate, RoundMode::REVENUE).expect("ok");
        assert_eq!(cost, Money::from_raw(1), "a cost must never round to zero");
        assert_eq!(revenue, Money::ZERO);
        assert!(cost > revenue);
    }

    #[test]
    fn basis_is_a_ratio_not_a_price() {
        // mark 68200, index 68000 -> ratio 1.002941176470... (68200/68000)
        let mark = Price::parse("68200").expect("ok");
        let index = Price::parse("68000").expect("ok");
        let ratio = mark.div_price(index, RoundMode::Trunc).expect("ok");
        // 68200/68000 = 1.00294117647058823529... truncated at 12 places:
        assert_eq!(ratio, Rate::parse("1.002941176470").expect("ok"));
        // Basis in bps = (ratio - 1) = 29.4117647 bps.
        let basis = ratio.checked_sub(Rate::ONE).expect("ok");
        assert_eq!(basis.to_bps().expect("fits"), Bps::parse_bps("29.4117647").expect("ok"));
    }

    #[test]
    fn apr_extrapolation_is_exact_integer_multiplication() {
        // 0.01% per 8h. 8h intervals per year = 8760/8 = 1095.
        // 0.0001 * 1095 = 0.1095 = 10.95%
        let funding = Rate::parse("0.0001").expect("ok");
        let apr = funding.checked_mul_int(1095).expect("ok");
        assert_eq!(apr, Rate::parse("0.1095").expect("ok"));
        assert_eq!(apr.to_bps().expect("fits"), Bps::from_whole(1095).expect("ok"));
    }

    #[test]
    fn reciprocal_round_trips_on_exact_powers() {
        let r = Rate::parse("0.5").expect("ok");
        assert_eq!(r.recip(RoundMode::Trunc).expect("ok"), Rate::parse("2").expect("ok"));
    }

    #[test]
    fn overflow_is_an_error_not_a_wrap() {
        assert_eq!(Price::MAX.checked_add(Price::EPSILON), Err(NumError::Overflow));
        assert_eq!(Price::MIN.checked_neg(), Err(NumError::Overflow));
        assert_eq!(Price::MIN.checked_abs(), Err(NumError::Overflow));
        assert_eq!(
            Price::MAX.mul_qty(Qty::from_int(2).expect("ok"), RoundMode::Trunc),
            Err(NumError::Overflow)
        );
    }

    #[test]
    fn division_by_zero_is_an_error_not_a_panic() {
        assert_eq!(
            Money::ONE.div_price(Price::ZERO, RoundMode::Trunc),
            Err(NumError::DivByZero)
        );
        assert_eq!(
            Money::ONE.div_qty(Qty::ZERO, RoundMode::Trunc),
            Err(NumError::DivByZero)
        );
        assert_eq!(Rate::ZERO.recip(RoundMode::Trunc), Err(NumError::DivByZero));
    }

    #[test]
    fn large_notional_does_not_overflow_the_intermediate() {
        // 13 million units at price 13 million. Both raw values are 1.3e19,
        // whose naive product is 1.7e38 = i128::MAX. Only the 256-bit
        // intermediate gets this right.
        let q = Qty::from_int(13_000_000).expect("ok");
        let p = Price::from_int(13_000_000).expect("ok");
        let n = p.mul_qty(q, RoundMode::Trunc).expect("must not overflow");
        assert_eq!(n, Money::from_int(169_000_000_000_000).expect("ok"));
    }

    #[test]
    fn display_and_debug_are_human_readable() {
        let p = Price::parse("68123.45").expect("ok");
        assert_eq!(p.to_string(), "68123.45");
        assert_eq!(format!("{p:?}"), "Price(68123.45)");
        assert_eq!(Bps::from_whole(12).expect("ok").to_string(), "12");
        assert_eq!(format!("{:?}", Bps::parse_bps("1.8").expect("ok")), "Bps(1.8)");
        assert_eq!(p.to_places(2), "68123.45");
        assert_eq!(p.to_places(0), "68123");
    }

    #[test]
    fn parse_rejects_excess_precision_at_the_type_level() {
        assert_eq!(Price::parse("0.0000000000001"), Err(NumError::PrecisionLoss));
        assert_eq!(Qty::parse("not a number"), Err(NumError::InvalidFormat));
    }

    #[test]
    fn ordering_and_zero_predicates() {
        let a = Price::parse("-1.5").expect("ok");
        let b = Price::ZERO;
        let c = Price::parse("1.5").expect("ok");
        assert!(a < b && b < c);
        assert_eq!(a.signum(), -1);
        assert_eq!(b.signum(), 0);
        assert_eq!(c.signum(), 1);
        assert!(b.is_zero() && !a.is_zero());
        assert!(a.is_negative() && c.is_positive());
    }
}
