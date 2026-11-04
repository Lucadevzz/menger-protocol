//! Messages between the two parties, carried in the custom records of Lightning payments
//! (keysend). Every message travels with a payment, so the channel itself is the only link
//! between the parties: no server, no relay.

use crate::unit::SatsPerMenger;
use serde::{Deserialize, Serialize};

/// Custom TLV record type for Menger messages. Odd, so that a node that does not understand it
/// still accepts the payment; above 2^16, as custom records must be.
pub const TLV_TYPE: u64 = 0x4D4E_4701;

/// Current version of the messages.
pub const VERSION: u8 = 1;

/// Amount sent with a message that is not a stability payment, in millisatoshis.
pub const MESSAGE_AMOUNT_MSAT: u64 = 1_000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Body {
    /// Saver to provider: proposes a position (or replaces the current one).
    /// A position of zero Mengers ends stabilization.
    Position {
        micro_at_start: u64,
        started_at: i64,
        rate_ppm_per_year: i64,
        backing_sats: u64,
        /// The saver's price when proposing, for the provider to compare with its own.
        sats_per_menger: SatsPerMenger,
    },
    /// Provider to saver: answer to a proposed position, identified by its start time.
    PositionReply { started_at: i64, accepted: bool, reason: Option<String> },
    /// Either side: the payment carrying this message is a stability payment.
    Stability {
        started_at: i64,
        /// The payer's price, so that the other side can audit the amount.
        sats_per_menger: SatsPerMenger,
        /// The saver's backing after this payment, as the payer computes it.
        backing_after: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub v: u8,
    #[serde(flatten)]
    pub body: Body,
}

impl Message {
    pub fn new(body: Body) -> Self {
        Message { v: VERSION, body }
    }

    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("messages always serialize")
    }

    /// Decodes a message of a known version; anything else is ignored by the caller.
    pub fn decode(bytes: &[u8]) -> Option<Message> {
        let m: Message = serde_json::from_slice(bytes).ok()?;
        (m.v == VERSION).then_some(m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let m = Message::new(Body::Stability {
            started_at: 1_793_793_600,
            sats_per_menger: SatsPerMenger(492_255),
            backing_after: 984_510,
        });
        let bytes = m.encode();
        assert!(bytes.len() < 200, "fits easily in an onion payload");
        assert_eq!(Message::decode(&bytes), Some(m));
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("\"kind\":\"stability\""));
    }

    #[test]
    fn ignores_unknown_versions_and_garbage() {
        assert_eq!(Message::decode(b"{\"v\":2,\"kind\":\"stability\"}"), None);
        assert_eq!(Message::decode(b"not json"), None);
    }

    // Odd, so that a node that does not understand it still accepts the payment; above 2^16, as
    // custom records must be. Checked when compiling.
    const _: () = assert!(TLV_TYPE % 2 == 1 && TLV_TYPE >= 1 << 16);
}
