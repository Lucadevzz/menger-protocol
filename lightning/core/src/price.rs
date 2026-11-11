//! Prices. Each party chooses its own sources: there is no common oracle.
//!
//! A price is accepted only when enough independent, recent sources agree with each other. The
//! median of the agreeing sources is used. Sources that read the same thing in several places (the
//! same gold token on three markets) form one family and count once: if that token loses its link
//! to gold, its three prices do not outvote the others. Gold does not trade from Friday 21:00 to
//! Monday 00:00
//! (UTC), which covers New York's summer and winter time with an hour of margin at the reopening.
//! During that window the last gold price of the week is kept, while bitcoin keeps moving. When no
//! acceptable price exists, nobody pays and nobody closes for non-payment: stale data never moves
//! money.

use crate::unit::SatsPerMenger;
use chrono::{DateTime, Datelike, Timelike, Utc, Weekday};
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Asset {
    /// Bitcoin, in dollars per bitcoin.
    Btc,
    /// Gold, in dollars per troy ounce.
    Gold,
}

/// One price read from one source.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Quote {
    /// Name of the source. A name such as `paxg:kraken` puts it in the family `paxg`, with every
    /// other source whose name starts the same way; a name without a colon is a family of one.
    pub source: String,
    pub asset: Asset,
    pub usd: f64,
    /// Unix time of the quote, in seconds.
    pub at: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PriceRules {
    /// Independent sources (families) that must agree, per asset.
    pub min_sources: usize,
    /// Oldest bitcoin quote accepted, in seconds.
    pub btc_max_age: i64,
    /// Oldest gold quote accepted while the market is open, in seconds.
    pub gold_max_age: i64,
    /// Largest distance from the median for a bitcoin quote to count, in millionths.
    pub btc_max_spread_ppm: u64,
    /// Largest distance from the median for a gold quote to count, in millionths.
    pub gold_max_spread_ppm: u64,
    /// Longest the last gold price may be carried over while the market is closed, in seconds.
    pub gold_max_carry: i64,
}

impl Default for PriceRules {
    fn default() -> Self {
        PriceRules {
            min_sources: 2,
            btc_max_age: 120,
            gold_max_age: 900,
            btc_max_spread_ppm: 20_000,
            gold_max_spread_ppm: 10_000,
            // A weekend lasts 51 hours; four days also covers a holiday on the Friday or Monday.
            gold_max_carry: 4 * 24 * 3600,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum PriceError {
    /// Fewer recent sources than required.
    NotEnoughSources { asset: Asset, fresh: usize, required: usize },
    /// The sources do not agree closely enough.
    Disagreement { asset: Asset, agreeing: usize, required: usize },
    /// The gold market is closed and no gold price from before the closing is known (or it is too old).
    NoGoldPrice,
    /// The two prices give no usable price of a Menger.
    Invalid,
}

impl fmt::Display for PriceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PriceError::NotEnoughSources { asset, fresh, required } => {
                write!(f, "{} : {fresh} source(s) récente(s), il en faut {required}", asset.name_fr())
            }
            PriceError::Disagreement { asset, agreeing, required } => {
                write!(f, "{} : {agreeing} source(s) concordante(s), il en faut {required}", asset.name_fr())
            }
            PriceError::NoGoldPrice => write!(f, "marché de l'or fermé, et pas de prix récent d'avant la fermeture"),
            PriceError::Invalid => write!(f, "ces prix ne donnent pas de prix valable pour le Menger"),
        }
    }
}

impl Asset {
    fn name_fr(self) -> &'static str {
        match self {
            Asset::Btc => "bitcoin",
            Asset::Gold => "or",
        }
    }
}

/// The family of a source: what comes before the colon in its name, or the whole name.
pub fn family(source: &str) -> &str {
    source.split_once(':').map_or(source, |(family, _)| family)
}

/// True from Friday 21:00 to Monday 00:00, UTC.
pub fn gold_market_closed(now: i64) -> bool {
    let t: DateTime<Utc> = match DateTime::from_timestamp(now, 0) {
        Some(t) => t,
        None => return true,
    };
    match t.weekday() {
        Weekday::Fri => t.hour() >= 21,
        Weekday::Sat | Weekday::Sun => true,
        _ => false,
    }
}

/// Median of the recent quotes for `asset`, provided at least `min_sources` independent sources
/// agree. The sources of one family count as one source, worth the median of the family.
pub fn aggregate(quotes: &[Quote], asset: Asset, now: i64, rules: &PriceRules) -> Result<f64, PriceError> {
    let (max_age, max_spread) = match asset {
        Asset::Btc => (rules.btc_max_age, rules.btc_max_spread_ppm),
        Asset::Gold => (rules.gold_max_age, rules.gold_max_spread_ppm),
    };
    let required = rules.min_sources.max(1);
    // One quote per source: the most recent.
    let mut best: Vec<&Quote> = Vec::new();
    for q in quotes.iter().filter(|q| q.asset == asset) {
        let fresh = now.saturating_sub(q.at) <= max_age && q.at <= now.saturating_add(60);
        if !(fresh && q.usd.is_finite() && q.usd > 0.0) {
            continue;
        }
        match best.iter_mut().find(|b| b.source == q.source) {
            Some(b) if b.at < q.at => *b = q,
            Some(_) => {}
            None => best.push(q),
        }
    }
    // One value per family: the median of its sources.
    let mut families: Vec<(&str, Vec<f64>)> = Vec::new();
    for q in &best {
        match families.iter_mut().find(|(name, _)| *name == family(&q.source)) {
            Some((_, values)) => values.push(q.usd),
            None => families.push((family(&q.source), vec![q.usd])),
        }
    }
    if families.len() < required {
        return Err(PriceError::NotEnoughSources { asset, fresh: families.len(), required });
    }
    let values: Vec<f64> = families.iter().map(|(_, values)| median(values)).collect();
    let m = median(&values);
    let agreeing: Vec<f64> = values
        .into_iter()
        .filter(|v| ((v - m).abs() / m) * 1_000_000.0 <= max_spread as f64)
        .collect();
    if agreeing.len() < required {
        return Err(PriceError::Disagreement { asset, agreeing: agreeing.len(), required });
    }
    Ok(median(&agreeing))
}

fn median(values: &[f64]) -> f64 {
    let mut v = values.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).expect("finite values"));
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

/// The last gold price accepted while the market was open.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct GoldMemory {
    pub usd: f64,
    pub at: i64,
}

/// A price of the Menger, with what it was made of.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PricePoint {
    pub sats_per_menger: SatsPerMenger,
    pub btc_usd: f64,
    pub xau_usd: f64,
    /// True when the gold price is the last one before the market closed.
    pub gold_carried: bool,
    pub at: i64,
}

/// Price of a Menger now, from this party's own quotes. Updates `gold` while the market is open.
pub fn menger_price(
    quotes: &[Quote],
    now: i64,
    rules: &PriceRules,
    gold: &mut Option<GoldMemory>,
) -> Result<PricePoint, PriceError> {
    let btc_usd = aggregate(quotes, Asset::Btc, now, rules)?;
    let (xau_usd, gold_carried) = if gold_market_closed(now) {
        match gold {
            Some(g) if now - g.at <= rules.gold_max_carry => (g.usd, true),
            _ => return Err(PriceError::NoGoldPrice),
        }
    } else {
        let usd = aggregate(quotes, Asset::Gold, now, rules)?;
        *gold = Some(GoldMemory { usd, at: now });
        (usd, false)
    };
    let sats_per_menger = SatsPerMenger::from_usd(btc_usd, xau_usd).ok_or(PriceError::Invalid)?;
    Ok(PricePoint { sats_per_menger, btc_usd, xau_usd, gold_carried, at: now })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Wednesday 4 November 2026, 12:00 UTC.
    const WED_NOON: i64 = 1_793_793_600;
    const DAY: i64 = 86_400;

    fn q(source: &str, asset: Asset, usd: f64, at: i64) -> Quote {
        Quote { source: source.into(), asset, usd, at }
    }

    #[test]
    fn calendar_reference() {
        let t = DateTime::from_timestamp(WED_NOON, 0).unwrap();
        assert_eq!(t.weekday(), Weekday::Wed);
        assert_eq!(t.hour(), 12);
    }

    #[test]
    fn market_hours() {
        assert!(!gold_market_closed(WED_NOON));
        let fri_20h = WED_NOON + 2 * DAY + 8 * 3600;
        let fri_21h = fri_20h + 3600;
        assert!(!gold_market_closed(fri_20h));
        assert!(gold_market_closed(fri_21h));
        assert!(gold_market_closed(WED_NOON + 3 * DAY));
        let sun_2359 = WED_NOON + 4 * DAY + 12 * 3600 - 60;
        assert!(gold_market_closed(sun_2359));
        assert!(!gold_market_closed(sun_2359 + 60));
    }

    #[test]
    fn median_of_agreeing_sources() {
        let quotes = vec![
            q("a", Asset::Btc, 100_000.0, WED_NOON),
            q("b", Asset::Btc, 100_500.0, WED_NOON),
            q("c", Asset::Btc, 140_000.0, WED_NOON), // broken source, ignored
        ];
        let r = PriceRules::default();
        assert_eq!(aggregate(&quotes, Asset::Btc, WED_NOON, &r).unwrap(), 100_250.0);
    }

    #[test]
    fn refuses_stale_or_single_sources() {
        let r = PriceRules::default();
        let quotes = vec![
            q("a", Asset::Btc, 100_000.0, WED_NOON),
            q("b", Asset::Btc, 100_000.0, WED_NOON - 3600),
        ];
        assert_eq!(
            aggregate(&quotes, Asset::Btc, WED_NOON, &r),
            Err(PriceError::NotEnoughSources { asset: Asset::Btc, fresh: 1, required: 2 })
        );
        // The same source twice does not count as two sources.
        let twice = vec![q("a", Asset::Btc, 1.0, WED_NOON - 5), q("a", Asset::Btc, 2.0, WED_NOON)];
        assert!(matches!(aggregate(&twice, Asset::Btc, WED_NOON, &r), Err(PriceError::NotEnoughSources { .. })));
    }

    #[test]
    fn one_family_counts_once() {
        let r = PriceRules::default();
        // The same gold token on three markets is one source, however many markets quote it.
        let alone = vec![
            q("paxg:kraken", Asset::Gold, 4_000.0, WED_NOON),
            q("paxg:coinbase", Asset::Gold, 4_001.0, WED_NOON),
            q("paxg:bitstamp", Asset::Gold, 4_002.0, WED_NOON),
        ];
        assert_eq!(
            aggregate(&alone, Asset::Gold, WED_NOON, &r),
            Err(PriceError::NotEnoughSources { asset: Asset::Gold, fresh: 1, required: 2 })
        );
        // The token loses its link to gold: its three prices do not outvote the two other sources.
        let mut quotes = vec![
            q("paxg:kraken", Asset::Gold, 3_000.0, WED_NOON),
            q("paxg:coinbase", Asset::Gold, 3_010.0, WED_NOON),
            q("paxg:bitstamp", Asset::Gold, 2_990.0, WED_NOON),
            q("spot:swissquote", Asset::Gold, 4_005.0, WED_NOON),
            q("xaut:kraken", Asset::Gold, 3_995.0, WED_NOON),
        ];
        assert_eq!(aggregate(&quotes, Asset::Gold, WED_NOON, &r), Ok(4_000.0));
        // All three families healthy: the median of the three, the token's own median counting once.
        for quote in quotes.iter_mut().take(3) {
            quote.usd += 1_000.0;
        }
        assert_eq!(aggregate(&quotes, Asset::Gold, WED_NOON, &r), Ok(4_000.0));
        // With the spot source down, the two tokens still agree with each other.
        quotes.remove(3);
        assert_eq!(aggregate(&quotes, Asset::Gold, WED_NOON, &r), Ok(3_997.5));
        assert_eq!(family("paxg:kraken"), "paxg");
        assert_eq!(family("kraken"), "kraken");
    }

    #[test]
    fn refuses_two_sources_that_disagree() {
        let r = PriceRules::default();
        let quotes = vec![q("a", Asset::Gold, 4_000.0, WED_NOON), q("b", Asset::Gold, 4_200.0, WED_NOON)];
        assert!(matches!(aggregate(&quotes, Asset::Gold, WED_NOON, &r), Err(PriceError::Disagreement { .. })));
    }

    #[test]
    fn gold_is_carried_over_the_weekend() {
        let r = PriceRules::default();
        let mut memory = None;
        let fri_20h = WED_NOON + 2 * DAY + 8 * 3600;
        let weekday = vec![
            q("btc1", Asset::Btc, 100_000.0, fri_20h),
            q("btc2", Asset::Btc, 100_000.0, fri_20h),
            q("au1", Asset::Gold, 4_319.0, fri_20h),
            q("au2", Asset::Gold, 4_319.0, fri_20h),
        ];
        let p = menger_price(&weekday, fri_20h, &r, &mut memory).unwrap();
        assert!(!p.gold_carried);
        assert_eq!(p.sats_per_menger, SatsPerMenger(492_255));

        // Saturday: bitcoin has moved, gold sources are silent or quote a synthetic weekend price.
        let sat = fri_20h + DAY;
        let saturday = vec![
            q("btc1", Asset::Btc, 80_000.0, sat),
            q("btc2", Asset::Btc, 80_000.0, sat),
            q("au1", Asset::Gold, 9_999.0, sat),
        ];
        let p = menger_price(&saturday, sat, &r, &mut memory).unwrap();
        assert!(p.gold_carried);
        assert_eq!(p.xau_usd, 4_319.0);
        assert_eq!(p.sats_per_menger, SatsPerMenger::from_usd(80_000.0, 4_319.0).unwrap());

        // Without a gold price from before the closing, there is no price at all.
        let mut empty = None;
        assert_eq!(menger_price(&saturday, sat, &r, &mut empty), Err(PriceError::NoGoldPrice));
    }

    #[test]
    fn gold_carried_too_long_is_refused() {
        let r = PriceRules::default();
        let sat = WED_NOON + 3 * DAY;
        let quotes = vec![q("b1", Asset::Btc, 1.0e5, sat), q("b2", Asset::Btc, 1.0e5, sat)];
        let mut old = Some(GoldMemory { usd: 4_000.0, at: sat - 5 * DAY });
        assert_eq!(menger_price(&quotes, sat, &r, &mut old), Err(PriceError::NoGoldPrice));
    }

    #[test]
    fn stale_gold_on_a_weekday_pauses_everything() {
        // A holiday: the market should be open but the sources have not moved for hours.
        let r = PriceRules::default();
        let mut memory = Some(GoldMemory { usd: 4_000.0, at: WED_NOON - 3 * 3600 });
        let quotes = vec![
            q("b1", Asset::Btc, 1.0e5, WED_NOON),
            q("b2", Asset::Btc, 1.0e5, WED_NOON),
            q("au1", Asset::Gold, 4_000.0, WED_NOON - 3 * 3600),
            q("au2", Asset::Gold, 4_000.0, WED_NOON - 3 * 3600),
        ];
        assert!(matches!(menger_price(&quotes, WED_NOON, &r, &mut memory), Err(PriceError::NotEnoughSources { .. })));
    }
}
