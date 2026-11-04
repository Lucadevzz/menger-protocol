//! One party (saver or provider): a Lightning node plus the Menger rules.
//!
//! Each call to [`Party::tick`] reads the node's events, updates the position, values it with the
//! party's own prices and does what the rules say: pay, wait, or leave.

use crate::state::{InFlight, State};
use anyhow::{anyhow, bail, Context, Result};
use ldk_node::bitcoin::secp256k1::PublicKey;
use ldk_node::bitcoin::Network;
use ldk_node::lightning::ln::channelmanager::PaymentId;
use ldk_node::lightning::ln::msgs::SocketAddress;
use ldk_node::{Builder, ChannelDetails, CustomTlvRecord, Event, Node};
use menger_core::price::{menger_price, PricePoint, PriceRules, Quote};
use menger_core::protocol::{Body, Message, MESSAGE_AMOUNT_MSAT, TLV_TYPE};
use menger_core::stability::{apply_payment, decide, Action, Decision, Params, Position, Role, Snapshot};
use menger_core::unit::format_mengers;
use serde::Serialize;
use std::path::PathBuf;
use std::str::FromStr;

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
    /// Saver: price of stability it proposes. Provider: lowest price it accepts.
    pub rate_ppm_per_year: i64,
    /// Provider: largest gap between the saver's price and its own when accepting a position.
    pub max_price_gap_ppm: u64,
    /// Provider: share of a bitcoin fall against gold it must be able to cover to accept.
    pub min_coverage_ppm: u64,
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
            rate_ppm_per_year: 0,
            max_price_gap_ppm: 10_000,
            min_coverage_ppm: 300_000,
        }
    }
}

/// What happened during one tick, for display and tests.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Report {
    pub at: i64,
    pub price: Option<PricePoint>,
    pub price_error: Option<String>,
    pub decision: Option<Decision>,
    pub micro_mengers: Option<u64>,
    pub backing_sats: Option<u64>,
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
        let node = builder.build().map_err(|e| anyhow!("building the Lightning node: {e}"))?;
        node.start().map_err(|e| anyhow!("starting the Lightning node: {e}"))?;
        let state_path = cfg.data_dir.join("menger-state.json");
        let state = State::load(&state_path)?;
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

    pub fn counterparty(&self) -> Result<PublicKey> {
        let hex = self.state.counterparty.as_deref().ok_or_else(|| anyhow!("no counterparty yet"))?;
        PublicKey::from_str(hex).map_err(|e| anyhow!("bad counterparty key: {e}"))
    }

    pub fn set_counterparty(&mut self, peer: PublicKey) -> Result<()> {
        self.state.counterparty = Some(peer.to_string());
        self.save()
    }

    /// Connects to the other party and opens a channel funded with our own bitcoin.
    pub fn open_channel(&mut self, peer: PublicKey, address: &str, sats: u64) -> Result<()> {
        let addr = SocketAddress::from_str(address).map_err(|_| anyhow!("invalid address {address}"))?;
        self.node.connect(peer, addr.clone(), true).map_err(|e| anyhow!("connecting: {e}"))?;
        self.node.open_channel(peer, addr, sats, None, None).map_err(|e| anyhow!("opening channel: {e}"))?;
        self.set_counterparty(peer)
    }

    /// Usable channels with the other party.
    pub fn channels(&self) -> Vec<ChannelDetails> {
        let Ok(peer) = self.counterparty() else { return Vec::new() };
        self.node
            .list_channels()
            .into_iter()
            .filter(|c| c.counterparty_node_id == peer && c.is_usable)
            .collect()
    }

    /// Largest single payment we can send to the other party now.
    pub fn our_spendable_sats(&self) -> u64 {
        self.channels().iter().map(|c| c.next_outbound_htlc_limit_msat / 1000).max().unwrap_or(0)
    }

    /// Everything the other party can send us, over all channels.
    pub fn their_spendable_sats(&self) -> u64 {
        self.channels().iter().map(|c| c.inbound_capacity_msat / 1000).sum()
    }

    /// What we can send, over all channels with the other party (our balance minus the reserves
    /// the channels require us to keep).
    pub fn our_channel_sats(&self) -> u64 {
        self.channels().iter().map(|c| c.outbound_capacity_msat / 1000).sum()
    }

    /// Our balance in the channels with the other party: what we would get if they closed now.
    pub fn our_balance_sats(&self) -> u64 {
        let Ok(peer) = self.counterparty() else { return 0 };
        self.node
            .list_balances()
            .lightning_balances
            .iter()
            .map(|b| match b {
                ldk_node::LightningBalance::ClaimableOnChannelClose { counterparty_node_id, amount_satoshis, .. }
                    if *counterparty_node_id == peer =>
                {
                    *amount_satoshis
                }
                _ => 0,
            })
            .sum()
    }

    fn send(&self, amount_msat: u64, body: Body) -> Result<PaymentId> {
        let peer = self.counterparty()?;
        let tlv = CustomTlvRecord { type_num: TLV_TYPE, value: Message::new(body).encode() };
        self.node
            .spontaneous_payment()
            .send_with_custom_tlvs(amount_msat, peer, None, vec![tlv])
            .map_err(|e| anyhow!("sending payment: {e}"))
    }

    /// Saver: proposes to keep `sats` stable in gold, valued at our own current price.
    pub fn propose(&mut self, sats: u64, quotes: &[Quote], now: i64) -> Result<Position> {
        if self.cfg.role != Role::Saver {
            bail!("only a saver proposes a position");
        }
        if sats > self.our_balance_sats() {
            bail!("we hold only {} sats in the channels, cannot stabilize {sats}", self.our_balance_sats());
        }
        let mut gold = self.state.gold;
        let price = menger_price(quotes, now, &self.cfg.price_rules, &mut gold).map_err(|e| anyhow!("no price: {e}"))?;
        self.state.gold = gold;
        let position = Position {
            micro_at_start: price.sats_per_menger.micro_for(sats),
            started_at: now,
            rate_ppm_per_year: self.cfg.rate_ppm_per_year,
            backing_sats: sats,
        };
        self.send(
            MESSAGE_AMOUNT_MSAT,
            Body::Position {
                micro_at_start: position.micro_at_start,
                started_at: position.started_at,
                rate_ppm_per_year: position.rate_ppm_per_year,
                backing_sats: position.backing_sats,
                sats_per_menger: price.sats_per_menger,
            },
        )?;
        self.state.proposed = Some(position.clone());
        self.state.note(
            now,
            format!(
                "position proposée : {} MNGR pour {sats} sats ({} sats par Menger)",
                format_mengers(position.micro_at_start),
                price.sats_per_menger.0
            ),
        );
        self.save()?;
        Ok(position)
    }

    /// One step: read events, value the position, act.
    pub fn tick(&mut self, quotes: &[Quote], now: i64) -> Result<Report> {
        let mut report = Report { at: now, ..Report::default() };
        while let Some(event) = self.node.next_event() {
            self.handle_event(event, quotes, now, &mut report)?;
            // Saved before the event is acknowledged, so that no update is lost in a crash. An event
            // replayed after a crash is not counted twice: outgoing payments clear `in_flight`,
            // incoming ones are remembered by hash.
            self.save()?;
            self.node.event_handled().map_err(|e| anyhow!("{e}"))?;
        }
        let result = self.act(quotes, now, &mut report);
        for e in &report.events {
            self.state.note(now, e.clone());
        }
        self.save()?;
        result.map(|_| report)
    }

    fn act(&mut self, quotes: &[Quote], now: i64, report: &mut Report) -> Result<()> {
        if self.state.closed.is_some() {
            return Ok(());
        }
        let Some(position) = self.state.position.clone() else { return Ok(()) };
        report.micro_mengers = Some(position.micro_mengers(now));
        report.backing_sats = Some(position.backing_sats);
        let mut gold = self.state.gold;
        let price = match menger_price(quotes, now, &self.cfg.price_rules, &mut gold) {
            Ok(p) => p,
            Err(e) => {
                // Without a price nobody can tell who owes what: the grace period starts again
                // once prices are back.
                report.price_error = Some(e.to_string());
                self.state.owed_to_us_since = None;
                return Ok(());
            }
        };
        self.state.gold = gold;
        let snapshot = Snapshot {
            role: self.cfg.role,
            position: &position,
            price: price.sats_per_menger,
            now,
            our_spendable_sats: self.our_spendable_sats(),
            their_spendable_sats: self.their_spendable_sats(),
            last_payment_at: self.state.last_payment_at,
            payment_in_flight: self.state.in_flight.is_some(),
            owed_to_us_since: self.state.owed_to_us_since,
        };
        let decision = decide(&snapshot, &self.cfg.params);
        report.price = Some(price.clone());
        report.decision = Some(decision.clone());
        self.state.owed_to_us_since = match decision.action {
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
                let id = self.send(
                    sats * 1000,
                    Body::Stability {
                        started_at: position.started_at,
                        sats_per_menger: price.sats_per_menger,
                        backing_after,
                    },
                )?;
                self.state.in_flight = Some(InFlight { payment_id: payment_hex(&id), sats: *sats });
                report.events.push(format!("paiement de stabilité envoyé : {sats} sats"));
            }
            Action::Close { reason } => {
                let why = format!("{reason:?}");
                self.close_all(&why)?;
                report.events.push(format!("fermeture du canal : {why}"));
                self.state.closed = Some(why);
            }
        }
        Ok(())
    }

    /// Closes every channel with the other party, cooperatively if possible.
    pub fn close_all(&mut self, reason: &str) -> Result<()> {
        let peer = self.counterparty()?;
        for c in self.node.list_channels().into_iter().filter(|c| c.counterparty_node_id == peer) {
            if self.node.close_channel(&c.user_channel_id, peer).is_err() {
                self.node
                    .force_close_channel(&c.user_channel_id, peer, Some(reason.to_string()))
                    .map_err(|e| anyhow!("force-closing: {e}"))?;
            }
        }
        Ok(())
    }

    fn handle_event(&mut self, event: Event, quotes: &[Quote], now: i64, report: &mut Report) -> Result<()> {
        match event {
            Event::PaymentSuccessful { payment_id: Some(id), .. } => {
                if let Some(f) = self.state.in_flight.clone() {
                    if f.payment_id == payment_hex(&id) {
                        if let Some(pos) = self.state.position.as_mut() {
                            apply_payment(pos, f.sats, self.cfg.role == Role::Provider);
                        }
                        self.state.in_flight = None;
                        self.state.last_payment_at = Some(now);
                        report.events.push(format!("paiement de stabilité arrivé : {} sats", f.sats));
                    }
                }
            }
            Event::PaymentFailed { payment_id: Some(id), reason, .. } => {
                if self.state.in_flight.as_ref().is_some_and(|f| f.payment_id == payment_hex(&id)) {
                    self.state.in_flight = None;
                    report.events.push(format!("paiement de stabilité échoué ({reason:?}), nouvel essai plus tard"));
                }
            }
            Event::PaymentReceived { payment_hash, amount_msat, custom_records, .. } => {
                if !self.state.first_time(&hex(&payment_hash.0)) {
                    return Ok(());
                }
                let msg = custom_records
                    .iter()
                    .find(|r| r.type_num == TLV_TYPE)
                    .and_then(|r| Message::decode(&r.value));
                if let Some(m) = msg {
                    self.handle_message(m.body, amount_msat, quotes, now, report)?;
                }
            }
            Event::ChannelReady { counterparty_node_id, .. } => {
                if let Some(peer) = counterparty_node_id {
                    if self.state.counterparty.is_none() {
                        self.state.counterparty = Some(peer.to_string());
                    }
                }
                report.events.push("canal prêt".into());
            }
            Event::ChannelClosed { reason, .. } => {
                report.events.push(format!("canal fermé ({reason:?})"));
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_message(&mut self, body: Body, amount_msat: u64, quotes: &[Quote], now: i64, report: &mut Report) -> Result<()> {
        match body {
            Body::Position { micro_at_start, started_at, rate_ppm_per_year, backing_sats, sats_per_menger } => {
                if self.cfg.role != Role::Provider {
                    return Ok(());
                }
                let position = Position { micro_at_start, started_at, rate_ppm_per_year, backing_sats };
                let verdict = self.evaluate_proposal(&position, sats_per_menger, quotes, now);
                let accepted = verdict.is_ok();
                if accepted {
                    report.events.push(format!(
                        "position acceptée : {} MNGR, {backing_sats} sats",
                        format_mengers(micro_at_start)
                    ));
                    self.state.position = (micro_at_start > 0).then_some(position);
                    self.state.owed_to_us_since = None;
                } else {
                    report.events.push(format!("position refusée : {}", verdict.as_ref().unwrap_err()));
                }
                self.send(MESSAGE_AMOUNT_MSAT, Body::PositionReply { started_at, accepted, reason: verdict.err() })?;
            }
            Body::PositionReply { started_at, accepted, reason } => {
                if self.cfg.role != Role::Saver {
                    return Ok(());
                }
                match self.state.proposed.take() {
                    Some(p) if p.started_at == started_at && accepted => {
                        report.events.push(format!("le fournisseur accepte : {} MNGR", format_mengers(p.micro_at_start)));
                        self.state.position = (p.micro_at_start > 0).then_some(p);
                        self.state.owed_to_us_since = None;
                    }
                    Some(p) if p.started_at == started_at => {
                        report.events.push(format!("le fournisseur refuse : {}", reason.unwrap_or_default()));
                    }
                    other => self.state.proposed = other,
                }
            }
            Body::Stability { started_at, sats_per_menger, .. } => {
                let sats = amount_msat / 1000;
                let Some(pos) = self.state.position.as_mut() else { return Ok(()) };
                if pos.started_at != started_at {
                    report.events.push("paiement de stabilité pour une autre position, ignoré".into());
                    return Ok(());
                }
                // Received from the other side: the provider pays the saver, or the saver the provider.
                apply_payment(pos, sats, self.cfg.role == Role::Saver);
                report.events.push(format!("paiement de stabilité reçu : {sats} sats (prix du payeur : {} sats par Menger)", sats_per_menger.0));
            }
        }
        Ok(())
    }

    /// Provider: checks a proposed position against our own price and means.
    fn evaluate_proposal(
        &mut self,
        p: &Position,
        their_price: menger_core::SatsPerMenger,
        quotes: &[Quote],
        now: i64,
    ) -> std::result::Result<(), String> {
        if (now - p.started_at).abs() > 600 {
            return Err("heure de départ trop éloignée".into());
        }
        if p.rate_ppm_per_year < self.cfg.rate_ppm_per_year {
            return Err(format!("prix de la stabilité trop bas (minimum {} ppm par an)", self.cfg.rate_ppm_per_year));
        }
        let mut gold = self.state.gold;
        let ours = menger_price(quotes, now, &self.cfg.price_rules, &mut gold).map_err(|e| format!("pas de prix : {e}"))?;
        self.state.gold = gold;
        let gap = (ours.sats_per_menger.0 as i128 - their_price.0 as i128).unsigned_abs() * 1_000_000
            / ours.sats_per_menger.0.max(1) as u128;
        if gap > self.cfg.max_price_gap_ppm as u128 {
            return Err(format!("prix trop différent du nôtre ({gap} ppm)"));
        }
        if their_price.micro_for(p.backing_sats).abs_diff(p.micro_at_start) > 1 {
            return Err("montant incohérent avec le prix annoncé".into());
        }
        if p.backing_sats > self.their_spendable_sats() {
            return Err("l'épargnant ne détient pas ces sats dans le canal".into());
        }
        let target = ours.sats_per_menger.sats_for(p.micro_at_start) as u128;
        let ours_in_channel = self.our_channel_sats() as u128;
        let coverage = ours_in_channel * 1_000_000 / (ours_in_channel + target).max(1);
        if coverage < self.cfg.min_coverage_ppm as u128 {
            return Err("pas assez de bitcoin de notre côté pour couvrir une baisse".into());
        }
        Ok(())
    }
}
