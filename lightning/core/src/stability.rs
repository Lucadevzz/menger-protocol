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
/// Largest price of stability either way, in millionths per year (100 % a year).
pub const MAX_RATE_PPM_PER_YEAR: i64 = 1_000_000;
/// More satoshis than will ever exist: larger amounts are refused outright.
pub const MAX_SATS: u64 = 2_100_000_000_000_000;
/// The price of stability is not counted beyond a century: the arithmetic stays far from overflow.
const MAX_ELAPSED_SECS: i64 = 100 * YEAR_SECS;

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
        let elapsed = now.saturating_sub(self.started_at).clamp(0, MAX_ELAPSED_SECS) as i128;
        let rate = self.rate_ppm_per_year.clamp(-MAX_RATE_PPM_PER_YEAR, MAX_RATE_PPM_PER_YEAR) as i128;
        let start = self.micro_at_start as i128;
        let change = start * rate * elapsed / (1_000_000i128 * YEAR_SECS as i128);
        (start - change).clamp(0, u64::MAX as i128) as u64
    }

    /// Nothing promised and nothing held: the position is over.
    pub fn is_empty(&self) -> bool {
        self.micro_at_start == 0 && self.backing_sats == 0
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
    /// Time the other party gets to pay what it owes while it is reachable, in seconds.
    pub grace_secs: i64,
    /// Time the other party gets while it cannot be reached: a party that is away cannot pay. A
    /// saver's computer sleeps; a provider is expected to be there. Each side picks its own.
    pub offline_grace_secs: i64,
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
            offline_grace_secs: 6 * 3_600,
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
    /// The other party can be reached now: payments can go either way.
    pub peer_online: bool,
    /// Since when it has been reachable without interruption, if known.
    pub online_since: Option<i64>,
    /// Provider: what the saver holds in the channels right now, when that can be told. A saver
    /// can send its bitcoin elsewhere at any time; the position only counts what is still there.
    pub their_balance_sats: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CloseReason {
    /// The other party has owed this much for longer than the grace period.
    Unpaid { owed_sats: u64 },
    /// The provider's bitcoin no longer covers a reasonable fall of bitcoin against gold.
    ProviderCannotCover { coverage_ppm: u64 },
    /// The saver no longer holds this much of the bitcoin its position counts: what it would owe
    /// if bitcoin rose is no longer there.
    BackingMissing { missing_sats: u64 },
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

    // What the other party owes us must be paid within a delay. A party that is away cannot
    // pay: it gets the long delay. Back, it gets the short one from the moment it returned, but
    // never more than both delays together since the debt began, so that coming and going does
    // not postpone the end for ever.
    let since = s.owed_to_us_since.unwrap_or(s.now).min(s.now);
    let away = since.saturating_add(p.offline_grace_secs);
    let deadline = if s.peer_online {
        let back = s.online_since.map_or(since, |t| t.clamp(since, s.now));
        back.saturating_add(p.grace_secs).min(away.saturating_add(p.grace_secs))
    } else {
        away
    };

    // Provider: a saver that moved away the bitcoin behind its position keeps the protection
    // against a fall without holding what it would owe after a rise. Nothing is paid to it until
    // the bitcoin is back, and if it stays that way, we leave.
    if let (Role::Provider, Some(held)) = (s.role, s.their_balance_sats) {
        let missing = s.position.backing_sats.saturating_sub(held);
        if missing > p.min_payment_sats {
            if s.now >= deadline {
                return decision(Action::Close { reason: CloseReason::BackingMissing { missing_sats: missing } }, warning);
            }
            return decision(Action::Await { owed_sats: missing, since }, warning);
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
        let cooling = s.last_payment_at.is_some_and(|t| s.now.saturating_sub(t) < p.cooldown_secs);
        // Nothing can be sent to a party that is away: it is paid when it comes back.
        if s.payment_in_flight || cooling || !s.peer_online {
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

    let close_level = ppm(target, p.close_owed_ppm).max(tolerance);
    if s.now >= deadline && owed >= close_level {
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
    let position = Position { micro_at_start: micro, started_at: at, rate_ppm_per_year: rate, backing_sats: backing };
    (!position.is_empty()).then_some(position)
}

/// What a provider accepts when a saver asks to change its position.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdjustRules {
    /// The provider's margin on each change, in millionths of the amount (0 = none). A market
    /// price like the price of stability: each provider sets its own, savers compare.
    pub spread_ppm: u64,
    /// Lowest price of stability accepted when a position is opened.
    pub min_rate_ppm_per_year: i64,
    /// Largest gap between the request's time and the provider's clock, in seconds.
    pub max_clock_skew_secs: i64,
    /// Smallest amount a position grows by, in satoshis.
    pub min_increase_sats: u64,
}

impl Default for AdjustRules {
    fn default() -> Self {
        AdjustRules { spread_ppm: 0, min_rate_ppm_per_year: 0, max_clock_skew_secs: 600, min_increase_sats: 1_000 }
    }
}

/// A requested change of position, as the saver sends it. The saver names the satoshis and the
/// worst amount of Mengers it accepts for them; the provider prices the change with its own price
/// and answers with the exact amount. Neither side ever has to take the other's price on trust.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdjustRequest {
    /// Identifies the request, and the time from which the new position accrues.
    pub at: i64,
    /// Satoshis added to the position (positive) or taken out of it (negative). Never zero.
    pub delta_backing: i64,
    /// Growing: the fewest micro-Mengers the saver accepts for these satoshis.
    /// Shrinking: the most micro-Mengers it gives up for them.
    pub limit_micro: u64,
    pub rate_ppm_per_year: i64,
}

/// Saver: the worst amount of Mengers it accepts for `delta_backing` satoshis, `slack_ppm` away
/// from what they are worth at its own price.
pub fn adjust_limit(delta_backing: i64, own_price: SatsPerMenger, slack_ppm: u64) -> u64 {
    let sats = delta_backing.unsigned_abs();
    let slack = slack_ppm.min(1_000_000) as u128;
    if delta_backing > 0 {
        (own_price.micro_for_floor(sats) as u128 * (1_000_000 - slack) / 1_000_000) as u64
    } else {
        let most = (own_price.micro_for_ceil(sats) as u128 * (1_000_000 + slack)).div_ceil(1_000_000);
        most.min(u64::MAX as u128) as u64
    }
}

/// Provider: prices a requested change with its own price and returns the change in
/// micro-Mengers, or the reason it refuses.
///
/// Satoshis coming in are counted for no more Mengers than they are worth, satoshis going out
/// for no fewer: at the provider's own price, what the saver owes it never shrinks. Taking out
/// the whole backing ends the position, which the provider accepts only while the saver owes it
/// no more than the usual tolerance.
pub fn quote_adjust(
    current: Option<&Position>,
    req: &AdjustRequest,
    own_price: SatsPerMenger,
    now: i64,
    rules: &AdjustRules,
    params: &Params,
) -> Result<i64, String> {
    if req.at.saturating_sub(now).saturating_abs() > rules.max_clock_skew_secs {
        return Err("heure de la demande trop éloignée de la nôtre".into());
    }
    if req.rate_ppm_per_year.saturating_abs() > MAX_RATE_PPM_PER_YEAR {
        return Err("prix de la stabilité hors des bornes".into());
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
    let sats = req.delta_backing.unsigned_abs();
    if sats == 0 || sats > MAX_SATS || own_price.0 == 0 {
        return Err("montant invalide".into());
    }
    let spread = rules.spread_ppm.min(1_000_000) as u128;
    let beyond_limit = || "notre prix est trop éloigné du tien pour ce changement".to_string();

    if req.delta_backing > 0 {
        if sats < rules.min_increase_sats {
            return Err(format!("montant trop petit (minimum {} sats)", rules.min_increase_sats));
        }
        let granted = (own_price.micro_for_floor(sats) as u128 * (1_000_000 - spread) / 1_000_000) as u64;
        let room = u64::MAX - current.map_or(0, |p| p.micro_mengers(req.at));
        if granted == 0 || granted > room.min(i64::MAX as u64) {
            return Err("montant invalide".into());
        }
        if granted < req.limit_micro {
            return Err(beyond_limit());
        }
        return Ok(granted as i64);
    }

    let Some(position) = current else { return Err("aucune position à réduire".into()) };
    let micro = position.micro_mengers(req.at);
    if sats > position.backing_sats {
        return Err("plus de sats retirés que la position n'en contient".into());
    }
    if micro > i64::MAX as u64 {
        return Err("montant invalide".into());
    }
    if sats == position.backing_sats {
        // Nothing would be left behind the promise: the position ends, and with it whatever one
        // side still owed the other. The saver's limit protects the saver; this protects us.
        let target = own_price.sats_for(micro);
        let tolerance = ppm(target, params.threshold_ppm).max(params.min_payment_sats);
        if position.backing_sats > target.saturating_add(tolerance) {
            return Err("ces sats valent plus que tes Mengers : règle d'abord ce que tu dois".into());
        }
        if micro > req.limit_micro {
            return Err(beyond_limit());
        }
        return Ok(-(micro as i64));
    }
    let taken = (own_price.micro_for_ceil(sats) as u128 * (1_000_000 + spread)).div_ceil(1_000_000);
    if taken >= micro as u128 {
        return Err("ces sats valent plus que tes Mengers : pour tout reprendre, retire la totalité".into());
    }
    if taken > req.limit_micro as u128 {
        return Err(beyond_limit());
    }
    Ok(-(taken as i64))
}

/// Saver: checks the provider's answer against what was asked. An answer outside the limit means
/// the provider did not do what was asked: the saver's remedy is to leave.
pub fn check_reply(req: &AdjustRequest, delta_micro: i64) -> Result<(), String> {
    let within = if req.delta_backing > 0 {
        delta_micro > 0 && delta_micro.unsigned_abs() >= req.limit_micro
    } else {
        delta_micro < 0 && delta_micro.unsigned_abs() <= req.limit_micro
    };
    if within {
        Ok(())
    } else {
        Err("réponse hors de la limite demandée".into())
    }
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
            peer_online: true,
            online_since: None,
            their_balance_sats: None,
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

    fn request(delta_backing: i64, limit_micro: u64) -> AdjustRequest {
        AdjustRequest { at: T0, delta_backing, limit_micro, rate_ppm_per_year: 0 }
    }

    fn quote(current: Option<&Position>, req: &AdjustRequest, price: SatsPerMenger) -> Result<i64, String> {
        quote_adjust(current, req, price, T0, &AdjustRules::default(), &Params::default())
    }

    #[test]
    fn the_provider_prices_a_change_with_its_own_price() {
        let worth = P0.micro_for_floor(500_000);
        // Growing: exactly what the satoshis are worth, never more, whatever the saver hoped for.
        assert_eq!(quote(None, &request(500_000, 0), P0), Ok(worth as i64));
        assert_eq!(quote(None, &request(500_000, worth), P0), Ok(worth as i64));
        let pos = position(2);
        // Shrinking: the satoshis leave for what they are worth, never less.
        let taken = P0.micro_for_ceil(500_000);
        assert_eq!(quote(Some(&pos), &request(-500_000, u64::MAX), P0), Ok(-(taken as i64)));
        // A provider whose price is 0.2 % higher gives fewer Mengers for the same satoshis.
        let dearer = SatsPerMenger(P0.0 * 1_002 / 1_000);
        let granted = quote(None, &request(500_000, 0), dearer).unwrap();
        assert!(granted < worth as i64 && granted > worth as i64 * 997 / 1_000);
    }

    #[test]
    fn the_saver_s_limit_is_respected_or_the_change_is_refused() {
        let slack = 3_000; // 0.3 %
        let grow = request(500_000, adjust_limit(500_000, P0, slack));
        let shrink = request(-500_000, adjust_limit(-500_000, P0, slack));
        let pos = position(2);
        // Prices 0.2 % apart: within the limit, and the answer passes the saver's check.
        for price in [SatsPerMenger(P0.0 * 1_002 / 1_000), SatsPerMenger(P0.0 * 998 / 1_000)] {
            let granted = quote(None, &grow, price).unwrap();
            assert_eq!(check_reply(&grow, granted), Ok(()));
            let taken = quote(Some(&pos), &shrink, price).unwrap();
            assert_eq!(check_reply(&shrink, taken), Ok(()));
        }
        // Prices 0.5 % apart, against the saver: refused rather than filled at a bad price.
        assert!(quote(None, &grow, SatsPerMenger(P0.0 * 1_005 / 1_000)).is_err());
        assert!(quote(Some(&pos), &shrink, SatsPerMenger(P0.0 * 995 / 1_000)).is_err());
        // An answer outside the limit, or of the wrong sign, does not pass.
        assert!(check_reply(&grow, grow.limit_micro as i64 - 1).is_err());
        assert!(check_reply(&grow, -1).is_err());
        assert!(check_reply(&shrink, -(shrink.limit_micro as i64) - 1).is_err());
        assert!(check_reply(&shrink, 1).is_err());
        assert!(check_reply(&shrink, 0).is_err());
    }

    #[test]
    fn satoshis_never_leave_without_their_mengers() {
        // What a hostile saver tried with the earlier messages: take satoshis back and keep the
        // Mengers. The saver no longer names the Mengers at all; its limit only ever makes the
        // provider refuse.
        let pos = position(2);
        for limit in [0, 1, u64::MAX] {
            let req = request(-500_000, limit);
            match quote(Some(&pos), &req, P0) {
                Ok(dm) => assert_eq!(dm, -(P0.micro_for_ceil(500_000) as i64)),
                Err(_) => assert!(limit < P0.micro_for_ceil(500_000)),
            }
        }
        // Nothing to take from a position that does not exist, nor more than it holds.
        assert!(quote(None, &request(-1, u64::MAX), P0).is_err());
        assert!(quote(Some(&pos), &request(-(pos.backing_sats as i64) - 1, u64::MAX), P0).is_err());
        assert!(quote(Some(&pos), &request(0, 0), P0).is_err());
        assert!(quote(None, &request(i64::MIN, u64::MAX), P0).is_err());
        assert!(quote(None, &request(i64::MAX, 0), P0).is_err());
    }

    #[test]
    fn taking_everything_out_ends_the_position() {
        let pos = position(2);
        let all = -(pos.backing_sats as i64);
        let dm = quote(Some(&pos), &request(all, adjust_limit(all, P0, 3_000)), P0).unwrap();
        assert_eq!(dm, -(2 * MICRO_PER_MENGER as i64));
        assert_eq!(adjust(Some(&pos), T0, dm, all, 0), None);
        // Bitcoin rose 3 %: the saver holds more than its Mengers are worth and owes the
        // difference. It cannot walk away with it by ending the position...
        let up = SatsPerMenger(P0.0 * 100 / 103);
        assert!(quote(Some(&pos), &request(all, u64::MAX), up).unwrap_err().contains("règle d'abord"));
        // ...nor by taking out more satoshis than its Mengers are worth and leaving the rest.
        let most = -(up.sats_for(2 * MICRO_PER_MENGER) as i64) - 1_000;
        assert!(quote(Some(&pos), &request(most, u64::MAX), up).is_err());
        // Bitcoin fell 3 %: the provider owes. Ending now would cost the saver that debt, so its
        // limit makes the provider refuse; it tries again once it has been paid.
        let down = SatsPerMenger(P0.0 * 103 / 100);
        assert!(quote(Some(&pos), &request(all, adjust_limit(all, down, 3_000)), down).is_err());
    }

    #[test]
    fn requests_out_of_bounds_are_refused() {
        let ok = request(500_000, 0);
        assert!(quote(None, &ok, P0).is_ok());
        // A stale request, a rate change, a rate out of bounds, too small an amount.
        assert!(quote_adjust(None, &ok, P0, T0 + 3_600, &AdjustRules::default(), &Params::default()).is_err());
        let pos = position(2);
        assert!(quote(Some(&pos), &AdjustRequest { rate_ppm_per_year: 10_000, ..ok.clone() }, P0).is_err());
        assert!(quote(None, &AdjustRequest { rate_ppm_per_year: MAX_RATE_PPM_PER_YEAR + 1, ..ok.clone() }, P0).is_err());
        assert!(quote(None, &AdjustRequest { rate_ppm_per_year: i64::MIN, ..ok.clone() }, P0).is_err());
        let rules = AdjustRules { min_rate_ppm_per_year: 1, ..AdjustRules::default() };
        assert!(quote_adjust(None, &ok, P0, T0, &rules, &Params::default()).is_err());
        assert!(quote(None, &request(999, 0), P0).is_err());
    }

    #[test]
    fn the_provider_s_margin() {
        let rules = AdjustRules { spread_ppm: 2_000, ..AdjustRules::default() }; // 0.2 %
        let worth = P0.micro_for_floor(500_000) as i64;
        let granted = quote_adjust(None, &request(500_000, 0), P0, T0, &rules, &Params::default()).unwrap();
        assert_eq!(granted, worth * 998 / 1_000);
        let pos = position(2);
        let taken = quote_adjust(Some(&pos), &request(-500_000, u64::MAX), P0, T0, &rules, &Params::default()).unwrap();
        assert!(-taken > worth && -taken <= worth * 1_002 / 1_000 + 2);
    }

    #[test]
    fn a_party_that_is_away_gets_more_time() {
        let pos = position(2);
        let price = SatsPerMenger::from_usd(90_000.0, 4_319.0).unwrap(); // the provider owes ~11 %
        let p = Params::default();
        let mut s = snap(Role::Saver, &pos, price, T0 + 3_600);
        s.owed_to_us_since = Some(T0);
        s.peer_online = false;
        // Unreachable for an hour: far past the ten minutes, but it could not have paid.
        assert!(matches!(decide(&s, &p).action, Action::Await { .. }));
        s.now = T0 + p.offline_grace_secs;
        assert!(matches!(decide(&s, &p).action, Action::Close { reason: CloseReason::Unpaid { .. } }));
        // Back after an hour: ten minutes from its return, not from the start of the debt.
        s.peer_online = true;
        s.online_since = Some(T0 + 3_600);
        s.now = T0 + 3_600 + 599;
        assert!(matches!(decide(&s, &p).action, Action::Await { .. }));
        s.now = T0 + 3_600 + 600;
        assert!(matches!(decide(&s, &p).action, Action::Close { .. }));
        // Coming and going does not postpone the end for ever.
        s.online_since = Some(T0 + p.offline_grace_secs + 500);
        s.now = T0 + p.offline_grace_secs + p.grace_secs;
        assert!(matches!(decide(&s, &p).action, Action::Close { .. }));
    }

    #[test]
    fn a_saver_that_moved_its_bitcoin_away_is_not_paid() {
        // Bitcoin fell 10 %: the provider owes. But the saver now holds only a tenth of the
        // satoshis its position counts: it sent the rest elsewhere and kept the position.
        let pos = position(2);
        let price = SatsPerMenger::from_usd(90_000.0, 4_319.0).unwrap();
        let p = Params::default();
        let mut s = snap(Role::Provider, &pos, price, T0);
        s.their_balance_sats = Some(pos.backing_sats / 10);
        let d = decide(&s, &p);
        assert_eq!(d.action, Action::Await { owed_sats: pos.backing_sats - pos.backing_sats / 10, since: T0 });
        s.owed_to_us_since = Some(T0);
        s.now = T0 + p.grace_secs;
        assert!(matches!(decide(&s, &p).action, Action::Close { reason: CloseReason::BackingMissing { .. } }));
        // A satoshi here and there (each message carries one) is not a reason for anything.
        s.their_balance_sats = Some(pos.backing_sats - 50);
        assert!(matches!(decide(&s, &p).action, Action::Pay { .. }));
        // Holding more than the position counts is the normal case.
        s.their_balance_sats = Some(pos.backing_sats * 2);
        assert!(matches!(decide(&s, &p).action, Action::Pay { .. }));
        // A saver does not judge the provider this way: it looks at what the provider can pay.
        let mut s = snap(Role::Saver, &pos, price, T0);
        s.their_balance_sats = Some(0);
        assert!(matches!(decide(&s, &p).action, Action::Await { .. }));
    }

    #[test]
    fn nobody_pays_a_party_that_is_away() {
        let pos = position(2);
        let price = SatsPerMenger::from_usd(90_000.0, 4_319.0).unwrap();
        let mut s = snap(Role::Provider, &pos, price, T0);
        s.peer_online = false;
        assert_eq!(decide(&s, &Params::default()).action, Action::Hold);
        s.peer_online = true;
        assert!(matches!(decide(&s, &Params::default()).action, Action::Pay { .. }));
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
        #![proptest_config(ProptestConfig { cases: 2_000, ..ProptestConfig::default() })]

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

        // Whatever a saver asks, an accepted change never lowers what the saver owes the provider
        // at the provider's own price; ending a position is accepted only within the tolerance.
        // And an accepted answer always passes the saver's own check.
        #[test]
        fn an_accepted_change_never_costs_the_provider(
            has_position in any::<bool>(),
            micro in 1_000u64..2_000_000_000,
            price in 1_000u64..5_000_000,
            // How far the backing is from the value of the Mengers: who owes whom, and how much.
            drift_ppm in -30_000i64..30_000,
            // The saver's own price, relative to the provider's.
            saver_price_ppm in 990_000u64..1_010_000,
            share_ppm in 1u64..1_200_000,
            grow in any::<bool>(),
            everything in any::<bool>(),
            limit in prop_oneof![Just(None), Just(Some(0u64)), Just(Some(u64::MAX)), any::<u64>().prop_map(Some)],
            spread in 0u64..5_000,
            rate in -50_000i64..50_000,
            elapsed in 0i64..YEAR_SECS,
        ) {
            let price = SatsPerMenger(price);
            let mut pos = Position { micro_at_start: micro, started_at: T0 - elapsed, rate_ppm_per_year: rate, backing_sats: 0 };
            let value = price.sats_for(pos.micro_mengers(T0)) as i128;
            pos.backing_sats = (value * (1_000_000 + drift_ppm as i128) / 1_000_000).max(1) as u64;
            let current = has_position.then_some(&pos);
            let sats = if everything && has_position {
                pos.backing_sats
            } else {
                (pos.backing_sats as u128 * share_ppm as u128 / 1_000_000).max(1) as u64
            };
            let delta_backing = if grow { sats as i64 } else { -(sats as i64) };
            let saver_price = SatsPerMenger((price.0 as u128 * saver_price_ppm as u128 / 1_000_000).max(1) as u64);
            let req = AdjustRequest {
                at: T0,
                delta_backing,
                // An honest limit, or anything at all.
                limit_micro: limit.unwrap_or_else(|| adjust_limit(delta_backing, saver_price, 3_000)),
                rate_ppm_per_year: rate,
            };
            let rules = AdjustRules { spread_ppm: spread, min_rate_ppm_per_year: i64::MIN, min_increase_sats: 1, ..AdjustRules::default() };
            let params = Params::default();
            let claim = |p: Option<&Position>| match p {
                Some(p) => p.backing_sats as i128 - price.sats_for(p.micro_mengers(T0)) as i128,
                None => 0,
            };
            if let Ok(dm) = quote_adjust(current, &req, price, T0, &rules, &params) {
                prop_assert_eq!(check_reply(&req, dm), Ok(()));
                let after = adjust(current, T0, dm, req.delta_backing, rate);
                let before = claim(current);
                match &after {
                    Some(p) => {
                        prop_assert!(claim(after.as_ref()) >= before, "claim {} -> {}", before, claim(after.as_ref()));
                        prop_assert!(p.micro_at_start > 0 && p.backing_sats > 0, "nothing half-empty is left behind");
                    }
                    None => {
                        let target = price.sats_for(pos.micro_mengers(T0));
                        let tolerance = ppm(target, params.threshold_ppm).max(params.min_payment_sats);
                        prop_assert!(before <= tolerance as i128);
                    }
                }
            }
        }

        // No input, however absurd, makes the rules panic.
        #[test]
        fn the_rules_never_panic(
            micro in any::<u64>(), started in any::<i64>(), rate in any::<i64>(), backing in any::<u64>(),
            price in any::<u64>(), now in any::<i64>(), at in any::<i64>(),
            delta in any::<i64>(), limit in any::<u64>(), dm in any::<i64>(),
            ours in any::<u64>(), theirs in any::<u64>(), since in proptest::option::of(any::<i64>()),
            last in proptest::option::of(any::<i64>()), online in any::<bool>(), provider in any::<bool>(),
        ) {
            let pos = Position { micro_at_start: micro, started_at: started, rate_ppm_per_year: rate, backing_sats: backing };
            let price = SatsPerMenger(price);
            let _ = pos.micro_mengers(now);
            let s = Snapshot {
                role: if provider { Role::Provider } else { Role::Saver },
                position: &pos,
                price,
                now,
                our_spendable_sats: ours,
                their_spendable_sats: theirs,
                last_payment_at: last,
                payment_in_flight: false,
                owed_to_us_since: since,
                peer_online: online,
                online_since: last,
                their_balance_sats: since.map(|_| theirs),
            };
            let _ = decide(&s, &Params::default());
            let req = AdjustRequest { at, delta_backing: delta, limit_micro: limit, rate_ppm_per_year: rate };
            let _ = quote_adjust(Some(&pos), &req, price, now, &AdjustRules::default(), &Params::default());
            let _ = quote_adjust(None, &req, price, now, &AdjustRules::default(), &Params::default());
            let _ = check_reply(&req, dm);
            let _ = adjust_limit(delta, price, limit);
            let _ = adjust(Some(&pos), at, dm, delta, rate);
            let mut p2 = pos.clone();
            apply_payment(&mut p2, backing, online);
        }
    }
}
