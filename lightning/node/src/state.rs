//! What a party remembers between runs, in `menger-state.json` next to its Lightning data.

use anyhow::{Context, Result};
use menger_core::{GoldMemory, Position};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// A stability payment we sent and that has not completed yet.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InFlight {
    pub payment_id: String,
    pub sats: u64,
}

/// One line of the audit trail.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub at: i64,
    pub text: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct State {
    /// The other party, as a hex public key.
    pub counterparty: Option<String>,
    /// The position both parties agreed on.
    pub position: Option<Position>,
    /// Saver only: a position proposed and not answered yet.
    pub proposed: Option<Position>,
    pub last_payment_at: Option<i64>,
    pub in_flight: Option<InFlight>,
    pub owed_to_us_since: Option<i64>,
    pub gold: Option<GoldMemory>,
    /// Why we closed the channels, if we did.
    pub closed: Option<String>,
    /// Hashes of the incoming payments already counted, so that a replayed event is not counted twice.
    #[serde(default)]
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
        let mut s = State { counterparty: Some("02ab".into()), ..State::default() };
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
