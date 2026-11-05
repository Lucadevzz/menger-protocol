//! Messages between the two parties, carried in the custom records of Lightning payments
//! (keysend). Every message travels with a payment, so the channel itself is the only link
//! between the parties: no server, no relay.
//!
//! Each message names its sender and is signed with the sender's node key. A keysend does not
//! reveal who sent it, so without the signature anyone could pretend to be someone else.

use crate::unit::SatsPerMenger;
use serde::{Deserialize, Serialize};

/// Custom TLV record type for Menger messages. Odd, so that a node that does not understand it
/// still accepts the payment; above 2^16, as custom records must be.
pub const TLV_TYPE: u64 = 0x4D4E_4701;

/// Current version of the messages.
pub const VERSION: u8 = 2;

/// Amount sent with a message that is not a stability payment, in millisatoshis.
pub const MESSAGE_AMOUNT_MSAT: u64 = 1_000;

const _: () = assert!(TLV_TYPE % 2 == 1 && TLV_TYPE >= 1 << 16);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Body {
    /// Saver to provider: change the position by these amounts, or open it if there is none.
    /// Nothing changes until the provider accepts; then both sides apply the same change.
    /// `at` identifies the request and is the time from which the new position accrues.
    Adjust {
        at: i64,
        delta_micro: i64,
        delta_backing: i64,
        rate_ppm_per_year: i64,
        /// The saver's price, for the provider to compare with its own.
        sats_per_menger: SatsPerMenger,
    },
    /// Provider to saver: answer to the adjustment `at`.
    Reply { at: i64, accepted: bool, reason: Option<String> },
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
    #[serde(flatten)]
    body: &'a Body,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    pub v: u8,
    /// Sender's node id, in hex.
    pub from: String,
    /// Sender's signature of everything else, made with its node key.
    pub sig: String,
    #[serde(flatten)]
    pub body: Body,
}

impl Envelope {
    /// The bytes the sender signs: the message without its signature.
    pub fn signing_bytes(from: &str, body: &Body) -> Vec<u8> {
        serde_json::to_vec(&Unsigned { v: VERSION, from, body }).expect("messages always serialize")
    }

    pub fn new(from: String, body: Body, sign: impl FnOnce(&[u8]) -> String) -> Self {
        let sig = sign(&Self::signing_bytes(&from, &body));
        Envelope { v: VERSION, from, sig, body }
    }

    /// True when `verify(bytes, signature)` accepts the signature for the claimed sender.
    pub fn verify(&self, verify: impl FnOnce(&[u8], &str) -> bool) -> bool {
        verify(&Self::signing_bytes(&self.from, &self.body), &self.sig)
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
        let body = Body::Adjust {
            at: 1_793_793_600,
            delta_micro: -500_000,
            delta_backing: -246_128,
            rate_ppm_per_year: 0,
            sats_per_menger: SatsPerMenger(492_255),
        };
        let e = Envelope::new("02ab".into(), body, fake_sign);
        let bytes = e.encode();
        assert!(bytes.len() < 400, "fits easily in an onion payload");
        let back = Envelope::decode(&bytes).unwrap();
        assert_eq!(back, e);
        assert!(back.verify(|b, s| fake_sign(b) == s));
        // Changing the sender or the content breaks the signature.
        let mut forged = back.clone();
        forged.from = "03cd".into();
        assert!(!forged.verify(|b, s| fake_sign(b) == s));
        let mut forged = back;
        forged.body = Body::Reply { at: 1, accepted: true, reason: None };
        assert!(!forged.verify(|b, s| fake_sign(b) == s));
    }

    #[test]
    fn ignores_other_versions_and_garbage() {
        assert_eq!(Envelope::decode(br#"{"v":1,"from":"02","sig":"x","kind":"stability","sats_per_menger":1,"backing_after":1}"#), None);
        assert_eq!(Envelope::decode(b"not json"), None);
    }
}
