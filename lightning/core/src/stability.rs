//! The stability rule, the same for both parties.
//!
//! The saver (épargnant) holds `backing_sats` in the channel for a promise worth a fixed number of
//! Mengers. Each party values that promise with its own price. When the bitcoin held is worth more
//! than the promise, the saver pays the difference to the provider (fournisseur); when it is worth
//! less, the provider pays the saver. Whoever owes pays; the other waits. A party that is owed too
//! much for too long closes the channel and leaves with what it holds: there is no enforcement
//! other than the right to leave, and nothing is ever taken by force.
//!
//! The price of stability (`rate_ppm_per_year`) is agreed between the two parties when the
//! position is opened. A positive rate means the saver pays the provider for stability; a negative
//! rate means the provider pays the saver for the bitcoin exposure. It is a market price, set by
//! competition between providers, never by the protocol.

use crate::unit::SatsPerMenger;
use serde::{Deserialize, Serialize};

/// Seconds in a year of 365.25 days.
pub const YEAR_SECS: i64 = 31_557_600;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    /// Wants a balance that keeps its value in gold.
    Saver,
    /// Takes the other side: gains when bitcoin outpaces gold, loses when it falls behind.
    Provider,
}

/// A stable position, as both parties record it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Position {
    /// Value promised when the position was opened, in micro-Mengers.
    pub micro_at_start: u64,
    /// Unix time the position was opened.
    pub started_at: i64,
    /// Price of stability, in millionths per year (20,000 = 2 % a year paid by the saver).
    pub rate_ppm_per_year: i64,
    /// Satoshis the saver currently holds for this position. Moves only with stability payments.
    pub backing_sats: u64,
}

impl Position {
    /// Value promised at `now`, in micro-Mengers, after the price of stability (simple interest,
    /// computed from the start so that both parties get exactly the same number).
    pub fn micro_mengers(&self, now: i64) -> u64 {
        let elapsed = (now - self.started_at).max(0) as i128;
        let start = self.micro_at_start as i128;
        let change = start * self.rate_ppm_per_year as i128 * elapsed / (1_000_000i128 * YEAR_SECS as i128);
        (start - change).clamp(0, u64::MAX as i128) as u64
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Params {
    /// Drift tolerated before anyone pays, in millionths of the promised value (1,000 = 0.1 %).
    pub threshold_ppm: u64,
    /// Smallest stability payment, in satoshis.
    pub min_payment_sats: u64,
    /// Pause after one of our own stability payments, in seconds.
    pub cooldown_secs: i64,
    /// Time the other party gets to pay what it owes, in seconds.
    pub grace_secs: i64,
    /// After the grace period, close if the other party still owes more than this share of the
    /// promised value, in millionths (20,000 = 2 %).
    pub close_owed_ppm: u64,
    /// Saver only: warn when the provider's bitcoin covers a smaller fall of bitcoin against gold
    /// than this, in millionths (300,000 = 30 %).
    pub warn_coverage_ppm: u64,
    /// Saver only: close when the provider's bitcoin covers a smaller fall than this.
    pub close_coverage_ppm: u64,
}

impl Default for Params {
    fn default() -> Self {
        Params {
            threshold_ppm: 1_000,
            min_payment_sats: 100,
            cooldown_secs: 60,
            grace_secs: 600,
            close_owed_ppm: 20_000,
            warn_coverage_ppm: 300_000,
            close_coverage_ppm: 100_000,
        }
    }
}

/// What a party knows at one moment.
#[derive(Clone, Debug)]
pub struct Snapshot<'a> {
    pub role: Role,
    pub position: &'a Position,
    pub price: SatsPerMenger,
    pub now: i64,
    /// What we can send to the other party right now, in satoshis.
    pub our_spendable_sats: u64,
    /// What the other party can send to us right now, in satoshis.
    pub their_spendable_sats: u64,
    /// Time of our last stability payment.
    pub last_payment_at: Option<i64>,
    /// One of our stability payments has not completed yet.
    pub payment_in_flight: bool,
    /// Since when the other party has owed us more than the tolerance, if it does.
    pub owed_to_us_since: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CloseReason {
    /// The other party has owed this much for longer than the grace period.
    Unpaid { owed_sats: u64 },
    /// The provider's bitcoin no longer covers a reasonable fall of bitcoin against gold.
    ProviderCannotCover { coverage_ppm: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Action {
    /// Within tolerance, or waiting after our own payment.
    Hold,
    /// We owe the other party this many satoshis: pay now.
    Pay { sats: u64 },
    /// The other party owes us; it has until `since + grace` to pay.
    Await { owed_sats: u64, since: i64 },
    /// Leave: close the channel and keep what we hold.
    Close { reason: CloseReason },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Warning {
    /// The provider covers a fall of bitcoin against gold of only this much (millionths).
    LowCoverage { coverage_ppm: u64 },
    /// We owe more than we can send right now.
    CannotPayInFull { owed_sats: u64, spendable_sats: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decision {
    pub action: Action,
    /// Value of the promise at the current price, in satoshis.
    pub target_sats: u64,
    /// `backing_sats - target_sats`: positive when the saver owes, negative when the provider owes.
    pub drift_sats: i64,
    /// Saver only: largest fall of bitcoin against gold the provider can still cover (millionths).
    pub coverage_ppm: Option<u64>,
    pub warning: Option<Warning>,
}

/// Decides what this party should do now. Pure: the caller performs the action and records it.
pub fn decide(s: &Snapshot, p: &Params) -> Decision {
    let micro = s.position.micro_mengers(s.now);
    let target = s.price.sats_for(micro);
    let drift = s.position.backing_sats as i128 - target as i128;
    let drift_sats = drift.clamp(i64::MIN as i128, i64::MAX as i128) as i64;
    let owed = drift.unsigned_abs().min(u64::MAX as u128) as u64;
    let tolerance = (ppm(target, p.threshold_ppm)).max(p.min_payment_sats);
    let saver_owes = drift > 0;

    // What the provider can still pay after settling what it owes now, as a covered fall.
    let mut warning = None;
    let coverage_ppm = match s.role {
        Role::Saver => {
            let owed_by_provider = if saver_owes { 0 } else { owed };
            let reserve = s.their_spendable_sats.saturating_sub(owed_by_provider) as u128;
            let t = target as u128;
            let cov = (reserve * 1_000_000).checked_div(reserve + t).map_or(1_000_000, |c| c as u64);
            Some(cov)
        }
        Role::Provider => None,
    };
    let decision = |action, warning| Decision { action, target_sats: target, drift_sats, coverage_ppm, warning };

    if let Some(cov) = coverage_ppm {
        if target > 0 && cov < p.close_coverage_ppm {
            return decision(
                Action::Close { reason: CloseReason::ProviderCannotCover { coverage_ppm: cov } },
                Some(Warning::LowCoverage { coverage_ppm: cov }),
            );
        }
        if target > 0 && cov < p.warn_coverage_ppm {
            warning = Some(Warning::LowCoverage { coverage_ppm: cov });
        }
    }

    if owed < tolerance {
        return decision(Action::Hold, warning);
    }

    let we_owe = match s.role {
        Role::Saver => saver_owes,
        Role::Provider => !saver_owes,
    };
    if we_owe {
        let cooling = s.last_payment_at.is_some_and(|t| s.now - t < p.cooldown_secs);
        if s.payment_in_flight || cooling {
            return decision(Action::Hold, warning);
        }
        // Down to half the tolerance, not to the exact target: two parties whose prices differ by
        // less than one and a half tolerances never send the difference back and forth.
        let due = owed - tolerance / 2;
        let sats = due.min(s.our_spendable_sats);
        if sats < due {
            warning = Some(Warning::CannotPayInFull { owed_sats: due, spendable_sats: s.our_spendable_sats });
        }
        if sats < p.min_payment_sats {
            return decision(Action::Hold, warning);
        }
        return decision(Action::Pay { sats }, warning);
    }

    let since = s.owed_to_us_since.unwrap_or(s.now);
    let close_level = ppm(target, p.close_owed_ppm).max(tolerance);
    if s.now - since >= p.grace_secs && owed >= close_level {
        return decision(Action::Close { reason: CloseReason::Unpaid { owed_sats: owed } }, warning);
    }
    decision(Action::Await { owed_sats: owed, since }, warning)
}

fn ppm(amount: u64, ppm: u64) -> u64 {
    (amount as u128 * ppm as u128 / 1_000_000) as u64
}

/// Records a stability payment in the position, from either side's point of view.
/// `to_saver` is true when the provider paid the saver.
pub fn apply_payment(position: &mut Position, sats: u64, to_saver: bool) {
    if to_saver {
        position.backing_sats = position.backing_sats.saturating_add(sats);
    } else {
        position.backing_sats = position.backing_sats.saturating_sub(sats);
    }
}

/// Applies an accepted adjustment. Both parties call it with the same arguments and get the same
/// position: the change is added to what each one has recorded, so stability payments that cross
/// the request on the way are never lost. A position brought to zero ends.
pub fn adjust(current: Option<&Position>, at: i64, delta_micro: i64, delta_backing: i64, rate_ppm_per_year: i64) -> Option<Position> {
    let (micro, backing, rate) = match current {
        Some(p) => (p.micro_mengers(at), p.backing_sats, p.rate_ppm_per_year),
        None => (0, 0, rate_ppm_per_year),
    };
    let micro = (micro as i128 + delta_micro as i128).clamp(0, u64::MAX as i128) as u64;
    let backing = (backing as i128 + delta_backing as i128).clamp(0, u64::MAX as i128) as u64;
    if micro == 0 && backing == 0 {
        return None;
    }
    Some(Position { micro_at_start: micro, started_at: at, rate_ppm_per_year: rate, backing_sats: backing })
}

/// What a provider accepts when a saver asks to change its position.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdjustRules {
    /// Largest difference between the Mengers asked for and what the satoshis are worth at the
    /// provider's own price, in millionths. Two honest parties never read exactly the same price.
    pub tolerance_ppm: u64,
    /// Largest gap between the saver's announced price and the provider's, in millionths.
    pub max_price_gap_ppm: u64,
    /// Lowest price of stability accepted when a position is opened.
    pub min_rate_ppm_per_year: i64,
    /// Largest gap between the request's time and the provider's clock, in seconds.
    pub max_clock_skew_secs: i64,
}

impl Default for AdjustRules {
    fn default() -> Self {
        AdjustRules { tolerance_ppm: 5_000, max_price_gap_ppm: 10_000, min_rate_ppm_per_year: 0, max_clock_skew_secs: 600 }
    }
}

/// A requested change of position, as the saver sends it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdjustRequest {
    pub at: i64,
    pub delta_micro: i64,
    pub delta_backing: i64,
    pub rate_ppm_per_year: i64,
    pub their_price: SatsPerMenger,
}

/// The provider's checks, at its own price. The provider never promises more Mengers than the
/// satoshis added are worth, and never lets satoshis go back for fewer Mengers than they are worth.
pub fn check_adjust(
    current: Option<&Position>,
    req: &AdjustRequest,
    own_price: SatsPerMenger,
    now: i64,
    rules: &AdjustRules,
) -> Result<(), String> {
    if (req.at - now).abs() > rules.max_clock_skew_secs {
        return Err("heure de la demande trop éloignée de la nôtre".into());
    }
    match current {
        Some(p) if p.rate_ppm_per_year != req.rate_ppm_per_year => {
            return Err("le prix de la stabilité ne change pas en cours de route".into())
        }
        None if req.rate_ppm_per_year < rules.min_rate_ppm_per_year => {
            return Err(format!("prix de la stabilité trop bas (minimum {} ppm par an)", rules.min_rate_ppm_per_year))
        }
        _ => {}
    }
    let gap = (own_price.0 as i128 - req.their_price.0 as i128).unsigned_abs() * 1_000_000 / own_price.0.max(1) as u128;
    if gap > rules.max_price_gap_ppm as u128 {
        return Err(format!("prix trop différent du nôtre ({gap} ppm)"));
    }
    if (req.delta_micro > 0 && req.delta_backing < 0) || (req.delta_micro < 0 && req.delta_backing > 0) {
        return Err("une position ne grandit pas d'un côté en rétrécissant de l'autre".into());
    }
    let worth = own_price.micro_for(req.delta_backing.unsigned_abs()) as i128;
    let tol = rules.tolerance_ppm as i128;
    let micro = req.delta_micro as i128;
    if micro > 0 && micro > worth * (1_000_000 + tol) / 1_000_000 + 1 {
        return Err("trop de Mengers demandés pour ces sats".into());
    }
    if micro < 0 && -micro < worth * (1_000_000 - tol) / 1_000_000 - 1 {
        return Err("trop peu de Mengers rendus pour ces sats".into());
    }
    if req.delta_backing < 0 && req.delta_backing.unsigned_abs() > current.map_or(0, |p| p.backing_sats) {
        return Err("plus de sats retirés que la position n'en contient".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::unit::MICRO_PER_MENGER;
    use proptest::prelude::*;

    const T0: i64 = 1_793_793_600;
    const P0: SatsPerMenger = SatsPerMenger(492_255); // BTC 100,000 $, gold 4,319 $/oz

    fn position(mengers: u64) -> Position {
        Position {
            micro_at_start: mengers * MICRO_PER_MENGER,
            started_at: T0,
            rate_ppm_per_year: 0,
            backing_sats: P0.sats_for(mengers * MICRO_PER_MENGER),
        }
    }

    fn snap(role: Role, pos: &Position, price: SatsPerMenger, now: i64) -> Snapshot<'_> {
        Snapshot {
            role,
            position: pos,
            price,
            now,
            our_spendable_sats: 10_000_000,
            their_spendable_sats: 10_000_000,
            last_payment_at: None,
            payment_in_flight: false,
            owed_to_us_since: None,
        }
    }

    #[test]
    fn nothing_to_do_at_par() {
        let pos = position(2);
        for role in [Role::Saver, Role::Provider] {
            let d = decide(&snap(role, &pos, P0, T0), &Params::default());
            assert_eq!(d.action, Action::Hold);
            assert_eq!(d.drift_sats, 0);
        }
    }

    #[test]
    fn bitcoin_up_the_saver_pays() {
        // BTC +10 % against gold: a Menger now costs fewer sats, the saver holds too many.
        let pos = position(2);
        let price = SatsPerMenger::from_usd(110_000.0, 4_319.0).unwrap();
        let target = price.sats_for(2 * MICRO_PER_MENGER);
        let excess = pos.backing_sats - target;
        let saver = decide(&snap(Role::Saver, &pos, price, T0), &Params::default());
        // Down to half the 0.1 % tolerance.
        assert_eq!(saver.action, Action::Pay { sats: excess - target / 2_000 });
        let provider = decide(&snap(Role::Provider, &pos, price, T0), &Params::default());
        assert_eq!(provider.action, Action::Await { owed_sats: excess, since: T0 });
    }

    #[test]
    fn bitcoin_down_the_provider_pays() {
        let pos = position(2);
        let price = SatsPerMenger::from_usd(90_000.0, 4_319.0).unwrap();
        let target = price.sats_for(2 * MICRO_PER_MENGER);
        let shortfall = target - pos.backing_sats;
        let provider = decide(&snap(Role::Provider, &pos, price, T0), &Params::default());
        assert_eq!(provider.action, Action::Pay { sats: shortfall - target / 2_000 });
        let saver = decide(&snap(Role::Saver, &pos, price, T0), &Params::default());
        assert_eq!(saver.action, Action::Await { owed_sats: shortfall, since: T0 });
    }

    #[test]
    fn gold_up_the_provider_pays() {
        let pos = position(1);
        let price = SatsPerMenger::from_usd(100_000.0, 4_319.0 * 1.05).unwrap();
        let d = decide(&snap(Role::Provider, &pos, price, T0), &Params::default());
        assert!(matches!(d.action, Action::Pay { .. }));
    }

    #[test]
    fn prices_a_little_apart_do_not_ping_pong() {
        // The provider's price is 0.12 % above the saver's: more than the tolerance, so it pays.
        let pos = position(2);
        let saver_price = P0;
        let provider_price = SatsPerMenger(P0.0 * 10_012 / 10_000);
        let mut pos_after = pos.clone();
        match decide(&snap(Role::Provider, &pos, provider_price, T0), &Params::default()).action {
            Action::Pay { sats } => apply_payment(&mut pos_after, sats, true),
            other => panic!("provider should pay, got {other:?}"),
        }
        // At its own price the saver now holds a little too much, but not enough to pay it back.
        let saver = decide(&snap(Role::Saver, &pos_after, saver_price, T0), &Params::default());
        assert_eq!(saver.action, Action::Hold);
        assert!(saver.drift_sats > 0);
    }

    #[test]
    fn small_moves_are_ignored() {
        let pos = position(2);
        let price = SatsPerMenger::from_usd(100_050.0, 4_319.0).unwrap(); // +0.05 %
        assert_eq!(decide(&snap(Role::Saver, &pos, price, T0), &Params::default()).action, Action::Hold);
    }

    #[test]
    fn cooldown_and_in_flight_hold_payments() {
        let pos = position(2);
        let price = SatsPerMenger::from_usd(90_000.0, 4_319.0).unwrap();
        let mut s = snap(Role::Provider, &pos, price, T0);
        s.last_payment_at = Some(T0 - 10);
        assert_eq!(decide(&s, &Params::default()).action, Action::Hold);
        s.last_payment_at = Some(T0 - 61);
        s.payment_in_flight = true;
        assert_eq!(decide(&s, &Params::default()).action, Action::Hold);
    }

    #[test]
    fn unpaid_debt_closes_after_grace() {
        let pos = position(2);
        let price = SatsPerMenger::from_usd(90_000.0, 4_319.0).unwrap(); // provider owes ~11 %
        let p = Params::default();
        let mut s = snap(Role::Saver, &pos, price, T0 + 599);
        s.owed_to_us_since = Some(T0);
        assert!(matches!(decide(&s, &p).action, Action::Await { .. }));
        s.now = T0 + 600;
        assert!(matches!(decide(&s, &p).action, Action::Close { reason: CloseReason::Unpaid { .. } }));
    }

    #[test]
    fn small_unpaid_debt_does_not_close() {
        // Prices that differ slightly between parties leave a small debt; it is not a reason to leave.
        let pos = position(2);
        let price = SatsPerMenger::from_usd(99_500.0, 4_319.0).unwrap(); // provider owes 0.5 %
        let mut s = snap(Role::Saver, &pos, price, T0 + 10_000);
        s.owed_to_us_since = Some(T0);
        assert!(matches!(decide(&s, &Params::default()).action, Action::Await { .. }));
    }

    #[test]
    fn provider_short_of_bitcoin() {
        let pos = position(2); // about 984,510 sats promised
        let p = Params::default();
        // The provider can only send 200,000 sats: covers a fall of 200k / (200k + 984k) = 16.9 %.
        let mut s = snap(Role::Saver, &pos, P0, T0);
        s.their_spendable_sats = 200_000;
        let d = decide(&s, &p);
        assert_eq!(d.action, Action::Hold);
        assert!(matches!(d.warning, Some(Warning::LowCoverage { .. })));
        // With 100,000 sats it covers 9.2 %: the saver leaves while it still holds its value.
        s.their_spendable_sats = 100_000;
        assert!(matches!(
            decide(&s, &p).action,
            Action::Close { reason: CloseReason::ProviderCannotCover { .. } }
        ));
    }

    #[test]
    fn provider_pays_what_it_can() {
        let pos = position(2);
        let price = SatsPerMenger::from_usd(90_000.0, 4_319.0).unwrap();
        let mut s = snap(Role::Provider, &pos, price, T0);
        s.our_spendable_sats = 50_000;
        let d = decide(&s, &Params::default());
        assert_eq!(d.action, Action::Pay { sats: 50_000 });
        assert!(matches!(d.warning, Some(Warning::CannotPayInFull { .. })));
    }

    #[test]
    fn price_of_stability_accrues_from_the_start() {
        let mut pos = position(100);
        pos.rate_ppm_per_year = 20_000; // the saver pays 2 % a year
        assert_eq!(pos.micro_mengers(T0), 100 * MICRO_PER_MENGER);
        assert_eq!(pos.micro_mengers(T0 + YEAR_SECS), 98 * MICRO_PER_MENGER);
        assert_eq!(pos.micro_mengers(T0 + YEAR_SECS / 2), 99 * MICRO_PER_MENGER);
        pos.rate_ppm_per_year = -10_000; // the provider pays 1 % a year
        assert_eq!(pos.micro_mengers(T0 + YEAR_SECS), 101 * MICRO_PER_MENGER);
        // Never negative.
        pos.rate_ppm_per_year = 2_000_000;
        assert_eq!(pos.micro_mengers(T0 + 10 * YEAR_SECS), 0);
    }

    #[test]
    fn adjustments_open_grow_shrink_and_end() {
        let open = adjust(None, T0, 2 * MICRO_PER_MENGER as i64, 984_510, 20_000).unwrap();
        assert_eq!(open, Position { micro_at_start: 2 * MICRO_PER_MENGER, started_at: T0, rate_ppm_per_year: 20_000, backing_sats: 984_510 });
        // Half a year later, after the 1 % accrued, the saver spends half a Menger.
        let t = T0 + YEAR_SECS / 2;
        let smaller = adjust(Some(&open), t, -(MICRO_PER_MENGER as i64) / 2, -246_128, 20_000).unwrap();
        assert_eq!(smaller.micro_at_start, 1_980_000 - 500_000);
        assert_eq!(smaller.backing_sats, 984_510 - 246_128);
        assert_eq!(smaller.started_at, t);
        assert_eq!(smaller.rate_ppm_per_year, 20_000, "the rate of an open position never changes");
        // Bringing it to zero ends it.
        assert_eq!(adjust(Some(&smaller), t, -1_480_000, -738_382, 0), None);
    }

    fn request(delta_micro: i64, delta_backing: i64) -> AdjustRequest {
        AdjustRequest { at: T0, delta_micro, delta_backing, rate_ppm_per_year: 0, their_price: P0 }
    }

    #[test]
    fn provider_accepts_fair_adjustments() {
        let r = AdjustRules::default();
        let worth = P0.micro_for(500_000) as i64;
        assert_eq!(check_adjust(None, &request(worth, 500_000), P0, T0, &r), Ok(()));
        let pos = position(2);
        assert_eq!(check_adjust(Some(&pos), &request(-worth, -500_000), P0, T0, &r), Ok(()));
        // Within half a percent either way: two honest price readings.
        assert_eq!(check_adjust(None, &request(worth * 1004 / 1000, 500_000), P0, T0, &r), Ok(()));
        assert_eq!(check_adjust(Some(&pos), &request(-worth * 996 / 1000, -500_000), P0, T0, &r), Ok(()));
    }

    #[test]
    fn provider_refuses_unfair_adjustments() {
        let r = AdjustRules::default();
        let worth = P0.micro_for(500_000) as i64;
        let pos = position(2);
        // More Mengers than the sats are worth, or too few given back.
        assert!(check_adjust(None, &request(worth * 101 / 100, 500_000), P0, T0, &r).is_err());
        assert!(check_adjust(Some(&pos), &request(-worth * 99 / 100, -500_000), P0, T0, &r).is_err());
        // Growing on one side while shrinking on the other.
        assert!(check_adjust(Some(&pos), &request(worth, -500_000), P0, T0, &r).is_err());
        // More sats than the position holds.
        let all = pos.backing_sats as i64 + 1;
        assert!(check_adjust(Some(&pos), &request(-(P0.micro_for(all as u64) as i64), -all), P0, T0, &r).is_err());
        // A price far from ours, a stale request, a rate change.
        let mut far = request(worth, 500_000);
        far.their_price = SatsPerMenger(P0.0 * 103 / 100);
        assert!(check_adjust(None, &far, P0, T0, &r).is_err());
        assert!(check_adjust(None, &request(worth, 500_000), P0, T0 + 3_600, &r).is_err());
        let mut rate = request(worth, 500_000);
        rate.rate_ppm_per_year = 10_000;
        assert!(check_adjust(Some(&pos), &rate, P0, T0, &r).is_err());
        assert!(check_adjust(None, &request(worth, 500_000), P0, T0, &AdjustRules { min_rate_ppm_per_year: 1, ..r }).is_err());
    }

    #[test]
    fn payments_move_the_backing() {
        let mut pos = position(1);
        let b = pos.backing_sats;
        apply_payment(&mut pos, 1_000, true);
        assert_eq!(pos.backing_sats, b + 1_000);
        apply_payment(&mut pos, 3_000, false);
        assert_eq!(pos.backing_sats, b - 2_000);
    }

    proptest! {
        // Whatever the prices, at most one of the two parties pays, and paying the amount it
        // decides brings the position back within half the tolerance (when it has the bitcoin to
        // do so).
        #[test]
        fn one_payer_and_back_to_par(
            mengers in 1u64..1_000,
            btc in 20_000.0f64..500_000.0,
            gold in 1_000.0f64..20_000.0,
            rate in -50_000i64..50_000,
            elapsed in 0i64..(2 * YEAR_SECS),
        ) {
            let mut pos = position(mengers);
            pos.rate_ppm_per_year = rate;
            let now = T0 + elapsed;
            let price = SatsPerMenger::from_usd(btc, gold).unwrap();
            let p = Params { close_coverage_ppm: 0, warn_coverage_ppm: 0, ..Params::default() };
            let mut saver_snap = snap(Role::Saver, &pos, price, now);
            saver_snap.our_spendable_sats = u64::MAX / 4;
            saver_snap.their_spendable_sats = u64::MAX / 4;
            let mut provider_snap = saver_snap.clone();
            provider_snap.role = Role::Provider;
            let saver = decide(&saver_snap, &p);
            let provider = decide(&provider_snap, &p);
            let saver_pays = matches!(saver.action, Action::Pay { .. });
            let provider_pays = matches!(provider.action, Action::Pay { .. });
            prop_assert!(!(saver_pays && provider_pays));
            if let Action::Pay { sats } = saver.action {
                apply_payment(&mut pos, sats, false);
            }
            if let Action::Pay { sats } = provider.action {
                apply_payment(&mut pos, sats, true);
            }
            let after = decide(&snap(Role::Saver, &pos, price, now), &p);
            let tolerance = ppm(after.target_sats, p.threshold_ppm).max(p.min_payment_sats);
            prop_assert!(after.drift_sats.unsigned_abs() < tolerance);
            if saver_pays || provider_pays {
                // Down to half the tolerance (a satoshi either way for rounding).
                prop_assert!(after.drift_sats.unsigned_abs() <= tolerance / 2 + 1);
            }
        }
    }
}
