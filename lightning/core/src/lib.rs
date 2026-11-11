//! Rules of Menger on Lightning, without any input or output.
//!
//! A saver keeps a balance whose value is fixed in gold (1 Menger = 3.545 g) inside a Lightning
//! channel with a provider. Both hold their own bitcoin; as prices move, whoever owes pays the
//! other through the channel. There is no token, no issuer, no common oracle and no governance:
//! each party reads its own prices and can leave at any moment with what it holds.

pub mod price;
pub mod protocol;
pub mod stability;
pub mod unit;

pub use price::{menger_price, Asset, GoldMemory, PricePoint, PriceRules, Quote};
pub use stability::{
    adjust, adjust_limit, check_reply, decide, quote_adjust, Action, AdjustRequest, AdjustRules, CloseReason, Decision, Params,
    Position, Role,
    Snapshot, Warning,
};
pub use unit::{SatsPerMenger, MICRO_PER_MENGER};
