//! What a party remembers between runs, in `menger-state.json` next to its Lightning data.

use anyhow::{Context, Result};
use menger_core::{GoldMemory, Position};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

/// Version of the messages this state was written for. A state from an earlier version drops its
/// unanswered requests: the other side would no longer understand them.
pub const STATE_VERSION: u32 = 3;

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
    pub delta_backing: i64,
    /// The worst amount of micro-Mengers we accept for these satoshis.
    #[serde(default)]
    pub limit_micro: u64,
    /// What the satoshis were worth at our own price when we asked, to compare with the answer.
    #[serde(default)]
    pub expected_micro: i64,
    pub rate_ppm_per_year: i64,
    /// When we last sent it.
    pub sent_at: i64,
    /// Sent on its own, to add received satoshis to the position: tried again if refused.
    #[serde(default)]
    pub automatic: bool,
    /// Releases satoshis for a payment to someone else.
    #[serde(default)]
    pub for_spend: bool,
}

/// Provider: the last answer given to a saver, sent again if the same request comes back.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answer {
    pub at: i64,
    pub accepted: bool,
    /// The change in micro-Mengers we granted, at our own price.
    #[serde(default)]
    pub delta_micro: i64,
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
    /// When the person asked for it. A payment that cannot leave in time is given up.
    #[serde(default)]
    pub started_at: i64,
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
    /// Since when this counterparty cannot be reached, if it cannot.
    pub unreachable_since: Option<i64>,
    /// Since when it has been reachable without interruption.
    pub reachable_since: Option<i64>,
    /// Why the channels with this counterparty are closed or closing, if they are.
    pub closed: Option<String>,
    /// When we asked for the channels to close. Past a delay, the close is forced.
    pub closing_since: Option<i64>,
    /// The close was forced: the funds come back after the waiting period.
    pub force_closed: bool,
    /// Provider: we already opened a channel in return for this saver's.
    pub return_opened: bool,
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
    /// See [`STATE_VERSION`].
    pub version: u32,
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

/// The state in `path`, or `None` if there is no such file.
fn read(path: &Path) -> Result<Option<State>> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).map(Some).with_context(|| format!("reading {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

const JOURNAL_LEN: usize = 500;
const SEEN_LEN: usize = 2_000;

impl State {
    /// Reads the state, or the copy kept from the save before if the file is missing or damaged.
    /// No file at all is a new start.
    pub fn load(path: &Path) -> Result<State> {
        let backup = path.with_extension("json.bak");
        let mut state = match read(path) {
            Ok(Some(state)) => state,
            Ok(None) => read(&backup)?.unwrap_or_default(),
            Err(e) => match read(&backup) {
                Ok(Some(state)) => {
                    eprintln!("{e:#} ; état relu depuis {}", backup.display());
                    state
                }
                _ => return Err(e),
            },
        };
        if state.version < STATE_VERSION {
            for peer in state.peers.values_mut() {
                peer.pending = None;
                peer.answered = None;
            }
            // A payment that had not left yet waited for one of those requests.
            if state.spend.as_ref().is_some_and(|s| s.payment_id.is_none()) {
                state.spend = None;
            }
            state.version = STATE_VERSION;
        }
        Ok(state)
    }

    /// Writes to a temporary file, makes sure it is on the disk, then renames it: neither a crash
    /// nor a power cut leaves a half-written state. The save before is kept as a copy.
    pub fn save(&self, path: &Path) -> Result<()> {
        let tmp = path.with_extension("json.tmp");
        let mut file = std::fs::File::create(&tmp).with_context(|| format!("writing {}", tmp.display()))?;
        file.write_all(&serde_json::to_vec_pretty(self)?).with_context(|| format!("writing {}", tmp.display()))?;
        file.sync_all().with_context(|| format!("writing {}", tmp.display()))?;
        drop(file);
        if path.exists() {
            let backup = path.with_extension("json.bak");
            std::fs::rename(path, &backup).with_context(|| format!("renaming to {}", backup.display()))?;
        }
        std::fs::rename(&tmp, path).with_context(|| format!("renaming to {}", path.display()))?;
        #[cfg(unix)]
        if let Some(dir) = path.parent().and_then(|d| std::fs::File::open(d).ok()) {
            let _ = dir.sync_all();
        }
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
        assert_eq!(State::load(&path).unwrap(), State { version: STATE_VERSION, ..State::default() });
        let mut s = State { version: STATE_VERSION, provider: Some("02ab".into()), ..State::default() };
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
    fn a_damaged_or_missing_file_falls_back_on_the_copy() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("menger-state.json");
        let mut s = State { version: STATE_VERSION, provider: Some("02ab".into()), ..State::default() };
        s.save(&path).unwrap();
        s.to_stabilize_sats = 5;
        s.save(&path).unwrap();
        // A file cut short: the save before is used.
        std::fs::write(&path, b"{ \"provider\": ").unwrap();
        let back = State::load(&path).unwrap();
        assert_eq!(back.provider.as_deref(), Some("02ab"));
        assert_eq!(back.to_stabilize_sats, 0);
        // A crash between the two renames leaves only the copy.
        std::fs::remove_file(&path).unwrap();
        assert_eq!(State::load(&path).unwrap().provider.as_deref(), Some("02ab"));
        // Damaged with no copy: an error, never a silent new start that forgets a position.
        std::fs::remove_file(path.with_extension("json.bak")).unwrap();
        std::fs::write(&path, b"{").unwrap();
        assert!(State::load(&path).is_err());
    }

    #[test]
    fn a_state_from_earlier_messages_drops_what_was_unanswered() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("menger-state.json");
        let old = r#"{"provider":"02ab","peers":{"02ab":{"position":{"micro_at_start":2000000,"started_at":1,"rate_ppm_per_year":0,"backing_sats":984510},
            "pending":{"at":5,"delta_micro":1,"delta_backing":2,"rate_ppm_per_year":0,"sats_per_menger":492255,"sent_at":5}}},
            "spend":{"invoice":"lnbc1","sats":1,"released_backing":1,"ready":false,"payment_id":null}}"#;
        std::fs::write(&path, old).unwrap();
        let s = State::load(&path).unwrap();
        assert_eq!(s.version, STATE_VERSION);
        let peer = &s.peers["02ab"];
        assert!(peer.pending.is_none() && s.spend.is_none());
        assert_eq!(peer.position.as_ref().unwrap().backing_sats, 984_510, "the position itself is kept");
    }

    #[test]
    fn payments_are_counted_once() {
        let mut s = State::default();
        assert!(s.first_time("aa"));
        assert!(!s.first_time("aa"));
        assert!(s.first_time("bb"));
    }
}
