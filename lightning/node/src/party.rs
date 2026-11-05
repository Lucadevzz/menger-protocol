//! One party (saver or provider): a Lightning node plus the Menger rules.
//!
//! A saver has one provider; a provider serves any number of savers. Each call to
//! [`Party::tick`] reads the node's events, values every position with the party's own prices and
//! does what the rules say: pay, wait, or leave. A saver can also pay anyone on Lightning from its
//! balance, through its provider, and add what it receives to its stable balance.

use crate::state::{Answer, InFlight, PeerState, PendingAdjust, Spend, State};
use anyhow::{anyhow, bail, Context, Result};
use ldk_node::bitcoin::secp256k1::PublicKey;
use ldk_node::bitcoin::{Network, Txid};
use ldk_node::lightning::ln::channelmanager::PaymentId;
use ldk_node::lightning::ln::msgs::SocketAddress;
use ldk_node::lightning_invoice::{Bolt11Invoice, Bolt11InvoiceDescription, Description};
use ldk_node::liquidity::LSPS2ServiceConfig;
use ldk_node::payment::PaymentKind;
use ldk_node::{Builder, ChannelDetails, CustomTlvRecord, Event, LightningBalance, Node};
use menger_core::price::{menger_price, PricePoint, PriceRules, Quote};
use menger_core::protocol::{Body, Envelope, MESSAGE_AMOUNT_MSAT, TLV_TYPE};
use menger_core::stability::{
    adjust, apply_payment, check_adjust, decide, Action, AdjustRequest, AdjustRules, CloseReason, Decision, Params,
    Position, Role, Snapshot,
};
use menger_core::unit::format_mengers;
use serde::Serialize;
use std::path::PathBuf;
use std::str::FromStr;

/// Saver: a request the provider has not answered is sent again after this many seconds.
pub const RESEND_SECS: i64 = 60;
/// Saver: smaller amounts are not worth a change of position.
pub const MIN_ADJUST_SATS: u64 = 1_000;
/// Provider: how far a saver's balance may fall short of the backing it claims, for the satoshi
/// that each message carries.
const MESSAGE_TOLERANCE_SATS: u64 = 10;

/// What a payment to someone else keeps aside for routing fees. What it does not spend returns
/// to the position.
fn fee_margin(sats: u64) -> u64 {
    sats / 200 + 10
}

#[derive(Clone, Debug)]
pub enum ChainSource {
    BitcoindRpc { host: String, port: u16, user: String, password: String },
    Esplora { url: String },
}

#[derive(Clone, Debug)]
pub struct PartyConfig {
    pub role: Role,
    pub data_dir: PathBuf,
    pub network: Network,
    /// Address the Lightning node listens on, e.g. `127.0.0.1:9735`.
    pub listen: String,
    pub chain: ChainSource,
    pub params: Params,
    pub price_rules: PriceRules,
    /// Saver: its provider's node id and address (`host:port`).
    pub provider: Option<(PublicKey, String)>,
    /// Saver: price of stability it asks for when opening a position.
    pub rate_ppm_per_year: i64,
    /// Provider: what it accepts when a saver changes its position.
    pub adjust_rules: AdjustRules,
    /// Provider: share of a bitcoin fall against gold it must be able to cover to accept more.
    pub min_coverage_ppm: u64,
    /// Saver: add the satoshis it receives to its stable balance.
    pub auto_stabilize: bool,
}

impl PartyConfig {
    pub fn new(role: Role, data_dir: PathBuf, network: Network, listen: String, chain: ChainSource) -> Self {
        PartyConfig {
            role,
            data_dir,
            network,
            listen,
            chain,
            params: Params::default(),
            price_rules: PriceRules::default(),
            provider: None,
            rate_ppm_per_year: 0,
            adjust_rules: AdjustRules::default(),
            min_coverage_ppm: 300_000,
            auto_stabilize: true,
        }
    }
}

/// One position, as valued during a tick.
#[derive(Clone, Debug, Serialize)]
pub struct PeerReport {
    pub peer: String,
    pub decision: Decision,
    pub micro_mengers: u64,
    pub backing_sats: u64,
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
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn payment_hex(id: &PaymentId) -> String {
    hex(&id.0)
}

fn signed_mengers(micro: i64) -> String {
    format!("{}{}", if micro < 0 { "-" } else { "+" }, format_mengers(micro.unsigned_abs()))
}

/// Why a party leaves, in words.
pub fn close_reason_text(reason: &CloseReason) -> String {
    match reason {
        CloseReason::Unpaid { owed_sats } => format!("l'autre partie ne paie pas les {owed_sats} sats qu'elle doit"),
        CloseReason::ProviderCannotCover { coverage_ppm } => format!(
            "le fournisseur ne couvre plus qu'une baisse de {} % du bitcoin face à l'or",
            coverage_ppm / 10_000
        ),
    }
}

/// The first characters of a node id, to tell savers apart in messages.
pub fn short_id(id: &str) -> &str {
    &id[..8.min(id.len())]
}

impl Party {
    /// Starts the Lightning node and loads the saved state.
    pub fn start(cfg: PartyConfig) -> Result<Party> {
        std::fs::create_dir_all(&cfg.data_dir).with_context(|| format!("creating {}", cfg.data_dir.display()))?;
        let mut builder = Builder::new();
        builder.set_network(cfg.network);
        builder.set_storage_dir_path(cfg.data_dir.join("ldk").to_string_lossy().into_owned());
        match &cfg.chain {
            ChainSource::BitcoindRpc { host, port, user, password } => {
                builder.set_chain_source_bitcoind_rpc(host.clone(), *port, user.clone(), password.clone());
            }
            ChainSource::Esplora { url } => {
                builder.set_chain_source_esplora(url.clone(), None);
            }
        }
        let listen = SocketAddress::from_str(&cfg.listen).map_err(|_| anyhow!("invalid listen address {}", cfg.listen))?;
        builder.set_listening_addresses(vec![listen]).map_err(|e| anyhow!("{e}"))?;
        if cfg.role == Role::Provider {
            // The provider is its savers' Lightning service provider (LSPS2, bLIP-52). With LDK
            // Node, this is what lets it forward payments to their private channels and accept
            // payments as large as the channels. Its just-in-time channels are not used by Menger
            // yet; should a client ask for one, it pays for the opening.
            //
            // Savers are deliberately not LSPS2 clients: that would make them accept the
            // provider's channels before they confirm, and so trust it not to double-spend them.
            builder.set_liquidity_provider_lsps2(LSPS2ServiceConfig {
                require_token: None,
                advertise_service: false,
                channel_opening_fee_ppm: 10_000,
                channel_over_provisioning_ppm: 0,
                min_channel_opening_fee_msat: 2_000_000,
                min_channel_lifetime: 1_008,
                max_client_to_self_delay: 2_016,
                min_payment_size_msat: 10_000_000,
                max_payment_size_msat: 100_000_000_000,
                client_trusts_lsp: false,
            });
        }
        let node = builder.build().map_err(|e| anyhow!("building the Lightning node: {e}"))?;
        node.start().map_err(|e| anyhow!("starting the Lightning node: {e}"))?;
        let state_path = cfg.data_dir.join("menger-state.json");
        let mut state = State::load(&state_path)?;
        if let (Role::Saver, Some((key, _)), None) = (cfg.role, &cfg.provider, &state.provider) {
            state.provider = Some(key.to_string());
        }
        Ok(Party { cfg, node, state, state_path })
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
        let known: Vec<Txid> = self
            .node
            .list_payments_with_filter(|p| matches!(p.kind, PaymentKind::Onchain { .. }))
            .into_iter()
            .filter_map(|p| match p.kind {
                PaymentKind::Onchain { txid, .. } => Some(txid),
                _ => None,
            })
            .collect();
        self.node.list_channels().iter().any(|c| {
            c.is_outbound && c.confirmations.unwrap_or(0) == 0 && c.funding_txo.is_none_or(|o| !known.contains(&o.txid))
        })
    }

    /// Connects to another party and opens a channel funded with our own bitcoin. One opening at
    /// a time: until the wallet has seen the previous funding transaction, this refuses.
    pub fn open_channel(&mut self, peer: PublicKey, address: &str, sats: u64) -> Result<()> {
        if self.opening_in_progress() {
            bail!("une autre ouverture de canal est en cours : réessaie quand sa transaction est partie");
        }
        let addr = SocketAddress::from_str(address).map_err(|_| anyhow!("invalid address {address}"))?;
        self.node.connect(peer, addr.clone(), true).map_err(|e| anyhow!("connecting: {e}"))?;
        self.node.open_channel(peer, addr, sats, None, None).map_err(|e| anyhow!("opening channel: {e}"))?;
        self.peer_mut(&peer);
        if self.cfg.role == Role::Saver && self.state.provider.is_none() {
            self.state.provider = Some(peer.to_string());
        }
        self.save()
    }

    /// Usable channels with `peer`.
    pub fn channels_with(&self, peer: &PublicKey) -> Vec<ChannelDetails> {
        self.node.list_channels().into_iter().filter(|c| c.counterparty_node_id == *peer && c.is_usable).collect()
    }

    /// Largest single payment we can send to `peer` now.
    pub fn spendable_to(&self, peer: &PublicKey) -> u64 {
        self.channels_with(peer).iter().map(|c| c.next_outbound_htlc_limit_msat / 1000).max().unwrap_or(0)
    }

    /// Everything `peer` can send us, over all channels.
    pub fn their_spendable(&self, peer: &PublicKey) -> u64 {
        self.channels_with(peer).iter().map(|c| c.inbound_capacity_msat / 1000).sum()
    }

    /// What `peer` holds in its channels with us, reserves included.
    pub fn their_balance(&self, peer: &PublicKey) -> u64 {
        self.channels_with(peer)
            .iter()
            .map(|c| c.inbound_capacity_msat / 1000 + c.counterparty_unspendable_punishment_reserve)
            .sum()
    }

    /// What we can send to `peer`, over all channels.
    pub fn our_outbound(&self, peer: &PublicKey) -> u64 {
        self.channels_with(peer).iter().map(|c| c.outbound_capacity_msat / 1000).sum()
    }

    /// Our balance in the channels with `peer`: what we would get if they closed now.
    pub fn balance_with(&self, peer: &PublicKey) -> u64 {
        self.node
            .list_balances()
            .lightning_balances
            .iter()
            .map(|b| match b {
                LightningBalance::ClaimableOnChannelClose { counterparty_node_id, amount_satoshis, .. }
                    if counterparty_node_id == peer =>
                {
                    *amount_satoshis
                }
                _ => 0,
            })
            .sum()
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
        let envelope = Envelope::new(self.node_id().to_string(), body, |bytes| self.node.sign_message(bytes));
        let tlv = CustomTlvRecord { type_num: TLV_TYPE, value: envelope.encode() };
        self.node
            .spontaneous_payment()
            .send_with_custom_tlvs(amount_msat, peer, None, vec![tlv])
            .map_err(|e| anyhow!("sending payment: {e}"))
    }

    fn price(&mut self, quotes: &[Quote], now: i64) -> std::result::Result<PricePoint, String> {
        let mut gold = self.state.gold;
        let result = menger_price(quotes, now, &self.cfg.price_rules, &mut gold).map_err(|e| e.to_string());
        self.state.gold = gold;
        result
    }

    // ----- Saver -----

    /// Saver: adds `sats` of our channel balance to the stable position, or opens it.
    pub fn stabilize(&mut self, sats: u64, quotes: &[Quote], now: i64) -> Result<()> {
        self.request_adjust(sats as i64, quotes, now)
    }

    /// Saver: asks the provider to grow (positive) or shrink (negative) the position by this many
    /// satoshis, valued at our own price. Nothing changes until the provider accepts.
    fn request_adjust(&mut self, delta_backing: i64, quotes: &[Quote], now: i64) -> Result<()> {
        if self.cfg.role != Role::Saver {
            bail!("seul un épargnant change sa position");
        }
        let provider = self.provider()?;
        let peer = self.peer(&provider).cloned().unwrap_or_default();
        if peer.pending.is_some() {
            bail!("une demande est déjà en cours");
        }
        if peer.closed.is_some() {
            bail!("les canaux avec le fournisseur sont fermés");
        }
        let price = self.price(quotes, now).map_err(|e| anyhow!("pas de prix : {e}"))?;
        let backing = peer.position.as_ref().map_or(0, |p| p.backing_sats);
        let balance = self.balance_with(&provider);
        if delta_backing > 0 && backing + delta_backing as u64 > balance {
            bail!("pas assez de sats dans les canaux : {balance} détenus, dont {backing} déjà stables");
        }
        if delta_backing < 0 && delta_backing.unsigned_abs() > backing {
            bail!("la position ne contient que {backing} sats");
        }
        let micro = price.sats_per_menger.micro_for(delta_backing.unsigned_abs()) as i64;
        let delta_micro = if delta_backing < 0 { -micro } else { micro };
        let rate = peer.position.as_ref().map_or(self.cfg.rate_ppm_per_year, |p| p.rate_ppm_per_year);
        // Each request has its own `at`, later than the one before: that is how the provider tells
        // a new request from one sent again.
        let at = peer.last_request_at.map_or(now, |last| now.max(last + 1));
        let pending = PendingAdjust {
            at,
            delta_micro,
            delta_backing,
            rate_ppm_per_year: rate,
            sats_per_menger: price.sats_per_menger,
            sent_at: now,
        };
        self.send_adjust(provider, &pending)?;
        let peer = self.peer_mut(&provider);
        peer.pending = Some(pending);
        peer.last_request_at = Some(at);
        self.state.note(now, format!("demande au fournisseur : {} MNGR ({delta_backing:+} sats)", signed_mengers(delta_micro)));
        self.save()
    }

    fn send_adjust(&self, provider: PublicKey, p: &PendingAdjust) -> Result<()> {
        let body = Body::Adjust {
            at: p.at,
            delta_micro: p.delta_micro,
            delta_backing: p.delta_backing,
            rate_ppm_per_year: p.rate_ppm_per_year,
            sats_per_menger: p.sats_per_menger,
        };
        self.send_msg(provider, MESSAGE_AMOUNT_MSAT, body).map(|_| ())
    }

    /// Creates an invoice for `micro` micro-Mengers, priced in satoshis at our current price.
    /// Returns the invoice and its amount in satoshis.
    pub fn invoice(&mut self, micro: u64, quotes: &[Quote], now: i64) -> Result<(String, u64)> {
        let price = self.price(quotes, now).map_err(|e| anyhow!("pas de prix : {e}"))?;
        let sats = price.sats_per_menger.sats_for(micro);
        let description =
            Description::new(format!("Menger : {} MNGR", format_mengers(micro))).map_err(|e| anyhow!("{e:?}"))?;
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
        let msat = parsed.amount_milli_satoshis().ok_or_else(|| anyhow!("la facture n'indique pas de montant"))?;
        let sats = msat.div_ceil(1000);
        let provider = self.provider()?;
        let backing = self.position_with(&provider).map_or(0, |p| p.backing_sats);
        let balance = self.balance_with(&provider);
        let need = sats + fee_margin(sats);
        let release = need.saturating_sub(balance.saturating_sub(backing));
        if release > backing {
            bail!("solde insuffisant : {balance} sats dans les canaux, {need} nécessaires");
        }
        self.state.spend = Some(Spend {
            invoice: invoice.trim().to_string(),
            sats,
            released_backing: release,
            ready: release == 0,
            payment_id: None,
        });
        if release > 0 {
            if let Err(e) = self.request_adjust(-(release as i64), quotes, now) {
                self.state.spend = None;
                self.save()?;
                return Err(e);
            }
        }
        self.save()
    }

    fn saver_chores(&mut self, quotes: &[Quote], now: i64, report: &mut Report) {
        let Ok(provider) = self.provider() else { return };
        // A request the provider has not answered goes again.
        if let Some(pending) = self.peer(&provider).and_then(|p| p.pending.clone()) {
            if now - pending.sent_at >= RESEND_SECS {
                match self.send_adjust(provider, &pending) {
                    Ok(()) => report.events.push("demande renvoyée au fournisseur".into()),
                    Err(e) => report.events.push(format!("demande non renvoyée : {e:#}")),
                }
                if let Some(p) = self.peer_mut(&provider).pending.as_mut() {
                    p.sent_at = now;
                }
            }
        }
        // A payment whose satoshis are free goes now.
        if let Some(spend) = self.state.spend.clone() {
            if spend.ready && spend.payment_id.is_none() {
                let sent = Bolt11Invoice::from_str(&spend.invoice)
                    .map_err(|e| anyhow!("{e}"))
                    .and_then(|invoice| self.node.bolt11_payment().send(&invoice, None).map_err(|e| anyhow!("{e}")));
                match sent {
                    Ok(id) => {
                        report.events.push(format!("paiement de {} sats envoyé", spend.sats));
                        self.state.spend = Some(Spend { payment_id: Some(payment_hex(&id)), ..spend });
                    }
                    Err(e) => {
                        report.events.push(format!("paiement impossible : {e:#}"));
                        self.state.to_stabilize_sats += spend.released_backing;
                        self.state.spend = None;
                    }
                }
            }
        }
        // Satoshis received, or released and not spent, join the position.
        let idle =
            self.state.spend.is_none() && self.peer(&provider).is_none_or(|p| p.pending.is_none() && p.closed.is_none());
        if idle && self.state.to_stabilize_sats > 0 {
            let sats = self.state.to_stabilize_sats.min(self.free_sats());
            self.state.to_stabilize_sats = sats;
            if sats >= MIN_ADJUST_SATS {
                match self.request_adjust(sats as i64, quotes, now) {
                    Ok(()) => {
                        self.state.to_stabilize_sats = 0;
                        report.events.push(format!("{sats} sats à ajouter à la position : demande envoyée"));
                    }
                    Err(e) => report.events.push(format!("stabilisation remise à plus tard : {e:#}")),
                }
            }
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
        let price = self.price(quotes, now);
        match &price {
            Ok(p) => report.price = Some(p.clone()),
            Err(e) => report.price_error = Some(e.clone()),
        }
        if self.cfg.role == Role::Saver {
            self.saver_chores(quotes, now, &mut report);
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
        if peer.closed.is_some() {
            return Ok(());
        }
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
        };
        let decision = decide(&snapshot, &self.cfg.params);
        report.peers.push(PeerReport {
            peer: key.to_string(),
            decision: decision.clone(),
            micro_mengers: position.micro_mengers(now),
            backing_sats: position.backing_sats,
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
                let id = self.send_msg(
                    peer_key,
                    sats * 1000,
                    Body::Stability { sats_per_menger: price.sats_per_menger, backing_after },
                )?;
                self.peer_mut(&peer_key).in_flight = Some(InFlight { payment_id: payment_hex(&id), sats: *sats });
                report.events.push(format!("paiement de stabilité envoyé : {sats} sats"));
            }
            Action::Close { reason } => {
                let why = close_reason_text(reason);
                self.close_with(&peer_key, &why)?;
                report.events.push(format!("fermeture des canaux : {why}"));
                self.peer_mut(&peer_key).closed = Some(why);
            }
        }
        Ok(())
    }

    /// Closes every channel with `peer`, cooperatively if possible.
    pub fn close_with(&mut self, peer: &PublicKey, reason: &str) -> Result<()> {
        for c in self.node.list_channels().into_iter().filter(|c| c.counterparty_node_id == *peer) {
            if self.node.close_channel(&c.user_channel_id, *peer).is_err() {
                self.node
                    .force_close_channel(&c.user_channel_id, *peer, Some(reason.to_string()))
                    .map_err(|e| anyhow!("force-closing: {e}"))?;
            }
        }
        Ok(())
    }

    fn handle_event(&mut self, event: Event, quotes: &[Quote], now: i64, report: &mut Report) -> Result<()> {
        let role = self.cfg.role;
        match event {
            Event::PaymentSuccessful { payment_id: Some(id), fee_paid_msat, .. } => {
                let id = payment_hex(&id);
                for peer in self.state.peers.values_mut() {
                    if peer.in_flight.as_ref().is_some_and(|f| f.payment_id == id) {
                        let f = peer.in_flight.take().expect("checked");
                        if let Some(pos) = peer.position.as_mut() {
                            apply_payment(pos, f.sats, role == Role::Provider);
                        }
                        peer.last_payment_at = Some(now);
                        report.events.push(format!("paiement de stabilité arrivé : {} sats", f.sats));
                    }
                }
                if self.state.spend.as_ref().and_then(|s| s.payment_id.as_deref()) == Some(id.as_str()) {
                    let spend = self.state.spend.take().expect("checked");
                    let fee = fee_paid_msat.unwrap_or(0).div_ceil(1000);
                    // What the margin for fees did not spend returns to the position.
                    self.state.to_stabilize_sats += fee_margin(spend.sats).saturating_sub(fee).min(spend.released_backing);
                    report.events.push(format!("paiement réussi : {} sats, frais de routage {fee} sats", spend.sats));
                }
            }
            Event::PaymentFailed { payment_id: Some(id), reason, .. } => {
                let id = payment_hex(&id);
                for peer in self.state.peers.values_mut() {
                    if peer.in_flight.as_ref().is_some_and(|f| f.payment_id == id) {
                        peer.in_flight = None;
                        report.events.push(format!("paiement de stabilité échoué ({reason:?}), nouvel essai plus tard"));
                    }
                }
                if self.state.spend.as_ref().and_then(|s| s.payment_id.as_deref()) == Some(id.as_str()) {
                    let spend = self.state.spend.take().expect("checked");
                    self.state.to_stabilize_sats += spend.released_backing;
                    report.events.push(format!("paiement échoué ({reason:?}) : les sats retournent dans la position"));
                }
            }
            Event::PaymentReceived { payment_hash, amount_msat, custom_records, .. } => {
                if !self.state.first_time(&hex(&payment_hash.0)) {
                    return Ok(());
                }
                let envelope =
                    custom_records.iter().find(|r| r.type_num == TLV_TYPE).and_then(|r| Envelope::decode(&r.value));
                match envelope {
                    Some(e) => self.handle_message(e, amount_msat, quotes, now, report)?,
                    None => {
                        let sats = amount_msat / 1000;
                        report.events.push(format!("paiement reçu : {sats} sats"));
                        if role == Role::Saver && self.cfg.auto_stabilize {
                            self.state.to_stabilize_sats += sats;
                        }
                    }
                }
            }
            Event::PaymentForwarded { outbound_amount_forwarded_msat, total_fee_earned_msat, .. } => {
                report.events.push(format!(
                    "paiement transmis : {} sats (frais gagnés : {} sats)",
                    outbound_amount_forwarded_msat.unwrap_or(0) / 1000,
                    total_fee_earned_msat.unwrap_or(0) / 1000
                ));
            }
            Event::ChannelReady { counterparty_node_id: Some(peer), .. } => {
                self.peer_mut(&peer);
                if role == Role::Saver && self.state.provider.is_none() {
                    self.state.provider = Some(peer.to_string());
                }
                report.events.push(format!("canal prêt avec {}…", short_id(&peer.to_string())));
            }
            Event::ChannelClosed { reason, .. } => {
                report.events.push(format!("canal fermé ({reason:?})"));
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
        if !envelope.verify(|bytes, sig| self.node.verify_signature(bytes, sig, &from)) {
            report.events.push("message mal signé, ignoré".into());
            return Ok(());
        }
        let short = short_id(&envelope.from).to_string();
        let role = self.cfg.role;
        match envelope.body {
            Body::Adjust { at, delta_micro, delta_backing, rate_ppm_per_year, sats_per_menger } => {
                if role != Role::Provider {
                    return Ok(());
                }
                let answer = match self.peer(&from).and_then(|p| p.answered.clone()) {
                    // The same request again: our answer was lost on the way, give it again.
                    Some(a) if a.at == at => a,
                    // An older request, already answered.
                    Some(a) if a.at > at => return Ok(()),
                    _ => {
                        if self.channels_with(&from).is_empty() && self.peer(&from).is_none() {
                            report.events.push(format!("demande de {short}…, qui n'a pas de canal avec nous : ignorée"));
                            return Ok(());
                        }
                        let request =
                            AdjustRequest { at, delta_micro, delta_backing, rate_ppm_per_year, their_price: sats_per_menger };
                        let verdict = self.evaluate_adjust(&from, &request, quotes, now);
                        let peer = self.peer_mut(&from);
                        if verdict.is_ok() {
                            peer.position = adjust(peer.position.as_ref(), at, delta_micro, delta_backing, rate_ppm_per_year);
                        }
                        let a = Answer { at, accepted: verdict.is_ok(), reason: verdict.err() };
                        peer.answered = Some(a.clone());
                        report.events.push(match &a.reason {
                            None => format!("épargnant {short}… : {} MNGR, accepté", signed_mengers(delta_micro)),
                            Some(r) => format!("épargnant {short}… : demande refusée, {r}"),
                        });
                        a
                    }
                };
                // Saved before answering: if the answer is lost, the saver asks again and gets the
                // same one.
                self.save()?;
                self.send_msg(from, MESSAGE_AMOUNT_MSAT, Body::Reply { at, accepted: answer.accepted, reason: answer.reason })?;
            }
            Body::Reply { at, accepted, reason } => {
                if role != Role::Saver || self.provider().ok() != Some(from) {
                    return Ok(());
                }
                let Some(pending) = self.peer(&from).and_then(|p| p.pending.clone()) else { return Ok(()) };
                if pending.at != at {
                    return Ok(());
                }
                let peer = self.peer_mut(&from);
                peer.pending = None;
                let early = std::mem::take(&mut peer.early_stability_sats);
                if accepted {
                    peer.position =
                        adjust(peer.position.as_ref(), at, pending.delta_micro, pending.delta_backing, pending.rate_ppm_per_year);
                    if let Some(pos) = peer.position.as_mut() {
                        apply_payment(pos, early, true);
                    }
                    report.events.push(format!("le fournisseur accepte : {} MNGR", signed_mengers(pending.delta_micro)));
                    if pending.delta_backing < 0 {
                        if let Some(spend) = self.state.spend.as_mut() {
                            spend.ready = true;
                        }
                    }
                } else {
                    report.events.push(format!("le fournisseur refuse : {}", reason.unwrap_or_default()));
                    if pending.delta_backing < 0 && self.state.spend.as_ref().is_some_and(|s| !s.ready) {
                        self.state.spend = None;
                        report.events.push("paiement annulé".into());
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
                        report.events.push(format!(
                            "paiement de stabilité reçu : {sats} sats (prix du payeur : {} sats par Menger)",
                            sats_per_menger.0
                        ));
                    }
                    // The provider opened the position and paid at once; its answer is still on
                    // the way. Counted when it arrives, as the provider counted it.
                    None if role == Role::Saver && peer.pending.is_some() => {
                        peer.early_stability_sats += sats;
                        report.events.push(format!("paiement de stabilité reçu avant l'accord : {sats} sats, comptés à l'ouverture"));
                    }
                    None => report.events.push(format!("paiement de stabilité sans position : {sats} sats gardés en bitcoin")),
                }
            }
        }
        Ok(())
    }

    /// Provider: checks a requested change of position against our own price and means.
    fn evaluate_adjust(
        &mut self,
        from: &PublicKey,
        request: &AdjustRequest,
        quotes: &[Quote],
        now: i64,
    ) -> std::result::Result<(), String> {
        if self.channels_with(from).is_empty() {
            return Err("pas de canal ouvert avec nous".into());
        }
        let own = self.price(quotes, now).map_err(|e| format!("pas de prix : {e}"))?;
        let current = self.position_with(from).cloned();
        check_adjust(current.as_ref(), request, own.sats_per_menger, now, &self.cfg.adjust_rules)?;
        if request.delta_backing > 0 {
            let after = adjust(
                current.as_ref(),
                request.at,
                request.delta_micro,
                request.delta_backing,
                request.rate_ppm_per_year,
            );
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
        Ok(())
    }
}
