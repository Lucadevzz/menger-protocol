//! The unit: 1 Menger = 3.545 g of fine gold, the weight of the Venetian ducat.
//!
//! Amounts of Mengers are counted in micro-Mengers (1 MNGR = 1,000,000 µMNGR) and bitcoin in
//! satoshis, both as integers. Floating point is used only to turn two dollar quotes into a price.

use serde::{Deserialize, Serialize};

/// Grams of fine gold in one Menger.
pub const GRAMS_PER_MENGER: f64 = 3.545;
/// Grams in one troy ounce, the unit gold is quoted in.
pub const GRAMS_PER_TROY_OUNCE: f64 = 31.103_476_8;
/// Micro-Mengers in one Menger.
pub const MICRO_PER_MENGER: u64 = 1_000_000;
/// Satoshis in one bitcoin.
pub const SATS_PER_BTC: f64 = 100_000_000.0;

/// Price of one Menger in satoshis.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SatsPerMenger(pub u64);

impl SatsPerMenger {
    /// Price of one Menger from the dollar price of a bitcoin and of a troy ounce of gold.
    /// Returns `None` for zero, negative or non-finite inputs, or a price under one satoshi.
    pub fn from_usd(btc_usd: f64, xau_usd_per_ounce: f64) -> Option<Self> {
        if !(btc_usd.is_finite() && xau_usd_per_ounce.is_finite()) || btc_usd <= 0.0 || xau_usd_per_ounce <= 0.0 {
            return None;
        }
        let menger_usd = xau_usd_per_ounce * GRAMS_PER_MENGER / GRAMS_PER_TROY_OUNCE;
        let sats = (SATS_PER_BTC * menger_usd / btc_usd).round();
        if !(sats >= 1.0 && sats < u64::MAX as f64) {
            return None;
        }
        Some(SatsPerMenger(sats as u64))
    }

    /// Satoshis worth `micro` micro-Mengers at this price, rounded to the nearest satoshi.
    pub fn sats_for(self, micro: u64) -> u64 {
        let v = (micro as u128 * self.0 as u128 + (MICRO_PER_MENGER as u128 / 2)) / MICRO_PER_MENGER as u128;
        v.min(u64::MAX as u128) as u64
    }

    /// Micro-Mengers worth `sats` satoshis at this price, rounded to the nearest micro-Menger.
    pub fn micro_for(self, sats: u64) -> u64 {
        if self.0 == 0 {
            return 0;
        }
        let v = (sats as u128 * MICRO_PER_MENGER as u128 + self.0 as u128 / 2) / self.0 as u128;
        v.min(u64::MAX as u128) as u64
    }

    /// Micro-Mengers worth `sats` satoshis at this price, rounded down: never more than they are
    /// worth.
    pub fn micro_for_floor(self, sats: u64) -> u64 {
        if self.0 == 0 {
            return 0;
        }
        let v = sats as u128 * MICRO_PER_MENGER as u128 / self.0 as u128;
        v.min(u64::MAX as u128) as u64
    }

    /// Micro-Mengers worth `sats` satoshis at this price, rounded up: never fewer than they are
    /// worth.
    pub fn micro_for_ceil(self, sats: u64) -> u64 {
        if self.0 == 0 {
            return 0;
        }
        let v = (sats as u128 * MICRO_PER_MENGER as u128).div_ceil(self.0 as u128);
        v.min(u64::MAX as u128) as u64
    }
}

/// Formats micro-Mengers as Mengers with six decimals, e.g. `1.250000`.
pub fn format_mengers(micro: u64) -> String {
    format!("{}.{:06}", micro / MICRO_PER_MENGER, micro % MICRO_PER_MENGER)
}

/// Mengers written the French way, without trailing zeros: `0,3`, `3,047201`, `2`.
pub fn format_mengers_fr(micro: u64) -> String {
    let whole = group_fr(micro / MICRO_PER_MENGER);
    let frac = format!("{:06}", micro % MICRO_PER_MENGER);
    let frac = frac.trim_end_matches('0');
    if frac.is_empty() {
        whole
    } else {
        format!("{whole},{frac}")
    }
}

/// Satoshis written the French way: `1 500 000`, with narrow no-break spaces.
pub fn format_sats_fr(sats: u64) -> String {
    group_fr(sats)
}

fn group_fr(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push('\u{202f}');
        }
        out.push(c);
    }
    out
}

/// Grams of gold represented by `micro` micro-Mengers.
pub fn grams(micro: u64) -> f64 {
    micro as f64 / MICRO_PER_MENGER as f64 * GRAMS_PER_MENGER
}

/// Reads an amount of Mengers typed by a person, such as `2`, `0.5` or `0,25`, into
/// micro-Mengers. At most six decimals; anything else is `None`.
pub fn parse_mengers(text: &str) -> Option<u64> {
    let text = text.trim().replace(',', ".");
    let (whole, frac) = text.split_once('.').unwrap_or((&text, ""));
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if (whole.is_empty() && frac.is_empty()) || !digits(whole) || !digits(frac) || frac.len() > 6 {
        return None;
    }
    let whole: u64 = if whole.is_empty() { 0 } else { whole.parse().ok()? };
    let frac: u64 = format!("{frac:0<6}").parse().ok()?;
    whole.checked_mul(MICRO_PER_MENGER)?.checked_add(frac)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn price_from_dollar_quotes() {
        // Gold at 4,319 $/oz makes 1 Menger worth 492.26 $; with BTC at 100,000 $ that is 492,255 sats.
        let p = SatsPerMenger::from_usd(100_000.0, 4_319.0).unwrap();
        assert_eq!(p, SatsPerMenger(492_255));
        assert_eq!(p.sats_for(MICRO_PER_MENGER), 492_255);
        assert_eq!(p.micro_for(492_255), MICRO_PER_MENGER);
    }

    #[test]
    fn rejects_bad_quotes() {
        assert_eq!(SatsPerMenger::from_usd(0.0, 4_000.0), None);
        assert_eq!(SatsPerMenger::from_usd(100_000.0, -1.0), None);
        assert_eq!(SatsPerMenger::from_usd(f64::NAN, 4_000.0), None);
        assert_eq!(SatsPerMenger::from_usd(100_000.0, f64::INFINITY), None);
        // A Menger worth less than one satoshi cannot be represented.
        assert_eq!(SatsPerMenger::from_usd(1e15, 1.0), None);
    }

    #[test]
    fn french_amounts() {
        assert_eq!(format_mengers_fr(300_000), "0,3");
        assert_eq!(format_mengers_fr(3_047_201), "3,047201");
        assert_eq!(format_mengers_fr(2_000_000), "2");
        assert_eq!(format_mengers_fr(0), "0");
        assert_eq!(format_sats_fr(1_500_000), "1\u{202f}500\u{202f}000");
        assert_eq!(format_sats_fr(147_677), "147\u{202f}677");
        assert_eq!(format_sats_fr(999), "999");
    }

    #[test]
    fn reading_typed_amounts() {
        assert_eq!(parse_mengers("2"), Some(2_000_000));
        assert_eq!(parse_mengers("0.5"), Some(500_000));
        assert_eq!(parse_mengers(" 0,25 "), Some(250_000));
        assert_eq!(parse_mengers(".000001"), Some(1));
        assert_eq!(parse_mengers("1.5000001"), None);
        assert_eq!(parse_mengers("-1"), None);
        assert_eq!(parse_mengers("1e3"), None);
        assert_eq!(parse_mengers("."), None);
        assert_eq!(parse_mengers(""), None);
    }

    #[test]
    fn formatting() {
        assert_eq!(format_mengers(1_250_000), "1.250000");
        assert_eq!(format_mengers(7), "0.000007");
        assert!((grams(2 * MICRO_PER_MENGER) - 7.09).abs() < 1e-9);
    }

    proptest! {
        // Converting sats to Mengers and back loses at most what one micro-Menger is worth.
        #[test]
        fn round_trip_is_tight(price in 1u64..10_000_000, sats in 0u64..2_100_000_000_000_000) {
            let p = SatsPerMenger(price);
            let back = p.sats_for(p.micro_for(sats));
            let one_micro_in_sats = price / MICRO_PER_MENGER + 1;
            prop_assert!(back.abs_diff(sats) <= one_micro_in_sats);
        }

        // A higher bitcoin price means fewer satoshis per Menger; a higher gold price, more.
        #[test]
        fn price_moves_the_right_way(btc in 1_000.0f64..1_000_000.0, gold in 100.0f64..20_000.0) {
            let base = SatsPerMenger::from_usd(btc, gold).unwrap();
            prop_assert!(SatsPerMenger::from_usd(btc * 1.1, gold).unwrap() <= base);
            prop_assert!(SatsPerMenger::from_usd(btc, gold * 1.1).unwrap() >= base);
        }
    }
}
