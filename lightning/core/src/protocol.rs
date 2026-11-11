//! Messages between the two parties, carried in the custom records of Lightning payments
//! (keysend). Every message travels with a payment, so the channel itself is the only link
//! between the parties: no server, no relay.
//!
//! Each message names its sender and its recipient and is signed with the sender's node key. A
//! keysend does not reveal who sent it, so without the signature anyone could pretend to be
//! someone else; and without the recipient's name, a message received from a provider could be
//! passed on to another of its savers as if it were meant for them.

use crate::unit::SatsPerMenger;
use serde::{Deserialize, Serialize};

/// Custom TLV record type for Menger messages. Odd, so that a node that does not understand it
/// still accepts the payment; above 2^16, as custom records must be.
pub const TLV_TYPE: u64 = 0x4D4E_4701;

/// Current version of the messages.
pub const VERSION: u8 = 3;

/// Amount sent with a message that is not a stability payment, in millisatoshis.
pub const MESSAGE_AMOUNT_MSAT: u64 = 1_000;

const _: () = assert!(TLV_TYPE % 2 == 1 && TLV_TYPE >= 1 << 16);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Body {
    /// Saver to provider: add these satoshis to the position (or take them out), or open it if
    /// there is none. The saver names the satoshis and the worst amount of Mengers it accepts for
    /// them; the provider prices the change with its own price. Nothing changes until the provider
    /// accepts; then both sides apply the same change. `at` identifies the request and is the time
    /// from which the new position accrues.
    Adjust {
        at: i64,
        delta_backing: i64,
        /// Growing: the fewest micro-Mengers accepted. Shrinking: the most given up.
        limit_micro: u64,
        rate_ppm_per_year: i64,
    },
    /// Provider to saver: answer to the adjustment `at`, with the change in micro-Mengers at the
    /// provider's price when it accepts (zero when it refuses).
    Reply {
        at: i64,
        accepted: bool,
        #[serde(default)]
        delta_micro: i64,
        reason: Option<String>,
    },
    /// Either side: the payment carrying this message is a stability payment.
    Stability {
        /// The payer's price, so that the other side can audit the amount.
        sats_per_menger: SatsPerMenger,
        /// The saver's backing after this payment, as the payer computes it.
        backing_after: u64,
    },
}

#[derive(Serialize)]
struct Unsigned<'a> {
    v: u8,
    from: &'a str,
    to: &'a str,
    #[serde(flatten)]
    body: &'a Body,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    pub v: u8,
    /// Sender's node id, in hex.
    pub from: String,
    /// Recipient's node id, in hex. A message for someone else is ignored.
    pub to: String,
    /// Sender's signature of everything else, made with its node key.
    pub sig: String,
    #[serde(flatten)]
    pub body: Body,
}

impl Envelope {
    /// The bytes the sender signs: the message without its signature.
    pub fn signing_bytes(from: &str, to: &str, body: &Body) -> Vec<u8> {
        serde_json::to_vec(&Unsigned { v: VERSION, from, to, body }).expect("messages always serialize")
    }

    pub fn new(from: String, to: String, body: Body, sign: impl FnOnce(&[u8]) -> String) -> Self {
        let sig = sign(&Self::signing_bytes(&from, &to, &body));
        Envelope { v: VERSION, from, to, sig, body }
    }

    /// True when `verify(bytes, signature)` accepts the signature for the claimed sender.
    pub fn verify(&self, verify: impl FnOnce(&[u8], &str) -> bool) -> bool {
        verify(&Self::signing_bytes(&self.from, &self.to, &self.body), &self.sig)
    }

    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("messages always serialize")
    }

    /// Decodes a message of the current version; anything else is ignored by the caller.
    pub fn decode(bytes: &[u8]) -> Option<Envelope> {
        let e: Envelope = serde_json::from_slice(bytes).ok()?;
        (e.v == VERSION).then_some(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_sign(bytes: &[u8]) -> String {
        format!("{:x}", bytes.iter().fold(0u64, |h, b| h.wrapping_mul(31).wrapping_add(*b as u64)))
    }

    #[test]
    fn round_trip_and_signature() {
        let body = Body::Adjust { at: 1_793_793_600, delta_backing: -246_128, limit_micro: 501_500, rate_ppm_per_year: 0 };
        let e = Envelope::new("02ab".into(), "03ef".into(), body, fake_sign);
        let bytes = e.encode();
        assert!(bytes.len() < 400, "fits easily in an onion payload");
        let back = Envelope::decode(&bytes).unwrap();
        assert_eq!(back, e);
        assert!(back.verify(|b, s| fake_sign(b) == s));
        // Changing the sender, the recipient or the content breaks the signature.
        let mut forged = back.clone();
        forged.from = "03cd".into();
        assert!(!forged.verify(|b, s| fake_sign(b) == s));
        let mut forged = back.clone();
        forged.to = "03cd".into();
        assert!(!forged.verify(|b, s| fake_sign(b) == s));
        let mut forged = back;
        forged.body = Body::Reply { at: 1, accepted: true, delta_micro: 1, reason: None };
        assert!(!forged.verify(|b, s| fake_sign(b) == s));
    }

    #[test]
    fn ignores_other_versions_and_garbage() {
        let old = br#"{"v":2,"from":"02","sig":"x","kind":"stability","sats_per_menger":1,"backing_after":1}"#;
        assert_eq!(Envelope::decode(old), None);
        let no_recipient = br#"{"v":3,"from":"02","sig":"x","kind":"stability","sats_per_menger":1,"backing_after":1}"#;
        assert_eq!(Envelope::decode(no_recipient), None);
        assert_eq!(Envelope::decode(b"not json"), None);
    }
}
