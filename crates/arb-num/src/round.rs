//! Explicit rounding modes.
//!
//! There is no default. Every lossy operation in this workspace names its
//! rounding mode at the call site, because the direction of the sub-tick
//! remainder is an economic choice: it decides whether an error in the last
//! digit favours us or the exchange. Systematically favouring ourselves is how
//! a scanner ends up reporting an edge it does not have.

/// How to dispose of a non-zero remainder.
///
/// "Magnitude" below means the absolute value of the result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RoundMode {
    /// Toward zero. Magnitude always decreases. `-1.5 -> -1`, `1.5 -> 1`.
    Trunc,
    /// Toward negative infinity. `-1.5 -> -2`, `1.5 -> 1`.
    Floor,
    /// Toward positive infinity. `-1.5 -> -1`, `1.5 -> 2`.
    Ceil,
    /// Away from zero. Magnitude always increases. `-1.5 -> -2`, `1.5 -> 2`.
    ///
    /// This is the mode for **costs**: fees, slippage, borrow. Rounding a cost
    /// away from zero is the conservative direction — we overstate what we pay,
    /// so the reported edge is never flattered by rounding.
    AwayFromZero,
    /// Nearest, ties to even magnitude. The neutral mode, for unit conversions
    /// where neither direction is conservative (e.g. mid-price).
    HalfEven,
}

impl RoundMode {
    /// The conservative mode for a quantity we pay out.
    pub const COST: RoundMode = RoundMode::AwayFromZero;
    /// The conservative mode for a quantity we receive.
    pub const REVENUE: RoundMode = RoundMode::Trunc;
}
