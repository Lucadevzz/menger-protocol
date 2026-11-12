//! One party (saver or provider): a Lightning node plus the Menger rules.
//!
//! A saver has one provider; a provider serves any number of savers. Each call to
//! [`Party::tick`] reads the node's events, values every position with the party's own prices and
//! does what the rules say: pay, wait, or leave. A saver can also pay anyone on Lightning from its
//! balance, through its provider, and add what it receives to its stable balance.

use crate::keys;
use crate::state::{Answer, InFlight, PeerState, PendingAdjust, Spend, State};
use anyhow::{anyhow, bail, Context, Result};
use ldk_node::bitcoin::secp256k1::PublicKey;
use ldk_node::bitcoin::{Address, Network, Txid};
use ldk_node::lightning::chain::channelmonitor::BalanceSource;
use ldk_node::lightning::ln::channelmanager::PaymentId;
use ldk_node::lightning::ln::msgs::SocketAddress;
use ldk_node::lightning::routing::router::RouteParametersConfig;
use ldk_node::lightning_invoice::{Bolt11Invoice, Bolt11InvoiceDescription, Description};
use ldk_node::liquidity::LSPS2ServiceConfig;
use ldk_node::lightning_types::payment::{PaymentHash, PaymentPreimage};
use ldk_node::payment::{PaymentKind, PaymentStatus};
use ldk_node::{Builder, ChannelDetails, CustomTlvRecord, Event, LightningBalance, Node, PendingSweepBalance};
use menger_core::price::{menger_price, PricePoint, PriceRules, Quote};
use menger_core::protocol::{Body, Envelope, MESSAGE_AMOUNT_MSAT, TLV_TYPE};
use menger_core::stability::{
    adjust, adjust_limit, apply_payment, check_reply, decide, quote_adjust, Action, AdjustRequest, AdjustRules, CloseReason,
    Decision, Params, Position, Role, Snapshot, MAX_RATE_PPM_PER_YEAR, MAX_SATS,
};
use menger_core::unit::{format_mengers_fr, format_sats_fr as sats_fr};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::str::FromStr;

/// Saver: a request the provider has not answered is sent again after this many seconds.
pub const RESEND_SECS: i64 = 60;
/// Saver: smaller amounts are not worth a change of position.
pub const MIN_ADJUST_SATS: u64 = 1_000;
/// Saver: satoshis that the provider refused to add to the position are offered again after this.
pub const RETRY_REFUSED_SECS: i64 = 300;
/// Saver: a payment whose satoshis the provider has not released after this long is given up.
pub const SPEND_TIMEOUT_SECS: i64 = 300;
/// A close asked politely that has not completed after this long is forced.
pub const CLOSE_TIMEOUT_SECS: i64 = 900;
/// Provider: how far a saver's balance may fall short of the backing it claims, for the satoshi
/// that each message carries.
const MESSAGE_TOLERANCE_SATS: u64 = 10;
/// Provider: on-chain bitcoin kept aside when it opens a channel in return, for fees.
const RETURN_CHANNEL_MARGIN_SATS: u64 = 30_000;

/// The most a payment to someone else may cost in routing fees. It is kept aside before the
/// payment leaves; what it does not spend returns to the position.
pub fn fee_margin(sats: u64) -> u64 {
    sats / 100 + 50
}

#[derive(Clone, Debug)]
pub enum ChainSource {
    BitcoindRpc { host: String, port: u16, user: String, password: String },
    Esplora { url: String },
}

/// Provider: opens a channel of its own towards each saver that opens one to it, so that it has
/// bitcoin on its side to pay the saver when bitcoin falls, and so that the saver can receive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReturnChannel {
    /// Size of our channel, in millionths of the saver's (1,000,000 = the same size).
    pub ratio_ppm: u64,
    /// Smaller channels from savers get nothing in return.
    pub min_sats: u64,
    /// Never more than this in one channel.
    pub max_sats: u64,
}

#[derive(Clone, Debug)]
pub struct PartyConfig {
    pub role: Role,
    pub data_dir: PathBuf,
    pub network: Network,
    /// Address the Lightning node listens on, e.g. `0.0.0.0:9735`. A saver needs none: it is the
    /// one that calls its provider.
    pub listen: Option<String>,
    pub chain: ChainSource,
    pub params: Params,
    pub price_rules: PriceRules,
    /// Saver: its provider's node id and address (`host:port`).
    pub provider: Option<(PublicKey, String)>,
    /// Saver: price of stability it asks for when opening a position.
    pub rate_ppm_per_year: i64,
    /// Saver: how far from its own price it lets the provider price a change of position, in
    /// millionths (3,000 = 0.3 %).
    pub max_slippage_ppm: u64,
    /// Provider: what it accepts when a saver changes its position.
    pub adjust_rules: AdjustRules,
    /// Provider: share of a bitcoin fall against gold it must be able to cover to accept more.
    pub min_coverage_ppm: u64,
    /// Provider: a name for the node, needed to open channels that the network knows about.
    pub alias: Option<String>,
    /// Provider: open a channel in return for each saver's.
    pub return_channel: Option<ReturnChannel>,
    /// Saver: add the satoshis it receives to its stable balance.
    pub auto_stabilize: bool,
    /// Restore the wallet from this recovery phrase (only in a folder that has no wallet yet).
    pub restore_phrase: Option<String>,
}

impl PartyConfig {
    pub fn new(role: Role, data_dir: PathBuf, network: Network, listen: Option<String>, chain: ChainSource) -> Self {
        let mut params = Params::default();
        if role == Role::Provider {
            // A saver's computer sleeps at night and over a week-end.
            params.offline_grace_secs = 72 * 3_600;
        }
        PartyConfig {
            role,
            data_dir,
            network,
            listen,
            chain,
            params,
            price_rules: PriceRules::default(),
            provider: None,
            rate_ppm_per_year: 0,
            max_slippage_ppm: 3_000,
            adjust_rules: AdjustRules::default(),
            min_coverage_ppm: 300_000,
            alias: None,
            return_channel: None,
            auto_stabilize: true,
            restore_phrase: None,
        }
    }
}

/// Why a saver asks for a change of position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Purpose {
    /// The person asked.
    Manual,
    /// Received satoshis join the position on their own.
    Automatic,
    /// Satoshis leave the position for a payment to someone else.
    Spend,
}

/// One position, as valued during a tick.
#[derive(Clone, Debug, Serialize)]
pub struct PeerReport {
    pub peer: String,
    pub decision: Decision,
    pub micro_mengers: u64,
    pub backing_sats: u64,
    /// The other party could be reached during this tick.
    pub online: bool,
}

/// What happened during one tick, for display and tests.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Report {
    pub at: i64,
    pub price: Option<PricePoint>,
    pub price_error: Option<String>,
    pub peers: Vec<PeerReport>,
    pub events: Vec<String>,
}

pub struct Party {
    pub cfg: PartyConfig,
    pub node: Node,
    pub state: State,
    state_path: PathBuf,
    /// The recovery phrase, when this start created it: to be shown once to its owner.
    pub new_phrase: Option<String>,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn payment_hex(id: &PaymentId) -> String {
    hex(&id.0)
}

fn signed_mengers(micro: i64) -> String {
    format!("{}{}", if micro < 0 { "−" } else { "+" }, format_mengers_fr(micro.unsigned_abs()))
}

fn signed_sats(sats: i64) -> String {
    format!("{}{}", if sats < 0 { "−" } else { "+" }, sats_fr(sats.unsigned_abs()))
}

/// Why a party leaves, in words.
pub fn close_reason_text(reason: &CloseReason) -> String {
    match reason {
        CloseReason::Unpaid { owed_sats } => format!("l'autre partie ne paie pas les {} sats qu'elle doit", sats_fr(*owed_sats)),
        CloseReason::ProviderCannotCover { coverage_ppm } => format!(
            "le fournisseur ne couvre plus qu'une baisse de {} % du bitcoin face à l'or",
            coverage_ppm / 10_000
        ),
        CloseReason::BackingMissing { missing_sats } => {
            format!("l'épargnant ne détient plus {} sats de ceux que compte sa position", sats_fr(*missing_sats))
        }
    }
}

/// What we hold in one channel, before closing fees. For a channel we opened, this leaves out
/// the two small outputs set aside for fees (660 sats), so each side counts a little less for
/// itself than the other side counts for it, never more.
fn our_balance_in(c: &ChannelDetails, claimable: &[LightningBalance]) -> u64 {
    if c.outbound_capacity_msat > 0 {
        // What we can send, plus the reserve the channel keeps us from sending.
        return c.outbound_capacity_msat / 1000 + c.unspendable_punishment_reserve.unwrap_or(0);
    }
    // Less than the reserve: only what a close would give us can be told.
    claimable
        .iter()
        .find_map(|b| match b {
            LightningBalance::ClaimableOnChannelClose { channel_id, amount_satoshis, .. } if *channel_id == c.channel_id => {
                Some(*amount_satoshis)
            }
            _ => None,
        })
        .unwrap_or(0)
}

/// The first characters of a node id, to tell savers apart in messages.
pub fn short_id(id: &str) -> &str {
    &id[..8.min(id.len())]
}

impl Party {
    /// Starts the Lightning node and loads the saved state.
    pub fn start(cfg: PartyConfig) -> Result<Party> {
        if cfg.rate_ppm_per_year.saturating_abs() > MAX_RATE_PPM_PER_YEAR {
            bail!("prix de la stabilité hors des bornes (au plus 100 % par an, dans un sens ou dans l'autre)");
        }
        std::fs::create_dir_all(&cfg.data_dir).with_context(|| format!("creating {}", cfg.data_dir.display()))?;
        let keys = keys::load_or_create(&cfg.data_dir, cfg.restore_phrase.as_deref())?;
        let mut builder = Builder::new();
        builder.set_network(cfg.network);
        builder.set_storage_dir_path(cfg.data_dir.join("ldk").to_string_lossy().into_owned());
        match &cfg.chain {
            ChainSource::BitcoindRpc { host, port, user, password } => {
                // A restored wallet looks for its coins from the start of the chain.
                let rescan_from = keys.restored.then_some(0);
                builder.set_chain_source_bitcoind_rpc(host.clone(), *port, user.clone(), password.clone(), rescan_from);
            }
            ChainSource::Esplora { url } => {
                builder.set_chain_source_esplora(url.clone(), None);
            }
        }
        if let Some(listen) = &cfg.listen {
            let listen = SocketAddress::from_str(listen).map_err(|_| anyhow!("invalid listen address {listen}"))?;
            builder.set_listening_addresses(vec![listen]).map_err(|e| anyhow!("{e}"))?;
        }
        if let Some(alias) = &cfg.alias {
            builder.set_node_alias(alias.clone()).map_err(|e| anyhow!("nom du nœud : {e}"))?;
        }
        if cfg.role == Role::Provider {
            // The provider is its savers' Lightning service provider (LSPS2, bLIP-52). With LDK
            // Node, this is what lets it forward payments to their private channels. Menger does
            // not use the just-in-time channels of that service, and nobody is given the secret
            // that asking for one requires: strangers cannot reach that code.
            //
            // Savers are deliberately not LSPS2 clients: they do not accept the provider's
            // channels before they confirm, and so never trust it not to double-spend them. Since
            // LDK 0.3 they accept payments as large as a private channel all the same.
            builder.enable_liquidity_provider(LSPS2ServiceConfig {
                require_token: Some(keys::random_hex(16)?),
                advertise_service: false,
                channel_opening_fee_ppm: 10_000,
                channel_over_provisioning_ppm: 0,
                min_channel_opening_fee_msat: 2_000_000,
                min_channel_lifetime: 1_008,
                max_client_to_self_delay: 2_016,
                min_payment_size_msat: 10_000_000,
                max_payment_size_msat: 100_000_000_000,
                client_trusts_lsp: false,
                disable_client_reserve: false,
            });
        }
        let node = builder.build(keys.entropy).map_err(|e| anyhow!("building the Lightning node: {e}"))?;
        node.start().map_err(|e| anyhow!("starting the Lightning node: {e}"))?;
        let state_path = cfg.data_dir.join("menger-state.json");
        let state = State::load(&state_path)?;
        let mut party = Party { cfg, node, state, state_path, new_phrase: keys.new_phrase };
        if party.cfg.role == Role::Saver {
            if let Some((key, _)) = party.cfg.provider.clone() {
                // The provider named at start-up becomes ours, unless channels still tie us to
                // another one: a saver has one provider at a time.
                let tied = party.provider().is_ok_and(|old| old != key && !party.channels_all(&old).is_empty());
                if !tied {
                    party.state.provider = Some(key.to_string());
                }
            }
        }
        Ok(party)
    }

    pub fn stop(self) -> Result<()> {
        self.state.save(&self.state_path)?;
        self.node.stop().map_err(|e| anyhow!("stopping: {e}"))
    }

    pub fn role(&self) -> Role {
        self.cfg.role
    }

    pub fn node_id(&self) -> PublicKey {
        self.node.node_id()
    }

    pub fn new_address(&self) -> Result<String> {
        Ok(self.node.onchain_payment().new_address().map_err(|e| anyhow!("{e}"))?.to_string())
    }

    pub fn onchain_spendable_sats(&self) -> u64 {
        self.node.list_balances().spendable_onchain_balance_sats
    }

    /// Bitcoin on its way back from closed channels: not in a channel any more, not spendable
    /// on the chain yet.
    pub fn returning_sats(&self) -> u64 {
        let b = self.node.list_balances();
        let open: Vec<_> = self.node.list_channels().into_iter().map(|c| c.channel_id).collect();
        let closing: u64 = b
            .lightning_balances
            .iter()
            .map(|l| match l {
                // Closed, and the closing transaction is not in a block yet.
                LightningBalance::ClaimableOnChannelClose { channel_id, amount_satoshis, .. }
                    if !open.contains(channel_id) =>
                {
                    *amount_satoshis
                }
                // A polite close pays our wallet directly: once confirmed, it is there.
                LightningBalance::ClaimableAwaitingConfirmations { source: BalanceSource::CoopClose, .. } => 0,
                // After a forced close, ours once the waiting period of the channel is over.
                LightningBalance::ClaimableAwaitingConfirmations { amount_satoshis, .. }
                | LightningBalance::ContentiousClaimable { amount_satoshis, .. }
                | LightningBalance::CounterpartyRevokedOutputClaimable { amount_satoshis, .. } => *amount_satoshis,
                _ => 0,
            })
            .sum();
        // Once its sweep is confirmed, an output is in the on-chain wallet; the node keeps listing
        // it for weeks, as a precaution of its own.
        let sweeping: u64 = b
            .pending_balances_from_channel_closures
            .iter()
            .map(|s| match s {
                PendingSweepBalance::PendingBroadcast { amount_satoshis, .. }
                | PendingSweepBalance::BroadcastAwaitingConfirmation { amount_satoshis, .. } => *amount_satoshis,
                PendingSweepBalance::AwaitingThresholdConfirmations { .. } => 0,
            })
            .sum();
        closing + sweeping
    }

    pub fn sync(&self) {
        let _ = self.node.sync_wallets();
    }

    fn save(&self) -> Result<()> {
        self.state.save(&self.state_path)
    }

    // ----- Counterparties and channels -----

    /// Saver: its provider.
    pub fn provider(&self) -> Result<PublicKey> {
        let hex = self.state.provider.as_deref().ok_or_else(|| anyhow!("pas encore de fournisseur"))?;
        PublicKey::from_str(hex).map_err(|e| anyhow!("bad provider key: {e}"))
    }

    pub fn peer(&self, peer: &PublicKey) -> Option<&PeerState> {
        self.state.peers.get(&peer.to_string())
    }

    fn peer_mut(&mut self, peer: &PublicKey) -> &mut PeerState {
        self.state.peers.entry(peer.to_string()).or_default()
    }

    pub fn position_with(&self, peer: &PublicKey) -> Option<&Position> {
        self.peer(peer).and_then(|p| p.position.as_ref())
    }

    /// Saver: its position with its provider.
    pub fn position(&self) -> Option<&Position> {
        self.provider().ok().and_then(|p| self.peer(&p)).and_then(|p| p.position.as_ref())
    }

    /// Saver: a change of position waiting for the provider's answer.
    pub fn pending(&self) -> Option<&PendingAdjust> {
        self.provider().ok().and_then(|p| self.peer(&p)).and_then(|p| p.pending.as_ref())
    }

    /// True while one of our channel openings has a funding transaction that is neither confirmed
    /// nor listed by our wallet yet. The wallet does not set aside the coins of a funding
    /// transaction it signs, so a second opening now could spend the same coins and never
    /// confirm. (With an Esplora server the wallet lists it as soon as it syncs; with a Bitcoin
    /// Core node, only once it confirms.)
    pub fn opening_in_progress(&self) -> bool {
        let pending: Vec<Option<Txid>> = self
            .node
            .list_channels()
            .iter()
            .filter(|c| c.is_outbound && c.confirmations.unwrap_or(0) == 0)
            .map(|c| c.funding_txo.map(|o| o.txid))
            .collect();
        if pending.is_empty() {
            return false;
        }
        if pending.iter().any(Option::is_none) {
            return true;
        }
        let mut known = Vec::new();
        let mut token = None;
        loop {
            let Ok(page) = self.node.list_payments(token.take()) else { return true };
            known.extend(page.payments.iter().filter_map(|p| match p.kind {
                PaymentKind::Onchain { txid, .. } => Some(txid),
                _ => None,
            }));
            match page.next_page_token {
                Some(next) => token = Some(next),
                None => break,
            }
        }
        pending.iter().flatten().any(|txid| !known.contains(txid))
    }

    /// Connects to another party and opens a channel funded with our own bitcoin. One opening at
    /// a time: until the wallet has seen the previous funding transaction, this refuses. A saver
    /// has one provider at a time: to change, it first leaves the one it has.
    pub fn open_channel(&mut self, peer: PublicKey, address: &str, sats: u64) -> Result<()> {
        self.open_any_channel(peer, address, sats, false)
    }

    /// Provider: opens a channel that the whole network knows about, towards a well-connected
    /// node, so that its savers can pay and be paid beyond its own clients. Needs a node name.
    pub fn open_public_channel(&mut self, peer: PublicKey, address: &str, sats: u64) -> Result<()> {
        if self.cfg.role != Role::Provider {
            bail!("seul un fournisseur ouvre des canaux publics");
        }
        self.open_any_channel(peer, address, sats, true)
    }

    fn open_any_channel(&mut self, peer: PublicKey, address: &str, sats: u64, announced: bool) -> Result<()> {
        if self.opening_in_progress() {
            bail!("une autre ouverture de canal est en cours : réessaie quand sa transaction est partie");
        }
        if self.cfg.role == Role::Saver {
            if let Ok(current) = self.provider() {
                if current != peer && !self.channels_all(&current).is_empty() {
                    bail!("un seul fournisseur à la fois : quitte d'abord celui que tu as");
                }
            }
            if self.peer(&peer).is_some_and(|p| p.closing_since.is_some()) {
                bail!("les anciens canaux avec ce fournisseur finissent de se fermer : réessaie ensuite");
            }
        }
        let addr = SocketAddress::from_str(address).map_err(|_| anyhow!("invalid address {address}"))?;
        self.node.connect(peer, addr.clone(), true).map_err(|e| anyhow!("connecting: {e}"))?;
        if announced {
            self.node.open_announced_channel(peer, addr, sats, None, None, None)
        } else {
            self.node.open_channel(peer, addr, sats, None, None, None)
        }
        .map_err(|e| anyhow!("opening channel: {e}"))?;
        self.peer_mut(&peer);
        if self.cfg.role == Role::Saver {
            self.state.provider = Some(peer.to_string());
        }
        self.save()
    }

    /// Every channel with `peer`, whatever its state: opening, ready, closing.
    pub fn channels_all(&self, peer: &PublicKey) -> Vec<ChannelDetails> {
        self.node.list_channels().into_iter().filter(|c| c.counterparty.node_id == *peer).collect()
    }

    /// Channels with `peer` that are open on the chain, whether or not it can be reached now.
    fn channels_ready(&self, peer: &PublicKey) -> Vec<ChannelDetails> {
        self.channels_all(peer).into_iter().filter(|c| c.is_channel_ready).collect()
    }

    /// Channels with `peer` through which a payment can go right now: open, and `peer` connected.
    pub fn channels_with(&self, peer: &PublicKey) -> Vec<ChannelDetails> {
        self.channels_all(peer).into_iter().filter(|c| c.is_usable).collect()
    }

    /// True when a payment can reach `peer` right now.
    pub fn reachable(&self, peer: &PublicKey) -> bool {
        !self.channels_with(peer).is_empty()
    }

    /// Largest single payment we can send to `peer` now.
    pub fn spendable_to(&self, peer: &PublicKey) -> u64 {
        self.channels_with(peer).iter().map(|c| c.next_outbound_htlc_limit_msat / 1000).max().unwrap_or(0)
    }

    /// Everything `peer` could send us, over all open channels. It does not depend on `peer`
    /// being connected: a provider that is away for a moment has not lost its bitcoin.
    pub fn their_spendable(&self, peer: &PublicKey) -> u64 {
        self.channels_ready(peer).iter().map(|c| c.inbound_capacity_msat / 1000).sum()
    }

    /// What `peer` holds in its channels with us: what the channels are worth, less what is
    /// ours and what is on its way from one side to the other.
    pub fn their_balance(&self, peer: &PublicKey) -> u64 {
        let claimable = self.node.list_balances().lightning_balances;
        self.channels_ready(peer)
            .iter()
            .map(|c| {
                let moving: u64 = c.pending_inbound_htlcs.iter().map(|h| h.amount_msat).sum::<u64>()
                    + c.pending_outbound_htlcs.iter().map(|h| h.amount_msat).sum::<u64>();
                c.channel_value_sats.saturating_sub(our_balance_in(c, &claimable)).saturating_sub(moving.div_ceil(1000))
            })
            .sum()
    }

    /// What we could send to `peer`, over all open channels.
    pub fn our_outbound(&self, peer: &PublicKey) -> u64 {
        self.channels_ready(peer).iter().map(|c| c.outbound_capacity_msat / 1000).sum()
    }

    /// Our balance in the channels with `peer`: what the channels record as ours, before the
    /// on-chain fee of a closing transaction (paid by whoever opened the channel). That fee moves
    /// with the fee market; leaving it out keeps the balance still when nothing was paid, and
    /// gives the same number as `peer` sees of us.
    pub fn balance_with(&self, peer: &PublicKey) -> u64 {
        let claimable = self.node.list_balances().lightning_balances;
        self.channels_all(peer).iter().map(|c| our_balance_in(c, &claimable)).sum()
    }

    /// True while a payment is under way in a channel with `peer`: balances are not settled.
    pub fn payments_under_way(&self, peer: &PublicKey) -> bool {
        self.channels_all(peer).iter().any(|c| !(c.pending_inbound_htlcs.is_empty() && c.pending_outbound_htlcs.is_empty()))
    }

    /// Saver: our balance with our provider.
    pub fn balance(&self) -> u64 {
        self.provider().map(|p| self.balance_with(&p)).unwrap_or(0)
    }

    /// Saver: the part of our balance that is not stable.
    pub fn free_sats(&self) -> u64 {
        self.balance().saturating_sub(self.position().map_or(0, |p| p.backing_sats))
    }

    fn send_msg(&self, peer: PublicKey, amount_msat: u64, body: Body) -> Result<PaymentId> {
        self.node
            .spontaneous_payment()
            .send_with_custom_tlvs(amount_msat, peer, None, vec![self.envelope(peer, body)])
            .map_err(|e| anyhow!("sending payment: {e}"))
    }

    fn envelope(&self, peer: PublicKey, body: Body) -> CustomTlvRecord {
        let envelope =
            Envelope::new(self.node_id().to_string(), peer.to_string(), body, |bytes| self.node.sign_message(bytes));
        CustomTlvRecord { type_num: TLV_TYPE, value: envelope.encode() }
    }

    /// Sends a stability payment. Its identity is chosen and written down before it leaves: if
    /// we stop at any moment, what became of it can be looked up when we are back, and it is
    /// never counted twice nor forgotten.
    fn send_stability(&mut self, peer: PublicKey, sats: u64, body: Body) -> Result<()> {
        let mut secret = [0u8; 32];
        getrandom::getrandom(&mut secret).map_err(|e| anyhow!("no randomness from the system: {e}"))?;
        let preimage = PaymentPreimage(secret);
        let id = PaymentId(PaymentHash::from(preimage).0);
        self.peer_mut(&peer).in_flight = Some(InFlight { payment_id: payment_hex(&id), sats });
        self.save()?;
        let sent = self.node.spontaneous_payment().send_with_preimage_and_custom_tlvs(
            sats * 1000,
            peer,
            vec![self.envelope(peer, body)],
            preimage,
            None,
        );
        if let Err(e) = sent {
            self.peer_mut(&peer).in_flight = None;
            self.save()?;
            bail!("sending payment: {e}");
        }
        Ok(())
    }

    /// What became of a payment of ours, as the node recorded it. `None`: it never left.
    fn payment_status(&self, id_hex: &str) -> Option<(PaymentStatus, Option<u64>)> {
        let mut id = [0u8; 32];
        if id_hex.len() != 64 {
            return None;
        }
        for (i, byte) in id.iter_mut().enumerate() {
            *byte = u8::from_str_radix(id_hex.get(2 * i..2 * i + 2)?, 16).ok()?;
        }
        let details = self.node.payment(&PaymentId(id)).ok()??;
        Some((details.status, details.fee_paid_msat))
    }

    /// A stability payment of ours has arrived.
    fn stability_arrived(&mut self, key: &str, now: i64, report: &mut Report) {
        let role = self.cfg.role;
        let Some(peer) = self.state.peers.get_mut(key) else { return };
        let Some(f) = peer.in_flight.take() else { return };
        if let Some(pos) = peer.position.as_mut() {
            apply_payment(pos, f.sats, role == Role::Provider);
        }
        if peer.position.as_ref().is_some_and(Position::is_empty) {
            peer.position = None;
        }
        peer.last_payment_at = Some(now);
        report.events.push(format!("paiement de stabilité arrivé : {} sats", sats_fr(f.sats)));
    }

    /// A payment to someone else has ended, well or not.
    fn spend_ended(&mut self, paid: bool, fee_msat: Option<u64>, why: &str, report: &mut Report) {
        let Some(spend) = self.state.spend.take() else { return };
        if paid {
            let fee = fee_msat.unwrap_or(0).div_ceil(1000);
            // What the margin for fees did not spend returns to the position.
            self.state.to_stabilize_sats += fee_margin(spend.sats).saturating_sub(fee).min(spend.released_backing);
            report.events.push(format!("paiement réussi : {} sats, frais de routage {} sats", sats_fr(spend.sats), sats_fr(fee)));
        } else {
            self.state.to_stabilize_sats += spend.released_backing;
            report.events.push(format!("paiement échoué ({why}) : les sats retournent dans la position"));
        }
    }

    /// Looks up what became of the payments we are waiting for. Normally an event tells us; this
    /// catches up when an event was lost, or when we stopped between writing a payment down and
    /// sending it.
    fn reconcile_payments(&mut self, now: i64, report: &mut Report) {
        let waiting: Vec<(String, String)> = self
            .state
            .peers
            .iter()
            .filter_map(|(key, p)| Some((key.clone(), p.in_flight.as_ref()?.payment_id.clone())))
            .collect();
        for (key, id) in waiting {
            match self.payment_status(&id) {
                Some((PaymentStatus::Succeeded, _)) => self.stability_arrived(&key, now, report),
                Some((PaymentStatus::Pending, _)) => {}
                Some((PaymentStatus::Failed, _)) | None => {
                    if let Some(p) = self.state.peers.get_mut(&key) {
                        p.in_flight = None;
                    }
                }
            }
        }
        if let Some(id) = self.state.spend.as_ref().and_then(|s| s.payment_id.clone()) {
            match self.payment_status(&id) {
                Some((PaymentStatus::Succeeded, fee)) => self.spend_ended(true, fee, "", report),
                Some((PaymentStatus::Failed, _)) => self.spend_ended(false, None, "sans suite", report),
                Some((PaymentStatus::Pending, _)) => {}
                // Written down but never sent: we stopped in between. It goes now.
                None => {
                    if let Some(spend) = self.state.spend.as_mut() {
                        spend.payment_id = None;
                    }
                }
            }
        }
    }

    fn price(&mut self, quotes: &[Quote], now: i64) -> std::result::Result<PricePoint, String> {
        let mut gold = self.state.gold;
        let result = menger_price(quotes, now, &self.cfg.price_rules, &mut gold).map_err(|e| e.to_string());
        self.state.gold = gold;
        result
    }

    // ----- Leaving -----

    /// Leaves `peer`: closes every channel with it and ends the position, each side keeping
    /// what it holds. Politely when `peer` can be reached and `force` is not asked: that is quick
    /// and cheap. By force otherwise: the bitcoin then comes back after the waiting period of the
    /// channel. A polite close that does not complete is forced later.
    pub fn begin_close(&mut self, peer: &PublicKey, why: &str, force: bool, now: i64) -> Result<()> {
        let mut all_forced = true;
        let mut failure = None;
        for c in self.channels_all(peer) {
            if !force && c.is_usable && self.node.close_channel(&c.user_channel_id, *peer).is_ok() {
                all_forced = false;
            } else if let Err(e) = self.node.force_close_channel(&c.user_channel_id, *peer, Some(why.to_string())) {
                failure = Some(anyhow!("fermeture forcée : {e}"));
                all_forced = false;
            }
        }
        let ours = self.cfg.role == Role::Saver && self.provider().ok() == Some(*peer);
        let p = self.peer_mut(peer);
        p.closed = Some(why.to_string());
        p.closing_since = Some(now);
        p.force_closed = all_forced;
        p.position = None;
        p.pending = None;
        p.in_flight = None;
        p.owed_to_us_since = None;
        p.early_stability_sats = 0;
        if ours {
            // A payment that has not left never will; one that has left ends on its own.
            if self.state.spend.as_ref().is_some_and(|s| s.payment_id.is_none()) {
                self.state.spend = None;
            }
            self.state.to_stabilize_sats = 0;
            self.state.stabilize_not_before = None;
        }
        self.save()?;
        failure.map_or(Ok(()), Err)
    }

    /// Saver: leaves its provider. Without `force`, only if the provider can be reached.
    pub fn leave(&mut self, force: bool, now: i64) -> Result<()> {
        if self.cfg.role != Role::Saver {
            bail!("seul un épargnant quitte son fournisseur");
        }
        let provider = self.provider()?;
        if self.channels_all(&provider).is_empty() {
            bail!("aucun canal ouvert avec le fournisseur");
        }
        if !force && !self.reachable(&provider) {
            bail!("le fournisseur est injoignable : réessaie plus tard, ou force la sortie (plus lente, avec plus de frais)");
        }
        self.begin_close(&provider, "départ demandé", force, now)?;
        self.state.note(now, if force { "sortie forcée demandée" } else { "sortie demandée" });
        self.save()
    }

    /// Sends bitcoin from the on-chain wallet to `address`: `sats`, or everything that is not
    /// kept for the fees of the channels still open. Returns the transaction's id.
    pub fn withdraw(&mut self, address: &str, sats: Option<u64>, now: i64) -> Result<String> {
        let address = Address::from_str(address.trim())
            .map_err(|e| anyhow!("adresse Bitcoin invalide : {e}"))?
            .require_network(self.cfg.network)
            .map_err(|_| anyhow!("cette adresse n'est pas une adresse de ce réseau"))?;
        let onchain = self.node.onchain_payment();
        let txid = match sats {
            Some(0) => bail!("le montant doit être plus grand que zéro"),
            Some(sats) => onchain.send_to_address(&address, sats, None),
            None => onchain.send_all_to_address(&address, true, None),
        }
        .map_err(|e| anyhow!("retrait impossible : {e}"))?;
        let amount = sats.map_or("tout le disponible".to_string(), |s| format!("{} sats", sats_fr(s)));
        self.state.note(now, format!("retrait sur la chaîne : {amount} vers {address} (transaction {txid})"));
        self.save()?;
        Ok(txid.to_string())
    }

    // ----- Saver -----

    /// Saver: adds `sats` of our channel balance to the stable position, or opens it.
    pub fn stabilize(&mut self, sats: u64, quotes: &[Quote], now: i64) -> Result<()> {
        let sats = i64::try_from(sats).ok().filter(|s| *s as u64 <= MAX_SATS).ok_or_else(|| anyhow!("montant invalide"))?;
        self.request_adjust(sats, Purpose::Manual, quotes, now)
    }

    /// Saver: takes `sats` out of the stable position, back to plain bitcoin in the channel.
    /// `None` takes everything out and ends the position.
    pub fn release(&mut self, sats: Option<u64>, quotes: &[Quote], now: i64) -> Result<()> {
        let backing = self.position().map_or(0, |p| p.backing_sats);
        let sats = sats.unwrap_or(backing);
        if sats == 0 {
            bail!("rien à reprendre en bitcoin");
        }
        let sats = i64::try_from(sats).ok().filter(|s| *s as u64 <= MAX_SATS).ok_or_else(|| anyhow!("montant invalide"))?;
        self.request_adjust(-sats, Purpose::Manual, quotes, now)
    }

    /// Saver: asks the provider to grow (positive) or shrink (negative) the position by this many
    /// satoshis. The provider prices the change; we name the worst we accept. Nothing changes
    /// until the provider accepts.
    fn request_adjust(&mut self, delta_backing: i64, purpose: Purpose, quotes: &[Quote], now: i64) -> Result<()> {
        if self.cfg.role != Role::Saver {
            bail!("seul un épargnant change sa position");
        }
        if delta_backing == 0 {
            bail!("le montant doit être plus grand que zéro");
        }
        let provider = self.provider()?;
        let peer = self.peer(&provider).cloned().unwrap_or_default();
        if peer.pending.is_some() {
            bail!("une demande est déjà en cours");
        }
        if peer.closed.is_some() {
            bail!("les canaux avec le fournisseur sont fermés");
        }
        if !self.reachable(&provider) {
            bail!("le fournisseur est injoignable pour l'instant");
        }
        let price = self.price(quotes, now).map_err(|e| anyhow!("pas de prix : {e}"))?;
        let backing = peer.position.as_ref().map_or(0, |p| p.backing_sats);
        let balance = self.balance_with(&provider);
        if delta_backing > 0 && backing.saturating_add(delta_backing as u64) > balance {
            bail!("pas assez de sats dans les canaux : {} détenus, dont {} déjà en or", sats_fr(balance), sats_fr(backing));
        }
        if delta_backing < 0 && delta_backing.unsigned_abs() > backing {
            bail!("le solde en or ne contient que {} sats", sats_fr(backing));
        }
        let worth = price.sats_per_menger.micro_for(delta_backing.unsigned_abs()).min(i64::MAX as u64) as i64;
        let expected_micro = if delta_backing < 0 { -worth } else { worth };
        let limit_micro = adjust_limit(delta_backing, price.sats_per_menger, self.cfg.max_slippage_ppm);
        let rate = peer.position.as_ref().map_or(self.cfg.rate_ppm_per_year, |p| p.rate_ppm_per_year);
        // Each request has its own `at`, later than the one before: that is how the provider tells
        // a new request from one sent again.
        let at = peer.last_request_at.map_or(now, |last| now.max(last.saturating_add(1)));
        let pending = PendingAdjust {
            at,
            delta_backing,
            limit_micro,
            expected_micro,
            rate_ppm_per_year: rate,
            sent_at: now,
            automatic: purpose == Purpose::Automatic,
            for_spend: purpose == Purpose::Spend,
        };
        // Written down before it leaves: if we stop right after sending, the answer still finds
        // its request when we are back.
        let p = self.peer_mut(&provider);
        p.pending = Some(pending.clone());
        p.last_request_at = Some(at);
        self.save()?;
        if let Err(e) = self.send_adjust(provider, &pending) {
            // Nothing left: the request never existed.
            self.peer_mut(&provider).pending = None;
            self.save()?;
            return Err(e);
        }
        self.state.note(
            now,
            format!(
                "demande au fournisseur : {} sats, soit environ {} MNGR à notre prix",
                signed_sats(delta_backing),
                signed_mengers(expected_micro)
            ),
        );
        self.save()
    }

    fn send_adjust(&self, provider: PublicKey, p: &PendingAdjust) -> Result<()> {
        let body = Body::Adjust {
            at: p.at,
            delta_backing: p.delta_backing,
            limit_micro: p.limit_micro,
            rate_ppm_per_year: p.rate_ppm_per_year,
        };
        self.send_msg(provider, MESSAGE_AMOUNT_MSAT, body).map(|_| ())
    }

    /// Creates an invoice for `micro` micro-Mengers, priced in satoshis at our current price.
    /// Returns the invoice and its amount in satoshis.
    pub fn invoice(&mut self, micro: u64, quotes: &[Quote], now: i64) -> Result<(String, u64)> {
        let price = self.price(quotes, now).map_err(|e| anyhow!("pas de prix : {e}"))?;
        let sats = price.sats_per_menger.sats_for(micro);
        if sats == 0 || sats > MAX_SATS {
            bail!("montant invalide");
        }
        let description =
            Description::new(format!("Menger : {} MNGR", format_mengers_fr(micro))).map_err(|e| anyhow!("{e:?}"))?;
        let invoice = self
            .node
            .bolt11_payment()
            .receive(sats * 1000, &Bolt11InvoiceDescription::Direct(description), 3600)
            .map_err(|e| anyhow!("facture : {e}"))?;
        Ok((invoice.to_string(), sats))
    }

    /// Saver: pays a Lightning invoice from our balance, with free satoshis first, then with the
    /// stable position, which the provider must first agree to shrink. The payment goes on a
    /// later tick.
    pub fn pay(&mut self, invoice: &str, quotes: &[Quote], now: i64) -> Result<()> {
        if self.cfg.role != Role::Saver {
            bail!("seul un épargnant paie depuis un solde stable");
        }
        if self.state.spend.is_some() {
            bail!("un paiement est déjà en cours");
        }
        let parsed = Bolt11Invoice::from_str(invoice.trim()).map_err(|e| anyhow!("facture invalide : {e}"))?;
        if parsed.is_expired() {
            bail!("facture expirée");
        }
        if parsed.network() != self.cfg.network {
            bail!("cette facture n'est pas pour ce réseau");
        }
        let msat = parsed.amount_milli_satoshis().ok_or_else(|| anyhow!("la facture n'indique pas de montant"))?;
        let sats = msat.div_ceil(1000);
        if sats == 0 || sats > MAX_SATS {
            bail!("montant invalide");
        }
        let provider = self.provider()?;
        if !self.reachable(&provider) {
            bail!("le fournisseur est injoignable pour l'instant : le paiement passe par lui");
        }
        let backing = self.position_with(&provider).map_or(0, |p| p.backing_sats);
        let balance = self.balance_with(&provider);
        let need = sats + fee_margin(sats);
        let mut release = need.saturating_sub(balance.saturating_sub(backing));
        if release > backing {
            bail!("solde insuffisant : {} sats dans les canaux, {} nécessaires avec la marge pour les frais", sats_fr(balance), sats_fr(need));
        }
        // A channel keeps a small reserve that cannot be sent: better to say so now than to
        // shrink the position for a payment that cannot leave.
        let sendable = self.spendable_to(&provider);
        if need > sendable {
            bail!(
                "le canal ne peut envoyer que {} sats en un paiement (il garde une réserve) : {} nécessaires avec la marge pour les frais",
                sats_fr(sendable),
                sats_fr(need)
            );
        }
        // Rather than leave a few satoshis of position behind, take it all out.
        if release > 0 && backing - release < MIN_ADJUST_SATS {
            release = backing;
        }
        self.state.spend = Some(Spend {
            invoice: invoice.trim().to_string(),
            sats,
            released_backing: release,
            ready: release == 0,
            payment_id: None,
            started_at: now,
        });
        if release > 0 {
            if let Err(e) = self.request_adjust(-(release as i64), Purpose::Spend, quotes, now) {
                self.state.spend = None;
                self.save()?;
                return Err(e);
            }
        }
        self.save()
    }

    /// Saver: gives up a payment that has not left yet. Satoshis already taken out of the
    /// position for it go back in.
    pub fn cancel_spend(&mut self, now: i64) -> Result<()> {
        match &self.state.spend {
            None => bail!("aucun paiement en cours"),
            Some(s) if s.payment_id.is_some() => bail!("le paiement est déjà parti"),
            Some(_) => {}
        }
        self.drop_unsent_spend();
        self.state.note(now, "paiement annulé");
        self.save()
    }

    /// Forgets a payment that has not left. What the position already released for it goes back;
    /// what it may still release (the provider has not answered yet) will go back when it does.
    fn drop_unsent_spend(&mut self) {
        if let Some(spend) = self.state.spend.take() {
            if spend.ready {
                self.state.to_stabilize_sats += spend.released_backing;
            }
        }
    }

    fn saver_chores(&mut self, quotes: &[Quote], now: i64, report: &mut Report) {
        let Ok(provider) = self.provider() else { return };
        let reachable = self.reachable(&provider);
        // A request the provider has not answered goes again.
        if let Some(pending) = self.peer(&provider).and_then(|p| p.pending.clone()) {
            if reachable && now.saturating_sub(pending.sent_at) >= RESEND_SECS {
                match self.send_adjust(provider, &pending) {
                    Ok(()) => report.events.push("demande renvoyée au fournisseur".into()),
                    Err(e) => report.events.push(format!("demande non renvoyée : {e:#}")),
                }
                if let Some(p) = self.peer_mut(&provider).pending.as_mut() {
                    p.sent_at = now;
                }
            }
        }
        if let Some(spend) = self.state.spend.clone() {
            if spend.payment_id.is_none() {
                let invoice = Bolt11Invoice::from_str(&spend.invoice).ok();
                let expired = invoice.as_ref().is_none_or(|i| i.is_expired());
                if expired || (!spend.ready && now.saturating_sub(spend.started_at) >= SPEND_TIMEOUT_SECS) {
                    // The invoice ran out, or the provider did not release the satoshis in time.
                    self.drop_unsent_spend();
                    report.events.push(
                        if expired { "paiement abandonné : la facture a expiré" } else { "paiement abandonné : le fournisseur n'a pas répondu à temps" }
                            .into(),
                    );
                } else if let (true, Some(invoice)) = (spend.ready, invoice) {
                    // An invoice is paid under the name of its hash: written down before it
                    // leaves, so that a payment already gone is never sent or refunded twice.
                    let id = payment_hex(&PaymentId(invoice.payment_hash().0));
                    let already = matches!(self.payment_status(&id), Some((PaymentStatus::Pending | PaymentStatus::Succeeded, _)));
                    self.state.spend = Some(Spend { payment_id: Some(id), ..spend.clone() });
                    if !already {
                        // A payment whose satoshis are free goes now, and never costs more in
                        // fees than what was kept aside for them.
                        let route = RouteParametersConfig {
                            max_total_routing_fee_msat: Some(fee_margin(spend.sats) * 1000),
                            ..RouteParametersConfig::default()
                        };
                        let sent = self.save().and_then(|()| {
                            self.node.bolt11_payment().send(&invoice, Some(route)).map_err(|e| anyhow!("{e}"))
                        });
                        match sent {
                            Ok(_) => report.events.push(format!("paiement de {} sats envoyé", sats_fr(spend.sats))),
                            Err(e) => {
                                report.events.push(format!("paiement impossible : {e:#}"));
                                self.state.spend = Some(spend);
                                self.drop_unsent_spend();
                            }
                        }
                    }
                }
            }
        }
        // Satoshis received, or released and not spent, join the position. The free satoshis are
        // measured once no payment is under way with the provider: during one, the balance is
        // not settled yet.
        let idle =
            self.state.spend.is_none() && self.peer(&provider).is_none_or(|p| p.pending.is_none() && p.closed.is_none());
        let settled = !self.payments_under_way(&provider);
        let due = self.state.stabilize_not_before.is_none_or(|t| now >= t);
        if idle && settled && due && reachable && self.state.to_stabilize_sats > 0 {
            let sats = self.state.to_stabilize_sats.min(self.free_sats());
            self.state.to_stabilize_sats = sats;
            if sats >= MIN_ADJUST_SATS {
                match self.request_adjust(sats as i64, Purpose::Automatic, quotes, now) {
                    Ok(()) => {
                        self.state.to_stabilize_sats = 0;
                        report.events.push(format!("{} sats à garder en or : demande envoyée", sats_fr(sats)));
                    }
                    Err(e) => report.events.push(format!("stabilisation remise à plus tard : {e:#}")),
                }
            }
        }
    }

    // ----- Provider -----

    /// Provider: opens a channel in return for each saver's, one at a time.
    fn provider_chores(&mut self, report: &mut Report) {
        let Some(rule) = self.cfg.return_channel.clone() else { return };
        // What each peer put in private channels towards us, and whether we have one towards it.
        let mut by_peer: BTreeMap<PublicKey, (u64, bool)> = BTreeMap::new();
        for c in self.node.list_channels() {
            let entry = by_peer.entry(c.counterparty.node_id).or_default();
            if c.is_outbound {
                entry.1 = true;
            } else if c.is_channel_ready && !c.is_announced {
                entry.0 += c.channel_value_sats;
            }
        }
        for (peer, (theirs, ours)) in by_peer {
            let known = self.peer(&peer).cloned().unwrap_or_default();
            if ours || theirs < rule.min_sats || known.return_opened || known.closed.is_some() {
                continue;
            }
            if self.opening_in_progress() {
                return;
            }
            let sats = ((theirs as u128 * rule.ratio_ppm as u128 / 1_000_000) as u64).min(rule.max_sats);
            if sats == 0 {
                continue;
            }
            if self.onchain_spendable_sats() < sats + RETURN_CHANNEL_MARGIN_SATS {
                report.events.push(format!(
                    "pas assez de bitcoins sur la chaîne pour ouvrir un canal de {} sats vers l'épargnant {}…",
                    sats_fr(sats),
                    short_id(&peer.to_string())
                ));
                return;
            }
            // The saver called us: we answer on the connection it opened.
            let Some(address) = self.node.list_peers().into_iter().find(|p| p.node_id == peer && p.is_connected).map(|p| p.address)
            else {
                continue;
            };
            match self.open_channel(peer, &address.to_string(), sats) {
                Ok(()) => {
                    self.peer_mut(&peer).return_opened = true;
                    report.events.push(format!(
                        "canal de {} sats ouvert en retour vers l'épargnant {}…",
                        sats_fr(sats),
                        short_id(&peer.to_string())
                    ));
                }
                Err(e) => report.events.push(format!("canal en retour non ouvert : {e:#}")),
            }
            // One opening at a time.
            return;
        }
    }

    // ----- Every tick -----

    /// One step: read events, value every position, act. Errors along the way are reported in
    /// the events; only a failure to save the state stops the tick.
    pub fn tick(&mut self, quotes: &[Quote], now: i64) -> Result<Report> {
        let mut report = Report { at: now, ..Report::default() };
        while let Some(event) = self.node.next_event() {
            if let Err(e) = self.handle_event(event, quotes, now, &mut report) {
                report.events.push(format!("erreur : {e:#}"));
            }
            // Saved before the event is acknowledged, so that no update is lost in a crash. An event
            // replayed after a crash is not counted twice: outgoing payments clear `in_flight`,
            // incoming ones are remembered by hash.
            self.save()?;
            self.node.event_handled().map_err(|e| anyhow!("{e}"))?;
        }
        self.reconcile_payments(now, &mut report);
        let price = self.price(quotes, now);
        match &price {
            Ok(p) => report.price = Some(p.clone()),
            Err(e) => report.price_error = Some(e.clone()),
        }
        match self.cfg.role {
            Role::Saver => self.saver_chores(quotes, now, &mut report),
            Role::Provider => self.provider_chores(&mut report),
        }
        let keys: Vec<String> = self.state.peers.keys().cloned().collect();
        for key in keys {
            if let Err(e) = self.act_for(&key, price.as_ref().ok(), now, &mut report) {
                report.events.push(format!("erreur : {e:#}"));
            }
        }
        for e in &report.events {
            self.state.note(now, e.clone());
        }
        self.save()?;
        Ok(report)
    }

    fn act_for(&mut self, key: &str, price: Option<&PricePoint>, now: i64, report: &mut Report) -> Result<()> {
        let peer_key = PublicKey::from_str(key).map_err(|e| anyhow!("{e}"))?;
        let Some(peer) = self.state.peers.get(key).cloned() else { return Ok(()) };
        let channels = self.channels_all(&peer_key);

        // No channel left with this party: whatever tied us to it is over.
        if channels.is_empty() {
            let ours = self.cfg.role == Role::Saver && self.provider().ok() == Some(peer_key);
            let p = self.peer_mut(&peer_key);
            if p.position.is_some() || p.pending.is_some() {
                p.position = None;
                p.pending = None;
                p.closed.get_or_insert_with(|| "les canaux ont été fermés".to_string());
                report.events.push(format!("position terminée : plus de canal avec {}…", short_id(key)));
                if ours && self.state.spend.as_ref().is_some_and(|s| s.payment_id.is_none()) {
                    self.state.spend = None;
                }
            }
            let p = self.peer_mut(&peer_key);
            if p.closing_since.take().is_some() {
                report.events.push(format!("canaux fermés avec {}…", short_id(key)));
            }
            p.in_flight = None;
            p.owed_to_us_since = None;
            p.unreachable_since = None;
            p.reachable_since = None;
            return Ok(());
        }

        // Leaving: a polite close that drags on is forced.
        if let Some(since) = peer.closing_since {
            if now.saturating_sub(since) >= CLOSE_TIMEOUT_SECS && !peer.force_closed {
                for c in &channels {
                    let _ = self.node.force_close_channel(&c.user_channel_id, peer_key, peer.closed.clone());
                }
                self.peer_mut(&peer_key).force_closed = true;
                report.events.push(format!("fermeture forcée avec {}… : la fermeture à l'amiable n'aboutissait pas", short_id(key)));
            }
            return Ok(());
        }
        // Channels again after an earlier parting: a new start, with nothing carried over but
        // the numbering of the requests.
        let peer = if peer.closed.is_some() {
            let fresh = PeerState { last_request_at: peer.last_request_at, answered: peer.answered, ..PeerState::default() };
            *self.peer_mut(&peer_key) = fresh.clone();
            fresh
        } else {
            peer
        };

        // Can a payment reach this party right now? A channel that is still opening is not an
        // absence: there is nothing to reach yet.
        let online = channels.iter().any(|c| c.is_usable);
        let open = channels.iter().any(|c| c.is_channel_ready);
        let p = self.peer_mut(&peer_key);
        if online {
            if p.unreachable_since.take().is_some() || p.reachable_since.is_none() {
                p.reachable_since = Some(now);
            }
        } else {
            p.reachable_since = None;
            if open {
                p.unreachable_since.get_or_insert(now);
            } else {
                p.unreachable_since = None;
            }
        }
        let online_since = p.reachable_since;

        let Some(position) = peer.position else { return Ok(()) };
        let Some(price) = price else {
            // Without a price nobody can tell who owes what: the grace period starts again once
            // prices are back.
            self.peer_mut(&peer_key).owed_to_us_since = None;
            return Ok(());
        };
        let snapshot = Snapshot {
            role: self.cfg.role,
            position: &position,
            price: price.sats_per_menger,
            now,
            our_spendable_sats: self.spendable_to(&peer_key),
            their_spendable_sats: self.their_spendable(&peer_key),
            last_payment_at: peer.last_payment_at,
            payment_in_flight: peer.in_flight.is_some(),
            owed_to_us_since: peer.owed_to_us_since,
            peer_online: online,
            online_since,
            // Told only when nothing is moving in the channels: a payment on its way belongs to
            // neither side yet.
            their_balance_sats: (self.cfg.role == Role::Provider && !self.payments_under_way(&peer_key))
                .then(|| self.their_balance(&peer_key)),
        };
        let decision = decide(&snapshot, &self.cfg.params);
        report.peers.push(PeerReport {
            peer: key.to_string(),
            decision: decision.clone(),
            micro_mengers: position.micro_mengers(now),
            backing_sats: position.backing_sats,
            online,
        });
        self.peer_mut(&peer_key).owed_to_us_since = match decision.action {
            Action::Await { since, .. } => Some(since),
            _ => None,
        };
        match &decision.action {
            Action::Hold | Action::Await { .. } => {}
            Action::Pay { sats } => {
                let backing_after = if self.cfg.role == Role::Saver {
                    position.backing_sats.saturating_sub(*sats)
                } else {
                    position.backing_sats.saturating_add(*sats)
                };
                self.send_stability(peer_key, *sats, Body::Stability { sats_per_menger: price.sats_per_menger, backing_after })?;
                report.events.push(format!("paiement de stabilité envoyé : {} sats", sats_fr(*sats)));
            }
            Action::Close { reason } => {
                let why = close_reason_text(reason);
                report.events.push(format!("fermeture des canaux : {why}"));
                self.begin_close(&peer_key, &why, false, now)?;
            }
        }
        Ok(())
    }

    fn handle_event(&mut self, event: Event, quotes: &[Quote], now: i64, report: &mut Report) -> Result<()> {
        let role = self.cfg.role;
        match event {
            Event::PaymentSuccessful { payment_id: id, fee_paid_msat, .. } => {
                let id = payment_hex(&id);
                let keys: Vec<String> = self
                    .state
                    .peers
                    .iter()
                    .filter(|(_, p)| p.in_flight.as_ref().is_some_and(|f| f.payment_id == id))
                    .map(|(key, _)| key.clone())
                    .collect();
                for key in keys {
                    self.stability_arrived(&key, now, report);
                }
                if self.state.spend.as_ref().and_then(|s| s.payment_id.as_deref()) == Some(id.as_str()) {
                    self.spend_ended(true, fee_paid_msat, "", report);
                }
            }
            Event::PaymentFailed { payment_id: id, reason, .. } => {
                let id = payment_hex(&id);
                for peer in self.state.peers.values_mut() {
                    if peer.in_flight.as_ref().is_some_and(|f| f.payment_id == id) {
                        peer.in_flight = None;
                        report.events.push(format!("paiement de stabilité échoué ({reason:?}), nouvel essai plus tard"));
                    }
                }
                if self.state.spend.as_ref().and_then(|s| s.payment_id.as_deref()) == Some(id.as_str()) {
                    self.spend_ended(false, None, &format!("{reason:?}"), report);
                }
            }
            Event::PaymentReceived { payment_hash, amount_msat, custom_records, .. } => {
                if !self.state.first_time(&hex(&payment_hash.0)) {
                    return Ok(());
                }
                let record = custom_records.iter().find(|r| r.type_num == TLV_TYPE);
                match (record, record.and_then(|r| Envelope::decode(&r.value))) {
                    (_, Some(e)) => self.handle_message(e, amount_msat, quotes, now, report)?,
                    // Meant as a Menger message, but not one we can read: an earlier or later
                    // version, or noise. Its satoshis stay plain bitcoin; nothing is assumed.
                    (Some(_), None) => report.events.push("message Menger illisible (autre version ?), ignoré".into()),
                    (None, None) => {
                        let sats = amount_msat / 1000;
                        report.events.push(format!("paiement reçu : {} sats", sats_fr(sats)));
                        if role == Role::Saver && self.cfg.auto_stabilize {
                            self.state.to_stabilize_sats = self.state.to_stabilize_sats.saturating_add(sats);
                        }
                    }
                }
            }
            Event::PaymentForwarded { outbound_amount_forwarded_msat, total_fee_earned_msat, .. } => {
                report.events.push(format!(
                    "paiement transmis : {} sats (frais gagnés : {} sats)",
                    sats_fr(outbound_amount_forwarded_msat / 1000),
                    sats_fr(total_fee_earned_msat.unwrap_or(0) / 1000)
                ));
            }
            Event::ChannelReady { counterparty_node_id: Some(peer), .. } => {
                self.peer_mut(&peer);
                if role == Role::Saver && self.state.provider.is_none() {
                    self.state.provider = Some(peer.to_string());
                }
                report.events.push(format!("canal prêt avec {}…", short_id(&peer.to_string())));
            }
            Event::ChannelClosed { reason, counterparty_node_id, .. } => {
                report.events.push(format!("canal fermé avec {}… ({reason:?})", short_id(&counterparty_node_id.to_string())));
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_message(
        &mut self,
        envelope: Envelope,
        amount_msat: u64,
        quotes: &[Quote],
        now: i64,
        report: &mut Report,
    ) -> Result<()> {
        let Ok(from) = PublicKey::from_str(&envelope.from) else {
            report.events.push("message d'un expéditeur illisible, ignoré".into());
            return Ok(());
        };
        if envelope.to != self.node_id().to_string() {
            report.events.push("message destiné à quelqu'un d'autre, ignoré".into());
            return Ok(());
        }
        if !envelope.verify(|bytes, sig| self.node.verify_signature(bytes, sig, &from)) {
            report.events.push("message mal signé, ignoré".into());
            return Ok(());
        }
        let short = short_id(&envelope.from).to_string();
        let role = self.cfg.role;
        match envelope.body {
            Body::Adjust { at, delta_backing, limit_micro, rate_ppm_per_year } => {
                if role != Role::Provider {
                    return Ok(());
                }
                let answer = match self.peer(&from).and_then(|p| p.answered.clone()) {
                    // The same request again: our answer was lost on the way, give it again.
                    Some(a) if a.at == at => a,
                    // An older request, already answered.
                    Some(a) if a.at > at => return Ok(()),
                    _ => {
                        if self.channels_all(&from).is_empty() {
                            report.events.push(format!("demande de {short}…, qui n'a pas de canal avec nous : ignorée"));
                            return Ok(());
                        }
                        let request = AdjustRequest { at, delta_backing, limit_micro, rate_ppm_per_year };
                        let verdict = self.evaluate_adjust(&from, &request, quotes, now);
                        let peer = self.peer_mut(&from);
                        if let Ok(delta_micro) = verdict {
                            peer.position = adjust(peer.position.as_ref(), at, delta_micro, delta_backing, rate_ppm_per_year);
                        }
                        let a = Answer {
                            at,
                            accepted: verdict.is_ok(),
                            delta_micro: *verdict.as_ref().unwrap_or(&0),
                            reason: verdict.err(),
                        };
                        peer.answered = Some(a.clone());
                        report.events.push(match &a.reason {
                            None => format!(
                                "épargnant {short}… : {} MNGR pour {} sats, accepté",
                                signed_mengers(a.delta_micro),
                                signed_sats(delta_backing)
                            ),
                            Some(r) => format!("épargnant {short}… : demande refusée, {r}"),
                        });
                        a
                    }
                };
                // Saved before answering: if the answer is lost, the saver asks again and gets the
                // same one.
                self.save()?;
                self.send_msg(
                    from,
                    MESSAGE_AMOUNT_MSAT,
                    Body::Reply { at, accepted: answer.accepted, delta_micro: answer.delta_micro, reason: answer.reason },
                )?;
            }
            Body::Reply { at, accepted, delta_micro, reason } => {
                if role != Role::Saver || self.provider().ok() != Some(from) {
                    return Ok(());
                }
                let Some(pending) = self.peer(&from).and_then(|p| p.pending.clone()) else { return Ok(()) };
                if pending.at != at {
                    return Ok(());
                }
                let request = AdjustRequest {
                    at,
                    delta_backing: pending.delta_backing,
                    limit_micro: pending.limit_micro,
                    rate_ppm_per_year: pending.rate_ppm_per_year,
                };
                if accepted {
                    if let Err(why) = check_reply(&request, delta_micro) {
                        // The provider did something else than what was asked: its books and ours
                        // no longer agree, and nothing we hold depends on staying.
                        let why = format!("le fournisseur a changé la position hors de ce qui était demandé ({why})");
                        report.events.push(format!("fermeture des canaux : {why}"));
                        return self.begin_close(&from, &why, false, now);
                    }
                }
                let peer = self.peer_mut(&from);
                peer.pending = None;
                let early = std::mem::take(&mut peer.early_stability_sats);
                let released = pending.delta_backing.min(0).unsigned_abs();
                if accepted {
                    peer.position =
                        adjust(peer.position.as_ref(), at, delta_micro, pending.delta_backing, pending.rate_ppm_per_year);
                    if let Some(pos) = peer.position.as_mut() {
                        apply_payment(pos, early, true);
                    }
                    report.events.push(format!(
                        "le fournisseur accepte : {} MNGR pour {} sats (à notre prix : {})",
                        signed_mengers(delta_micro),
                        signed_sats(pending.delta_backing),
                        signed_mengers(pending.expected_micro)
                    ));
                    if pending.for_spend {
                        match self.state.spend.as_mut().filter(|s| !s.ready && s.payment_id.is_none()) {
                            Some(spend) => spend.ready = true,
                            // The payment was given up while we waited: the satoshis go back.
                            None => self.state.to_stabilize_sats += released,
                        }
                    }
                } else {
                    report.events.push(format!("le fournisseur refuse : {}", reason.unwrap_or_default()));
                    if pending.for_spend && self.state.spend.as_ref().is_some_and(|s| !s.ready) {
                        self.state.spend = None;
                        report.events.push("paiement annulé".into());
                    }
                    if pending.automatic && pending.delta_backing > 0 {
                        // Received satoshis are not forgotten: they are offered again later.
                        self.state.to_stabilize_sats += pending.delta_backing as u64;
                        self.state.stabilize_not_before = Some(now + RETRY_REFUSED_SECS);
                    }
                }
            }
            Body::Stability { sats_per_menger, .. } => {
                let sats = amount_msat / 1000;
                let Some(peer) = self.state.peers.get_mut(&from.to_string()) else { return Ok(()) };
                match peer.position.as_mut() {
                    Some(pos) => {
                        // From the other side: the provider pays the saver, or the saver the provider.
                        apply_payment(pos, sats, role == Role::Saver);
                        if pos.is_empty() {
                            peer.position = None;
                        }
                        report.events.push(format!(
                            "paiement de stabilité reçu : {} sats (prix du payeur : {} sats par Menger)",
                            sats_fr(sats),
                            sats_fr(sats_per_menger.0)
                        ));
                    }
                    // The provider opened the position and paid at once; its answer is still on
                    // the way. Counted when it arrives, as the provider counted it.
                    None if role == Role::Saver && peer.pending.is_some() => {
                        peer.early_stability_sats = peer.early_stability_sats.saturating_add(sats);
                        report.events.push(format!("paiement de stabilité reçu avant l'accord : {} sats, comptés à l'ouverture", sats_fr(sats)));
                    }
                    None => report.events.push(format!("paiement de stabilité sans position : {} sats gardés en bitcoin", sats_fr(sats))),
                }
            }
        }
        Ok(())
    }

    /// Provider: prices a requested change of position with our own price, and checks our means.
    /// Returns the change in micro-Mengers.
    fn evaluate_adjust(
        &mut self,
        from: &PublicKey,
        request: &AdjustRequest,
        quotes: &[Quote],
        now: i64,
    ) -> std::result::Result<i64, String> {
        if self.channels_with(from).is_empty() {
            return Err("pas de canal ouvert avec nous".into());
        }
        if self.peer(from).is_some_and(|p| p.closed.is_some()) {
            return Err("les canaux se ferment".into());
        }
        let own = self.price(quotes, now).map_err(|e| format!("pas de prix : {e}"))?;
        let current = self.position_with(from).cloned();
        let delta_micro =
            quote_adjust(current.as_ref(), request, own.sats_per_menger, now, &self.cfg.adjust_rules, &self.cfg.params)?;
        if request.delta_backing > 0 {
            let after = adjust(current.as_ref(), request.at, delta_micro, request.delta_backing, request.rate_ppm_per_year);
            if let Some(p) = after {
                if p.backing_sats > self.their_balance(from) + MESSAGE_TOLERANCE_SATS {
                    return Err("l'épargnant ne détient pas ces sats dans ses canaux".into());
                }
                // Enough of our own bitcoin in the channels to cover a fall of the share we promise.
                let target = own.sats_per_menger.sats_for(p.micro_mengers(now)) as u128;
                let ours = self.our_outbound(from) as u128;
                let coverage = ours * 1_000_000 / (ours + target).max(1);
                if coverage < self.cfg.min_coverage_ppm as u128 {
                    return Err("pas assez de bitcoin de notre côté pour couvrir une baisse".into());
                }
            }
        }
        Ok(delta_micro)
    }
}
