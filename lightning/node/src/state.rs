//! What a party remembers between runs, in `menger-state.json` next to its Lightning data.

use anyhow::{Context, Result};
use menger_core::{GoldMemory, Position, SatsPerMenger};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// A stability payment we sent and that has not completed yet.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InFlight {
    pub payment_id: String,
    pub sats: u64,
}

/// Saver: a change of position sent to the provider and not answered yet. It is sent again,
/// unchanged, until the provider answers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingAdjust {
    /// Identifies the request; each request uses a later `at` than the one before.
    pub at: i64,
    pub delta_micro: i64,
    pub delta_backing: i64,
    pub rate_ppm_per_year: i64,
    pub sats_per_menger: SatsPerMenger,
    /// When we last sent it.
    pub sent_at: i64,
    /// Sent on its own, to add received satoshis to the position: tried again if refused.
    #[serde(default)]
    pub automatic: bool,
}

/// Provider: the last answer given to a saver, sent again if the same request comes back.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answer {
    pub at: i64,
    pub accepted: bool,
    pub reason: Option<String>,
}

/// Saver: a payment to someone else, paid once the position has released the satoshis it needs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Spend {
    pub invoice: String,
    pub sats: u64,
    /// Satoshis taken out of the position to make this payment (zero when free sats sufficed).
    pub released_backing: u64,
    /// The position has released what was needed: the payment can go.
    pub ready: bool,
    pub payment_id: Option<String>,
}

/// Everything about one counterparty: the provider for a saver, each saver for a provider.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PeerState {
    pub position: Option<Position>,
    pub pending: Option<PendingAdjust>,
    /// Saver: `at` of our last request.
    pub last_request_at: Option<i64>,
    /// Provider: our last answer to this saver.
    pub answered: Option<Answer>,
    /// Saver: stability payments that arrived before the answer that opens the position. They
    /// count from the moment the position exists.
    pub early_stability_sats: u64,
    pub last_payment_at: Option<i64>,
    pub in_flight: Option<InFlight>,
    pub owed_to_us_since: Option<i64>,
    /// Why we closed the channels with this counterparty, if we did.
    pub closed: Option<String>,
}

/// One line of the audit trail.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub at: i64,
    pub text: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    /// Saver: its provider, as a hex node id.
    pub provider: Option<String>,
    /// Counterparties by hex node id.
    pub peers: BTreeMap<String, PeerState>,
    /// Saver: a payment to someone else in progress.
    pub spend: Option<Spend>,
    /// Saver: satoshis received (or released and not spent) waiting to be added to the position.
    pub to_stabilize_sats: u64,
    /// Saver: after a refusal, the satoshis to stabilize wait until then.
    pub stabilize_not_before: Option<i64>,
    pub gold: Option<GoldMemory>,
    /// Hashes of the incoming payments already counted, so that a replayed event is not counted twice.
    pub seen_payments: Vec<String>,
    /// The most recent events, newest last.
    pub journal: Vec<Entry>,
}

const JOURNAL_LEN: usize = 500;
const SEEN_LEN: usize = 2_000;

impl State {
    pub fn load(path: &Path) -> Result<State> {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).with_context(|| format!("reading {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(State::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    /// Writes to a temporary file, then renames it: a crash never leaves a half-written state.
    pub fn save(&self, path: &Path) -> Result<()> {
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, path).with_context(|| format!("renaming to {}", path.display()))?;
        Ok(())
    }

    /// Records an incoming payment; false if it was already counted.
    pub fn first_time(&mut self, payment_hash: &str) -> bool {
        if self.seen_payments.iter().any(|h| h == payment_hash) {
            return false;
        }
        self.seen_payments.push(payment_hash.to_string());
        if self.seen_payments.len() > SEEN_LEN {
            let excess = self.seen_payments.len() - SEEN_LEN;
            self.seen_payments.drain(..excess);
        }
        true
    }

    pub fn note(&mut self, at: i64, text: impl Into<String>) {
        self.journal.push(Entry { at, text: text.into() });
        if self.journal.len() > JOURNAL_LEN {
            let excess = self.journal.len() - JOURNAL_LEN;
            self.journal.drain(..excess);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("menger-state.json");
        assert_eq!(State::load(&path).unwrap(), State::default());
        let mut s = State { provider: Some("02ab".into()), ..State::default() };
        s.peers.insert("02ab".into(), PeerState { last_payment_at: Some(7), ..PeerState::default() });
        for i in 0..600 {
            s.note(i, format!("event {i}"));
        }
        s.save(&path).unwrap();
        let back = State::load(&path).unwrap();
        assert_eq!(back, s);
        assert_eq!(back.journal.len(), 500);
        assert_eq!(back.journal[0].text, "event 100");
    }

    #[test]
    fn payments_are_counted_once() {
        let mut s = State::default();
        assert!(s.first_time("aa"));
        assert!(!s.first_time("aa"));
        assert!(s.first_time("bb"));
    }
}
